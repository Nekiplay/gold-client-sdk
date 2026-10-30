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
    /// Bind-pose данные: повороты/позиция/масштаб в исходной позе.
    ///
    /// Для отрисовки скелета из рантайма не нужны — там используются готовые
    /// матрицы из объекта рендерера.
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
    /// Мировой AABB.
    pub min: Vec3,
    /// Мировой AABB.
    pub max: Vec3,
}

impl WorldHitbox {
    /// Центр объёма — то, вокруг чего рисуется метка.
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
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
        Some(Self { header, len: hdr.length as usize })
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
    /// Берутся из заголовка (`min`/`max` — объём, покрывающий все анимации), а
    /// не из хитбоксов: так коробка охватывает и тело, и руки в любой позе.
    pub fn hull(&self) -> Option<(Vec3, Vec3)> {
        let header = self.header();
        let (min, max) = (header.min, header.max);
        let size = Vec3::new(max.x - min.x, max.y - min.y, max.z - min.z);
        if !min.is_finite() || !max.is_finite() || size.x <= 0.0 || size.z <= 0.0 {
            return None;
        }
        Some((min, max))
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
        let end = match (count as usize).checked_mul(size).and_then(|n| start.checked_add(n)) {
            Some(end) => end,
            None => return &[],
        };
        if end > self.len {
            return &[];
        }
        // SAFETY: границы проверены, выравнивание соблюдено (заголовок и массивы
        // в формате выровнены по 4).
        unsafe { core::slice::from_raw_parts(self.header.cast::<u8>().add(start) as *const T, count as usize) }
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
        self.bones()
            .iter()
            .enumerate()
            .filter_map(move |(i, b)| {
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
        Self { m: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0] }
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
        let fake = StudioModel { header: &hdr, len: model.len() };
        assert!(fake.bones().is_empty());
    }

    #[test]
    fn bone_matrix_origin_is_column_three() {
        let m = BoneMatrix { m: [1.0, 0.0, 0.0, 10.0, 0.0, 1.0, 0.0, 20.0, 0.0, 0.0, 1.0, 30.0] };
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