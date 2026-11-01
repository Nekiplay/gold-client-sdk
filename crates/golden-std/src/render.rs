//! Отрисовка: линии в мире, скелет, круг у головы, текст.
//!
//! [`Overlay`] создаётся один раз на кадр и знает камеру, FOV и размер экрана,
//! поэтому мировые точки проецируются без ручной возни с матрицами.
//!
//! ```no_run
//! # use golden_std::{Color, Overlay};
//! # use golden_sdk::{renderer::StudioRenderer, studio::StudioModel};
//! # fn example(renderer: &StudioRenderer, model: &StudioModel) {
//! let overlay = Overlay::new().unwrap();
//! overlay.skeleton(renderer, model, Color::GREEN, 1.5);
//! # }
//! ```

use golden_sdk::draw::{self, Gl};
use golden_sdk::math::{Camera, Fov, ScreenPos, Vec3};
use golden_sdk::renderer::StudioRenderer;
use golden_sdk::studio::StudioModel;

use crate::game::Game;
use std::sync::Mutex;

pub use golden_sdk::draw::Color;

/// Оформление обводки модели: цвет, толщина и видимость сквозь стены.
///
/// Цвет выбирается по модели, команде игрока или чему угодно ещё — достаточно
/// вернуть готовый стиль из [`Overlay::outline_of`] или передать его в
/// [`outline`]:
///
/// ```no_run
/// # use golden_std::{Color, OutlineStyle};
/// let style = OutlineStyle::new(Color::RED).width(2.0).through_walls(false);
/// assert_eq!(style.color(), Color::RED);
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutlineStyle {
    wire: draw::WireStyle,
}

impl OutlineStyle {
    /// Цветная обводка: 1.5 px, видна сквозь стены.
    pub fn new(color: Color) -> Self {
        Self {
            wire: draw::WireStyle::new(color),
        }
    }

    /// Толщина линий в пикселях (приводится к 1..=10).
    pub fn width(mut self, width: f32) -> Self {
        self.wire = self.wire.width(width);
        self
    }

    /// `false` — контур прячется за телом и стенами, как обычная геометрия.
    pub fn through_walls(mut self, through_walls: bool) -> Self {
        self.wire = self.wire.through_walls(through_walls);
        self
    }

    /// Цвет линий.
    pub fn color(&self) -> Color {
        self.wire.color
    }

    /// Толщина линий в пикселях.
    pub fn line_width(&self) -> f32 {
        self.wire.width
    }

    /// Виден ли контур сквозь стены и другие модели.
    pub fn is_through_walls(&self) -> bool {
        self.wire.through_walls
    }
}

impl Default for OutlineStyle {
    fn default() -> Self {
        Self::new(Color::WHITE)
    }
}

/// Обводит то, что нарисует `draw` (обычно вызов движка с текущей моделью).
///
/// Нужен в перехвате отрисовки, где модель рисует сам движок: обёртка меняет
/// режим полигонов и цвет на время вызова и возвращает GL-состояние назад.
/// `false`, если GL ещё не поднят.
pub fn outline(style: OutlineStyle, draw: impl FnOnce()) -> bool {
    // SAFETY: вызывается из перехвата отрисовки, где контекст уже активен.
    let Some(gl) = (unsafe { Gl::get() }) else {
        return false;
    };
    // SAFETY: `draw` — тот же вызов движка, что был бы и без обводки.
    unsafe {
        gl.wireframe_style(style.wire, draw);
    }
    true
}

/// Два прохода: контур сквозь стены и его видимая часть.
///
/// Первый проход рисует всё в цвете `hidden` без теста глубины — контур виден
/// сквозь мир и тела. Второй повторяет ту же отрисовку с тестом глубины в
/// цвете `visible`: проходят только пиксели, которые не закрыты стеной,
/// ящиком или другим телом. Закрытая часть остаётся в `hidden`, видимая
/// становится `visible`.
///
/// `draw` вызывается дважды, поэтому это должен быть повторяемый вызов
/// движковой отрисовки (`StudioDrawPlayer`), а не разовое действие.
///
/// `false`, если GL ещё не поднят.
pub fn outline_split(hidden: OutlineStyle, visible: OutlineStyle, draw: impl Fn()) -> bool {
    let first = outline(hidden, &draw);
    let second = outline(visible, &draw);
    first && second
}

/// Смещение `HUD_DrawNormalTriangles` в `client.dll` (см. `docs/STUDIO_REVERSE.md`).
///
/// Движок зовёт его в конце 3D-сцены, после мира и всех сущностей: здесь GL
/// свободен, и здесь же рисуются подписи, которые не должны уходить под
/// геометрию.
pub const DRAW_NORMAL_TRIANGLES_RVA: usize = 0xB16D0;

/// Ширина окна игры в пикселях.
const DEFAULT_FOV: f32 = 90.0;

/// Отрисовка поверх кадра.
///
/// Создавать и использовать только из хука отрисовки (`DrawModel`/`DrawPlayer`):
/// в этот момент GL-контекст активен и камера уже посчитана движком.
#[derive(Debug)]
pub struct Overlay {
    gl: &'static Gl,
    camera: Camera,
    fov: Fov,
    viewport: (f32, f32),
    text_scale: f32,
}

impl Overlay {
    /// Текущий кадр: камера, FOV и вьюпорт.
    ///
    /// `None`, если движок ещё не готов или GL не загрузился.
    pub fn new() -> Option<Self> {
        let gl = unsafe { Gl::get() }?;
        let (_, _, width, height) = unsafe { gl.viewport() }?;
        let view = Game::get()?.get_view()?;
        // FOV движок держит по соглашению 4:3 (`default_fov` 90), а ширину
        // расширяет под аспект окна.
        let fov = Fov::from_horizontal(DEFAULT_FOV, width / height);
        Some(Self {
            gl,
            camera: Camera::new(view.origin, view.forward, view.right, view.up),
            fov,
            viewport: (width, height),
            text_scale: 2.0,
        })
    }

    /// Размер экрана в пикселях.
    pub fn viewport(&self) -> (f32, f32) {
        self.viewport
    }

    /// Масштаб текста подписей (по умолчанию 2).
    pub fn text_scale(&mut self, scale: f32) {
        self.text_scale = scale.max(0.5);
    }

    /// Мировая точка → пиксели экрана; `None`, если точка за камерой.
    pub fn project(&self, world: Vec3) -> Option<ScreenPos> {
        self.camera.project(world, self.fov, self.viewport)
    }

    /// Отрезки в мировых координатах с проверкой глубины (прячутся за телом).
    pub fn lines(&self, segments: &[(Vec3, Vec3)], color: Color, width: f32) {
        unsafe { self.gl.lines(color, width, segments) }
    }

    /// То же, но видно сквозь модели и стены.
    pub fn xray(&self, segments: &[(Vec3, Vec3)], color: Color, width: f32) {
        unsafe { self.gl.lines_xray(color, width, segments) }
    }

    /// Перерисовывает модель контурами в цвете команды.
    ///
    /// `false`, если движок не дал слот или GL не поднят.
    pub fn outline(&self, renderer: &StudioRenderer, color: Color, width: f32) -> bool {
        self.outline_style(renderer, OutlineStyle::new(color).width(width))
    }

    /// Перерисовывает подготовленную движком модель контурами с заданным
    /// оформлением.
    ///
    /// `false`, если движок не дал слот или GL не поднят.
    pub fn outline_style(&self, renderer: &StudioRenderer, style: OutlineStyle) -> bool {
        renderer.render_wireframe_style(style.wire)
    }

    /// Обводит модель, выбирая оформление по ней самой — например, цвет по
    /// команде игрока или по имени модели:
    ///
    /// ```
    /// # use golden_std::{Color, OutlineStyle, Overlay};
    /// # use golden_sdk::renderer::StudioRenderer;
    /// # fn demo(overlay: &Overlay, renderer: &StudioRenderer) {
    /// overlay.outline_of(renderer, |model| {
    ///     if model.name().starts_with("terror") {
    ///         OutlineStyle::new(Color::RED)
    ///     } else {
    ///         OutlineStyle::new(Color::CYAN)
    ///     }
    /// });
    /// # }
    /// ```
    ///
    /// `false`, если у рендерера сейчас нет модели или слот недоступен.
    pub fn outline_of(
        &self,
        renderer: &StudioRenderer,
        style: impl FnOnce(&StudioModel) -> OutlineStyle,
    ) -> bool {
        let Some(model) = renderer.studio_model() else {
            return false;
        };
        self.outline_style(renderer, style(&model))
    }

    /// Коробка вокруг модели: 12 рёбер AABB, видимых сквозь стены.
    ///
    /// `min`/`max` — габариты в собственных координатах модели; они
    /// сдвигаются на `origin`, чтобы коробка встала ровно по телу.
    pub fn box_xray(&self, min: Vec3, max: Vec3, origin: Vec3, color: Color, width: f32) {
        let corners = [
            Vec3::new(min.x, min.y, min.z),
            Vec3::new(max.x, min.y, min.z),
            Vec3::new(min.x, max.y, min.z),
            Vec3::new(max.x, max.y, min.z),
            Vec3::new(min.x, min.y, max.z),
            Vec3::new(max.x, min.y, max.z),
            Vec3::new(min.x, max.y, max.z),
            Vec3::new(max.x, max.y, max.z),
        ]
        .map(|point| point + origin);
        const EDGES: [(usize, usize); 12] = [
            (0, 1),
            (0, 2),
            (1, 3),
            (2, 3),
            (4, 5),
            (4, 6),
            (5, 7),
            (6, 7),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];
        let segments: Vec<(Vec3, Vec3)> = EDGES
            .iter()
            .map(|(a, b)| (corners[*a], corners[*b]))
            .collect();
        self.xray(&segments, color, width);
    }

    /// Отрезки скелета в мировых координатах, видимые сквозь модель.
    pub fn skeleton_lines(&self, segments: &[(Vec3, Vec3)], color: Color, width: f32) {
        self.xray(segments, color, width);
    }

    /// Круг в мировой точке — билиборд, всегда лицом к камере.
    pub fn circle_at(&self, center: Vec3, radius: f32, color: Color, width: f32) {
        let Some(renderer) = StudioRenderer::global() else {
            return;
        };
        unsafe {
            self.gl.ring(
                center,
                renderer.view_right(),
                renderer.view_up(),
                radius,
                color,
                width,
                16,
            );
        }
    }

    /// Скелет модели — по костям, которые движок только что посчитал.
    pub fn skeleton(&self, renderer: &StudioRenderer, model: &StudioModel, color: Color, width: f32) {
        let segments = renderer.skeleton_lines_checked(model);
        self.xray(&segments, color, width);
    }

    /// Круг у головы: билиборд в мире, всегда лицом к камере.
    pub fn head_circle(
        &self,
        renderer: &StudioRenderer,
        model: &StudioModel,
        color: Color,
        radius: f32,
        width: f32,
    ) {
        let Some(center) = renderer.head_point(model) else {
            return;
        };
        unsafe {
            self.gl
                .ring(center, renderer.view_right(), renderer.view_up(), radius, color, width, 16);
        }
    }

    /// Текст в экранных пикселях — для проверки отрисовки подписей.
    pub fn text_at(&self, x: f32, y: f32, text: &str, color: Color, scale: f32) {
        unsafe { self.gl.text(x, y, text, color, scale) }
    }

    /// Подпись у мировой точки. `above` — над точкой, иначе под ней.
    pub fn label(&self, world: Vec3, text: &str, color: Color, above: bool) {
        let Some(pos) = self.project(world) else {
            return;
        };
        let scale = self.text_scale;
        let offset = if above {
            -(draw::text_height(scale) + 6.0)
        } else {
            8.0
        };
        let x = pos.x - draw::text_width(text, scale) / 2.0;
        unsafe { self.gl.text(x, pos.y + offset, text, color, scale) }
    }
}
/// Сколько подписей может копиться за кадр.
const DEFERRED_LIMIT: usize = 256;

/// Отложенная подпись: экранные координаты, посчитанные в момент отрисовки.
struct Deferred {
    x: f32,
    y: f32,
    text: String,
    color: Color,
    scale: f32,
}

static PENDING: Mutex<Vec<Deferred>> = Mutex::new(Vec::new());

/// Отложить подпись до конца кадра.
///
/// Проекция считается сразу (камера в этом кадре уже известна), а сам текст
/// рисуется в [`flush_labels`] перед сменой буферов. Иначе движок успевает
/// нарисовать поверх подписи часть геометрии — ящики и прозрачные поверхности
/// идут после моделей, и подпись оказывается под ними.
pub fn defer_label(world: Vec3, text: &str, color: Color, above: bool) {
    let Some(overlay) = Overlay::new() else {
        return;
    };
    let Some(pos) = overlay.project(world) else {
        return;
    };
    let scale = overlay.text_scale;
    let offset = if above {
        -(draw::text_height(scale) + 6.0)
    } else {
        8.0
    };
    let x = pos.x - draw::text_width(text, scale) / 2.0;
    if let Ok(mut queue) = PENDING.lock() {
        if queue.len() < DEFERRED_LIMIT {
            queue.push(Deferred {
                x,
                y: pos.y + offset,
                text: text.to_string(),
                color,
                scale,
            });
        }
    }
}

/// Нарисовать все отложенные подписи и очистить очередь.
///
/// Вызывать из потока с активным GL-контекстом, когда кадр уже отрисован.
pub fn flush_labels() {
    let items = match PENDING.lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };
    if items.is_empty() {
        return;
    }
    let Some(overlay) = Overlay::new() else {
        return;
    };
    for item in items {
        // SAFETY: вызывающий гарантирует активный GL-контекст.
        unsafe {
            overlay
                .gl
                .text(item.x, item.y, &item.text, item.color, item.scale)
        }
    }
}
