//! # example-grenade
//!
//! Показывает, куда полетит граната из глаз локального игрока, где она
//! взорвётся и кого заденет: для HE — урон по игрокам, для флешки — список
//! ослеплённых и время слепоты.
//!
//! ## Как это работает
//!
//! * Бросок и полёт считает [`golden_sdk::grenade`] по формулам сервера:
//!   `WeaponIdle` (углы и скорость броска), `SV_Physics_Bounce` (гравитация,
//!   трение, отскоки), `CGrenade::Detonate` (луч вниз и выталкивание из стены),
//!   `RadiusDamage` и `RadiusFlash`.
//! * [`golden_std::grenade`] подставляет в формулы состояние игры: глаза и
//!   скорость локального игрока, позиции и углы остальных, трассировку по
//!   текущей карте.
//! * Перехват ставится в конце 3D-сцены: там GL свободен, а мир и игроки уже
//!   нарисованы. Траектория рисуется поверх кадра линиями (сквозь стены) и
//!   обновляется несколько раз в секунду.
//!
//! ## Команды
//!
//! * `grenade_damage` — показать траекторию HE и напечатать урон по игрокам
//!   (у HE есть и алиас `grenade_he`, но это имя может быть занято командой
//!   клиента, тогда сработает только `grenade_damage`);
//! * `grenade_flash` — то же для флешки: кого ослепит и на сколько;
//! * `grenade_smoke` — траектория смока и точка, где он задымит;
//! * `grenade_off` — выключить отрисовку;
//! * `unload_grenades` — выгрузить мод без перезапуска игры.
//!
//! ## Границы
//!
//! Предсказание клиентское: позиции чужих игроков интерполированы, броня и
//! вода не моделируются, а brush-сущности проверяются только рядом с лучом.
//! Пример рассчитан на оффлайн-игру с ботами: показывать траекторию своих
//! гранат в мультиплеере — это уже подсказка, выходящая за рамки правил.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod report;
pub mod scene;

use std::ffi::c_void;
use std::time::{Duration, Instant};

use golden_sdk::grenade::Kind;

const DLL_PROCESS_ATTACH: u32 = 1;

/// Сколько ждать загрузки `hw.dll`/`client.dll`.
const MODULE_WAIT: Duration = Duration::from_secs(180);

/// Как часто проверять, что подмена интерфейса на месте.
const WATCHDOG: Duration = Duration::from_millis(250);

/// Консольная команда `unload_grenades`: выгружает мод, не перезапуская игру.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_unload(_args: *const golden_std::concommand::CommandArgs) {
    // Отключаемся из отдельного потока: колбэк исполняется на стеке движка, и
    // снимать свою же регистрацию прямо отсюда нельзя.
    golden_std::unload::defer_unload(golden_sdk::module::DEFAULT_DELAY_MS);
}

/// Консольная команда `grenade_he`.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_he(_args: *const golden_std::concommand::CommandArgs) {
    golden_sdk::log!("grenade: команда HE получена");
    scene::show(Kind::He);
    golden_sdk::log!("grenade: показываем HE");
}

/// Консольная команда `grenade_flash`.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_flash(_args: *const golden_std::concommand::CommandArgs) {
    scene::show(Kind::Flash);
    golden_sdk::log!("grenade: показываем флешку");
}

/// Консольная команда `grenade_smoke`.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_smoke(_args: *const golden_std::concommand::CommandArgs) {
    scene::show(Kind::Smoke);
    golden_sdk::log!("grenade: показываем смок");
}

/// Консольная команда `grenade_off`.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_off(_args: *const golden_std::concommand::CommandArgs) {
    scene::hide();
    golden_sdk::log!("grenade: отрисовка выключена");
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
            .name("example-grenade".to_string())
            .spawn(bootstrap);
    }
    1
}

/// Стартовый сценарий: дождаться модулей, подменить интерфейс, зарегистрировать
/// команды и следить за подменой.
fn bootstrap() {
    // Поток принадлежит DLL: при выгрузке он должен выйти первым.
    let mut token = Some(golden_sdk::module::ThreadToken::new());
    golden_sdk::log!("example-grenade {} запускается", env!("CARGO_PKG_VERSION"));
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

    match scene::install() {
        Ok(()) => golden_sdk::log!("интерфейс рендерера подменён, ждём первого кадра"),
        Err(err) => {
            golden_sdk::log!("подмена не удалась: {err} — выходим");
            return;
        }
    }

    golden_sdk::log!("F12 — выгрузка мода без перезапуска игры");

    register("unload_grenades", cmd_unload, "выгрузить мод");
    register("grenade_damage", cmd_he, "траектория HE и урон по игрокам");
    register("grenade_he", cmd_he, "траектория HE и урон по игрокам");
    register(
        "grenade_flash",
        cmd_flash,
        "траектория флешки и список ослеплённых",
    );
    register(
        "grenade_smoke",
        cmd_smoke,
        "траектория смока и точка, где он задымит",
    );
    register("grenade_off", cmd_off, "выключить отрисовку гранат");

    // Эхо набранных команд убираем на стороне клиента: консоль Gold Client
    // держит историю в `GameUI.dll` и про печать движка ничего не знает.
    if golden_std::console::install(&[
        "unload_grenades",
        "grenade_he",
        "grenade_damage",
        "grenade_flash",
        "grenade_smoke",
        "grenade_off",
    ]) {
        golden_sdk::log!("консоль: эхо команд скрыто");
    }

    // Управление извне: строки из файла рядом с логом исполняются как команды
    // консоли (`grenade_flash`, `!unload`).
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
            // Выгрузка могла быть отменена: над модом загружен другой, и
            // выгружаться первым нельзя. Тогда продолжаем работать.
            if !golden_std::unload::can_unload() {
                golden_std::unload::unload_after(0);
                continue;
            }
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
    }
    golden_sdk::log!("выгрузка: поток watchdog вышел");
}

fn register(name: &str, callback: golden_std::concommand::CommandCallback, help: &str) {
    match golden_std::concommand::Command::new(name, callback)
        .help(help)
        .hidden()
        .register()
    {
        Ok(()) => golden_sdk::log!("команда {name} зарегистрирована"),
        Err(error) => golden_sdk::log!("{name} не зарегистрирована: {error}"),
    }
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
        if golden_sdk::process::hw_base().is_some() && golden_sdk::process::client_base().is_some()
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
