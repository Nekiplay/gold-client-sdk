//! Демонстрация [`golden_std::Scheduler`]: цепочка команд с задержкой 25 мс.
//!
//! Кадр двигает очередь: в перехвате `StudioDrawModel` вызывается
//! [`demo::tick`], который раз в [`demo::PERIOD`] ставит в очередь две команды —
//! немедленную и отложенную на 25 мс, — и тут же отправляет созревшие. Ни одна
//! задержка не превращается в `sleep`: функция либо отправляет команду, либо
//! сразу возвращает управление, поэтому ни рендер, ни обработка ввода не ждут.
//!
//! В лог пишется измеренная задержка отправки второй команды, чтобы точность
//! была видна числом, а не на словах.
//!
//! ## Границы
//!
//! Пример намеренно работает на `echo`-командах: он проверяет механику
//! планировщика, а не поведение игрока. Привязку к движению (прыжок по флагу
//! `on_ground`) здесь нет — это автоматизация игры за живого игрока, правила
//! проекта её не включают.
//!
//! ```text
//! 14:22:31.418 [scheduler] цепочка: 2 команды через 0 и 25 мс
//! 14:22:31.419 [scheduler] отправлено 1 (echo [scheduler] первая) +0.4 мс
//! 14:22:31.443 [scheduler] отправлено 1 (echo [scheduler] вторая) +25.7 мс
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use golden_std::Scheduler;

/// Как часто запускать новую цепочку.
pub const PERIOD: Duration = Duration::from_secs(2);

/// Задержка второй команды — та самая, что не должна блокировать кадр.
pub const DELAY: Duration = Duration::from_millis(25);

/// Очередь команд.
static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

/// Когда поставили текущую цепочку — для измерения фактической задержки.
static CHAIN_START: Mutex<Option<Instant>> = Mutex::new(None);

/// Сколько цепочек уже отработало.
static CHAINS: AtomicUsize = AtomicUsize::new(0);

/// Планирует новую цепочку и сразу отправляет то, что уже созрело.
///
/// Вызывать из перехвата кадра: функция не блокирует поток.
pub fn start_chain() {
    let mut scheduler = match SCHEDULER.lock() {
        Ok(scheduler) => scheduler,
        Err(_) => return,
    };

    if let Ok(mut start) = CHAIN_START.lock() {
        *start = Some(Instant::now());
    }
    scheduler.after(Duration::ZERO, "echo [scheduler] первая команда");
    scheduler.after(DELAY, "echo [scheduler] вторая команда через 25 мс");
    golden_sdk::log!(
        "цепочка: 2 команды через 0 и {} мс",
        DELAY.as_millis()
    );
    dispatch(&mut scheduler);
}

/// Отправляет созревшие команды. `true`, если что-то ушло.
pub fn tick() -> bool {
    let mut scheduler = match SCHEDULER.lock() {
        Ok(scheduler) => scheduler,
        Err(_) => return false,
    };
    dispatch(&mut scheduler) > 0
}

fn dispatch(scheduler: &mut Scheduler) -> usize {
    let sent = scheduler.tick();
    if sent == 0 {
        return 0;
    }
    let elapsed = CHAIN_START
        .lock()
        .ok()
        .and_then(|start| *start)
        .map(|start| start.elapsed())
        .unwrap_or_default();
    let percent = elapsed.as_secs_f64() * 100.0 / DELAY.as_secs_f64();
    golden_sdk::log!(
        "отправлено {sent} за {:.1} мс ({:.0}% от {} мс), в очереди {}",
        elapsed.as_secs_f64() * 1000.0,
        percent,
        DELAY.as_millis(),
        scheduler.len()
    );
    if scheduler.is_empty() {
        CHAINS.fetch_add(1, Ordering::Relaxed);
    }
    sent
}

/// Сколько цепочек отработало — для отчёта из основного потока.
pub fn chains() -> usize {
    CHAINS.load(Ordering::Relaxed)
}