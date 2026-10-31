//! `engine_studio_api_t` — таблица 48 функций, которую движок передаёт клиенту.
//!
//! Клиент копирует её себе (`client.dll + 0x1B2548`), но обе копии идентичны,
//! поэтому SDK читает движковую (`hw.dll + 0x271B08`): она живёт дольше и не
//! зависит от того, успел ли клиент скопировать.
//!
//! Сигнатуры восстановлены по декомпиляции каждого слота, имена — по смыслу;
//! слоты, чьё HLSDK-имя не подтверждено, названы по смещению (`PTR_0x40`).
//! Подробности — `docs/STUDIO_REVERSE.md`, раздел 3.

use alloc::format;
use alloc::string::{String, ToString};

use core::ffi::{CStr, c_void};

use crate::math::Vec3;
use crate::offsets::{api, hw};
use crate::process;
use crate::studio::StudioModel;

/// Время клиента и индекс сущности, которую рисуем.
#[derive(Debug, Default, Clone, Copy)]
pub struct Times {
    pub view_entity: i32,
    pub time: f32,
    pub old_time: f32,
}

/// Указатель на сущность, которую отдаёт движок.
///
/// Обёртка нужна, чтобы не тащить `unsafe` в каждый доступ к полю и держать
/// смещения в одном месте ([`crate::offsets::entity`]).
#[derive(Debug, Clone, Copy)]
pub struct EntityRef {
    ptr: *mut u8,
}

impl EntityRef {
    /// # Safety
    /// `ptr` должен указывать на сущность движка.
    pub unsafe fn from_ptr(ptr: *mut c_void) -> Option<Self> {
        (!ptr.is_null()).then_some(Self { ptr: ptr as *mut u8 })
    }

    /// Сущность по «сырому» адресу; `None`, если адрес не читается.
    ///
    /// В отличие от [`EntityRef::from_ptr`] проверяет память: адреса приходят из
    /// игры, и после ухода с карты там может быть мусор.
    pub fn from_raw(addr: usize) -> Option<Self> {
        process::is_readable(addr).then_some(Self { ptr: addr as *mut u8 })
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr as *mut c_void
    }

    pub fn addr(&self) -> usize {
        self.ptr as usize
    }

    /// Позиция сущности в мире.
    pub fn origin(&self) -> Vec3 {
        // SAFETY: указатель проверен при создании.
        unsafe { process::read(self.addr() + crate::offsets::entity::ORIGIN) }
    }

    /// `model_t *`.
    pub fn model(&self) -> *mut c_void {
        // SAFETY: указатель проверен при создании.
        unsafe { process::read(self.addr() + crate::offsets::entity::MODEL) }
    }

    /// Тип/состояние модели (17 — удалённый игрок, 12 — смена скина).
    pub fn model_state(&self) -> i32 {
        // SAFETY: указатель проверен при создании.
        unsafe { process::read(self.addr() + crate::offsets::entity::MODEL_STATE) }
    }

    /// Упакованные цвета игрока: `(top, bottom)`.
    pub fn player_colors(&self) -> (u8, u8) {
        // SAFETY: указатель проверен при создании.
        let packed: i32 = unsafe { process::read(self.addr() + crate::offsets::entity::PLAYER_COLORS) };
        ((packed & 0xFF) as u8, ((packed >> 8) & 0xFF) as u8)
    }

    /// Указатель на `player_info_t` (поле `+0xB60` сущности).
    pub fn player_info(&self) -> usize {
        self.addr() + crate::offsets::entity::PLAYER_INFO
    }

    /// Номер игрока (слот клиента) из `player_info_t`.
    ///
    /// Основной путь — поле [`crate::offsets::entity::INDEX`] (`+0x00`), в нём
    /// лежит номер слота: у ботов и игроков он совпадал с их моделью и команда
    /// находилась правильно. `player_info_t` первым полем хранит не номер,
    /// поэтому используется только как запасной вариант.
    pub fn player_index(&self) -> i32 {
        // SAFETY: указатель на сущность проверен при создании.
        let direct: i32 = unsafe { process::read(self.addr() + crate::offsets::entity::INDEX) };
        if (0..64).contains(&direct) {
            return direct;
        }
        // SAFETY: указатель на сущность проверен при создании.
        let fallback: i32 = unsafe { process::read(self.player_info()) };
        if (0..64).contains(&fallback) { fallback } else { -1 }
    }

    /// Имя игрока из `player_info_t`.
    ///
    /// `player_info_t` — блок `0x30` байт по `+0xB60`: декомпиляция
    /// `client+0x05F760` копирует его целиком (`FUN_1013cf40(entity + 0xB60, .., 0x30)`),
    /// а `name[MAXPLAYERNAME]` лежит в его начале, то есть по `+0xB60`.
    pub fn player_name(&self) -> String {
        // SAFETY: `read_cstr` сам проверяет читаемость страниц.
        process::read_cstr(self.player_info(), 32).unwrap_or_default()
    }
    /// Номер модели тела (`modelindex`).
    pub fn model_index(&self) -> i32 {
        // SAFETY: указатель проверен при создании.
        unsafe { process::read(self.addr() + crate::offsets::entity::MODEL_INDEX) }
    }

    /// Номер модели оружия в руках (`weaponmodel`), 0 — оружия нет.
    pub fn weapon_model_index(&self) -> i32 {
        // SAFETY: указатель проверен при создании.
        let index: i32 = unsafe { process::read(self.addr() + crate::offsets::entity::WEAPON_MODEL) };
        if (0..8192).contains(&index) { index } else { 0 }
    }

    /// Имя модели оружия в руках, например `w_usp.mdl`.
    pub fn weapon_model_name(&self) -> Option<String> {
        let index = self.weapon_model_index();
        if index == 0 {
            return None;
        }
        EngineStudioApi::for_engine()?.model_name_by_index(index)
    }
    /// Флаги рендера (бит 1 — «не рисовать»).
    pub fn render_flags(&self) -> u32 {
        // SAFETY: указатель проверен при создании.
        unsafe { process::read(self.addr() + crate::offsets::entity::RENDER_FLAGS) }
    }

    /// Студийная модель сущности, если модель уже загружена.
    pub fn studio_model(&self) -> Option<StudioModel> {
        // SAFETY: model_t проверяется в `model_extradata`.
        unsafe { crate::studio::model_extradata(self.model()) }
    }

    /// Сырое чтение `i32` по смещению от начала сущности — для диагностики
    /// полей, у которых ещё нет имени.
    pub fn raw_i32(&self, offset: usize) -> Option<i32> {
        let addr = self.addr() + offset;
        process::is_readable(addr).then(|| {
            // SAFETY: адрес проверен на читаемость.
            unsafe { process::read(addr) }
        })
    }
}

/// Выполняет консольную команду как из консоли: `"+jump"`, `"-jump"`,
/// `"say hello"`, `"cl_crosshair_color 0 255 0"`.
///
/// Команда не выполняется мгновенно: она уходит в командный буфер движка
/// (`Cbuf_AddText`, `hw+0x031E80`) и разбирается на ближайшем кадре — как если
/// бы её напечатали в консоли. Поэтому `+jump` нужно добавлять каждый кадр,
/// а снимать — парным `-jump`.
///
/// `Cbuf_Execute` режет буфер по `'\n'` и `';'` (проверено по `Cbuf_AddText`
/// → `0x10018060` → `0x10017fd0` в Ghidra), так что `"-jump;+jump"` — это две
/// команды. Но обе они попадают в **один** проход командного буфера, то есть в
/// один кадр, и движок видит кнопку всё ещё зажатой (`pmove->oldbuttons &
/// IN_JUMP`) — прыжка не будет. Чтобы команды разъехались по кадрам, ставьте их
/// через `Scheduler::after_frames`.
///
/// Возвращает `false`, если движок ещё не загружен или строка содержит
/// нулевой байт.
pub fn run_command(command: &str) -> bool {
    let Some(base) = process::hw_base() else {
        return false;
    };
    let line = format!("{}\n", command.trim_end_matches(['\r', '\n']));
    let Ok(text) = alloc::ffi::CString::new(line) else {
        return false;
    };
    // SAFETY: `hw+0x031E80` — `Cbuf_AddText(const char *)`, cdecl; строка живёт
    // до возврата, дальше движок копирует её в свой буфер.
    let add_text: extern "C" fn(*const i8) -> i32 =
        unsafe { core::mem::transmute(base + hw::CBUF_ADD_TEXT) };
    add_text(text.as_ptr());
    true
}

/// Индекс модели оружия в руках у рисуемого игрока.
///
/// `player_info` — второй аргумент `StudioDrawPlayer`. Движок сам кладёт сюда
/// индекс модели оружия ([`crate::offsets::player_info::WEAPON_MODEL`]) и рисует
/// её вторым проходом — значит это ровно то оружие, которое видно в руках,
/// а не устаревшая копия в сущности.
///
/// `None`, если оружия нет (`0`) или указатель пуст.
pub fn player_weapon_model(player_info: *const c_void) -> Option<i32> {
    if player_info.is_null() {
        return None;
    }
    // SAFETY: указатель пришёл от движка; читаем одно int-поле его структуры.
    let index: i32 = unsafe {
        process::read((player_info as usize) + crate::offsets::player_info::WEAPON_MODEL)
    };
    (index > 0 && index < 0x4000).then_some(index)
}

/// Номер игрока, которого движок рисует: `player_info + 0x04`.
///
/// `None`, если указатель пуст или номер вне диапазона.
pub fn player_of_draw(player_info: *const c_void) -> Option<i32> {
    if player_info.is_null() {
        return None;
    }
    // SAFETY: указатель пришёл от движка; читаем одно int-поле его структуры.
    let index: i32 =
        unsafe { process::read((player_info as usize) + crate::offsets::player_info::INDEX) };
    (0..2048).contains(&index).then_some(index)
}

/// Таблица функций движка.
#[repr(C)]
pub struct EngineStudioApi {
    slots: [*const c_void; Self::SLOT_COUNT],
}

impl core::fmt::Debug for EngineStudioApi {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EngineStudioApi")
            .field("slots", &self.slots.len())
            .field("first", &self.slots.first().map(|p| *p as usize))
            .finish()
    }
}

impl EngineStudioApi {
    /// Количество слотов в таблице.
    pub const SLOT_COUNT: usize = 48;

    /// # Safety
    /// `ptr` должен указывать на таблицу из `hw.dll`.
    pub unsafe fn from_ptr(ptr: *const Self) -> Option<&'static Self> {
        if ptr.is_null() {
            return None;
        }
        // SAFETY: контракт функции.
        let api = unsafe { &*ptr };
        // Первые три слота обязаны быть кодом движка, иначе смещение уехало.
        let base = process::hw_base()?;
        let ok = api.slots[..3].iter().all(|&p| process::is_code(p as usize, base));
        ok.then_some(api)
    }

    /// Таблица, которую движок держит у себя.
    pub fn for_engine() -> Option<&'static Self> {
        let base = process::hw_base()?;
        // SAFETY: адрес — статическая таблица движка.
        unsafe { Self::from_ptr((base + hw::ENGINE_STUDIO_API) as *const Self) }
    }

    /// Копия таблицы внутри клиента (полезно, если движковую подменили).
    pub fn client_copy() -> Option<&'static Self> {
        let base = process::client_base()?;
        // SAFETY: адрес — статическая копия в клиенте.
        unsafe { Self::from_ptr((base + crate::offsets::client::ENGINE_STUDIO_API_COPY) as *const Self) }
    }

    /// Сырой указатель слота по байтовому смещению.
    pub fn slot(&self, offset: usize) -> Option<*const c_void> {
        let index = offset / core::mem::size_of::<usize>();
        let ptr = *self.slots.get(index)?;
        (!ptr.is_null()).then_some(ptr)
    }

    /// Приводит слот к типу функции.
    fn func<F: Copy>(&self, offset: usize) -> Option<F> {
        let ptr = self.slot(offset)?;
        if core::mem::size_of::<F>() != core::mem::size_of::<usize>() {
            return None;
        }
        // SAFETY: указатель на код с известной сигнатурой (см. offsets::api).
        Some(unsafe { core::mem::transmute_copy(&ptr) })
    }

    /// `studiohdr_t *Mod_Extradata(model_t *)` — заголовок модели.
    pub fn extradata(&self, model: *mut c_void) -> Option<StudioModel> {
        let f: unsafe extern "C" fn(*mut c_void) -> *const c_void = self.func(api::MOD_EXTRADATA)?;
        // SAFETY: сигнатура слота, model проверяется на null внутри.
        if model.is_null() {
            return None;
        }
        let hdr = unsafe { f(model) };
        // SAFETY: движок возвращает валидный заголовок либо null.
        unsafe { StudioModel::from_ptr(hdr.cast()) }
    }

    /// Сущность, которую движок рисует сейчас.
    pub fn current_entity(&self) -> Option<EntityRef> {
        let f: unsafe extern "C" fn() -> *mut c_void = self.func(api::GET_CURRENT_ENTITY)?;
        // SAFETY: сигнатура слота.
        unsafe { EntityRef::from_ptr(f()) }
    }

    /// Сущность по индексу в списке клиента.
    pub fn entity_by_index(&self, index: i32) -> Option<EntityRef> {
        let f: unsafe extern "C" fn(i32) -> *mut c_void = self.func(api::GET_ENTITY_BY_INDEX)?;
        // SAFETY: сигнатура слота; индекс проверяется движком.
        unsafe { EntityRef::from_ptr(f(index)) }
    }

    /// Индекс сущности в списке клиента — поиском по указателю.
    ///
    /// Поле `index` внутри `cl_entity_t` у Gold Client лежит не там, где в HLSDK,
    /// а `GetEntityByIndex` — официальный способ движка отдать сущность по
    /// номеру, поэтому индекс надёжнее получить сравнением адресов. Список
    /// обходим один раз на сущность (результат стоит кэшировать у себя).
    /// Ищет номер сущности по её позиции и модели.
    ///
    /// `GetEntityByIndex` возвращает **лёгкую** `cl_entity_t` (`0x154` байта),
    /// а рендерер работает с отдельной, более крупной «рендер-сущностью», поэтому
    /// сравнение адресов не срабатывает. Совпадают у них позиция и модель — по
    /// ним и ищем.
    pub fn entity_index_by_origin(&self, origin: Vec3, model_name: &str) -> Option<i32> {
        let mut best: Option<(i32, f32)> = None;
        for index in 1..=Self::MAX_EDICTS {
            let Some(entity) = self.entity_by_index(index) else {
                continue;
            };
            let delta = entity.origin() - origin;
            let distance = delta.dot(delta);
            if !distance.is_finite() || distance > 4.0 {
                continue;
            }
            let matches_model = entity
                .studio_model()
                .is_some_and(|m| m.name().eq_ignore_ascii_case(model_name));
            if !matches_model && distance > 0.01 {
                continue;
            }
            if best.is_none_or(|(_, d)| distance < d) {
                best = Some((index, distance));
            }
        }
        best.map(|(index, _)| index)
    }

    pub fn entity_index(&self, entity: EntityRef) -> Option<i32> {
        let target = entity.addr();
        for index in 0..=Self::MAX_EDICTS {
            if self.entity_by_index(index).is_some_and(|e| e.addr() == target) {
                return Some(index);
            }
        }
        None
    }

    /// Сколько слотов сущностей перебирать в [`EngineStudioApi::entity_index`]:
    /// игроки занимают первые 33 слота, остальное — обычные сущности.
    pub const MAX_EDICTS: i32 = 64;

    /// Время клиента и индекс сущности.
    pub fn times(&self) -> Option<Times> {
        let f: unsafe extern "C" fn(*mut i32, *mut f32, *mut f32) = self.func(api::GET_TIMES)?;
        let mut t = Times::default();
        // SAFETY: сигнатура слота, передаём валидные указатели на локальные.
        unsafe { f(&mut t.view_entity, &mut t.time, &mut t.old_time) };
        Some(t)
    }

    /// Базис камеры: `(origin, up, right, forward)`.
    pub fn view_info(&self) -> Option<(Vec3, Vec3, Vec3, Vec3)> {
        let f: unsafe extern "C" fn(*mut Vec3, *mut Vec3, *mut Vec3, *mut Vec3) =
            self.func(api::GET_VIEW_INFO)?;
        let (mut origin, mut up, mut right, mut forward) =
            (Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, Vec3::ZERO);
        // SAFETY: сигнатура слота.
        unsafe { f(&mut origin, &mut up, &mut right, &mut forward) };
        Some((origin, up, right, forward))
    }

    /// Цвета игрока `(top, bottom)` из настроек рендера.
    pub fn player_colors(&self) -> Option<(f32, f32)> {
        let f: unsafe extern "C" fn(*mut f32, *mut f32) = self.func(api::GET_PLAYER_COLORS)?;
        let (mut top, mut bottom) = (0.0, 0.0);
        // SAFETY: сигнатура слота.
        unsafe { f(&mut top, &mut bottom) };
        Some((top, bottom))
    }

    /// `R_StudioCheckBBox` — стоит ли вообще рисовать текущую модель.
    pub fn check_bbox(&self) -> Option<i32> {
        let f: unsafe extern "C" fn() -> i32 = self.func(api::STUDIO_CHECK_BBOX)?;
        // SAFETY: сигнатура слота.
        Some(unsafe { f() })
    }

    /// `GetEntityState(index)` — состояние сущности из сетевого массива движка
    /// (`hw.dll + 0x2F90A18`, шаг [`crate::offsets::entity_state::STRIDE`]).
    ///
    /// Именно здесь лежат поля, которых нет в `cl_entity_t`: например модель
    /// оружия в руках игрока (`weaponmodel`).
    pub fn entity_state(&self, index: i32) -> Option<*const u8> {
        let f: unsafe extern "C" fn(i32) -> *const u8 = self.func(api::GET_ENTITY_STATE)?;
        // SAFETY: сигнатура слота; индекс проверяет движок.
        let ptr = unsafe { f(index) };
        (!ptr.is_null()).then_some(ptr)
    }

    /// `Mod_ForIndex(index)` — `model_t *` по индексу из массива моделей.
    pub fn model_by_index(&self, index: i32) -> Option<*mut c_void> {
        let f: unsafe extern "C" fn(i32) -> *mut c_void = self.func(api::MOD_FOR_INDEX)?;
        // SAFETY: сигнатура слота; неверный индекс движок отсекает сам.
        let ptr = unsafe { f(index) };
        (!ptr.is_null()).then_some(ptr)
    }

    /// Имя модели по индексу (`terror\terror.mdl`, `v_m4a1.mdl`, ...).
    ///
    /// Заголовок читается **напрямую** из `model_t` (`+0x184`), без вызова
    /// `Mod_Extradata`: тот при ошибке зовёт `Host_Error` («Mod_Extradata:
    /// caching failed») и роняет игру. Если модель ещё не разобрана движком,
    /// `extradata` пуст — тогда имени просто нет.
    pub fn model_name_by_index(&self, index: i32) -> Option<String> {
        let model = self.model_by_index(index)?;
        // SAFETY: `model_extradata` проверяет читаемость и магию заголовка.
        let studio = unsafe { crate::studio::model_extradata(model) }?;
        Some(studio.name().to_string())
    }

    /// `Mod_ForName(int load, const char *name)`.
    ///
    /// Порядок аргументов — как в бинарнике Gold Client, **не** как в HLSDK.
    pub fn mod_for_name(&self, load: bool, name: &CStr) -> Option<*mut c_void> {
        let f: unsafe extern "C" fn(i32, *const c_void) -> *mut c_void = self.func(api::MOD_FOR_NAME)?;
        // SAFETY: сигнатура слота, `name` живёт дольше вызова.
        let model = unsafe { f(load as i32, name.as_ptr().cast()) };
        (!model.is_null()).then_some(model)
    }
}
