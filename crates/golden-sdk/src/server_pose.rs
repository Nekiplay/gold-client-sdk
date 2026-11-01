//! Состояние анимации игрока и расчёт костей движком.
//!
//! Низкий уровень: чтение `sequence`/`frame`/`angles`/`origin`/контроллеров
//! из записи, которую движок отдаёт в `StudioDrawPlayer`, и прямой вызов
//! `SV_StudioSetupBones` (`hw.dll + 0x92C10`) — того же расчёта, которым
//! движок строит кости для отрисовки и трассировки.
//!
//! Понадобиться это может, когда нужно посчитать позу не в момент отрисовки, а
//! по сохранённому состоянию (например, восстановить кадр задним числом).
//!
//! # Важно: попадание считается не по этому расчёту
//!
//! Построитель hull (`SV_HullForStudioModel`, `hw.dll + 0x93440`) вызывает
//! расчёт костей через таблицу движка и передаёт ему то состояние, которое
//! получил сам; на клиенте это клиентский расчёт, и матрицы он кладёт в тот же
//! буфер, из которого клиент рисует тело. Поэтому хитбоксы, по которым игра
//! трассирует выстрел, — это поза нарисованной модели. Расчёт по состоянию из
//! пакета даёт другую позу (в среднем на 11 единиц, на руках до 27), и боксы по
//! ней на тело не ложатся.
//!
//! # Как это работает
//!
//! `SV_StudioSetupBones` — обычный `cdecl`, аргументы в стеке (проверено по
//! прологу `PUSH EBP / MOV EBP,ESP / SUB ESP,0x250`, аргументы читаются как
//! `[EBP+8]`, `[EBP+0x10]` и далее). Заголовок модели функция берёт не из
//! аргумента, а из глобального кэша `hw + 0x29F4888`; этот кэш заполняет
//! вызывающий (`SV_HullForStudioModel` пишет туда `model_t->cache.data`,
//! `+0x184`). Поэтому перед вызовом кэш нужно выставить самим.
//!
//! Матрицы функция кладёт в общий буфер [`hw::TRACE_BONES`]: 12 float на кость,
//! три строки по четыре. Буфер общий с отрисовкой, поэтому расчёт в середине
//! кадра затирает позу следующей модели — считать можно только там, где
//! отрисовка уже закончилась (конец 3D-сцены).

use core::ffi::c_void;

use crate::math::Vec3;
use crate::offsets::{hw, model, player_state};
use crate::process;

/// Состояние анимации игрока — вход серверного расчёта костей.
///
/// Ровно те поля, которые `SV_StudioSetupBones` берёт у сущности. Снятое в
/// перехвате `StudioDrawPlayer` состояние остаётся годным и в конце кадра:
/// это данные из последнего пакета сервера, а не результат отрисовки.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AnimState {
    /// Номер последовательности (`entity_state_t::sequence`).
    pub sequence: i32,
    /// Кадр анимации вместе с дробной частью.
    pub frame: f32,
    /// Углы поворота тела в градусах.
    pub angles: Vec3,
    /// Позиция тела в мире.
    pub origin: Vec3,
    /// Значения контроллеров костей — четыре байта из состояния сущности.
    pub controllers: [u8; 4],
}

impl AnimState {
    /// Читает состояние из записи, которую движок отдаёт в `StudioDrawPlayer`.
    ///
    /// # Safety
    /// `info` — указатель на запись игрока, переданный движком в перехват
    /// `StudioDrawPlayer`; в другой момент он может быть уже освобождён.
    pub unsafe fn from_player_info(info: *mut c_void) -> Option<Self> {
        let base = info as usize;
        if !process::is_readable(base) {
            return None;
        }
        // SAFETY: указатель пришёл от движка и проверен на читаемость; смещения
        // полей сверены дампом живой записи (см. `offsets::player_state`).
        Some(unsafe {
            Self {
                origin: process::read(base + player_state::ORIGIN),
                angles: process::read(base + player_state::ANGLES),
                sequence: process::read(base + player_state::SEQUENCE),
                frame: process::read(base + player_state::FRAME),
                controllers: process::read(base + player_state::CONTROLLERS),
            }
        })
    }

    /// Похоже ли состояние на живое: номер последовательности в разумных
    /// пределах, кадр конечен, позиция не «мусор».
    pub fn is_plausible(&self, num_sequences: i32) -> bool {
        (0..num_sequences.max(1)).contains(&self.sequence)
            && self.frame.is_finite()
            && (0.0..=4096.0).contains(&self.frame)
            && self.origin.is_finite()
            && self.angles.is_finite()
    }
}

/// Считает матрицы костей так, как это делает сервер, и кладёт их в
/// [`hw::TRACE_BONES`].
///
/// Возвращает `true`, если функция движка отработала: после этого буфер
/// [`hw::TRACE_BONES`] содержит позу этого игрока.
///
/// # Safety
/// `model` — `model_t *` живой модели игрока (студийная модель уже загружена).
/// Вызывать только когда трассировка выстрела не идёт: функция пишет в общий
/// буфер движка. Конец 3D-сцены — подходящий момент.
pub unsafe fn setup_bones(model: *mut c_void, state: &AnimState) -> bool {
    let Some(base) = process::hw_base() else {
        return false;
    };
    let model_addr = model as usize;
    if model_addr == 0 || !process::is_readable(model_addr) {
        return false;
    }
    // `model_t->cache.data` — загруженный `studiohdr_t`.
    // SAFETY: указатель на модель проверен на читаемость.
    let header: *const c_void = unsafe { process::read(model_addr + model::EXTRADATA) };
    let header_addr = header as usize;
    if header_addr == 0 || !process::is_readable(header_addr) {
        return false;
    }
    // Буфер матриц должен быть отображён целиком, иначе движок запишет за его
    // пределы. Смещение берётся от базы модуля (в Ghidra тот же буфер —
    // `0x129F2A58`).
    let bones = base + hw::TRACE_BONES;
    let last = bones + crate::studio::MAXSTUDIOBONES * core::mem::size_of::<crate::studio::BoneMatrix>() - 1;
    if !process::is_readable(bones) || !process::is_readable(last) {
        return false;
    }
    let setup_at = base + hw::SV_STUDIO_SETUP_BONES;
    if !process::is_readable(setup_at) {
        return false;
    }
    // Заголовок модель функция берёт из своего кэша — выставляем его так же,
    // как это делает `SV_HullForStudioModel`.
    // SAFETY: адрес — глобал движка внутри образа `hw.dll`.
    unsafe { process::write(base + hw::STUDIO_HEADER_CACHE, header_addr) };

    let angles = [state.angles.x, state.angles.y, state.angles.z];
    let origin = [state.origin.x, state.origin.y, state.origin.z];
    // Куда движок складывает поправки контроллеров: восемь float, для игрока
    // заполняются нулями, если контроллеры не заданы.
    let mut adjustments = [0.0f32; 8];

    // SAFETY: сигнатура снята с пролога функции: `cdecl`,
    // `(model_t *, float frame, int sequence, const float angles[3],
    // const float origin[3], float boneadj[8], const unsigned char controller[4],
    // int bone)`. `-1` — посчитать все кости модели.
    let setup: unsafe extern "C" fn(
        *mut c_void,
        f32,
        i32,
        *const f32,
        *const f32,
        *mut f32,
        *const u8,
        i32,
    ) = unsafe { core::mem::transmute(setup_at) };
    unsafe {
        setup(
            model,
            state.frame,
            state.sequence,
            angles.as_ptr(),
            origin.as_ptr(),
            adjustments.as_mut_ptr(),
            state.controllers.as_ptr(),
            -1,
        );
    }
    true
}