//! Win32 и OpenGL 1.1 напрямую: окно, курсор, буфер обмена, резолвер GL-функций.
//!
//! Всё объявлено здесь, чтобы не тянуть `windows-sys` с тяжёлыми фичами:
//! крейту хватает десятка функций user32/kernel32/opengl32.

use std::ffi::c_void;
use std::sync::OnceLock;

/// Хендл окна.
pub type Hwnd = *mut c_void;

/// Оригинальная оконная процедура.
pub type WndProc = unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize;

pub const GWLP_WNDPROC: i32 = -4;
pub const WM_EGUI_CURSOR: u32 = 0x8000 + 0x41;

pub const WM_SETCURSOR: u32 = 0x0020;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_CHAR: u32 = 0x0102;
pub const WM_SYSKEYDOWN: u32 = 0x0104;
pub const WM_SYSKEYUP: u32 = 0x0105;
pub const WM_SYSCHAR: u32 = 0x0106;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;
pub const WM_RBUTTONDBLCLK: u32 = 0x0206;
pub const WM_MBUTTONDOWN: u32 = 0x0207;
pub const WM_MBUTTONUP: u32 = 0x0208;
pub const WM_MBUTTONDBLCLK: u32 = 0x0209;
pub const WM_MOUSEWHEEL: u32 = 0x020A;
pub const WM_XBUTTONDOWN: u32 = 0x020B;
pub const WM_XBUTTONUP: u32 = 0x020C;
pub const WM_XBUTTONDBLCLK: u32 = 0x020E;
pub const WM_MOUSEHWHEEL: u32 = 0x0211;
pub const WM_INPUT: u32 = 0x00FF;

pub const VK_SHIFT: u32 = 0x10;
pub const VK_CONTROL: u32 = 0x11;
pub const VK_MENU: u32 = 0x12;

const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;

// GL_TEXTURE_BIT исключён: движок сам кэширует привязки текстур.
pub const GL_ALL_ATTRIB_BITS: u32 = 0x000B_FFFF;
pub const GL_CLIENT_ALL_ATTRIB_BITS: u32 = 0xFFFF_FFFF;
pub const GL_MODELVIEW: u32 = 0x1700;
pub const GL_PROJECTION: u32 = 0x1701;
pub const GL_TEXTURE: u32 = 0x1702;
pub const GL_TEXTURE_2D: u32 = 0x0DE1;
pub const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
pub const GL_ACTIVE_TEXTURE: u32 = 0x84E0;
pub const GL_FRAMEBUFFER: u32 = 0x8D40;
pub const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
pub const GL_CURRENT_PROGRAM: u32 = 0x8B8D;
pub const GL_VERTEX_ARRAY_BINDING: u32 = 0x85B5;

/// Системные курсоры (`IDC_*` из Win32).
pub mod cursor {
    pub const ARROW: usize = 32512;
    pub const IBEAM: usize = 32513;
    pub const WAIT: usize = 32514;
    pub const CROSS: usize = 32515;
    pub const SIZENWSE: usize = 32642;
    pub const SIZENESW: usize = 32643;
    pub const SIZEWE: usize = 32644;
    pub const SIZENS: usize = 32645;
    pub const SIZEALL: usize = 32646;
    pub const NO: usize = 32648;
    pub const HAND: usize = 32649;
}

#[repr(C)]
#[derive(Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct CursorInfo {
    size: u32,
    flags: u32,
    cursor: *mut c_void,
    pos: Point,
}

const CURSOR_SHOWING: u32 = 0x0000_0001;

#[link(name = "user32")]
unsafe extern "system" {
    fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> i32;
    #[cfg_attr(target_pointer_width = "32", link_name = "GetWindowLongW")]
    fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> usize;
    #[cfg_attr(target_pointer_width = "32", link_name = "SetWindowLongW")]
    fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: usize) -> usize;
    fn CallWindowProcW(prev: usize, hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
    fn ClipCursor(rect: *const Rect) -> i32;
    fn ReleaseCapture() -> i32;
    fn GetForegroundWindow() -> Hwnd;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn ScreenToClient(hwnd: Hwnd, point: *mut Point) -> i32;
    fn SetCursor(cursor: *mut c_void) -> *mut c_void;
    fn LoadCursorW(instance: *mut c_void, name: usize) -> *mut c_void;
    fn ShowCursor(show: i32) -> i32;
    fn GetCursorInfo(info: *mut CursorInfo) -> i32;
    fn OpenClipboard(hwnd: Hwnd) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(format: u32) -> *mut c_void;
    fn SetClipboardData(format: u32, data: *mut c_void) -> *mut c_void;
}

/// Освобождает ограничение курсора, установленное игрой.
pub fn release_cursor_clip() {
    unsafe {
        ClipCursor(std::ptr::null());
        ReleaseCapture();
    }
}

pub fn is_foreground(hwnd: Hwnd) -> bool {
    unsafe { GetForegroundWindow() == hwnd }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleA(name: *const u8) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *const c_void;
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
    fn GlobalLock(mem: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(mem: *mut c_void) -> i32;
}

#[link(name = "opengl32")]
unsafe extern "system" {
    pub fn glPushAttrib(mask: u32);
    pub fn glPopAttrib();
    pub fn glPushClientAttrib(mask: u32);
    pub fn glPopClientAttrib();
    pub fn glMatrixMode(mode: u32);
    pub fn glPushMatrix();
    pub fn glPopMatrix();
    pub fn glBindTexture(target: u32, texture: u32);
    pub fn glGetIntegerv(name: u32, out: *mut i32);
    pub fn glDisable(cap: u32);
    pub fn glPixelStorei(name: u32, value: i32);
}

/// Клиентский размер окна в пикселях; `None` — свёрнуто/нет размера.
pub fn client_size(hwnd: Hwnd) -> Option<(u32, u32)> {
    let mut rect = Rect::default();
    // SAFETY: `rect` — валидный буфер размера RECT.
    if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
        return None;
    }
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    (w > 0 && h > 0).then_some((w as u32, h as u32))
}

/// Позиция курсора в клиентских координатах окна; `None`, если курсор снаружи.
pub fn cursor_pos(hwnd: Hwnd) -> Option<(f32, f32)> {
    let mut point = Point::default();
    // SAFETY: `point` — валидный буфер размера POINT.
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return None;
    }
    if unsafe { ScreenToClient(hwnd, &mut point) } == 0 {
        return None;
    }
    let (w, h) = client_size(hwnd)?;
    let inside = point.x >= 0 && point.y >= 0 && (point.x as u32) < w && (point.y as u32) < h;
    inside.then_some((point.x as f32, point.y as f32))
}

/// Подменяет оконную процедуру; возвращает прежнюю.
pub fn subclass(hwnd: Hwnd, proc: WndProc) -> Option<usize> {
    // SAFETY: `hwnd` — живое окно игры, `proc` — валидная процедура.
    let prev = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc as usize) };
    (prev != 0).then_some(prev)
}

/// Текущая оконная процедура.
pub fn current_proc(hwnd: Hwnd) -> usize {
    // SAFETY: чтение слота окна.
    unsafe { GetWindowLongPtrW(hwnd, GWLP_WNDPROC) }
}

/// Откат подмены: вернуть `prev`, если слот всё ещё наш.
pub fn restore(hwnd: Hwnd, ours: usize, prev: usize) {
    // SAFETY: окно живо, запись в слот — атомарная.
    unsafe {
        if current_proc(hwnd) == ours {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, prev);
        }
    }
}

/// Зовёт следующую процедуру в цепочке.
///
/// # Safety
/// `prev` должен быть адресом оконной процедуры.
pub unsafe fn call_prev(prev: usize, hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: на вызывающем.
    unsafe { CallWindowProcW(prev, hwnd, msg, wparam, lparam) }
}

/// Процедура по умолчанию (когда нашей цепочки уже нет).
pub fn call_default(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: стандартный вызов.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Изменяет счётчик курсора вызывающего потока на единицу.
pub fn show_cursor(show: bool) -> i32 {
    unsafe { ShowCursor(show as i32) }
}

/// Просит оконный поток согласовать видимость курсора.
pub fn request_cursor_update(hwnd: Hwnd) {
    unsafe { PostMessageW(hwnd, WM_EGUI_CURSOR, 0, 0); }
}

/// Виден ли системный курсор сейчас.
pub fn cursor_visible() -> bool {
    let mut info = CursorInfo {
        size: size_of::<CursorInfo>() as u32,
        flags: 0,
        cursor: std::ptr::null_mut(),
        pos: Point::default(),
    };
    // SAFETY: `info` — валидный буфер размера CURSORINFO.
    unsafe { GetCursorInfo(&mut info) != 0 && info.flags & CURSOR_SHOWING != 0 }
}

/// Ставит системный курсор по `IDC_*`-константе.
pub fn set_cursor(idc: usize) {
    // SAFETY: системные курсоры не требуют освобождения.
    unsafe {
        let cursor = LoadCursorW(std::ptr::null_mut(), idc);
        if !cursor.is_null() {
            SetCursor(cursor);
        }
    }
}

/// Экспорт `opengl32.dll` по имени; `None` — библиотека ещё не загружена.
pub fn opengl_export(name: &str) -> Option<usize> {
    // SAFETY: строки NUL-терминированы локально.
    unsafe {
        let module = GetModuleHandleA(c"opengl32.dll".as_ptr().cast());
        if module.is_null() {
            return None;
        }
        let cname = std::ffi::CString::new(name).ok()?;
        let proc = GetProcAddress(module, cname.as_ptr() as *const u8);
        (!proc.is_null()).then_some(proc as usize)
    }
}

/// Резолвер GL-функций для `glow`: сначала `wglGetProcAddress`, затем экспорт.
pub fn gl_proc_address(name: &str) -> *const c_void {
    type WglGetProcAddress = unsafe extern "system" fn(*const u8) -> *const c_void;

    static WGL_GET_PROC_ADDRESS: OnceLock<Option<WglGetProcAddress>> = OnceLock::new();

    let wgl_get_proc_address = WGL_GET_PROC_ADDRESS.get_or_init(|| {
        // SAFETY: поиск экспорта в уже загруженной библиотеке.
        unsafe {
            let module = GetModuleHandleA(c"opengl32.dll".as_ptr().cast());
            if module.is_null() {
                return None;
            }
            let proc = GetProcAddress(module, c"wglGetProcAddress".as_ptr().cast());
            if proc.is_null() {
                None
            } else {
                Some(std::mem::transmute::<*const c_void, WglGetProcAddress>(
                    proc,
                ))
            }
        }
    });

    let Ok(cname) = std::ffi::CString::new(name) else {
        return std::ptr::null();
    };

    // SAFETY: `wglGetProcAddress` требует текущий GL-контекст; мы в рендер-потоке.
    unsafe {
        if let Some(get_proc) = wgl_get_proc_address {
            let ptr = get_proc(cname.as_ptr() as *const u8);
            // wglGetProcAddress возвращает NULL, 1, 2, 3 или -1 при неудаче.
            if !ptr.is_null() && ptr as isize > 3 {
                return ptr;
            }
        }
        let module = GetModuleHandleA(c"opengl32.dll".as_ptr().cast());
        if module.is_null() {
            return std::ptr::null();
        }
        GetProcAddress(module, cname.as_ptr() as *const u8)
    }
}

/// Читает текст из буфера обмена (UTF-16).
pub fn clipboard_get() -> Option<String> {
    // SAFETY: стандартная последовательность работы с буфером обмена.
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let result = (|| {
            let data = GetClipboardData(CF_UNICODETEXT);
            if data.is_null() {
                return None;
            }
            let text = GlobalLock(data) as *const u16;
            if text.is_null() {
                return None;
            }
            let mut len = 0;
            while *text.add(len) != 0 {
                len += 1;
            }
            let slice = std::slice::from_raw_parts(text, len);
            let string = String::from_utf16_lossy(slice);
            GlobalUnlock(data);
            Some(string)
        })();
        CloseClipboard();
        result
    }
}

/// Кладёт текст в буфер обмена (UTF-16).
pub fn clipboard_set(text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: стандартная последовательность; память передаётся буферу обмена.
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return;
        }
        EmptyClipboard();
        let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        if !mem.is_null() {
            let dst = GlobalLock(mem) as *mut u16;
            if !dst.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
                GlobalUnlock(mem);
                // После SetClipboardData память принадлежит системе.
                if SetClipboardData(CF_UNICODETEXT, mem).is_null() {
                    // Не получилось — память наша, освобождаем.
                    kernel32_global_free(mem);
                }
            }
        }
        CloseClipboard();
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalFree(mem: *mut c_void) -> *mut c_void;
}

fn kernel32_global_free(mem: *mut c_void) {
    // SAFETY: `mem` выделен `GlobalAlloc` и не передан системе.
    unsafe {
        GlobalFree(mem);
    }
}

/// Открывает ссылку в браузере.
pub fn open_url(url: &str) {
    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` живёт до конца вызова.
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        );
    }
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(
        hwnd: *mut c_void,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> *mut c_void;
}
