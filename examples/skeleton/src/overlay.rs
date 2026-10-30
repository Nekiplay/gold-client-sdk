//! Подмена интерфейса студийного рендера и рисование скелета.
//!
//! Вся работа с движком — через [`golden_sdk`], удобный слой — [`golden_std`]:
//! здесь только хук и то, что должно быть на экране.

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use golden_sdk::hook::SwappedInterface;
use golden_sdk::math::Vec3;
use golden_sdk::player::Team;
use golden_sdk::renderer::{self, StudioInterface, StudioRenderer, interface_slot};
use golden_std::{Color, Overlay, Player, PoseCache};

/// Оригинальный интерфейс, который был до подмены.
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// Оригинальный `StudioDrawPlayer` (движок зовёт его для игроков напрямую).
static ORIGINAL_PLAYER: AtomicUsize = AtomicUsize::new(0);
/// Наша структура `r_studio_interface_t` (живёт в куче, адрес стабилен).
static OURS: AtomicUsize = AtomicUsize::new(0);
/// [`SwappedInterface`] — нужен, чтобы возвращать подмену после сброса.
static SWAP: AtomicUsize = AtomicUsize::new(0);

/// Сколько скелетов нарисовано.
static DRAWN: AtomicU64 = AtomicU64::new(0);
/// Сколько моделей пропущено (не игрок).
static SKIPPED: AtomicU64 = AtomicU64::new(0);
/// Сколько вызовов без разбираемой модели.
static NO_MODEL: AtomicU64 = AtomicU64::new(0);
/// Сколько моделей-игроков отброшено проверкой геометрии.
static REJECTED: AtomicU64 = AtomicU64::new(0);
/// Сколько скелетов осталось без команды (кастомные модели).
static NO_TEAM: AtomicU64 = AtomicU64::new(0);
/// Сколько вызовов пришло через `DrawModel`.
static CALLS_MODEL: AtomicU32 = AtomicU32::new(0);
/// Сколько вызовов пришло через `DrawPlayer`.
static CALLS_PLAYER: AtomicU32 = AtomicU32::new(0);
/// Перехват `StudioSetupBones` (держим, чтобы трамплин не убрали).
static BONES_DETOUR: Mutex<Option<golden_sdk::hook::Detour>> = Mutex::new(None);
/// Перехват `StudioRenderModel`.
static RENDER_MODEL_DETOUR: Mutex<Option<golden_sdk::hook::Detour>> = Mutex::new(None);
/// Оригинал `StudioRenderModel`.
static RENDER_MODEL_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// Сколько раз нарисовали контуры (для отчёта).
static OUTLINED: AtomicU32 = AtomicU32::new(0);
/// Счётчик попыток рисовать контуры.
static OUTLINES: AtomicU32 = AtomicU32::new(0);
/// Оригинал `StudioSetupBones`.
static SETUP_BONES_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// Позы игроков, снятые в перехвате `StudioSetupBones`.
static POSES: PoseCache = PoseCache::new();
/// Модель оружия в руках по номеру игрока (заполняется в `StudioDrawPlayer`).
static WEAPONS: [AtomicI32; 64] = [const { AtomicI32::new(0) }; 64];
/// Последняя нарисованная сущность и время — чтобы не рисовать её дважды за кадр.
static LAST_DRAWN: Mutex<Option<(usize, Instant)>> = Mutex::new(None);
/// Имя последней рассмотренной модели (для отчёта).
static LAST_MODEL: Mutex<Option<String>> = Mutex::new(None);
/// Когда последний раз отчитывались.
static LAST_REPORT: Mutex<Option<Instant>> = Mutex::new(None);

/// Цвет, если команду определить не удалось (кастомный сервер без `TeamInfo`).
const UNKNOWN_COLOR: Color = Color::WHITE;
/// Толщина линий контуров в пикселях.
const OUTLINE_WIDTH: f32 = 2.0;
/// Толщина линий скелета в пикселях.
const LINE_WIDTH: f32 = 1.6;

/// Ставит перехват `StudioRenderModel` (слот 8 vtable рендерера).
///
/// Берём адрес из живой vtable: у объекта стоит vtable производного класса.
fn install_render_model() {
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let Some(entry) = renderer.method(golden_sdk::renderer::SLOT_RENDER_MODEL) else {
        golden_sdk::log!("в vtable рендерера нет слота отрисовки меша");
        return;
    };
    // SAFETY: `entry` — `void StudioRenderModel(void)` этого объекта; подменяем
    // на функцию с той же сигнатурой.
    let detour = match unsafe {
        golden_sdk::hook::Detour::new(entry, hook_render_model as *const ())
    } {
        Ok(detour) => detour,
        Err(error) => {
            golden_sdk::log!("не удалось перехватить StudioRenderModel: {error}");
            return;
        }
    };
    if let Ok(mut slot) = RENDER_MODEL_DETOUR.lock() {
        if slot.is_none() {
            if let Err(error) = detour.enable() {
                golden_sdk::log!("не удалось включить перехват рендера модели: {error}");
                return;
            }
            RENDER_MODEL_ORIGINAL.store(detour.trampoline() as usize, Ordering::Release);
            golden_sdk::log!("перехват StudioRenderModel включён ({entry:#x})");
            *slot = Some(detour);
        }
    }
}

/// Устанавливает подмену интерфейса (ждёт, пока клиент отдаст свой рендерер).
pub fn install() -> Result<(), String> {
    let prev = wait_for_interface(Duration::from_secs(120))
        .ok_or_else(|| "движок так и не отдал интерфейс рендерера".to_string())?;
    ORIGINAL.store(prev as usize, Ordering::Release);

    let ours = our_interface(prev);
    // SAFETY: `ours` живёт в куче до конца процесса, поля совпадают по ABI.
    let swap = unsafe { SwappedInterface::install(ours) }.map_err(|e| e.to_string())?;
    let swap = Box::leak(Box::new(swap));
    SWAP.store(swap as *mut SwappedInterface as usize, Ordering::Release);
    Ok(())
}

/// Перехват `CStudioModelRenderer::StudioSetupBones` (`client.dll+0x0ACB20`).
///
/// Кости для отрисовки нужны те, что посчитаны **внутри** рендера этого
/// игрока: после возврата из `StudioDrawPlayer` движок их уже не хранит.
/// Здесь ловим момент, когда они гарантированно свежие.
fn install_setup_bones() {
    let Some(base) = golden_sdk::process::client_base() else {
        return;
    };
    // Точка входа берётся из живой vtable объекта рендерера: у объекта стоит
    // vtable производного класса, и её слот может отличаться от базовой.
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let vtable = renderer.vtable();
    if vtable == 0 {
        return;
    }
    // SAFETY: vtable проверен на читаемость в `StudioRenderer`.
    let entry = unsafe { golden_sdk::process::read::<usize>(vtable + 0x1C) };
    golden_sdk::log!("vtable рендерера {vtable:#x}, слот 7 = {entry:#x}");
    let target = if entry != 0 { entry } else { base + 0x0ACB20 };
    golden_sdk::log!("перехват StudioSetupBones: {target:#x}");
    // SAFETY: адрес — начало `StudioSetupBones` в клиенте этой сборки (см.
    // `docs/STUDIO_REVERSE.md`), сигнатура `int(int)` thiscall.
    let detour = match unsafe {
        golden_sdk::hook::Detour::new(target, hook_setup_bones as *const ())
    } {
        Ok(detour) => detour,
        Err(error) => {
            golden_sdk::log!("не удалось перехватить StudioSetupBones: {error}");
            return;
        }
    };
    if let Ok(mut slot) = BONES_DETOUR.lock() {
        if slot.is_none() {
            if let Err(error) = detour.enable() {
                golden_sdk::log!("не удалось включить перехват костей: {error}");
                return;
            }
            SETUP_BONES_ORIGINAL.store(detour.trampoline() as usize, Ordering::Release);
            golden_sdk::log!("перехват костей включён");
            *slot = Some(detour);
        }
    }
}

/// Возвращает подмену, если клиент её сбросил. `true` — пришлось вернуть.
pub fn watchdog() -> bool {
    let ptr = SWAP.load(Ordering::Acquire) as *const SwappedInterface;
    if ptr.is_null() {
        return false;
    }
    // SAFETY: указатель на утёкший `SwappedInterface`, он живёт до конца процесса.
    unsafe { (*ptr).reinstall() }
}

/// Строка отчёта, если с прошлого раза что-то изменилось.
pub fn take_report() -> Option<String> {
    let calls = CALLS_MODEL.load(Ordering::Relaxed) + CALLS_PLAYER.load(Ordering::Relaxed);
    if calls == 0 {
        return None;
    }
    {
        let mut last = LAST_REPORT.lock().ok()?;
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return None;
        }
        *last = Some(Instant::now());
    }
    let model = LAST_MODEL.lock().ok()?.clone().unwrap_or_default();
    Some(format!(
        "скелетов: {}, вызовов: {} (DrawModel {} / DrawPlayer {}), не игрок: {}, без модели: {}, без костей: {}, без команды: {}, модель: {model}",
        DRAWN.load(Ordering::Relaxed),
        calls,
        CALLS_MODEL.load(Ordering::Relaxed),
        CALLS_PLAYER.load(Ordering::Relaxed),
        SKIPPED.load(Ordering::Relaxed),
        NO_MODEL.load(Ordering::Relaxed),
        REJECTED.load(Ordering::Relaxed),
        NO_TEAM.load(Ordering::Relaxed)
    ))
}

/// Ждёт, пока в `hw.dll` появится указатель на интерфейс клиента.
fn wait_for_interface(timeout: Duration) -> Option<*mut StudioInterface> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(slot) = interface_slot() {
            // SAFETY: слот — статическое поле движка.
            let current = unsafe { *slot };
            if !current.is_null() {
                // Дополнительно ждём, пока клиент отдаст свой рендерер: у
                // дефолтного движкового нет костей, рисовать по нему нечего.
                if renderer::is_client_renderer_active()
                    && StudioRenderer::global().is_some_and(|r| r.is_initialized())
                {
                    return Some(current);
                }
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Наша структура интерфейса (создаётся один раз).
fn our_interface(prev: *const StudioInterface) -> *mut StudioInterface {
    let existing = OURS.load(Ordering::Acquire);
    if existing != 0 {
        return existing as *mut StudioInterface;
    }
    // SAFETY: `prev` — живой интерфейс движка/клиента.
    let draw_player = unsafe { (*prev).draw_player };
    match draw_player {
        Some(f) => ORIGINAL_PLAYER.store(f as usize, Ordering::Release),
        None => ORIGINAL_PLAYER.store(0, Ordering::Release),
    }
    let ours = StudioInterface {
        version: StudioInterface::VERSION,
        draw_model: Some(hook_draw_model),
        draw_player: Some(hook_draw_player),
    };
    let leaked = Box::leak(Box::new(ours));
    OURS.store(leaked as *mut StudioInterface as usize, Ordering::Release);
    leaked as *mut StudioInterface
}

/// Наш `StudioDrawModel`: только считает вызовы.
///
/// Игроков движок рисует через `StudioDrawPlayer`; рисовать отсюда нельзя —
/// на движении и склонах один игрок попадает сюда повторно (тень, второй
/// проход), и на экране смешивались бы два скелета.
///
/// # Safety
/// Вызывается движком из рендер-потока с активным GL-контекстом.
unsafe extern "C" fn hook_draw_model(flags: i32) -> i32 {
    let prev = ORIGINAL.load(Ordering::Acquire) as *const StudioInterface;
    if prev.is_null() {
        return 0;
    }
    // SAFETY: `prev` — интерфейс, который был до подмены, и он жив.
    let result = unsafe { (*prev).call_draw_model(flags) };
    CALLS_MODEL.fetch_add(1, Ordering::Relaxed);
    result
}

/// Наш `StudioRenderModel`: тело рисуется как обычно, затем та же геометрия
/// перерисовывается линиями в цвете команды.
///
/// Перехват нужен именно здесь, а не в `StudioDrawPlayer`: после возврата из
/// `StudioDrawPlayer` движок восстанавливает матрицы, и нарисованное «снаружи»
/// уходит за пределы экрана.
///
/// # Safety
/// Вызывается движком (thiscall) из рендер-потока с активным GL-контекстом.
unsafe extern "thiscall" fn hook_render_model(this: *mut c_void) {
    let orig = RENDER_MODEL_ORIGINAL.load(Ordering::Acquire);
    if orig == 0 {
        return;
    }
    // SAFETY: сохранённый адрес оригинала `void StudioRenderModel(void)`.
    let f: unsafe extern "thiscall" fn(*mut c_void) = unsafe { core::mem::transmute(orig) };
    unsafe { f(this) };

    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let model = renderer.studio_model();
    if OUTLINES.fetch_add(1, Ordering::Relaxed) < 5 {
        golden_sdk::log!(
            "контур: модель {:?} сущность {:?} слот {:#x}",
            model.as_ref().map(|model| model.name()),
            renderer.current_entity().map(|entity| entity.addr()),
            this as usize
        );
    }
    let Some(model) = model else {
        return;
    };
    if !model.is_player_model() {
        return;
    }
    let Some(color) = entity_color(&renderer) else {
        if OUTLINES.load(Ordering::Relaxed) < 10 {
            golden_sdk::log!("контур: цвет команды не определён");
        }
        return;
    };
    let Some(gl) = (unsafe { golden_sdk::draw::Gl::get() }) else {
        return;
    };
    // SAFETY: тот же объект и тот же контекст, что и у оригинала.
    unsafe {
        gl.wireframe(color, OUTLINE_WIDTH, || {
            f(this);
        });
    }
    OUTLINED.fetch_add(1, Ordering::Relaxed);
}

/// Цвет команды для сущности, которую сейчас рисует движок.
fn entity_color(renderer: &StudioRenderer) -> Option<Color> {
    let entity = renderer.current_entity();
    let model = renderer.studio_model()?;
    let index = entity
        .map(|entity| entity.player_index())
        .filter(|index| (0..=63).contains(index));
    let player = index.and_then(|index| entity.and_then(|raw| Player::from_raw(index, raw)));
    player
        .and_then(|player| player.team())
        .or_else(|| Team::from_model_name(model.name()))
        .map(|team| team.color())
}

/// Наш `StudioDrawPlayer`: оригинал, затем скелет.
///
/// # Safety
/// Вызывается движком из рендер-потока с активным GL-контекстом.
unsafe extern "C" fn hook_draw_player(flags: i32, player_info: *mut c_void) -> i32 {
    let prev = ORIGINAL_PLAYER.load(Ordering::Acquire) as *const ();
    let result = if prev.is_null() {
        0
    } else {
        // SAFETY: сигнатура `StudioDrawPlayer(int, player_info_s*)` из HLSDK.
        let f: unsafe extern "C" fn(i32, *mut c_void) -> i32 = unsafe { core::mem::transmute(prev) };
        unsafe { f(flags, player_info) }
    };
    CALLS_PLAYER.fetch_add(1, Ordering::Relaxed);
    // Перехват костей ставим отсюда: `StudioDrawPlayer` вызывается заведомо,
    // и к этому моменту объект рендерера уже сконструирован.
    if BONES_DETOUR.lock().is_ok_and(|slot| slot.is_none()) {
        install_setup_bones();
    }
    if RENDER_MODEL_DETOUR
        .lock()
        .is_ok_and(|slot| slot.is_none())
    {
        install_render_model();
    }
    // Оружие и номер игрока запоминаем по номеру: отрисовка скелета — ниже, по
    // снимку костей, снятому в `StudioSetupBones`.
    let index = golden_sdk::engine::player_of_draw(player_info);
    let weapon = golden_sdk::engine::player_weapon_model(player_info);
    if let (Some(index), Some(weapon)) = (index, weapon) {
        if (0..64).contains(&index) {
            WEAPONS[index as usize].store(weapon, Ordering::Release);
        }
    }
    draw_skeleton(Some(DrawInfo {
        index,
        weapon: weapon.or_else(|| {
            index
                .map(|index| WEAPONS[index as usize].load(Ordering::Acquire))
                .filter(|weapon| *weapon > 0)
        }),
    }));
    result
}

/// Наш `StudioSetupBones`: после оригинала кости этого игрока уже посчитаны.
///
/// # Safety
/// Вызывается движком (thiscall) из рендер-потока.
unsafe extern "thiscall" fn hook_setup_bones(this: *mut c_void, flags: i32) -> i32 {
    let prev = SETUP_BONES_ORIGINAL.load(Ordering::Acquire);
    let result = if prev == 0 {
        0
    } else {
        // SAFETY: сохранённый адрес оригинала той же сигнатуры.
        let f: unsafe extern "thiscall" fn(*mut c_void, i32) -> i32 =
            unsafe { core::mem::transmute(prev) };
        unsafe { f(this, flags) }
    };
    // Снимок костей: внутри `StudioSetupBones` они посчитаны именно для этой
    // сущности. Рисовать здесь нельзя — тело рисуется после нас и закрывает
    // линии, поэтому позу складываем в кэш, а рисуем в `StudioDrawPlayer`.
    if let Some(renderer) = StudioRenderer::global() {
        if let (Some(entity), Some(model)) = (renderer.current_entity(), renderer.studio_model()) {
            if model.is_player_model() {
                POSES.capture(entity.addr(), &renderer, &model);
            }
        }
    }
    result
}

/// Что движок сообщил о рисуемом игроке в `StudioDrawPlayer`.
#[derive(Debug, Clone, Copy)]
struct DrawInfo {
    /// Номер игрока (`player_info + 0x04`).
    index: Option<i32>,
    /// Номер модели оружия в руках (`player_info + 0xB4`).
    weapon: Option<i32>,
}

/// Рисует скелет модели, которую клиент только что нарисовал.
///
/// `draw` — данные от `StudioDrawPlayer`; `None`, если пришли из
/// `StudioDrawModel`, где движок ничего о игроке не сообщает.
fn draw_skeleton(draw: Option<DrawInfo>) {
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    if !renderer.is_initialized() {
        return;
    }

    // Перехват `StudioSetupBones` ставится из `StudioDrawPlayer` (см. ниже).

    let Some(model) = renderer
        .studio_model()
        .or_else(|| renderer.entity_model())
    else {
        NO_MODEL.fetch_add(1, Ordering::Relaxed);
        return;
    };

    let entity = renderer.current_entity();

    if !model.is_player_model() {
        SKIPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // Проверка геометрии отбрасывает матрицы, посчитанные для другой модели,
    // и скелеты в системе координат вьюмодели.
    //
    // Позиции берём из снимка, снятого в `StudioSetupBones`: только там они
    // принадлежат этому игроку.
    let pose = entity.and_then(|entity| POSES.get(entity.addr(), &model));
    if POSES.len() < 1 && REJECTED.load(Ordering::Relaxed) < 5 {
        let captured = POSES.capture(
            entity.map(|entity| entity.addr()).unwrap_or(0),
            &renderer,
            &model,
        );
        golden_sdk::log!(
            "поза: entity {:?} модель {} захват {} в кэше {}",
            entity.map(|entity| entity.addr()),
            model.name(),
            captured,
            POSES.len()
        );
    }

    // Одна сущность за кадр: движок иногда рисует модель игрока дважды подряд.
    if let Some(entity) = entity {
        if let Ok(mut last) = LAST_DRAWN.lock() {
            if let Some((addr, at)) = *last {
                if addr == entity.addr() && at.elapsed() < Duration::from_millis(2) {
                    return;
                }
            }
            *last = Some((entity.addr(), Instant::now()));
        }
    }

    if let Ok(mut last) = LAST_MODEL.lock() {
        *last = Some(model.name().to_string());
    }

    // Номер игрока: из `StudioDrawPlayer`, а если пришли из `StudioDrawModel` —
    // из самой сущности (`+0x04`). Без верного номера подпись и цвет берутся
    // у чужого игрока, поэтому без номера скелет не рисуем.
    let index = draw
        .and_then(|draw| draw.index)
        .or_else(|| entity.map(|entity| entity.player_index()))
        .filter(|index| (0..=63).contains(index));
    let Some(index) = index else {
        NO_TEAM.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let weapon = draw.and_then(|draw| draw.weapon);
    let player = entity
        .and_then(|raw| Player::from_raw(index, raw))
        .map(|player| player.with_weapon_model(weapon));
    let team = player
        .and_then(|player| player.team())
        .or_else(|| Team::from_model_name(model.name()));
    let color = match team {
        Some(team) => team.color(),
        None => {
            NO_TEAM.fetch_add(1, Ordering::Relaxed);
            UNKNOWN_COLOR
        }
    };

    let Some(overlay) = Overlay::new() else {
        return;
    };

    // Контуры рисуются в перехвате `StudioRenderModel` (см. `hook_render_model`):
    // только там движок держит нужные GL-матрицы.

    // Скелет — только по свежему снимку позы: осевшие в памяти кости
    // принадлежат другому игроку, рисовать по ним нельзя.
    let mut segments = Vec::new();
    if let Some(pose) = &pose {
        if pose.origins.len() == model.bones().len() {
            segments = pose.segments(&model);
            overlay.skeleton_lines(&segments, color, LINE_WIDTH);
        }
    }
    if segments.is_empty() {
        REJECTED.fetch_add(1, Ordering::Relaxed);
    }

    if let Some((point, radius)) = pose.as_ref().and_then(|pose| pose.head) {
        overlay.circle_at(point, radius, color, LINE_WIDTH);
    }

    // Подписи: имя над головой, оружие (и бомба/кит) — под ногами.
    let extremes = pose
        .as_ref()
        .and_then(|pose| pose.extremes())
        .or_else(|| extremes(&segments))
        .or_else(|| {
            // Без снимка позы — по габаритам модели от позиции сущности.
            let (min, max) = model.hull()?;
            let origin = entity.map(|entity| entity.origin())?;
            Some((origin + min, origin + max))
        });
    if let Some((lowest, highest)) = extremes {
        let name = player.map(|player| player.name()).unwrap_or_default();
        if !name.is_empty() {
            overlay.label(highest, &name, color, true);
        }
        if let Some(text) = under_text(player) {
            overlay.label(lowest, &text, color, false);
        }
    }

    DRAWN.fetch_add(1, Ordering::Relaxed);
}

/// Самая низкая и самая высокая точки скелета — к ним привязываются подписи.
fn extremes(segments: &[(Vec3, Vec3)]) -> Option<(Vec3, Vec3)> {
    let mut lowest: Option<Vec3> = None;
    let mut highest: Option<Vec3> = None;
    for (a, b) in segments {
        for point in [a, b] {
            if lowest.is_none_or(|q| point.z < q.z) {
                lowest = Some(*point);
            }
            if highest.is_none_or(|q| point.z > q.z) {
                highest = Some(*point);
            }
        }
    }
    Some((lowest?, highest?))
}

/// Подпись под скелетом: оружие, бомба, дефуз-кит.
fn under_text(player: Option<golden_std::Player>) -> Option<String> {
    let player = player?;
    let mut parts = Vec::new();
    if let Some(weapon) = player.weapon() {
        parts.push(weapon);
    }
    if player.has_bomb() {
        parts.push("C4".to_string());
    }
    if player.has_defuser() {
        parts.push("Kit".to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}
