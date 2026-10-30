//! Отложенные консольные команды без блокировки кадра.
//!
//! Задача, которая возникает постоянно: отправить команду сейчас, а другую —
//! через N миллисекунд. Спать в потоке рендера нельзя (кадр встанет, ввод
//! потеряется), спавнить поток на каждую пару команд — тоже перебор. Поэтому
//! здесь очередь с дедлайнами, которую двигает сам кадр: [`Scheduler::tick`]
//! вызывается из перехвата отрисовки, проверяет время и отдаёт созревшие строки
//! в командный буфер движка. Если ничего не созрело, вызов возвращает `0`
//! за пару наносекунд — кадр не ждёт.
//!
//! ```no_run
//! use std::time::Duration;
//! use golden_std::Scheduler;
//!
//! let mut scheduler = Scheduler::new();
//! scheduler.after(Duration::ZERO, "echo start");
//! scheduler.after_millis(25, "echo 25ms");
//!
//! // ... в каждом кадре:
//! while scheduler.tick() > 0 {}
//! ```
//!
//! Дедлайны считаются по [`std::time::Instant`], а не по игровому времени:
//! игровое время замирает в меню и на паузе, а отложенная команда должна уйти
//! по реальным часам.

use std::time::{Duration, Instant};

use golden_sdk::engine::run_command;

/// Одна отложенная команда.
#[derive(Debug, Clone)]
struct Pending {
    due: Instant,
    command: String,
}

/// Очередь отложенных команд.
#[derive(Debug, Default)]
pub struct Scheduler {
    pending: Vec<Pending>,
    /// Сколько команд отправлено за всё время — для отчётов и тестов.
    sent: usize,
}

impl Scheduler {
    /// Пустая очередь.
    pub const fn new() -> Self {
        Self { pending: Vec::new(), sent: 0 }
    }

    /// Ставит `command` в очередь с задержкой `delay` от текущего момента.
    ///
    /// `Duration::ZERO` — отправить на ближайшем кадре.
    pub fn after(&mut self, delay: Duration, command: impl Into<String>) {
        self.pending.push(Pending {
            due: Instant::now() + delay,
            command: command.into(),
        });
    }

    /// То же в миллисекундах: `scheduler.after_millis(25, "-jump")`.
    pub fn after_millis(&mut self, millis: u64, command: impl Into<String>) {
        self.after(Duration::from_millis(millis), command);
    }

    /// Ставит несколько команд с одинаковой задержкой.
    ///
    /// Удобно, когда движку нужно отдать связку одной строкой:
    /// `scheduler.after_millis(0, ["-jump", "+jump"])`.
    pub fn after_each<I, S>(&mut self, delay: Duration, commands: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for command in commands {
            self.after(delay, command);
        }
    }

    /// Отправляет всё, что уже созрело. Возвращает число отправленных команд.
    ///
    /// Не блокирует: ни `sleep`, ни ожидания. Порядок отправки — порядок
    /// добавления, даже если дедлайны совпали.
    pub fn tick(&mut self) -> usize {
        if self.pending.is_empty() {
            return 0;
        }
        let now = Instant::now();
        let mut sent = 0;
        let mut index = 0;
        while index < self.pending.len() {
            if self.pending[index].due <= now {
                let task = self.pending.remove(index);
                if run_command(&task.command) {
                    sent += 1;
                }
            } else {
                index += 1;
            }
        }
        self.sent += sent;
        sent
    }

    /// Сколько команд ещё ждёт своего времени.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Очередь пуста.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Сколько команд отправлено за всё время жизни очереди.
    pub fn sent(&self) -> usize {
        self.sent
    }

    /// Ближайший дедлайн — чтобы вызывающий мог пропускать `tick`, если рано.
    pub fn next_due(&self) -> Option<Instant> {
        self.pending.iter().map(|task| task.due).min()
    }

    /// Сбрасывает очередь, ничего не отправляя.
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scheduler_does_nothing() {
        let mut scheduler = Scheduler::new();
        assert_eq!(scheduler.tick(), 0);
        assert!(scheduler.is_empty());
        assert_eq!(scheduler.next_due(), None);
    }

    #[test]
    fn deadline_is_respected() {
        let mut scheduler = Scheduler::new();
        scheduler.after_millis(50, "echo later");
        assert_eq!(scheduler.len(), 1);
        // Сразу — не созрело, но и не заблокировало вызов.
        let started = Instant::now();
        assert_eq!(scheduler.tick(), 0);
        assert!(started.elapsed() < Duration::from_millis(20));
        assert_eq!(scheduler.len(), 1);
    }

    #[test]
    fn zero_delay_is_due_immediately() {
        let mut scheduler = Scheduler::new();
        scheduler.after(Duration::ZERO, "echo now");
        assert_eq!(scheduler.tick(), 0); // движка в тесте нет — `run_command` вернёт false
        assert!(scheduler.is_empty());
        assert_eq!(scheduler.sent(), 0);
    }

    #[test]
    fn commands_keep_insertion_order() {
        let mut scheduler = Scheduler::new();
        scheduler.after_each(Duration::ZERO, ["-jump", "+jump"]);
        assert_eq!(scheduler.len(), 2);
        assert_eq!(
            scheduler
                .pending
                .iter()
                .map(|task| task.command.as_str())
                .collect::<Vec<_>>(),
            vec!["-jump", "+jump"]
        );
    }

    #[test]
    fn clear_drops_pending() {
        let mut scheduler = Scheduler::new();
        scheduler.after_millis(1000, "echo");
        scheduler.clear();
        assert!(scheduler.is_empty());
    }
}