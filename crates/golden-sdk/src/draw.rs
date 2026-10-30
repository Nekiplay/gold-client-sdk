//! Рисование линий поверх кадра средствами OpenGL.
//!
//! Движок (`hw.dll`) рисует через `opengl32.dll`, поэтому из своего кода можно
//! звать те же функции: контекст и матрицы уже настроены, а мировая система
//! координат активна всё время, пока движок обходит сущности. Значит из хука
//! отрисовки модели можно рисовать **в мировых координатах**, не трогая матрицы.
//!
//! Всё рисование идёт внутри [`Gl`]-скоупа, который сохраняет состояние
//! (`glPushAttrib(GL_ALL_ATTRIB_BITS)`) и возвращает его обратно: у движка свой
//! кеш GL-состояния, и после нас он должен увидеть ровно то, что оставил.
//!
//! Тест глубины при этом **не** отключается: линия, закрытая стеной, не видна —
//! оверлей остаётся отладочным, а не «сквозь стены».
//!
//! # Безопасность
//!
//! Все функции требуют активного GL-контекста, то есть вызова из рендер-потока
//! игры (из хука отрисовки модели или HUD). Из другого потока вызов упадёт или
//! нарисует в чужой контекст.

use core::ffi::CStr;
use core::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::System::LibraryLoader::{
    GetModuleHandleA, GetProcAddress, LoadLibraryA,
};

use crate::math::Vec3;

/// `GL_LINES`.
pub const GL_LINES: u32 = 0x0001;
const GL_BLEND: u32 = 0x0BE2;
const GL_SRC_ALPHA: u32 = 0x0302;
const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_DEPTH_TEST: u32 = 0x0B71;
const GL_ALL_ATTRIB_BITS: u32 = 0x000F_FFFF;

/// Как рисовать относительно теста глубины.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// С тестом: линия прячется за моделью, стеной, чем угодно. Так выглядит
    /// отладочная геометрия, привязанная к миру.
    Test,
    /// Без теста: линия видна сквозь всё, включая стены. Это уже не отладка
    /// мира, а оверлей — так рисуют скелеты, которые хотят видеть всегда.
    Ignore,
}

/// Цвет в диапазоне `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const WHITE: Self = Self::rgba(1.0, 1.0, 1.0, 1.0);
    pub const RED: Self = Self::rgba(1.0, 0.2, 0.2, 1.0);
    pub const GREEN: Self = Self::rgba(0.2, 1.0, 0.3, 1.0);
    pub const YELLOW: Self = Self::rgba(1.0, 0.9, 0.2, 1.0);
    pub const CYAN: Self = Self::rgba(0.2, 0.9, 1.0, 1.0);

    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self::rgba(r, g, b, 1.0)
    }

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

type BeginFn = unsafe extern "system" fn(u32);
type EndFn = unsafe extern "system" fn();
type Vertex3fFn = unsafe extern "system" fn(f32, f32, f32);
type Color4fFn = unsafe extern "system" fn(f32, f32, f32, f32);
type EnableFn = unsafe extern "system" fn(u32);
type LineWidthFn = unsafe extern "system" fn(f32);
type PushAttribFn = unsafe extern "system" fn(u32);
type PopAttribFn = unsafe extern "system" fn();
type BlendFuncFn = unsafe extern "system" fn(u32, u32);

/// Указатели на нужные функции `opengl32.dll`.
#[derive(Debug)]
struct Fns {
    begin: BeginFn,
    end: EndFn,
    vertex3f: Vertex3fFn,
    color4f: Color4fFn,
    enable: EnableFn,
    disable: EnableFn,
    line_width: LineWidthFn,
    push_attrib: PushAttribFn,
    pop_attrib: PopAttribFn,
    blend_func: BlendFuncFn,
}

/// Доступ к рисованию OpenGL.
#[derive(Debug)]
pub struct Gl {
    fns: Fns,
}

static GL: AtomicUsize = AtomicUsize::new(0);

impl Gl {
    /// Загружает `opengl32.dll` (если ещё не загружен) и резолвит функции.
    ///
    /// # Safety
    /// Вызывать только из потока с активным GL-контекстом игры.
    pub unsafe fn get() -> Option<&'static Self> {
        let cached = GL.load(Ordering::Acquire);
        if cached != 0 {
            // SAFETY: в кеше лежит `Box::leak` из этой же функции.
            return Some(unsafe { &*(cached as *const Self) });
        }
        // SAFETY: имена NUL-терминированы, вызовы Win32 без побочных эффектов.
        let gl = unsafe { Self::load() }?;
        let leak = Box::leak(Box::new(gl)) as *mut Self as usize;
        GL.store(leak, Ordering::Release);
        // SAFETY: только что создали и утекли объект.
        Some(unsafe { &*(leak as *const Self) })
    }

    /// # Safety
    /// См. [`Gl::get`].
    unsafe fn load() -> Option<Self> {
        let name = c"opengl32.dll";
        // SAFETY: строка NUL-терминирована.
        let module = unsafe { GetModuleHandleA(name.as_ptr().cast()) };
        let module = if module.is_null() {
            // SAFETY: то же; грузим, если движок ещё не успел.
            unsafe { LoadLibraryA(name.as_ptr().cast()) }
        } else {
            module
        };
        if module.is_null() {
            return None;
        }
        // SAFETY: сигнатуры ниже — стандартные точки входа GL 1.1.
        unsafe {
            Some(Self {
                fns: Fns {
                    begin: resolve(module, c"glBegin")?,
                    end: resolve(module, c"glEnd")?,
                    vertex3f: resolve(module, c"glVertex3f")?,
                    color4f: resolve(module, c"glColor4f")?,
                    enable: resolve(module, c"glEnable")?,
                    disable: resolve(module, c"glDisable")?,
                    line_width: resolve(module, c"glLineWidth")?,
                    push_attrib: resolve(module, c"glPushAttrib")?,
                    pop_attrib: resolve(module, c"glPopAttrib")?,
                    blend_func: resolve(module, c"glBlendFunc")?,
                },
            })
        }
    }

    /// Рисует набор отрезков в текущих координатах.
    ///
    /// Вызов из хука отрисовки модели рисует в мировых координатах; вызов из
    /// HUD — в пикселях экрана.
    ///
    /// `depth` по умолчанию — [`Depth::Test`] через [`Gl::lines`]; если нужно
    /// видеть линии сквозь модель, зови [`Gl::lines_xray`].
    ///
    /// # Safety
    /// См. [`Gl::get`].
    pub unsafe fn lines(&self, color: Color, width: f32, segments: &[(Vec3, Vec3)]) {
        // SAFETY: контракт передан вызывающим.
        unsafe { self.lines_with(color, width, segments, Depth::Test) }
    }

    /// То же, но линии видны сквозь модели и стены.
    ///
    /// # Safety
    /// См. [`Gl::get`].
    pub unsafe fn lines_xray(&self, color: Color, width: f32, segments: &[(Vec3, Vec3)]) {
        // SAFETY: контракт передан вызывающим.
        unsafe { self.lines_with(color, width, segments, Depth::Ignore) }
    }

    /// Рисует отрезки с явно выбранным режимом глубины.
    ///
    /// # Safety
    /// См. [`Gl::get`].
    pub unsafe fn lines_with(
        &self,
        color: Color,
        width: f32,
        segments: &[(Vec3, Vec3)],
        depth: Depth,
    ) {
        if segments.is_empty() {
            return;
        }
        // SAFETY: контракт передан вызывающим: контекст активен в этом потоке.
        unsafe {
            (self.fns.push_attrib)(GL_ALL_ATTRIB_BITS);
            (self.fns.disable)(GL_TEXTURE_2D);
            match depth {
                Depth::Test => (self.fns.enable)(GL_DEPTH_TEST),
                Depth::Ignore => (self.fns.disable)(GL_DEPTH_TEST),
            }
            (self.fns.enable)(GL_BLEND);
            (self.fns.blend_func)(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            (self.fns.line_width)(width.clamp(1.0, 10.0));
            (self.fns.color4f)(color.r, color.g, color.b, color.a);
            (self.fns.begin)(GL_LINES);
            for (a, b) in segments {
                (self.fns.vertex3f)(a.x, a.y, a.z);
                (self.fns.vertex3f)(b.x, b.y, b.z);
            }
            (self.fns.end)();
            (self.fns.pop_attrib)();
        }
    }
}

/// Резолвит точку входа модуля.
///
/// # Safety
/// `module` должен быть валидным HMODULE, `name` — NUL-терминированным именем.
unsafe fn resolve<T: Copy>(module: *mut core::ffi::c_void, name: &CStr) -> Option<T> {
    // SAFETY: контракт функции.
    let proc = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
    if core::mem::size_of::<T>() != core::mem::size_of_val(&proc) {
        return None;
    }
    // SAFETY: адрес получен из таблицы экспортов; тип проверен по размеру.
    Some(unsafe { core::mem::transmute_copy(&proc) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_in_range() {
        for c in [Color::WHITE, Color::RED, Color::GREEN, Color::YELLOW, Color::CYAN] {
            assert!((0.0..=1.0).contains(&c.r));
            assert!((0.0..=1.0).contains(&c.a));
        }
    }
}