//! # example-skeleton
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
            .name("example-skeleton".to_string())
            .spawn(bootstrap);
    }
    1
}

/// Стартовый сценарий: дождаться модулей, подменить интерфейс, следить за ним.
fn bootstrap() {
    golden_sdk::log!("example-skeleton {} запускается", env!("CARGO_PKG_VERSION"));
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

    // Клиент может переинициализировать интерфейс (смена карты, vid_restart).
    loop {
        std::thread::sleep(WATCHDOG);
        if overlay::watchdog() {
            golden_sdk::log!("интерфейс сбросили — подмена возвращена");
        }
        if let Some(line) = overlay::take_report() {
            golden_sdk::log!("{line}");
        }
    }
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