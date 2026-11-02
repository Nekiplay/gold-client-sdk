//! Загруженный модуль: кто мы, откуда и как выгрузиться.
//!
//! Моду для мода — это сама DLL. Движку и стенду её надо уметь опознать
//! (`Module::current`), чтобы положить лог рядом, а нам — выгрузить без
//! перезапуска игры (`Module::free` и [`unload_self_after`]).
//!
//! Выгрузка изнутри процесса делается только через
//! `FreeLibraryAndExitThread`: `FreeLibrary` сам по себе выгрузил бы код,
//! который в этот момент исполняется на стеке, а `FreeLibraryAndExitThread`
//! возвращает управление в `kernel32`, уже не заходя в нашу память.
//!
//! Выгружать нужно **после** снятия хуков: иначе движок вызовет освобождённую
//! функцию. Для этого есть реестр очистки — [`crate::unload`].

use alloc::string::String;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

/// `GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS`.
const FROM_ADDRESS: u32 = 0x0000_0004;
/// `GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT` — не увеличивать счётчик
/// ссылок: выгружать надо ровно ту ссылку, которую дал инжектор.
const UNCHANGED_REFCOUNT: u32 = 0x0000_0002;
/// `INVALID_HANDLE_VALUE` для `CreateThread` — 0.
const NO_HANDLE: isize = 0;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleExA(flags: u32, name: *const u8, module: *mut *mut c_void) -> i32;
    fn GetModuleFileNameA(module: *mut c_void, buf: *mut u8, size: u32) -> u32;
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn FreeLibraryAndExitThread(module: *mut c_void, code: u32) -> !;
    fn CreateThread(
        attributes: *mut c_void,
        stack_size: usize,
        start: unsafe extern "system" fn(*mut c_void) -> u32,
        param: *mut c_void,
        flags: u32,
        id: *mut u32,
    ) -> isize;
    fn CloseHandle(handle: isize) -> i32;
    fn GetCurrentProcessId() -> u32;
    fn GetCurrentThreadId() -> u32;
}

/// Загруженный модуль (в Win32 — `HMODULE`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Module(*mut c_void);

impl core::fmt::Debug for Module {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Module")
            .field("handle", &(self.0 as usize))
            .finish()
    }
}

impl Module {
    /// Модуль, в чьём коде лежит `address`.
    ///
    /// Счётчик ссылок не меняется: полученный handle — это «свой» модуль, а не
    /// новая ссылка на него, поэтому его нельзя отдать в `FreeLibrary` больше
    /// одного раза.
    pub fn from_address(address: usize) -> Option<Self> {
        if address == 0 {
            return None;
        }
        let mut module: *mut c_void = core::ptr::null_mut();
        // SAFETY: адрес внутри нашего кода, буфер под результат наш.
        let ok = unsafe {
            GetModuleHandleExA(
                FROM_ADDRESS | UNCHANGED_REFCOUNT,
                address as *const u8,
                &mut module,
            )
        };
        if ok == 0 || module.is_null() {
            return None;
        }
        Some(Self(module))
    }

    /// Модуль, в котором исполняется этот вызов, — то есть наша DLL.
    pub fn current() -> Option<Self> {
        Self::from_address(Self::current as *const () as usize)
    }

    /// Голый `HMODULE`.
    pub fn handle(&self) -> *mut c_void {
        self.0
    }

    /// Лежит ли `address` внутри этого модуля.
    ///
    /// Нужно перед освобождением: если движок всё ещё держит указатель на наши
    /// данные или код, освобождать модуль нельзя.
    pub fn contains(&self, address: usize) -> bool {
        match Self::from_address(address) {
            Some(other) => other.0 == self.0,
            None => false,
        }
    }

    /// Полный путь к файлу модуля.
    pub fn path(&self) -> Option<String> {
        let mut buf = [0u8; 260];
        // SAFETY: буфер и его длина переданы корректно.
        let len =
            unsafe { GetModuleFileNameA(self.0, buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if len == 0 || len >= buf.len() {
            return None;
        }
        Some(String::from_utf8_lossy(&buf[..len]).into_owned())
    }

    /// Имя файла модуля без каталогов (`example_skeleton.dll`).
    pub fn file_name(&self) -> Option<String> {
        let path = self.path()?;
        let start = path.rfind(['\\', '/']).map(|cut| cut + 1).unwrap_or(0);
        Some(String::from(&path[start..]))
    }

    /// Освобождает одну ссылку на модуль.
    ///
    /// Возвращает `false`, если ОС отказала. **Не вызывайте это для себя из
    /// своего же кода:** выгрузится код, который ещё исполняется на стеке, —
    /// для выгрузки себя используйте [`unload_self_after`].
    pub fn free(self) -> bool {
        // SAFETY: handle получен нами, ссылка ещё не освобождена.
        unsafe { FreeLibrary(self.0) != 0 }
    }
}

/// Код возврата и задержка для потока выгрузки.
///
/// Передаются статиками, потому что `CreateThread` отдаёт потоку один
/// указатель, а нужны два числа.
static EXIT_CODE: AtomicU32 = AtomicU32::new(0);
static DELAY_MS: AtomicU32 = AtomicU32::new(0);

/// Освобождает модуль и завершает текущий поток.
///
/// # Safety
/// Вызывать только из кода, который после этого вызова больше не нужен: после
/// `FreeLibraryAndExitThread` страницы модуля могут быть освобождены, а
/// управление не вернётся.
pub unsafe fn free_and_exit_thread(module: Module, exit_code: u32) -> ! {
    // SAFETY: контракт функции.
    unsafe { FreeLibraryAndExitThread(module.handle(), exit_code) }
}

/// Точка входа потока выгрузки.
///
/// `# Safety`-оговорки те же, что у [`free_and_exit_thread`]: после паузы поток
/// навсегда уходит в `kernel32`, не заходя в код модуля.
unsafe extern "system" fn unload_thread(module: *mut c_void) -> u32 {
    let delay = DELAY_MS.load(Ordering::Relaxed);
    if delay != 0 {
        // Пауза нужна, чтобы игровой поток успел выйти из наших функций перед
        // тем, как их код исчезнет.
        crate::time::sleep_ms(delay);
    }
    let code = EXIT_CODE.load(Ordering::Relaxed);
    // SAFETY: адрес модуля пришёл из `Module::current`.
    unsafe { FreeLibraryAndExitThread(module, code) }
}

/// Запускает в отдельном потоке выгрузку своей DLL через `delay_ms`.
///
/// Задержка по умолчанию спасает от гонки: игровой поток мог в этот момент
/// исполнять наш хук, и выгружать модуль под ним нельзя.
///
/// Возвращает `false`, если не удалось найти свой модуль или создать поток.
pub fn unload_self_after(delay_ms: u32, exit_code: u32) -> bool {
    let Some(module) = Module::current() else {
        return false;
    };
    EXIT_CODE.store(exit_code, Ordering::Relaxed);
    DELAY_MS.store(delay_ms, Ordering::Relaxed);
    // SAFETY: поток запускается на функции из этого же модуля; параметр —
    // `HMODULE`, который жив до выгрузки.
    let thread = unsafe {
        CreateThread(
            core::ptr::null_mut(),
            0,
            unload_thread,
            module.handle(),
            0,
            core::ptr::null_mut(),
        )
    };
    if thread == NO_HANDLE {
        return false;
    }
    // Дескриптор потока нам больше не нужен, сам поток продолжает жить.
    // SAFETY: дескриптор создан нами и не используется дальше.
    unsafe {
        CloseHandle(thread);
    }
    true
}

/// То же, что [`unload_self_after`], с задержкой [`DEFAULT_DELAY_MS`].
pub fn unload_self(exit_code: u32) -> bool {
    unload_self_after(DEFAULT_DELAY_MS, exit_code)
}

/// Задержка по умолчанию перед выгрузкой: три кадра при 60 fps.
pub const DEFAULT_DELAY_MS: u32 = 300;

/// Сколько сейчас потоков исполняет наш код.
static IN_FLIGHT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Потоки мода должны завершиться: выгрузка уже идёт.
static STOP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Сколько потоков мода сейчас живёт.
static THREADS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Просьба остановиться. Фоновые потоки мода проверяют её в цикле:
///
/// ```no_run
/// while !golden_sdk::module::should_stop() {
///     // ... работа кадра
///     golden_sdk::time::sleep_ms(100);
/// }
/// ```
///
/// Поток, который не вышел, останется исполнять код освобождённой DLL — именно
/// из-за этого игра зависает после выгрузки.
pub fn should_stop() -> bool {
    STOP.load(core::sync::atomic::Ordering::Acquire)
}

/// Поднимает флаг остановки. Возвращает `true`, если его подняли впервые.
pub fn request_stop() -> bool {
    !STOP.swap(true, core::sync::atomic::Ordering::AcqRel)
}

/// Отметка «этот поток принадлежит модулю».
///
/// Поток обязан умереть до освобождения DLL, поэтому выгрузка ждёт
/// [`wait_threads`]. Ставится первым делом в теле фонового потока.
#[derive(Debug)]
pub struct ThreadToken(());

impl ThreadToken {
    /// Регистрирует текущий поток.
    pub fn new() -> Self {
        THREADS.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
        Self(())
    }
}

impl Default for ThreadToken {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ThreadToken {
    fn drop(&mut self) {
        THREADS.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
    }
}

/// Сколько потоков мода ещё живо.
pub fn threads() -> usize {
    THREADS.load(core::sync::atomic::Ordering::Acquire)
}

/// Ждёт, пока все фоновые потоки мода выйдут.
///
/// Возвращает `true`, если дождались. Считаются только потоки, взявшие
/// [`ThreadToken::new`]; вызывающий поток токен не берёт, иначе ждал бы сам
/// себя.
pub fn wait_threads(timeout_ms: u32) -> bool {
    let deadline = crate::time::now_ms() + u64::from(timeout_ms);
    loop {
        if threads() == 0 {
            return true;
        }
        if crate::time::now_ms() >= deadline {
            return false;
        }
        crate::time::sleep_ms(1);
    }
}

/// Отметка «этот поток внутри кода DLL».
///
/// Освобождать модуль, пока в нём кто-то есть, нельзя: поток вернётся по адресу,
/// которого уже нет. Поэтому каждый хук берёт [`InFlight::enter`] первым делом,
/// а выгрузка ждёт [`wait_idle`] после снятия хуков.
///
/// ```no_run
/// use golden_sdk::module::InFlight;
///
/// extern "fastcall" fn hook(key: i32, down: i32) {
///     let _inside = InFlight::enter();
///     // ... вызов оригинала
/// }
/// ```
#[derive(Debug)]
pub struct InFlight(());

impl InFlight {
    /// Отмечает вход в код модуля.
    pub fn enter() -> Self {
        IN_FLIGHT.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
        Self(())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
    }
}

/// Сколько потоков сейчас внутри кода модуля.
pub fn in_flight() -> usize {
    IN_FLIGHT.load(core::sync::atomic::Ordering::Acquire)
}

/// Ждёт, пока все потоки выйдут из кода модуля.
///
/// Возвращает `true`, если дождались. `false` — время вышло: кто-то застрял в
/// нашем коде, и выгружать модуль опасно.
pub fn wait_idle(timeout_ms: u32) -> bool {
    let deadline = crate::time::now_ms() + u64::from(timeout_ms);
    loop {
        if in_flight() == 0 {
            return true;
        }
        if crate::time::now_ms() >= deadline {
            return false;
        }
        crate::time::sleep_ms(1);
    }
}

/// Идентификатор процесса игры.
pub fn process_id() -> u32 {
    // SAFETY: вызов без аргументов.
    unsafe { GetCurrentProcessId() }
}

/// Идентификатор текущего потока.
pub fn thread_id() -> u32 {
    // SAFETY: вызов без аргументов.
    unsafe { GetCurrentThreadId() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_own_module_by_address() {
        let module = Module::current().expect("свой модуль находится всегда");
        assert!(!module.handle().is_null());
    }

    #[test]
    fn path_ends_with_file_name() {
        let module = Module::current().expect("свой модуль");
        let name = module.file_name().expect("имя файла");
        let path = module.path().expect("путь");
        assert!(path.ends_with(&name), "{path} не заканчивается на {name}");
    }

    #[test]
    fn null_address_is_rejected() {
        assert!(Module::from_address(0).is_none());
    }

    #[test]
    fn process_and_thread_ids_are_nonzero() {
        assert_ne!(process_id(), 0);
        assert_ne!(thread_id(), 0);
    }
}
