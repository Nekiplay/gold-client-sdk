//! Геометрия карты: разбор `.bsp` и трассировка луча.
//!
//! Мир GoldSrc — это BSP30: плоскости, узлы (дерево BSP), листы, грани и
//! текстуры. Движок уже держит всё это в памяти, но своего трассировщика для
//! клиента он не отдаёт: `PM_PlayerTrace`/`SV_Move` требуют состояния,
//! которого у клиента нет. Поэтому трейс считаем сами по файлу карты.
//!
//! Что здесь есть:
//!
//! * [`Bsp::parse`] — разбор файла карты (версия 30, без сетки и лайтмапов);
//! * [`Bsp::trace`] — луч по миру: та же рекурсия по узлам, что в движке
//!   (`Hull_RecursiveHullCheck` для оболочки точки);
//! * [`Bsp::trace_brush`] — то же по brush-сущности (двери, ящики): модель
//!   берётся из того же файла, сдвиг — из `origin` сущности;
//! * [`Bsp::texture_at`] — имя текстуры под точкой удара, из него считается
//!   материал (см. [`crate::ballistics`]).
//!
//! ```no_run
//! # fn example(bytes: &[u8]) -> Option<()> {
//! let bsp = golden_sdk::bsp::Bsp::parse(bytes).ok()?;
//! let hit = bsp.trace(golden_sdk::Vec3::ZERO, golden_sdk::Vec3::new(1024.0, 0.0, 0.0));
//! if hit.fraction < 1.0 {
//!     println!("стена на {:?}: {:?}", hit.end, bsp.texture_at(hit.end, hit.normal));
//! }
//! # Some(())
//! # }
//! ```
//!
//! ## Границы
//!
//! Разбирается только геометрия мира (плоскости, узлы, листы, грани,
//! текстуры) и таблица моделей с текстом сущностей. Сетка вершин для
//! отрисовки, лайтмапы и видимость не нужны и не читаются. Карты в `.pak`
//! так не открыть: [`Bsp::parse`] работает с уже прочитанными байтами.

use alloc::string::String;
use alloc::vec::Vec;

use crate::math::Vec3;

/// Версия карты, которую понимает GoldSrc.
pub const VERSION: i32 = 30;

/// Содержимое листа: сплошное.
pub const CONTENTS_SOLID: i32 = -2;
/// Содержимое листа: пусто.
pub const CONTENTS_EMPTY: i32 = -1;
/// Содержимое листа: вода.
pub const CONTENTS_WATER: i32 = -3;
/// Содержимое листа: слизь.
pub const CONTENTS_SLIME: i32 = -4;
/// Содержимое листа: лава.
pub const CONTENTS_LAVA: i32 = -5;
/// Содержимое листа: небо (луч в него не идёт).
pub const CONTENTS_SKY: i32 = -6;

/// Сдвиг точки удара с поверхности, как в движке: `DIST_EPSILON`.
const DIST_EPSILON: f32 = 0.03125;

/// Предел глубины рекурсии по дереву: защита от битого файла.
const MAX_DEPTH: u32 = 512;

/// Предел шагов обхода на один луч.
///
/// Обход дерева по одному лучу должен укладываться в сотни шагов; запас нужен
/// на случай вырожденной геометрии, когда обе ветки узла сплошные и рекурсия
/// расходится. Мод живёт в рендер-потоке: лучше считать луч непробитым, чем
/// заморозить игру.
const STEP_BUDGET: u32 = 50_000;

/// Номера lump'ов BSP30.
mod lump {
    pub const PLANES: usize = 1;
    pub const TEXTURES: usize = 2;
    pub const VERTICES: usize = 3;
    pub const NODES: usize = 5;
    pub const TEXINFO: usize = 6;
    pub const FACES: usize = 7;
    pub const LEAFS: usize = 10;
    pub const MARKSURFACES: usize = 11;
    pub const EDGES: usize = 12;
    pub const SURFEDGES: usize = 13;
    pub const MODELS: usize = 14;
    pub const ENTITIES: usize = 0;
}

/// Почему файл не разобрался.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BspError {
    /// Файл короче заголовка.
    Truncated,
    /// Версия не 30: это не карта GoldSrc.
    Version(i32),
    /// Lump выходит за пределы файла.
    LumpOutOfRange(usize),
    /// Структура внутри lump'а не влезла в него.
    Structure(usize),
    /// В файле нет модели мира.
    NoWorldModel,
}

impl core::fmt::Display for BspError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => f.write_str("файл карты короче заголовка"),
            Self::Version(version) => write!(f, "версия карты {version}, а нужна {VERSION}"),
            Self::LumpOutOfRange(index) => write!(f, "lump {index} выходит за файл"),
            Self::Structure(index) => write!(f, "структура в lump {index} не влезла"),
            Self::NoWorldModel => f.write_str("в карте нет модели мира"),
        }
    }
}

/// Плоскость: `normal · p = dist`.
///
/// Осевые плоскости (`kind < 3`) не считаются отдельно: у нормали может быть
/// любой знак, и сокращение «взять компоненту вектора» тогда врёт. Скалярное
/// произведение даёт верный знак всегда.
#[derive(Debug, Clone, Copy)]
pub struct Plane {
    /// Нормаль.
    pub normal: Vec3,
    /// Расстояние от начала координат.
    pub dist: f32,
}

/// Узел дерева BSP.
#[derive(Debug, Clone, Copy)]
pub struct Node {
    /// Номер плоскости-разделителя.
    pub plane: u16,
    /// Дети: `>= 0` — узел, `< 0` — лист `-(child + 1)`.
    pub children: [i32; 2],
}

/// Лист: содержимое и грани, которые в нём видны.
#[derive(Debug, Clone, Copy)]
pub struct Leaf {
    /// `CONTENTS_*`.
    pub contents: i32,
    /// Первая грань в `marksurfaces`.
    pub first_mark_surface: u16,
    /// Сколько граней.
    pub mark_surfaces: u16,
}

/// Грань мира.
#[derive(Debug, Clone, Copy)]
pub struct Face {
    /// Номер плоскости.
    pub plane: u16,
    /// Сторона плоскости: 0 — лицевая, 1 — обратная.
    pub side: i16,
    /// Первое ребро в `surfedges`.
    pub first_edge: i32,
    /// Сколько рёбер.
    pub edges: i16,
    /// Номер `texinfo`.
    pub texinfo: i16,
}

/// Модель карты: мир (`0`) или brush-сущность (`1..`).
#[derive(Debug, Clone, Copy)]
pub struct Model {
    /// Корни деревьев оболочек (`headnode[0]` — оболочка точки).
    pub head_node: [i32; 4],
    /// Первая грань модели.
    pub first_face: i32,
    /// Сколько граней.
    pub faces: i32,
}

/// Brush-сущность: модель карты и её сдвиг в мире.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Brush {
    /// Номер модели в таблице карты.
    pub model: usize,
    /// `origin` сущности.
    pub origin: Vec3,
}

/// Результат трассировки луча.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trace {
    /// Доля пройденного пути: `1.0` — ни во что не попали.
    pub fraction: f32,
    /// Точка удара (или конец луча, если не попали).
    pub end: Vec3,
    /// Нормаль поверхности удара, смотрит в сторону луча.
    pub normal: Vec3,
    /// Луч начался внутри сплошного.
    pub startsolid: bool,
    /// Луч целиком внутри сплошного.
    pub allsolid: bool,
    /// Модель, в которую попали: `0` — мир, иначе номер brush-модели.
    pub model: usize,
}

impl Trace {
    /// Луч ни во что не попал.
    fn clear(end: Vec3) -> Self {
        Self {
            fraction: 1.0,
            end,
            normal: Vec3::ZERO,
            startsolid: false,
            allsolid: false,
            model: 0,
        }
    }

    /// Попали в поверхность.
    pub fn hit(&self) -> bool {
        self.fraction < 1.0
    }
}

/// Карта: геометрия, текстуры и модели.
#[derive(Debug)]
pub struct Bsp {
    planes: Vec<Plane>,
    nodes: Vec<Node>,
    leafs: Vec<Leaf>,
    faces: Vec<Face>,
    /// `texinfo[i]` — номер текстуры.
    texinfo: Vec<i32>,
    textures: Vec<String>,
    vertices: Vec<Vec3>,
    edges: Vec<[u16; 2]>,
    surfedges: Vec<i32>,
    marksurfaces: Vec<u16>,
    models: Vec<Model>,
    brushes: Vec<Brush>,
    /// Локальные AABB моделей: `bounds[model] = (mins, maxs)`.
    bounds: Vec<(Vec3, Vec3)>,
}

impl Bsp {
    /// Разбирает файл карты.
    ///
    /// # Errors
    /// [`BspError`], если файл не карта GoldSrc или обрезан.
    pub fn parse(bytes: &[u8]) -> Result<Self, BspError> {
        let version = read_i32(bytes, 0).ok_or(BspError::Truncated)?;
        if version != VERSION {
            return Err(BspError::Version(version));
        }
        let mut lumps = [(0usize, 0usize); 16];
        for (index, slot) in lumps.iter_mut().enumerate() {
            let offset = 4 + index * 8;
            let start = read_i32(bytes, offset).ok_or(BspError::Truncated)?;
            let length = read_i32(bytes, offset + 4).ok_or(BspError::Truncated)?;
            if start < 0 || length < 0 {
                return Err(BspError::LumpOutOfRange(index));
            }
            let start = start as usize;
            let length = length as usize;
            // Пустой lump (например лайтмапы, которых в этой сборке нет) может
            // лежать с нулевым смещением и мусорной длиной.
            if start == 0 {
                *slot = (0, 0);
                continue;
            }
            if start.saturating_add(length) > bytes.len() {
                return Err(BspError::LumpOutOfRange(index));
            }
            *slot = (start, length);
        }
        let cut = |index: usize| {
            let (start, length) = lumps[index];
            &bytes[start..start + length]
        };

        let planes = cut(lump::PLANES)
            .chunks_exact(20)
            .map(|raw| Plane {
                normal: Vec3::new(
                    read_f32(raw, 0).unwrap_or_default(),
                    read_f32(raw, 4).unwrap_or_default(),
                    read_f32(raw, 8).unwrap_or_default(),
                ),
                dist: read_f32(raw, 12).unwrap_or_default(),
            })
            .collect();

        let nodes = cut(lump::NODES)
            .chunks_exact(24)
            .map(|raw| Node {
                plane: read_u16(raw, 0).unwrap_or_default(),
                children: [
                    read_i16(raw, 4).map_or(0, i32::from),
                    read_i16(raw, 6).map_or(0, i32::from),
                ],
            })
            .collect();

        let leafs = cut(lump::LEAFS)
            .chunks_exact(28)
            .map(|raw| Leaf {
                contents: read_i32(raw, 0).unwrap_or(CONTENTS_EMPTY),
                first_mark_surface: read_u16(raw, 20).unwrap_or_default(),
                mark_surfaces: read_u16(raw, 22).unwrap_or_default(),
            })
            .collect();

        let faces: Vec<Face> = cut(lump::FACES)
            .chunks_exact(20)
            .map(|raw| Face {
                plane: read_u16(raw, 0).unwrap_or_default(),
                side: read_i16(raw, 2).unwrap_or_default(),
                first_edge: read_i32(raw, 4).unwrap_or_default(),
                edges: read_i16(raw, 8).unwrap_or_default(),
                texinfo: read_i16(raw, 10).unwrap_or_default(),
            })
            .collect();

        let texinfo = cut(lump::TEXINFO)
            .chunks_exact(40)
            .map(|raw| read_i32(raw, 32).unwrap_or(-1))
            .collect();

        let textures = parse_textures(cut(lump::TEXTURES));

        let vertices: Vec<Vec3> = cut(lump::VERTICES)
            .chunks_exact(12)
            .map(|raw| {
                Vec3::new(
                    read_f32(raw, 0).unwrap_or_default(),
                    read_f32(raw, 4).unwrap_or_default(),
                    read_f32(raw, 8).unwrap_or_default(),
                )
            })
            .collect();

        let edges: Vec<[u16; 2]> = cut(lump::EDGES)
            .chunks_exact(4)
            .map(|raw| {
                [
                    read_u16(raw, 0).unwrap_or_default(),
                    read_u16(raw, 2).unwrap_or_default(),
                ]
            })
            .collect();

        let surfedges: Vec<i32> = cut(lump::SURFEDGES)
            .chunks_exact(4)
            .map(|raw| read_i32(raw, 0).unwrap_or_default())
            .collect();

        let marksurfaces = cut(lump::MARKSURFACES)
            .chunks_exact(2)
            .map(|raw| read_u16(raw, 0).unwrap_or_default())
            .collect();

        let models: Vec<Model> = cut(lump::MODELS)
            .chunks_exact(64)
            .map(|raw| Model {
                head_node: [
                    read_i32(raw, 36).unwrap_or_default(),
                    read_i32(raw, 40).unwrap_or_default(),
                    read_i32(raw, 44).unwrap_or_default(),
                    read_i32(raw, 48).unwrap_or_default(),
                ],
                first_face: read_i32(raw, 56).unwrap_or_default(),
                faces: read_i32(raw, 60).unwrap_or_default(),
            })
            .collect();
        if models.is_empty() {
            return Err(BspError::NoWorldModel);
        }

        let brushes = parse_brushes(cut(lump::ENTITIES), models.len());
        let bounds = model_bounds(&models, &faces, &edges, &surfedges, &vertices);

        Ok(Self {
            planes,
            nodes,
            leafs,
            faces,
            texinfo,
            textures,
            vertices,
            edges,
            surfedges,
            marksurfaces,
            models,
            brushes,
            bounds,
        })
    }

    /// Модели карты (индекс `0` — мир).
    pub fn models(&self) -> &[Model] {
        &self.models
    }

    /// Brush-сущности с их сдвигами.
    pub fn brushes(&self) -> &[Brush] {
        &self.brushes
    }

    /// Локальные границы модели `(mins, maxs)`; `None`, если модель пустая.
    pub fn model_bounds(&self, model: usize) -> Option<(Vec3, Vec3)> {
        self.bounds.get(model).copied()
    }

    /// Имена текстур карты.
    pub fn textures(&self) -> &[String] {
        &self.textures
    }

    /// Содержимое точки в мире.
    pub fn point_contents(&self, point: Vec3) -> i32 {
        self.contents_in(0, self.models[0].head_node[0], point)
    }

    /// Луч по миру: `fraction` — доля пути до поверхности.
    pub fn trace(&self, start: Vec3, end: Vec3) -> Trace {
        self.trace_model(0, start, end)
    }

    /// Луч по модели карты (в координатах мира, `origin` модели не учитывается).
    pub fn trace_model(&self, model: usize, start: Vec3, end: Vec3) -> Trace {
        let Some(world) = self.models.get(model) else {
            return Trace::clear(end);
        };
        self.trace_in(model, world.head_node[0], start, end)
    }

    /// Луч по brush-сущности: модель сдвинута на `origin`.
    pub fn trace_brush(&self, brush: &Brush, start: Vec3, end: Vec3) -> Trace {
        let Some(model) = self.models.get(brush.model) else {
            return Trace::clear(end);
        };
        let local_start = start - brush.origin;
        let local_end = end - brush.origin;
        // Дешёвое отсечение: если отрезок не задевает AABB модели, обход её
        // BSP-дерева не нужен. Без этого LOS-проверка флешки на каждого игрока
        // перебирает все brush-модели карты и роняет FPS.
        if let Some((mins, maxs)) = self.model_bounds(brush.model) {
            if !segment_intersects_aabb(local_start, local_end, mins, maxs) {
                return Trace::clear(end);
            }
        }
        let mut trace = self.trace_in(brush.model, model.head_node[0], local_start, local_end);
        trace.end = trace.end + brush.origin;
        trace
    }

    /// Ближайший удар по миру и brush-сущностям.
    pub fn trace_all(&self, start: Vec3, end: Vec3) -> Trace {
        let mut best = self.trace(start, end);
        for brush in &self.brushes {
            let trace = self.trace_brush(brush, start, end);
            if trace.fraction < best.fraction {
                best = trace;
            }
        }
        best
    }

    /// Имя текстуры под точкой удара с нормалью `normal`.
    ///
    /// Ищет грань в листе, в которой лежит точка: сначала по плоскости, затем по
    /// контуру грани. Без совпадения берёт любую грань с той же нормалью, а если
    /// и такой нет — `None`.
    pub fn texture_at(&self, point: Vec3, normal: Vec3) -> Option<&str> {
        let leaf = self.leaf_at(0, point)?;
        self.texture_in_leaf(leaf, point, normal)
    }

    /// Имя текстуры brush-модели под точкой удара (точка — в мировых координатах).
    pub fn texture_at_brush(&self, brush: &Brush, point: Vec3, normal: Vec3) -> Option<&str> {
        let local = point - brush.origin;
        let leaf = self.leaf_at(brush.model, local)?;
        self.texture_in_leaf(leaf, local, normal)
    }

    /// Brush-модель по номеру модели карты.
    pub fn brush_of_model(&self, model: usize) -> Option<&Brush> {
        self.brushes.iter().find(|brush| brush.model == model)
    }

    /// Текстура грани листа, ближайшей к точке.
    fn texture_in_leaf(&self, leaf: Leaf, point: Vec3, normal: Vec3) -> Option<&str> {
        let first = leaf.first_mark_surface as usize;
        let count = leaf.mark_surfaces as usize;
        let mut same_plane = None;
        for index in self.marksurfaces.iter().skip(first).take(count).copied() {
            let face = self.faces.get(index as usize)?;
            let plane = self.planes.get(face.plane as usize)?;
            let face_normal = if face.side == 0 {
                plane.normal
            } else {
                plane.normal * -1.0
            };
            if face_normal.dot(normal) < 0.99 {
                continue;
            }
            let name = self.face_texture(face);
            if self.face_contains(face, point) {
                return name;
            }
            if same_plane.is_none() {
                same_plane = name;
            }
        }
        same_plane
    }

    /// Текстура грани по её контуру.
    fn face_texture(&self, face: &Face) -> Option<&str> {
        let index = *self.texinfo.get(usize::try_from(face.texinfo).ok()?)?;
        let index = usize::try_from(index).ok()?;
        self.textures.get(index).map(String::as_str)
    }

    /// Лежит ли точка внутри грани (грань выпуклая, обход по контуру).
    fn face_contains(&self, face: &Face, point: Vec3) -> bool {
        let normal = self.face_normal(face);
        if normal.dot(normal) == 0.0 {
            return false;
        }
        let mut points = Vec::new();
        if !self.face_points(face, &mut points) || points.len() < 3 {
            return false;
        }
        for index in 0..points.len() {
            let a = points[index];
            let b = points[(index + 1) % points.len()];
            let edge = b - a;
            let to_point = point - a;
            let cross = Vec3::new(
                edge.y * to_point.z - edge.z * to_point.y,
                edge.z * to_point.x - edge.x * to_point.z,
                edge.x * to_point.y - edge.y * to_point.x,
            );
            if cross.dot(normal) < -0.1 {
                return false;
            }
        }
        true
    }

    /// Нормаль грани в мире.
    fn face_normal(&self, face: &Face) -> Vec3 {
        let Some(plane) = self.planes.get(face.plane as usize) else {
            return Vec3::ZERO;
        };
        if face.side == 0 {
            plane.normal
        } else {
            plane.normal * -1.0
        }
    }

    /// Вершины грани в порядке обхода.
    fn face_points(&self, face: &Face, out: &mut Vec<Vec3>) -> bool {
        let Ok(first) = usize::try_from(face.first_edge) else {
            return false;
        };
        let count = face.edges.max(0) as usize;
        for step in 0..count {
            let Some(&surfedge) = self.surfedges.get(first + step) else {
                return false;
            };
            let (index, flip) = if surfedge >= 0 {
                (surfedge as usize, false)
            } else {
                ((-surfedge) as usize, true)
            };
            let Some(edge) = self.edges.get(index) else {
                return false;
            };
            let vertex = if flip { edge[1] } else { edge[0] };
            let Some(&point) = self.vertices.get(vertex as usize) else {
                return false;
            };
            out.push(point);
        }
        true
    }

    /// Лист, в котором лежит точка (обход от узла `head`).
    fn leaf_at(&self, model: usize, point: Vec3) -> Option<Leaf> {
        let head = self.models.get(model)?.head_node[0];
        let mut node = head;
        for _ in 0..MAX_DEPTH {
            if node < 0 {
                return self.leafs.get((-node - 1) as usize).copied();
            }
            let entry = self.nodes.get(node as usize)?;
            let plane = self.planes.get(entry.plane as usize)?;
            let distance = plane.normal.dot(point) - plane.dist;
            node = entry.children[if distance >= 0.0 { 0 } else { 1 }];
        }
        None
    }

    /// Содержимое точки, начиная с узла `node`.
    fn contents_in(&self, _model: usize, node: i32, point: Vec3) -> i32 {
        let mut node = node;
        for _ in 0..MAX_DEPTH {
            if node < 0 {
                return self
                    .leafs
                    .get((-node - 1) as usize)
                    .map_or(CONTENTS_EMPTY, |leaf| leaf.contents);
            }
            let Some(entry) = self.nodes.get(node as usize) else {
                return CONTENTS_EMPTY;
            };
            let Some(plane) = self.planes.get(entry.plane as usize) else {
                return CONTENTS_EMPTY;
            };
            let distance = plane.normal.dot(point) - plane.dist;
            node = entry.children[if distance >= 0.0 { 0 } else { 1 }];
        }
        CONTENTS_EMPTY
    }

    /// Трассировка внутри модели: `Hull_RecursiveHullCheck` для точки.
    fn trace_in(&self, model: usize, head: i32, start: Vec3, end: Vec3) -> Trace {
        let mut trace = Trace::clear(end);
        trace.model = model;
        trace.allsolid = true;
        let mut steps = STEP_BUDGET;
        let _ = self.recursive(model, head, 0.0, 1.0, start, end, &mut trace, 0, &mut steps);
        trace
    }

    /// Один шаг рекурсии по дереву. `false` — удар найден, дальше не идём.
    #[allow(clippy::too_many_arguments)]
    fn recursive(
        &self,
        model: usize,
        num: i32,
        p1f: f32,
        p2f: f32,
        p1: Vec3,
        p2: Vec3,
        trace: &mut Trace,
        depth: u32,
        steps: &mut u32,
    ) -> bool {
        if depth > MAX_DEPTH || *steps == 0 {
            return true;
        }
        *steps -= 1;
        if num < 0 {
            let contents = self
                .leafs
                .get((-num - 1) as usize)
                .map_or(CONTENTS_EMPTY, |leaf| leaf.contents);
            if contents != CONTENTS_SOLID {
                trace.allsolid = false;
            } else {
                trace.startsolid = true;
            }
            return true;
        }
        let Some(node) = self.nodes.get(num as usize) else {
            return true;
        };
        let Some(plane) = self.planes.get(node.plane as usize) else {
            return true;
        };
        let (t1, t2) = (
            plane.normal.dot(p1) - plane.dist,
            plane.normal.dot(p2) - plane.dist,
        );
        if t1 >= 0.0 && t2 >= 0.0 {
            return self.recursive(model, node.children[0], p1f, p2f, p1, p2, trace, depth + 1, steps);
        }
        if t1 < 0.0 && t2 < 0.0 {
            return self.recursive(model, node.children[1], p1f, p2f, p1, p2, trace, depth + 1, steps);
        }

        // Точку пересечения отодвигаем на DIST_EPSILON в сторону начала луча.
        let mut frac = if t1 < 0.0 {
            (t1 + DIST_EPSILON) / (t1 - t2)
        } else {
            (t1 - DIST_EPSILON) / (t1 - t2)
        };
        if frac < 0.0 {
            frac = 0.0;
        }
        if frac > 1.0 {
            frac = 1.0;
        }
        let midf = p1f + (p2f - p1f) * frac;
        let mid = p1.lerp(p2, frac);
        let side = usize::from(t1 < 0.0);

        if !self.recursive(
            model,
            node.children[side],
            p1f,
            midf,
            p1,
            mid,
            trace,
            depth + 1,
            steps,
        ) {
            return false;
        }
        // За узлом не сплошное — идём дальше.
        if self.contents_in(model, node.children[side ^ 1], mid) != CONTENTS_SOLID {
            return self.recursive(
                model,
                node.children[side ^ 1],
                midf,
                p2f,
                mid,
                p2,
                trace,
                depth + 1,
                steps,
            );
        }
        if trace.allsolid {
            return false;
        }
        // Удар о плоскость этого узла.
        trace.normal = if side == 0 {
            plane.normal
        } else {
            plane.normal * -1.0
        };
        let mut back_frac = frac;
        let mut back_midf = midf;
        let mut back_mid = mid;
        for _ in 0..64 {
            let head = self.models.get(model).map_or(0, |entry| entry.head_node[0]);
            if self.contents_in(model, head, back_mid) != CONTENTS_SOLID {
                break;
            }
            back_frac -= 0.1;
            if back_frac < 0.0 {
                trace.fraction = back_midf;
                trace.end = back_mid;
                return false;
            }
            back_midf = p1f + (p2f - p1f) * back_frac;
            back_mid = p1.lerp(p2, back_frac);
        }
        trace.fraction = back_midf;
        trace.end = back_mid;
        false
    }
}

/// Имена текстур из lump'а текстур.
/// Считает локальные AABB всех моделей карты.
///
/// Нужно для отсечения `trace_brush`: у brush-модели нет готовых границ, а
/// обходить её BSP-дерево на каждый луч накладно.
fn model_bounds(
    models: &[Model],
    faces: &[Face],
    edges: &[[u16; 2]],
    surfedges: &[i32],
    vertices: &[Vec3],
) -> Vec<(Vec3, Vec3)> {
    let mut out = Vec::with_capacity(models.len());
    for model in models {
        let mut mins = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
        let mut maxs = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
        let start = model.first_face.max(0) as usize;
        let end = start.saturating_add(model.faces.max(0) as usize);
        for face in faces.get(start..end).unwrap_or(&[]) {
            let first = face.first_edge.max(0) as usize;
            let count = face.edges.max(0) as usize;
            for &surfedge in surfedges.get(first..first.saturating_add(count)).unwrap_or(&[]) {
                let Some(edge) = edges.get(surfedge.unsigned_abs() as usize) else {
                    continue;
                };
                let index = if surfedge >= 0 { edge[0] } else { edge[1] };
                let Some(vertex) = vertices.get(index as usize) else {
                    continue;
                };
                mins.x = mins.x.min(vertex.x);
                mins.y = mins.y.min(vertex.y);
                mins.z = mins.z.min(vertex.z);
                maxs.x = maxs.x.max(vertex.x);
                maxs.y = maxs.y.max(vertex.y);
                maxs.z = maxs.z.max(vertex.z);
            }
        }
        if mins.x.is_finite() {
            // Небольшой запас: точка на самой грани не должна отсекаться.
            out.push((
                mins - Vec3::new(1.0, 1.0, 1.0),
                maxs + Vec3::new(1.0, 1.0, 1.0),
            ));
        } else {
            out.push((Vec3::ZERO, Vec3::ZERO));
        }
    }
    out
}

/// Пересекает ли отрезок `start..end` AABB `mins..maxs`.
fn segment_intersects_aabb(start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> bool {
    let axes = [
        (start.x, end.x, mins.x, maxs.x),
        (start.y, end.y, mins.y, maxs.y),
        (start.z, end.z, mins.z, maxs.z),
    ];
    let mut near = 0.0f32;
    let mut far = 1.0f32;
    for (from, to, min, max) in axes {
        let delta = to - from;
        if delta.abs() < f32::EPSILON {
            if from < min || from > max {
                return false;
            }
            continue;
        }
        let inverse = 1.0 / delta;
        let mut first = (min - from) * inverse;
        let mut second = (max - from) * inverse;
        if first > second {
            core::mem::swap(&mut first, &mut second);
        }
        near = near.max(first);
        far = far.min(second);
        if near > far {
            return false;
        }
    }
    true
}

fn parse_textures(lump: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let Some(count) = read_i32(lump, 0) else {
        return names;
    };
    for index in 0..count.max(0) as usize {
        let Some(offset) = read_i32(lump, 4 + index * 4) else {
            break;
        };
        if offset < 0 {
            names.push(String::new());
            continue;
        }
        let start = offset as usize;
        let raw = lump.get(start..start + 16).unwrap_or_default();
        let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
        names.push(String::from_utf8_lossy(&raw[..end]).into_owned());
    }
    names
}

/// Останавливает ли эта brush-сущность гранату (и вообще что-либо).
///
/// Движок линкует в столкновения только сущности с `solid != SOLID_NOT`, а у
/// триггеров геометрия есть, но она невидима и ничему не мешает. Без этой
/// проверки граната «врезается» в невидимую зону закупки или в триггер бомбсайта.
fn solid_entity(classname: &str) -> bool {
    if classname.starts_with("trigger_") {
        return false;
    }
    !matches!(
        classname,
        "func_buyzone"
            | "func_bomb_target"
            | "func_hostage_rescue"
            | "func_illusionary"
            | "func_water"
            | "func_ladder"
            | "func_friction"
            | "func_areaportal"
            | "func_areaportalwindow"
    )
}

/// Brush-сущности из текста `entities`.
fn parse_brushes(entities: &[u8], models: usize) -> Vec<Brush> {
    let text = String::from_utf8_lossy(entities);
    let mut brushes = Vec::new();
    let mut model = None;
    let mut origin = Vec3::ZERO;
    let mut classname = String::new();
    let mut skipped = 0usize;
    for (key, value) in entity_pairs(&text) {
        match key.as_str() {
            "{" => {
                model = None;
                origin = Vec3::ZERO;
                classname.clear();
            }
            "}" => {
                if let Some(index) = model.take() {
                    if index > 0 && index < models {
                        if solid_entity(&classname) {
                            brushes.push(Brush {
                                model: index,
                                origin,
                            });
                        } else {
                            skipped += 1;
                        }
                    }
                }
                origin = Vec3::ZERO;
                classname.clear();
            }
            "model" => {
                if let Some(index) = value.strip_prefix('*') {
                    model = index.trim().parse::<usize>().ok();
                }
            }
            "classname" => {
                classname = String::from(value.trim());
            }
            "origin" => {
                let mut parts = value
                    .split_whitespace()
                    .filter_map(|part| part.parse::<f32>().ok());
                if let (Some(x), Some(y), Some(z)) = (parts.next(), parts.next(), parts.next()) {
                    origin = Vec3::new(x, y, z);
                }
            }
            _ => {}
        }
    }
    if skipped > 0 {
        crate::log!("bsp: невидимых brush-сущностей (триггеры) пропущено: {skipped}");
    }
    brushes
}

/// Пары «ключ-значение» из текста `entities`, включая скобки блоков.
fn entity_pairs(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut pending: Option<String> = None;
    for token in entity_tokens(text) {
        match token.as_str() {
            "{" | "}" => {
                pairs.push((token, String::new()));
                pending = None;
            }
            _ => match pending.take() {
                None => pending = Some(token),
                Some(key) => pairs.push((key, token)),
            },
        }
    }
    pairs
}

/// Токены текста сущностей: строки в кавычках и скобки блоков отдельно.
///
/// Разбивать по кавычкам нельзя: между сущностями скобки идут подряд (`}\n{`),
/// и такой токен терял бы одну из них.
fn entity_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for symbol in text.chars() {
        match symbol {
            '"' => {
                if quoted {
                    tokens.push(core::mem::take(&mut current));
                }
                quoted = !quoted;
            }
            '{' | '}' if !quoted => {
                // Текст вне кавычек (переводы строк, отступы) не нужен.
                current.clear();
                tokens.push(String::from(symbol));
            }
            _ => {
                if quoted {
                    current.push(symbol);
                }
            }
        }
    }
    tokens
}

fn read_i32(bytes: &[u8], offset: usize) -> Option<i32> {
    let raw: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
    Some(i32::from_le_bytes(raw))
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let raw: [u8; 2] = bytes.get(offset..offset + 2)?.try_into().ok()?;
    Some(u16::from_le_bytes(raw))
}

fn read_i16(bytes: &[u8], offset: usize) -> Option<i16> {
    let raw: [u8; 2] = bytes.get(offset..offset + 2)?.try_into().ok()?;
    Some(i16::from_le_bytes(raw))
}

fn read_f32(bytes: &[u8], offset: usize) -> Option<f32> {
    let raw: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
    Some(f32::from_le_bytes(raw))
}

/// Пустая карта для тестов: одна плоскость `x = dist`.
#[cfg(test)]
mod tests {
    use super::*;

    /// Собирает минимальную карту: плоскость `-x = -wall`, слева пусто,
    /// справа сплошное.
    fn wall(wall: f32) -> Vec<u8> {
        let mut header = alloc::vec![0u8; 4 + 16 * 8];
        header[0..4].copy_from_slice(&VERSION.to_le_bytes());

        let mut planes = Vec::new();
        planes.extend_from_slice(&(-1.0f32).to_le_bytes());
        planes.extend_from_slice(&0.0f32.to_le_bytes());
        planes.extend_from_slice(&0.0f32.to_le_bytes());
        planes.extend_from_slice(&(-wall).to_le_bytes());
        planes.extend_from_slice(&0i32.to_le_bytes());

        let mut nodes = Vec::new();
        nodes.extend_from_slice(&0i32.to_le_bytes()); // planenum
        nodes.extend_from_slice(&(-1i16).to_le_bytes()); // front — пусто
        nodes.extend_from_slice(&(-2i16).to_le_bytes()); // back — сплошное
        nodes.extend_from_slice(&[0u8; 16]);

        let mut leafs = Vec::new();
        let mut leaf = |contents: i32| {
            let mut raw = Vec::new();
            raw.extend_from_slice(&contents.to_le_bytes());
            raw.extend_from_slice(&[0u8; 24]);
            leafs.extend_from_slice(&raw);
        };
        leaf(CONTENTS_EMPTY);
        leaf(CONTENTS_SOLID);

        let mut models = Vec::new();
        models.extend_from_slice(&[0u8; 36]);
        models.extend_from_slice(&0i32.to_le_bytes()); // headnode[0]
        models.extend_from_slice(&[0u8; 24]);

        let lumps = [
            (planes, lump::PLANES),
            (nodes, lump::NODES),
            (leafs, lump::LEAFS),
            (models, lump::MODELS),
        ];
        let mut body = Vec::new();
        for (data, index) in lumps {
            let start = header.len() + body.len();
            header[4 + index * 8..4 + index * 8 + 4].copy_from_slice(&(start as i32).to_le_bytes());
            header[4 + index * 8 + 4..4 + index * 8 + 8]
                .copy_from_slice(&(data.len() as i32).to_le_bytes());
            body.extend_from_slice(&data);
        }
        header.extend_from_slice(&body);
        header
    }

    /// Триггеры и прочая невидимая геометрия гранату не останавливают.
    #[test]
    fn triggers_are_not_solid() {
        assert!(!solid_entity("func_buyzone"));
        assert!(!solid_entity("func_bomb_target"));
        assert!(!solid_entity("trigger_multiple"));
        assert!(!solid_entity("func_illusionary"));
        assert!(solid_entity("func_wall"));
        assert!(solid_entity("func_door"));
        // Без класса (мировая геометрия) считаем препятствием.
        assert!(solid_entity(""));
    }

    #[test]
    fn traces_wall() {
        let bsp = Bsp::parse(&wall(100.0)).expect("карта");
        let hit = bsp.trace(Vec3::ZERO, Vec3::new(200.0, 0.0, 0.0));
        assert!(hit.hit());
        assert!((hit.end.x - 100.0).abs() < 0.1, "{:?}", hit.end);
        assert!((hit.fraction - 0.5).abs() < 0.01, "{}", hit.fraction);
    }

    #[test]
    fn misses_outside() {
        let bsp = Bsp::parse(&wall(100.0)).expect("карта");
        let hit = bsp.trace(Vec3::new(50.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 0.0));
        assert!(!hit.hit());
        assert_eq!(hit.fraction, 1.0);
    }

    #[test]
    fn rejects_other_version() {
        let mut bytes = wall(100.0);
        bytes[0..4].copy_from_slice(&29i32.to_le_bytes());
        assert_eq!(Bsp::parse(&bytes).err(), Some(BspError::Version(29)));
    }

    #[test]
    fn reads_entity_pairs() {
        let text = "{\n\"classname\" \"worldspawn\"\n}\n{\n\"classname\" \"func_door\"\n\"model\" \"*3\"\n\"origin\" \"16 -8 0\"\n}\n";
        let pairs = entity_pairs(text);
        assert!(
            pairs
                .iter()
                .any(|(key, value)| key == "model" && value == "*3")
        );
        let brushes = parse_brushes(text.as_bytes(), 8);
        assert_eq!(brushes.len(), 1);
        assert_eq!(brushes[0].model, 3);
        assert_eq!(brushes[0].origin, Vec3::new(16.0, -8.0, 0.0));
    }
}
