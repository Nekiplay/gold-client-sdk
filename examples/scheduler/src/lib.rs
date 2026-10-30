//! # example-scheduler
//!
//! Пример работы с [`golden_std::Scheduler`] — очередью отложенных консольных
//! команд, которую двигает кадр.
//!
//! Как это устроено:
//!
//! 1. Ставим перехват `CStudioModelRenderer::StudioDrawModel` (слот 3 vtable):
//!    движок вызывает его каждый кадр, пока на экране есть хоть одна модель.
//! 2. Раз в [`demo::PERIOD`] планируем две команды: `echo` сейчас и `echo`
//!    через [`demo::DELAY`] (25 мс).
//! 3. На каждом кадре `tick` отправляет созревшие строки в командный буфер
//!    движка (`Cbuf_AddText`) и пишет в лог измеренную задержку.
//!
//! Поток рендера не блокируется ни на миллисекунду: `tick` — это проверка
//! `Instant` и, если пора, вызов `Cbuf_AddText`. Никаких `sleep` в хуках и
//! никаких фоновых потоков «на каждую пару команд».
//!
//! ## Как проверить
//!
//! Инжектить в `hl.exe`, зайти на карту (нужны модели в кадре), открыть
//! `golden-client.log` рядом с DLL — там строки `отправлено 1 (…) +25.7 мс`.
//! Команды уходят в консоль игры: `echo [scheduler] вторая команда через 25 мс`.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod demo;

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const DLL_PROCESS_ATTACH: u32 = 1;

/// Сколько ждать загрузки `hw.dll`/`client.dll`.
const MODULE_WAIT: Duration = Duration::from_secs(180);

/// Интервал между цепочками команд.
const CHAIN_INTERVAL: Duration = demo::PERIOD;

/// Слот `CStudioModelRenderer::StudioDrawModel` в vtable.
const SLOT_DRAW_MODEL: usize = 3;

/// Адрес оригинала `int StudioDrawModel(int flags)`.
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

/// Когда последний раз запускали цепочку.
static LAST_CHAIN: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// Хранилище детура: держим его живым на всё время работы DLL.
static DETOUR: std::sync::Mutex<Option<golden_sdk::hook::Detour>> =
    std::sync::Mutex::new(None);

/// Точка входа DLL.
///
/// # Safety
/// Вызывается загрузчиком Windows; `module` — HMODULE, `reason` — код события.
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(module: *mut c_void, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        golden_sdk::log::init(golden_sdk::log::path_next_to(module));
        // В DllMain нельзя блокировать загрузчик — вся работа в своём потоке.
        let _ = std::thread::Builder::new()
            .name("example-scheduler".to_string())
            .spawn(bootstrap);
    }
    1
}

fn bootstrap() {
    golden_sdk::log!("example-scheduler {} запускается", env!("CARGO_PKG_VERSION"));
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

    if let Err(error) = install() {
        golden_sdk::log!("перехват не поставлен: {error} — выходим");
    }
}

/// Ставит перехват `StudioDrawModel` — он и будет нашим «тиком» кадра.
fn install() -> Result<(), String> {
    let deadline = Instant::now() + MODULE_WAIT;
    loop {
        if let Some(renderer) = golden_sdk::renderer::StudioRenderer::global() {
            if let Some(entry) = renderer.method(SLOT_DRAW_MODEL) {
                // SAFETY: `entry` — начало `StudioDrawModel` этого рендерера;
                // подменяем функцию с той же сигнатурой.
                let detour = unsafe {
                    golden_sdk::hook::Detour::new(entry, hook_draw_model as *const ())
                }
                .map_err(|error| format!("{error}"))?;
                detour.enable().map_err(|error| format!("{error}"))?;
                ORIGINAL.store(detour.trampoline() as usize, Ordering::Release);
                if let Ok(mut slot) = DETOUR.lock() {
                    *slot = Some(detour);
                }
                golden_sdk::log!("перехват StudioDrawModel включён ({entry:#x})");
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err("движок так и не отдал рендерер".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Перехват кадра: планирует цепочку по расписанию и отправляет созревшее.
///
/// # Safety
/// Вызывается движком (thiscall) из потока рендера.
unsafe extern "thiscall" fn hook_draw_model(this: *mut c_void, flags: i32) -> i32 {
    let original = ORIGINAL.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }

    // Планирование — до отрисовки, чтобы команды успели попасть в буфер кадра.
    if let Ok(mut last) = LAST_CHAIN.lock() {
        let due = last.map(|last| last.elapsed() >= CHAIN_INTERVAL).unwrap_or(true);
        if due {
            *last = Some(Instant::now());
            demo::start_chain();
        }
    }
    demo::tick();

    // SAFETY: сохранённый адрес оригинала `int StudioDrawModel(int)`.
    let call: unsafe extern "thiscall" fn(*mut c_void, i32) -> i32 =
        unsafe { core::mem::transmute(original) };
    unsafe { call(this, flags) }
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