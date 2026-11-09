//! # example-triggerbot
//!
//! Триггербот без обводки: стреляет, когда прицел стоит на игроке и тому
//! ничего не мешает. Цели — игроки, которых движок рисует в этом кадре
//! (фильтр «только враги» ESP), хитбоксы снимаются из той же позы, в которой
//! движок нарисовал тело, а «не закрыт ли» — по буферу глубины, тем же
//! сравнением, каким обводка ESP красит видимую часть. Сама обводка выключена
//! ([`golden_std::esp::Esp::outline(false)`]), поэтому мод не рисует ни
//! пикселя.
//!
//! Как это работает (вся механика — в [`golden_std::trigger`]):
//!
//! 1. Перехваты отрисовки ставит ESP с выключенной обводкой: цели собираются
//!    прямо из обычного движкового рендера, без второй отрисовки моделей.
//! 2. В конце 3D-сцены триггербот ищет хитбокс под прицельным лучом,
//!    проверяет точку входа по буферу глубины и, если цель под прицелом
//!    дольше «реакции», зажимает огонь.
//! 3. Перехваченный `CL_CreateMove` добавляет бит `IN_ATTACK` прямо в
//!    исходящую команду игрока: кнопка уходит на сервер с ближайшей
//!    командой, минуя командный буфер движка и его задержку.
//!
//! Управление:
//!
//! * Держать `MOUSE5` (боковая кнопка мыши) — пока зажата, автострельба
//!   разрешена: прицел на враге → мод жмёт кнопку;
//! * `F11` — огонь вкл/выкл (включён с момента загрузки);
//! * `trigger` — то же из консоли;
//! * `F12` или `unload_trigger` — выгрузить мод без перезапуска игры.
//!
//! ## Границы
//!
//! Пример рассчитан на оффлайн-игру с ботами. Автострельба по живым игрокам
//! в мультиплеере — нечестная игра, и правила проекта её запрещают.

#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::time::{Duration, Instant};

use golden_std::input::Key;

const DLL_PROCESS_ATTACH: u32 = 1;

/// Сколько ждать загрузки `hw.dll`/`client.dll`.
const MODULE_WAIT: Duration = Duration::from_secs(180);

/// Как часто проверять хоткеи и подмену интерфейса.
const WATCHDOG: Duration = Duration::from_millis(250);

/// «Время реакции»: цель должна постоять под прицелом столько, прежде чем
/// мод нажмёт кнопку. `0` — огонь в тот же кадр, как прицел на цели
/// (минимальная задержка). Поднять до ~40–80 — огонь «погуманнее».
const REACTION: Duration = Duration::from_millis(0);

/// Огонь разрешён, пока зажата эта кнопка (боковая кнопка мыши).
const ACTIVATION: Key = Key::Mouse5;
/// `F11` — переключить огонь.
const TOGGLE_KEY: Key = Key::F11;
/// `F12` — выгрузить мод.
const UNLOAD_KEY: Key = Key::F12;

/// Консольная команда `trigger`: переключает огонь.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_trigger(_args: *const golden_std::concommand::CommandArgs) {
    let on = !golden_std::trigger::enabled();
    golden_std::trigger::set_enabled(on);
}

/// Консольная команда `unload_trigger`: выгружает мод, не перезапуская игру.
///
/// # Safety
/// Вызывается движком из игрового потока; аргументы не разыменовываем.
unsafe extern "C" fn cmd_unload(_args: *const golden_std::concommand::CommandArgs) {
    // Отключаемся из отдельного потока: колбэк исполняется на стеке движка, и
    // снимать свою же регистрацию прямо отсюда нельзя — движок вернётся в
    // объект команды, который мы освободим.
    golden_std::unload::defer_unload(golden_sdk::module::DEFAULT_DELAY_MS);
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
            .name("example-triggerbot".to_string())
            .spawn(bootstrap);
    }
    1
}

/// Стартовый сценарий: дождаться модулей, поставить ESP и триггербот.
fn bootstrap() {
    // Поток принадлежит DLL: при выгрузке он должен выйти первым.
    let mut token = Some(golden_sdk::module::ThreadToken::new());
    let mut fired_seen = 0u64;
    golden_sdk::log!(
        "example-triggerbot {} запускается",
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
    // триггербота. Обводка выключена: мод ничего не рисует, игроки снимаются
    // прямо из движкового рендера, без второй отрисовки моделей.
    let esp = golden_std::esp::Esp::new().outline(false);
    if let Err(error) = esp.install() {
        golden_sdk::log!("esp: перехваты не встали: {error} — без них целей нет");
        return;
    }
    golden_sdk::log!("esp: перехваты нужны как источник целей, обводка выключена");

    // Триггербот: перехватов не ставит, только подписывается на цели ESP.
    // Огонь — пока зажата кнопка активации: стреляет, когда её держит
    // игрок и прицел на враге.
    match golden_std::trigger::Trigger::new()
        .delay(REACTION)
        .activation(ACTIVATION)
        .install()
    {
        Ok(()) => {
            golden_sdk::log!(
                "триггербот: реакция {} мс, огонь пока зажата {ACTIVATION}, цели ждёт от ESP",
                REACTION.as_millis()
            );
        }
        Err(error) => {
            golden_sdk::log!("триггербот: не встал: {error}");
            return;
        }
    }

    golden_sdk::log!("F11 — огонь вкл/выкл, F12 — выгрузка мода");

    match golden_std::concommand::Command::new("trigger", cmd_trigger)
        .help("триггербот: огонь вкл/выкл")
        .hidden()
        .register()
    {
        Ok(()) => golden_sdk::log!("команда trigger зарегистрирована"),
        Err(error) => golden_sdk::log!("trigger не зарегистрирована: {error}"),
    }
    match golden_std::concommand::Command::new("unload_trigger", cmd_unload)
        .help("выгрузить мод")
        .hidden()
        .register()
    {
        Ok(()) => golden_sdk::log!("команда unload_trigger зарегистрирована"),
        Err(error) => golden_sdk::log!("unload_trigger не зарегистрирована: {error}"),
    }
    let hidden = ["trigger", "unload_trigger"];

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
    // тогда триггербот перестаёт получать цели — возвращаем подмену на место.
    while !golden_sdk::module::should_stop() {
        std::thread::sleep(WATCHDOG);
        if golden_sdk::module::should_stop() {
            break;
        }
        if TOGGLE_KEY.held() {
            let on = !golden_std::trigger::enabled();
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
        // Уведомление об огне: раз на серию, не на каждый выстрел.
        let fired = golden_std::trigger::fired();
        if fired != fired_seen {
            fired_seen = fired;
            golden_sdk::log!("триггербот: выстрел {fired}");
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
