//! # example-aimbot
//!
//! Аим на ближайшую часть тела: мод плавно ведёт прицел к той части тела
//! врага, что ближе всего к перекрестию, и **не стреляет, пока прицел не
//! встал на цель**. Огонь в этом примере разрешает триггербот
//! ([`golden_std::trigger`]) — та же проверка «прицел на игроке, и точка не
//! закрыта», что и в `example-triggerbot`, только теперь к цели прицел
//! привозит аим.
//!
//! Как это работает (механика — в [`golden_std::aim`] и [`golden_std::trigger`]):
//!
//! 1. Перехваты отрисовки ставит ESP с выключенной обводкой: хитбоксы
//!    снимаются прямо из движкового рендера, из той же позы, в которой
//!    движок нарисовал тело.
//! 2. Аим в перехваченном `CL_CreateMove` выбирает хитбокс с наименьшей
//!    угловой ошибкой от прицела и на шаг (плавность) ведёт к нему угол
//!    `cl.viewangles` — поле, из которого движок собирает и камеру, и
//!    команду для сервера. Прицел едет — триггербот молчит.
//! 3. Когда прицел встал на тело и точка видна (буфер глубины), триггербот
//!    зажимает `IN_ATTACK` в исходящей команде — кнопка уходит на сервер
//!    без командного буфера и его задержки.
//!
//! Управление:
//!
//! * Наведение — всегда, пока враг в секторе `aim_fov`: никаких кнопок
//!   держать не нужно; собственный выстрел подавлен, пока прицел едет к цели;
//! * `aim_key <vk>` — вернуть кнопку активации (например, `aim_key 6` —
//!   MOUSE5; `aim_key 0` — снова всегда);
//! * `F11` — вкл/выкл всего поведения (аим и огонь вместе);
//! * `aim` — то же из консоли, `aim_status` — текущие настройки;
//! * `aim_smooth <0..100>` — плавность доводки (0 — щелчок);
//! * `aim_fov <1..180>` — сектор поиска цели в градусах от прицела;
//! * `F12` или `unload_aim` — выгрузить мод без перезапуска игры.
//!
//! ## Границы
//!
//! Пример рассчитан на оффлайн-игру с ботами. Доводка прицела и автострельба
//! по живым игрокам в мультиплеере — нечестная игра, и правила проекта её
//! запрещают.

#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::time::{Duration, Instant};

use golden_std::concommand::{self, Command, CommandArgs};
use golden_std::input::Key;

const DLL_PROCESS_ATTACH: u32 = 1;

/// Сколько ждать загрузки `hw.dll`/`client.dll`.
const MODULE_WAIT: Duration = Duration::from_secs(180);

/// Как часто проверять хоткеи и подмену интерфейса.
const WATCHDOG: Duration = Duration::from_millis(250);

/// Плавность доводки: доля остатка, которую аим берёт за одну команду.
/// `0` — щелчок на цель, `35` — заметное, но быстрое ведение.
const AIM_SMOOTH: u32 = 35;

/// Сектор поиска цели, градусы от прицела: цели дальше сектора не трогаются.
const AIM_FOV: f32 = 30.0;

/// «Время реакции» триггербота: сколько цель должна постоять под прицелом,
/// прежде чем нажать кнопку. `0` — огонь в тот же кадр; при плавной доводке
/// прицел сам «постоит», пока едет.
const REACTION: Duration = Duration::from_millis(0);

/// `F11` — включить/выключить поведение.
const TOGGLE_KEY: Key = Key::F11;
/// `F12` — выгрузить мод.
const UNLOAD_KEY: Key = Key::F12;

/// Консольная команда `aim`: переключает аим и огонь вместе.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_aim(_args: *const CommandArgs) {
    golden_sdk::log!("аим: команда aim: вход");
    let on = !golden_std::aim::enabled();
    golden_std::aim::set_enabled(on);
    golden_std::trigger::set_enabled(on);
    report();
}

/// Консольная команда `aim_smooth <0..100>`: плавность доводки.
///
/// # Safety
/// См. [`cmd_aim`].
unsafe extern "C" fn cmd_smooth(args: *const CommandArgs) {
    // SAFETY: движок передаёт живой объект аргументов на время команды.
    let args = unsafe { &*args };
    golden_sdk::log!("аим: команда aim_smooth: вход, argc={}", args.argc());
    let Some(value) = args.arg(1).and_then(|v| v.parse::<u32>().ok()) else {
        concommand::print("aim_smooth <0..100>: 0 — щелчок, 100 — ползёт 5% за команду");
        report();
        return;
    };
    golden_std::aim::set_smooth(value);
    report();
}

/// Консольная команда `aim_fov <1..180>`: сектор поиска цели.
///
/// # Safety
/// См. [`cmd_aim`].
unsafe extern "C" fn cmd_fov(args: *const CommandArgs) {
    // SAFETY: движок передаёт живой объект аргументов на время команды.
    let args = unsafe { &*args };
    golden_sdk::log!("аим: команда aim_fov: вход, argc={}", args.argc());
    let Some(value) = args.arg(1).and_then(|v| v.parse::<f32>().ok()) else {
        concommand::print("aim_fov <1..180>: сектор поиска цели от прицела, градусы");
        report();
        return;
    };
    if !(1.0..=180.0).contains(&value) {
        concommand::print("aim_fov: сектор — от 1 до 180 градусов");
        return;
    }
    golden_std::aim::set_fov(value);
    report();
}

/// Консольная команда `aim_key <vk>`: кнопка активации по VK-коду.
/// `0` — наведение и огонь всегда; например, `6` — MOUSE5 (VK 0x06).
///
/// # Safety
/// См. [`cmd_aim`].
unsafe extern "C" fn cmd_key(args: *const CommandArgs) {
    // SAFETY: движок передаёт живой объект аргументов на время команды.
    let args = unsafe { &*args };
    golden_sdk::log!("аим: команда aim_key: вход, argc={}", args.argc());
    let Some(value) = args
        .arg(1)
        .and_then(|v| v.trim_start_matches("0x").parse::<u32>().ok())
    else {
        concommand::print("aim_key <vk>: 0 — всегда, 6 — MOUSE5 (VK 0x06)");
        report();
        return;
    };
    golden_std::aim::set_activation_vk(value);
    golden_std::trigger::set_activation_vk(value);
    if value == 0 {
        concommand::print("аим: активация — всегда, без кнопки");
    } else {
        concommand::print(&format!("аим: активация — VK 0x{value:02X}"));
    }
}

/// Консольная команда `aim_status`: настройки и счётчики.
///
/// # Safety
/// См. [`cmd_aim`].
unsafe extern "C" fn cmd_status(_args: *const CommandArgs) {
    golden_sdk::log!("аим: команда aim_status: вход");
    report();
}

/// Консольная команда `unload_aim`: выгружает мод, не перезапуская игру.
///
/// # Safety
/// См. [`cmd_aim`].
unsafe extern "C" fn cmd_unload(_args: *const CommandArgs) {
    golden_sdk::log!("аим: команда unload_aim: вход");
    // Отключаемся из отдельного потока: колбэк исполняется на стеке движка, и
    // снимать свою же регистрацию прямо отсюда нельзя — движок вернётся в
    // объект команды, который мы освободим.
    golden_std::unload::defer_unload(golden_sdk::module::DEFAULT_DELAY_MS);
}

/// Строка состояния в консоль: чем настроен аим, сколько раз он правил угол и
/// сколько выстрелов снял предохранитель, пока прицел ехал к цели.
fn report() {
    concommand::print(&format!(
        "аим: {}, плавность {}, сектор {:.0}°, правок угла {}, огонь снят {} раз",
        if golden_std::aim::enabled() {
            "включён"
        } else {
            "выключен"
        },
        golden_std::aim::smooth(),
        golden_std::aim::fov(),
        golden_std::aim::corrections(),
        golden_std::aim::suppressions(),
    ));
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
            .name("example-aimbot".to_string())
            .spawn(bootstrap);
    }
    1
}

/// Стартовый сценарий: дождаться модулей, поставить ESP, аим и триггербот.
fn bootstrap() {
    // Поток принадлежит DLL: при выгрузке он должен выйти первым.
    let mut token = Some(golden_sdk::module::ThreadToken::new());
    let mut corrections_seen = 0u64;
    golden_sdk::log!(
        "example-aimbot {} запускается",
        env!("CARGO_PKG_VERSION")
    );
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

    // Сначала перехваты отрисовки: их ставит `Esp`, и они же — источник целей
    // аима и триггербота. Обводка выключена: мод ничего не рисует, хитбоксы
    // снимаются прямо из движкового рендера, без второй отрисовки моделей.
    let esp = golden_std::esp::Esp::new().outline(false);
    if let Err(error) = esp.install() {
        golden_sdk::log!("esp: перехваты не встали: {error} — без них целей нет");
        return;
    }
    golden_sdk::log!("esp: перехваты нужны как источник целей, обводка выключена");

    // Аим: ведёт прицел всегда, когда враг в секторе — кнопка не нужна.
    match golden_std::aim::Aim::new().smooth(AIM_SMOOTH).fov(AIM_FOV).install() {
        Ok(()) => {
            golden_sdk::log!(
                "аим: плавность {AIM_SMOOTH}, сектор {AIM_FOV:.0}°, наведение всегда (aim_key <vk> — вернуть кнопку)"
            );
        }
        Err(error) => {
            golden_sdk::log!("аим: не встал: {error}");
            return;
        }
    }

    // Триггербот: тот же источник целей; жмёт огонь, только когда прицел
    // приехал на тело и точка не закрыта. Пока аим ведёт — триггербот молчит.
    if let Err(error) = golden_std::trigger::Trigger::new().delay(REACTION).install() {
        golden_sdk::log!("триггербот: не встал: {error} — огонь нечем нажимать");
        return;
    }
    golden_sdk::log!(
        "триггербот: реакция {} мс, огонь сам — когда прицел на теле",
        REACTION.as_millis()
    );

    golden_sdk::log!("F11 — вкл/выкл, F12 — выгрузка мода");

    for (name, help) in [
        ("aim", "аим: вкл/выкл (вместе с огнём)"),
        ("aim_status", "аим: настройки и счётчики"),
        ("aim_smooth", "aim_smooth <0..100> — плавность доводки"),
        ("aim_fov", "aim_fov <1..180> — сектор поиска, градусы"),
        ("aim_key", "aim_key <vk> — 0: всегда, 6: MOUSE5"),
        ("unload_aim", "выгрузить мод"),
    ] {
        let callback = match name {
            "aim" => cmd_aim,
            "aim_status" => cmd_status,
            "aim_smooth" => cmd_smooth,
            "aim_fov" => cmd_fov,
            "aim_key" => cmd_key,
            _ => cmd_unload,
        };
        match Command::new(name, callback).help(help).hidden().register() {
            Ok(()) => golden_sdk::log!("команда {name} зарегистрирована"),
            Err(error) => golden_sdk::log!("{name} не зарегистрирована: {error}"),
        }
    }
    let hidden = ["aim", "aim_status", "aim_smooth", "aim_fov", "aim_key", "unload_aim"];

    // Эхо набранной команды убираем на стороне клиента: консоль Gold Client
    // держит историю в `GameUI.dll` и про печать движка ничего не знает.
    if golden_std::console::install(&hidden) {
        golden_sdk::log!("консоль: эхо команд скрыто");
    }

    // Управление извне: строки из файла рядом с логом исполняются как команды
    // консоли. Так стенд может заморозить ботов и снять кадр без фокуса окна.
    if let Some(dir) = golden_sdk::log::path()
        .map(std::path::PathBuf::from)
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
    {
        golden_std::commands::watch(golden_std::commands::path_next_to(&dir));
    }

    // Клиент может переинициализировать интерфейс (смена карты, vid_restart);
    // тогда аим и триггербот перестанут получать цели — возвращаем подмену.
    while !golden_sdk::module::should_stop() {
        std::thread::sleep(WATCHDOG);
        if golden_sdk::module::should_stop() {
            break;
        }
        if TOGGLE_KEY.held() {
            let on = !golden_std::aim::enabled();
            golden_std::aim::set_enabled(on);
            golden_std::trigger::set_enabled(on);
            // Ждём отпускания: одно нажатие — одно переключение (лог пишет
            // сам `set_enabled`).
            while TOGGLE_KEY.held() && !golden_sdk::module::should_stop() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if UNLOAD_KEY.held() {
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
            golden_sdk::log!("интерфейс сбросили — подмена возвращена");
        }
        // Уведомление о доводке: раз на серию коррекций, не на каждую правку.
        let corrections = golden_std::aim::corrections();
        if corrections != corrections_seen {
            corrections_seen = corrections;
            golden_sdk::log!("аим: правка угла {corrections}");
        }
    }
    golden_sdk::log!("выгрузка: поток watchdog вышел");
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
