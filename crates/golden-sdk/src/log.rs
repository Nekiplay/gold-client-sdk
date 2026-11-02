//! Лог в файл и в отладчик — без `std` и без внешних зависимостей.
//!
//! Файл лежит рядом с DLL, потому что у процесса игры может не быть консоли, а
//! `OutputDebugString` видно только из отладчика.
//!
//! Живёт в SDK, а не в каждом примере: любой мод хочет логировать одинаково.
//! Вместо `std::fs` — прямой `CreateFileA`/`WriteFile`, вместо `SystemTime` —
//! `GetLocalTime`: SDK не тянет `std` вообще.

use alloc::string::{String, ToString};
use core::ffi::c_void;

use crate::sync::SpinLock;

/// Имя лог-файла рядом с DLL.
pub const FILE_NAME: &str = "golden-client.log";

/// `GENERIC_WRITE`.
const GENERIC_WRITE: u32 = 0x4000_0000;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE`.
const FILE_SHARE_READ_WRITE: u32 = 0x0000_0003;
/// `OPEN_ALWAYS` — создать или открыть существующий.
const OPEN_ALWAYS: u32 = 4;
/// `FILE_END` — позицию ставим от конца файла.
const FILE_END: u32 = 2;
/// `INVALID_HANDLE_VALUE`.
const INVALID_HANDLE: isize = -1;

/// `SYSTEMTIME` из Win32 — только нужные поля, порядок как в заголовке.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileA(
        name: *const u8,
        access: u32,
        share: u32,
        security: *mut c_void,
        creation: u32,
        flags: u32,
        template: isize,
    ) -> isize;
    fn WriteFile(
        handle: isize,
        buf: *const u8,
        len: u32,
        written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn SetFilePointer(handle: isize, distance: i32, high: *mut i32, method: u32) -> u32;
    fn CloseHandle(handle: isize) -> i32;
    fn GetLocalTime(time: *mut SystemTime);
    fn GetModuleFileNameA(module: *mut c_void, buf: *mut u8, size: u32) -> u32;
    fn OutputDebugStringA(text: *const u8);
}

/// Открытый лог-файл (`0` — не открыт).
static LOG_FILE: SpinLock<isize> = SpinLock::new(0);

/// Путь, по которому открыт лог.
static LOG_PATH: SpinLock<Option<String>> = SpinLock::new(None);

/// Инициализирует лог. Повторные вызовы безопасны.
pub fn init(path: impl Into<String>) {
    let path = path.into();
    let mut bytes = path.as_bytes().to_vec();
    if bytes.contains(&0) {
        return;
    }
    bytes.push(0);
    // SAFETY: строка NUL-терминирована, буфер живёт до конца вызова.
    let handle = unsafe {
        CreateFileA(
            bytes.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ_WRITE,
            core::ptr::null_mut(),
            OPEN_ALWAYS,
            0,
            0,
        )
    };
    if handle == INVALID_HANDLE || handle == 0 {
        return;
    }
    // `OPEN_ALWAYS` открывает существующий файл с позицией 0 — без этого лог
    // каждой следующей загрузки затирал бы начало предыдущего.
    // SAFETY: дескриптор наш и валиден, `FILE_END` = 2, смещение 0.
    unsafe { SetFilePointer(handle, 0, core::ptr::null_mut(), FILE_END) };
    let mut slot = LOG_FILE.lock();
    if *slot != 0 {
        // SAFETY: закрываем прежний дескриптор, он наш.
        unsafe { CloseHandle(*slot) };
    }
    *slot = handle;
    *LOG_PATH.lock() = Some(path);
}

/// Путь к файлу лога, если лог инициализирован.
pub fn path() -> Option<String> {
    LOG_PATH.lock().clone()
}

/// Закрывает лог-файл.
///
/// Нужен при выгрузке DLL: дескриптор живёт в процессе игры, и без закрытия он
/// останется за нами до её перезапуска. После вызова пишем только в отладчик.
pub fn close() {
    let mut slot = LOG_FILE.lock();
    if *slot != 0 {
        // SAFETY: дескриптор открыт нами и больше не используется.
        unsafe { CloseHandle(*slot) };
        *slot = 0;
    }
    drop(slot);
    *LOG_PATH.lock() = None;
}

/// Пишет строку в лог-файл и в отладчик.
pub fn line(message: impl AsRef<str>) {
    let text = alloc::format!("[{}] {}\n", timestamp(), message.as_ref());

    {
        let handle = *LOG_FILE.lock();
        if handle != 0 {
            let mut written = 0u32;
            // SAFETY: дескриптор открыт на запись, буфер и длина корректны.
            unsafe {
                // Позицию ставим на конец перед каждой строкой: несколько модов
                // пишут в один файл через свои дескрипторы, и у каждого свой
                // указатель — без этого строки затирали бы друг друга.
                SetFilePointer(handle, 0, core::ptr::null_mut(), FILE_END);
                WriteFile(
                    handle,
                    text.as_ptr(),
                    text.len() as u32,
                    &mut written,
                    core::ptr::null_mut(),
                );
            }
        }
    }

    let mut bytes: alloc::vec::Vec<u8> = text.trim_end().bytes().filter(|b| *b != 0).collect();
    bytes.push(0);
    // SAFETY: строка NUL-терминирована и живёт до конца вызова.
    unsafe { OutputDebugStringA(bytes.as_ptr()) };
}

/// Путь к логу рядом с модулем `module` (HMODULE) — используется в `DllMain`.
pub fn path_next_to(module: *mut core::ffi::c_void) -> String {
    let mut buf = [0u8; 260];
    // SAFETY: буфер и его длина переданы корректно.
    let len = unsafe { GetModuleFileNameA(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if len == 0 || len >= buf.len() {
        return FILE_NAME.to_string();
    }
    let path = String::from_utf8_lossy(&buf[..len]).into_owned();
    match path.rfind(['\\', '/']) {
        Some(cut) => alloc::format!("{}{FILE_NAME}", &path[..=cut]),
        None => FILE_NAME.to_string(),
    }
}

/// Время `HH:MM:SS` по локальным часам — хватает для лога.
fn timestamp() -> String {
    let mut time = SystemTime::default();
    // SAFETY: структура наша, вызов только заполняет её.
    unsafe { GetLocalTime(&mut time) };
    alloc::format!("{:02}:{:02}:{:02}", time.hour, time.minute, time.second)
}
