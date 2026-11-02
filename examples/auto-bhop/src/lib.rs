//! # example-auto-bhop
//!
//! Пример [`golden_std::input`] — перехвата `Key_Event`, блокировки клавиши и
//! прыжка колесом — плюс [`golden_std::Scheduler`], очередь отложенных действий,
//! которую двигает кадр.
//!
//! Как это устроено:
//!
//! 1. Ставим перехват `CStudioModelRenderer::StudioDrawModel` (слот 3 vtable):
//!    движок вызывает его каждый кадр, пока на экране есть хоть одна модель.
//!    Это наш «тик» кадра.
//! 2. На старте планировщик блокирует пробел: `Key_Event` с кодом `VK_SPACE`
//!    дальше перехвата не идёт — игра не видит ни `+jump`, ни отпускания.
//! 3. На каждом кадре `tick` прыгает колесом, пока пробел физически зажат,
//!    и раз в [`demo::PERIOD`] пишет в лог скорость и счётчик прыжков.
//!
//! Поток рендера не блокируется ни на миллисекунду: `tick` — это проверка
//! `Instant` и, если пора, вызов движка. Никаких `sleep` в хуках и никаких
//! фоновых потоков «на каждое действие».
//!
//! ## Как проверить
//!
//! Инжектить в `hl.exe`, зайти на карту и зажать пробел: игрок прыгает сам, в
//! `golden-client.log` рядом с DLL растут счётчик прыжков и скорость.
//!
//! ```text
//! перехват Key_Event включён, пробел блокируется
//! пробел заблокирован: true
//! прыжков 12, скорость 345, проглочено 0 нажатий за 2.0 с, на земле false, vz -218.9, в игре true, кадр 844
//! ```
//!
//! В чате (`say`) пробел печатается как обычно: текстовый ввод перехват
//! пропускает.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod demo;

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const DLL_PROCESS_ATTACH: u32 = 1;

/// Сколько ждать загрузки `hw.dll`/`client.dll`.
const MODULE_WAIT: Duration = Duration::from_secs(180);

/// Слот `CStudioModelRenderer::StudioDrawModel` в vtable.
const SLOT_DRAW_MODEL: usize = 3;

/// Адрес оригинала `int StudioDrawModel(int flags)`.
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

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
            .name("example-auto-bhop".to_string())
            .spawn(bootstrap);
    }
    1
}

fn bootstrap() {
    golden_sdk::log!(
        "example-auto-bhop {} запускается",
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

    if let Err(error) = install() {
        golden_sdk::log!("перехват кадра не поставлен: {error} — выходим");
        return;
    }
    // Блокировка ставится сразу, а не на первом кадре: в меню моделей нет, а
    // пробел там тоже не должен уходить в игру.
    demo::start();
}

/// Ставит перехват `StudioDrawModel` — он и будет нашим «тиком» кадра.
///
/// Хук живёт в реестре `golden-std`: выгрузка снимает его сама, примеру
/// достаточно не мешать.
fn install() -> Result<(), String> {
    let deadline = Instant::now() + MODULE_WAIT;
    loop {
        if let Some(renderer) = golden_sdk::renderer::StudioRenderer::global() {
            if let Some(entry) = renderer.method(SLOT_DRAW_MODEL) {
                // SAFETY: `hook_draw_model` — функция того же ABI, что и
                // `StudioDrawModel` этого рендерера.
                let hook =
                    unsafe { golden_std::hooks::install(entry, hook_draw_model as *const ()) }
                        .map_err(|error| format!("{error}"))?;
                ORIGINAL.store(hook.original(), Ordering::Release);
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

golden_std::hook! {
    /// Перехват кадра: двигает очередь действий и пишет отчёт.
    ///
    /// Вызывается движком (thiscall) из потока рендера.
    unsafe extern "thiscall" fn hook_draw_model(this: *mut c_void, flags: i32)
        original: draw_model_original
        -> i32
    {
        // До отрисовки, чтобы отложенные строки успели попасть в буфер кадра.
        demo::tick();
        unsafe { draw_model_original()(this, flags) }
    }
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
