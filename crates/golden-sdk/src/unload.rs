//! Выгрузка мода из процесса игры без перезапуска.
//!
//! Порядок такой: сначала снимаются хуки и отпускаются клавиши, потом
//! освобождается сама DLL. Если выгрузить DLL с включённым хуком, движок
//! прыгнет в освобождённую страницу и процесс упадёт, поэтому выгрузка — это
//! всегда «отписаться, потом уйти».
//!
//! Кто такие отписки: любой модуль SDK и любой пример может отдать функцию
//! очистки через [`on_unload`]. Они вызываются в обратном порядке регистрации
//! (последним пришёл — первым ушёл), ровно один раз.
//!
//! ```no_run
//! use golden_sdk::unload;
//!
//! // Очистка — обычная функция: она снимает хук, забирая его из статика.
//! fn remove_hooks() {
//!     // DETOUR.lock().unwrap().take();
//! }
//!
//! fn main() {
//!     unload::on_unload(remove_hooks);
//!     unload::unload(0);
//! }
//! ```
//!
//! Для модов на `golden-std` удобнее [`golden_std::unload`]: он умеет то же
//! самое плюс снимает перехват ввода и закрывает лог.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::module;
use crate::sync::SpinLock;

/// Очистка при выгрузке: обычная функция без аргументов и результата.
pub type Cleanup = fn();

/// Сколько очисток помещается в реестр.
pub const MAX_CLEANUPS: usize = 32;

/// Реестр очисток. Массив, а не `Vec`: SDK не обязан аллоцировать ради пары
/// функций, а переполнение просто означает «отказано».
static CLEANUPS: SpinLock<[Option<Cleanup>; MAX_CLEANUPS]> =
    SpinLock::new([None; MAX_CLEANUPS]);

/// Очистки уже выполнены — повторный вызов ничего не делает.
static RAN: AtomicBool = AtomicBool::new(false);

/// Регистрирует очистку, которая выполнится при выгрузке.
///
/// Возвращает `false`, если места в реестре нет или очистки уже выполнены.
pub fn on_unload(cleanup: Cleanup) -> bool {
    if RAN.load(Ordering::Acquire) {
        return false;
    }
    let mut slots = CLEANUPS.lock();
    for slot in slots.iter_mut() {
        if slot.is_none() {
            *slot = Some(cleanup);
            return true;
        }
    }
    false
}

/// Сколько очисток зарегистрировано.
pub fn count() -> usize {
    CLEANUPS.lock().iter().filter(|slot| slot.is_some()).count()
}

/// Выполняет все очистки в обратном порядке регистрации.
///
/// Возвращает число выполненных очисток; повторный вызов вернёт `0`.
pub fn run_cleanups() -> usize {
    if RAN.swap(true, Ordering::AcqRel) {
        return 0;
    }
    let mut done = 0;
    // Снимаем копию списка под замком, а сами очистки зовём уже без него:
    // очистка может захотеть залогировать строку, а лог берёт свой замок.
    let mut pending: [Option<Cleanup>; MAX_CLEANUPS] = [None; MAX_CLEANUPS];
    {
        let mut slots = CLEANUPS.lock();
        for (index, slot) in slots.iter_mut().enumerate() {
            pending[index] = slot.take();
        }
    }
    for slot in pending.iter_mut().rev() {
        if let Some(cleanup) = slot.take() {
            cleanup();
            done += 1;
        }
    }
    done
}

/// Выполнены ли очистки.
pub fn finished() -> bool {
    RAN.load(Ordering::Acquire)
}

/// Выгружает текущую DLL: очистки, затем `FreeLibraryAndExitThread` в отдельном
/// потоке через [`module::DEFAULT_DELAY_MS`].
///
/// Возвращает `false`, если свой модуль не найден или поток не создался.
pub fn unload(exit_code: u32) -> bool {
    unload_after(module::DEFAULT_DELAY_MS, exit_code)
}

/// То же, что [`unload`], но с явной задержкой перед освобождением памяти.
///
/// После снятия хуков будим фоновые потоки, ждём их выхода и только потом
/// освобождаем модуль. Поток, оставшийся внутри DLL, после выгрузки исполнял бы
/// освобождённую память — отсюда зависание игры.
pub fn unload_after(delay_ms: u32, exit_code: u32) -> bool {
    module::request_stop();
    run_cleanups();
    if !module::wait_threads(WAIT_THREADS_MS) {
        crate::log!(
            "выгрузка: потоки мода не вышли ({}, осталось {}), выгружаем всё равно",
            WAIT_THREADS_MS,
            module::threads()
        );
    }
    if !module::wait_idle(WAIT_IDLE_MS) {
        crate::log!(
            "выгрузка: в нашем коде остались потоки ({}), выгружаем всё равно",
            module::in_flight()
        );
    }
    module::unload_self_after(delay_ms, exit_code)
}

/// Сколько ждать выхода фоновых потоков мода.
pub const WAIT_THREADS_MS: u32 = 2_000;

/// Сколько ждать выхода потоков из кода модуля перед освобождением памяти.
pub const WAIT_IDLE_MS: u32 = 1_000;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static ORDER: AtomicUsize = AtomicUsize::new(0);
    static FIRST: AtomicUsize = AtomicUsize::new(0);
    static SECOND: AtomicUsize = AtomicUsize::new(0);

    fn first() {
        FIRST.store(ORDER.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
    }

    fn second() {
        SECOND.store(ORDER.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
    }

    // Единственный тест на реестр: он глобальный и одноразовый, поэтому чужие
    // тесты сюда не лезут.
    #[test]
    fn cleanups_run_once_in_reverse_order() {
        assert!(on_unload(first));
        assert!(on_unload(second));
        assert!(!finished());

        assert_eq!(run_cleanups(), 2);
        assert!(finished());
        assert_eq!(FIRST.load(Ordering::SeqCst), 2);
        assert_eq!(SECOND.load(Ordering::SeqCst), 1);

        // Повтор ничего не делает и новые очистки уже не принимаются.
        assert_eq!(run_cleanups(), 0);
        assert!(!on_unload(first));
        assert_eq!(count(), 0);
    }
}