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
use alloc::boxed::Box;
use core::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::System::LibraryLoader::{
    GetModuleHandleA, GetProcAddress, LoadLibraryA,
};

use crate::math::Vec3;

/// `GL_LINES`.
pub const GL_LINES: u32 = 0x0001;
const GL_LINE_LOOP: u32 = 0x0002;
const GL_QUADS: u32 = 0x0007;
const GL_MODELVIEW: u32 = 0x1700;
const GL_PROJECTION: u32 = 0x1701;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_BLEND: u32 = 0x0BE2;
const GL_SRC_ALPHA: u32 = 0x0302;
const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_DEPTH_TEST: u32 = 0x0B71;
/// `GL_ALWAYS`: фрагмент проходит проверку глубины всегда.
const GL_ALWAYS: u32 = 0x0207;
/// `GL_DEPTH_BUFFER_BIT`: только буфер глубины.
const GL_DEPTH_BUFFER_BIT: u32 = 0x0000_0100;
const GL_ALL_ATTRIB_BITS: u32 = 0x000F_FFFF;
/// `GL_LIGHTING`: без этого `glColor4f` не влияет на геометрию — цвет пикселя
/// считает освещение, и контур выходит цветом материала движка, а не нашим.
const GL_LIGHTING: u32 = 0x0B50;
/// `GL_COLOR_MATERIAL`: пока lighting включён, движок скормил его материал
/// глобальному ambient, и `glColor4f` на геометрию не действует. Выключаем его
/// вместе с освещением — тогда цвет берётся из текущего цвета.
const GL_COLOR_MATERIAL: u32 = 0x0B57;
const GL_CULL_FACE: u32 = 0x0B44;
/// `GL_COLOR_ARRAY` — массив цветов вершин.
///
/// Если движок рисует модель через vertex arrays, цвет каждой вершины он
/// берёт из этого массива, и `glColor*` в этот момент не действует. Отключаем
/// массив на время своего прохода — тогда GL снова берёт текущий цвет.
const GL_COLOR_ARRAY: u32 = 0x8076;
/// `GL_VERTEX_ARRAY` - массив вершин: при включённом массиве `glVertex3f`
/// игнорируется, и нарисовать нечего.
const GL_VERTEX_ARRAY: u32 = 0x8074;
/// `GL_TEXTURE_COORD_ARRAY` - массив текстурных координат.
const GL_TEXTURE_COORD_ARRAY: u32 = 0x8078;
/// `glPolygonMode` — какие грани и как рисовать.
const GL_FRONT_AND_BACK: u32 = 0x0408;
const GL_LINE: u32 = 0x1B01;

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
/// Как рисовать контур модели: цвет, толщина и проверка глубины.
///
/// Отдельный тип нужен, чтобы вызывающий код мог описать обводку один раз
/// (например, по команде игрока) и передать её в [`Gl::wireframe_style`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WireStyle {
    pub color: Color,
    /// Толщина линий в пикселях, приводится к 1..=10.
    pub width: f32,
    /// `true` — контур виден сквозь стены и другие модели (глубина выключена).
    pub through_walls: bool,
}

impl WireStyle {
    /// Цветная обводка толщиной 1.5 px, видимая сквозь стены.
    pub const fn new(color: Color) -> Self {
        Self {
            color,
            width: 1.5,
            through_walls: true,
        }
    }

    pub const fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub const fn through_walls(mut self, through_walls: bool) -> Self {
        self.through_walls = through_walls;
        self
    }
}

impl Default for WireStyle {
    fn default() -> Self {
        Self::new(Color::WHITE)
    }
}

type Vertex3fFn = unsafe extern "system" fn(f32, f32, f32);
type Color4fFn = unsafe extern "system" fn(f32, f32, f32, f32);
type EnableFn = unsafe extern "system" fn(u32);
type LineWidthFn = unsafe extern "system" fn(f32);
type PushAttribFn = unsafe extern "system" fn(u32);
type PopAttribFn = unsafe extern "system" fn();
type BlendFuncFn = unsafe extern "system" fn(u32, u32);
type MatrixModeFn = unsafe extern "system" fn(u32);
type LoadIdentityFn = unsafe extern "system" fn();
type OrthoFn = unsafe extern "system" fn(f64, f64, f64, f64, f64, f64);
type PushMatrixFn = unsafe extern "system" fn();
type GetIntegervFn = unsafe extern "system" fn(u32, *mut i32);
type PolygonModeFn = unsafe extern "system" fn(u32, u32);
type IsEnabledFn = unsafe extern "system" fn(u32) -> u32;
/// `glDepthFunc(mode)`.
type DepthFuncFn = unsafe extern "system" fn(u32);
/// `glClear(mask)`.
type ClearFn = unsafe extern "system" fn(u32);
/// `glGetError` — код последней ошибки GL.
type GetErrorFn = unsafe extern "system" fn() -> u32;

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
    matrix_mode: MatrixModeFn,
    load_identity: LoadIdentityFn,
    ortho: OrthoFn,
    push_matrix: PushMatrixFn,
    pop_matrix: PushMatrixFn,
    get_integerv: GetIntegervFn,
    polygon_mode: PolygonModeFn,
    is_enabled: IsEnabledFn,
    get_error: GetErrorFn,
    disable_client_state: EnableFn,
    enable_client_state: EnableFn,
    depth_func: DepthFuncFn,
    clear: ClearFn,
}

/// Доступ к рисованию OpenGL.
#[derive(Debug)]
pub struct Gl {
    fns: Fns,
}

/// Разовый замер GL-состояния: печатается один раз при первом тексте.
pub mod probe {
    use core::sync::atomic::{AtomicUsize, Ordering};

    static LEFT: AtomicUsize = AtomicUsize::new(1);

    /// Нужен ли ещё замер (и забирает право на него).
    pub fn wanted() -> bool {
        LEFT.load(Ordering::Acquire) == 0
    }

    /// Единожды печатает состояние флагов GL.
    pub fn report(enabled: [u32; 6]) {
        if LEFT.swap(0, Ordering::AcqRel) == 0 {
            return;
        }
        crate::log!(
            "GL: lighting {} глубина {};
             смешивание {};
             текстура {}",
            enabled[0] as i32,
            enabled[1] as i32,
            enabled[2] as i32,
            enabled[3] as i32
        );
    }
}

static GL: AtomicUsize = AtomicUsize::new(0);

impl Gl {
    /// Читает интересующие флаги: `lighting`, `depth`, `blend`, `texture`, `cull`,
    /// `color material`.
    ///
    /// # Safety
    /// См. [`Gl::get`].
    unsafe fn probe_enabled(&self) -> [u32; 6] {
        const FLAGS: [u32; 6] = [
            GL_LIGHTING,
            GL_DEPTH_TEST,
            GL_BLEND,
            GL_TEXTURE_2D,
            GL_CULL_FACE,
            GL_COLOR_MATERIAL,
        ];
        let mut out = [0u32; 6];
        for (slot, flag) in FLAGS.iter().enumerate() {
            // SAFETY: `glIsEnabled` — стандартная точка входа GL 1.1.
            out[slot] = unsafe { (self.fns.is_enabled)(*flag) };
        }
        out
    }

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
                    matrix_mode: resolve(module, c"glMatrixMode")?,
                    load_identity: resolve(module, c"glLoadIdentity")?,
                    ortho: resolve(module, c"glOrtho")?,
                    push_matrix: resolve(module, c"glPushMatrix")?,
                    pop_matrix: resolve(module, c"glPopMatrix")?,
                    get_integerv: resolve(module, c"glGetIntegerv")?,
                    polygon_mode: resolve(module, c"glPolygonMode")?,
                    is_enabled: resolve(module, c"glIsEnabled")?,
                    get_error: resolve(module, c"glGetError")?,
        depth_func: resolve(module, c"glDepthFunc")?,
        clear: resolve(module, c"glClear")?,
                    disable_client_state: resolve(module, c"glDisableClientState")?,
                    enable_client_state: resolve(module, c"glEnableClientState")?,
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
            // Без этого `glColor4f` на геометрию не действует: пока lighting
            // включён, движок берёт материал, а он у него белый — линии
            // выходят белыми вместо цвета команды.
            (self.fns.disable)(GL_LIGHTING);
            (self.fns.disable)(GL_COLOR_MATERIAL);
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

    /// Контуры уже подготовленной модели: полигоны переводятся в линии, затем
    /// вызывается `render` — движок рисует ту же геометрию, что и тело.
    ///
    /// Своё состояние GL (режим полигонов, текстура, глубина, цвет, толщина
    /// линий) закрыто в `glPushAttrib`/`glPopAttrib`, поэтому после вызова
    /// движок продолжает рисовать как ни в чём не бывало.
    ///
    /// # Safety
    /// См. [`Gl::get`]; `render` должен рисовать только в текущем GL-контексте.
    pub unsafe fn wireframe(&self, color: Color, width: f32, render: impl FnOnce()) {
        // SAFETY: контракт передан вызывающим.
        unsafe {
            self.wireframe_style(WireStyle::new(color).width(width), render);
        }
    }

    /// То же, что [`Gl::wireframe`], но оформление задаётся [`WireStyle`].
    ///
    /// # Safety
    /// См. [`Gl::get`]; `render` должен рисовать только в текущем GL-контексте.
    pub unsafe fn wireframe_style(&self, style: WireStyle, render: impl FnOnce()) {
        // Движок задаёт цвет сам внутри рендера, поэтому одной `glColor4f`
        // мало: подменяем его цвет до тех пор, пока рисуется наш контур.
        crate::glcolor::install_once();
        let Color { r, g, b, a } = style.color;
        // SAFETY: контракт передан вызывающим.
        unsafe {
            (self.fns.push_attrib)(GL_ALL_ATTRIB_BITS);
            (self.fns.disable)(GL_TEXTURE_2D);
            (self.fns.disable)(GL_LIGHTING);
            (self.fns.disable)(GL_COLOR_MATERIAL);
            // Цвет вершины из массива перебивает и `glColor`, и подмену
            // таблицы — пока рисуем контур, массив цветов выключен. Клиентские
            // состояния `push_attrib` не сохраняет, поэтому возвращаем сами.
            let color_array = (self.fns.is_enabled)(GL_COLOR_ARRAY) != 0;
            if color_array {
                (self.fns.disable_client_state)(GL_COLOR_ARRAY);
            }
            if style.through_walls {
                // Движок включает проверку глубины сам, уже внутри своего
                // рендера, поэтому `glDisable` до вызова не помогает: он же
                // и перекрывает контур телом/ящиком. `GL_ALWAYS` переживает
                // это включение, а `pop_attrib` возвращает прежнюю функцию.
                (self.fns.disable)(GL_DEPTH_TEST);
                (self.fns.depth_func)(GL_ALWAYS);
            }
            (self.fns.enable)(GL_BLEND);
            (self.fns.blend_func)(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            (self.fns.polygon_mode)(GL_FRONT_AND_BACK, GL_LINE);
            (self.fns.line_width)(style.width.clamp(1.0, 10.0));
            (self.fns.color4f)(r, g, b, a);
            crate::glcolor::begin(style.color);
            render();
            crate::glcolor::end();
            if color_array {
                (self.fns.enable_client_state)(GL_COLOR_ARRAY);
            }
            (self.fns.pop_attrib)();
        }
    }

    /// Очистить только буфер глубины.
    ///
    /// Нужно для отрисовки поверх уже собранного кадра: цвет не трогается,
    /// поэтому картинка остаётся, а нарисованное после этого не отсекается
    /// глубиной, которую записали мир и ящики.
    ///
    /// # Safety
    /// См. [`Gl::get`].
    pub unsafe fn clear_depth(&self) {
        // SAFETY: контракт передан вызывающим.
        unsafe { (self.fns.clear)(GL_DEPTH_BUFFER_BIT) };
    }

    /// Размер вьюпорта `(x, y, width, height)` в пикселях.
    ///
    /// # Safety
    /// См. [`Gl::get`].
    pub unsafe fn viewport(&self) -> Option<(f32, f32, f32, f32)> {
        let mut vp = [0i32; 4];
        // SAFETY: контракт передан вызывающим; массив нужного размера.
        unsafe { (self.fns.get_integerv)(GL_VIEWPORT, vp.as_mut_ptr()) };
        if vp[2] <= 0 || vp[3] <= 0 {
            return None;
        }
        Some((vp[0] as f32, vp[1] as f32, vp[2] as f32, vp[3] as f32))
    }

    /// Круг в мире, повёрнутый к камере (билиборд).
    ///
    /// `right`/`up` — базис камеры из
    /// [`crate::engine::EngineStudioApi::view_info`], `radius` — в юнитах мира.
    ///
    /// # Safety
    /// См. [`Gl::get`].
    pub unsafe fn ring(
        &self,
        center: Vec3,
        right: Vec3,
        up: Vec3,
        radius: f32,
        color: Color,
        width: f32,
        segments: usize,
    ) {
        let segments = segments.max(6);
        // SAFETY: контракт передан вызывающим.
        unsafe {
            (self.fns.push_attrib)(GL_ALL_ATTRIB_BITS);
            (self.fns.disable)(GL_TEXTURE_2D);
            (self.fns.disable)(GL_LIGHTING);
            (self.fns.disable)(GL_COLOR_MATERIAL);
            (self.fns.disable)(GL_DEPTH_TEST);
            (self.fns.enable)(GL_BLEND);
            (self.fns.blend_func)(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            (self.fns.line_width)(width.clamp(1.0, 10.0));
            (self.fns.color4f)(color.r, color.g, color.b, color.a);
            (self.fns.begin)(GL_LINE_LOOP);
            for i in 0..segments {
                let angle = i as f32 / segments as f32 * core::f32::consts::TAU;
                let (sin, cos) = angle.sin_cos();
                let p = center + right * (cos * radius) + up * (sin * radius);
                (self.fns.vertex3f)(p.x, p.y, p.z);
            }
            (self.fns.end)();
            (self.fns.pop_attrib)();
        }
    }

    /// Текст в координатах экрана (пиксели, начало — левый верхний угол).
    ///
    /// Свой шрифт `5x7` — не зависит ни от шрифтов игры, ни от GDI: рисуем
    /// прямоугольниками в ортопроекции, состояние GL возвращаем как было.
    ///
    /// # Safety
    /// См. [`Gl::get`].
    pub unsafe fn text(&self, x: f32, y: f32, text: &str, color: Color, scale: f32) {
        let Some((vx, vy, vw, vh)) = (unsafe { self.viewport() }) else {
            return;
        };
        let scale = scale.max(0.5);
        let mut quads = 0u32;
        let before = unsafe { (self.fns.get_error)() };
        // SAFETY: контракт передан вызывающим.
        unsafe {
            (self.fns.push_attrib)(GL_ALL_ATTRIB_BITS);
            (self.fns.disable)(GL_TEXTURE_2D);
            (self.fns.disable)(GL_LIGHTING);
            (self.fns.disable)(GL_COLOR_MATERIAL);
            (self.fns.disable)(GL_DEPTH_TEST);
            (self.fns.enable)(GL_BLEND);
            (self.fns.blend_func)(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
            // Движок во время отрисовки моделей держит включёнными клиентские
            // массивы: при `GL_COLOR_ARRAY` цвет берётся из чужого массива, при
            // `GL_VERTEX_ARRAY` наши `glVertex3f` вообще игнорируются. Без этого
            // подписи не видны, хотя контур (он массивы выключает) рисуется.
            (self.fns.disable_client_state)(GL_COLOR_ARRAY);
            (self.fns.disable_client_state)(GL_VERTEX_ARRAY);
            (self.fns.disable_client_state)(GL_TEXTURE_COORD_ARRAY);

            (self.fns.matrix_mode)(GL_PROJECTION);
            (self.fns.push_matrix)();
            (self.fns.load_identity)();
            // Ось Y вниз — так координаты текста совпадают с пикселями экрана.
            // Глифы при этом выводятся в перевёрнутом порядке строк: движок
            // держит проекцию с вертикальным отражением, и без этого текст
            // выходит вверх ногами (см. `py` ниже).
            (self.fns.ortho)(0.0, vw as f64, vh as f64, 0.0, -1.0, 1.0);
            (self.fns.matrix_mode)(GL_MODELVIEW);
            (self.fns.push_matrix)();
            (self.fns.load_identity)();
            if crate::draw::probe::wanted() {
                // SAFETY: состояние читается в том же контексте, что и рисование.
                let enabled = self.probe_enabled();
                crate::draw::probe::report(enabled);
            }

            (self.fns.color4f)(color.r, color.g, color.b, color.a);
            (self.fns.begin)(GL_QUADS);
            for (index, ch) in text.chars().enumerate() {
                let glyph = font::glyph(ch);
                let origin_x = x + (vx + index as f32 * font::ADVANCE * scale);
                for (row, bits) in glyph.iter().enumerate() {
                    for col in 0..font::WIDTH {
                        // Старший бит строки — левая колонка глифа (шрифт 5x7,
                        // `'L'` = `0b10000` в верхних строках), поэтому колонку
                        // берём с конца: иначе текст выходит зеркальным.
                        if bits & (1 << (font::WIDTH - 1 - col)) == 0 {
                            continue;
                        }
                        let px = origin_x + col as f32 * scale;
                        // Строки глифа выводятся снизу вверх: проекция движка
                        // здесь вертикально отражена (см. `ortho` выше).
                        let py = y + vy + row as f32 * scale;
                        (self.fns.vertex3f)(px, py, 0.0);
                        (self.fns.vertex3f)(px + scale, py, 0.0);
                        quads += 1;
                        (self.fns.vertex3f)(px + scale, py + scale, 0.0);
                        (self.fns.vertex3f)(px, py + scale, 0.0);
                    }
                }
            }
            (self.fns.end)();
            let after = (self.fns.get_error)();
            {
                static TEXT_DRAWS: core::sync::atomic::AtomicU32 =
                    core::sync::atomic::AtomicU32::new(0);
                if TEXT_DRAWS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 3 {
                    crate::log!(
                        "текст: вьюпорт {}x{} в точке ({}, {}), масштаб {:.2}, квадов {}, GL {} -> {}",
                        vw,
                        vh,
                        vx,
                        vy,
                        scale,
                        quads,
                        before,
                        after
                    );
                }
            }

            (self.fns.matrix_mode)(GL_MODELVIEW);
            (self.fns.pop_matrix)();
            (self.fns.matrix_mode)(GL_PROJECTION);
            (self.fns.pop_matrix)();
            (self.fns.matrix_mode)(GL_MODELVIEW);
            (self.fns.pop_attrib)();
        }
    }
}

/// Ширина текста в пикселях при заданном масштабе.
pub fn text_width(text: &str, scale: f32) -> f32 {
    text.chars().count() as f32 * font::ADVANCE * scale
}

/// Высота строки в пикселях при заданном масштабе.
pub fn text_height(scale: f32) -> f32 {
    font::HEIGHT as f32 * scale
}

/// Встроенный шрифт `5x7`: только то, что нужно для подписей в игре.
///
/// Свой шрифт вместо движкового выбран осознанно: не надо тянуть
/// `cl_enginefunc_t` ради одной строки текста, и разметка не зависит от
/// настроек шрифтов у пользователя.
pub mod font {
    /// Ширина глифа в точках.
    pub const WIDTH: usize = 5;
    /// Высота глифа в точках.
    pub const HEIGHT: usize = 7;
    /// Шаг между глифами в точках (ширина + пробел).
    pub const ADVANCE: f32 = 6.0;

    /// Строки глифа: бит 0 — левая колонка, бит 4 — правая, бит 6 — нижняя строка.
    pub fn glyph(c: char) -> [u8; HEIGHT] {
        match c.to_ascii_uppercase() {
            'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
            'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
            'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
            'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
            'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
            'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
            'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
            'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
            'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
            'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
            'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
            'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
            'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
            'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
            'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
            'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
            'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
            'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
            'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
            'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
            'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
            'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
            'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
            'X' => [0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b01010, 0b10001],
            'Y' => [0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
            'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
            '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
            '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
            '2' => [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111],
            '3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
            '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
            '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
            '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
            '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
            '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
            '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
            '_' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111],
            '-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
            '.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00110],
            '/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
            ':' => [0b00000, 0b00110, 0b00110, 0b00000, 0b00110, 0b00110, 0b00000],
            _ => [0b00000; HEIGHT],
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn known_glyphs_have_pixels() {
            for ch in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-./:".chars() {
                let rows = glyph(ch);
                assert!(rows.iter().any(|r| *r != 0), "пустой глиф для {ch:?}");
                assert!(rows.iter().all(|r| *r < 32), "глиф {ch:?} шире 5 точек");
            }
            assert_eq!(glyph(' '), [0; HEIGHT]);
            assert_eq!(glyph('a'), glyph('A'), "строчные приводятся к заглавным");
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