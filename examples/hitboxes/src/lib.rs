//! # example-hitboxes
//!
//! Рисует хитбоксы тела игрока — те самые, которые движок перебирает, когда
//! решает, куда попала пуля.
//!
//! Отличие от «коробки по габаритам модели» принципиальное: у каждого хитбокса
//! своя кость и свои границы в её системе координат, поэтому бокс поворачивается
//! вместе с рукой или головой. Рисуются именно эти повёрнутые объёмы
//! ([`golden_sdk::studio::WorldHitbox::edges`]), а не AABB по ним — AABB у
//! повёрнутого бокса заметно больше и на экране накрывает соседние части тела.
//!
//! ## Откуда берутся матрицы костей
//!
//! Боксы строятся в момент отрисовки тела: только там матрицы костей этого
//! игрока валидны. Читается `m_pbonetransform` (`renderer + 0x40B4`) — тот же
//! массив `hw.dll + 0x29F2A58`, из которого игра строит хитбоксы для выстрела:
//! её построитель (`hw.dll + 0x93440`) не считает кости сам, а вызывает расчёт
//! через таблицу движка, передавая ему то состояние, которое получил. На
//! клиенте это клиентский расчёт, поэтому hull строится из той позы, которой
//! тело нарисовано.
//!
//! Боксы строятся сразу после оригинала отрисовки: до вызова в буфере ещё чужая
//! модель, после — эта.
//!
//! ## Как ставится перехват
//!
//! 1. Подменяется интерфейс рендерера ([`golden_std::hooks::interface`]) — наш
//!    `StudioDrawPlayer` вызывает клиентский.
//! 2. Из первого его вызова (мы уже в рендер-потоке) ставятся остальные
//!    перехваты: `StudioRenderModel` в таблице клиентского рендерера
//!    ([`golden_std::hooks::install`], слот [`SLOT_RENDER_MODEL`]) и конец
//!    3D-сцены.
//! 3. Наш `StudioRenderModel` вызывает клиентский (тело рисуется как обычно), а
//!    после возврата читает матрицы костей и строит боксы.
//! 4. В конце 3D-сцены (`HUD_DrawNormalTriangles`) боксы выводятся линиями.
//!
//! ## Команды
//!
//! * `hitboxes_dump` — в следующем кадре напечатать боксы и кости тел;
//! * `hitboxes_bones` — подробный список костей.
//!
//! ## Границы
//!
//! Пример рассчитан на оффлайн-игру с ботами и на разбор своих моделей. Рисовать
//! хитбоксы живых игроков в мультиплеере — это wallhack, и правила проекта это
//! запрещают.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod boxes;

use std::ffi::c_void;
use std::time::{Duration, Instant};

const DLL_PROCESS_ATTACH: u32 = 1;

/// Сколько ждать загрузки `hw.dll`/`client.dll`.
const MODULE_WAIT: Duration = Duration::from_secs(180);

/// Как часто проверять, что подмена интерфейса на месте.
const WATCHDOG: Duration = Duration::from_millis(250);

/// Консольная команда `unload_hitboxes`: выгружает мод, не перезапуская игру.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_unload(_args: *const golden_std::concommand::CommandArgs) {
    // Отключаемся из отдельного потока: колбэк исполняется на стеке движка, и
    // снимать свою же регистрацию прямо отсюда нельзя — движок вернётся в
    // объект команды, который мы освободим.
    golden_std::unload::defer_unload(golden_sdk::module::DEFAULT_DELAY_MS);
}

/// Консольная команда `hitboxes_dump`: печатает боксы всех нарисованных игроков.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_dump(_args: *const golden_std::concommand::CommandArgs) {
    boxes::request_dump();
}

/// Консольная команда `hitboxes_bones`: печатает ещё и кости каждого тела.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_bones(_args: *const golden_std::concommand::CommandArgs) {
    boxes::request_bone_dump();
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
            .name("example-hitboxes".to_string())
            .spawn(bootstrap);
    }
    1
}

/// Стартовый сценарий: дождаться модулей, подменить интерфейс, следить за ним.
fn bootstrap() {
    // Поток принадлежит DLL: при выгрузке он должен выйти первым.
    let mut token = Some(golden_sdk::module::ThreadToken::new());
    golden_sdk::log!("example-hitboxes {} запускается", env!("CARGO_PKG_VERSION"));
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

    match boxes::install() {
        Ok(()) => golden_sdk::log!("интерфейс рендерера подменён, рисуем хитбоксы"),
        Err(err) => {
            golden_sdk::log!("подмена не удалась: {err} — выходим");
            return;
        }
    }

    golden_sdk::log!("F12 — выгрузка мода без перезапуска игры");

    match golden_std::concommand::Command::new("unload_hitboxes", cmd_unload)
        .help("выгрузить мод")
        .hidden()
        .register()
    {
        Ok(()) => golden_sdk::log!("команда unload_hitboxes зарегистрирована"),
        Err(error) => golden_sdk::log!("unload_hitboxes не зарегистрирована: {error}"),
    }

    match golden_std::concommand::Command::new("hitboxes_dump", cmd_dump)
        .help("напечатать хитбоксы нарисованных игроков")
        .hidden()
        .register()
    {
        Ok(()) => golden_sdk::log!("команда hitboxes_dump зарегистрирована"),
        Err(error) => golden_sdk::log!("hitboxes_dump не зарегистрирована: {error}"),
    }

    match golden_std::concommand::Command::new("hitboxes_bones", cmd_bones)
        .help("напечатать хитбоксы и кости нарисованных игроков")
        .hidden()
        .register()
    {
        Ok(()) => golden_sdk::log!("команда hitboxes_bones зарегистрирована"),
        Err(error) => golden_sdk::log!("hitboxes_bones не зарегистрирована: {error}"),
    }

    // Эхо набранных команд убираем на стороне клиента: консоль Gold Client
    // держит историю в `GameUI.dll` и про печать движка ничего не знает.
    if golden_std::console::install(&["unload_hitboxes", "hitboxes_dump", "hitboxes_bones"]) {
        golden_sdk::log!("консоль: эхо команд скрыто");
    }

    // Управление извне: строки из файла рядом с логом исполняются как команды
    // консоли (`bot_stop 1`, `snapshot`, `hitboxes_dump`, `!unload`).
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
        if golden_std::hooks::reinstall_interfaces() {
            golden_sdk::log!("подмены сброшены клиентом — вернули");
        }
        if let Some(line) = boxes::take_report() {
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