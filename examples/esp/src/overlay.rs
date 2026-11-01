//! ESP: обводка игроков и подписи.
//!
//! Вся механика живёт в [`golden_std::esp`] — здесь только выбор цвета, ширины
//! и текстов. Так пример показывает ровно то, что отличает один ESP от другого.
//!
//! Строчки `Esp`:
//!
//! * `width` — толщина линий контура;
//! * `color` — цвет контура и подписей (по умолчанию цвет команды);
//! * `label` — первая строка над головой, вторая под ногами.
//!
//! Обводка рисуется в конце 3D-сцены, поэтому её не закрывают ящики, контейнеры
//! и прочие brush-модели, которые движок выводит после игроков. Там же ставятся
//! подписи — они тоже не уходят под геометрию.
//!
//! ## Границы
//!
//! Пример рассчитан на оффлайн-игру с ботами и на разбор своих моделей. Рисовать
//! ESP живых игроков в мультиплеере — это wallhack, и правила проекта это
//! запрещают.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use golden_sdk::player::Team;
use golden_std::{Color, Player};

/// Как часто писать отчёт в лог.
const REPORT_EVERY: Duration = Duration::from_secs(5);

/// Ширина линий контура.
const WIDTH: f32 = 2.0;

static LAST_REPORT: Mutex<Option<Instant>> = Mutex::new(None);

/// Ставит ESP: цвет по команде, ник над головой, оружие под ногами.
///
/// # Errors
/// Строка с причиной, если движок не отдал интерфейс или перехват не встал.
pub fn install() -> Result<(), String> {
    golden_std::esp::Esp::new()
        .width(WIDTH)
        .color(|player: &Player| player.team().map_or(Color::WHITE, Team::color))
        .label(|player: &Player| (Some(player.name()), player.weapon()))
        .install()
}

/// Возвращает подмену интерфейса, если клиент её сбросил. `true` — вернули.
pub fn watchdog() -> bool {
    golden_std::hooks::reinstall_interfaces()
}

/// Строка отчёта, если с прошлого раза прошло достаточно времени.
pub fn take_report() -> Option<String> {
    {
        let mut last = LAST_REPORT.lock().ok()?;
        if last.is_some_and(|at: Instant| at.elapsed() < REPORT_EVERY) {
            return None;
        }
        *last = Some(Instant::now());
    }
    Some(format!(
        "esp: моделей {}, подписей {}",
        golden_std::esp::drawn(),
        golden_std::esp::labels()
    ))
}