//! Доступ к процессу: базовые адреса модулей и чтение/запись памяти.
//!
//! Всё, что касается «как найти игру», живёт здесь, чтобы функции игры не
//! размазывали `GetModuleHandle` по коду.

use alloc::string::String;
use alloc::vec::Vec;

use core::ffi::c_void;

/// Возвращает базу загруженного модуля по имени (`"hw.dll"`, `"client.dll"`).
///
/// Внутри — `GetModuleHandleA`, никаких перечислений и сканов.
pub fn module_base(name: &str) -> Option<usize> {
    let mut buf = [0u8; 260];
    let bytes = name.as_bytes();
    if bytes.len() >= buf.len() {
        return None;
    }
    buf[..bytes.len()].copy_from_slice(bytes);

    // SAFETY: строка NUL-терминирована (буфер нулевой), вызов только читает.
    let handle =
        unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleA(buf.as_ptr()) };
    (!handle.is_null()).then_some(handle as usize)
}

/// База `hw.dll` (движок).
pub fn hw_base() -> Option<usize> {
    module_base("hw.dll")
}

/// База `client.dll` (клиент CS).
pub fn client_base() -> Option<usize> {
    module_base("client.dll")
}

/// Адрес функции, которую экспортирует загруженный модуль.
///
/// Нужна для `opengl32.dll`: движок достаёт GL-функции через `GetProcAddress`
/// и держит их в своей таблице, поэтому сравнение с настоящим адресом экспорта —
/// единственный способ узнать, какие ячейки этой таблицы — цвет.
pub fn export(module: &str, name: &str) -> Option<usize> {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};

    let mut module_buf = [0u8; 260];
    let module_bytes = module.as_bytes();
    if module_bytes.len() >= module_buf.len() {
        return None;
    }
    module_buf[..module_bytes.len()].copy_from_slice(module_bytes);

    let mut name_buf = [0u8; 128];
    let name_bytes = name.as_bytes();
    if name_bytes.len() >= name_buf.len() {
        return None;
    }
    name_buf[..name_bytes.len()].copy_from_slice(name_bytes);

    // SAFETY: обе строки NUL-терминированы; вызовы только читают.
    unsafe {
        let handle = GetModuleHandleA(module_buf.as_ptr());
        if handle.is_null() {
            return None;
        }
        let proc = GetProcAddress(handle, name_buf.as_ptr());
        proc.map(|f| f as usize)
    }
}

/// Размер образа модуля в памяти (`SizeOfImage` из PE-заголовка).
///
/// # Safety
/// `base` — база загруженного модуля, адрес читается напрямую.
pub unsafe fn module_size(base: usize) -> Option<usize> {
    // SAFETY: контракт передан вызывающим: PE-заголовок по базе модуля.
    unsafe {
        let lfanew: u32 = read(base + 0x3C);
        if lfanew == 0 || lfanew as usize > 0x1000 {
            return None;
        }
        // Смещение `SizeOfImage` в OptionalHeader одинаково для PE32 и PE32+.
        let size: u32 = read(base + lfanew as usize + 0x18 + 0x38);
        (size as usize > 0x1000).then_some(size as usize)
    }
}

/// Можно ли писать в память по адресу (страница выделена и доступна на запись).
///
/// Нужно, чтобы правка таблицы указателей не задела код: значения там могут
/// случайно совпасть с адресом GL-функции, а запись в `.text` — это порча.
pub fn is_writable(addr: usize) -> bool {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};

    let mut info: MEMORY_BASIC_INFORMATION = unsafe { core::mem::zeroed() };
    let size = core::mem::size_of::<MEMORY_BASIC_INFORMATION>();
    // SAFETY: `info` — валидный буфер нужного размера.
    let written = unsafe { VirtualQuery(addr as *const c_void, &mut info, size) };
    if written == 0 {
        return false;
    }
    const PAGE_READWRITE: u32 = 0x04;
    const PAGE_WRITECOPY: u32 = 0x08;
    const PAGE_EXECUTE_READWRITE: u32 = 0x40;
    const PAGE_EXECUTE_WRITECOPY: u32 = 0x80;
    const PROTECTION_MASK: u32 = 0xFF;
    let protection = info.Protect & PROTECTION_MASK;
    info.State == 0x1000
        && matches!(
            protection,
            PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY
        )
}

/// Читает значение типа `T` по адресу.
///
/// # Safety
/// Адрес должен быть валидным и указывать на `T`.
pub unsafe fn read<T: Copy>(addr: usize) -> T {
    // SAFETY: контракт функции передан вызывающему.
    unsafe { *(addr as *const T) }
}

/// Записывает значение по адресу.
///
/// # Safety
/// Адрес должен быть записываемым и указывать на `T`.
pub unsafe fn write<T: Copy>(addr: usize, value: T) {
    // SAFETY: контракт функции передан вызывающему.
    unsafe { *(addr as *mut T) = value }
}

/// Читает указатель по адресу, возвращает `None` для null.
///
/// # Safety
/// См. [`read`].
pub unsafe fn read_ptr(addr: usize) -> Option<usize> {
    // SAFETY: контракт функции передан вызывающему.
    let p = unsafe { read::<*mut c_void>(addr) } as usize;
    (p != 0).then_some(p)
}

/// Читает C-строку не длиннее `max` байт (без NUL в конце).
///
/// Возвращает `None`, если первый байт не читается; обрывает чтение на NUL, на
/// нечитаемой странице или на пределе длины. Нужна для строк в структурах
/// клиента, где длина поля известна (например `teamname[16]`).
pub fn read_cstr(addr: usize, max: usize) -> Option<String> {
    if !is_readable(addr) {
        return None;
    }
    let mut bytes = Vec::with_capacity(max);
    for i in 0..max {
        let at = addr + i;
        if !is_readable(at) {
            break;
        }
        // SAFETY: байт по адресу проверен на читаемость.
        let b = unsafe { read::<u8>(at) };
        if b == 0 {
            break;
        }
        bytes.push(b);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Проверяет, что адрес лежит в диапазоне `[base, base + len)`.
pub fn in_range(addr: usize, base: usize, len: usize) -> bool {
    addr >= base && addr < base.saturating_add(len)
}

/// Можно ли читать память по адресу (страница выделена и доступна на чтение).
///
/// Нужно для диагностики: чужие указатели приходят из игры и могут быть мусором,
/// а падать на чтении по мусорному адресу в чужом процессе — это вылет игры.
pub fn is_readable(addr: usize) -> bool {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};

    const PAGE_READONLY: u32 = 0x02;
    const PAGE_READWRITE: u32 = 0x04;
    const PAGE_WRITECOPY: u32 = 0x08;
    const PAGE_EXECUTE_READ: u32 = 0x20;
    const PAGE_EXECUTE_READWRITE: u32 = 0x40;
    const PAGE_EXECUTE_WRITECOPY: u32 = 0x80;
    const PAGE_GUARD: u32 = 0x100;

    let readable = PAGE_READONLY
        | PAGE_READWRITE
        | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ
        | PAGE_EXECUTE_READWRITE
        | PAGE_EXECUTE_WRITECOPY;
    if addr == 0 {
        return false;
    }
    let mut info = core::mem::MaybeUninit::<MEMORY_BASIC_INFORMATION>::zeroed();
    // SAFETY: структура и её размер переданы корректно, вызов только читает.
    let written = unsafe {
        VirtualQuery(
            addr as *const core::ffi::c_void,
            info.as_mut_ptr(),
            core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if written == 0 {
        return false;
    }
    // SAFETY: `VirtualQuery` вернул ненулевой размер, структура заполнена.
    let info = unsafe { info.assume_init() };
    info.Protect & readable != 0 && info.Protect & PAGE_GUARD == 0
}

/// Проверяет, что функция (адрес кода) лежит в `.text` модуля.
///
/// `.text` в обоих бинарниках начинается с `+0x1000` и заметно меньше мегабайта
/// на клиенте; для проверки хватает грубого диапазона, а точные границы не нужны —
/// задача поймать «смещение уехало на мусор».
pub fn is_code(addr: usize, base: usize) -> bool {
    in_range(addr, base + 0x1000, 0x20_0000)
}
