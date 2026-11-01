//! # example-esp
//!
//! Оверлей скелета: рисует линии по костям моделей игроков, которые движок
//! рисует в этом кадре.
//!
//! Как это работает (всё тяжёлое — в [`golden_sdk`]):
//!
//! 1. Движок читает указатель на `r_studio_interface_t` из `hw.dll + 0x271B04`
//!    каждый кадр; SDK подменяет его своей структурой ([`golden_sdk::hook`]).
//! 2. Наша реализация `StudioDrawModel` вызывает оригинал, а после него читает
//!    у клиентского рендерера готовые матрицы костей
//!    ([`golden_sdk::renderer::StudioRenderer`]) — считать анимацию самим не надо.
//! 3. Отрезки `кость → родитель` рисуются OpenGL-ом в мировых координатах, пока
//!    матрица вида ещё активна ([`golden_sdk::draw`]).
//!
//! Тест глубины не отключается, поэтому скелет, закрытый стеной, не виден.
//!
//! ## Границы
//!
//! Пример рассчитан на оффлайн-игру с ботами и на разбор своих моделей. Рисовать
//! скелеты живых игроков в мультиплеере — это уже wallhack, и правила проекта
//! это запрещают: механику (доступ к костям, рисование) даёт SDK, а вот так
//! подключать её в сетевой игре не нужно.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod overlay;

use std::ffi::c_void;
use std::time::{Duration, Instant};

const DLL_PROCESS_ATTACH: u32 = 1;

/// Сколько ждать загрузки `hw.dll`/`client.dll`.
const MODULE_WAIT: Duration = Duration::from_secs(180);

/// Как часто проверять, что подмена интерфейса на месте.
const WATCHDOG: Duration = Duration::from_millis(250);

/// Консольная команда `unload_esp`: выгружает мод, не перезапуская игру.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_unload(_args: *const golden_std::concommand::CommandArgs) {
    // Отключаемся из отдельного потока: колбэк исполняется на стеке движка, и
    // снимать свою же регистрацию прямо отсюда нельзя — движок вернётся в
    // объект команды, который мы освободим.
    golden_std::unload::defer_detach();
}

/// Точка входа DLL.
///
/// # Safety
/// Вызывается загрузчиком Windows; `module` — HMODULE, `reason` — код события.
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(module: *mut c_void, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        golden_sdk::log::init(golden_sdk::log::path_next_to(module));
        // Работа в отдельном потоке: в DllMain нельзя блокировать загрузчик.
        let _ = std::thread::Builder::new()
            .name("example-esp".to_string())
            .spawn(bootstrap);
    }
    1
}

/// Стартовый сценарий: дождаться модулей, подменить интерфейс, следить за ним.
fn bootstrap() {
    // Поток принадлежит DLL: при выгрузке он должен выйти первым.
    let mut token = Some(golden_sdk::module::ThreadToken::new());
    golden_sdk::log!("example-esp {} запускается", env!("CARGO_PKG_VERSION"));
    golden_sdk::log!("сборка смещений: {}", golden_sdk::BUILD);

    if !wait_for_modules(MODULE_WAIT) {
        golden_sdk::log!("не дождались hw.dll/client.dll за {MODULE_WAIT:?} — выходим");
        return;
    }

    golden_sdk::log!(
        "hw.dll base = {:#x}, client.dll base = {:#x}",
        golden_sdk::process::hw_base().unwrap_or(0),
        golden_sdk::process::client_base().unwrap_or(0)
    );

    match overlay::install() {
        Ok(()) => golden_sdk::log!("интерфейс рендерера подменён, рисуем скелеты"),
        Err(err) => {
            golden_sdk::log!("подмена не удалась: {err} — выходим");
            return;
        }
    }

    // Выгрузка без перезапуска игры: `F12` или команда `unload_esp` в консоли.
    // Свои хуки пример не снимает — это делает `golden_std::unload` сам.
    golden_sdk::log!("F12 — выгрузка мода без перезапуска игры");

    // Единственная команда мода — выгрузка; снимается автоматически.
    match golden_std::concommand::Command::new("unload_esp", cmd_unload)
        .help("выгрузить мод")
        .hidden()
        .register()
    {
        Ok(()) => golden_sdk::log!("команда unload_esp зарегистрирована"),
        Err(error) => golden_sdk::log!("unload_esp не зарегистрирована: {error}"),
    }

    // Эхо набранной команды убираем на стороне клиента: консоль Gold Client
    // держит историю в `GameUI.dll` и про печать движка ничего не знает,
    // поэтому фильтр в `hw.dll` её не ловит. Перехват снимается при выгрузке.
    if golden_std::console::install(&["unload_esp"]) {
        golden_sdk::log!("консоль: эхо команд скрыто");
    }

    // Управление извне: строки из файла рядом с логом исполняются как команды
    // консоли. Так стенд (`golden-stand`) может, например, заморозить ботов
    // (`bot_stop 1`) и снять кадр (`snapshot`) без возни с фокусом окна.
    if let Some(dir) = golden_sdk::log::path()
        .map(std::path::PathBuf::from)
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
    {
        golden_std::commands::watch(golden_std::commands::path_next_to(&dir));
    }

    // Клиент может переинициализировать интерфейс (смена карты, vid_restart).
    while !golden_sdk::module::should_stop() {
        std::thread::sleep(WATCHDOG);
        if golden_sdk::module::should_stop() {
            break;
        }
        if hotkey_pressed(UNLOAD_KEY) {
            golden_sdk::log!("F12: выгружаем мод");
            // Поток выгрузки не должен ждать сам себя в `wait_threads`.
            drop(token.take());
            // Пауза перед освобождением памяти: движок успевает выйти из
            // наших функций и перерисовать кадр с оригинальным интерфейсом.
            golden_std::unload::unload_after(300);
            return;
        }
        if overlay::watchdog() {
            golden_sdk::log!("интерфейс сбросили — подмена возвращена");
        }
        if let Some(line) = overlay::take_report() {
            golden_sdk::log!("{line}");
        }
    }
    golden_sdk::log!("выгрузка: поток watchdog вышел");
}

/// `VK_F12`.
const UNLOAD_KEY: i32 = 0x7B;
/// `GetAsyncKeyState` вернул «клавиша нажата прямо сейчас».
const KEY_DOWN: i16 = 0x8000u16 as i16;

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(key: i32) -> i16;
}

/// Нажата ли клавиша прямо сейчас.
///
/// Штатный `is_down` из `golden_std::input` годится только для клавиш, которые
/// проходят через перехват `Key_Event`; здесь перехвата нет, поэтому спрашиваем
/// систему напрямую.
fn hotkey_pressed(key: i32) -> bool {
    // SAFETY: вызов без аргументов-указателей.
    unsafe { GetAsyncKeyState(key) & KEY_DOWN != 0 }
}

fn wait_for_modules(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if golden_sdk::process::hw_base().is_some() && golden_sdk::process::client_base().is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}