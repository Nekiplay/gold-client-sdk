//! Оконная процедура: события мыши/клавиатуры для egui и их перехват у игры.
//!
//! Подмена делается через `SetWindowLongPtrW` (subclassing), а не хук байтов:
//! свою процедуру можно вернуть на место без разбора чужого кода. Пока меню
//! открыто, сообщения ввода глотаются — игра их не видит; скрытое меню всё
//! пропускает в оригинальную процедуру.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use egui::{Event, Key, Modifiers, PointerButton, Pos2};

use crate::sys;

static SET_CURSOR_POS_INSTALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static SDL_WARP_INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SDL_RELATIVE_INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SDL_PREVIOUS_RELATIVE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

golden_std::hook! {
    /// Движок перемещает курсор через SDL, минуя SetCursorPos клиента.
    unsafe extern "C" fn hook_sdl_warp(window: *mut std::ffi::c_void, x: i32, y: i32) original: sdl_warp_original {
        if !crate::visible() {
            unsafe { sdl_warp_original()(window, x, y) };
        }
    }
}

golden_std::hook! {
    /// Не позволяем движку повторно захватить мышь при открытом меню.
    unsafe extern "C" fn hook_sdl_relative(enabled: i32) original: sdl_relative_original -> i32 {
        if crate::visible() && enabled != 0 {
            return 0;
        }
        unsafe { sdl_relative_original()(enabled) }
    }
}

/// Установка производится только в рендер-потоке, как и остальные детуры.
pub(crate) fn ensure_sdl_mouse_released() {
    if !SDL_WARP_INSTALLED.load(Ordering::Acquire) {
        if let Some(target) = golden_sdk::process::export("SDL2.dll", "SDL_WarpMouseInWindow") {
            match unsafe { golden_std::hooks::install(target, hook_sdl_warp as *const ()) } {
                Ok(_) => { SDL_WARP_INSTALLED.store(true, Ordering::Release); golden_sdk::log!("egui: SDL_WarpMouseInWindow перехвачен"); }
                Err(error) => golden_sdk::log!("egui: SDL_WarpMouseInWindow не перехвачен: {error}"),
            }
        }
    }
    if !SDL_RELATIVE_INSTALLED.load(Ordering::Acquire) {
        if let Some(target) = golden_sdk::process::export("SDL2.dll", "SDL_SetRelativeMouseMode") {
            match unsafe { golden_std::hooks::install(target, hook_sdl_relative as *const ()) } {
                Ok(_) => { SDL_RELATIVE_INSTALLED.store(true, Ordering::Release); golden_sdk::log!("egui: SDL_SetRelativeMouseMode перехвачен"); }
                Err(error) => golden_sdk::log!("egui: SDL_SetRelativeMouseMode не перехвачен: {error}"),
            }
        }
    }
    if SDL_PREVIOUS_RELATIVE.load(Ordering::Acquire) == -1 {
        if let (Some(get), Some(set)) = (
            golden_sdk::process::export("SDL2.dll", "SDL_GetRelativeMouseMode"),
            golden_sdk::process::export("SDL2.dll", "SDL_SetRelativeMouseMode"),
        ) {
            let get: unsafe extern "C" fn() -> i32 = unsafe { std::mem::transmute(get) };
            let set: unsafe extern "C" fn(i32) -> i32 = unsafe { std::mem::transmute(set) };
            let previous = unsafe { get() };
            if unsafe { set(0) } == 0 {
                SDL_PREVIOUS_RELATIVE.store(previous, Ordering::Release);
                golden_sdk::log!("egui: SDL relative mode отключён (был {previous})");
            } else {
                golden_sdk::log!("egui: не удалось отключить SDL relative mode");
            }
        }
    }
}

pub(crate) fn restore_sdl_mouse() {
    let previous = SDL_PREVIOUS_RELATIVE.swap(-1, Ordering::AcqRel);
    if previous >= 0 {
        if let Some(target) = golden_sdk::process::export("SDL2.dll", "SDL_SetRelativeMouseMode") {
            let set: unsafe extern "C" fn(i32) -> i32 = unsafe { std::mem::transmute(target) };
            let result = unsafe { set(previous) };
            golden_sdk::log!("egui: SDL relative mode восстановлен: {previous}, результат {result}");
        }
    }
}

golden_std::hook! {
    /// Блокирует центрирование системного курсора игрой, пока открыто GUI.
    unsafe extern "system" fn hook_set_cursor_pos(x: i32, y: i32) original: set_cursor_pos_original -> i32 {
        if crate::visible() {
            return 1;
        }
        unsafe { set_cursor_pos_original()(x, y) }
    }
}

/// Ставит хук на `user32!SetCursorPos` из render-потока.
pub(crate) fn install_set_cursor_pos_hook() {
    if SET_CURSOR_POS_INSTALLED.load(Ordering::Acquire) {
        return;
    }
    let Some(target) = golden_sdk::process::export("user32.dll", "SetCursorPos") else {
        return;
    };
    match unsafe { golden_std::hooks::install(target, hook_set_cursor_pos as *const ()) } {
        Ok(_) => {
            SET_CURSOR_POS_INSTALLED.store(true, Ordering::Release);
            golden_sdk::log!("egui: user32!SetCursorPos перехвачен");
        }
        Err(error) => golden_sdk::log!("egui: SetCursorPos не перехвачен: {error}"),
    }
}

/// Оригинальная процедура окна игры.
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

/// Очередь событий для ближайшего кадра egui.
static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());

/// Старшая часть суррогатной пары из `WM_CHAR`.
static SURROGATE: Mutex<Option<u16>> = Mutex::new(None);

/// Последняя известная позиция курсора в клиентских координатах.
static POINTER: Mutex<Option<Pos2>> = Mutex::new(None);

/// Курсор, который просит egui.
static CURSOR: Mutex<egui::CursorIcon> = Mutex::new(egui::CursorIcon::Default);

/// Выключаем клиентский режим мыши, если он действительно был активен.
static MOUSE_WAS_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn release_game_mouse() {
    const MOUSE_ACTIVE: usize = 0x1B1250;
    let Some(base) = golden_sdk::process::client_base() else {
        return;
    };
    let Some(deactivate) = golden_sdk::process::export("client.dll", "IN_DeactivateMouse") else {
        return;
    };
    let address = base + MOUSE_ACTIVE;
    if !golden_sdk::process::is_readable(address) {
        return;
    }
    let active = unsafe { golden_sdk::process::read::<i32>(address) != 0 };
    MOUSE_WAS_ACTIVE.store(active, Ordering::Release);
    if active {
        let deactivate: unsafe extern "C" fn() = unsafe { std::mem::transmute(deactivate) };
        unsafe { deactivate() };
    }
    sys::release_cursor_clip();
}

/// GoldSrc может снова включить relative mouse mode во время следующего
/// кадра. Поэтому этот вызов повторяется из render-hook.
pub(crate) fn ensure_game_mouse_released() {
    const MOUSE_ACTIVE: usize = 0x1B1250;
    let active = golden_sdk::process::client_base()
        .map(|base| base + MOUSE_ACTIVE)
        .filter(|&address| golden_sdk::process::is_readable(address))
        .map(|address| unsafe { golden_sdk::process::read::<i32>(address) != 0 })
        .unwrap_or(false);
    if active {
        if let Some(deactivate) = golden_sdk::process::export("client.dll", "IN_DeactivateMouse") {
            let deactivate: unsafe extern "C" fn() = unsafe { std::mem::transmute(deactivate) };
            unsafe { deactivate() };
        }
    }
    sys::release_cursor_clip();
}

pub(crate) fn restore_game_mouse() {
    if !MOUSE_WAS_ACTIVE.swap(false, Ordering::AcqRel) {
        return;
    }
    let Some(hwnd) = golden_std::input::game_window() else {
        return;
    };
    if !sys::is_foreground(hwnd as sys::Hwnd) {
        return;
    }
    if let Some(activate) = golden_sdk::process::export("client.dll", "IN_ActivateMouse") {
        let activate: unsafe extern "C" fn() = unsafe { std::mem::transmute(activate) };
        unsafe { activate() };
    }
}

pub(crate) fn capture_game_input() {
    golden_std::input::release_physical_keys();
    let blocker = golden_std::input::blocker();
    for vk in 0..=0xFF {
        blocker.suppress(vk);
    }
}

pub(crate) fn release_game_input() {
    golden_std::input::blocker().release_all();
}

/// Подменяет оконную процедуру игры на нашу. Возврат к оригиналу — при выгрузке.
pub(crate) fn install(hwnd: usize) -> bool {
    if ORIGINAL.load(Ordering::Acquire) != 0 {
        return true;
    }
    let Some(prev) = sys::subclass(hwnd as sys::Hwnd, wndproc) else {
        return false;
    };
    ORIGINAL.store(prev, Ordering::Release);
    if !golden_std::unload::on_unload(move || restore(hwnd)) {
        restore(hwnd);
        return false;
    }
    true
}

/// Возвращает оригинальную процедуру, если слот окна всё ещё наш.
fn restore(hwnd: usize) {
    let prev = ORIGINAL.load(Ordering::Acquire);
    if prev == 0 {
        return;
    }
    sys::restore(hwnd as sys::Hwnd, wndproc as *const () as usize, prev);
    golden_sdk::log!("egui: оконная процедура возвращена");
}

/// Собирает события, накопленные с прошлого кадра.
pub(crate) fn drain_events() -> Vec<Event> {
    std::mem::take(&mut EVENTS.lock().unwrap())
}

/// Модификаторы клавиатуры на момент вызова (из `GetAsyncKeyState`).
pub(crate) fn modifiers() -> Modifiers {
    let shift = golden_std::input::held_vk(sys::VK_SHIFT);
    let ctrl = golden_std::input::held_vk(sys::VK_CONTROL);
    Modifiers {
        alt: golden_std::input::held_vk(sys::VK_MENU),
        ctrl,
        shift,
        mac_cmd: false,
        command: ctrl,
    }
}

/// Передаёт в egui текущую позицию курсора (при открытии меню).
pub(crate) fn feed_cursor_now() {
    let Some(hwnd) = golden_std::input::game_window() else {
        return;
    };
    match sys::cursor_pos(hwnd as sys::Hwnd) {
        Some((x, y)) => push_pointer_moved(x, y),
        None => feed_pointer_gone(),
    }
}

/// Сообщает egui, что курсор ушёл (при закрытии меню).
pub(crate) fn feed_pointer_gone() {
    *POINTER.lock().unwrap() = None;
    push(Event::PointerGone);
}

/// Курсор, который egui просит показать (вызывается из рендер-потока).
pub(crate) fn set_cursor_icon(icon: egui::CursorIcon) {
    *CURSOR.lock().unwrap() = icon;
}

/// Наша оконная процедура.
unsafe extern "system" fn wndproc(
    hwnd: sys::Hwnd,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    let _inside = golden_sdk::module::InFlight::enter();
    let original = ORIGINAL.load(Ordering::Acquire);

    // Клавиша-переключатель работает и при скрытом меню.
    if msg == sys::WM_KEYDOWN && !is_repeat(lparam) {
        let toggle = crate::toggle_key();
        if toggle != 0 && wparam as u32 == toggle {
            crate::toggle();
            return 0;
        }
    }

    if crate::visible() {
        match msg {
            sys::WM_KEYDOWN | sys::WM_SYSKEYDOWN => {
                if wparam as u32 == 0x1B {
                    // Escape закрывает меню.
                    crate::set_visible(false);
                } else {
                    push_key(wparam as u32, true, is_repeat(lparam));
                }
                return 0;
            }
            sys::WM_KEYUP | sys::WM_SYSKEYUP => {
                push_key(wparam as u32, false, false);
                return 0;
            }
            sys::WM_CHAR => {
                push_char(wparam as u32);
                return 0;
            }
            sys::WM_SYSCHAR => return 0,
            sys::WM_MOUSEMOVE => {
                let (x, y) = lparam_pos(lparam);
                push_pointer_moved(x, y);
                return 0;
            }
            sys::WM_LBUTTONDOWN | sys::WM_LBUTTONDBLCLK => {
                return button(PointerButton::Primary, true);
            }
            sys::WM_LBUTTONUP => return button(PointerButton::Primary, false),
            sys::WM_RBUTTONDOWN | sys::WM_RBUTTONDBLCLK => {
                return button(PointerButton::Secondary, true);
            }
            sys::WM_RBUTTONUP => return button(PointerButton::Secondary, false),
            sys::WM_MBUTTONDOWN | sys::WM_MBUTTONDBLCLK => {
                return button(PointerButton::Middle, true);
            }
            sys::WM_MBUTTONUP => return button(PointerButton::Middle, false),
            sys::WM_XBUTTONDOWN | sys::WM_XBUTTONDBLCLK => return button(xbutton(wparam), true),
            sys::WM_XBUTTONUP => return button(xbutton(wparam), false),
            sys::WM_MOUSEWHEEL => {
                push_wheel(false, wparam);
                return 0;
            }
            sys::WM_MOUSEHWHEEL => {
                push_wheel(true, wparam);
                return 0;
            }
            // Сырая мышь движка: не даём камере крутиться.
            sys::WM_INPUT => return 0,
            sys::WM_SETCURSOR => {
                apply_cursor();
                return 1;
            }
            _ => {}
        }
    }

    if original != 0 {
        // SAFETY: `original` — оригинальная процедура окна игры.
        unsafe { sys::call_prev(original, hwnd, msg, wparam, lparam) }
    } else {
        sys::call_default(hwnd, msg, wparam, lparam)
    }
}

/// Кнопка мыши → событие; возвращает 0 для выхода из wndproc.
fn button(button: PointerButton, pressed: bool) -> isize {
    let pos = (*POINTER.lock().unwrap()).unwrap_or(Pos2::ZERO);
    push(Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: modifiers(),
    });
    0
}

/// `XBUTTON1/2` из старшего слова wparam.
fn xbutton(wparam: usize) -> PointerButton {
    if (wparam >> 16) & 0xFFFF == 1 {
        PointerButton::Extra1
    } else {
        PointerButton::Extra2
    }
}

/// Бит 30 lparam: повторное нажатие от удержания.
fn is_repeat(lparam: isize) -> bool {
    (lparam >> 30) & 1 == 1
}

/// Координаты курсора из lparam мышиных сообщений.
fn lparam_pos(lparam: isize) -> (f32, f32) {
    let x = (lparam & 0xFFFF) as i16 as f32;
    let y = ((lparam >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
}

fn push_pointer_moved(x: f32, y: f32) {
    let pos = Pos2::new(x, y);
    *POINTER.lock().unwrap() = Some(pos);
    push(Event::PointerMoved(pos));
}

fn push_wheel(horizontal: bool, wparam: usize) {
    let lines = ((wparam >> 16) as i16 as f32 / 120.0) * 3.0;
    let delta = if horizontal {
        egui::vec2(lines, 0.0)
    } else {
        egui::vec2(0.0, lines)
    };
    push(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: modifiers(),
    });
}

/// Клавиша → событие egui; сочетания с Ctrl становятся Copy/Cut/Paste,
/// как в egui-winit.
fn push_key(vk: u32, pressed: bool, repeat: bool) {
    let mods = modifiers();
    if pressed && mods.ctrl {
        match vk {
            0x43 => {
                push(Event::Copy);
                return;
            }
            0x58 => {
                push(Event::Cut);
                return;
            }
            0x56 => {
                if let Some(text) = sys::clipboard_get() {
                    push(Event::Paste(text));
                }
                return;
            }
            _ => {}
        }
    }
    if let Some(key) = translate_key(vk) {
        push(Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers: mods,
        });
    }
}

/// Символ из `WM_CHAR` (UTF-16, включая суррогатные пары).
fn push_char(unit: u32) {
    let unit = unit as u16;
    let ch = {
        let mut pending = SURROGATE.lock().unwrap();
        if (0xD800..0xDC00).contains(&unit) {
            *pending = Some(unit);
            return;
        }
        if (0xDC00..0xE000).contains(&unit) {
            match pending.take() {
                Some(high) => char::from_u32(
                    0x10000 + (((high as u32) - 0xD800) << 10) + ((unit as u32) - 0xDC00),
                ),
                None => None,
            }
        } else {
            pending.take();
            char::from_u32(unit as u32)
        }
    };
    if let Some(ch) = ch {
        // Управляющие символы приходят и от Enter/Tab/Backspace — они уже
        // обработаны как Key.
        if ch >= ' ' && ch != '\u{7F}' {
            push(Event::Text(ch.to_string()));
        }
    }
}

fn push(event: Event) {
    EVENTS.lock().unwrap().push(event);
}

/// Применяет системный курсор по запросу egui.
fn apply_cursor() {
    let icon = *CURSOR.lock().unwrap();
    sys::set_cursor(cursor_idc(icon));
}

/// `egui::CursorIcon` → системный `IDC_*`.
fn cursor_idc(icon: egui::CursorIcon) -> usize {
    use egui::CursorIcon;
    match icon {
        CursorIcon::PointingHand => sys::cursor::HAND,
        CursorIcon::Text | CursorIcon::VerticalText => sys::cursor::IBEAM,
        CursorIcon::Crosshair | CursorIcon::Cell => sys::cursor::CROSS,
        CursorIcon::ResizeHorizontal | CursorIcon::ResizeColumn => sys::cursor::SIZEWE,
        CursorIcon::ResizeVertical | CursorIcon::ResizeRow => sys::cursor::SIZENS,
        CursorIcon::ResizeNeSw => sys::cursor::SIZENESW,
        CursorIcon::ResizeNwSe => sys::cursor::SIZENWSE,
        CursorIcon::Move | CursorIcon::Grab | CursorIcon::Grabbing => sys::cursor::SIZEALL,
        CursorIcon::NotAllowed | CursorIcon::NoDrop => sys::cursor::NO,
        CursorIcon::Wait | CursorIcon::Progress => sys::cursor::WAIT,
        _ => sys::cursor::ARROW,
    }
}

const LETTERS: [Key; 26] = [
    Key::A,
    Key::B,
    Key::C,
    Key::D,
    Key::E,
    Key::F,
    Key::G,
    Key::H,
    Key::I,
    Key::J,
    Key::K,
    Key::L,
    Key::M,
    Key::N,
    Key::O,
    Key::P,
    Key::Q,
    Key::R,
    Key::S,
    Key::T,
    Key::U,
    Key::V,
    Key::W,
    Key::X,
    Key::Y,
    Key::Z,
];

const DIGITS: [Key; 10] = [
    Key::Num0,
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

const FKEYS: [Key; 12] = [
    Key::F1,
    Key::F2,
    Key::F3,
    Key::F4,
    Key::F5,
    Key::F6,
    Key::F7,
    Key::F8,
    Key::F9,
    Key::F10,
    Key::F11,
    Key::F12,
];

/// Виртуальный код клавиши → `egui::Key`.
fn translate_key(vk: u32) -> Option<Key> {
    let key = match vk {
        0x08 => Key::Backspace,
        0x09 => Key::Tab,
        0x0D => Key::Enter,
        0x1B => Key::Escape,
        0x20 => Key::Space,
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0x23 => Key::End,
        0x24 => Key::Home,
        0x25 => Key::ArrowLeft,
        0x26 => Key::ArrowUp,
        0x27 => Key::ArrowRight,
        0x28 => Key::ArrowDown,
        0x2D => Key::Insert,
        0x2E => Key::Delete,
        0x30..=0x39 => DIGITS[(vk - 0x30) as usize],
        0x41..=0x5A => LETTERS[(vk - 0x41) as usize],
        0x60..=0x69 => DIGITS[(vk - 0x60) as usize],
        0x70..=0x7B => FKEYS[(vk - 0x70) as usize],
        0xBA => Key::Semicolon,
        0xBB => Key::Equals,
        0xBC => Key::Comma,
        0xBD => Key::Minus,
        0xBE => Key::Period,
        0xBF => Key::Slash,
        0xC0 => Key::Backtick,
        0xDB => Key::OpenBracket,
        0xDC => Key::Backslash,
        0xDD => Key::CloseBracket,
        0xDE => Key::Quote,
        _ => return None,
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_letters() {
        assert_eq!(translate_key(0x41), Some(Key::A));
        assert_eq!(translate_key(0x5A), Some(Key::Z));
    }

    #[test]
    fn translates_digits() {
        assert_eq!(translate_key(0x30), Some(Key::Num0));
        assert_eq!(translate_key(0x39), Some(Key::Num9));
        assert_eq!(translate_key(0x60), Some(Key::Num0));
    }

    #[test]
    fn translates_fkeys() {
        assert_eq!(translate_key(0x70), Some(Key::F1));
        assert_eq!(translate_key(0x7B), Some(Key::F12));
    }

    #[test]
    fn skips_unknown() {
        assert_eq!(translate_key(0x00), None);
        assert_eq!(translate_key(0xFF), None);
    }

    #[test]
    fn surrogate_pair_becomes_one_char() {
        push_char(0xD83D);
        assert!(drain_events().is_empty());
        push_char(0xDE00);
        let events = drain_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::Text(text) if text == "\u{1F600}"));
    }

    #[test]
    fn control_chars_are_skipped() {
        push_char(0x0D);
        push_char(0x09);
        push_char(0x41);
        let events = drain_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::Text(text) if text == "A"));
    }
}
