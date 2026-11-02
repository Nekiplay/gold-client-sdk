//! Студийные модели GoldSrc (`.mdl`, версия 10): структуры и разбор.
//!
//! Все layout'ы проверены на реальных моделях из установки CS 1.6
//! (`cstrike/models/player/gign/gign.mdl`, `bag.mdl`, `chick.mdl`), а не взяты
//! из HLSDK «по памяти». Отличия от HLSDK-заголовков, которые важно помнить:
//!
//! * в `studiohdr_t` **нет** поля `checksum`: имя занимает `0x08..0x48`, сразу за
//!   ним идёт `length` (`0x48`), заголовок ровно `0xF4` байта;
//! * имена костей **inline** (`char name[32]` внутри `mstudiobone_t`), а не
//!   смещениями, как в некоторых других форматах;
//! * `mstudiobone_t` = `0x70`, `parent` в `+0x20`, `flags` в `+0x24`,
//!   `bonecontroller[6]` в `+0x28`, дальше 12 float bind-pose данных;
//! * `mstudioattachment_t` = `0x58` (`name[32]`, `bone` в `+0x20`, `org` в `+0x24`),
//!   а не `0x18` из HLSDK.
//!
//! Layout'ы скомпилированы как `#[repr(C)]` со `static_assert`'ами на размер, так
//! что расхождение с бинарником поймает компилятор, а не игра.

use alloc::vec;
use alloc::vec::Vec;

use core::ffi::c_void;
use core::mem::size_of;

use crate::math::Vec3;

/// `"IDST"` как little-endian `i32`.
pub const IDST: i32 = 0x5453_4449;
/// Поддерживаемая версия формата.
pub const VERSION: i32 = 10;
/// Размер `studiohdr_t`.
pub const HEADER_SIZE: usize = 0xF4;
/// Предел костей в GoldSrc (`MAXSTUDIOBONES`).
pub const MAXSTUDIOBONES: usize = 128;

/// Заголовок модели.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StudioHeader {
    /// `"IDST"`.
    pub id: i32,
    /// Должно быть [`VERSION`].
    pub version: i32,
    /// Внутренний путь компиляции, не путь в игре.
    pub name: [u8; 64],
    /// Полный размер модели в байтах (включая внешние `T.mdl` не учитывается).
    pub length: i32,
    pub eyeposition: Vec3,
    pub min: Vec3,
    pub max: Vec3,
    pub bbmin: Vec3,
    pub bbmax: Vec3,
    pub flags: i32,
    pub numbones: i32,
    pub boneindex: i32,
    pub numbonecontrollers: i32,
    pub bonecontrollerindex: i32,
    pub numhitboxes: i32,
    pub hitboxindex: i32,
    pub numseq: i32,
    pub seqindex: i32,
    pub numseqgroups: i32,
    pub seqgroupindex: i32,
    pub numtextures: i32,
    pub textureindex: i32,
    pub texturedataindex: i32,
    pub numskinref: i32,
    pub numskinfamilies: i32,
    pub skinindex: i32,
    pub numbodyparts: i32,
    pub bodypartindex: i32,
    pub numattachments: i32,
    pub attachmentindex: i32,
    pub soundtable: i32,
    pub soundindex: i32,
    pub soundgroups: i32,
    pub soundgroupindex: i32,
    pub numtransitions: i32,
    pub transitionindex: i32,
}

const _: () = assert!(size_of::<StudioHeader>() == HEADER_SIZE);

/// `mstudiobone_t` (`0x70`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StudioBone {
    /// Имя кости, NUL-терминированное, лежит прямо в записи.
    pub name: [u8; 32],
    /// Индекс родителя, `-1` у корня.
    pub parent: i32,
    pub flags: i32,
    /// Индексы контроллеров по осям (или `-1`).
    pub bonecontroller: [i32; 6],
    /// Bind-pose данные: смещение, поворот и масштаб в исходной позе.
    ///
    /// Проверено на реальных моделях: `[0..3]` — смещение относительно родителя
    /// в юнитах, `[3..6]` — поворот в радианах (у корня player-моделей
    /// `-1.5708`), `[6..12]` — масштаб (значения порядка `1/256`). Для готовой
    /// bind-позы есть [`StudioModel::bind_pose`]; в рантайме кости приходят
    /// готовыми матрицами из объекта рендерера.
    pub bind_pose: [f32; 12],
}

const _: () = assert!(size_of::<StudioBone>() == 0x70);

/// Группы хитбоксов — те же номера, что в `mstudiohitbox_t::group`.
///
/// Совпадают с `HITGROUP_*` из HLSDK и с тем, как движок считает попадания:
/// `1` — голова, `4` — левая рука, `5` — правая, `6` — левая нога, `7` — правая.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitGroup {
    /// Общее тело / бункер.
    Generic,
    /// Голова.
    Head,
    /// Грудь.
    Chest,
    /// Живот.
    Stomach,
    /// Левая рука.
    LeftArm,
    /// Правая рука.
    RightArm,
    /// Левая нога.
    LeftLeg,
    /// Правая нога.
    RightLeg,
    /// Неизвестная группа (не 0..=7).
    Other(i32),
}

impl HitGroup {
    /// Группа по номеру из модели.
    pub fn from_number(number: i32) -> Self {
        match number {
            0 => Self::Generic,
            1 => Self::Head,
            2 => Self::Chest,
            3 => Self::Stomach,
            4 => Self::LeftArm,
            5 => Self::RightArm,
            6 => Self::LeftLeg,
            7 => Self::RightLeg,
            other => Self::Other(other),
        }
    }

    /// Номер группы.
    pub fn number(self) -> i32 {
        match self {
            Self::Generic => 0,
            Self::Head => 1,
            Self::Chest => 2,
            Self::Stomach => 3,
            Self::LeftArm => 4,
            Self::RightArm => 5,
            Self::LeftLeg => 6,
            Self::RightLeg => 7,
            Self::Other(other) => other,
        }
    }

    /// Русское имя для логов и подписей.
    pub fn name(self) -> &'static str {
        match self {
            Self::Generic => "тело",
            Self::Head => "голова",
            Self::Chest => "грудь",
            Self::Stomach => "живот",
            Self::LeftArm => "левая рука",
            Self::RightArm => "правая рука",
            Self::LeftLeg => "левая нога",
            Self::RightLeg => "правая нога",
            Self::Other(_) => "?",
        }
    }
}

/// Хитбокс, переведённый в мировые координаты.
#[derive(Debug, Clone, Copy)]
pub struct WorldHitbox {
    /// Группа: голова, руки, ноги, тело.
    pub group: HitGroup,
    /// Кость, к которой привязан хитбокс.
    pub bone: usize,
    /// Мировой AABB: годится для меток, но не для попадания — у повёрнутого
    /// хитбокса он заметно больше настоящего объёма.
    pub min: Vec3,
    /// Мировой AABB.
    pub max: Vec3,
    /// Матрица кости: переводит локальные границы в мир.
    pub matrix: BoneMatrix,
    /// Границы хитбокса в системе координат кости (`bbmin`).
    pub local_min: Vec3,
    /// Границы хитбокса в системе координат кости (`bbmax`).
    pub local_max: Vec3,
}

impl WorldHitbox {
    /// Центр объёма — то, вокруг чего рисуется метка.
    ///
    /// Считается через матрицу кости: центр AABB у повёрнутого хитбокса
    /// смещён от настоящего.
    pub fn center(&self) -> Vec3 {
        self.matrix
            .transform_point((self.local_min + self.local_max) * 0.5)
    }

    /// Верхняя точка по мировому Z.
    pub fn top(&self) -> Vec3 {
        let center = self.center();
        Vec3::new(center.x, center.y, self.max.z)
    }

    /// Размеры объёма.
    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }

    /// 12 рёбер настоящего (повёрнутого вместе с костью) объёма.
    ///
    /// Углы берутся в системе кости и переводятся её матрицей — рисовать надо
    /// именно их: AABB [`Self::min`]/[`Self::max`] у повёрнутого хитбокса шире
    /// настоящего объёма и на экране выглядит больше тела.
    pub fn edges(&self) -> [(Vec3, Vec3); 12] {
        let (a, b) = (self.local_min, self.local_max);
        let corners = [
            Vec3::new(a.x, a.y, a.z),
            Vec3::new(b.x, a.y, a.z),
            Vec3::new(a.x, b.y, a.z),
            Vec3::new(b.x, b.y, a.z),
            Vec3::new(a.x, a.y, b.z),
            Vec3::new(b.x, a.y, b.z),
            Vec3::new(a.x, b.y, b.z),
            Vec3::new(b.x, b.y, b.z),
        ]
        .map(|point| self.matrix.transform_point(point));
        const PAIRS: [(usize, usize); 12] = [
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
        PAIRS.map(|(first, second)| (corners[first], corners[second]))
    }

    /// Точное пересечение луча с хитбоксом: расстояние до входа.
    ///
    /// Луч переводится в систему координат кости (где хитбокс — обычный
    /// параллелепипед), проверяется методом плит и возвращается обратно. Так
    /// попадание считается по настоящему, повёрнутому вместе с костью объёму,
    /// а не по раздутому AABB.
    pub fn ray(&self, origin: Vec3, direction: Vec3) -> Option<f32> {
        let local_origin = self.matrix.untransform_point(origin);
        let local_direction = self.matrix.untransform_direction(direction);
        let start = [local_origin.x, local_origin.y, local_origin.z];
        let step = [local_direction.x, local_direction.y, local_direction.z];
        let low = [self.local_min.x, self.local_min.y, self.local_min.z];
        let high = [self.local_max.x, self.local_max.y, self.local_max.z];
        let mut near = f32::NEG_INFINITY;
        let mut far = f32::INFINITY;
        for axis in 0..3 {
            if step[axis].abs() < 1e-6 {
                if start[axis] < low[axis] || start[axis] > high[axis] {
                    return None;
                }
                continue;
            }
            let first = (low[axis] - start[axis]) / step[axis];
            let second = (high[axis] - start[axis]) / step[axis];
            let (nearer, farther) = if first < second {
                (first, second)
            } else {
                (second, first)
            };
            near = near.max(nearer);
            far = far.min(farther);
            if near > far {
                return None;
            }
        }
        (far >= 0.0).then(|| near.max(0.0))
    }
}

/// `mstudiobonecontroller_t` (`0x18`). Поля не проверялись по значениям —
/// используется только размер (для контроля разметки массива).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StudioBoneController {
    pub bone: i32,
    pub ctrl_type: i32,
    pub start: f32,
    pub end: f32,
    pub rest: f32,
    pub index: i32,
}

const _: () = assert!(size_of::<StudioBoneController>() == 0x18);

/// `mstudiohitbox_t` (`0x20`): параллелепипед, привязанный к кости.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StudioHitbox {
    pub bone: i32,
    pub group: i32,
    pub bbmin: Vec3,
    pub bbmax: Vec3,
}

const _: () = assert!(size_of::<StudioHitbox>() == 0x20);

/// `mstudioseqdesc_t` (`0xB0`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StudioSeqDesc {
    pub name: [u8; 32],
    pub fps: f32,
    pub flags: i32,
    pub activity: i32,
    pub actweight: i32,
    pub numevents: i32,
    pub eventindex: i32,
    pub numframes: i32,
    pub numpivots: i32,
    pub pivotindex: i32,
    pub motiontype: i32,
    pub motionbone: i32,
    pub line: Vec3,
    pub bbmin: Vec3,
    pub bbmax: Vec3,
    pub numblends: i32,
    pub animindexindex: i32,
    pub groupsize: [i32; 2],
    pub blendtype: [i32; 2],
    pub blendstart: [f32; 2],
    pub blendend: [f32; 2],
    pub blendparent: i32,
    pub seqgroup: i32,
    pub entrynode: i32,
    pub exitnode: i32,
    pub nodeflags: i32,
    pub nextseq: i32,
}

const _: () = assert!(size_of::<StudioSeqDesc>() == 0xB0);

/// `mstudiotexture_t` (`0x50`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StudioTexture {
    pub name: [u8; 64],
    pub flags: i32,
    pub width: i32,
    pub height: i32,
    pub index: i32,
}

const _: () = assert!(size_of::<StudioTexture>() == 0x50);

/// `mstudiobodyparts_t` (`0x4C`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StudioBodyPart {
    pub name: [u8; 64],
    pub nummodels: i32,
    /// Базовое значение для выбора варианта.
    pub base: i32,
    /// Смещение до массива `mstudiomodel_t`.
    pub modelindex: i32,
}

const _: () = assert!(size_of::<StudioBodyPart>() == 0x4C);

/// `mstudiomodel_t` (`0x70`): вариант внутри bodypart'а.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StudioModelPart {
    pub name: [u8; 64],
    pub part_type: i32,
    pub boundingradius: f32,
    pub nummesh: i32,
    pub meshindex: i32,
    pub numverts: i32,
    pub vertinfoindex: i32,
    pub vertindex: i32,
    pub numnorms: i32,
    pub norminfoindex: i32,
    pub normindex: i32,
    pub numgroups: i32,
    pub groupindex: i32,
}

const _: () = assert!(size_of::<StudioModelPart>() == 0x70);

/// `mstudioattachment_t` (`0x58` в этом билде).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StudioAttachment {
    pub name: [u8; 32],
    /// Кость, к которой привязан attachment.
    pub bone: i32,
    pub org: Vec3,
    /// Хвост записи (`0x28`), назначение не подтверждено.
    pub reserved: [u8; 0x28],
}

const _: () = assert!(size_of::<StudioAttachment>() == 0x58);

/// Прочитанная строка из inline-массива.
fn cstr(bytes: &[u8]) -> &str {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..end]).unwrap_or("")
}

impl StudioBone {
    pub fn name(&self) -> &str {
        cstr(&self.name)
    }
}

impl StudioSeqDesc {
    pub fn name(&self) -> &str {
        cstr(&self.name)
    }
}

impl StudioTexture {
    pub fn name(&self) -> &str {
        cstr(&self.name)
    }
}

impl StudioBodyPart {
    pub fn name(&self) -> &str {
        cstr(&self.name)
    }
}

impl StudioModelPart {
    pub fn name(&self) -> &str {
        cstr(&self.name)
    }
}

impl StudioAttachment {
    pub fn name(&self) -> &str {
        cstr(&self.name)
    }
}

impl StudioHeader {
    pub fn name(&self) -> &str {
        cstr(&self.name)
    }
}

/// Вид на загруженную в память студийную модель с проверкой границ.
///
/// Держит указатель на `studiohdr_t` и длину буфера: любой доступ к массивам
/// проверяется, поэтому битый или недогруженный модель не уронит процесс.
#[derive(Debug, Clone, Copy)]
pub struct StudioModel {
    header: *const StudioHeader,
    len: usize,
}

impl StudioModel {
    /// Оборачивает указатель на `studiohdr_t`.
    ///
    /// # Safety
    /// `header` должен указывать на валидный заголовок студийной модели.
    /// Возвращает `None`, если заголовок не похож на студийную модель.
    pub unsafe fn from_ptr(header: *const StudioHeader) -> Option<Self> {
        if header.is_null() {
            return None;
        }
        // SAFETY: контракт функции — указатель валиден.
        let hdr = unsafe { &*header };
        if hdr.id != IDST || hdr.version != VERSION {
            return None;
        }
        if hdr.length <= HEADER_SIZE as i32 || hdr.length > 0x400_0000 {
            return None;
        }
        if hdr.numbones < 0 || hdr.numbones as usize > MAXSTUDIOBONES {
            return None;
        }
        Some(Self {
            header,
            len: hdr.length as usize,
        })
    }

    pub fn header(&self) -> &StudioHeader {
        // SAFETY: указатель проверен в `from_ptr`.
        unsafe { &*self.header }
    }

    pub fn name(&self) -> &str {
        self.header().name()
    }

    /// `IDST` — должно быть `0x54534449`.
    pub fn header_id(&self) -> i32 {
        self.header().id
    }

    /// Версия формата; GoldSrc — 10.
    pub fn header_version(&self) -> i32 {
        self.header().version
    }

    /// Похожа ли модель на модель игрока (а не оружия/пропа).
    ///
    /// В заголовке `.mdl` имя хранится без каталога `models/` — например
    /// `terror\terror.mdl`, поэтому проверка идёт по правилам
    /// [`crate::player::is_player_model_name`].
    pub fn is_player_model(&self) -> bool {
        crate::player::is_player_model_name(self.name())
    }

    /// Габариты модели в её собственных координатах: `(минимум, максимум)`.
    ///
    /// Берутся из заголовка (`min`/`max` — объём, покрывающий все анимации).
    /// У игровых моделей CS 1.6 эти поля часто нулевые (проверено на
    /// `gign.mdl`, `v_ak47.mdl`, `bag.mdl`), поэтому когда заголовок пуст —
    /// возвращается объём по хитбоксам в bind-позе.
    pub fn hull(&self) -> Option<(Vec3, Vec3)> {
        let header = self.header();
        let (min, max) = (header.min, header.max);
        let size = Vec3::new(max.x - min.x, max.y - min.y, max.z - min.z);
        if min.is_finite() && max.is_finite() && size.x > 0.0 && size.z > 0.0 {
            return Some((min, max));
        }
        self.hitbox_bounds()
    }

    /// Матрицы костей в bind-позе (в системе координат модели).
    ///
    /// Локальный поворот кости собирается как `Y(yaw) * X(pitch) * Z(roll)` —
    /// порядок `AngleMatrix` из HLSDK. Смещение применяется после поворота, а
    /// матрица ребёнка домножается на матрицу родителя.
    pub fn bind_pose(&self) -> Vec<BoneMatrix> {
        let bones = self.bones();
        let mut out = vec![BoneMatrix::identity(); bones.len()];
        for (index, bone) in bones.iter().enumerate() {
            let data = &bone.bind_pose;
            let local = BoneMatrix::translated(Vec3::new(data[0], data[1], data[2]))
                .mul(&BoneMatrix::from_euler(data[3], data[4], data[5]));
            let parent = bone.parent;
            out[index] = if parent >= 0 && (parent as usize) < index {
                out[parent as usize].mul(&local)
            } else {
                local
            };
        }
        out
    }

    /// Объём по хитбоксам в bind-позе: восемь углов каждой коробки в мировых
    /// координатах модели, затем общая рамка.
    pub fn hitbox_bounds(&self) -> Option<(Vec3, Vec3)> {
        let poses = self.bind_pose();
        let mut min = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
        let mut max = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
        let mut seen = false;
        for hitbox in self.hitboxes() {
            if hitbox.bone < 0 {
                continue;
            }
            let Some(matrix) = poses.get(hitbox.bone as usize) else {
                continue;
            };
            for x in [hitbox.bbmin.x, hitbox.bbmax.x] {
                for y in [hitbox.bbmin.y, hitbox.bbmax.y] {
                    for z in [hitbox.bbmin.z, hitbox.bbmax.z] {
                        let point = matrix.transform_point(Vec3::new(x, y, z));
                        if !point.is_finite() {
                            continue;
                        }
                        seen = true;
                        min = Vec3::new(min.x.min(point.x), min.y.min(point.y), min.z.min(point.z));
                        max = Vec3::new(max.x.max(point.x), max.y.max(point.y), max.z.max(point.z));
                    }
                }
            }
        }
        seen.then_some((min, max))
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Массив по смещению и количеству с проверкой, что он целиком внутри модели.
    fn table<T: Copy>(&self, offset: i32, count: i32) -> &[T] {
        if offset < HEADER_SIZE as i32 || count <= 0 {
            return &[];
        }
        let start = offset as usize;
        let size = size_of::<T>();
        let end = match (count as usize)
            .checked_mul(size)
            .and_then(|n| start.checked_add(n))
        {
            Some(end) => end,
            None => return &[],
        };
        if end > self.len {
            return &[];
        }
        // SAFETY: границы проверены, выравнивание соблюдено (заголовок и массивы
        // в формате выровнены по 4).
        unsafe {
            core::slice::from_raw_parts(
                self.header.cast::<u8>().add(start) as *const T,
                count as usize,
            )
        }
    }

    pub fn bones(&self) -> &[StudioBone] {
        let h = self.header();
        self.table(h.boneindex, h.numbones)
    }

    pub fn bone(&self, index: usize) -> Option<&StudioBone> {
        self.bones().get(index)
    }

    pub fn sequences(&self) -> &[StudioSeqDesc] {
        let h = self.header();
        self.table(h.seqindex, h.numseq)
    }

    pub fn bodyparts(&self) -> &[StudioBodyPart] {
        let h = self.header();
        self.table(h.bodypartindex, h.numbodyparts)
    }

    pub fn attachments(&self) -> &[StudioAttachment] {
        let h = self.header();
        self.table(h.attachmentindex, h.numattachments)
    }

    /// Модели внутри bodypart'а (варианты скина/оружия). Требует указания
    /// bodypart'а, потому что массив у каждого свой.
    pub fn bodypart_models(&self, part: &StudioBodyPart) -> &[StudioModelPart] {
        self.table(part.modelindex, part.nummodels)
    }

    pub fn hitboxes(&self) -> &[StudioHitbox] {
        let h = self.header();
        self.table(h.hitboxindex, h.numhitboxes)
    }

    pub fn hitbox_controllers(&self) -> &[StudioBoneController] {
        let h = self.header();
        self.table(h.bonecontrollerindex, h.numbonecontrollers)
    }

    pub fn textures(&self) -> &[StudioTexture] {
        let h = self.header();
        self.table(h.textureindex, h.numtextures)
    }

    /// Рёбра скелета: `(кость, родитель)` для всех костей, у которых есть родитель.
    ///
    /// Это и есть геометрия «скелета» модели: рисуя отрезки между позициями
    /// костей, получаем проволочный скелет.
    pub fn skeleton_edges(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        let count = self.bones().len();
        self.bones().iter().enumerate().filter_map(move |(i, b)| {
            let parent = b.parent;
            (parent >= 0 && (parent as usize) < count && parent as usize != i)
                .then_some((i, parent as usize))
        })
    }

    /// Собирает скелет по готовым позициям костей.
    ///
    /// Нужен, когда позиции сняты в один момент (внутри `StudioSetupBones`), а
    /// рисуются позже: топология берётся из модели, координаты — из `origins`.
    /// Рёбра с несуществующими индексами пропускаются.
    pub fn skeleton_lines_with(&self, origins: &[Vec3]) -> Vec<(Vec3, Vec3)> {
        self.skeleton_edges()
            .filter_map(|(child, parent)| Some((*origins.get(parent)?, *origins.get(child)?)))
            .collect()
    }

    /// Находит кость по имени (регистронезависимо): удобно для «головы», «шеи» и т.п.
    pub fn find_bone(&self, name: &str) -> Option<usize> {
        self.bones()
            .iter()
            .position(|b| b.name().eq_ignore_ascii_case(name))
    }
}

/// Умножение матриц 3x3 (используется при сборке поворота из углов Эйлера).
fn mul3(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (column, cell) in out_row.iter_mut().enumerate() {
            *cell = a[row][0] * b[0][column] + a[row][1] * b[1][column] + a[row][2] * b[2][column];
        }
    }
    out
}

/// Матрица кости 3x4 (`float[3][4]`), как её хранит движок.
///
/// Трансляция лежит в `[0][3]`, `[1][3]`, `[2][3]`, то есть в плоском массиве
/// это индексы 3, 7, 11 — порядок подтверждён кодом `SV_StudioSetupBones`.
#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub struct BoneMatrix {
    pub m: [f32; 12],
}

impl BoneMatrix {
    pub const fn identity() -> Self {
        Self {
            m: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        }
    }

    /// Перенос на вектор.
    pub fn translated(position: Vec3) -> Self {
        let mut out = Self::identity();
        out.m[3] = position.x;
        out.m[7] = position.y;
        out.m[11] = position.z;
        out
    }

    /// Поворот по углам Эйлера в радианах: `Y(yaw) * X(pitch) * Z(roll)`.
    ///
    /// Та же последовательность, что в `AngleMatrix` из HLSDK. Собирается
    /// явным умножением трёх матриц 3x3 — так в формуле не перепутать строки и
    /// столбцы, что даёт «сложенный в точку» скелет.
    pub fn from_euler(pitch: f32, yaw: f32, roll: f32) -> Self {
        let (sp, cp) = pitch.sin_cos();
        let (sy, cy) = yaw.sin_cos();
        let (sr, cr) = roll.sin_cos();

        let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
        let rx = [[1.0, 0.0, 0.0], [0.0, cp, -sp], [0.0, sp, cp]];
        let rz = [[cr, -sr, 0.0], [sr, cr, 0.0], [0.0, 0.0, 1.0]];
        let m = mul3(&mul3(&ry, &rx), &rz);
        Self {
            m: [
                m[0][0], m[0][1], m[0][2], 0.0, m[1][0], m[1][1], m[1][2], 0.0, m[2][0], m[2][1],
                m[2][2], 0.0,
            ],
        }
    }

    /// `self * other`: обычное умножение 3x4 (поворот плюс смещение).
    pub fn mul(&self, other: &Self) -> Self {
        let (a, b) = (&self.m, &other.m);
        let mut out = [0.0f32; 12];
        for row in 0..3 {
            for column in 0..3 {
                out[row * 4 + column] = a[row * 4] * b[column]
                    + a[row * 4 + 1] * b[4 + column]
                    + a[row * 4 + 2] * b[8 + column];
            }
            out[row * 4 + 3] =
                a[row * 4] * b[3] + a[row * 4 + 1] * b[7] + a[row * 4 + 2] * b[11] + a[row * 4 + 3];
        }
        Self { m: out }
    }

    /// Позиция кости в мире.
    pub fn origin(&self) -> Vec3 {
        Vec3::new(self.m[3], self.m[7], self.m[11])
    }

    /// Преобразует точку из системы координат кости в мир.
    ///
    /// Матрица хранится как поворот `3x3` плюс трансляция в третьей колонке,
    /// поэтому `v' = R * v + T`.
    pub fn transform_point(&self, point: Vec3) -> Vec3 {
        Vec3::new(
            self.m[0] * point.x + self.m[1] * point.y + self.m[2] * point.z + self.m[3],
            self.m[4] * point.x + self.m[5] * point.y + self.m[6] * point.z + self.m[7],
            self.m[8] * point.x + self.m[9] * point.y + self.m[10] * point.z + self.m[11],
        )
    }

    pub fn is_finite(&self) -> bool {
        self.m.iter().all(|v| v.is_finite())
    }

    /// Преобразует точку из мира в систему координат кости: `R^T * (p - T)`.
    ///
    /// Поворот ортонормированный (без масштаба), поэтому обратный — просто
    /// транспонированный.
    pub fn untransform_point(&self, point: Vec3) -> Vec3 {
        let dx = point.x - self.m[3];
        let dy = point.y - self.m[7];
        let dz = point.z - self.m[11];
        Vec3::new(
            self.m[0] * dx + self.m[4] * dy + self.m[8] * dz,
            self.m[1] * dx + self.m[5] * dy + self.m[9] * dz,
            self.m[2] * dx + self.m[6] * dy + self.m[10] * dz,
        )
    }

    /// Преобразует направление из мира в систему координат кости (`R^T * d`).
    pub fn untransform_direction(&self, direction: Vec3) -> Vec3 {
        Vec3::new(
            self.m[0] * direction.x + self.m[4] * direction.y + self.m[8] * direction.z,
            self.m[1] * direction.x + self.m[5] * direction.y + self.m[9] * direction.z,
            self.m[2] * direction.x + self.m[6] * direction.y + self.m[10] * direction.z,
        )
    }
}

/// Читает матрицу кости из массива `m_pbonetransform`.
///
/// # Safety
/// `base` должен указывать на начало массива матриц (`renderer::BONE_TRANSFORM`
/// объекта рендерера), а `bone` быть меньше фактического числа костей модели.
pub unsafe fn bone_matrix(base: *const BoneMatrix, bone: usize) -> BoneMatrix {
    // SAFETY: контракт функции передан вызывающему.
    unsafe { *base.add(bone) }
}

/// Приводит `model_t *` к заголовку модели через `Mod_Extradata`-совместимый путь.
///
/// # Safety
/// `model` должен быть валидным `model_t *`.
pub unsafe fn model_extradata(model: *mut c_void) -> Option<StudioModel> {
    let addr = model as usize;
    if addr == 0 || !crate::process::is_readable(addr) {
        return None;
    }
    // SAFETY: контракт функции — валидный model_t; страница проверена.
    let field = addr + crate::offsets::model::EXTRADATA;
    if !crate::process::is_readable(field) {
        return None;
    }
    // SAFETY: поле лежит внутри читаемой страницы.
    let hdr = unsafe { *(field as *const *const StudioHeader) };
    // SAFETY: `from_ptr` проверяет магию заголовка.
    unsafe { StudioModel::from_ptr(hdr) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Собирает минимальную валидную модель: заголовок + 3 кости.
    fn synthetic() -> Vec<u8> {
        let bone_index = HEADER_SIZE;
        let bones = 3usize;
        let len = bone_index + bones * size_of::<StudioBone>();
        let mut buf = vec![0u8; len];

        // id / version / length
        buf[0..4].copy_from_slice(&IDST.to_le_bytes());
        buf[4..8].copy_from_slice(&VERSION.to_le_bytes());
        buf[0x48..0x4C].copy_from_slice(&(len as i32).to_le_bytes());
        // numbones / boneindex
        buf[0x8C..0x90].copy_from_slice(&(bones as i32).to_le_bytes());
        buf[0x90..0x94].copy_from_slice(&(bone_index as i32).to_le_bytes());

        // Bip01 (-1), Bip01 Pelvis (0), Bip01 Head (1)
        for (i, (name, parent)) in [("Bip01", -1i32), ("Bip01 Pelvis", 0), ("Bip01 Head", 1)]
            .into_iter()
            .enumerate()
        {
            let off = bone_index + i * size_of::<StudioBone>();
            let bytes = name.as_bytes();
            buf[off..off + bytes.len()].copy_from_slice(bytes);
            buf[off + 0x20..off + 0x24].copy_from_slice(&parent.to_le_bytes());
        }
        buf
    }

    #[test]
    fn parses_synthetic_model() {
        let buf = synthetic();
        // SAFETY: буфер сформирован как валидный заголовок.
        let model = unsafe { StudioModel::from_ptr(buf.as_ptr().cast()) }.expect("валидная модель");
        assert_eq!(model.bones().len(), 3);
        assert_eq!(model.bone(0).unwrap().name(), "Bip01");
        assert_eq!(model.bone(2).unwrap().name(), "Bip01 Head");
        assert_eq!(model.bone(2).unwrap().parent, 1);
        assert_eq!(model.find_bone("bip01 head"), Some(2));
    }

    #[test]
    fn skeleton_edges_skip_roots() {
        let buf = synthetic();
        let model = unsafe { StudioModel::from_ptr(buf.as_ptr().cast()) }.unwrap();
        let edges: Vec<_> = model.skeleton_edges().collect();
        assert_eq!(edges, vec![(1, 0), (2, 1)]);
    }

    #[test]
    fn rejects_foreign_data() {
        let buf = vec![0u8; 512];
        // SAFETY: проверяем, что нулевой заголовок отвергается.
        assert!(unsafe { StudioModel::from_ptr(buf.as_ptr().cast()) }.is_none());
    }

    #[test]
    fn table_access_is_bounds_checked() {
        let buf = synthetic();
        let model = unsafe { StudioModel::from_ptr(buf.as_ptr().cast()) }.unwrap();
        // Подделываем заголовок: массив костей за пределами `length`.
        let mut hdr = *model.header();
        hdr.numbones = 10_000;
        let fake = StudioModel {
            header: &hdr,
            len: model.len(),
        };
        assert!(fake.bones().is_empty());
    }

    #[test]
    fn bone_matrix_origin_is_column_three() {
        let m = BoneMatrix {
            m: [
                1.0, 0.0, 0.0, 10.0, 0.0, 1.0, 0.0, 20.0, 0.0, 0.0, 1.0, 30.0,
            ],
        };
        assert_eq!(m.origin(), Vec3::new(10.0, 20.0, 30.0));
        assert!(m.is_finite());
    }

    #[test]
    fn struct_sizes_match_binary() {
        assert_eq!(size_of::<StudioHeader>(), 0xF4);
        assert_eq!(size_of::<StudioBone>(), 0x70);
        assert_eq!(size_of::<StudioSeqDesc>(), 0xB0);
        assert_eq!(size_of::<StudioBodyPart>(), 0x4C);
        assert_eq!(size_of::<StudioModelPart>(), 0x70);
        assert_eq!(size_of::<StudioTexture>(), 0x50);
        assert_eq!(size_of::<StudioAttachment>(), 0x58);
        assert_eq!(size_of::<StudioHitbox>(), 0x20);
        assert_eq!(size_of::<StudioBoneController>(), 0x18);
    }
}
