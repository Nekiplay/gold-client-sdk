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
            let mut rect = Rect { left: 0, top: 0, right: 0, bottom: 0 };
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
    let detour = unsafe { Detour::new(base + offsets::hw::KEY_EVENT, hook_key_event as *const ()) }?;
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