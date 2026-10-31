//! Примитивы синхронизации без `std`.
//!
//! SDK живёт внутри процесса игры и не тянет `std`; мьютекс здесь нужен только
//! для коротких критических секций (запись строки в лог, флаг инициализации),
//! поэтому спинлок уместнее системного мьютекса: держать его дольше пары
//! микросекунд нельзя, и «заснуть» в потоке рендера всё равно нечем.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

/// Простой спинлок: занят — крутимся в `spin_loop`.
///
/// # Safety
/// Реализация `Sync` корректна: доступ к `T` идёт только через [`SpinGuard`],
/// который освобождает флаг в `Drop`.
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: см. документацию типа — доступ сериализуется флагом.
unsafe impl<T: Send> Sync for SpinLock<T> {}
// SAFETY: `UnsafeCell` — единственное не-`Send` поле, а сам он передаётся целиком.
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// Новый незанятый замок.
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Захватывает замок, крутясь на месте, пока он занят.
    pub fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SpinGuard { lock: self }
    }

    /// Пробует захватить замок без ожидания.
    pub fn try_lock(&self) -> Option<SpinGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| SpinGuard { lock: self })
    }
}

impl<T> core::fmt::Debug for SpinLock<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SpinLock")
            .field("locked", &self.locked.load(Ordering::Relaxed))
            .finish()
    }
}

/// Захваченный [`SpinLock`]: отпускает замок при разрушении.
pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: замок захвачен, других ссылок на значение нет.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: замок захвачен, других ссылок на значение нет.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

impl<T> core::fmt::Debug for SpinGuard<'_, T>
where
    T: core::fmt::Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&**self, f)
    }
}