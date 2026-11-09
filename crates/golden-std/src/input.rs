//! Подавление клавиш на уровне движка: перехват `Key_Event`.
//!
//! `Key_Event(int keynum, int down)` — единственное место, где GoldSrc
//! превращает нажатие в команду (`+jump`, `+forward`, `slot1`…). Перехват этой
//! функции даёт «клавиша нажата, но игра её не видит»: событие не доходит ни до
//! биндов, ни до массива нажатых клавиш, ни до клиента.
//!
//! Адрес и соглашение — [`golden_sdk::offsets::hw::KEY_EVENT`] (`0x10083480`,
//! `__fastcall`: `keynum` в `ECX`, `down` в `EDX`).
//!
//! Пока открыт чат или консоль (`key_dest != 0`), перехват пропускает события
//! без изменений — иначе в сообщении нельзя было бы напечатать пробел. Заодно
//! это значит, что блокировка не ломает `say` и ввод в консоли.
//!
//! ```no_run
//! use golden_std::input;
//!
//! # fn example() -> Result<(), input::InputError> {
//! input::install()?; // движок уже загружен
//! input::blocker().suppress(0x20); // VK_SPACE
//! # Ok(())
//! # }
//! ```

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use golden_sdk::hook::Detour;
use golden_sdk::offsets;
use golden_sdk::process;

/// Сколько virtual-key кодов умещаем (0..255 — весь диапазон Win32).
pub const KEYS: usize = 256;

/// `VK_SPACE` — самая частая цель блокировки, вынесена для читаемости.
pub const VK_SPACE: u32 = 0x20;

// --- перечисление клавиш ---

/// `GetAsyncKeyState` вернул «клавиша нажата прямо сейчас».
const KEY_DOWN: i16 = 0x8000u16 as i16;

/// Вся таблица клавиш в одном месте: вариант, VK-код, имя для логов.
///
/// Макрос разворачивает её в перечисление [`Key`], конверсии в обе стороны
/// и `Display` — добавление клавиши это одна строка, а не правка трёх
/// `match` в разных местах.
macro_rules! keys {
    ( $( $variant:ident = $vk:literal , $name:literal ; )+ ) => {
        /// Физическая клавиша: системный VK-код и удобное имя.
        ///
        /// Таблица покрывает обычную клавиатуру целиком: кнопки мыши,
        /// буквенно-цифровой ряд, нампад (вместе с арифметикой и десятичной
        /// запятой), лево- и правосторонние модификаторы, навигацию и
        /// `F`-клавиши вплоть до `F24`. «Символьные» клавиши (`;`, `[`,
        /// `` ` ``…) именуются по физическим позициям US-клавиатуры
        /// (`VK_OEM_*`): VK стабилен при любой раскладке, а вот символ на
        /// клавише от неё зависит.
        ///
        /// Опрос состояния ([`Key::held`], [`held_vk`]) идёт через
        /// `GetAsyncKeyState` — это системный уровень: работает для любой
        /// клавиши из перечисления (включая боковые кнопки мыши), не
        /// зависит ни от биндов игрока, ни от того, переваривает ли движок
        /// эту кнопку вообще («сырым» входом GoldSrc-клавиши мыши идут мимо
        /// `WM_XBUTTON*`).
        ///
        /// VK-код — код Windows, а не движковый номер клавиши (`K_MOUSE5` в
        /// `Key_Event` — другой): у большинства клавишных клавиш они совпадают
        /// (`K_SPACE == VK_SPACE == 0x20`), у кнопок мыши — нет.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Key {
            $( $variant, )+
        }

        impl Key {
            /// Системный VK-код клавиши.
            pub const fn win_vk(self) -> u32 {
                match self {
                    $( Key::$variant => $vk, )+
                }
            }

            /// Клавиша с таким VK-кодом, если она есть в перечислении.
            ///
            /// Модификаторы `Shift`/`Ctrl`/`Alt` — «логические» коды
            /// `0x10..=0x12`: система считает их зажатыми при любой из
            /// сторон; конкретная сторона — отдельные коды `0xA0..=0xA5`.
            pub const fn from_win_vk(vk: u32) -> Option<Self> {
                match vk {
                    $( $vk => Some(Key::$variant), )+
                    _ => None,
                }
            }
        }

        impl core::fmt::Display for Key {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                // Имена как в биндах CS: mouse5, w, f11 — по ним игрок узнаёт
                // клавишу с экрана.
                let name = match self {
                    $( Key::$variant => $name, )+
                };
                f.write_str(name)
            }
        }
    };
}

keys! {
    // Кнопки мыши.
    Mouse1 = 0x01, "mouse1";
    Mouse2 = 0x02, "mouse2";
    Mouse3 = 0x04, "mouse3";
    Mouse4 = 0x05, "mouse4";
    Mouse5 = 0x06, "mouse5";
    // Клавиши правки и навигации.
    Backspace = 0x08, "backspace";
    Tab = 0x09, "tab";
    Enter = 0x0D, "enter";
    Pause = 0x13, "pause";
    Escape = 0x1B, "escape";
    Space = 0x20, "space";
    PageUp = 0x21, "pgup";
    PageDown = 0x22, "pgdn";
    End = 0x23, "end";
    Home = 0x24, "home";
    Left = 0x25, "left";
    Up = 0x26, "up";
    Right = 0x27, "right";
    Down = 0x28, "down";
    PrintScreen = 0x2C, "printscreen";
    Insert = 0x2D, "ins";
    Delete = 0x2E, "del";
    // Цифры верхнего ряда.
    Digit0 = 0x30, "0";
    Digit1 = 0x31, "1";
    Digit2 = 0x32, "2";
    Digit3 = 0x33, "3";
    Digit4 = 0x34, "4";
    Digit5 = 0x35, "5";
    Digit6 = 0x36, "6";
    Digit7 = 0x37, "7";
    Digit8 = 0x38, "8";
    Digit9 = 0x39, "9";
    // Буквы.
    A = 0x41, "a";
    B = 0x42, "b";
    C = 0x43, "c";
    D = 0x44, "d";
    E = 0x45, "e";
    F = 0x46, "f";
    G = 0x47, "g";
    H = 0x48, "h";
    I = 0x49, "i";
    J = 0x4A, "j";
    K = 0x4B, "k";
    L = 0x4C, "l";
    M = 0x4D, "m";
    N = 0x4E, "n";
    O = 0x4F, "o";
    P = 0x50, "p";
    Q = 0x51, "q";
    R = 0x52, "r";
    S = 0x53, "s";
    T = 0x54, "t";
    U = 0x55, "u";
    V = 0x56, "v";
    W = 0x57, "w";
    X = 0x58, "x";
    Y = 0x59, "y";
    Z = 0x5A, "z";
    // Системные клавиши.
    LWin = 0x5B, "lwin";
    RWin = 0x5C, "rwin";
    Apps = 0x5D, "apps";
    // Нампад.
    Numpad0 = 0x60, "numpad0";
    Numpad1 = 0x61, "numpad1";
    Numpad2 = 0x62, "numpad2";
    Numpad3 = 0x63, "numpad3";
    Numpad4 = 0x64, "numpad4";
    Numpad5 = 0x65, "numpad5";
    Numpad6 = 0x66, "numpad6";
    Numpad7 = 0x67, "numpad7";
    Numpad8 = 0x68, "numpad8";
    Numpad9 = 0x69, "numpad9";
    NumpadMultiply = 0x6A, "numpad*";
    NumpadAdd = 0x6B, "numpad+";
    NumpadSubtract = 0x6D, "numpad-";
    NumpadDecimal = 0x6E, "numpad.";
    NumpadDivide = 0x6F, "numpad/";
    // F-клавиши.
    F1 = 0x70, "f1";
    F2 = 0x71, "f2";
    F3 = 0x72, "f3";
    F4 = 0x73, "f4";
    F5 = 0x74, "f5";
    F6 = 0x75, "f6";
    F7 = 0x76, "f7";
    F8 = 0x77, "f8";
    F9 = 0x78, "f9";
    F10 = 0x79, "f10";
    F11 = 0x7A, "f11";
    F12 = 0x7B, "f12";
    F13 = 0x7C, "f13";
    F14 = 0x7D, "f14";
    F15 = 0x7E, "f15";
    F16 = 0x7F, "f16";
    F17 = 0x80, "f17";
    F18 = 0x81, "f18";
    F19 = 0x82, "f19";
    F20 = 0x83, "f20";
    F21 = 0x84, "f21";
    F22 = 0x85, "f22";
    F23 = 0x86, "f23";
    F24 = 0x87, "f24";
    // Замки.
    NumLock = 0x90, "numlock";
    ScrollLock = 0x91, "scrolllock";
    // Модификаторы: логические и лево-/правосторонние.
    Shift = 0x10, "shift";
    Ctrl = 0x11, "ctrl";
    Alt = 0x12, "alt";
    LShift = 0xA0, "lshift";
    RShift = 0xA1, "rshift";
    LCtrl = 0xA2, "lctrl";
    RCtrl = 0xA3, "rctrl";
    LAlt = 0xA4, "lalt";
    RAlt = 0xA5, "ralt";
    CapsLock = 0x14, "capslock";
    // OEM-клавиши — физические позиции US-клавиатуры.
    Semicolon = 0xBA, ";";
    Equals = 0xBB, "=";
    Comma = 0xBC, ",";
    Minus = 0xBD, "-";
    Period = 0xBE, ".";
    Slash = 0xBF, "/";
    Backquote = 0xC0, "`";
    LBracket = 0xDB, "[";
    Backslash = 0xDC, "\\";
    RBracket = 0xDD, "]";
    Quote = 0xDE, "'";
}

impl Key {
    /// Зажата ли клавиша прямо сейчас.
    ///
    /// Опрос идёт у системы, не у движка: видно и то, что игра не переварила
    /// (кнопки мыши при «сыром» входе), и то, что проглотила блокировка
    /// [`blocker`]. Работает из любого потока.
    pub fn held(self) -> bool {
        held_vk(self.win_vk())
    }
}

/// Зажата ли клавиша с системным VK-кодом `vk` прямо сейчас.
///
/// То же, что [`Key::held`], но для кода напрямую — когда клавиша приходит,
/// например, из настройки. `GetAsyncKeyState` знает весь диапазон `0..255`;
/// вне его ответ всегда «отпущена».
pub fn held_vk(vk: u32) -> bool {
    if vk > 0xFF {
        return false;
    }
    platform::async_key_down(vk)
}

/// Состояние всех кнопок мыши одним опросом (диагностика входа).
///
/// Показывает, что система вообще считает зажатым: если мод ждёт одну
/// кнопку, а у мыши другая сторона или драйвер шлёт свой скан-код, разница
/// сразу видна по маске.
#[derive(Debug, Clone, Copy)]
pub struct MouseMask {
    /// Левая кнопка (VK 0x01).
    pub left: bool,
    /// Правая кнопка (VK 0x02).
    pub right: bool,
    /// Средняя кнопка/колесо (VK 0x04).
    pub middle: bool,
    /// Дополнительная 1 (VK 0x05).
    pub x1: bool,
    /// Дополнительная 2 (VK 0x06).
    pub x2: bool,
}

impl MouseMask {
    /// Опросить все кнопки мыши.
    pub fn held() -> Self {
        Self {
            left: held_vk(0x01),
            right: held_vk(0x02),
            middle: held_vk(0x04),
            x1: held_vk(0x05),
            x2: held_vk(0x06),
        }
    }
}

/// Сигнатура `Key_Event`: на x86 движок передаёт аргументы в `ECX`/`EDX`.
#[cfg(target_arch = "x86")]
type KeyEventFn = unsafe extern "fastcall" fn(i32, i32);

/// См. [`KeyEventFn`]: на чужих архитектурах тип нужен только чтобы код
/// собирался (тесты идут на x64, движка там нет).
#[cfg(not(target_arch = "x86"))]
type KeyEventFn = unsafe extern "C" fn(i32, i32);

/// Ошибка установки перехвата ввода.
#[derive(Debug)]
pub enum InputError {
    /// `hw.dll` ещё не загружен — ставить некуда.
    NoEngine,
    /// `retour` не смог поставить детур.
    Hook(golden_sdk::hook::HookError),
    /// Сборка не под 32-битный движок: соглашение вызова другое.
    Unsupported,
}

impl core::fmt::Display for InputError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoEngine => write!(f, "hw.dll не загружен"),
            Self::Hook(error) => write!(f, "перехват Key_Event: {error}"),
            Self::Unsupported => write!(f, "нужна сборка под i686"),
        }
    }
}

impl std::error::Error for InputError {}

impl From<golden_sdk::hook::HookError> for InputError {
    fn from(value: golden_sdk::hook::HookError) -> Self {
        Self::Hook(value)
    }
}

/// Набор заблокированных клавиш и счётчик проглоченных нажатий.
pub struct KeyBlocker {
    suppressed: [AtomicBool; KEYS],
    swallowed: AtomicUsize,
}

impl KeyBlocker {
    /// Пустой набор: ничего не заблокировано.
    pub const fn new() -> Self {
        Self {
            suppressed: [const { AtomicBool::new(false) }; KEYS],
            swallowed: AtomicUsize::new(0),
        }
    }

    /// Блокирует клавишу `vk` (virtual-key код). `true`, если перехват стоит и
    /// клавиша теперь действительно глушится.
    ///
    /// Без установленного перехвата возвращает `false` и ничего не меняет:
    /// «заблокировано» должна означать реально работающая блокировка, а не
    /// обещание.
    pub fn suppress(&self, vk: u32) -> bool {
        if vk as usize >= KEYS || !installed() {
            return false;
        }
        self.suppressed[vk as usize].store(true, Ordering::Release);
        true
    }

    /// Снимает блокировку с `vk`. `true`, если перехват стоит.
    pub fn release(&self, vk: u32) -> bool {
        if vk as usize >= KEYS || !installed() {
            return false;
        }
        self.suppressed[vk as usize].store(false, Ordering::Release);
        true
    }

    /// Снимает блокировку со всех клавиш.
    pub fn release_all(&self) {
        for key in &self.suppressed {
            key.store(false, Ordering::Release);
        }
    }

    /// Заблокирована ли клавиша прямо сейчас.
    pub fn is_suppressed(&self, vk: u32) -> bool {
        (vk as usize) < KEYS && self.suppressed[vk as usize].load(Ordering::Acquire)
    }

    /// Все заблокированные клавиши — для отчётов и логов.
    pub fn held(&self) -> Vec<u32> {
        let mut keys = Vec::new();
        for (vk, cell) in self.suppressed.iter().enumerate() {
            if cell.load(Ordering::Acquire) {
                keys.push(vk as u32);
            }
        }
        keys
    }

    /// Сколько нажатий перехват уже проглотил.
    pub fn swallowed(&self) -> usize {
        self.swallowed.load(Ordering::Relaxed)
    }

    fn count_swallowed(&self) {
        self.swallowed.fetch_add(1, Ordering::Relaxed);
    }
}

impl Default for KeyBlocker {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for KeyBlocker {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KeyBlocker")
            .field("held", &self.held())
            .field("swallowed", &self.swallowed())
            .finish()
    }
}

/// Глобальный набор: с ним работает и перехват, и вызывающий код.
static BLOCKER: KeyBlocker = KeyBlocker::new();

// --- колесо мыши ---

/// Один щелчок колеса вверх (`WHEEL_DELTA`).
pub const WHEEL_UP: i32 = 120;

/// Один щелчок колеса вниз.
pub const WHEEL_DOWN: i32 = -120;

/// Окно игры, найденное по PID; `0` — ещё не искали.
static WINDOW: AtomicUsize = AtomicUsize::new(0);

/// Главное окно процесса игры.
///
/// Ищем перебором окон, принадлежащих нашему процессу: так не нужно ни класса
/// окна, ни заголовка, которые меняются от сборки к сборке.
pub fn game_window() -> Option<usize> {
    let cached = WINDOW.load(Ordering::Acquire);
    if cached != 0 && platform::is_window(cached) {
        return Some(cached);
    }
    let window = platform::main_window(std::process::id())?;
    WINDOW.store(window, Ordering::Release);
    Some(window)
}

/// Прокручивает колесо **в окно игры**, не трогая системную мышь.
///
/// Это принципиально другой путь, чем команды через `Cbuf_AddText`. Команда из
/// нашего кадра попадает в буфер и исполняется только на следующем кадре —
/// то есть прыжок происходит уже после кадра приземления, и трение успевает
/// съесть скорость. Сообщение `WM_MOUSEWHEEL` движок разбирает сам, в своём
/// насосе сообщений, и бинд (`mwheelup +jump`) срабатывает в том же кадре.
///
/// Бинд должен быть у игрока: в CS 1.6 по умолчанию это `bind mwheelup +jump`.
pub fn scroll(delta: i32) -> bool {
    match game_window() {
        Some(window) => platform::post_wheel(window, delta),
        None => false,
    }
}

/// Win32-часть: окно и сообщение колеса.
#[cfg(windows)]
mod platform {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// `WM_MOUSEWHEEL`.
    const WM_MOUSEWHEEL: u32 = 0x020A;

    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(callback: extern "system" fn(usize, isize) -> i32, param: isize) -> i32;
        fn GetAsyncKeyState(key: i32) -> i16;
        fn GetWindowThreadProcessId(window: usize, pid: *mut u32) -> u32;
        fn GetClientRect(window: usize, rect: *mut Rect) -> i32;
        fn IsWindow(window: usize) -> i32;
        fn IsWindowVisible(window: usize) -> i32;
        fn PostMessageW(window: usize, message: u32, wparam: usize, lparam: isize) -> i32;
    }

    /// PID, окно которого ищем (одно окно за раз — поиск идёт из одного потока).
    static TARGET_PID: AtomicUsize = AtomicUsize::new(0);

    /// Найденное окно.
    static FOUND: AtomicUsize = AtomicUsize::new(0);

    pub fn main_window(pid: u32) -> Option<usize> {
        TARGET_PID.store(pid as usize, Ordering::Release);
        FOUND.store(0, Ordering::Release);
        // SAFETY: колбэк соответствует сигнатуре `WNDENUMPROC`, состояние — только
        // наши статики.
        unsafe { EnumWindows(visit, 0) };
        match FOUND.load(Ordering::Acquire) {
            0 => None,
            window => Some(window),
        }
    }

    extern "system" fn visit(window: usize, _param: isize) -> i32 {
        // SAFETY: стандартные вызовы user32 по валидному дескриптору из перебора.
        unsafe {
            if IsWindowVisible(window) == 0 {
                return 1;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, &mut pid);
            if pid as usize != TARGET_PID.load(Ordering::Acquire) {
                return 1;
            }
            // Служебные окна движка крошечные: отличаем их по клиентской области.
            let mut rect = Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if GetClientRect(window, &mut rect) == 0 {
                return 1;
            }
            if rect.right - rect.left < 16 || rect.bottom - rect.top < 16 {
                return 1;
            }
            FOUND.store(window, Ordering::Release);
            0 // окно нашли — перебор дальше не нужен
        }
    }

    pub fn is_window(window: usize) -> bool {
        // SAFETY: проверка дескриптора, побочных эффектов нет.
        unsafe { IsWindow(window) != 0 }
    }

    pub fn post_wheel(window: usize, delta: i32) -> bool {
        // Старшее слово `wParam` — величина прокрутки со знаком, младшее — состояние
        // клавиш; движку достаточно величины.
        let wparam = ((delta as u32) & 0xFFFF) << 16;
        // SAFETY: сообщение кладём в очередь окна игры, координаты курсора не нужны.
        unsafe { PostMessageW(window, WM_MOUSEWHEEL, wparam as usize, 0) != 0 }
    }

    /// Зажата ли клавиша с VK-кодом `vk` прямо сейчас.
    ///
    /// Код ограничен обёрткой [`super::held_vk`].
    pub fn async_key_down(vk: u32) -> bool {
        // SAFETY: системный опрос без аргументов-указателей.
        unsafe { GetAsyncKeyState(vk as i32) & super::KEY_DOWN != 0 }
    }
}

/// Заглушка для тестов и сборок не под Windows.
#[cfg(not(windows))]
mod platform {
    pub fn main_window(_pid: u32) -> Option<usize> {
        None
    }

    pub fn is_window(_window: usize) -> bool {
        false
    }

    pub fn post_wheel(_window: usize, _delta: i32) -> bool {
        false
    }

    pub fn async_key_down(_vk: u32) -> bool {
        false
    }
}

/// Хранилище детура — держим его живым на всё время работы DLL.
static DETOUR: Mutex<Option<Detour>> = Mutex::new(None);

/// Трамплин к оригинальному `Key_Event` (`0` — перехват не поставлен).
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

/// Что игрок физически держит прямо сейчас.
///
/// Заполняется в перехвате **до** решения «проглотить»: клавиша в игру не
/// уходит, но мы знаем, что её нажали. Это и есть разница между «пробел задат»
/// и «игра видит пробел».
static PHYSICAL: [AtomicBool; KEYS] = [const { AtomicBool::new(false) }; KEYS];

/// База `hw.dll` на момент установки: по ней читается `key_dest`.
static ENGINE_BASE: AtomicUsize = AtomicUsize::new(0);

/// Поставлен ли перехват.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Глобальный набор заблокированных клавиш.
pub fn blocker() -> &'static KeyBlocker {
    &BLOCKER
}

/// Нажимает клавишу от лица игры: вызывает оригинальный `Key_Event(vk, 1)`.
///
/// Это не «синтетическое сообщение окна», а ровно тот путь, которым идёт
/// настоящее нажатие: движок пишет `keys[vk] = 1`, находит бинд и кладёт в
/// командный буфер `+jump 32` — то есть срабатывают те же бинды, что у игрока.
///
/// Блокировка из [`KeyBlocker`] на этот вызов не действует: она стоит в
/// перехвате, а здесь мы зовём оригинал напрямую. Иначе синтетическое нажатие
/// глушило бы само себя.
///
/// Возвращает `false`, если перехват не поставлен ([`install`]) — без него нет
/// трамплина к оригиналу.
pub fn press(vk: u32) -> bool {
    call_original(vk, 1)
}

/// Отпускает клавишу от лица игры: оригинальный `Key_Event(vk, 0)`.
///
/// Пара к [`press`]. Пропускать отпускание нельзя, иначе движок считает клавишу
/// зажатой: `in_jump` останется `1`, а сервер (`pmove->oldbuttons & IN_JUMP`)
/// перестанет разрешать следующий прыжок.
pub fn lift(vk: u32) -> bool {
    call_original(vk, 0)
}

fn call_original(vk: u32, down: i32) -> bool {
    if !installed() {
        return false;
    }
    let original = ORIGINAL.load(Ordering::Acquire);
    if original == 0 {
        return false;
    }
    // SAFETY: `original` — трамплин `retour` к настоящему `Key_Event`, с той же
    // сигнатурой `__fastcall(int keynum, int down)`.
    let call: KeyEventFn = unsafe { core::mem::transmute(original) };
    unsafe { call(vk as i32, down) };
    true
}

/// Ставит перехват `Key_Event`. Повторный вызов ничего не делает.
///
/// Звать после загрузки `hw.dll` и `client.dll` (см. `process::hw_base`).
pub fn install() -> Result<(), InputError> {
    if !cfg!(target_arch = "x86") {
        return Err(InputError::Unsupported);
    }
    if installed() {
        return Ok(());
    }
    let base = process::hw_base().ok_or(InputError::NoEngine)?;
    // SAFETY: `base + KEY_EVENT` — начало `Key_Event` этого движка; наш хук
    // объявлен с тем же соглашением (`__fastcall`, `int, int`).
    let detour =
        unsafe { Detour::new(base + offsets::hw::KEY_EVENT, hook_key_event as *const ()) }?;
    detour.enable()?;

    ORIGINAL.store(detour.trampoline() as usize, Ordering::Release);
    ENGINE_BASE.store(base, Ordering::Release);
    if let Ok(mut slot) = DETOUR.lock() {
        *slot = Some(detour);
    }
    INSTALLED.store(true, Ordering::Release);
    Ok(())
}

/// Стоит ли перехват `Key_Event` (после него блокировка реально работает).
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

/// Снимает перехват `Key_Event` и отпускает клавиши, которые мод удерживал.
///
/// Вызывается при выгрузке DLL: с включённым хуком выгружать модуль нельзя —
/// движок прыгнет в освобождённую страницу. Возвращает `false`, если перехвата
/// не было.
pub fn shutdown() -> bool {
    // Сначала отпускаем клавиши: движок должен увидеть `-key`, пока перехват
    // ещё стоит, иначе клавиша останется зажатой уже без мода.
    blocker().release_all();
    let detour = DETOUR.lock().ok().and_then(|mut slot| slot.take());
    let Some(detour) = detour else {
        INSTALLED.store(false, Ordering::Release);
        return false;
    };
    let disabled = detour.disable().is_ok();
    // Детур освобождает код трамплина в `Drop`, поэтому держим его только до
    // этого места и дальше не используем.
    drop(detour);
    ORIGINAL.store(0, Ordering::Release);
    INSTALLED.store(false, Ordering::Release);
    disabled
}

/// Перехват: глушит заблокированные клавиши, остальные передаёт движку.
///
/// # Safety
/// Вызывается движком из потока, который обрабатывает сообщения окна.
#[cfg(target_arch = "x86")]
unsafe extern "fastcall" fn hook_key_event(key: i32, down: i32) {
    pass_key_event(key, down)
}

/// См. [`hook_key_event`].
///
/// # Safety
/// См. [`hook_key_event`].
#[cfg(not(target_arch = "x86"))]
unsafe extern "C" fn hook_key_event(key: i32, down: i32) {
    pass_key_event(key, down)
}

fn pass_key_event(key: i32, down: i32) {
    note_physical(key, down);
    if should_swallow(key) {
        return;
    }
    let original = ORIGINAL.load(Ordering::Acquire);
    if original == 0 {
        return;
    }
    // SAFETY: `original` — трамплин `retour` к настоящему `Key_Event` с той же
    // сигнатурой и соглашением вызова.
    let call: KeyEventFn = unsafe { core::mem::transmute(original) };
    unsafe { call(key, down) }
}

/// Решение «проглотить нажатие или отдать движку».
fn should_swallow(key: i32) -> bool {
    if !(0..KEYS as i32).contains(&key) || !BLOCKER.is_suppressed(key as u32) {
        return false;
    }
    if text_input_open() {
        return false;
    }
    BLOCKER.count_swallowed();
    true
}

/// Запоминает физическое состояние клавиши — до того, как событие проглотят.
fn note_physical(key: i32, down: i32) {
    if (0..KEYS as i32).contains(&key) {
        PHYSICAL[key as usize].store(down != 0, Ordering::Release);
    }
}

/// Держит ли игрок клавишу `vk` физически.
///
/// Работает и для заблокированных клавиш: перехват `Key_Event` видит нажатие
/// раньше, чем решает его проглотить. Так «пробел задат» остаётся известно, хотя
/// игра пробела не видит.
pub fn is_down(vk: u32) -> bool {
    (vk as usize) < KEYS && PHYSICAL[vk as usize].load(Ordering::Acquire)
}

/// Открыт чат или консоль: там клавиши нужны для ввода текста.
fn text_input_open() -> bool {
    let base = ENGINE_BASE.load(Ordering::Acquire);
    if base == 0 {
        return false;
    }
    // SAFETY: `key_dest` — глобальная переменная движка по известному смещению,
    // чтение не меняет состояние.
    unsafe { process::read::<i32>(base + offsets::hw::CHAT_OPENED) != 0 }
}
