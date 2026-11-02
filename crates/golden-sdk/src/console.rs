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
use core::sync::atomic::{AtomicUsize, Ordering};

/// Смещение приёмника строк в `GameUI.dll` — тот, что вызывают и движок, и
/// консоль клиента.
///
/// Проверяется по прологу [`ADD_LINE_RVA`]: если в этой сборке игры по
/// смещению лежит другая функция, [`add_line`] вернёт `None` и перехват не
/// поставится.
pub const ADD_LINE_RVA: usize = 0x000E_8F20;

/// `push ebp; mov ebp, esp` — пролог приёмника в этой сборке.
const PROLOGUE: [u8; 3] = [0x55, 0x8B, 0xEC];

/// Оригинал приёмника, если мод поставил на него перехват.
///
/// Свой перехват (фильтр эха) патчит первые байты приёмника, поэтому звать
/// `GameUI+ADD_LINE_RVA` из вывода нельзя: попадём в свой же хук, а пролог
/// уже не совпадёт с [`PROLOGUE`]. [`crate::console::Writer`] берёт отсюда
/// трамплин, который ведёт в настоящую функцию.
static RECEIVER_ORIGINAL: AtomicUsize = AtomicUsize::new(0);

/// Запоминает трамплин приёмника строк.
///
/// Зовётся слоем `golden-std` сразу после установки перехвата на
/// `GameUI+ADD_LINE_RVA`; вручную обычно не нужен.
pub fn set_receiver_original(address: usize) {
    RECEIVER_ORIGINAL.store(address, Ordering::Release);
}

/// Трамплин приёмника строк, если перехват уже стоит.
pub fn receiver_original() -> Option<usize> {
    let address = RECEIVER_ORIGINAL.load(Ordering::Acquire);
    (address != 0).then_some(address)
}

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

// ------------------------------------------------------------------ вывод ---
//
// `Con_Printf` из `hw.dll` до консоли Gold Client не доходит: история живёт в
// `GameUI.dll`. Поэтому пишем прямо в неё тем же путём, что и сам клиент:
//
// 1. `GameUI+0xE7930` (`set_color`) добавляет в историю цветовую метку на
//    текущую позицию текста — все байты после неё рисуются этим цветом;
// 2. `GameUI+0xE8F20` (`add_line`) дописывает строку и разбирает `#`-цвета,
//    встроенные в сам текст.
//
// Обе функции — `__thiscall`, `this` — объект истории. Объект находим от
// указателя интерфейса консоли в `hw.dll`: интерфейс -> диалог (+0x8) ->
// адаптер истории (+0x198) -> история (+0xC). Смещения проверены на этой
// сборке игры; если раскладка другая, [`Writer::open`] вернёт `None` и мод
// просто ничего не напечатает вместо падения.

/// Смещение функции установки цвета в `GameUI.dll`.
pub const SET_COLOR_RVA: usize = 0x000E_7930;

/// Смещение обычного вывода истории — по нему проверяем, что класс тот же.
const PLAIN_ADD_RVA: usize = 0x0003_62B0;

/// Ячейка в `hw.dll` с указателем на интерфейс консоли клиента.
pub const CONSOLE_PTR_RVA: usize = 0x29F4_938;

/// Смещение адаптера истории внутри объекта диалога консоли.
const WRAPPER_OFFSET: usize = 0x198;

/// Смещение истории внутри адаптера.
const HISTORY_OFFSET: usize = 0x0C;

/// Смещение цвета обычных строк в адаптере.
const DEFAULT_COLOR_OFFSET: usize = 0x20;

/// Смещение вектора цветовых меток в истории.
const COLOR_VECTOR_OFFSET: usize = 0xE8;

/// Смещение счётчика цветовых меток.
const COLOR_COUNT_OFFSET: usize = 0xF4;

/// Сколько байт текста пишем за один вызов (предел стека приёмника).
pub const MAX_TEXT: usize = 2048;

/// Цвет текста консоли: байты в том порядке, в котором их ждёт движок.
///
/// `a` — прозрачность; для обычного текста всегда `0xFF`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Color {
    /// Красный канал.
    pub r: u8,
    /// Зелёный канал.
    pub g: u8,
    /// Синий канал.
    pub b: u8,
    /// Прозрачность.
    pub a: u8,
}

impl Color {
    /// Цвет из каналов.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Непрозрачный цвет из каналов.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 0xFF)
    }

    /// Непрозрачный цвет из `0xRRGGBB`.
    pub const fn hex(rgb: u32) -> Self {
        Self::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    /// Цвет из машинного слова движка: байты в памяти идут как `r, g, b, a`.
    pub const fn from_bits(bits: u32) -> Self {
        Self {
            r: bits as u8,
            g: (bits >> 8) as u8,
            b: (bits >> 16) as u8,
            a: (bits >> 24) as u8,
        }
    }

    /// Машинное слово движка для этого цвета.
    pub const fn bits(self) -> u32 {
        self.r as u32
            | (self.g as u32) << 8
            | (self.b as u32) << 16
            | (self.a as u32) << 24
    }

    /// Белый — цвет обычного вывода движка.
    pub const WHITE: Self = Self::hex(0xFF_FF_FF);
    /// Чёрный.
    pub const BLACK: Self = Self::hex(0x00_00_00);
    /// Красный.
    pub const RED: Self = Self::hex(0xFF_00_00);
    /// Тёмно-красный.
    pub const DARK_RED: Self = Self::hex(0x8B_00_00);
    /// Зелёный.
    pub const GREEN: Self = Self::hex(0x00_FF_00);
    /// Тёмно-зелёный.
    pub const FOREST_GREEN: Self = Self::hex(0x22_8B_22);
    /// Жёлтый.
    pub const YELLOW: Self = Self::hex(0xFF_FF_00);
    /// Оранжевый.
    pub const ORANGE: Self = Self::hex(0xFF_A5_00);
    /// Золотой.
    pub const GOLD: Self = Self::hex(0xFF_D7_00);
    /// Хаки.
    pub const KHAKI: Self = Self::hex(0xF0_E6_8C);
    /// Коричневый.
    pub const BROWN: Self = Self::hex(0xA5_2A_2A);
    /// Синий.
    pub const BLUE: Self = Self::hex(0x00_00_FF);
    /// Тёмно-синий.
    pub const DARK_BLUE: Self = Self::hex(0x00_00_8B);
    /// Голубой.
    pub const LIGHT_BLUE: Self = Self::hex(0xAD_D8_E6);
    /// Бирюзовый.
    pub const CYAN: Self = Self::hex(0x00_FF_FF);
    /// Тёмно-бирюзовый.
    pub const TEAL: Self = Self::hex(0x00_80_80);
    /// Пурпурный.
    pub const MAGENTA: Self = Self::hex(0xFF_00_FF);
    /// Розовый.
    pub const PINK: Self = Self::hex(0xFF_C0_CB);
    /// Серый.
    pub const GREY: Self = Self::hex(0x80_80_80);
    /// Серебристый.
    pub const SILVER: Self = Self::hex(0xC0_C0_C0);
    /// Тёмно-серый.
    pub const DIM_GREY: Self = Self::hex(0x69_69_69);
    /// Бледно-зелёный.
    pub const PALE_GREEN: Self = Self::hex(0x98_FB_98);
}

/// Цвет обычного вывода в этой консоли.
///
/// Нужен, чтобы вернуть историю в исходное состояние после цветной строки:
/// следующая строка движка не должна унаследовать наш цвет.
pub fn default_color() -> Option<Color> {
    let gui = module_base("GameUI.dll")?;
    let hw = crate::process::hw_base()?;
    let wrapper = history_wrapper(gui, hw)?;
    // SAFETY: адаптер проверен в `history_wrapper`, поле существует.
    let bits = unsafe { crate::process::read::<u32>(wrapper + DEFAULT_COLOR_OFFSET) };
    Some(Color::from_bits(bits))
}

/// Указатель на адаптер истории консоли.
fn history_wrapper(gui: usize, hw: usize) -> Option<usize> {
    let size = unsafe { crate::process::module_size(hw) }?;
    let slot = hw.checked_add(CONSOLE_PTR_RVA)?;
    if !crate::process::in_range(slot, hw, size) {
        return None;
    }
    // SAFETY: ячейка внутри образа `hw.dll`.
    let console = unsafe { crate::process::read_ptr(slot)? };
    if !crate::process::is_readable(console + 8) {
        return None;
    }
    // SAFETY: поле прочитано, указатель на диалог может быть нулевым.
    let dialog = unsafe { crate::process::read_ptr(console + 8)? };
    let wrapper = dialog.checked_add(WRAPPER_OFFSET)?;
    if !crate::process::is_readable(wrapper + DEFAULT_COLOR_OFFSET + 4) {
        return None;
    }
    // Сверяем класс адаптера с той сборкой, под которую посчитаны смещения:
    // слот обычного вывода в его vtable должен указывать на `GameUI+0x362B0`.
    // SAFETY: указатель на объект адаптера не нулевой.
    let vtable = unsafe { crate::process::read_ptr(wrapper)? };
    if !crate::process::is_readable(vtable) || !crate::process::is_readable(vtable + 0x0C) {
        return None;
    }
    // SAFETY: vtable-слот внутри объекта адаптера.
    let plain = unsafe { crate::process::read::<usize>(vtable + 8) };
    if plain != gui + PLAIN_ADD_RVA {
        return None;
    }
    // SAFETY: адаптер проверен, берём из него объект истории.
    let history = unsafe { crate::process::read_ptr(wrapper + HISTORY_OFFSET)? };
    // `set_color` читает последнюю метку, поэтому пустой вектор нам не годится.
    // SAFETY: поле истории проверено выше.
    let count = unsafe { crate::process::read::<u32>(history + COLOR_COUNT_OFFSET) };
    if count == 0 {
        return None;
    }
    // SAFETY: указатель на вектор может быть нулевым при нулевом счётчике,
    // но он уже больше нуля.
    let colors = unsafe { crate::process::read_ptr(history + COLOR_VECTOR_OFFSET)? };
    let last = (count as usize - 1)
        .checked_mul(0x20)?
        .checked_add(colors)?
        .checked_add(0x1C)?;
    if !crate::process::is_readable(last + 4) {
        return None;
    }
    Some(wrapper)
}

/// Пишущая часть консоли: объект истории и функции вывода.
///
/// Открывается на один вывод и живёт, пока мы пишем. Хранит только адреса
/// внутри игры; `Drop` ничего не делает.
#[derive(Clone, Copy)]
pub struct Writer {
    history: usize,
    default: Color,
    set: unsafe extern "thiscall" fn(*mut c_void, u32),
    add: unsafe extern "thiscall" fn(*mut c_void, *const u8),
}

impl core::fmt::Debug for Writer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Writer {{ history: {:#x} }}", self.history)
    }
}

impl Writer {
    /// Открывает консоль, если она уже создана и раскладка совпадает.
    pub fn open() -> Option<Self> {
        let gui = module_base("GameUI.dll")?;
        let gui_size = unsafe { crate::process::module_size(gui) }?;
        let set = gui.checked_add(SET_COLOR_RVA)?;
        let hooked = receiver_original();
        let add = match hooked {
            Some(original) => original,
            None => gui.checked_add(ADD_LINE_RVA)?,
        };
        if !crate::process::in_range(set, gui, gui_size)
            || !crate::process::in_range(set + PROLOGUE.len(), gui, gui_size)
        {
            return None;
        }
        // `set_color` мы не перехватываем: у него всегда обычный пролог.
        // SAFETY: адрес внутри образа модуля, читаем три байта.
        let set_head = unsafe { core::slice::from_raw_parts(set as *const u8, PROLOGUE.len()) };
        if set_head != PROLOGUE {
            return None;
        }
        if hooked.is_none() {
            // Хука ещё нет — приёмник по смещению должен быть настоящим.
            if !crate::process::in_range(add, gui, gui_size)
                || !crate::process::in_range(add + PROLOGUE.len(), gui, gui_size)
            {
                return None;
            }
            // SAFETY: адрес внутри образа модуля.
            let add_head = unsafe { core::slice::from_raw_parts(add as *const u8, PROLOGUE.len()) };
            if add_head != PROLOGUE {
                return None;
            }
        }

        let hw = crate::process::hw_base()?;
        let wrapper = history_wrapper(gui, hw)?;
        // SAFETY: `history_wrapper` проверил адаптер и историю.
        let history = unsafe { crate::process::read_ptr(wrapper + HISTORY_OFFSET)? };
        // SAFETY: поле существует, проверено в `history_wrapper`.
        let default = unsafe { crate::process::read::<u32>(wrapper + DEFAULT_COLOR_OFFSET) };

        Some(Self {
            history,
            default: Color::from_bits(default),
            // SAFETY: сигнатуры проверены по прологу и совпадают с этой сборкой.
            set: unsafe { core::mem::transmute::<usize, unsafe extern "thiscall" fn(*mut c_void, u32)>(set) },
            // SAFETY: то же для приёмника строк.
            add: unsafe { core::mem::transmute::<usize, unsafe extern "thiscall" fn(*mut c_void, *const u8)>(add) },
        })
    }

    /// Цвет, которым консоль пишет обычные строки движка.
    pub const fn default_color(&self) -> Color {
        self.default
    }

    /// Делает `color` текущим цветом текста.
    ///
    /// Возвращает `false`, только если история уже не та, куда мы открывались
    /// (например, консоль пересоздали при смене карты).
    pub fn set_color(&self, color: Color) -> bool {
        if !crate::process::is_readable(self.history + COLOR_COUNT_OFFSET) {
            return false;
        }
        // SAFETY: `this` — объект истории, сигнатура совпадает с оригиналом.
        unsafe { (self.set)(self.history as *mut c_void, color.bits()) };
        true
    }

    /// Дописывает текст без перевода строки.
    ///
    /// Возвращает `false`, если консоль исчезла или текст пуст.
    pub fn write(&self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let mut end = text.len().min(MAX_TEXT);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            return false;
        }
        let mut line = alloc::vec::Vec::with_capacity(end + 1);
        for &byte in &text.as_bytes()[..end] {
            // Приёмник читает строку до нуля: внутренний нуль стал бы концом.
            line.push(if byte == 0 { b' ' } else { byte });
        }
        line.push(0);
        // SAFETY: `this` — объект истории, строка завершена нулём и живёт до
        // конца вызова, длина ограничена `MAX_TEXT`.
        unsafe { (self.add)(self.history as *mut c_void, line.as_ptr()) };
        true
    }

    /// Дописывает строку с переводом строки.
    pub fn line(&self, text: &str) -> bool {
        self.write(text) && self.write("\n")
    }
}

/// Печатает строку обычным цветом консоли.
///
/// Это замена [`crate::concommand`]-печати через `Con_Printf`: текст попадает
/// в историю Gold Client, а не в консоль движка. `false` — консоль ещё не
/// создана или её раскладка не совпала; тогда остаётся лог.
pub fn print(text: &str) -> bool {
    let Some(writer) = Writer::open() else {
        return false;
    };
    writer.set_color(writer.default_color()) && writer.line(text)
}

/// Печатает строку заданным цветом.
pub fn print_colored(color: Color, text: &str) -> bool {
    let Some(writer) = Writer::open() else {
        return false;
    };
    if !writer.set_color(color) {
        return false;
    }
    let written = writer.line(text);
    // Возвращаем цвет по умолчанию: следующая строка движка не должна
    // оказаться нашей.
    writer.set_color(writer.default_color());
    written
}