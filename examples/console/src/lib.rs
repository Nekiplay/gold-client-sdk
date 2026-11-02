//! # example-console
//!
//! Показывает, как писать в консоль Gold Client: строки с цветом собираются
//! [`golden_std::console::Builder`], а не печатаются через `Con_Printf`
//! (`hw.dll` её историю не видит).
//!
//! Команды:
//!
//! * `console_demo` — цветной отчёт, собранный билдером;
//! * `console_plain` — обычная строка через `concommand::print`;
//! * `unload_console` — выгрузка мода.
//!
//! Цвет — [`golden_std::console::Color`]; у каждого фрагмента свой цвет, а
//! переводы строк и пары «ключ: значение» добавляются методами билдера.

#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::time::{Duration, Instant};

use golden_std::concommand::{self, Command, CommandArgs};
use golden_std::console::Color;

const DLL_PROCESS_ATTACH: u32 = 1;

/// Сколько ждать загрузки `hw.dll`/`client.dll` при старте.
const MODULE_WAIT: Duration = Duration::from_secs(180);

/// Билдер: заголовок, пары, цвета в одной строке, готовые строки статуса.
unsafe extern "C" fn cmd_demo(_args: *const CommandArgs) {
    let hw = golden_sdk::process::hw_base().unwrap_or(0);
    let client = golden_sdk::process::client_base().unwrap_or(0);
    let printed = golden_std::console::builder()
        .title("golden-client: вывод в консоль")
        .kv("сборка", golden_sdk::BUILD)
        .kv("hw.dll", format!("{hw:#x}"))
        .kv("client.dll", format!("{client:#x}"))
        .blank()
        .text("один цвет: ")
        .colored(Color::GREEN, "зелёный")
        .blank()
        .text("в строке: ")
        .colored(Color::GREEN, "ok")
        .text(" / ")
        .colored(Color::YELLOW, "warn")
        .text(" / ")
        .colored(Color::RED, "error")
        .blank()
        .ok("ok() — зелёная строка")
        .warn("warn() — жёлтая строка")
        .error("error() — красная строка")
        .send();
    golden_sdk::log!(
        "console_demo: вывод {}",
        if printed {
            "напечатан"
        } else {
            "не напечатан"
        }
    );
}

/// Обычная печать: `concommand::print` сам выбирает консоль Gold Client.
unsafe extern "C" fn cmd_plain(_args: *const CommandArgs) {
    concommand::print("console_plain: строка через concommand::print");
}

/// Выгрузка мода без перезапуска игры: DLL освобождается целиком.
unsafe extern "C" fn cmd_unload(_args: *const CommandArgs) {
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
            .name("example-console".to_string())
            .spawn(bootstrap);
    }
    1
}

/// Стартовый сценарий: дождаться модулей и зарегистрировать команды.
fn bootstrap() {
    // Поток принадлежит DLL: при выгрузке он должен выйти первым.
    let _token = golden_sdk::module::ThreadToken::new();
    golden_sdk::log!("example-console {} запускается", env!("CARGO_PKG_VERSION"));
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

    register("console_demo", "показать цветной вывод", cmd_demo);
    register("console_plain", "напечатать обычную строку", cmd_plain);
    register("unload_console", "выгрузить мод", cmd_unload);

    // Консоль Gold Client держит историю в `GameUI.dll`: эхо набранных команд
    // скрываем там же, иначе каждая команда дублируется строкой `] имя`.
    if golden_std::console::install(&["console_demo", "console_plain", "unload_console"]) {
        golden_sdk::log!("консоль: эхо команд скрыто");
    }

    // Управление извне: строки из файла рядом с логом исполняются как команды
    // консоли (`console_demo`, `!unload`).
    if let Some(dir) = golden_sdk::log::path()
        .map(std::path::PathBuf::from)
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
    {
        golden_std::commands::watch(golden_std::commands::path_next_to(&dir));
    }

    golden_sdk::log!("готово: console_demo, console_plain");
}

/// Регистрирует скрытую команду и пишет итог в лог.
fn register(name: &str, help: &str, callback: concommand::CommandCallback) {
    match Command::new(name, callback).help(help).hidden().register() {
        Ok(()) => golden_sdk::log!("команда {name} зарегистрирована"),
        Err(error) => golden_sdk::log!("{name}: не зарегистрирована: {error}"),
    }
}

/// Ждёт загрузки обоих модулей.
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
