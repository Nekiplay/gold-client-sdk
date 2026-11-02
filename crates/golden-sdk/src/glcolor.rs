//! Цвет геометрии, которую рисует движок: подмена его таблицы GL-функций.
//!
//! ## Зачем
//!
//! Контур модели рисуется через `StudioRenderModel` — то есть кодом движка.
//! Свой цвет ему не передать: внутри рендера движок сам зовёт `glColor3f` /
//! `glColor4f` перед каждой гранью, и последний вызов выигрывает. Поэтому
//! `glColor4f` в нашём GL-скоупе ничего не даёт, и линии выходят белыми.
//!
//! ## Как
//!
//! `hw.dll` почти не импортирует OpenGL напрямую: в таблице импорта всего
//! пара десятков `gl*`, а остальное движок достаёт через `GetProcAddress` по
//! именам (`"glColor3f"`, `"glColor4f"`, …) и складывает в свою таблицу
//! указателей в **записываемой** памяти. Именно по этой таблице идут все
//! вызовы при отрисовке.
//!
//! Значит, точки, где можно влиять на цвет, — это ячейки таблицы. Мы находим
//! их поиском в образе `hw.dll` значения, равного настоящему адресу экспорта
//! `opengl32!glColor*`, и подменяем на обёртку. Обёртка при активном
//! переопределении отдаёт наш цвет и только потом зовёт оригинал, иначе просто
//! пропускает вызов дальше. Код движка не трогаем, `opengl32.dll` не патчим.
//!
//! ```no_run
//! // один раз при старте
//! let slots = golden_sdk::glcolor::install();
//! # let _ = slots;
//! // на время отрисовки своего контура
//! unsafe {
//!     golden_sdk::glcolor::begin(golden_sdk::Color::rgba(1.0, 0.35, 0.15, 1.0));
//!     // ... вызов рендера модели ...
//!     golden_sdk::glcolor::end();
//! }
//! ```

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::draw::Color;
use crate::process;

/// `glColor*`-функции, которыми движок задаёт цвет.
///
/// Порядок важен: по индексу лежат оригиналы в [`ORIGINALS`] и обёртки в
/// [`WRAPPERS`].
pub const NAMES: [&str; 6] = [
    "glColor3f",
    "glColor4f",
    "glColor3fv",
    "glColor4fv",
    "glColor3ub",
    "glColor4ub",
];

/// Адреса настоящих функций (заполняются в [`install`]).
static ORIGINALS: [AtomicUsize; NAMES.len()] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];

/// Сколько ячеек таблицы удалось подменить.
static SLOTS: AtomicUsize = AtomicUsize::new(0);

/// Сколько раз движок звал каждую из обёрток.
///
/// Это единственный способ узнать, каким именно способом он задаёт цвет:
/// если, например, `glColor3f` растёт, а `glColor4f` нет, значит цвет идёт
/// через 3-компонентную форму.
static HITS: [AtomicU32; NAMES.len()] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

/// Переопределённый цвет: `0` — выключено, иначе `0x0100_0000 | r << 16 | g << 8 | b`.
static FORCED: AtomicU32 = AtomicU32::new(0);

const ENABLED: u32 = 0x0100_0000;

/// Включает переопределение цвета до парного [`end`].
///
/// # Safety
/// Вызывать из потока с активным GL-контекстом, между `begin` и `end` не
/// должно быть чужих отрисовок.
pub unsafe fn begin(color: Color) {
    let r = (color.r.clamp(0.0, 1.0) * 255.0) as u32;
    let g = (color.g.clamp(0.0, 1.0) * 255.0) as u32;
    let b = (color.b.clamp(0.0, 1.0) * 255.0) as u32;
    FORCED.store(ENABLED | (r << 16) | (g << 8) | b, Ordering::Release);
}

/// Возвращает цвет движку.
///
/// # Safety
/// Парная к [`begin`].
pub unsafe fn end() {
    FORCED.store(0, Ordering::Release);
}

/// Сколько ячеек таблицы подменено (0 — движок рисует своим цветом).
pub fn slots() -> usize {
    SLOTS.load(Ordering::Acquire)
}

/// Счётчики вызовов обёрток: `[(имя функции, сколько раз позвали)]`.
///
/// Нужны для отчёта: по ним видно, через какую `glColor*` движок красит модель.
pub fn stats() -> alloc::vec::Vec<(&'static str, u32)> {
    NAMES
        .iter()
        .enumerate()
        .map(|(index, name)| (*name, HITS[index].load(Ordering::Relaxed)))
        .collect()
}

/// Отмечает вызов обёртки в счётчике.
fn hit(kind: usize) {
    HITS[kind].fetch_add(1, Ordering::Relaxed);
}

/// Ставит подмену один раз за процесс (идемпотентно).
///
/// Возвращает число подменённых ячеек; повторный вызов лишь отдаёт сохранённое
/// значение, поэтому звать можно из горячего пути.
pub fn install_once() -> usize {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.load(Ordering::Acquire) {
        return slots();
    }
    let count = install();
    INSTALLED.store(true, Ordering::Release);
    crate::log!("цвет движка: подменено ячеек таблицы GL — {count}");
    count
}

/// Текущее переопределение, если оно активно.
fn forced() -> Option<(f32, f32, f32)> {
    let value = FORCED.load(Ordering::Acquire);
    if value & ENABLED == 0 {
        return None;
    }
    let r = ((value >> 16) & 0xFF) as f32 / 255.0;
    let g = ((value >> 8) & 0xFF) as f32 / 255.0;
    let b = (value & 0xFF) as f32 / 255.0;
    Some((r, g, b))
}

/// Ищет таблицу GL-указателей в `hw.dll` и подменяет цветовые ячейки.
///
/// Если таблицы не нашлось (движок может достать GL-функции и не сохраняя
/// их в записываемой памяти), обёртки ставятся inline-хуком на сами экспорты
/// `opengl32.dll` — результат для цвета тот же, только с накладными расходами
/// на вызов.
///
/// Возвращает число подменённых точек: 0 значит, что цвет останется движковым.
pub fn install() -> usize {
    // Адреса обёрток считаются здесь, а не в статике: Rust не разрешает
    // приводить указатели на функции к числам на этапе компиляции.
    let wrappers = [
        color3f as unsafe extern "stdcall" fn(f32, f32, f32) as usize,
        color4f as unsafe extern "stdcall" fn(f32, f32, f32, f32) as usize,
        color3fv as unsafe extern "stdcall" fn(*const f32) as usize,
        color4fv as unsafe extern "stdcall" fn(*const f32) as usize,
        color3ub as unsafe extern "stdcall" fn(u8, u8, u8) as usize,
        color4ub as unsafe extern "stdcall" fn(u8, u8, u8, u8) as usize,
    ];

    let patched = match process::hw_base().and_then(|hw| unsafe { process::module_size(hw) }) {
        // SAFETY: база и размер образа получены у загрузчика.
        Some(size) => {
            let hw = process::hw_base().unwrap_or(0);
            patch_table(hw, size, &wrappers)
        }
        None => 0,
    };
    let count = if patched > 0 {
        patched
    } else {
        install_detours(&wrappers)
    };
    SLOTS.store(count, Ordering::Release);
    count
}

/// Подменяет ячейки таблицы GL-функций внутри образа `hw.dll`.
fn patch_table(hw: usize, size: usize, wrappers: &[usize; NAMES.len()]) -> usize {
    let mut patched = 0;
    for (kind, name) in NAMES.iter().enumerate() {
        let Some(original) = process::export("opengl32.dll", name) else {
            continue;
        };
        ORIGINALS[kind].store(original, Ordering::Release);
        let wrapper = wrappers[kind];
        // Ячейка таблицы — dword, выровненный на 4 байта.
        for offset in (0..size.saturating_sub(4)).step_by(4) {
            let addr = hw + offset;
            // SAFETY: адрес внутри образа модуля; проверка «можно ли писать»
            // ниже не даёт задеть код.
            let value: usize = unsafe { process::read(addr) };
            if value != original || !process::is_writable(addr) {
                continue;
            }
            // SAFETY: ячейка в записываемой памяти и хранит ровно нужный адрес.
            unsafe { process::write(addr, wrapper) };
            patched += 1;
        }
    }
    patched
}

/// Ставит обёртки inline-хуком на экспорты `opengl32.dll`.
///
/// Хук живёт до конца процесса, поэтому детуры утекают намеренно: снять их в
/// момент выгрузки мода нельзя, а держать в статике — лишний `Mutex` в пути.
fn install_detours(wrappers: &[usize; NAMES.len()]) -> usize {
    let mut count = 0;
    for (kind, name) in NAMES.iter().enumerate() {
        let original = match ORIGINALS[kind].load(Ordering::Acquire) {
            0 => {
                let Some(found) = process::export("opengl32.dll", name) else {
                    continue;
                };
                ORIGINALS[kind].store(found, Ordering::Release);
                found
            }
            found => found,
        };
        // SAFETY: `original` — начало экспорта `opengl32!glColor*`, обёртка
        // имеет ту же сигнатуру и соглашение вызова.
        let detour = unsafe { crate::hook::Detour::new(original, wrappers[kind] as *const ()) };
        let Ok(detour) = detour else {
            continue;
        };
        if detour.enable().is_err() {
            continue;
        }
        // Дальше обёртка зовёт оригинал через трамплин детура.
        ORIGINALS[kind].store(detour.trampoline() as usize, Ordering::Release);
        alloc::boxed::Box::leak(alloc::boxed::Box::new(detour));
        count += 1;
    }
    count
}

/// Вызывает оригинал по индексу, если он известен.
///
/// # Safety
/// Сигнатура функции по индексу должна совпадать с вызовом.
unsafe fn call3(kind: usize, a: f32, b: f32, c: f32) {
    let addr = ORIGINALS[kind].load(Ordering::Acquire);
    if addr == 0 {
        return;
    }
    // SAFETY: `addr` — экспорт `opengl32!glColor3f` (см. `install`).
    let f: unsafe extern "stdcall" fn(f32, f32, f32) = unsafe { core::mem::transmute(addr) };
    unsafe { f(a, b, c) }
}

/// # Safety
/// Вызывается движком через таблицу GL-функций.
unsafe extern "stdcall" fn color3f(r: f32, g: f32, b: f32) {
    hit(0);
    let (r, g, b) = forced().unwrap_or((r, g, b));
    // SAFETY: оригинал известен и совпадает по сигнатуре.
    unsafe { call3(0, r, g, b) }
}

/// # Safety
/// Вызывается движком через таблицу GL-функций.
unsafe extern "stdcall" fn color4f(r: f32, g: f32, b: f32, a: f32) {
    hit(1);
    let addr = ORIGINALS[1].load(Ordering::Acquire);
    if addr == 0 {
        return;
    }
    let (r, g, b, a) = match forced() {
        Some((r, g, b)) => (r, g, b, 1.0),
        None => (r, g, b, a),
    };
    // SAFETY: `addr` — экспорт `opengl32!glColor4f`.
    let f: unsafe extern "stdcall" fn(f32, f32, f32, f32) = unsafe { core::mem::transmute(addr) };
    unsafe { f(r, g, b, a) }
}

/// # Safety
/// `v` должен указывать на три `float`.
unsafe extern "stdcall" fn color3fv(v: *const f32) {
    hit(2);
    let addr = ORIGINALS[2].load(Ordering::Acquire);
    if addr == 0 || v.is_null() {
        return;
    }
    // SAFETY: `addr` — экспорт `opengl32!glColor3fv`, `v` — 3 float по контракту.
    let f: unsafe extern "stdcall" fn(*const f32) = unsafe { core::mem::transmute(addr) };
    match forced() {
        // SAFETY: тот же контракт, что у оригинала.
        Some((r, g, b)) => unsafe { f([r, g, b].as_ptr()) },
        None => unsafe { f(v) },
    }
}

/// # Safety
/// `v` должен указывать на четыре `float`.
unsafe extern "stdcall" fn color4fv(v: *const f32) {
    hit(3);
    let addr = ORIGINALS[3].load(Ordering::Acquire);
    if addr == 0 || v.is_null() {
        return;
    }
    // SAFETY: `addr` — экспорт `opengl32!glColor4fv`, `v` — 4 float по контракту.
    let f: unsafe extern "stdcall" fn(*const f32) = unsafe { core::mem::transmute(addr) };
    match forced() {
        // SAFETY: тот же контракт, что у оригинала.
        Some((r, g, b)) => unsafe { f([r, g, b, 1.0].as_ptr()) },
        None => unsafe { f(v) },
    }
}

/// # Safety
/// Вызывается движком через таблицу GL-функций.
unsafe extern "stdcall" fn color3ub(r: u8, g: u8, b: u8) {
    hit(4);
    let addr = ORIGINALS[4].load(Ordering::Acquire);
    if addr == 0 {
        return;
    }
    // SAFETY: `addr` — экспорт `opengl32!glColor3ub`.
    let f: unsafe extern "stdcall" fn(u8, u8, u8) = unsafe { core::mem::transmute(addr) };
    match forced() {
        Some((r, g, b)) => unsafe { f((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8) },
        None => unsafe { f(r, g, b) },
    }
}

/// # Safety
/// Вызывается движком через таблицу GL-функций.
unsafe extern "stdcall" fn color4ub(r: u8, g: u8, b: u8, a: u8) {
    hit(5);
    let addr = ORIGINALS[5].load(Ordering::Acquire);
    if addr == 0 {
        return;
    }
    // SAFETY: `addr` — экспорт `opengl32!glColor4ub`.
    let f: unsafe extern "stdcall" fn(u8, u8, u8, u8) = unsafe { core::mem::transmute(addr) };
    match forced() {
        Some((r, g, b)) => unsafe {
            f((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 255)
        },
        None => unsafe { f(r, g, b, a) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_color_roundtrip() {
        assert!(forced().is_none());
        // SAFETY: тест не трогает GL — только атомарное состояние.
        unsafe { begin(Color::rgba(1.0, 0.35, 0.15, 1.0)) };
        let (r, g, b) = forced().expect("цвет включён");
        assert!((r - 1.0).abs() < 0.01);
        assert!((g - 0.35).abs() < 0.01);
        assert!((b - 0.15).abs() < 0.01);
        // SAFETY: парный вызов.
        unsafe { end() };
        assert!(forced().is_none());
    }
}
