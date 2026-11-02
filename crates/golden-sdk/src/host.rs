//! Внешние операции над процессом игры: запуск, ожидание, остановка, инъекция.
//!
//! Всё это нужно тестам и стенду: поднять локальную игру с ботами, дождаться
//! окна, загрузить свою DLL, потом погасить процесс. SDK делает это напрямую
//! через Win32 (`CreateProcessA`, `OpenProcess`, `CreateRemoteThread`), без
//! `std::process`, потому что SDK не зависит от `std`.
//!
//! ```no_run
//! use golden_sdk::host::Child;
//!
//! # fn example() {
//! let child = Child::launch("hl.exe", "-console +map de_dust2", ".").expect("игра");
//! if child.wait_for_window(30_000).is_some() {
//!     // окно есть — можно инжектить свою DLL
//! }
//! child.terminate();
//! # }
//! ```

use alloc::format;
use alloc::vec::Vec;
use core::ffi::c_void;

use crate::sync::SpinLock;

/// `CREATE_NEW_CONSOLE` — у игры своя консоль, не наша.
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
/// `PROCESS_ALL_ACCESS`.
const PROCESS_ALL_ACCESS: u32 = 0x001F_0FFF;
/// `MEM_COMMIT | MEM_RESERVE`.
const MEM_COMMIT_RESERVE: u32 = 0x0000_1000 | 0x0000_2000;
/// `PAGE_READWRITE`.
const PAGE_READWRITE: u32 = 0x04;
/// `INFINITE`.
const INFINITE: u32 = 0xFFFF_FFFF;
/// `WAIT_OBJECT_0`.
const WAIT_OBJECT_0: u32 = 0;
/// `STILL_ACTIVE`.
const STILL_ACTIVE: u32 = 259;

#[repr(C)]
#[derive(Debug, Default)]
struct StartupInfo {
    cb: u32,
    reserved: *mut u8,
    desktop: *mut u8,
    title: *mut u8,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_chars: u32,
    y_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    reserved2: u16,
    reserved3: *mut u8,
    stdin: isize,
    stdout: isize,
    stderr: isize,
}

#[repr(C)]
#[derive(Debug, Default)]
struct ProcessInformation {
    process: isize,
    thread: isize,
    process_id: u32,
    thread_id: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateProcessA(
        application: *const u8,
        command_line: *mut u8,
        process_attributes: *mut c_void,
        thread_attributes: *mut c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *mut c_void,
        current_directory: *const u8,
        startup_info: *mut StartupInfo,
        process_info: *mut ProcessInformation,
    ) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
    fn TerminateProcess(process: isize, code: u32) -> i32;
    fn CloseHandle(handle: isize) -> i32;
    fn WaitForSingleObject(handle: isize, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(process: isize, code: *mut u32) -> i32;
    fn VirtualAllocEx(
        process: isize,
        addr: *mut c_void,
        size: usize,
        allocation: u32,
        protect: u32,
    ) -> *mut c_void;
    fn WriteProcessMemory(
        process: isize,
        addr: *mut c_void,
        buf: *const c_void,
        size: usize,
        written: *mut usize,
    ) -> i32;
    fn CreateRemoteThread(
        process: isize,
        attributes: *mut c_void,
        stack: usize,
        start: *mut c_void,
        param: *mut c_void,
        flags: u32,
        id: *mut u32,
    ) -> isize;
    fn GetModuleHandleA(name: *const u8) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn GetExitCodeThread(thread: isize, code: *mut u32) -> i32;
}

/// Ошибка запуска или инъекции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostError {
    /// `CreateProcessA` не смог запустить процесс (нет файла, плохая строка).
    Spawn,
    /// Процесс не открылся с нужными правами.
    OpenProcess,
    /// Не удалось выделить память в чужом процессе.
    Allocate,
    /// Не удалось записать путь DLL в чужой процесс.
    Write,
    /// `CreateRemoteThread` не создал поток.
    RemoteThread,
    /// `LoadLibraryA` в чужом процессе вернул `0`: DLL не нашлась или не загрузилась.
    LoadLibrary,
}

impl core::fmt::Display for HostError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Self::Spawn => "не удалось запустить процесс",
            Self::OpenProcess => "не удалось открыть процесс",
            Self::Allocate => "не удалось выделить память в процессе",
            Self::Write => "не удалось записать путь DLL в процесс",
            Self::RemoteThread => "не удалось создать поток в процессе",
            Self::LoadLibrary => "LoadLibrary в процессе не загрузил DLL",
        };
        f.write_str(text)
    }
}

impl core::error::Error for HostError {}

/// Запущенный процесс игры.
#[derive(Debug)]
pub struct Child {
    process: isize,
    pid: u32,
}

impl Child {
    /// Запускает `exe` с аргументами `args` в каталоге `dir`.
    ///
    /// `args` — уже собранная строка командной строки (без имени файла).
    pub fn launch(exe: &str, args: &str, dir: &str) -> Result<Self, HostError> {
        let mut exe_bytes = exe.as_bytes().to_vec();
        exe_bytes.push(0);
        let mut dir_bytes = dir.as_bytes().to_vec();
        dir_bytes.push(0);

        // В `lpCommandLine` первым идёт имя файла: так процесс видит привычный
        // `GetCommandLine`, а путь к exe остаётся отдельным аргументом.
        let mut command = format!("\"{exe}\" {args}");
        let mut command_bytes = command.as_bytes().to_vec();
        command.push('\0');
        command_bytes.push(0);

        let mut startup = StartupInfo {
            cb: core::mem::size_of::<StartupInfo>() as u32,
            ..StartupInfo::default()
        };
        let mut info = ProcessInformation::default();
        // SAFETY: все буферы NUL-терминированы и живут до конца вызова.
        let ok = unsafe {
            CreateProcessA(
                exe_bytes.as_ptr(),
                command_bytes.as_mut_ptr(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                0,
                CREATE_NEW_CONSOLE,
                core::ptr::null_mut(),
                dir_bytes.as_ptr(),
                &mut startup,
                &mut info,
            )
        };
        if ok == 0 {
            return Err(HostError::Spawn);
        }
        // Дескриптор потока не нужен, процесс держим до конца жизни.
        if info.thread != 0 {
            // SAFETY: дескриптор наш, закрываем один раз.
            unsafe { CloseHandle(info.thread) };
        }
        Ok(Self {
            process: info.process,
            pid: info.process_id,
        })
    }

    /// PID запущенного процесса.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Процесс ещё жив.
    pub fn is_running(&self) -> bool {
        let mut code = 0u32;
        // SAFETY: дескриптор процесса открыт в `launch`.
        let ok = unsafe { GetExitCodeProcess(self.process, &mut code) };
        ok != 0 && code == STILL_ACTIVE
    }

    /// Ждёт завершения не дольше `timeout_ms`. `true` — процесс завершился.
    pub fn wait(&self, timeout_ms: u32) -> bool {
        // SAFETY: дескриптор процесса открыт в `launch`.
        let result = unsafe { WaitForSingleObject(self.process, timeout_ms) };
        result == WAIT_OBJECT_0
    }

    /// Ждёт, пока у процесса появится видимое окно. Возвращает его дескриптор.
    pub fn wait_for_window(&self, timeout_ms: u32) -> Option<usize> {
        let deadline = crate::time::now_ms().saturating_add(timeout_ms as u64);
        loop {
            if let Some(window) = window_of(self.pid) {
                return Some(window);
            }
            if crate::time::now_ms() >= deadline {
                return None;
            }
            crate::time::sleep_ms(50);
        }
    }

    /// Завершает процесс.
    pub fn terminate(&self) -> bool {
        // SAFETY: дескриптор процесса открыт в `launch`.
        unsafe { TerminateProcess(self.process, 0) != 0 }
    }

    /// Окно процесса прямо сейчас (первое видимое с разумным размером).
    pub fn window(&self) -> Option<usize> {
        window_of(self.pid)
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if self.process != 0 {
            if self.is_running() {
                self.terminate();
            }
            // SAFETY: дескриптор наш, закрываем один раз.
            unsafe { CloseHandle(self.process) };
        }
    }
}

/// Загружает `dll` в процесс `pid` через `CreateRemoteThread(LoadLibraryA)`.
///
/// Тот же приём, что у любого инжектора: путь к DLL копируется в чужое
/// адресное пространство, затем в процессе вызывается `LoadLibraryA`.
pub fn inject(pid: u32, dll: &str) -> Result<(), HostError> {
    // SAFETY: права проверяет система; дескриптор закрываем ниже.
    let process = unsafe { OpenProcess(PROCESS_ALL_ACCESS, 0, pid) };
    if process == 0 {
        return Err(HostError::OpenProcess);
    }
    let result = inject_with(process, dll);
    // SAFETY: дескриптор наш.
    unsafe { CloseHandle(process) };
    result
}

fn inject_with(process: isize, dll: &str) -> Result<(), HostError> {
    let mut path: Vec<u8> = dll.as_bytes().to_vec();
    path.push(0);

    // SAFETY: размер и права корректны; адрес проверяем на null.
    let memory = unsafe {
        VirtualAllocEx(
            process,
            core::ptr::null_mut(),
            path.len(),
            MEM_COMMIT_RESERVE,
            PAGE_READWRITE,
        )
    };
    if memory.is_null() {
        return Err(HostError::Allocate);
    }

    let mut written = 0usize;
    // SAFETY: буфер и размер — наши, адрес выделен выше.
    let ok = unsafe {
        WriteProcessMemory(
            process,
            memory,
            path.as_ptr().cast(),
            path.len(),
            &mut written,
        )
    };
    if ok == 0 || written != path.len() {
        return Err(HostError::Write);
    }

    let mut name = [0u8; 13];
    name[..11].copy_from_slice(b"LoadLibrary");
    // `LoadLibraryA` — 12 символов плюс NUL.
    name[11] = b'A';
    // SAFETY: kernel32 загружен в наш процесс, имя функции NUL-терминировано.
    let kernel = unsafe { GetModuleHandleA(c"kernel32.dll".as_ptr().cast()) };
    if kernel.is_null() {
        return Err(HostError::RemoteThread);
    }
    // SAFETY: `name` NUL-терминирован, модуль валиден.
    let load = unsafe { GetProcAddress(kernel, name.as_ptr()) };
    if load.is_null() {
        return Err(HostError::RemoteThread);
    }

    // SAFETY: адрес и параметр принадлежат целевому процессу, права есть.
    let thread = unsafe {
        CreateRemoteThread(
            process,
            core::ptr::null_mut(),
            0,
            load,
            memory,
            0,
            core::ptr::null_mut(),
        )
    };
    if thread == 0 {
        return Err(HostError::RemoteThread);
    }
    // Ждём загрузки и проверяем результат: `LoadLibraryA` возвращает `0`, если
    // DLL не нашлась или не загрузилась. Молчаливое «инъекция прошла» здесь хуже
    // всего — потом ищешь, почему мод не отчитался в лог.
    let mut exit_code = 0u32;
    // SAFETY: дескриптор потока наш, код выхода — `u32`.
    unsafe {
        WaitForSingleObject(thread, INFINITE);
        GetExitCodeThread(thread, &mut exit_code);
        CloseHandle(thread);
    }
    if exit_code == 0 {
        return Err(HostError::LoadLibrary);
    }
    Ok(())
}

/// Первое видимое окно процесса `pid` с клиентской областью разумного размера.
pub fn window_of(pid: u32) -> Option<usize> {
    let found = SpinLock::new(0usize);
    let target = SpinLock::new(pid as usize);
    // SAFETY: колбэк соответствует `WNDENUMPROC`.
    unsafe {
        EnumWindows(
            visit_window,
            &mut WindowSearch {
                found: &found,
                target: &target,
            } as *mut _ as isize,
        )
    };
    let window = *found.lock();
    (window != 0).then_some(window)
}

/// `WM_KEYDOWN` / `WM_KEYUP` / `WM_CHAR`.
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_CHAR: u32 = 0x0102;
/// `VK_RETURN` — Enter, `VK_ESCAPE` — Escape.
pub const VK_RETURN: u32 = 0x0D;
/// `VK_ESCAPE`.
pub const VK_ESCAPE: u32 = 0x1B;

#[link(name = "user32")]
unsafe extern "system" {
    fn PostMessageA(window: usize, message: u32, wparam: usize, lparam: isize) -> i32;
    fn MapVirtualKeyA(code: u32, map: u32) -> u32;
}

/// Нажимает или отпускает клавишу в окне игры.
///
/// Игра получает обычное сообщение окна — то же, что пришло бы от клавиатуры,
/// поэтому работает и в консоли, и в меню. `lparam` собирается как у настоящего
/// нажатия: скан-код в старшем слове и бит повтора, иначе GoldSrc его отбросит.
pub fn post_key(window: usize, down: bool, vk: u32) -> bool {
    // SAFETY: `MapVirtualKeyA` — чистая функция user32.
    let scan = unsafe { MapVirtualKeyA(vk, 0) };
    let lparam: isize = if down {
        1 | ((scan as isize) << 16)
    } else {
        1 | ((scan as isize) << 16) | (1isize << 30) | (1isize << 31)
    };
    let message = if down { WM_KEYDOWN } else { WM_KEYUP };
    // SAFETY: дескриптор окна получен из `window_of`/`wait_for_window`.
    unsafe { PostMessageA(window, message, vk as usize, lparam) != 0 }
}

/// Отправляет в окно один символ (`WM_CHAR`) — так текст попадает в консоль игры.
pub fn post_char(window: usize, ch: u8) -> bool {
    // SAFETY: дескриптор окна получен из `window_of`/`wait_for_window`.
    unsafe { PostMessageA(window, WM_CHAR, ch as usize, 1) != 0 }
}

/// Состояние поиска окна: один проход `EnumWindows`.
struct WindowSearch<'a> {
    found: &'a SpinLock<usize>,
    target: &'a SpinLock<usize>,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(callback: unsafe extern "system" fn(usize, isize) -> i32, param: isize) -> i32;
    fn GetWindowThreadProcessId(window: usize, pid: *mut u32) -> u32;
    fn IsWindowVisible(window: usize) -> i32;
    fn GetClientRect(window: usize, rect: *mut Rect) -> i32;
}

#[repr(C)]
#[derive(Debug, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// # Safety
/// Вызывается `EnumWindows` с валидным дескриптором окна и нашим указателем.
unsafe extern "system" fn visit_window(window: usize, param: isize) -> i32 {
    // SAFETY: `param` — указатель на `WindowSearch`, живущий до конца перебора.
    let search = unsafe { &*(param as *const WindowSearch<'_>) };
    // SAFETY: стандартные вызовы user32 по дескриптору из перебора.
    unsafe {
        if IsWindowVisible(window) == 0 {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        if pid as usize != *search.target.lock() {
            return 1;
        }
        let mut rect = Rect::default();
        if GetClientRect(window, &mut rect) == 0 {
            return 1;
        }
        if rect.right - rect.left < 16 || rect.bottom - rect.top < 16 {
            return 1;
        }
        *search.found.lock() = window;
        0
    }
}
