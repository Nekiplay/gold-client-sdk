//! # example-diagnostics
//!
//! Инструмент, а не фича: прогоняет самопроверки SDK (смещения, таблица api,
//! vtable рендерера, формат модели) и пишет отчёт в лог. Нужен после обновления
//! игры, чтобы понять, какие адреса уехали, до того как что-то рисовать.
//!
//! Точка входа — `DllMain`, которая поднимает рабочий поток (в `DllMain` нельзя
//! грузить библиотеки и ждать). Лог и все проверки живут в [`golden_sdk`].

#![deny(unsafe_op_in_unsafe_fn)]

pub mod diagnostics;

use std::ffi::c_void;
use std::time::{Duration, Instant};

const DLL_PROCESS_ATTACH: u32 = 1;

/// Сколько ждать загрузки `hw.dll`/`client.dll` при старте.
const MODULE_WAIT: Duration = Duration::from_secs(180);

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
            .name("example-diagnostics".to_string())
            .spawn(bootstrap);
    }
    1
}

/// Стартовый сценарий: дождаться модулей, прогнать диагностику.
fn bootstrap() {
    golden_sdk::log!("example-diagnostics {} запускается", env!("CARGO_PKG_VERSION"));
    golden_sdk::log!("сборка смещений: {}", golden_sdk::BUILD);

    if !wait_for_modules(MODULE_WAIT) {
        golden_sdk::log!("не дождались hw.dll/client.dll за {MODULE_WAIT:?} — выходим");
        return;
    }

    let hw = golden_sdk::process::hw_base().unwrap_or(0);
    let client = golden_sdk::process::client_base().unwrap_or(0);
    golden_sdk::log!("hw.dll base = {hw:#x}, client.dll base = {client:#x}");

    let report = diagnostics::run_when_ready(Duration::from_secs(120));
    if report.is_ok() {
        golden_sdk::log!("все проверки прошли — смещения актуальны");
    } else {
        golden_sdk::log!(
            "{} проверок упало — смещения нужно перепроверить (docs/STUDIO_REVERSE.md, раздел 9)",
            report.failures()
        );
    }
}

/// Ждёт загрузки обоих модулей.
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