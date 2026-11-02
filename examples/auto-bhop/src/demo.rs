//! Демонстрация [`golden_std::Scheduler`] и блокировки клавиш: пробел игрока не
//! доходит до игры, а прыжок делает сам движок по прокрутке колеса.
//!
//! Что делает пример:
//!
//! 1. Ставит перехват `Key_Event` ([`golden_std::input`]) — единственное место,
//!    где GoldSrc превращает нажатие в команду.
//! 2. Блокирует `VK_SPACE`: событие не доходит ни до биндов (`+jump`), ни до
//!    массива нажатых клавиш. Пробел при этом остаётся известен нам
//!    ([`input::is_down`]), поэтому он работает как кнопка «включить банихоп».
//! 3. Пока пробел зажат и игрок на земле, шлёт `WM_MOUSEWHEEL` в окно игры
//!    ([`input::scroll`]). Движок разбирает сообщение в своём насосе и по бинду
//!    `mwheelup +jump` прыгает **в том же кадре** — в отличие от команды через
//!    `Cbuf_AddText`, которая исполняется только на следующем кадре, уже после
//!    приземления, и трение успевает съесть скорость.
//! 4. Раз в [`report::PERIOD`] пишет скорость, `on_ground` и счётчик прыжков.
//!
//! ```text
//! 14:22:31.418 [scheduler] перехват Key_Event включён, пробел блокируется
//! 14:22:31.419 [scheduler] пробел заблокирован: true
//! 14:22:33.421 [scheduler] прыжков 24, скорость 412, проглочено 0 нажатий за 2.0 с
//! ```
//!
//! # Границы
//!
//! Блокировка клавиши и автопрыжок — для локальной игры с ботами и для отладки
//! движка. На живом сервере с другими игроками это чит, и правила проекта его
//! не включают.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use golden_sdk::state::EngineState;
use golden_std::Scheduler;
use golden_std::input;

/// Как часто писать отчёт.
pub const PERIOD: Duration = Duration::from_secs(2);

/// Клавиша, которую глушим: пробел.
pub const KEY: u32 = input::VK_SPACE;

/// Очередь действий.
static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

/// Клиентское время последнего кадра — по нему определяется новый кадр.
static LAST_TIME: Mutex<Option<f32>> = Mutex::new(None);

/// Когда писали отчёт в прошлый раз.
static LAST_REPORT: Mutex<Option<Instant>> = Mutex::new(None);

/// Сколько нажатий было проглочено на момент прошлого отчёта.
static SWALLOWED_SEEN: AtomicUsize = AtomicUsize::new(0);

/// Сколько прыжков отправлено.
static JUMPS: AtomicU32 = AtomicU32::new(0);

/// Включает блокировку пробела.
///
/// Вызывать из своего потока после загрузки движка: функция не блокирует кадр.
pub fn start() {
    match input::install() {
        Ok(()) => golden_sdk::log!("перехват Key_Event включён, пробел блокируется"),
        Err(error) => golden_sdk::log!("перехват Key_Event не поставлен: {error}"),
    }

    let mut scheduler = match SCHEDULER.lock() {
        Ok(scheduler) => scheduler,
        Err(_) => return,
    };
    let blocked = scheduler.block_key(KEY);
    scheduler.after_millis(25, "echo [scheduler] пробел заблокирован");
    golden_sdk::log!("пробел заблокирован: {blocked}");
}

/// Двигает очередь: звать каждый вызов `StudioDrawModel`, кадр определяется
/// внутри по клиентскому времени.
///
/// `StudioDrawModel` зовётся на каждую модель, то есть несколько раз за кадр,
/// поэтому кадровые действия выполняются только при смене
/// [`EngineState::time`]. В главном меню моделей нет — кадров от рендерера там
/// тоже нет, и это нормально: прыгать в меню некуда.
pub fn tick() -> bool {
    let Ok(mut scheduler) = SCHEDULER.lock() else {
        return false;
    };

    let mut applied = 0;
    if new_frame() {
        drive_jump();
        applied += scheduler.tick_frame();
    }
    applied += scheduler.tick();

    if applied > 0 {
        golden_sdk::log!(
            "выполнено {applied}, в очереди {}, кадр {}",
            scheduler.len(),
            scheduler.frames()
        );
    }
    report(&scheduler);
    applied > 0
}

/// Новый кадр: клиентское время сменилось.
fn new_frame() -> bool {
    let time = EngineState::time();
    let Ok(mut last) = LAST_TIME.lock() else {
        return false;
    };
    match *last {
        Some(previous) if previous == time => false,
        _ => {
            *last = Some(time);
            true
        }
    }
}

/// Прыжок: колесо в окно игры, пока пробел зажат и мы на земле.
///
/// Прыгает движок, а не мы: `WM_MOUSEWHEEL` → `Key_Event(MWHEELUP)` → бинд
/// `+jump` → кнопка в `usercmd` текущего кадра. Поэтому на кадре приземления
/// прыжок успевает произойти раньше, чем трение следующего кадра срежет
/// скорость, и разгон сохраняется без всякого автострейфа.
fn drive_jump() {
    if !EngineState::in_game() || !EngineState::on_ground() {
        return;
    }
    if !input::is_down(KEY) {
        return;
    }
    if input::scroll(input::WHEEL_UP) {
        JUMPS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Пишет в лог скорость, состояние игрока и счётчики.
fn report(scheduler: &Scheduler) {
    let due = match LAST_REPORT.lock() {
        Ok(mut last) => {
            let due = last.map(|last| last.elapsed() >= PERIOD).unwrap_or(true);
            if due {
                *last = Some(Instant::now());
            }
            due
        }
        Err(_) => false,
    };
    if !due {
        return;
    }
    let total = input::blocker().swallowed();
    let previous = SWALLOWED_SEEN.swap(total, Ordering::Relaxed);
    golden_sdk::log!(
        "прыжков {}, скорость {:.0}, проглочено {} нажатий за {:.1} с, на земле {}, vz {:.1}, в игре {}, кадр {}",
        JUMPS.load(Ordering::Relaxed),
        EngineState::velocity().length(),
        total.saturating_sub(previous),
        PERIOD.as_secs_f64(),
        EngineState::on_ground(),
        EngineState::velocity().z,
        EngineState::in_game(),
        scheduler.frames()
    );
}
