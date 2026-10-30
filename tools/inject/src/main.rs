//! Крошечный инжектор: `CreateRemoteThread` + `LoadLibraryA`.
//!
//! Нужен, чтобы проверять сборки в живой игре без лаунчера. Ничего не скрывает
//! и не обходит защиты: это обычное `LoadLibrary`, тот же вызов, что делает любой
//! загрузчик плагинов.
//!
//! ```text
//! golden-inject <pid|имя.exe> <путь к dll>
//! ```
//!
//! Собирать под **i686**: инжектор должен быть той же разрядности, что и цель
//! (`hl.exe` — 32-битный), иначе адрес `LoadLibraryA` в чужом процессе неверный.

use std::ffi::CString;
use std::process::ExitCode;

use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
};
use windows_sys::Win32::System::Threading::{
    CreateRemoteThread, GetExitCodeThread, OpenProcess, PROCESS_CREATE_THREAD,
    PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_WRITE, WaitForSingleObject,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: golden-inject <pid|process.exe> <path to dll>");
        return ExitCode::from(2);
    }
    let pid = match args[0].parse::<u32>() {
        Ok(pid) => pid,
        Err(_) => match find_pid(&args[0]) {
            Some(pid) => pid,
            None => {
                eprintln!("процесс {} не найден", args[0]);
                return ExitCode::from(1);
            }
        },
    };

    match inject(pid, &args[1]) {
        Ok(()) => {
            println!("загружено в pid {pid}: {}", args[1]);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("ошибка: {err}");
            ExitCode::from(1)
        }
    }
}

/// Ищет pid по имени процесса (`hl.exe` без учёта регистра).
fn find_pid(name: &str) -> Option<u32> {
    let output = std::process::Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let wanted = name.to_ascii_lowercase();
    for line in text.lines() {
        let mut fields = line.split("\",\"");
        let image = fields.next()?.trim_matches('"').to_ascii_lowercase();
        let pid = fields.next()?.trim_matches('"');
        if image == wanted {
            return pid.parse().ok();
        }
    }
    None
}

fn inject(pid: u32, dll: &str) -> Result<(), String> {
    let path = CString::new(dll).map_err(|_| "путь содержит NUL".to_string())?;
    let bytes = path.as_bytes_with_nul();

    // SAFETY: все вызовы Win32 ниже — с проверкой результатов; буфер живёт до конца.
    unsafe {
        let process = OpenProcess(
            PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_WRITE,
            0,
            pid,
        );
        if process.is_null() {
            return Err(format!("OpenProcess({pid}): {}", std::io::Error::last_os_error()));
        }

        let remote = VirtualAllocEx(process, std::ptr::null(), bytes.len(), MEM_COMMIT, PAGE_READWRITE);
        if remote.is_null() {
            CloseHandle(process);
            return Err(format!("VirtualAllocEx: {}", std::io::Error::last_os_error()));
        }

        let mut written = 0usize;
        let ok = WriteProcessMemory(process, remote, bytes.as_ptr().cast(), bytes.len(), &mut written);
        if ok == 0 || written != bytes.len() {
            VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            CloseHandle(process);
            return Err(format!("WriteProcessMemory: {}", std::io::Error::last_os_error()));
        }

        let kernel32 = GetModuleHandleA(c"kernel32.dll".as_ptr().cast());
        let load_library = GetProcAddress(kernel32, c"LoadLibraryA".as_ptr().cast());
        let Some(load_library) = load_library else {
            VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            CloseHandle(process);
            return Err("нет LoadLibraryA в kernel32".to_string());
        };

        let thread = CreateRemoteThread(
            process,
            std::ptr::null(),
            0,
            Some(std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                unsafe extern "system" fn(*mut core::ffi::c_void) -> u32,
            >(load_library)),
            remote,
            0,
            std::ptr::null_mut(),
        );
        if thread.is_null() {
            VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            CloseHandle(process);
            return Err(format!("CreateRemoteThread: {}", std::io::Error::last_os_error()));
        }

        let wait = WaitForSingleObject(thread, 15_000);
        let mut code: u32 = 0;
        GetExitCodeThread(thread, &mut code);
        CloseHandle(thread);
        VirtualFreeEx(process, remote, 0, MEM_RELEASE);
        CloseHandle(process);

        if wait != WAIT_OBJECT_0 {
            return Err("поток не завершился за 15 с".to_string());
        }
        if code == 0 {
            return Err("LoadLibraryA вернул NULL (DLL не загрузилась)".to_string());
        }
    }
    Ok(())
}