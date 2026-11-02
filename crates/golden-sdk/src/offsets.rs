//! Смещения и адреса Gold Client (CS 1.6), полученные статическим анализом.
//!
//! Все числа — **RVA** (offset от базы модуля); база обоих модулей одна и та же,
//! [`IMAGE_BASE`], поэтому абсолютный адрес = `base + RVA`.
//!
//! Источник истины и разбор каждой цифры — `docs/STUDIO_REVERSE.md`. Смещения,
//! которые удалось проверить в живой игре, помечены ✔. После обновления
//! бинарников сверить их помогает [`crate::validate`].
//!
//! # Как читать этот файл
//!
//! | Модуль | Что внутри |
//! | --- | --- |
//! | [`hw`] | `hw.dll`: глобалы движка, его функции, командный буфер |
//! | [`client`] | `client.dll`: интерфейс рендера, `g_PlayerExtraInfo`, vtable |
//! | [`renderer`] | поля объекта `CStudioModelRenderer` (кости, камера, цвета) |
//! | [`api`] | слоты `engine_studio_api_t` (48 функций движка) |
//! | [`entity`] | поля `cl_entity_t`: модель, оружие, позиция, флаги |
//! | [`player_info`] | аргумент `StudioDrawPlayer`: номер игрока, модель оружия |
//! | [`entity_state`] | сетевой массив состояний (`GetEntityState`) |
//! | [`model`] | поля `model_t` (`studiohdr_t *` лежит в `+0x184`) |

/// Preferred база образа у `hw.dll` и `client.dll`.
pub const IMAGE_BASE: usize = 0x1000_0000;

/// Идентификатор сборки, к которой относятся смещения в этом файле.
/// Менять вместе со смещениями, если сборка другая.
pub const BUILD: &str = "goldclient-cs16-2026";

// =====================================================================
// hw.dll — движок
// =====================================================================

/// `hw.dll`: глобальные переменные и функции движка.
pub mod hw {
    // --- студийный рендер: точки подмены ---

    /// `r_studio_interface_t **g_pStudioInterface` — то, что движок читает каждый
    /// кадр. Подмена этого указателя = подмена рендерера целиком.
    pub const P_STUDIO_INTERFACE: usize = 0x27_1B04;

    /// `engine_studio_api_t g_EngineStudioAPI` — таблица 48 функций, которую
    /// движок отдаёт клиенту при инициализации.
    pub const ENGINE_STUDIO_API: usize = 0x27_1B08;

    /// `r_studio_interface_t g_DefaultStudioInterface` — дефолтный (движковый)
    /// рендерер `{ version = 1, DrawModel, DrawPlayer }`; костей не считает.
    pub const DEFAULT_STUDIO_INTERFACE: usize = 0x27_1AF0;

    /// `void StudioInterfaceSetup(void)` — ставит интерфейс рендера; зовётся при
    /// инициализации рендера и после загрузки клиента.
    pub const STUDIO_INTERFACE_SETUP: usize = 0x35_FA0;

    // --- состояние мира ---

    /// Указатель на `HUD_GetStudioModelInterface` из `client.dll`.
    pub const P_CLIENT_STUDIO_ENTRY: usize = 0x43_DE4C;

    /// Флаг «клиент загружен и готов».
    pub const CLIENT_READY: usize = 0x43_DB9C;

    /// Сущность, которую движок рисует сейчас (её же отдаёт
    /// `engine_studio_api_t::GetCurrentEntity`).
    pub const P_CURRENT_ENTITY: usize = 0x78_E470;

    /// `cl_entity_t **g_pRenderEntities` — массив крупных «рендер-сущностей»
    /// (`0xBC4` байта на элемент), с которыми работают рендерер и трассировка.
    ///
    /// Адрес взят из `SV_HullForEntity` (`0x100507C0`): перед вызовом
    /// построителя хитбоксов движок считает
    /// `ECX = [0x10442880] + entity_index * 0xBC4`.
    pub const P_RENDER_ENTITIES: usize = 0x44_2880;

    /// Шаг элемента [`P_RENDER_ENTITIES`].
    pub const RENDER_ENTITY_STRIDE: usize = 0xBC4;

    /// `matrix3x4 *g_boneTransform` — буфер матриц костей (128 по 12 float),
    /// по которому движок строит хитбоксы для трассировки выстрела.
    ///
    /// Пишут его `R_StudioSetupBones` (`0x10091580`), `SV_StudioSetupBones`
    /// (`0x10092C10`) и `0x100950B0`; читает построитель хитбоксов
    /// ([`STUDIO_TRACE_HITBOXES`]). Рядом лежат ещё два буфера по `0x1800`:
    /// `0x129F1258` и `0x129F4258`.
    ///
    /// Здесь смещение от базы модуля; в Ghidra тот же буфер — `0x129F2A58`.
    pub const TRACE_BONES: usize = 0x29F_2A58;

    /// `int StudioHitboxesForTrace(cl_entity_t *, const float *offset,
    /// int *count, int skip_gear)` — строит хитбоксы по матрицам костей.
    ///
    /// Перебирает `numhitboxes` записей по `0x20` байт, для каждой берёт
    /// `bbmin`/`bbmax` в системе кости и переводит их матрицей из
    /// [`TRACE_BONES`]. При `skip_gear != 0` пропускает запись с номером
    /// `0x14` (бокс оружия в руках). Первые два аргумента передаются в
    /// `ECX`/`EDX`, остальные — в стеке; чистит стек вызывающий.
    pub const STUDIO_TRACE_HITBOXES: usize = 0x92_930;

    /// `int cl_parsecount` — слот `GetParsecountPtr`.
    pub const PARSECOUNT: usize = 0xF8_FE30;

    /// Массив состояний сущностей — реэкспорт [`super::entity_state`].
    pub use super::entity_state;

    /// Начало массива состояний (то же, что [`entity_state::ARRAY`]).
    pub const ENTITY_STATE_ARRAY: usize = entity_state::ARRAY;

    // --- консоль ---

    /// `Cbuf_AddText(const char *)` — единственная обёртка над командным буфером,
    /// принимающая строку. Найдено по строке «Cbuf_AddText: buffer overflow».
    pub const CBUF_ADD_TEXT: usize = 0x03_1E80;

    /// `ICvar *g_pCVar` — интерфейс консоли. Читается в
    /// `ConCommandBase::Add` (`0x10019FE0`) как `g_pCVar->vtable[0]`, то есть
    /// `CCvar::RegisterConCommand` (`0x100BC170`).
    pub const G_P_CVAR: usize = 0x03E_222C;

    /// Статический объект `ConCommand` движка для команды `cmdlist`
    /// (`0x102D9CC4`). У него после инициализации заполнены все поля, включая
    /// непонятные `+0x18`/`+0x1C`/`+0x24`/`+0x28`, поэтому он служит шаблоном
    /// для своих команд — см. [`crate::concommand`].
    pub const CON_COMMAND_TEMPLATE: usize = 0x02D_9CC4;

    /// `CCvar::RegisterConCommand(ConCommandBase *)` — вставка в список и хеш.
    /// Вызывать не нужно: это делает `ConCommandBase::Add`.
    pub const CCVAR_REGISTER: usize = 0x0B_C170;

    /// `CCvar::UnregisterConCommand(ConCommandBase *)` — снять флаг `registered`,
    /// вынуть из списка и хеша. Вызывать не нужно: это делает
    /// `ConCommandBase::Remove` (`0x1001A000`).
    pub const CCVAR_UNREGISTER: usize = 0x0B_C310;

    // --- ввод ---

    /// `void Key_Event(int keynum, int down)` — нажатие (`1`) и отпускание (`0`)
    /// клавиши: здесь движок ищет бинд и отдаёт `+jump`/`+forward` в командный
    /// буфер. Соглашение `__fastcall` (`keynum` в `ECX`, `down` в `EDX`) —
    /// проверено по прологу `0x10083480` в Ghidra.
    ///
    /// Перехват этой функции глушит клавишу целиком: она не доходит ни до биндов,
    /// ни до массива нажатых клавиш.
    pub const KEY_EVENT: usize = 0x08_3480;

    // --- функции моделей (cdecl) ---

    /// `model_t *Mod_ForName(int load, const char *name)`.
    ///
    /// Порядок аргументов — как в бинарнике, **обратный** HLSDK: сначала `load`.
    pub const MOD_FOR_NAME: usize = 0x9D_870;

    /// `studiohdr_t *Mod_Extradata(model_t *m)`.
    ///
    /// Осторожно: при ошибке зовёт `Host_Error` и роняет игру. Для чтения имени
    /// модели брать [`super::model::EXTRADATA`] напрямую.
    pub const MOD_EXTRADATA: usize = 0x69_D40;

    /// `model_t *Mod_ForIndex(int index)`.
    pub const MOD_FOR_INDEX: usize = 0x48_5B0;

    // --- функции рендера моделей ---

    /// `void R_StudioSetupModel(int modelindex, void **ppsubmodel, void **ppmodel)`.
    pub const R_STUDIO_SETUP_MODEL: usize = 0x98_E10;

    /// `int R_StudioCheckBBox(void)`.
    pub const R_STUDIO_CHECK_BBOX: usize = 0x94_3C0;

    /// `void StudioSetupLighting(...)` — слот api `+0x58`.
    pub const STUDIO_SETUP_LIGHTING: usize = 0x99_560;

    /// `void SV_StudioSetupBones(model_t *model, float frame, int sequence,
    /// const float angles[3], const float origin[3], float boneadj[8],
    /// const unsigned char controllers[4], int bone)` — серверный расчёт
    /// костей.
    ///
    /// Обычный `cdecl`: пролог `PUSH EBP / MOV EBP,ESP / SUB ESP,0x250`,
    /// аргументы в стеке. Заголовок модели берётся не из аргумента, а из
    /// [`STUDIO_HEADER_CACHE`], поэтому вызывающий обязан его выставить.
    /// `bone = -1` — посчитать все кости. Матрицы ложатся в
    /// [`TRACE_BONES`].
    pub const SV_STUDIO_SETUP_BONES: usize = 0x92_C10;

    /// `studiohdr_t *g_pStudioHeaderCache` — заголовок модели, с которым
    /// работает [`SV_STUDIO_SETUP_BONES`].
    ///
    /// Заполняет `SV_HullForStudioModel`: `model_t->cache.data`, то есть
    /// поле `+0x184` модели ([`super::model::EXTRADATA`]).
    pub const STUDIO_HEADER_CACHE: usize = 0x29F_4888;

    // --- состояние мира ---

    /// ⚠ `int` по адресу `hw+0x274B18`, который считался `cl.in_game`.
    ///
    /// Проверено в живой игре: в момент, когда карта загружена, игрок стоит на
    /// земле и `ref_params::TIME` растёт (160.5 с), здесь лежит `0`. Значение
    /// с «в игре» не совпадает, поэтому [`super::super::state::EngineState::in_game`]
    /// им **не** пользуется и определяет игру по живому `ref_params`.
    pub const IN_GAME_UNVERIFIED: usize = 0x27_4B18;

    /// ✔ `int` — открыто меню (`1`), при этом мир продолжает идти.
    pub const IN_MENU: usize = 0x333_A62C;

    /// ✔ `int` — открыт чат (`1`): клавиши уходят в строку ввода.
    pub const CHAT_OPENED: usize = 0x27_E2E00;

    /// `ref_params_t` — см. [`super::ref_params`].
    pub use super::ref_params;
}

// =====================================================================
// client.dll — игровой клиент
// =====================================================================

/// `ref_params_t` — снимок кадра, который движок собирает перед отрисовкой.
///
/// В `hw.dll` лежит **копия** клиентской структуры: `FUN_100B4700` копирует
/// `0x3A` двойных слов из `0x12F9C5A8` в [`BASE`] каждый кадр (виден цикл
/// `*puVar5 = *puVar3` в декомпиляции). Отсюда движок берёт камеру, время и
/// флаги состояния — значит значения всегда свежие и совпадают с тем, что
/// видит рендер.
///
/// Раскладка сверена с живым процессом: `vieworg = simorg + view_ofs`
/// (разница ровно `17.0` по `z`), `cl_viewangles` совпадают с `viewangles`,
/// `health = 100`, `viewsize = 120`, `width/height = 1920/1080`.
///
/// ✔ — сверено живым замером, ← — позиция выведена из порядка полей.
pub mod ref_params {
    /// База структуры в `hw.dll`.
    pub const BASE: usize = 0x43_E25C;

    /// ✔ `vec3_t vieworg` — позиция камеры (origin + [`VIEW_OFS`]).
    pub const VIEW_ORIGIN: usize = BASE + 0x00;

    /// ✔ `vec3_t viewangles` — углы камеры с отдачей от стрельбы.
    pub const VIEW_ANGLES: usize = BASE + 0x0C;

    /// ✔ `vec3_t forward` — единичный вектор взгляда.
    pub const FORWARD: usize = BASE + 0x18;

    /// ✔ `vec3_t right` — единичный вектор вправо.
    pub const RIGHT: usize = BASE + 0x24;

    /// ✔ `vec3_t up` — единичный вектор вверх.
    pub const UP: usize = BASE + 0x30;

    /// ✔ `float frametime` — длительность кадра в секундах (`0.0243`).
    pub const FRAMETIME: usize = BASE + 0x3C;

    /// ✔ `float time` — игровое время в секундах.
    pub const TIME: usize = BASE + 0x40;

    /// ← `int intermission` — идёт смена карты.
    pub const INTERMISSION: usize = BASE + 0x44;

    /// ← `int paused`.
    pub const PAUSED: usize = BASE + 0x48;

    /// ← `int spectator` — камера наблюдателя.
    pub const SPECTATOR: usize = BASE + 0x4C;

    /// ✔ `int onground` — стоит на земле (`1`), в прыжке/на лестнице (`0`).
    pub const ON_GROUND: usize = BASE + 0x50;

    /// ✔ `int waterlevel` — `0` сухо, `1` по щиколотку, `2` в воде, `3` под водой.
    pub const WATER_LEVEL: usize = BASE + 0x54;

    /// ✔ `vec3_t simvel` — скорость игрока в юнитах/с.
    pub const SIMVEL: usize = BASE + 0x58;

    /// ✔ `vec3_t simorg` — позиция игрока (совпадает с origin сущности).
    pub const SIMORG: usize = BASE + 0x64;

    /// ✔ `vec3_t view_ofs` — смещение глаз от позиции игрока (`z = 17` стоя).
    ///
    /// В приседе `z` падает до `12`, поэтому это единственный признак приседа,
    /// который движок публикует без обращения к серверному состоянию.
    pub const VIEW_OFS: usize = BASE + 0x70;

    /// ✔ `vec3_t cl_viewangles` — углы ввода без отдачи.
    pub const CL_VIEWANGLES: usize = BASE + 0x80;

    /// ✔ `int health` — здоровье; `-1`, если движок не знает.
    pub const HEALTH: usize = BASE + 0x8C;

    /// ← `vec3_t crosshairangle`.
    pub const CROSSHAIRANGLE: usize = BASE + 0x90;

    /// ✔ `float viewsize` — `cl_viewsize`, по умолчанию `120`.
    pub const VIEWSIZE: usize = BASE + 0x9C;

    /// ← `vec3_t punchangle` — отдача, которую движок добавил к камере.
    pub const PUNCHANGLE: usize = BASE + 0xA0;

    /// ✔ `int maxclients` — `32` на обычном сервере.
    pub const MAXCLIENTS: usize = BASE + 0xAC;

    /// ✔ `int viewentity` — номер сущности, за которой смотрит игрок.
    pub const VIEWENTITY: usize = BASE + 0xB0;

    /// ✔ `int width` — ширина окна в пикселях.
    pub const SCREEN_WIDTH: usize = BASE + 0xD8;

    /// ✔ `int height` — высота окна в пикселях.
    pub const SCREEN_HEIGHT: usize = BASE + 0xDC;
}

/// `client.dll`: интерфейс рендера, данные игроков, vtable рендерера.
pub mod client {
    // --- студийный интерфейс ---

    /// Экспорт `HUD_GetStudioModelInterface` (тонкая обёртка через vtable).
    pub const HUD_GET_STUDIO_MODEL_INTERFACE: usize = 0xBA_0F0;

    /// Реальная реализация `HUD_GetStudioModelInterface(version, &iface, pstudio)`.
    pub const STUDIO_INTERFACE_INIT: usize = 0x60_AE0;

    /// `int Init(void)` — инициализация рендерера и загрузка моделей игроков.
    pub const STUDIO_RENDERER_INIT: usize = 0x60_AB0;

    /// `r_studio_interface_t g_StudioInterface` — то, что клиент отдаёт движку.
    pub const STUDIO_INTERFACE: usize = 0x1966_B0;

    /// Копия `engine_studio_api_t` (0xC0 байт) внутри клиента.
    pub const ENGINE_STUDIO_API_COPY: usize = 0x1B25_48;

    /// Объект `CStudioModelRenderer` (статический).
    pub const STUDIO_RENDERER: usize = 0x1F51_C8;

    /// Vtable `CStudioModelRenderer` (26 слотов), поля — [`super::renderer`].
    pub const STUDIO_RENDERER_VTABLE: usize = 0x168F_94;

    // --- слоты vtable `CStudioModelRenderer` ---

    /// `int Init(void)`.
    pub const VT_INIT: usize = 0x01;
    /// Конструктор/повторная инициализация.
    pub const VT_CONSTRUCT: usize = 0x02;
    /// `int StudioDrawModel(int flags)`.
    pub const VT_STUDIO_DRAW_MODEL: usize = 0x03;
    /// `int StudioDrawPlayer(int flags, player_info_s *info)`.
    pub const VT_STUDIO_DRAW_PLAYER: usize = 0x04;
    /// `int StudioSetupBones(void)` — здесь считаются матрицы костей.
    pub const VT_STUDIO_SETUP_BONES: usize = 0x07;
    /// `int StudioRenderModel(void)`.
    pub const VT_STUDIO_RENDER_MODEL: usize = 0x08;
    /// `int SetPlayerModel(model_t *model)`.
    pub const VT_SET_PLAYER_MODEL: usize = 0x0A;

    // --- `g_PlayerExtraInfo`: то же, из чего клиент собирает таблицу счёта ---

    /// `extra_player_info_t g_PlayerExtraInfo[]`, база массива.
    ///
    /// Элемент — [`PLAYER_EXTRA_STRIDE`] байт по номеру игрока. Заполняется
    /// обработчиком сообщения `TeamInfo` ([`MSG_TEAM_INFO`]); отсюда клиент берёт
    /// данные для таблицы счёта, поэтому значения стабильны и не зависят от того,
    /// что сейчас нарисовано на экране.
    pub const PLAYER_EXTRA_INFO: usize = 0x22_4E40;

    /// Размер элемента `g_PlayerExtraInfo` (`imul` в `MsgFunc_TeamInfo`).
    pub const PLAYER_EXTRA_STRIDE: usize = 0xC0;

    /// ✔ Номер команды: `1` — террористы, `2` — спецназ, `3` — наблюдатель.
    pub const PLAYER_EXTRA_TEAM: usize = 0x7C;

    /// Имя команды из сетевого сообщения: `"CT"`, `"TERRORIST"`, `"SPECTATOR"`.
    pub const PLAYER_EXTRA_TEAMNAME: usize = 0x80;

    /// ✔ Убийства (`"frags"` в таблице счёта).
    pub const PLAYER_EXTRA_FRAGS: usize = 0x28;

    /// ✔ Смерти (`"deaths"`).
    pub const PLAYER_EXTRA_DEATHS: usize = 0x2C;

    /// ✔ Несёт бомбу (C4) — байт (класс `#Cstrike_BOMB`).
    pub const PLAYER_EXTRA_BOMB: usize = 0x30;

    /// VIP в режиме `as_*` — байт (`#Cstrike_VIP`).
    pub const PLAYER_EXTRA_VIP: usize = 0x31;

    /// ✔ Мёртв — байт (`#Cstrike_DEAD`).
    pub const PLAYER_EXTRA_DEAD: usize = 0x32;

    /// ✔ Есть дефуз-кит — байт (`#Cstrike_DEFUSE_KIT`).
    pub const PLAYER_EXTRA_DEFUSER: usize = 0x33;

    /// ✔ Здоровье (`"health"`).
    pub const PLAYER_EXTRA_HEALTH: usize = 0xB8;

    /// ✔ Деньги (`"account"`).
    pub const PLAYER_EXTRA_MONEY: usize = 0xBC;

    /// Обработчик сообщения `TeamInfo` — пишет `g_PlayerExtraInfo`.
    pub const MSG_TEAM_INFO: usize = 0x0B_9BE0;

    // --- консольные переменные ---

    /// `cvar_t` в этой сборке: `+0x00` — `char *name`, `+0x04` — `char *string`,
    /// `+0x08` — `int flags`, `+0x0C` — `float value`, `+0x10` — `float` зеркало
    /// значения, `+0x14` — `char *def`. Глобалы ниже — **указатели** на `cvar_t`,
    /// поэтому сначала читается указатель, потом `+0x0C`.
    pub mod cvars {
        /// ✔ `sensitivity` (`2.5` у этой установки).
        pub const SENSITIVITY: usize = 0x1B_1174;

        /// ✔ `m_yaw` (`0.022`): множитель горизонтального движения мышью.
        pub const M_YAW: usize = 0x1B_1164;

        /// ✔ `default_fov` (`90`) — не сама переменная, а **имя** рядом с ней:
        /// по этому адресу лежит строка `"default_fov"`, а `cvar_t` живёт
        /// в `.data` `client.dll` и находится движком через реестр cvars.
        pub const DEFAULT_FOV_NAME: usize = 0x228_AB8;

        /// Значение `cvar_t` внутри структуры.
        pub const VALUE: usize = 0x0C;

        /// Указатель на имя внутри `cvar_t`.
        pub const NAME: usize = 0x00;
    }

    // --- поле зрения ---

    /// Кандидаты на `m_iFov` (текущий FOV, `90` по умолчанию, `40` при зуме AWP).
    ///
    /// Все три адреса в живом процессе читались как `90`, поэтому какой из них
    /// именно работает — не определено; [`crate::state::EngineState::fov`] берёт
    /// первый, а [`crate::validate`] сверяет все три между собой.
    pub mod view {
        /// `m_iFov`, кандидат 1.
        pub const FOV_1: usize = 0x1CE_4D0;
        /// `m_iFov`, кандидат 2.
        pub const FOV_2: usize = 0x1F4_ACC;
        /// `m_iFov`, кандидат 3.
        pub const FOV_3: usize = 0x224_A34;
    }
}

// =====================================================================
// Структуры
// =====================================================================

/// Поля объекта `CStudioModelRenderer` (thiscall, `this` = `client+0x1F51C8`).
pub mod renderer {
    /// Указатель на vtable (26 слотов).
    pub const VTABLE: usize = 0x00;
    /// Время клиента в кадре.
    pub const CL_TIME: usize = 0x08;
    /// Время предыдущего кадра.
    pub const CL_OLD_TIME: usize = 0x10;
    /// Счётчик кадров.
    pub const FRAME_COUNT: usize = 0x20;
    /// `cl_entity_t *m_pCurrentEntity` — сущность, которую рисуем.
    pub const CURRENT_ENTITY: usize = 0x30;
    /// `int m_bIsPlayer` — рисуем игрока, а не объект.
    pub const IS_PLAYER: usize = 0x34;
    /// `model_t *m_pRenderModel` — модель, выбранная движком для этого прохода.
    pub const RENDER_MODEL: usize = 0x38;
    /// `int m_nRenderAmt` — яркость рендера.
    pub const RENDER_AMT: usize = 0x40;
    /// `studiohdr_t *m_pStudioHeader` — заголовок текущей модели.
    pub const STUDIO_HEADER: usize = 0x4C;
    /// Верхний цвет модели (скин).
    pub const TOP_COLOR: usize = 0x58;
    /// Нижний цвет модели (скин).
    pub const BOTTOM_COLOR: usize = 0x5C;
    /// Указатель на `StudioDrawPlayer` (ставится при инициализации).
    pub const DRAW_PLAYER_FN: usize = 0x6C;

    /// Цвет игрока, верхняя половина.
    pub const PLAYER_TOP_COLOR: usize = 0x406C;
    /// Цвет игрока, нижняя половина.
    pub const PLAYER_BOTTOM_COLOR: usize = 0x4070;

    /// «Вверх» камеры кадра.
    pub const VIEW_UP: usize = 0x4074;
    /// «Вправо» камеры кадра.
    pub const VIEW_RIGHT: usize = 0x4080;
    /// Направление взгляда камеры.
    pub const VIEW_NORMAL: usize = 0x408C;
    /// Позиция камеры.
    pub const VIEW_ORIGIN: usize = 0x4098;
    /// Сколько моделей нарисовано (счётчик движка).
    pub const MODELS_DRAWN: usize = 0x40A8;

    /// `matrix3x4 *m_pbonetransform` — матрицы костей, посчитанные в этом кадре.
    ///
    /// Кость — 12 float (3x4, [`crate::studio::MAXSTUDIOBONES`] штук подряд),
    /// трансляция в индексах 3, 7, 11.
    pub const BONE_TRANSFORM: usize = 0x40B4;
    /// `m_plighttransform` — то же для источников света.
    pub const LIGHT_TRANSFORM: usize = 0x40B8;
}

/// Слоты `engine_studio_api_t`: байтовый offset от начала таблицы.
///
/// Восстановлены по декомпиляции `hw.dll`. Где смысл не подтверждён, в имени
/// стоит `GET_*`/`RETURN_ZERO*` — такие слоты в SDK не используются.
pub mod api {
    /// `void *Mem_Calloc(int count, int size)`.
    pub const MEM_CALLOC: usize = 0x00;
    /// `void Mem_Free(void *p)`.
    pub const MEM_FREE: usize = 0x04;
    /// `void *COM_LoadFile(const char *path, int usehunk)`.
    pub const COM_LOAD_FILE: usize = 0x08;
    /// `void Mod_SetFlags(void)`.
    pub const MOD_SET_FLAGS: usize = 0x0C;
    /// `studiohdr_t *Mod_Extradata(model_t *)`.
    pub const MOD_EXTRADATA: usize = 0x10;
    /// `model_t *Mod_ForIndex(int index)`.
    pub const MOD_FOR_INDEX: usize = 0x14;
    /// `cl_entity_t *GetCurrentEntity(void)`.
    pub const GET_CURRENT_ENTITY: usize = 0x18;
    /// `entity_state_t *GetEntityState(int index)`.
    pub const GET_ENTITY_STATE: usize = 0x1C;
    /// `cl_entity_t *GetEntityByIndex(int index)`.
    pub const GET_ENTITY_BY_INDEX: usize = 0x20;
    /// `int *GetParsecountPtr(void)`.
    pub const GET_PARSECOUNT_PTR: usize = 0x24;
    /// `void GetTimes(int *viewent, float *time, float *oldtime)`.
    pub const GET_TIMES: usize = 0x28;
    /// `void Mod_UnloadModel(void *)`.
    pub const MOD_UNLOAD_MODEL: usize = 0x2C;
    /// `void GetViewInfo(vec3 origin, vec3 up, vec3 right, vec3 forward)`.
    pub const GET_VIEW_INFO: usize = 0x30;
    /// `int GetRenderFlagsPtr(void)`.
    pub const GET_RENDER_FLAGS_PTR: usize = 0x34;
    /// `void GetLightPointers(void **a, void **b)`.
    pub const GET_LIGHT_POINTERS: usize = 0x38;
    /// `void GetPlayerColors(float *top, float *bottom)`.
    pub const GET_PLAYER_COLORS: usize = 0x3C;

    /// Неизвестный слот (возвращает указатель).
    pub const GET_PTR_40: usize = 0x40;
    /// Неизвестный слот (возвращает указатель).
    pub const GET_PTR_44: usize = 0x44;
    /// `int ReturnZero(void)`.
    pub const RETURN_ZERO: usize = 0x48;
    /// Неизвестный слот (возвращает указатель).
    pub const GET_PTR_4C: usize = 0x4C;

    /// `void SetupModel(int modelindex, void **ppsubmodel, void **ppmodel)`.
    pub const SETUP_MODEL: usize = 0x50;
    /// `int StudioCheckBBox(void)`.
    pub const STUDIO_CHECK_BBOX: usize = 0x54;
    /// `void StudioSetupLighting(cl_entity_t *, alight_t *)`.
    pub const STUDIO_SETUP_LIGHTING: usize = 0x58;
    /// `void StudioSetupDlights(void)`.
    pub const STUDIO_SETUP_DLIGHTS: usize = 0x5C;
    /// `void StudioSetupLightTransform(void)`.
    pub const STUDIO_SETUP_LIGHT_TRANSFORM: usize = 0x60;
    /// `void StudioDrawGlowShell(void)`.
    pub const STUDIO_DRAW_GLOW_SHELL: usize = 0x64;
    /// `void StudioDrawModelInternal(void)`.
    pub const STUDIO_DRAW_MODEL_INTERNAL: usize = 0x68;
    /// `void StudioDrawMeshes(void)` — нарезка на меши.
    pub const STUDIO_DRAW_MESHES: usize = 0x6C;
    /// `void StudioDrawMesh(void)`.
    pub const STUDIO_DRAW_MESH: usize = 0x70;
    /// `void StudioDrawTextures(void)`.
    pub const STUDIO_DRAW_TEXTURES: usize = 0x74;
    /// `void SetPlayerModelColors(void)`.
    pub const SET_PLAYER_MODEL_COLORS: usize = 0x78;
    /// `model_t *GetPlayerModel(int slot)`.
    pub const GET_PLAYER_MODEL: usize = 0x7C;
    /// `void StudioRenderFinal(void)`.
    pub const STUDIO_RENDER_FINAL: usize = 0x80;
    /// Неизвестный слот.
    pub const GET_84: usize = 0x84;
    /// `void SetStudioHeader(studiohdr_t *)`.
    pub const SET_STUDIO_HEADER: usize = 0x88;
    /// `void SetRenderHeader(model_t *)`.
    pub const SET_RENDER_HEADER: usize = 0x8C;
    /// `void SetSubmodel(int)`.
    pub const SET_SUBMODEL: usize = 0x90;
    /// `void StudioInitLight(void)`.
    pub const STUDIO_INIT_LIGHT: usize = 0x94;
    /// `void StudioSetTextureState(int)`.
    pub const STUDIO_SET_TEXTURE_STATE: usize = 0x98;
    /// `void StudioCopyViewOrigin(void)`.
    pub const STUDIO_COPY_VIEW_ORIGIN: usize = 0x9C;
    /// `int GetRenderMode(void)`.
    pub const GET_RENDER_MODE: usize = 0xA0;
    /// `void StudioBindTexture(void)`.
    pub const STUDIO_BIND_TEXTURE: usize = 0xA4;
    /// `void StudioSetTextureMode(int)`.
    pub const STUDIO_SET_TEXTURE_MODE: usize = 0xA8;
    /// `void StudioSetRenderAmt(int)`.
    pub const STUDIO_SET_RENDER_AMT: usize = 0xAC;
    /// `int ReturnZero2(void)`.
    pub const RETURN_ZERO_2: usize = 0xB0;
    /// `void StudioDrawModelByIndex(int modelindex)`.
    pub const STUDIO_DRAW_MODEL_BY_INDEX: usize = 0xB4;
    /// `void SetRenderTarget(void *)`.
    pub const SET_RENDER_TARGET: usize = 0xB8;
    /// `model_t *Mod_ForName(int load, const char *name)` — порядок как в бинарнике.
    pub const MOD_FOR_NAME: usize = 0xBC;
}

/// Поля `cl_entity_t` — сущности, которую отдаёт `GetEntityByIndex` /
/// `GetCurrentEntity`.
///
/// Структура заметно больше классического `cl_entity_t` (`0x154`), целиком она
/// не описана: здесь только поля, которые проверены кодом клиента и на экране.
pub mod entity {
    /// ✔ Номер сущности/игрока.
    ///
    /// Проверено в игре по строкам лога: у сущностей `0x…2a04`/`0x…d7a8`/`0x…35c8`
    /// поле `+0` даёт 9/2/10 и совпадает с моделью, а `+4` у всех равно 1 — это
    /// `modelindex`, а не номер слота. Раньше здесь стоял `0x04`, и все модели
    /// получали команду игрока со слотом 1.
    pub const INDEX: usize = 0x00;

    /// ✔ Позиция в мире.
    pub const ORIGIN: usize = 0xB48;

    /// Углы поворота сущности в градусах — сразу за [`ORIGIN`].
    ///
    /// В `cl_entity_t` `origin` и `angles` идут подряд; здесь это
    /// `0xB48 + 12 = 0xB54`. Используется для направления взгляда чужих
    /// игроков: сетевой `entity_state` в Gold Client для них непригоден.
    pub const ANGLES: usize = 0xB54;

    /// ✔ `model_t *` — модель тела. Заголовок читается из [`super::model::EXTRADATA`].
    pub const MODEL: usize = 0xB94;

    /// ✔ Номер модели тела (`modelindex`) — то же, что [`MODEL`], но индексом.
    ///
    /// Подтверждено `FUN_1005f760`: поле сверяется с числом загруженных моделей и
    /// обнуляется, если индекс вне диапазона.
    pub const MODEL_INDEX: usize = 0x2DC;

    /// ✔ `player_info_t` сущности игрока (ник и т.п.).
    pub const PLAYER_INFO: usize = 0xB60;

    /// ✔ Цвета модели игрока, упакованные в `int`: `[0..8]` — верх, `[8..16]` — низ.
    pub const PLAYER_COLORS: usize = 0x2E4;

    /// `render amount` — яркость/затухание.
    pub const RENDER_AMT: usize = 0x2FC;

    /// Тип/состояние модели (`17` = `PLAYER_REMOTE`).
    pub const MODEL_STATE: usize = 0x304;

    /// Дополнительное состояние (`12` — смена скина, `3`/`5` — эффекты).
    pub const EXTRA_STATE: usize = 0x308;

    /// Флаги рендера (бит 1 — «не рисовать»).
    pub const RENDER_FLAGS: usize = 0xBC0;

    /// Номер модели оружия в руках (копия `weaponmodel`).
    ///
    /// Найдено в `FUN_1005f760`: обнуляется вместе с `entity_state + 0xB8`, после
    /// чего оружие перестаёт рисоваться.
    ///
    /// **Для «оружия в руках» его лучше не брать**: он отстаёт от того, что
    /// движок реально рисует. Точный источник — [`super::player_info::WEAPON_MODEL`],
    /// из которого `StudioDrawPlayer` сам поднимает модель оружия.
    pub const WEAPON_MODEL: usize = 0x368;
}

/// Аргумент `StudioDrawPlayer(int flags, player_info_s *info)` — вторая половина.
///
/// Всё взято из декомпиляции `CStudioModelRenderer::StudioDrawPlayer`
/// (`client.dll+0x100AFE60`): движок передаёт сюда готовую запись о рисуемом
/// игроке, и клиент читает из неё ровно эти поля.
pub mod player_info {
    /// ✔ Номер игрока (`entity_state_t::number`) — ключ к `g_PlayerExtraInfo`.
    pub const INDEX: usize = 0x04;

    /// ✔ Индекс модели оружия в руках; `0` — оружия нет.
    ///
    /// `StudioDrawPlayer` берёт его как `*(int *)(info + 0xB4)`, поднимает модель
    /// через `Mod_ForIndex` и рисует вторым проходом. Это и есть то оружие,
    /// которое видно в руках.
    pub const WEAPON_MODEL: usize = 0xB4;
}

/// Состояние анимации в записи, которую движок отдаёт в `StudioDrawPlayer`.
///
/// Начало записи совпадает с `entity_state_t`: `entityType`, `number`,
/// `msg_time`, `messageNum`, затем позиция. Смещения сверены дампом живой
/// записи в игре (позиция, углы, номер последовательности и кадр совпали с
/// тем, что показывает движок).
pub mod player_state {
    /// ✔ Позиция тела в мире (`+0x10`).
    pub const ORIGIN: usize = 0x10;

    /// ✔ Углы поворота тела в градусах (`+0x1C`).
    pub const ANGLES: usize = 0x1C;

    /// ✔ Номер последовательности анимации (`+0x2C`).
    pub const SEQUENCE: usize = 0x2C;

    /// ✔ Кадр анимации вместе с дробной частью (`+0x30`).
    pub const FRAME: usize = 0x30;

    /// Контроллеры костей — четыре байта (`+0x48`).
    ///
    /// В `entity_state_t` это `byte controller[4]`: по ним считается поправка
    /// позы (наклон корпуса и головы при прицеливании). Сверены не были:
    /// значения в дампе нужно проверить глазами.
    pub const CONTROLLERS: usize = 0x48;
}

/// Массив состояний сущностей (`GetEntityState`) — сетевые данные движка.
pub mod entity_state {
    /// Начало массива в `hw.dll`.
    pub const ARRAY: usize = 0x2F90_A18;

    /// Размер элемента — из `GetEntityState` (`&array + index * 0xA0`).
    pub const STRIDE: usize = 0xA0;

    /// `modelindex` сущности.
    pub const MODEL_INDEX: usize = 0x20;
}

/// Поля `model_t`, нужные для студийных моделей.
pub mod model {
    /// Флаги (`FLAG_LOADED` — модель загружена).
    pub const FLAGS: usize = 0x40;

    /// Тип модели (`TYPE_STUDIO` — студийная).
    pub const TYPE: usize = 0x44;

    /// `studiohdr_t *` (extradata). Читать **напрямую**, не через `Mod_Extradata`.
    pub const EXTRADATA: usize = 0x184;

    /// Значение [`FLAGS`] «модель загружена».
    pub const FLAG_LOADED: i32 = 1;

    /// Значение [`TYPE`] «студийная модель».
    pub const TYPE_STUDIO: i32 = 1;
}
