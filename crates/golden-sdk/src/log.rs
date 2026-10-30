//! Лог в файл и в отладчик — без внешних зависимостей.
//!
//! Файл лежит рядом с DLL, потому что у процесса игры может не быть консоли, а
//! `OutputDebugString` видно только из отладчика.
//!
//! Живёт в SDK, а не в каждом примере: любой мод хочет логировать одинаково.

use std::ffi::CString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Инициализирует лог. Повторные вызовы безопасны.
pub fn init(path: PathBuf) {
    let _ = LOG_PATH.set(path.clone());
    let file = OpenOptions::new().create(true).append(true).open(path).ok();
    let _ = LOG_FILE.set(Mutex::new(file));
}

/// Путь к файлу лога, если лог инициализирован.
pub fn path() -> Option<&'static PathBuf> {
    LOG_PATH.get()
}

/// Пишет строку в лог-файл и в отладчик.
pub fn line(message: impl AsRef<str>) {
    let text = format!("[{}] {}\n", timestamp(), message.as_ref());

    if let Some(cell) = LOG_FILE.get() {
        if let Ok(mut guard) = cell.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.write_all(text.as_bytes());
                let _ = file.flush();
            }
        }
    }

    if let Ok(cstr) = CString::new(text.trim_end().replace('\0', "")) {
        // SAFETY: строка NUL-терминирована и живёт до конца вызова.
        unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringA(cstr.as_ptr().cast());
        }
    }
}

/// Имя лог-файла рядом с DLL.
pub const FILE_NAME: &str = "golden-client.log";

/// Путь к логу рядом с модулем `module` (HMODULE) — используется в `DllMain`.
pub fn path_next_to(module: *mut core::ffi::c_void) -> PathBuf {
    let mut buf = [0u8; 260];
    // SAFETY: буфер и его длина переданы корректно.
    let len = unsafe {
        windows_sys::Win32::System::LibraryLoader::GetModuleFileNameA(
            module,
            buf.as_mut_ptr(),
            buf.len() as u32,
        )
    } as usize;
    if len == 0 || len >= buf.len() {
        return PathBuf::from(FILE_NAME);
    }
    let mut path = PathBuf::from(String::from_utf8_lossy(&buf[..len]).into_owned());
    path.pop();
    path.push(FILE_NAME);
    path
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Простое HH:MM:SS из секунд суток — хватает для лога.
    let day = secs % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, (day % 3600) / 60, day % 60)
}