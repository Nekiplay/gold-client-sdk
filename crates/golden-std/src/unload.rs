//! Выгрузка мода из процесса игры без перезапуска.
//!
//! SDK умеет освободить свою DLL ([`golden_sdk::module`]) и держит реестр
//! очисток ([`golden_sdk::unload`]), но у мода на `golden-std` очисток больше:
//! снять перехват ввода, отпустить зажатые модом клавиши, вернуть интерфейс
//! рендерера, закрыть лог. Всё это делает [`unload`]:
//!
//! 1. очистки, зарегистрированные через [`on_unload`] — в обратном порядке;
//! 2. [`crate::hooks::detach_all`] — все перехваты и подмены интерфейса;
//! 3. [`golden_std::input::shutdown`] — перехват `Key_Event` снимается всегда,
//!    даже если пример про него забыл;
//! 4. [`crate::concommand::shutdown`] — свои консольные команды;
//! 5. очистки SDK;
//! 6. закрытие лог-файла;
//! 7. `FreeLibraryAndExitThread` в отдельном потоке.
//!
//! ```no_run
//! use golden_std::unload;
//!
//! fn main() {
//!     unload::on_unload(|| println!("пока"));
//!     unload::unload();
//! }
//! ```
//!
//! Вызов безопасен из игрового потока: освобождение памяти откладывается на
//! [`golden_sdk::module::DEFAULT_DELAY_MS`], чтобы движок успел выйти из наших
//! функций.

use std::sync::Mutex;

use golden_sdk::{log, module};

/// Очистки, зарегистрированные модом.
///
/// `FnOnce` в боксе: очистка часто забирает себе детур или подмену, а такое
/// можно выполнить только один раз.
static CLEANUPS: Mutex<Vec<Box<dyn FnOnce() + Send>>> = Mutex::new(Vec::new());

/// Выгрузка уже началась: новые очистки не принимаются.
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Защита от повторного запуска освобождения модуля.
static UNLOAD_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Регистрирует очистку, которая выполнится при выгрузке.
///
/// Возвращает `false`, если выгрузка уже началась.
pub fn on_unload(cleanup: impl FnOnce() + Send + 'static) -> bool {
    if STARTED.load(std::sync::atomic::Ordering::Acquire) {
        return false;
    }
    match CLEANUPS.lock() {
        Ok(mut list) => {
            list.push(Box::new(cleanup));
            true
        }
        Err(_) => false,
    }
}

/// Сколько очисток зарегистрировано.
pub fn count() -> usize {
    CLEANUPS.lock().map(|list| list.len()).unwrap_or(0)
}

/// Выполняет очистки в обратном порядке регистрации.
///
/// Возвращает число выполненных; повторный вызов вернёт `0`.
pub fn run_cleanups() -> usize {
    let pending = match CLEANUPS.lock() {
        Ok(mut list) => std::mem::take(&mut *list),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };
    run_all(pending)
}

/// Выполняет готовый список очисток: последняя зарегистрированная уходит первой.
fn run_all(pending: Vec<Box<dyn FnOnce() + Send>>) -> usize {
    let mut done = 0;
    for cleanup in pending.into_iter().rev() {
        // Паника в одной очистке не должна мешать остальным: мод всё равно
        // должен снять хуки, иначе игра упадёт уже по-настоящему.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup)).is_err() {
            log::line("выгрузка: очистка завершилась паникой, продолжаем");
        }
        done += 1;
    }
    done
}

/// Выгружает DLL: очистки, отписка от ввода, закрытие лога, освобождение модуля.
///
/// Возвращает `false`, если свой модуль не найден или поток выгрузки не создался.
pub fn unload() -> bool {
    unload_after(module::DEFAULT_DELAY_MS)
}

/// Выгружает мод из отдельного потока, а не там, где его позвали.
///
/// Нужно для команды в консоли: колбэк исполняется на стеке движка, и снимать
/// регистрацию команды прямо внутри него нельзя — движок вернётся в объект,
/// который мы только что освободили, и напечатает мусор вместо ответа.
pub fn defer_unload(after_ms: u32) -> bool {
    std::thread::Builder::new()
        .name("golden-unload".to_string())
        .spawn(move || {
            // Поток выгрузки не должен ждать сам себя в `wait_threads`.
            drop(golden_sdk::module::ThreadToken::new());
            std::thread::sleep(std::time::Duration::from_millis(150));
            unload_after(after_ms);
        })
        .is_ok()
}

/// Выгружает DLL с явной паузой перед освобождением памяти.
///
/// Большая пауза нужна, если выгрузка запускается прямо из перехвата, который
/// ещё исполняется на стеке игрового потока.
pub fn unload_after(delay_ms: u32) -> bool {
    if UNLOAD_STARTED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return false;
    }
    // Новые очистки после начала выгрузки не принимаем.
    STARTED.store(true, std::sync::atomic::Ordering::Release);
    golden_sdk::log!("выгрузка: снимаем хуки и освобождаем модуль");
    // 1. Потоки мода должны узнать, что пора выходить.
    module::request_stop();
    // 2. Перехваты, ввод, команды и очистки SDK.
    let done = run_cleanups();
    // Перехваты снимаем первыми: пока они стоят, движок может войти в наш код.
    let hooks_off = crate::hooks::detach_all();
    let input_off = crate::input::shutdown();
    let commands_off = crate::concommand::shutdown();
    golden_sdk::unload::run_cleanups();
    // 3. Ждём, пока потоки выйдут и перестанут исполнять наш код.
    let threads_out = module::wait_threads(golden_sdk::unload::WAIT_THREADS_MS);
    let idle = module::wait_idle(golden_sdk::unload::WAIT_IDLE_MS);
    golden_sdk::log!(
        "выгрузка: очисток {done}, перехватов снято: {hooks_off}, \
         перехват ввода снят: {input_off}, команд снято: {commands_off}, \
         потоки вышли: {threads_out} (осталось {}), код пуст: {idle} ({})",
        module::threads(),
        module::in_flight()
    );
    // Если движок после снятия всё ещё держит указатель на нашу таблицу, то
    // после освобождения модуля он прыгнет в ноль. Пишем значение до свободной
    // памяти: по логу сразу видно, вернулся ли интерфейс клиенту.
    let mut free_ok = true;
    if let Some(slot) = golden_sdk::renderer::interface_slot() {
        // SAFETY: слот живёт в `hw.dll` и валиден весь процесс.
        let current = unsafe { *slot };
        golden_sdk::log!(
            "выгрузка: g_pStudioInterface = {:#x} (client base {:#x})",
            current as usize,
            golden_sdk::process::client_base().unwrap_or(0)
        );
        // Указатель внутрь нашей DLL — освобождать нельзя: движок прыгнет в
        // освобождённую память. Тогда остаёмся в процессе.
        if golden_sdk::module::Module::current().is_some_and(|me| me.contains(current as usize)) {
            golden_sdk::log!(
                "выгрузка: движок всё ещё держит нашу таблицу интерфейса — \
                 модуль остаётся в памяти"
            );
            free_ok = false;
        }
    }
    if !free_ok {
        return false;
    }
    // 4. Лог закрываем сами: `FreeLibraryAndExitThread` должен видеть чистый
    //    дескриптор, а писать после этого уже некуда.
    log::close();
    module::unload_self_after(delay_ms, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Список очисток собирается вручную: реестр в модуле глобальный и общий на
    /// весь процесс, а тесты идут параллельно.
    #[test]
    fn run_all_is_reverse_order() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let (a, b, c) = (order.clone(), order.clone(), order.clone());
        let list: Vec<Box<dyn FnOnce() + Send>> = vec![
            Box::new(move || a.lock().unwrap().push(1)),
            Box::new(move || b.lock().unwrap().push(2)),
            Box::new(move || c.lock().unwrap().push(3)),
        ];

        assert_eq!(run_all(list), 3);
        assert_eq!(*order.lock().unwrap(), vec![3, 2, 1]);
    }

    #[test]
    fn panicking_cleanup_does_not_stop_others() {
        static RAN: AtomicUsize = AtomicUsize::new(0);
        let list: Vec<Box<dyn FnOnce() + Send>> = vec![
            Box::new(|| {
                RAN.fetch_add(1, Ordering::SeqCst);
            }),
            Box::new(|| panic!("нарочно")),
        ];

        assert_eq!(run_all(list), 2);
        assert_eq!(RAN.load(Ordering::SeqCst), 1);
    }
}
