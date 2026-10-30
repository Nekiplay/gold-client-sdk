//! Самопроверка смещений.
//!
//! Смысл: после обновления игры смещения «тихо» съезжают, и падает уже что-то
//! невнятное. Здесь набор дешёвых проверок, каждая из которых ловит именно
//! уехавшее смещение, а не «что-то пошло не так».

use core::ffi::c_void;

use crate::engine::EngineStudioApi;
use crate::offsets::{client, entity, hw};
use crate::player::PlayerExtra;
use crate::process;
use crate::renderer::{interface_slot, studio_interface, StudioRenderer, StudioInterface};
use crate::studio::{IDST, VERSION};

/// Результат одной проверки.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self { name, ok: true, detail: detail.into() }
    }

    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self { name, ok: false, detail: detail.into() }
    }

    fn require(condition: bool, name: &'static str, detail: impl Into<String>) -> Self {
        if condition { Self::pass(name, detail) } else { Self::fail(name, detail) }
    }
}

/// Полный прогон проверок. Возвращает отчёт по порядку.
pub fn run() -> Vec<Check> {
    let mut checks = Vec::new();

    let hw_base = process::hw_base();
    let client_base = process::client_base();

    checks.push(Check::require(hw_base.is_some(), "hw.dll загружен", fmt_opt(hw_base)));
    checks.push(Check::require(client_base.is_some(), "client.dll загружен", fmt_opt(client_base)));

    let (Some(hw_base), Some(client_base)) = (hw_base, client_base) else {
        return checks;
    };

    checks.extend(check_api_table(hw_base));
    checks.extend(check_interface(hw_base, client_base));
    checks.extend(check_renderer(client_base));
    checks.extend(check_client_copy_matches(client_base));
    checks.extend(check_world(hw_base, client_base));

    checks
}

/// Мир: текущая сущность рендерера и записи игроков.
///
/// Здесь проверяются смещения, на которых держится весь прикладной слой
/// (`golden-std`): номер сущности, её модель и `g_PlayerExtraInfo`.
fn check_world(hw_base: usize, _client_base: usize) -> Vec<Check> {
    let mut out = Vec::new();

    out.push(Check::require(
        process::in_range(hw_base + hw::ENTITY_STATE_ARRAY, hw_base, 0x600_0000),
        "массив entity_state в hw.dll",
        format!("hw+{:#x}", hw::ENTITY_STATE_ARRAY),
    ));

    // Текущая сущность рендерера: номер и модель.
    // SAFETY: статический указатель движка, значение либо null, либо сущность.
    let current = unsafe { process::read::<usize>(hw_base + hw::P_CURRENT_ENTITY) };
    if current == 0 || !process::is_readable(current) {
        out.push(Check::pass("текущая сущность", "нет активной сущности (вне отрисовки)"));
    } else {
        // SAFETY: страница сущности проверена на читаемость.
        let index = unsafe { process::read::<i32>(current + entity::INDEX) };
        out.push(Check::require(
            (0..=EngineStudioApi::MAX_EDICTS).contains(&index),
            "entity: номер сущности",
            format!("{index} @ {current:#x}"),
        ));
        // SAFETY: то же.
        let model = unsafe { process::read::<usize>(current + entity::MODEL) };
        let header = unsafe { crate::studio::model_extradata(model as *mut c_void) };
        out.push(Check::require(
            header.is_some(),
            "entity: model_t -> studiohdr_t",
            match header {
                Some(model) => format!("{} ({} костей)", model.name(), model.bones().len()),
                None => format!("model_t {model:#x} не открылся"),
            },
        ));
    }

    // Записи игроков: ник, команда, здоровье.
    let mut named = 0usize;
    let mut bad_team = Vec::new();
    let mut bad_health = Vec::new();
    for index in 0..=63 {
        let Some(extra) = PlayerExtra::for_index(index) else {
            continue;
        };
        let name = extra.name();
        if name.is_empty() {
            continue;
        }
        named += 1;
        let team = extra.team_number();
        if team != 0 && !(1..=2).contains(&team) {
            bad_team.push(format!("{index}:{team}"));
        }
        let health = extra.health();
        if !(-100..=200).contains(&health) {
            bad_health.push(format!("{index}:{health}"));
        }
    }
    out.push(Check::require(
        named > 0,
        "g_PlayerExtraInfo: есть заполненные записи",
        format!("{named} игроков, база client+{:#x}, шаг {:#x}", client::PLAYER_EXTRA_INFO, client::PLAYER_EXTRA_STRIDE),
    ));
    out.push(Check::require(
        bad_team.is_empty(),
        "g_PlayerExtraInfo: teamnumber 1 или 2",
        if bad_team.is_empty() { "все ок".to_string() } else { format!("странные: {}", bad_team.join(" ")) },
    ));
    out.push(Check::require(
        bad_health.is_empty(),
        "g_PlayerExtraInfo: health в пределах",
        if bad_health.is_empty() { "все ок".to_string() } else { format!("странные: {}", bad_health.join(" ")) },
    ));
    out
}

/// Таблица `engine_studio_api_t` должна состоять из кода `hw.dll`.
fn check_api_table(hw_base: usize) -> Vec<Check> {
    let mut out = Vec::new();
    let api = EngineStudioApi::for_engine();
    let api = match api {
        Some(api) => api,
        None => {
            out.push(Check::fail("engine_studio_api_t", "таблица не читается"));
            return out;
        }
    };

    let mut code = 0usize;
    let mut non_code = Vec::new();
    for offset in (0..EngineStudioApi::SLOT_COUNT * 4).step_by(4) {
        match api.slot(offset) {
            Some(ptr) if process::is_code(ptr as usize, hw_base) => code += 1,
            Some(ptr) => non_code.push(format!("+0x{offset:02x}@{ptr:p}")),
            None => non_code.push(format!("+0x{offset:02x}=null")),
        }
    }

    out.push(Check::require(
        code >= 40,
        "api: слоты указывают в код hw.dll",
        format!("{code}/{} слотов ок{}", EngineStudioApi::SLOT_COUNT,
                if non_code.is_empty() { String::new() } else { format!(", проблемные: {}", non_code.join(" ")) }),
    ));
    out
}

/// Цепочка `g_pStudioInterface` → `r_studio_interface_t`.
fn check_interface(hw_base: usize, client_base: usize) -> Vec<Check> {
    let mut out = Vec::new();

    let slot = interface_slot();
    out.push(Check::require(slot.is_some(), "g_pStudioInterface", format!("hw+{:#x}", hw::P_STUDIO_INTERFACE)));
    let Some(slot) = slot else { return out };

    // SAFETY: корректный слот движка.
    let ptr = unsafe { *slot } as usize;
    if ptr == 0 {
        out.push(Check::fail("r_studio_interface_t", "указатель пустой — клиент ещё не инициализировался"));
        return out;
    }

    let expected = client_base + client::STUDIO_INTERFACE;
    out.push(Check::require(
        ptr == expected || process::in_range(ptr, client_base, 0x20_0000),
        "g_pStudioInterface указывает в client.dll",
        format!("{ptr:#x} (ожидалось {expected:#x})"),
    ));

    if let Some(iface) = studio_interface() {
        out.push(Check::require(
            iface.version == StudioInterface::VERSION,
            "r_studio_interface_t::version == 1",
            format!("{}", iface.version),
        ));
        let draw_model = iface.draw_model.map(|f| f as usize).unwrap_or(0);
        let draw_player = iface.draw_player.map(|f| f as usize).unwrap_or(0);
        out.push(Check::require(
            process::is_code(draw_model, client_base) && process::is_code(draw_player, client_base),
            "DrawModel/DrawPlayer — код клиента",
            format!("{draw_model:#x} / {draw_player:#x}"),
        ));
    }

    // Статическая проверка: адрес экспорта в hw.dll должен указывать на реализацию в клиенте.
    // SAFETY: чтение статического указателя движка.
    let entry = unsafe { process::read::<usize>(hw_base + hw::P_CLIENT_STUDIO_ENTRY) };
    out.push(Check::require(
        process::in_range(entry, client_base, 0x20_0000),
        "hw: указатель на HUD_GetStudioModelInterface",
        format!("{entry:#x}"),
    ));

    out
}

/// Объект `CStudioModelRenderer` и его vtable.
fn check_renderer(client_base: usize) -> Vec<Check> {
    let mut out = Vec::new();
    let renderer = match StudioRenderer::global() {
        Some(r) => r,
        None => {
            out.push(Check::fail("CStudioModelRenderer", "client.dll не загружен"));
            return out;
        }
    };

    let vtable = renderer.vtable();
    out.push(Check::require(
        process::in_range(vtable, client_base, 0x20_0000),
        "CStudioModelRenderer::vtable в client.dll",
        format!("{vtable:#x} (ожидалось {:#x})", client_base + client::STUDIO_RENDERER_VTABLE),
    ));

    for (slot, label) in [
        (client::VT_INIT, "Init"),
        (client::VT_STUDIO_DRAW_MODEL, "StudioDrawModel"),
        (client::VT_STUDIO_DRAW_PLAYER, "StudioDrawPlayer"),
        (client::VT_STUDIO_SETUP_BONES, "StudioSetupBones"),
        (client::VT_STUDIO_RENDER_MODEL, "StudioRenderModel"),
    ] {
        let addr = renderer.method(slot).unwrap_or(0);
        out.push(Check::require(
            process::is_code(addr, client_base),
            "vtable: метод — код клиента",
            format!("{label} = {addr:#x}"),
        ));
    }

    if let Some(model) = renderer.studio_model() {
        let hdr = model.header();
        out.push(Check::require(
            hdr.id == IDST && hdr.version == VERSION,
            "текущая модель: IDST/version 10",
            format!("id={:#x} ver={} bones={}", hdr.id, hdr.version, hdr.numbones),
        ));
    } else {
        out.push(Check::pass("текущая модель", "нет активной модели (вне кадра отрисовки)"));
    }

    out
}

/// Копия таблицы api в клиенте должна совпадать с движковой.
fn check_client_copy_matches(client_base: usize) -> Vec<Check> {
    let mut out = Vec::new();
    let Some(engine) = EngineStudioApi::for_engine() else {
        out.push(Check::fail("копия api в клиенте", "движковая таблица не читается"));
        return out;
    };
    let Some(copy) = EngineStudioApi::client_copy() else {
        out.push(Check::fail("копия api в клиенте", "копия не читается"));
        return out;
    };

    let mut mismatched = Vec::new();
    for offset in (0..EngineStudioApi::SLOT_COUNT * 4).step_by(4) {
        if engine.slot(offset).map(|p| p as usize) != copy.slot(offset).map(|p| p as usize) {
            mismatched.push(format!("+0x{offset:02x}"));
        }
    }

    out.push(Check::require(
        mismatched.is_empty(),
        "копия api в клиенте совпадает с движком",
        if mismatched.is_empty() {
            format!("{:#x}", client_base + client::ENGINE_STUDIO_API_COPY)
        } else {
            format!("расходятся слоты: {}", mismatched.join(" "))
        },
    ));
    out
}

fn fmt_opt(base: Option<usize>) -> String {
    match base {
        Some(base) => format!("base {base:#x}"),
        None => "не загружен".to_string(),
    }
}

/// Проверяет, что по адресу лежит заголовок студийной модели.
///
/// # Safety
/// `header` должен либо быть null, либо указывать на читаемую память.
pub unsafe fn looks_like_studio_header(header: *const c_void) -> bool {
    if header.is_null() {
        return false;
    }
    // SAFETY: контракт функции.
    let id = unsafe { process::read::<i32>(header as usize) };
    let version = unsafe { process::read::<i32>(header as usize + 4) };
    id == IDST && version == VERSION
}