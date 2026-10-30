//! Доступ к клиентскому рендереру студийных моделей.
//!
//! Объект `CStudioModelRenderer` (`client.dll + 0x1F51C8`) — то самое место, где
//! после каждого кадра лежат готовые матрицы костей нарисованной модели. Читать
//! их дешевле и безопаснее, чем считать скелет самому: движок уже посчитал
//! интерполяцию анимации, контроллеры и привязку к `attachment`.
//!
//! Порядок использования в кадре:
//!
//! 1. [`StudioRenderer::global`] — объект (живёт с загрузки клиента).
//! 2. [`StudioRenderer::is_initialized`] — движок уже отдал интерфейс.
//! 3. [`StudioRenderer::studio_model`] — модель, которую сейчас рисовали.
//! 4. [`StudioRenderer::bone_origin`] — позиции костей в мире.

use core::ffi::{c_void, CStr};

use crate::engine::EntityRef;
use crate::math::Vec3;
use crate::offsets::{client, hw, renderer};
use crate::process;
use crate::studio::{BoneMatrix, StudioHeader, StudioModel, MAXSTUDIOBONES};

/// Интерфейс рендерера, который клиент отдаёт движку (`r_studio_interface_t`).
#[repr(C)]
pub struct StudioInterface {
    /// Движок принимает только `1`.
    pub version: i32,
    pub draw_model: Option<unsafe extern "C" fn(i32) -> i32>,
    pub draw_player: Option<unsafe extern "C" fn(i32, *mut c_void) -> i32>,
}

impl core::fmt::Debug for StudioInterface {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StudioInterface")
            .field("version", &self.version)
            .field("draw_model", &self.draw_model.map(|p| p as usize))
            .field("draw_player", &self.draw_player.map(|p| p as usize))
            .finish()
    }
}

impl StudioInterface {
    pub const VERSION: i32 = 1;

    /// Вызывает `StudioDrawModel` этого интерфейса.
    ///
    /// Так из своей подмены зовут оригинал: `prev.call_draw_model(flags)`.
    ///
    /// # Safety
    /// Только из хука отрисовки моделей (рендер-поток с активным GL-контекстом);
    /// `self` должен указывать на живой интерфейс.
    pub unsafe fn call_draw_model(&self, flags: i32) -> i32 {
        match self.draw_model {
            // SAFETY: контракт передан вызывающим.
            Some(f) => unsafe { f(flags) },
            None => 0,
        }
    }

    /// Вызывает `StudioDrawPlayer` этого интерфейса.
    ///
    /// # Safety
    /// См. [`StudioInterface::call_draw_model`]; `player_info` — то, что передал
    /// движок.
    pub unsafe fn call_draw_player(&self, flags: i32, player_info: *mut c_void) -> i32 {
        match self.draw_player {
            // SAFETY: контракт передан вызывающим.
            Some(f) => unsafe { f(flags, player_info) },
            None => 0,
        }
    }
}

/// Адрес поля `g_pStudioInterface` в `hw.dll` — точка подмены рендерера.
pub fn interface_slot() -> Option<*mut *mut StudioInterface> {
    let base = process::hw_base()?;
    Some((base + hw::P_STUDIO_INTERFACE) as *mut *mut StudioInterface)
}

/// Текущий интерфейс, который видит движок.
pub fn studio_interface() -> Option<&'static StudioInterface> {
    let slot = interface_slot()?;
    // SAFETY: слот существует, значение может быть null — проверяем.
    let ptr = unsafe { *slot };
    if ptr.is_null() {
        return None;
    }
    // SAFETY: движок ожидает ровно такую структуру.
    Some(unsafe { &*ptr })
}

/// Активен ли клиентский рендерер (а не дефолтный движковый).
///
/// Пока клиент не загрузился, в слоте лежит `hw::DEFAULT_STUDIO_INTERFACE` — у
/// него нет костей (`m_pbonetransform` клиентского объекта пуст), поэтому
/// рисовать по нему нечего. Удобно как условие «можно подменять».
pub fn is_client_renderer_active() -> bool {
    let slot = match interface_slot() {
        Some(slot) => slot,
        None => return false,
    };
    let base = match process::client_base() {
        Some(base) => base,
        None => return false,
    };
    // SAFETY: слот — статическое поле движка, значение либо null, либо интерфейс.
    unsafe { *slot as usize == base + client::STUDIO_INTERFACE }
}

/// Объект `CStudioModelRenderer`.
#[derive(Debug, Clone, Copy)]
pub struct StudioRenderer {
    ptr: *mut u8,
}

/// Снимок состояния рендерера (см. [`StudioRenderer::snapshot`]).
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    /// Адрес объекта.
    pub renderer: usize,
    /// Удалось ли прочитать сам объект.
    pub readable: bool,
    /// Первое поле объекта — vtable.
    pub vtable: usize,
    /// Прошли ли проверки инициализации.
    pub initialized: bool,
    /// `+0x30`: сущность, которую рисуем.
    pub current_entity: usize,
    /// Тип/состояние модели сущности (`+0x304`).
    pub entity_state: i32,
    /// `model_t *` сущности (`+0xB94`).
    pub entity_model: usize,
    /// `studiohdr_t *`, взятый из `model_t` сущности (`+0x184`).
    pub entity_header: usize,
    /// `studiohdr_t *` из поля рендерера `+0x4C`.
    pub studio_header: usize,
    /// `IDST` заголовка (`0x54534449` — правильный).
    pub header_id: i32,
    /// Версия заголовка (должна быть 10).
    pub header_version: i32,
    /// Число костей в заголовке.
    pub header_bones: i32,
    /// `m_pbonetransform` — начало массива матриц.
    pub bone_base: usize,
    /// Первая матрица костей (трансляция в `m[3]`, `m[7]`, `m[11]`).
    pub first_bone: BoneMatrix,
}

impl StudioRenderer {
    /// Глобальный объект клиента или `None`, если клиент ещё не загружен.
    pub fn global() -> Option<Self> {
        let base = process::client_base()?;
        Some(Self { ptr: (base + client::STUDIO_RENDERER) as *mut u8 })
    }

    pub fn addr(&self) -> usize {
        self.ptr as usize
    }

    /// Адрес метода из vtable (проверяя, что vtable — ожидаемая).
    pub fn method(&self, slot: usize) -> Option<usize> {
        let vtable = self.vtable();
        if vtable == 0 || slot > 0x40 {
            return None;
        }
        // SAFETY: vtable прочитан из объекта; читаем слот.
        let f = unsafe { process::read::<usize>(vtable + slot * core::mem::size_of::<usize>()) };
        (f != 0).then_some(f)
    }

    pub fn vtable(&self) -> usize {
        // SAFETY: объект статический, первый dword — vtable.
        unsafe { process::read(self.addr() + renderer::VTABLE) }
    }

    /// Похож ли объект на инициализированный рендерер.
    ///
    /// Признак: vtable указывает внутрь клиента и начинается с `Init`, который
    /// лежит в `.text` клиента.
    pub fn is_initialized(&self) -> bool {
        let client_base = match process::client_base() {
            Some(base) => base,
            None => return false,
        };
        let vtable = self.vtable();
        if !process::in_range(vtable, client_base, 0x20_0000) {
            return false;
        }
        self.method(client::VT_INIT)
            .is_some_and(|f| process::is_code(f, client_base))
    }

    /// Сущность, которую рендерер обрабатывает сейчас.
    pub fn current_entity(&self) -> Option<EntityRef> {
        // SAFETY: объект статический.
        let ptr: *mut c_void = unsafe { process::read(self.addr() + renderer::CURRENT_ENTITY) };
        // SAFETY: указатель из движка либо null.
        unsafe { EntityRef::from_ptr(ptr) }
    }

    /// `true`, если рисуется модель локального игрока (view entity).
    pub fn is_player(&self) -> bool {
        // SAFETY: объект статический.
        unsafe { process::read::<u8>(self.addr() + renderer::IS_PLAYER) != 0 }
    }

    /// `model_t *`, который сейчас рисуется.
    pub fn render_model(&self) -> *mut c_void {
        // SAFETY: объект статический.
        unsafe { process::read(self.addr() + renderer::RENDER_MODEL) }
    }

    /// Сырой указатель на `studiohdr_t`.
    pub fn studio_header(&self) -> *const StudioHeader {
        // SAFETY: объект статический.
        unsafe { process::read(self.addr() + renderer::STUDIO_HEADER) }
    }

    /// Разобранная модель, которую рисовали в этом кадре.
    pub fn studio_model(&self) -> Option<StudioModel> {
        // SAFETY: заголовок движок держит валидным на время рендера.
        unsafe { StudioModel::from_ptr(self.studio_header()) }
    }

    /// Похоже ли то, что сейчас рисуется, на модель игрока (а не оружия/пропа).
    ///
    /// Критерий — путь модели: у игроков он `models/player/<модель>/<модель>.mdl`,
    /// у view-model — `models/v_*.mdl`, у пропов — что угодно ещё. Проверка по
    /// пути не зависит от индексов сущностей и потому переживает смену карты.
    pub fn is_player_model(&self) -> bool {
        self.studio_model().is_some_and(|m| m.is_player_model())
    }

    /// То же, но для конкретной модели (когда заголовок взят из сущности).
    pub fn is_player_model_for(&self, model: &StudioModel) -> bool {
        model.is_player_model()
    }

    /// Начало массива матриц костей (`m_pbonetransform`).
    ///
    /// Массив живёт в объекте рендерера и перезаписывается каждым
    /// `StudioSetupBones`, поэтому читать его нужно сразу после отрисовки модели.
    pub fn bone_matrix_base(&self) -> Option<*const BoneMatrix> {
        // SAFETY: объект статический.
        let ptr: *const BoneMatrix = unsafe { process::read(self.addr() + renderer::BONE_TRANSFORM) };
        (!ptr.is_null()).then_some(ptr)
    }

    /// Позиция кости в мире, если кость есть в модели.
    pub fn bone_origin(&self, bone: usize) -> Option<Vec3> {
        let model = self.studio_model()?;
        if bone >= model.bones().len() || bone >= MAXSTUDIOBONES {
            return None;
        }
        let base = self.bone_matrix_base()?;
        // SAFETY: индекс проверен по числу костей и по пределу движка.
        let matrix = unsafe { crate::studio::bone_matrix(base, bone) };
        matrix.is_finite().then(|| matrix.origin())
    }

    /// Все позиции костей текущей модели.
    pub fn bone_origins(&self) -> Vec<Vec3> {
        let model = match self.studio_model() {
            Some(m) => m,
            None => return Vec::new(),
        };
        let base = match self.bone_matrix_base() {
            Some(b) => b,
            None => return Vec::new(),
        };
        let count = model.bones().len().min(MAXSTUDIOBONES);
        (0..count)
            .filter_map(|i| {
                // SAFETY: индекс внутри и массива, и лимита движка.
                let m = unsafe { crate::studio::bone_matrix(base, i) };
                m.is_finite().then(|| m.origin())
            })
            .collect()
    }

    /// Позиции костей, сгруппированные в рёбра скелета: `(родитель, ребёнок)`.
    ///
    /// Готово к отрисовке линиями.
    pub fn skeleton_lines(&self) -> Vec<(Vec3, Vec3)> {
        let model = match self.studio_model() {
            Some(m) => m,
            None => return Vec::new(),
        };
        let origins = self.bone_origins();
        if origins.len() != model.bones().len() {
            return Vec::new();
        }
        model
            .skeleton_edges()
            .filter_map(|(child, parent)| Some((*origins.get(parent)?, *origins.get(child)?)))
            .collect()
    }

    pub fn render_origin(&self) -> Vec3 {
        // SAFETY: объект статический.
        unsafe { process::read(self.addr() + renderer::VIEW_ORIGIN) }
    }

    /// Модель сущности, которую рендерер обрабатывает сейчас (через её `model_t`).
    ///
    /// Не зависит от поля `+0x4C`: то поле клиент может чистить сразу после
    /// отрисовки, а `model_t` сущности живёт, пока живёт модель.
    pub fn entity_model(&self) -> Option<StudioModel> {
        self.current_entity()?.studio_model()
    }

    /// Рёбра скелета для конкретной модели (позиции — из `m_pbonetransform`).
    pub fn skeleton_lines_for(&self, model: &StudioModel) -> Vec<(Vec3, Vec3)> {
        let base = match self.bone_matrix_base() {
            Some(base) => base,
            None => return Vec::new(),
        };
        let count = model.bones().len().min(MAXSTUDIOBONES);
        let mut origins = Vec::with_capacity(count);
        for i in 0..count {
            // SAFETY: индекс внутри массива матриц и лимита движка.
            let m = unsafe { crate::studio::bone_matrix(base, i) };
            if !m.is_finite() {
                return Vec::new();
            }
            origins.push(m.origin());
        }
        model
            .skeleton_edges()
            .filter_map(|(child, parent)| Some((*origins.get(parent)?, *origins.get(child)?)))
            .collect()
    }

    /// Предельный разброс костей относительно корневой.
    ///
    /// Рост модели игрока CS 1.6 — около 72 юнитов, руки с оружием добавляют ещё
    /// ~30. Скелет, у которого кости разлетелись дальше, посчитан не для этой
    /// модели: так выглядит массив матриц, оставшийся от другого прохода рендера.
    pub const MAX_BONE_EXTENT: f32 = 140.0;

    /// Радиус у камеры, в котором скелет считается «вьюмоделью».
    ///
    /// Матрицы костей оружия в руках считаются в системе координат камеры, и если
    /// движок не пересчитал их для текущей модели, в массиве остаются позиции у
    /// самого глаза — по ним получаются линии из ниоткуда (обычно к своему
    /// оружию). Такие скелеты отбрасываем.
    pub const VIEWMODEL_RADIUS: f32 = 40.0;

    /// Рёбра скелета с проверкой геометрии.
    ///
    /// Отличается от [`StudioRenderer::skeleton_lines_for`] тем, что отбрасывает
    /// заведомо чужие матрицы: разлетевшиеся кости и скелет в системе координат
    /// вьюмодели. Для отрисовки в игре нужен именно этот вариант.
    pub fn skeleton_lines_checked(&self, model: &StudioModel) -> Vec<(Vec3, Vec3)> {
        let base = match self.bone_matrix_base() {
            Some(base) => base,
            None => return Vec::new(),
        };
        let count = model.bones().len().min(MAXSTUDIOBONES);
        if count == 0 {
            return Vec::new();
        }
        let mut origins = Vec::with_capacity(count);
        for i in 0..count {
            // SAFETY: индекс внутри массива матриц и лимита движка.
            let m = unsafe { crate::studio::bone_matrix(base, i) };
            if !m.is_finite() {
                return Vec::new();
            }
            origins.push(m.origin());
        }
        let root = origins[0];
        if origins
            .iter()
            .any(|p| p.distance_to(root) > Self::MAX_BONE_EXTENT)
        {
            return Vec::new();
        }
        if root.distance_to(self.render_origin()) < Self::VIEWMODEL_RADIUS {
            return Vec::new();
        }
        model
            .skeleton_edges()
            .filter_map(|(child, parent)| Some((*origins.get(parent)?, *origins.get(child)?)))
            .collect()
    }

    /// Диагностический снимок: всё, что читается из памяти прямо сейчас.
    ///
    /// Нужен, когда «скелет не видно»: по нему видно, какое именно поле не то —
    /// vtable, указатель на сущность, заголовок модели или массив костей.
    pub fn snapshot(&self) -> Snapshot {
        let addr = self.addr();
        let readable = process::is_readable(addr);
        let vtable = if readable { self.vtable() } else { 0 };
        let current_entity = if readable {
            // SAFETY: объект проверен на читаемость.
            unsafe { process::read::<usize>(addr + renderer::CURRENT_ENTITY) }
        } else {
            0
        };
        let studio_header = if readable {
            // SAFETY: то же.
            unsafe { process::read::<usize>(addr + renderer::STUDIO_HEADER) }
        } else {
            0
        };
        let bone_base = if readable {
            // SAFETY: то же.
            unsafe { process::read::<usize>(addr + renderer::BONE_TRANSFORM) }
        } else {
            0
        };

        let entity = EntityRef::from_raw(current_entity);
        let entity_model = entity.map(|e| e.model() as usize).unwrap_or(0);
        let entity_header = entity
            .and_then(|e| e.studio_model())
            .map(|m| m.header() as *const StudioHeader as usize)
            .unwrap_or(0);
        let entity_state = entity.map(|e| e.model_state()).unwrap_or(-1);

        let header_ptr = if studio_header != 0 { studio_header } else { entity_header };
        let (header_id, header_version, header_bones) = if process::is_readable(header_ptr) {
            // SAFETY: заголовок читаем, поля лежат в начале структуры.
            unsafe {
                let h = header_ptr as *const StudioHeader;
                ((*h).id, (*h).version, (*h).numbones)
            }
        } else {
            (0, 0, 0)
        };
        let first_bone = if bone_base != 0 && process::is_readable(bone_base) {
            // SAFETY: массив матриц читаем.
            unsafe { crate::studio::bone_matrix(bone_base as *const BoneMatrix, 0) }
        } else {
            BoneMatrix::default()
        };

        Snapshot {
            renderer: addr,
            readable,
            vtable,
            initialized: self.is_initialized(),
            current_entity,
            entity_state,
            entity_model,
            entity_header,
            studio_header,
            header_id,
            header_version,
            header_bones,
            bone_base,
            first_bone,
        }
    }

    pub fn view_up(&self) -> Vec3 {
        // SAFETY: объект статический.
        unsafe { process::read(self.addr() + renderer::VIEW_UP) }
    }

    pub fn view_right(&self) -> Vec3 {
        // SAFETY: объект статический.
        unsafe { process::read(self.addr() + renderer::VIEW_RIGHT) }
    }

    pub fn view_forward(&self) -> Vec3 {
        // SAFETY: объект статический.
        unsafe { process::read(self.addr() + renderer::VIEW_NORMAL) }
    }

    /// Камера, которой движок рисовал этот кадр: `origin`, `forward`, `right`, `up`.
    ///
    /// Готова к [`crate::math::Camera::project`] — вместе с [`crate::math::Fov`]
    /// даёт экранные координаты точек мира без обращения к движку.
    pub fn camera(&self) -> crate::math::Camera {
        crate::math::Camera::new(
            self.render_origin(),
            self.view_forward(),
            self.view_right(),
            self.view_up(),
        )
    }

    /// Цвета модели игрока `(top, bottom)` в виде множителей.
    pub fn player_colors(&self) -> (f32, f32) {
        // SAFETY: объект статический.
        unsafe {
            (
                process::read(self.addr() + renderer::PLAYER_TOP_COLOR),
                process::read(self.addr() + renderer::PLAYER_BOTTOM_COLOR),
            )
        }
    }

    /// `m_nRenderAmt` — «прозрачность»/количество проходов рендера.
    pub fn render_amt(&self) -> i32 {
        // SAFETY: объект статический.
        unsafe { process::read(self.addr() + renderer::RENDER_AMT) }
    }

    /// Сколько моделей нарисовано в этом кадре.
    pub fn models_drawn(&self) -> i32 {
        // SAFETY: объект статический.
        unsafe { process::read(self.addr() + renderer::MODELS_DRAWN) }
    }

    /// Загружает модель по имени через движок (`Mod_ForName`).
    ///
    /// Удобно для просмотрщика моделей и отладочных сценариев; в игре модель
    /// должна быть спрекеширована, иначе движок откажется её грузить.
    pub fn load_model(&self, name: &CStr) -> Option<*mut c_void> {
        crate::engine::EngineStudioApi::for_engine()?.mod_for_name(true, name)
    }
}