//! Консоль Gold Client: приёмник строк истории.
//!
//! Свои строки движок печатает через `Con_Printf` в `hw.dll`, но консоль
//! Gold Client — своя, клиентская: она держит историю в памяти как текст
//! UTF-16 и рисует её поверх игры. Всё, что попадает в неё — и вывод движка,
//! и эхо набранной команды — проходит через один приёмник в `GameUI.dll`:
//! он разбирает `#`-цвета и переводит строку в UTF-16.
//!
//! перехват этого приёмника — единственный надёжный способ не дать строке
//! попасть на экран: печать `hw.dll` про эхо ничего не знает, потому что
//! строку набирает и показывает клиент.
//!
//! ```ignore
//! use golden_std::{console, hook};
//!
//! hook! {
//!     unsafe extern "thiscall" fn on_line(this: *mut c_void, text: *const u8)
//!         original: on_line_original
//!     {
//!         if !console::hidden(text) {
//!             unsafe { on_line_original()(this, text) }
//!         }
//!     }
//! }
//! ```

use core::ffi::c_void;

/// Смещение приёмника строк в `GameUI.dll` — тот, что вызывают и движок, и
/// консоль клиента.
///
/// Проверяется по прологу [`ADDRESS`]: если в этой сборке игры по смещению
/// лежит другая функция, [`add_line`] вернёт `None` и перехват не поставится.
pub const ADD_LINE_RVA: usize = 0x000E_8F20;

/// `push ebp; mov ebp, esp` — пролог приёмника в этой сборке.
const PROLOGUE: [u8; 3] = [0x55, 0x8B, 0xEC];

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleA(name: *const u8) -> *mut c_void;
}

/// База модуля по имени файла.
///
/// `None`, если модуль не загружен — например, пока не открыт интерфейс игры.
pub fn module_base(name: &str) -> Option<usize> {
    // Имя должно быть с завершающим нулём: API читает строку до него.
    let mut buf = [0u8; 64];
    let bytes = name.as_bytes();
    if bytes.len() + 1 > buf.len() {
        return None;
    }
    buf[..bytes.len()].copy_from_slice(bytes);
    // SAFETY: буфер с нулём на конце, API только читает строку.
    let handle = unsafe { GetModuleHandleA(buf.as_ptr()) };
    (!handle.is_null()).then_some(handle as usize)
}

/// Адрес приёмника строк консоли.
///
/// `None`, если `GameUI.dll` ещё не загружен или его сборка не совпадает с
/// той, под которую посчитано [`ADD_LINE_RVA`].
pub fn add_line() -> Option<usize> {
    let base = module_base("GameUI.dll")?;
    let address = base.checked_add(ADD_LINE_RVA)?;
    // SAFETY: адрес внутри образа загруженного модуля.
    let head = unsafe { core::slice::from_raw_parts(address as *const u8, PROLOGUE.len()) };
    (head == PROLOGUE).then_some(address)
}

/// Текст строки по указателю.
///
/// Приёмник получает строку в ANSI и сам переводит её в UTF-16, поэтому
/// читаем как байты до нуля. `None`, если указатель пуст или строка
/// не помещается в [`MAX_LINE`] — такую всё равно не сравнить.
///
/// # Safety
/// `text` должен указывать на строку с завершающим нулём.
pub unsafe fn line_bytes(text: *const u8) -> Option<&'static [u8]> {
    if text.is_null() {
        return None;
    }
    let mut len = 0usize;
    // SAFETY: контракт функции — строка с нулём на конце.
    while len < MAX_LINE && unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    if len == MAX_LINE {
        return None;
    }
    // SAFETY: прочитали ровно `len` байт строки.
    Some(unsafe { core::slice::from_raw_parts(text, len) })
}

/// Сколько байт строки консоли разбираем.
pub const MAX_LINE: usize = 1024;