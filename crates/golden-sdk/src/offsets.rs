//! Смещения и адреса Gold Client (CS 1.6), полученные статическим анализом.
//!
//! Все адреса — **RVA** (offset от базы модуля). База обоих модулей одинаковая:
//! `IMAGE_BASE = 0x10000000`, поэтому абсолютный адрес = `base + RVA`.
//!
//! Источник истины: `docs/STUDIO_REVERSE.md`. При обновлении бинарников сверять
//! через [`crate::validate`] — там проверки, которые ловят уехавшие смещения.

/// Preferred база образа у `hw.dll` и `client.dll`.
pub const IMAGE_BASE: usize = 0x1000_0000;

/// Идентификатор сборки, к которой относятся смещения в этом файле.
/// Менять вместе со смещениями, если сборка другая.
pub const BUILD: &str = "goldclient-cs16-2026";

/// `hw.dll` — движок.
pub mod hw {
    /// `r_studio_interface_t **g_pStudioInterface` — указатель, который движок
    /// отдаёт клиенту и читает каждый кадр. Подмена этого поля = подмена рендерера.
    pub const P_STUDIO_INTERFACE: usize = 0x27_1B04;

    /// `engine_studio_api_t g_EngineStudioAPI` — таблица 48 функций, которую движок
    /// передаёт клиенту при инициализации.
    pub const ENGINE_STUDIO_API: usize = 0x27_1B08;

    /// `r_studio_interface_t g_DefaultStudioInterface` — дефолтный (движковый)
    /// рендерер: `{ 1, 0x1009C1E0, 0x1009BCD0 }`.
    pub const DEFAULT_STUDIO_INTERFACE: usize = 0x27_1AF0;

    /// Функция, устанавливающая интерфейс студийного рендера (вызывается при
    /// инициализации рендера и после загрузки клиента).
    pub const STUDIO_INTERFACE_SETUP: usize = 0x35_FA0;

    /// Указатель на `HUD_GetStudioModelInterface` из `client.dll`.
    pub const P_CLIENT_STUDIO_ENTRY: usize = 0x43_DE4C;

    /// Флаг «клиент загружен и готов».
    pub const CLIENT_READY: usize = 0x43_DB9C;

    /// `cl_entity_t *g_pCurrentEntity`-подобный указатель: сущность, которую
    /// сейчас рисует движок (её же отдаёт `engine_studio_api_t::GetCurrentEntity`).
    pub const P_CURRENT_ENTITY: usize = 0x78_E470;

    /// `int cl_parsecount` — то, что отдаёт слот `GetParsecountPtr`.
    pub const PARSECOUNT: usize = 0xF8_FE30;

    // --- функции движка, полезные напрямую (cdecl) ---

    /// `model_t *Mod_ForName(int load, const char *name)` — внимание на порядок
    /// аргументов, в HLSDK он обратный.
    pub const MOD_FOR_NAME: usize = 0x9D_870;

    /// `studiohdr_t *Mod_Extradata(model_t *m)`.
    pub const MOD_EXTRADATA: usize = 0x69_D40;

    /// `model_t *Mod_ForIndex(int index)`.
    pub const MOD_FOR_INDEX: usize = 0x48_5B0;

    /// `void R_StudioSetupModel(int modelindex, void **ppsubmodel, void **ppmodel)`.
    pub const R_STUDIO_SETUP_MODEL: usize = 0x98_E10;

    /// `int R_StudioCheckBBox(void)`.
    pub const R_STUDIO_CHECK_BBOX: usize = 0x94_3C0;

    /// `void StudioSetupLighting(...)` — слот api `+0x58`.
    pub const STUDIO_SETUP_LIGHTING: usize = 0x99_560;

    /// `void SV_StudioSetupBones(...)` — серверная копия расчёта костей,
    /// удобный эталон для сверки математики.
    pub const SV_STUDIO_SETUP_BONES: usize = 0x92_C70;
}

/// `client.dll` — игровой клиент (CS).
pub mod client {
    /// Экспорт `HUD_GetStudioModelInterface` (тонкая обёртка).
    pub const HUD_GET_STUDIO_MODEL_INTERFACE: usize = 0xBA_0F0;

    /// Реальная реализация `HUD_GetStudioModelInterface(version, &iface, pstudio)`.
    pub const STUDIO_INTERFACE_INIT: usize = 0x60_AE0;

    /// `int Init(void)` — инициализация рендерера и загрузка 10 моделей игроков.
    pub const STUDIO_RENDERER_INIT: usize = 0x60_AB0;

    /// `r_studio_interface_t g_StudioInterface` — то, что клиент отдаёт движку.
    pub const STUDIO_INTERFACE: usize = 0x1966_B0;

    /// Копия `engine_studio_api_t` (0xC0 байт) внутри клиента.
    pub const ENGINE_STUDIO_API_COPY: usize = 0x1B25_48;

    /// Объект `CStudioModelRenderer` (статический).
    pub const STUDIO_RENDERER: usize = 0x1F51_C8;

    /// Vtable `CStudioModelRenderer` (26 слотов).
    pub const STUDIO_RENDERER_VTABLE: usize = 0x168F_94;

    /// `extra_player_info_t g_PlayerExtraInfo[]` — массив по индексу игрока,
    /// заполняется обработчиком сообщения `TeamInfo` (`MsgFunc_TeamInfo`,
    /// `client+0x0B9BE0`). По нему команда известна и на кастомных моделях.
    pub const PLAYER_EXTRA_INFO: usize = 0x22_4EBC;

    /// Размер элемента `g_PlayerExtraInfo` (`0xC0` — из `imul` в `MsgFunc_TeamInfo`).
    pub const PLAYER_EXTRA_STRIDE: usize = 0xC0;

    /// Номер команды в элементе: `1` — террористы, `2` — спецназ, `3` — наблюдатель.
    pub const PLAYER_EXTRA_TEAM: usize = 0x00;

    /// Имя команды из сетевого сообщения: `"CT"`, `"TERRORIST"`, `"SPECTATOR"`.
    pub const PLAYER_EXTRA_TEAMNAME: usize = 0x04;

    /// Обработчик сообщения `TeamInfo` (пишет `g_PlayerExtraInfo`).
    pub const MSG_TEAM_INFO: usize = 0x0B_9BE0;

    // --- слоты vtable CStudioModelRenderer ---
    pub const VT_INIT: usize = 0x01;
    pub const VT_CONSTRUCT: usize = 0x02;
    pub const VT_STUDIO_DRAW_MODEL: usize = 0x03;
    pub const VT_STUDIO_DRAW_PLAYER: usize = 0x04;
    pub const VT_STUDIO_SETUP_BONES: usize = 0x07;
    pub const VT_STUDIO_RENDER_MODEL: usize = 0x08;
    pub const VT_SET_PLAYER_MODEL: usize = 0x0A;
}

/// Смещения полей объекта `CStudioModelRenderer` (thiscall, `this` = `client+0x1F51C8`).
pub mod renderer {
    pub const VTABLE: usize = 0x00;
    pub const CL_TIME: usize = 0x08;
    pub const CL_OLD_TIME: usize = 0x10;
    pub const FRAME_COUNT: usize = 0x20;
    pub const CURRENT_ENTITY: usize = 0x30;
    pub const IS_PLAYER: usize = 0x34;
    pub const RENDER_MODEL: usize = 0x38;
    pub const RENDER_AMT: usize = 0x40;
    pub const STUDIO_HEADER: usize = 0x4C;
    pub const TOP_COLOR: usize = 0x58;
    pub const BOTTOM_COLOR: usize = 0x5C;
    /// Указатель на функцию `StudioDrawPlayer` (ставится при инициализации).
    pub const DRAW_PLAYER_FN: usize = 0x6C;
    pub const PLAYER_TOP_COLOR: usize = 0x406C;
    pub const PLAYER_BOTTOM_COLOR: usize = 0x4070;
    pub const VIEW_UP: usize = 0x4074;
    pub const VIEW_RIGHT: usize = 0x4080;
    pub const VIEW_NORMAL: usize = 0x408C;
    pub const VIEW_ORIGIN: usize = 0x4098;
    pub const MODELS_DRAWN: usize = 0x40A8;
    /// `m_pbonetransform`: массив матриц 3x4 по костям, см. [`crate::studio::MAXSTUDIOBONES`].
    pub const BONE_TRANSFORM: usize = 0x40B4;
    /// `m_plighttransform` — то же для источников света.
    pub const LIGHT_TRANSFORM: usize = 0x40B8;
}

/// Смещения слотов `engine_studio_api_t` (байтовые offset от начала таблицы).
///
/// Восстановлены по декомпиляции `hw.dll`; имена — по смыслу, там где HLSDK-имя
/// не подтверждено, оно помечено в `docs/STUDIO_REVERSE.md`.
pub mod api {
    pub const MEM_CALLOC: usize = 0x00;
    pub const MEM_FREE: usize = 0x04;
    pub const COM_LOAD_FILE: usize = 0x08;
    pub const MOD_SET_FLAGS: usize = 0x0C;
    pub const MOD_EXTRADATA: usize = 0x10;
    pub const MOD_FOR_INDEX: usize = 0x14;
    pub const GET_CURRENT_ENTITY: usize = 0x18;
    pub const GET_ENTITY_STATE: usize = 0x1C;
    pub const GET_ENTITY_BY_INDEX: usize = 0x20;
    pub const GET_PARSECOUNT_PTR: usize = 0x24;
    pub const GET_TIMES: usize = 0x28;
    pub const MOD_UNLOAD_MODEL: usize = 0x2C;
    pub const GET_VIEW_INFO: usize = 0x30;
    pub const GET_RENDER_FLAGS_PTR: usize = 0x34;
    pub const GET_LIGHT_POINTERS: usize = 0x38;
    pub const GET_PLAYER_COLORS: usize = 0x3C;
    pub const GET_PTR_40: usize = 0x40;
    pub const GET_PTR_44: usize = 0x44;
    pub const RETURN_ZERO: usize = 0x48;
    pub const GET_PTR_4C: usize = 0x4C;
    pub const SETUP_MODEL: usize = 0x50;
    pub const STUDIO_CHECK_BBOX: usize = 0x54;
    pub const STUDIO_SETUP_LIGHTING: usize = 0x58;
    pub const STUDIO_SETUP_DLIGHTS: usize = 0x5C;
    pub const STUDIO_SETUP_LIGHT_TRANSFORM: usize = 0x60;
    pub const STUDIO_DRAW_GLOW_SHELL: usize = 0x64;
    pub const STUDIO_DRAW_MODEL_INTERNAL: usize = 0x68;
    pub const STUDIO_DRAW_MESHES: usize = 0x6C;
    pub const STUDIO_DRAW_MESH: usize = 0x70;
    pub const STUDIO_DRAW_TEXTURES: usize = 0x74;
    pub const SET_PLAYER_MODEL_COLORS: usize = 0x78;
    pub const GET_PLAYER_MODEL: usize = 0x7C;
    pub const STUDIO_RENDER_FINAL: usize = 0x80;
    pub const GET_84: usize = 0x84;
    pub const SET_STUDIO_HEADER: usize = 0x88;
    pub const SET_RENDER_HEADER: usize = 0x8C;
    pub const SET_SUBMODEL: usize = 0x90;
    pub const STUDIO_INIT_LIGHT: usize = 0x94;
    pub const STUDIO_SET_TEXTURE_STATE: usize = 0x98;
    pub const STUDIO_COPY_VIEW_ORIGIN: usize = 0x9C;
    pub const GET_RENDER_MODE: usize = 0xA0;
    pub const STUDIO_BIND_TEXTURE: usize = 0xA4;
    pub const STUDIO_SET_TEXTURE_MODE: usize = 0xA8;
    pub const STUDIO_SET_RENDER_AMT: usize = 0xAC;
    pub const RETURN_ZERO_2: usize = 0xB0;
    pub const STUDIO_DRAW_MODEL_BY_INDEX: usize = 0xB4;
    pub const SET_RENDER_TARGET: usize = 0xB8;
    /// `model_t *Mod_ForName(int load, const char *name)` — порядок как в бинарнике.
    pub const MOD_FOR_NAME: usize = 0xBC;
}

/// Смещения полей сущности, которую отдаёт `engine_studio_api_t`.
///
/// Подтверждены кодом клиента и движка; структура больше классического
/// `cl_entity_t` (0x154), поэтому не описываем её целиком.
pub mod entity {
    /// Цвета игрока, упакованные в один int: `[0..8]` — top, `[8..16]` — bottom.
    pub const PLAYER_COLORS: usize = 0x2E4;
    /// Счётчик/«render amount».
    pub const RENDER_AMT: usize = 0x2FC;
    /// Тип/состояние модели (17 = `PLAYER_REMOTE`, 12 — используется для скинов).
    pub const MODEL_STATE: usize = 0x304;
    /// Дополнительное состояние (12 — смена скина, 3/5 — эффекты).
    pub const EXTRA_STATE: usize = 0x308;
    /// Позиция в мире.
    pub const ORIGIN: usize = 0xB48;
    /// `player_info_t` для сущности игрока.
    pub const PLAYER_INFO: usize = 0xB60;
    /// `model_t *`.
    pub const MODEL: usize = 0xB94;
    /// Флаги рендера (бит 1 = «не рисовать», ставится движком в некоторых режимах).
    pub const RENDER_FLAGS: usize = 0xBC0;
}

/// Смещения полей `model_t`, которые нужны для студийных моделей.
pub mod model {
    pub const FLAGS: usize = 0x40;
    pub const TYPE: usize = 0x44;
    /// `studiohdr_t *` (extradata).
    pub const EXTRADATA: usize = 0x184;

    pub const FLAG_LOADED: i32 = 1;
    pub const TYPE_STUDIO: i32 = 1;
}