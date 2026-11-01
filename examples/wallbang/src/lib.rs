//! # example-wallbang
//!
//! Урон через стены: подпись у прицела говорит, кого видно, сколько стен
//! пробьёт пуля и сколько урона дойдёт до корпуса.
//!
//! Как это работает (всё тяжёлое — в [`golden_sdk`] и [`golden_std`]):
//!
//! 1. [`golden_sdk::bsp`] разбирает `.bsp` текущей карты и трассирует луч по её
//!    геометрии: мир и brush-модели (двери, ящики) — сплошные поверхности.
//! 2. [`golden_sdk::ballistics`] считает выстрел как `FireBullets3` из `mp.dll`:
//!    материал поверхности тратит силу пробития, урон падает с расстоянием,
//!    число стен ограничено типом пули.
//! 3. [`golden_std::wallbang`] привязывает это к игре: берёт карту, на которой
//!    идёт матч, цель под прицелом, оружие в руках и рисует подпись в конце
//!    кадра (`HUD_DrawNormalTriangles`).
//!
//! ## Команды
//!
//! * `wallbang_probe` — посчитать выстрел по каждому живому игроку от своего
//!   глаза и написать в лог. Так видно работу без ручного прицеливания: стенд
//!   подаёт команду и читает лог.
//! * `unload_wallbang` (и `F12`) — выгрузить мод, не перезапуская игру.
//!
//! ## Границы
//!
//! Пример рассчитан на оффлайн-игру с ботами и на разбор своих карт. Считать
//! урон по живым игрокам в мультиплеере — это wallhack, и правила проекта это
//! запрещают: механику (доступ к геометрии, прицелу, отрисовке) даёт SDK, а
//! подключать её в сетевой игре не нужно.

#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::time::{Duration, Instant};

use golden_std::wallbang::Aim;

const DLL_PROCESS_ATTACH: u32 = 1;

/// Сколько ждать загрузки `hw.dll`/`client.dll`.
const MODULE_WAIT: Duration = Duration::from_secs(180);

/// Как часто писать отчёт в лог.
const REPORT_EVERY: Duration = Duration::from_secs(5);

/// Цвет подписи: цель видна — зелёный, за стеной — жёлтый, не пробить — красный.
fn color_for(aim: &Aim) -> golden_std::Color {
    match aim.damage {
        Some(_) if aim.walls == 0 => golden_std::Color::GREEN,
        Some(_) => golden_std::Color::YELLOW,
        None => golden_std::Color::RED,
    }
}

/// Консольная команда `unload_wallbang`: выгружает мод без перезапуска игры.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_unload(_args: *const golden_std::concommand::CommandArgs) {
    // Отключаемся из отдельного потока: колбэк исполняется на стеке движка, и
    // снимать свою же регистрацию прямо отсюда нельзя.
    golden_std::unload::defer_detach();
}

/// Консольная команда `wallbang_probe`: считает выстрел по каждому игроку.
///
/// Счёт идёт на следующем кадре, из рендер-потока: там вид игрока и позиции
/// сущностей уже обновлены. Результат — строки `wallbang_probe:` в логе.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_probe(_args: *const golden_std::concommand::CommandArgs) {
    golden_std::wallbang::request_probe();
}

/// Консольная команда `wallbang_aim`: печатает то, что видно под прицелом.
///
/// Нужна для проверки без экрана: подпись у прицела — это картинка, а команда
/// показывает те же числа строкой в логе.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_aim(_args: *const golden_std::concommand::CommandArgs) {
    golden_std::wallbang::request_aim_report();
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
            .name("example-wallbang".to_string())
            .spawn(bootstrap);
    }
    1
}

/// Стартовый сценарий: дождаться модулей, поставить подпись, следить за кадром.
fn bootstrap() {
    // Поток принадлежит DLL: при выгрузке он должен выйти первым.
    let mut token = Some(golden_sdk::module::ThreadToken::new());
    golden_sdk::log!("example-wallbang {} запускается", env!("CARGO_PKG_VERSION"));
    golden_sdk::log!("сборка смещений: {}", golden_sdk::BUILD);

    if !wait_for_modules(MODULE_WAIT) {
        golden_sdk::log!("не дождались hw.dll/client.dll за {MODULE_WAIT:?} — выходим");
        return;
    }

    match golden_std::wallbang::Wallbang::new().color(color_for).install() {
        Ok(()) => golden_sdk::log!("wallbang: подпись у прицела включена"),
        Err(err) => {
            golden_sdk::log!("wallbang не установился: {err} — выходим");
            return;
        }
    }

    // Выгрузка без перезапуска игры: `F12` или команда в консоли.
    golden_sdk::log!("F12 или unload_wallbang — выгрузка мода без перезапуска игры");

    match golden_std::concommand::Command::new("unload_wallbang", cmd_unload)
        .help("выгрузить мод")
        .hidden()
        .register()
    {
        Ok(()) => golden_sdk::log!("команда unload_wallbang зарегистрирована"),
        Err(error) => golden_sdk::log!("unload_wallbang не зарегистрирована: {error}"),
    }

    match golden_std::concommand::Command::new("wallbang_probe", cmd_probe)
        .help("посчитать урон по каждому игроку от своего глаза")
        .hidden()
        .register()
    {
        Ok(()) => golden_sdk::log!("команда wallbang_probe зарегистрирована"),
        Err(error) => golden_sdk::log!("wallbang_probe не зарегистрирована: {error}"),
    }

    match golden_std::concommand::Command::new("wallbang_aim", cmd_aim)
        .help("показать в логе то, что видно под прицелом")
        .hidden()
        .register()
    {
        Ok(()) => golden_sdk::log!("команда wallbang_aim зарегистрирована"),
        Err(error) => golden_sdk::log!("wallbang_aim не зарегистрирована: {error}"),
    }

    // Управление извне: строки из файла рядом с логом исполняются как команды.
    if let Some(dir) = golden_sdk::log::path()
        .map(std::path::PathBuf::from)
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
    {
        golden_std::commands::watch(golden_std::commands::path_next_to(&dir));
    }

    let mut last_report = Instant::now();
    while !golden_sdk::module::should_stop() {
        std::thread::sleep(Duration::from_millis(250));
        if golden_sdk::module::should_stop() {
            break;
        }
        if hotkey_pressed(UNLOAD_KEY) {
            golden_sdk::log!("F12: выгружаем мод");
            drop(token.take());
            golden_std::unload::unload_after(300);
            return;
        }
        if last_report.elapsed() >= REPORT_EVERY {
            last_report = Instant::now();
            report();
        }
    }
    golden_sdk::log!("выгрузка: поток примера вышел");
}

/// Строка отчёта: сколько кадров с целью и что было под прицелом.
fn report() {
    match golden_std::wallbang::Wallbang::last() {
        Some(aim) => golden_sdk::log!(
            "wallbang: под прицелом {}, кадров {}, с целью {}, урон {}",
            aim.name,
            golden_std::wallbang::Wallbang::frames(),
            golden_std::wallbang::Wallbang::hits(),
            aim.damage
                .map_or_else(|| "не пробьёт".to_string(), |damage| damage.to_string())
        ),
        None => golden_sdk::log!(
            "wallbang: под прицелом никого, кадров {}, с целью {}",
            golden_std::wallbang::Wallbang::frames(),
            golden_std::wallbang::Wallbang::hits()
        ),
    }
}

/// `VK_F12`.
const UNLOAD_KEY: i32 = 0x7B;
/// `GetAsyncKeyState`: клавиша нажата прямо сейчас.
const KEY_DOWN: i16 = 0x8000u16 as i16;

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(key: i32) -> i16;
}

/// Нажата ли клавиша прямо сейчас (шире, чем `golden_std::input`: перехвата нет).
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