//! Отложенные действия без блокировки кадра.
//!
//! Задача, которая возникает постоянно: что-то сделать сейчас, а другое —
//! через N миллисекунд или через N кадров. Спать в потоке рендера нельзя (кадр
//! встанет, ввод потеряется), спавнить поток на каждую пару действий — тоже
//! перебор. Поэтому здесь очередь с дедлайнами, которую двигает сам кадр:
//! [`Scheduler::tick`] проверяет время, [`Scheduler::tick_frame`] — номер
//! кадра. Если ничего не созрело, вызов возвращает `0` за пару наносекунд —
//! кадр не ждёт.
//!
//! Два вида дедлайнов нужны потому, что в GoldSrc команда живёт один кадр, а не
//! одну миллисекунду. Две команды, отправленные в одном кадре, выполняются в
//! одном проходе командного буфера — и, например, `-jump` с `+jump` схлопнутся,
//! и движок увидит кнопку зажатой. Кадровый дедлайн (`after_frames`) разводит
//! их по разным кадрам, что и требуется.
//!
//! Действия:
//!
//! * [`Scheduler::after`] — строка в командный буфер движка;
//! * [`Scheduler::block_key`] — не пускать клавишу в игру совсем
//!   (перехват `Key_Event`, см. [`crate::input`]);
//! * [`Scheduler::unblock_key`] — снова пускать;
//! * [`Scheduler::press_key`] / [`Scheduler::lift_key`] — нажать и отпустить
//!   клавишу от лица игры, как настоящую.
//!
//! ```no_run
//! use std::time::Duration;
//! use golden_std::{Scheduler, input};
//!
//! let mut scheduler = Scheduler::new();
//! scheduler.block_key(input::VK_SPACE);     // пробел игрока не доходит до игры
//! scheduler.after_frames(0, "-jump");       // кадр 0: снять кнопку
//! scheduler.after_frames(1, "+jump");       // кадр 1: нажать — прыжок случится
//! scheduler.after_frames(2, "-jump");
//! scheduler.after_millis(25, "echo готово");
//!
//! // ... в каждом кадре:
//! scheduler.tick_frame();
//! scheduler.tick();
//! ```

use std::time::{Duration, Instant};

use golden_sdk::engine::run_command;

use crate::input;

/// Когда выполнять действие.
#[derive(Debug, Clone, Copy)]
enum Due {
    /// По реальным часам: игровое время стоит в меню и на паузе, а «через
    /// 25 мс» должно значить 25 мс.
    At(Instant),
    /// По номеру кадра: команды, которым нужен свой кадр.
    Frame(u64),
}

/// Одно отложенное действие.
#[derive(Debug, Clone)]
enum Action {
    /// Строка в командный буфер движка.
    Command(String),
    /// Не пускать клавишу в игру.
    Block(u32),
    /// Снова пускать клавишу в игру.
    Unblock(u32),
    /// Нажать клавишу от лица игры.
    Press(u32),
    /// Отпустить клавишу от лица игры.
    Lift(u32),
}

impl Action {
    /// Выполняет действие. `false` — движок/перехват ещё не готовы.
    fn apply(&self) -> bool {
        match self {
            Self::Command(command) => run_command(command),
            Self::Block(vk) => input::blocker().suppress(*vk),
            Self::Unblock(vk) => input::blocker().release(*vk),
            Self::Press(vk) => input::press(*vk),
            Self::Lift(vk) => input::lift(*vk),
        }
    }
}

/// Одно отложенное действие с дедлайном.
#[derive(Debug, Clone)]
struct Pending {
    due: Due,
    action: Action,
}

/// Очередь отложенных действий.
#[derive(Debug, Default)]
pub struct Scheduler {
    pending: Vec<Pending>,
    /// Клавиши, которые эта очередь сейчас держит заблокированными.
    held: Vec<u32>,
    /// Сколько кадров прошло через [`Self::tick_frame`].
    frames: u64,
    /// Сколько действий выполнено за всё время — для отчётов и тестов.
    applied: usize,
}

impl Scheduler {
    /// Пустая очередь.
    pub const fn new() -> Self {
        Self {
            pending: Vec::new(),
            held: Vec::new(),
            frames: 0,
            applied: 0,
        }
    }

    // --- командный буфер ---

    /// Ставит `command` в очередь с задержкой `delay` от текущего момента.
    ///
    /// `Duration::ZERO` — отправить на ближайшем `tick`. Строка уходит в
    /// `Cbuf_AddText` с дописанным `'\n'`; `';'` тоже разделяет команды, но обе
    /// половинки исполнятся в одном кадре — для разнесения по кадрам
    /// [`Self::after_frames`].
    pub fn after(&mut self, delay: Duration, command: impl Into<String>) {
        self.push_time(delay, Action::Command(command.into()));
    }

    /// То же в миллисекундах: `scheduler.after_millis(25, "-jump")`.
    pub fn after_millis(&mut self, millis: u64, command: impl Into<String>) {
        self.after(Duration::from_millis(millis), command);
    }

    /// Ставит `command` через `frames` кадров (см. [`Self::tick_frame`]).
    ///
    /// `0` — на ближайшем кадре, `1` — на следующем после него. Это и есть
    /// способ развести `-jump` и `+jump`: в одном кадре движок увидит кнопку
    /// зажатой и прыжок не разрешит.
    pub fn after_frames(&mut self, frames: u32, command: impl Into<String>) {
        self.push_frame(frames, Action::Command(command.into()));
    }

    /// Ставит несколько команд с одинаковой задержкой по времени.
    pub fn after_each<I, S>(&mut self, delay: Duration, commands: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for command in commands {
            self.after(delay, command);
        }
    }

    // --- клавиши ---

    /// Блокирует клавишу `vk` на ближайшем `tick`.
    ///
    /// Пока блокировка действует, `Key_Event` с этим кодом не доходит до игры:
    /// ни биндов, ни массива нажатых клавиш. Чат и консоль — исключение: там
    /// клавиша нужна для текста.
    ///
    /// `true` — перехват стоит, значит клавиша действительно перестанет доходить
    /// до игры. Без перехвата (см. [`crate::input::install`]) возвращает
    /// `false`: молча «заблокировать в пустоту» хуже внятного отказа.
    pub fn block_key(&mut self, vk: u32) -> bool {
        self.block_key_after(Duration::ZERO, vk)
    }

    /// Блокирует клавишу `vk` через `delay`.
    pub fn block_key_after(&mut self, delay: Duration, vk: u32) -> bool {
        if vk as usize >= input::KEYS {
            return false;
        }
        self.push_time(delay, Action::Block(vk));
        input::installed()
    }

    /// Блокирует клавишу на `hold` и снимает блокировку через `delay + hold`.
    pub fn block_key_for(&mut self, delay: Duration, vk: u32, hold: Duration) -> bool {
        if vk as usize >= input::KEYS {
            return false;
        }
        let queued = self.block_key_after(delay, vk);
        self.unblock_key_after(delay + hold, vk);
        queued
    }

    /// Снова пускает клавишу `vk` в игру на ближайшем `tick`.
    pub fn unblock_key(&mut self, vk: u32) -> bool {
        self.unblock_key_after(Duration::ZERO, vk)
    }

    /// Снова пускает клавишу `vk` в игру через `delay`.
    pub fn unblock_key_after(&mut self, delay: Duration, vk: u32) -> bool {
        if vk as usize >= input::KEYS {
            return false;
        }
        self.push_time(delay, Action::Unblock(vk));
        input::installed()
    }

    /// Нажимает клавишу `vk` от лица игры на ближайшем `tick`.
    ///
    /// Срабатывают настоящие бинды: для пробела движок сам положит в буфер
    /// `+jump 32`. Отпустить нужно парным [`Self::lift_key`] — иначе движок
    /// будет считать клавишу зажатой и следующий прыжок не разрешит.
    pub fn press_key(&mut self, vk: u32) -> bool {
        self.press_key_after(Duration::ZERO, vk)
    }

    /// Нажимает клавишу `vk` через `delay`.
    pub fn press_key_after(&mut self, delay: Duration, vk: u32) -> bool {
        if vk as usize >= input::KEYS {
            return false;
        }
        self.push_time(delay, Action::Press(vk));
        input::installed()
    }

    /// Отпускает клавишу `vk` от лица игры на ближайшем `tick`.
    pub fn lift_key(&mut self, vk: u32) -> bool {
        self.lift_key_after(Duration::ZERO, vk)
    }

    /// Отпускает клавишу `vk` через `delay`.
    pub fn lift_key_after(&mut self, delay: Duration, vk: u32) -> bool {
        if vk as usize >= input::KEYS {
            return false;
        }
        self.push_time(delay, Action::Lift(vk));
        input::installed()
    }

    /// Нажимает клавишу и отпускает её через `hold` кадров.
    ///
    /// `0` — отпустить на следующем кадре: движок успевает увидеть нажатие, но
    /// кнопка не остаётся зажатой.
    pub fn tap_key(&mut self, vk: u32, hold_frames: u32) -> bool {
        if vk as usize >= input::KEYS {
            return false;
        }
        let pressed = self.press_key(vk);
        self.push_frame(hold_frames, Action::Lift(vk));
        pressed
    }

    fn push_time(&mut self, delay: Duration, action: Action) {
        self.pending.push(Pending {
            due: Due::At(Instant::now() + delay),
            action,
        });
    }

    fn push_frame(&mut self, frames: u32, action: Action) {
        let due = Due::Frame(self.frames + 1 + u64::from(frames));
        self.pending.push(Pending { due, action });
    }

    // --- исполнение ---

    /// Выполняет всё, что созрело по часам. Возвращает число выполненных
    /// действий.
    ///
    /// Не блокирует: ни `sleep`, ни ожиданий. Звать можно сколько угодно часто.
    pub fn tick(&mut self) -> usize {
        let now = Instant::now();
        self.run(|due| matches!(due, Due::At(at) if at <= now))
    }

    /// Отмечает новый кадр и выполняет то, что назначено на этот кадр.
    ///
    /// Звать **ровно один раз за кадр**. В GoldSrc `StudioDrawModel` зовётся на
    /// каждую модель, поэтому кадр нужно определять самому — например, по
    /// `EngineState::time()`: он меняется один раз за кадр.
    pub fn tick_frame(&mut self) -> usize {
        self.frames += 1;
        let current = self.frames;
        self.run(|due| matches!(due, Due::Frame(frame) if frame <= current))
    }

    fn run(&mut self, due: impl Fn(Due) -> bool) -> usize {
        if self.pending.is_empty() {
            return 0;
        }
        let mut applied = 0;
        let mut index = 0;
        while index < self.pending.len() {
            if due(self.pending[index].due) {
                let task = self.pending.remove(index);
                if task.action.apply() {
                    applied += 1;
                    self.remember(&task.action);
                }
            } else {
                index += 1;
            }
        }
        self.applied += applied;
        applied
    }

    /// Запоминает, какие клавиши очередь держит сама, чтобы [`Self::clear`]
    /// мог их отпустить.
    fn remember(&mut self, action: &Action) {
        match action {
            Action::Block(vk) => {
                if !self.held.contains(vk) {
                    self.held.push(*vk);
                }
            }
            Action::Unblock(vk) => self.held.retain(|held| held != vk),
            Action::Command(_) | Action::Press(_) | Action::Lift(_) => {}
        }
    }

    /// Сколько действий ещё ждёт своего времени.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Очередь пуста.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Сколько действий выполнено за всё время жизни очереди.
    pub fn applied(&self) -> usize {
        self.applied
    }

    /// Сколько кадров прошло через [`Self::tick_frame`].
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Клавиши, которые очередь держит заблокированными прямо сейчас.
    pub fn held_keys(&self) -> &[u32] {
        &self.held
    }

    /// Ближайший дедлайн по часам — чтобы вызывающий мог пропускать `tick`.
    pub fn next_due(&self) -> Option<Instant> {
        self.pending
            .iter()
            .filter_map(|task| match task.due {
                Due::At(at) => Some(at),
                Due::Frame(_) => None,
            })
            .min()
    }

    /// Сбрасывает очередь, ничего не выполняет и отпускает все клавиши, которые
    /// держала сама: иначе выгруженный хук оставил бы клавишу нажатой.
    pub fn clear(&mut self) {
        self.pending.clear();
        for vk in self.held.drain(..) {
            input::blocker().release(vk);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scheduler_does_nothing() {
        let mut scheduler = Scheduler::new();
        assert_eq!(scheduler.tick(), 0);
        assert_eq!(scheduler.tick_frame(), 0);
        assert!(scheduler.is_empty());
        assert_eq!(scheduler.next_due(), None);
    }

    #[test]
    fn time_deadline_is_respected() {
        let mut scheduler = Scheduler::new();
        scheduler.after_millis(50, "echo later");
        let started = Instant::now();
        assert_eq!(scheduler.tick(), 0);
        assert!(started.elapsed() < Duration::from_millis(20));
        assert_eq!(scheduler.len(), 1);
    }

    #[test]
    fn frame_deadline_counts_frames() {
        let mut scheduler = Scheduler::new();
        scheduler.after_frames(1, "echo frame");
        // Кадр 0 не наступил: ещё ничего.
        assert_eq!(scheduler.tick(), 0);
        assert_eq!(scheduler.len(), 1);
        // Первый кадр — задача с due=frame 2 ещё рано... проверяем счётчик.
        scheduler.tick_frame();
        assert_eq!(scheduler.frames(), 1);
        assert_eq!(scheduler.len(), 1);
        scheduler.tick_frame();
        assert_eq!(scheduler.frames(), 2);
        // Движка нет в тесте, поэтому задача выполнится «в пустоту» и уйдёт.
        assert_eq!(scheduler.tick(), 0);
        assert!(scheduler.is_empty());
    }

    #[test]
    fn frame_zero_fires_on_first_frame() {
        let mut scheduler = Scheduler::new();
        scheduler.after_frames(0, "echo now");
        scheduler.tick_frame();
        assert!(scheduler.is_empty());
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
                .map(|task| match &task.action {
                    Action::Command(command) => command.as_str(),
                    _ => "",
                })
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

    #[test]
    fn key_block_queues_block_and_unblock() {
        let mut scheduler = Scheduler::new();
        scheduler.block_key_for(
            Duration::from_millis(50),
            input::VK_SPACE,
            Duration::from_millis(200),
        );
        assert_eq!(scheduler.len(), 2);
        assert_eq!(scheduler.tick(), 0);
        assert!(scheduler.held_keys().is_empty());
    }

    #[test]
    fn clear_keeps_nothing_held() {
        let mut scheduler = Scheduler::new();
        scheduler.block_key(input::VK_SPACE);
        scheduler.clear();
        assert!(scheduler.is_empty());
        assert!(scheduler.held_keys().is_empty());
    }

    #[test]
    fn out_of_range_key_is_rejected() {
        let mut scheduler = Scheduler::new();
        assert!(!scheduler.block_key(0x1FF));
        assert!(!scheduler.tap_key(0x1FF, 1));
        assert!(scheduler.is_empty());
    }

    #[test]
    fn tap_key_queues_press_and_lift() {
        let mut scheduler = Scheduler::new();
        scheduler.tap_key(input::VK_SPACE, 2);
        assert_eq!(scheduler.len(), 2);
        assert!(matches!(scheduler.pending[0].action, Action::Press(_)));
        assert!(matches!(scheduler.pending[1].action, Action::Lift(_)));
        assert!(matches!(scheduler.pending[1].due, Due::Frame(3)));
    }
}
