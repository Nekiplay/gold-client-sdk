//! Перехваты примера: конец 3D-сцены, расчёт и отрисовка.
//!
//! Схема та же, что в `example-hitboxes`:
//!
//! 1. [`install`] подменяет `draw_player` в интерфейсе рендерера через
//!    [`golden_std::hooks::interface`] — это единственная точка, которую движок
//!    подхватывает сам.
//! 2. Из первого вызова `draw_player` (мы уже в рендер-потоке) ставится
//!    перехват конца 3D-сцены `HUD_DrawNormalTriangles`.
//! 3. В конце сцены пересчитывается предсказание и рисуется траектория.
//!
//! Так же, как в ESP и хитбоксах, рисуем **до** оригинала конца сцены: после
//! него движок оставляет включённые массивы вершин и цветов, и чужой вызов в
//! этом состоянии падает.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use golden_sdk::draw::Color;
use golden_sdk::grenade::Kind;
use golden_sdk::module::InFlight;
use golden_sdk::offsets::{player_info, player_state};
use golden_sdk::renderer::{StudioInterface, StudioRenderer};
use golden_std::Game;
use golden_std::Vec3;
use golden_std::grenade::{self, Prediction, Seen};
use golden_std::render::{DRAW_NORMAL_TRIANGLES_RVA, Overlay};

use crate::report;

/// Пересчёт 30 раз в секунду: траектория достаточно плавная, но кадр не
/// тратит на баллистику всё время. Чаще — заметно проседает FPS.
const REFRESH: Duration = Duration::from_millis(33);

/// Цели (урон/ослепление) пересчитываются реже траектории: LOS-лучи по
/// игрокам стоят миллисекунды, а игроки за 0.25 с почти не двигаются.
const TARGETS_REFRESH: Duration = Duration::from_millis(250);

/// Показывать ли траекторию.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Тип гранаты: `0` — HE, `1` — флешка.
static KIND: AtomicU8 = AtomicU8::new(0);

/// Команда попросила пересчитать и напечатать отчёт в ближайшем кадре.
static RECALC: AtomicBool = AtomicBool::new(false);

/// Указатель на клиентский `StudioDrawPlayer`.
static ORIGINAL_PLAYER: AtomicUsize = AtomicUsize::new(0);

/// Указатель на клиентский `StudioDrawModel` — тот, что рисует оружие в руках.
static ORIGINAL_MODEL: AtomicUsize = AtomicUsize::new(0);

/// Показ включён вручную командой и не должен гаснуть от смены оружия.
static FORCED: AtomicBool = AtomicBool::new(false);

/// Последняя увиденная view-модель — чтобы не заливать лог каждым кадром.
static LAST_MODEL: Mutex<Option<String>> = Mutex::new(None);

/// Поставлен ли перехват `StudioDrawModel` рендерера.
static WEAPON_HOOKED: AtomicBool = AtomicBool::new(false);

/// Поставлен ли перехват конца сцены.
static SCENE_HOOKED: AtomicBool = AtomicBool::new(false);

/// Был ли первый вызов `draw_player` — из него ставим остальные перехваты.
static FIRST_DRAW: AtomicBool = AtomicBool::new(false);

/// Последнее предсказание и время расчёта.
static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

/// Позы игроков, снятые в `draw_player` (слот, origin, углы).
static SEEN: Mutex<Vec<Seen>> = Mutex::new(Vec::new());

/// Последняя ошибка расчёта, чтобы не заливать лог каждый кадр.
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Время последнего медленного кадра — для редкой диагностики.
static SLOW_LOG: Mutex<Option<Instant>> = Mutex::new(None);

#[derive(Debug)]
struct Cached {
    prediction: Prediction,
    at: Instant,
    targets_at: Instant,
    /// Готовые подписи: имя, урон/длительность, родство. Считаются вместе с
    /// целями, а не в кадре: чтение игроков на каждую подпись каждый кадр —
    /// это и была просадка FPS, когда в радиусе взрыва есть игроки.
    labels: Vec<(Vec3, String, Color)>,
}

/// Сколько живёт след за летящей гранатой.
const TRAIL_LIFE: Duration = Duration::from_millis(1200);

/// Минимальный шаг между точками следа: чаще — лишние вершины и дёрганье.
const TRAIL_STEP: f32 = 12.0;

/// Скорость, с которой граната считается брошенной. Лежащая граната-подбор
/// тоже есть в списке сущностей, но она стоит на месте.
const TRAIL_CONFIRM_SPEED: f32 = 260.0;

/// Как часто снимаем позиции гранат из списка сущностей.
const TRAIL_POLL: Duration = Duration::from_millis(16);

/// Когда след перестаёт обновляться — значит граната взорвалась или скрылась.
const TRAIL_GONE: Duration = Duration::from_millis(250);

/// Сколько следов помним одновременно.
const TRAILS_MAX: usize = 8;

/// Точка пути, снятая с настоящей гранаты.
#[derive(Clone, Copy)]
struct Sample {
    pos: Vec3,
    at: Instant,
}

/// След за конкретной летящей гранатой.
struct Trail {
    entity: i32,
    kind: Kind,
    /// Граната действительно летела, а не лежала подбором на земле.
    flying: bool,
    /// Когда гранату видели в последний раз: по этому следу и гаснем.
    last_seen: Instant,
    samples: Vec<Sample>,
}

/// Следы брошенных гранат: настоящий путь, снятый со списка сущностей.
static TRAILS: Mutex<Vec<Trail>> = Mutex::new(Vec::new());

/// Когда последний раз опрашивали список сущностей.
static LAST_POLL: Mutex<Option<Instant>> = Mutex::new(None);

/// Текущий тип гранаты.
pub fn kind() -> Kind {
    kind_from_code(KIND.load(Ordering::Acquire))
}

/// Код типа в атомарном состоянии: `KIND` хранит именно его.
const fn kind_code(kind: Kind) -> u8 {
    match kind {
        Kind::He => 0,
        Kind::Flash => 1,
        Kind::Smoke => 2,
    }
}

/// Обратный перевод кода в тип.
const fn kind_from_code(code: u8) -> Kind {
    match code {
        1 => Kind::Flash,
        2 => Kind::Smoke,
        _ => Kind::He,
    }
}

/// Включает показ гранаты: `show(Kind::He)`, `show(Kind::Flash)` или
/// `show(Kind::Smoke)`.
pub fn show(kind: Kind) {
    FORCED.store(true, Ordering::Release);
    KIND.store(kind_code(kind), Ordering::Release);
    ENABLED.store(true, Ordering::Release);
    RECALC.store(true, Ordering::Release);
}

/// Выключает отрисовку.
pub fn hide() {
    FORCED.store(false, Ordering::Release);
    ENABLED.store(false, Ordering::Release);
}

/// Подменяет интерфейс рендерера.
///
/// # Errors
/// Строка с причиной, если движок ещё не отдал клиентский интерфейс.
pub fn install() -> Result<(), String> {
    let Some(current) = golden_sdk::renderer::studio_interface().cloned() else {
        return Err("движок ещё не выставил интерфейс рендерера".to_string());
    };
    let Some(original) = current.draw_player else {
        return Err("у интерфейса нет draw_player: клиентский рендерер ещё не активен".to_string());
    };
    ORIGINAL_PLAYER.store(original as usize, Ordering::Release);
    if let Some(model) = current.draw_model {
        ORIGINAL_MODEL.store(model as usize, Ordering::Release);
    }

    let ours = StudioInterface {
        version: StudioInterface::VERSION,
        // Через `draw_model` узнаём, какая модель в руках: движок зовёт его
        // для view-модели оружия. Оригинал вызываем сами.
        draw_model: current
            .draw_model
            .map(|_| hook_draw_model as unsafe extern "C" fn(i32) -> i32),
        draw_player: Some(hook_draw_player),
    };
    let ours = Box::leak(Box::new(ours)) as *mut StudioInterface;
    // SAFETY: структура того же типа, что у клиента; `hooks::interface`
    // восстановит слот при выгрузке.
    unsafe { golden_std::hooks::interface(ours) }
        .map_err(|error| format!("подмена интерфейса не встала: {error}"))?;
    Ok(())
}

// Клиентский `StudioDrawModel`: по модели в руках выбираем тип гранаты.
unsafe extern "C" fn hook_draw_model(index: i32) -> i32 {
    let _inside = InFlight::enter();
    detect_held(index);
    let original = ORIGINAL_MODEL.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    // SAFETY: в `install` сюда записан указатель клиентского `StudioDrawModel`
    // ровно этой сигнатуры.
    let call: unsafe extern "C" fn(i32) -> i32 = unsafe { core::mem::transmute(original) };
    // SAFETY: контракт передан вызывающим (рендер-поток с активным GL).
    unsafe { call(index) }
}

/// Определяет гранату в руках по имени view-модели.
///
/// `draw_model` движок зовёт именно для модели в руках, поэтому имя
/// `models/v_hegrenade.mdl` — надёжный признак, что игрок достал HE.
fn detect_held(index: i32) {
    let Some(name) = golden_sdk::engine::EngineStudioApi::for_engine()
        .and_then(|api| api.model_name_by_index(index))
    else {
        return;
    };
    apply_held_name(&name.to_ascii_lowercase());
}

/// Разбирает имя модели, которую движок рисует сейчас.
///
/// Имя берём из `StudioRenderer::studio_model()` в перехвате `StudioDrawModel`:
/// интерфейсный `draw_model` Gold Client не зовёт, а слот 3 рендерера — зовёт
/// ровно для модели в руках.
fn detect_held_from_renderer() {
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let Some(model) = renderer.studio_model() else {
        return;
    };
    apply_held_name(&model.name().to_ascii_lowercase());
}

/// Оружие локального игрока из его сущности.
///
/// Работает в `draw_player`, где движок уже выставил текущую сущность. Этот
/// путь не зависит от перехвата `StudioDrawModel` и переживает смену карты.
fn detect_held_from_entity() {
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let Some(entity) = renderer.current_entity() else {
        return;
    };
    let Some(name) = entity.weapon_model_name() else {
        return;
    };
    let name = name.to_ascii_lowercase();
    // `weaponmodel` локальной сущности — мировая модель оружия (`w_usp.mdl`),
    // а не та, что в руках: она отстаёт от реального оружия и переключала бы
    // показ каждый кадр. Берём отсюда только view-модель.
    if !is_view_model(&name) {
        return;
    }
    apply_held_name(&name);
}

/// View-модель (то, что в руках у локального игрока)?
///
/// `StudioDrawModel` рендерера зовётся и для мировых моделей оружия в руках
/// других игроков (`w_usp.mdl`), поэтому значимо только имя с префиксом `v_`.
/// Путь `models/` движок может и не писать — снимаем его, если он есть.
fn is_view_model(name: &str) -> bool {
    name.strip_prefix("models/")
        .unwrap_or(name)
        .starts_with("v_")
}

/// Тип брошенной гранаты по имени мировой модели (`models/w_hegrenade.mdl`).
fn world_kind(name: &str) -> Option<Kind> {
    let name = name.strip_prefix("models/").unwrap_or(name);
    if !name.starts_with("w_") {
        return None;
    }
    if name.contains("hegrenade") {
        Some(Kind::He)
    } else if name.contains("flashbang") {
        Some(Kind::Flash)
    } else if name.contains("smokegrenade") {
        Some(Kind::Smoke)
    } else {
        None
    }
}

/// Запоминает положение брошенных гранат — и своих, и чужих.
///
/// Раньше позиция снималась с той модели, которую движок рисует в этот момент,
/// и это работало только для гранат рядом с камерой. Список сущностей даёт все
/// брошенные гранаты сразу: своя и чужие — одни и те же `w_*grenade` сущности
/// сервера. Путь оттуда точный — с настоящими отскоками и остановками.
fn capture_flying() {
    let now = Instant::now();
    {
        let mut last = lock(&LAST_POLL);
        if last.is_some_and(|at| now.duration_since(at) < TRAIL_POLL) {
            return;
        }
        *last = Some(now);
    }
    let Some(game) = Game::get() else {
        return;
    };
    let mut trails = lock(&TRAILS);
    for entity in game.get_entities() {
        let index = entity.index();
        // `1..=32` — игроки: у них та же мировая модель висит в руках.
        if index <= 32 {
            continue;
        }
        let Some(path) = entity.model_path() else {
            continue;
        };
        let Some(kind) = world_kind(&path.to_ascii_lowercase()) else {
            continue;
        };
        let pos = entity.origin();
        if !pos.x.is_finite() || !pos.y.is_finite() || !pos.z.is_finite() {
            continue;
        }
        if pos.x == 0.0 && pos.y == 0.0 && pos.z == 0.0 {
            continue;
        }
        touch_trail(&mut trails, index, kind, pos, now);
    }
}

/// Добавляет точку к следу гранаты или заводит новый след.
fn touch_trail(trails: &mut Vec<Trail>, entity: i32, kind: Kind, pos: Vec3, now: Instant) {
    if let Some(trail) = trails.iter_mut().find(|trail| trail.entity == entity) {
        trail.kind = kind;
        trail.last_seen = now;
        let speed = trail.samples.last().map_or(0.0, |last| {
            let dt = now.duration_since(last.at).as_secs_f32().max(1e-3);
            last.pos.distance_to(pos) / dt
        });
        if speed >= TRAIL_CONFIRM_SPEED {
            trail.flying = true;
        }
        let far_enough = trail
            .samples
            .last()
            .is_none_or(|last| last.pos.distance_to(pos) >= TRAIL_STEP);
        if far_enough {
            trail.samples.push(Sample { pos, at: now });
        }
        trail
            .samples
            .retain(|sample| now.duration_since(sample.at) <= TRAIL_LIFE);
        return;
    }

    if trails.len() >= TRAILS_MAX {
        if let Some(oldest) = trails
            .iter()
            .enumerate()
            .min_by_key(|(_, trail)| trail.last_seen)
            .map(|(index, _)| index)
        {
            trails.swap_remove(oldest);
        }
    }
    trails.push(Trail {
        entity,
        kind,
        flying: false,
        last_seen: now,
        samples: vec![Sample { pos, at: now }],
    });
}

/// Рисует следы летящих гранат: старые точки прозрачнее, хвост гаснет плавно.
fn draw_trails() {
    let now = Instant::now();
    let segments = {
        let mut trails = lock(&TRAILS);
        trails.retain(|trail| now.duration_since(trail.last_seen) < TRAIL_GONE);
        let mut segments: Vec<(Vec3, Vec3, Color)> = Vec::new();
        for trail in trails.iter() {
            if !trail.flying || trail.samples.len() < 2 {
                continue;
            }
            let base = match trail.kind {
                Kind::He => Color::RED,
                Kind::Flash => Color::CYAN,
                Kind::Smoke => Color::WHITE,
            };
            for pair in trail.samples.windows(2) {
                let (from, to) = (pair[0], pair[1]);
                let age = now.duration_since(to.at).as_secs_f32() / TRAIL_LIFE.as_secs_f32();
                let alpha = (1.0 - age).clamp(0.0, 1.0);
                if alpha <= 0.02 {
                    continue;
                }
                segments.push((from.pos, to.pos, Color::rgba(base.r, base.g, base.b, alpha)));
            }
        }
        segments
    };
    if segments.is_empty() {
        return;
    }
    let Some(overlay) = Overlay::new() else {
        return;
    };
    overlay.trail(&segments, 3.0);
}

fn apply_held_name(name: &str) {
    if !is_view_model(name) {
        return;
    }
    let mut last = lock(&LAST_MODEL);
    if last.as_deref() != Some(name) {
        golden_sdk::log!("grenade: модель оружия {name}");
        *last = Some(name.to_string());
    }
    drop(last);
    let kind = if name.contains("hegrenade") {
        Some(Kind::He)
    } else if name.contains("flashbang") {
        Some(Kind::Flash)
    } else if name.contains("smokegrenade") {
        Some(Kind::Smoke)
    } else {
        None
    };

    let Some(kind) = kind else {
        // Достали другое оружие (нож, пистолет): показ гаснет всегда, даже если
        // его включили командой. Иначе предсказание прошлой гранаты остаётся
        // на экране и продолжает считаться.
        FORCED.store(false, Ordering::Release);
        ENABLED.store(false, Ordering::Release);
        return;
    };

    FORCED.store(false, Ordering::Release);
    let byte = kind_code(kind);
    let was_enabled = ENABLED.swap(true, Ordering::AcqRel);
    let old_kind = KIND.swap(byte, Ordering::AcqRel);
    if !was_enabled || old_kind != byte {
        RECALC.store(true, Ordering::Release);
        golden_sdk::log!("grenade: в руках {}", kind.name());
    }
}

golden_std::hook! {
    /// Перехват `StudioDrawModel` рендерера: здесь видно модель в руках.
    unsafe extern "thiscall" fn hook_weapon(this: *mut c_void, flags: i32)
        original: hook_weapon_original
        -> i32
    {
        detect_held_from_renderer();
        capture_flying();
        unsafe { hook_weapon_original()(this, flags) }
    }
}

/// Ставит перехват `StudioDrawModel` из рендер-потока.
///
/// Слот `3` — тот же, что в `auto-bhop`; `hooks::install` запомнит хук и снимет
/// его при выгрузке.
fn install_weapon_hook() {
    if WEAPON_HOOKED.swap(true, Ordering::AcqRel) {
        return;
    }
    let entry = StudioRenderer::global().and_then(|renderer| renderer.method(3));
    let Some(entry) = entry else {
        WEAPON_HOOKED.store(false, Ordering::Release);
        return;
    };
    // SAFETY: `hook_weapon` — функция того же ABI, что и `StudioDrawModel`.
    match unsafe { golden_std::hooks::install(entry, hook_weapon as *const ()) } {
        Ok(_) => golden_sdk::log!("grenade: перехват StudioDrawModel включён"),
        Err(error) => {
            WEAPON_HOOKED.store(false, Ordering::Release);
            golden_sdk::log!("grenade: перехват StudioDrawModel не встал: {error}");
        }
    }
}

// Клиентский `StudioDrawPlayer`: зовём оригинал и с первого вызова ставим
// перехват конца сцены — здесь мы уже в рендер-потоке.
unsafe extern "C" fn hook_draw_player(flags: i32, player_info: *mut c_void) -> i32 {
    let _inside = InFlight::enter();
    // Пока движок рисует игрока, у нас есть его запись: слот, origin, углы.
    // В конце сцены этих данных уже не достать, поэтому запоминаем сейчас.
    // SAFETY: `player_info` — аргумент движка; читаем только при ненулевом.
    let slot = unsafe { remember_player(player_info) };
    // Локальный игрок: оружие в руках надёжно читается из его сущности, а не
    // из view-модели. После смены карты клиент пересоздаёт таблицу рендерера,
    // и перехват `StudioDrawModel` может отвалиться — этот путь не зависит от
    // него.
    if slot == Some(golden_sdk::state::EngineState::view_entity()) {
        detect_held_from_entity();
    }
    // SAFETY: `ORIGINAL_PLAYER` выставлен в `install` указателем клиента.
    let result = unsafe { call_original_draw_player(flags, player_info) };
    if !FIRST_DRAW.swap(true, Ordering::AcqRel) {
        install_weapon_hook();
        install_scene_end();
    }
    result
}

/// Кладёт позу нарисованного игрока в таблицу [`SEEN`].
///
/// # Safety
/// `info` должен быть записью из `draw_player` (или нулём).
unsafe fn remember_player(info: *mut c_void) -> Option<i32> {
    let info = info as usize;
    if info == 0 {
        return None;
    }
    // SAFETY: запись движка; смещение слота сверено в `offsets::player_info`.
    let slot: i32 = unsafe { golden_sdk::process::read(info + player_info::INDEX) };
    if !(0..64).contains(&slot) {
        return None;
    }
    // SAFETY: та же запись; origin/angles проверены дампом живой памяти.
    let origin_raw: [f32; 3] = unsafe { golden_sdk::process::read(info + player_state::ORIGIN) };
    // SAFETY: те же гарантии.
    let angles_raw: [f32; 3] = unsafe { golden_sdk::process::read(info + player_state::ANGLES) };
    let origin = Vec3::new(origin_raw[0], origin_raw[1], origin_raw[2]);
    let angles = Vec3::new(angles_raw[0], angles_raw[1], angles_raw[2]);
    if !finite(origin) || !finite(angles) || origin == Vec3::ZERO {
        return None;
    }

    let mut seen = lock(&SEEN);
    match seen.iter_mut().find(|seen| seen.index == slot) {
        Some(item) => {
            item.origin = origin;
            item.angles = angles;
        }
        None => seen.push(Seen {
            index: slot,
            origin,
            angles,
        }),
    }
    Some(slot)
}

/// Все компоненты конечны.
fn finite(vector: Vec3) -> bool {
    vector.x.is_finite() && vector.y.is_finite() && vector.z.is_finite()
}

/// # Safety
/// [`ORIGINAL_PLAYER`] должен указывать на настоящий `StudioDrawPlayer`.
unsafe fn call_original_draw_player(flags: i32, player_info: *mut c_void) -> i32 {
    let original = ORIGINAL_PLAYER.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    // SAFETY: сигнатура из HLSDK: `int StudioDrawPlayer(int, player_info_s *)`.
    let function: unsafe extern "C" fn(i32, *mut c_void) -> i32 =
        unsafe { core::mem::transmute(original) };
    unsafe { function(flags, player_info) }
}

/// Ставит перехват конца 3D-сцены.
fn install_scene_end() {
    if SCENE_HOOKED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(module) = golden_sdk::process::client_base() else {
        SCENE_HOOKED.store(false, Ordering::Release);
        golden_sdk::log!("grenade: client.dll ещё не загружен, конец сцены не перехвачен");
        return;
    };
    let target = module + DRAW_NORMAL_TRIANGLES_RVA;
    // SAFETY: `hook_scene_end` — функция того же ABI (`void(void)`).
    match unsafe { golden_std::hooks::install(target, hook_scene_end as *const ()) } {
        Ok(_) => golden_sdk::log!("grenade: перехват конца сцены установлен ({target:#x})"),
        Err(error) => {
            SCENE_HOOKED.store(false, Ordering::Release);
            golden_sdk::log!("grenade: перехват конца сцены не встал: {error}");
        }
    }
}

// Конец 3D-сцены: мир, brush-модели и игроки уже нарисованы, GL свободен.
golden_std::hook! {
    unsafe extern "system" fn hook_scene_end()
        original: scene_end_original
    {
        let _inside = InFlight::enter();
        frame();
        unsafe { scene_end_original()() };
    }
}

/// Пересчитывает предсказание (если пора) и рисует его.
fn frame() {
    // Следы летящих гранат рисуем всегда: они не зависят от того, что в руках.
    draw_trails();
    if !ENABLED.load(Ordering::Acquire) {
        return;
    }

    let kind = kind();
    let recalc = RECALC.swap(false, Ordering::AcqRel);
    let now = Instant::now();

    let (stale, targets_due, previous_targets_at) = {
        let cache = lock(&CACHE);
        match cache.as_ref() {
            None => (true, true, None),
            Some(cached) => {
                let kind_changed = cached.prediction.kind != kind;
                (
                    kind_changed || now.duration_since(cached.at) >= REFRESH,
                    kind_changed || now.duration_since(cached.targets_at) >= TARGETS_REFRESH,
                    Some(cached.targets_at),
                )
            }
        }
    };

    if recalc || stale {
        let seen = lock(&SEEN).clone();
        let started = Instant::now();
        let full = recalc || targets_due;
        match if recalc {
            grenade::inspect(kind, &seen)
        } else if full {
            grenade::predict(kind, &seen)
        } else {
            grenade::predict_flight(kind)
        } {
            Ok(mut prediction) => {
                if !full {
                    // Цели не пересчитываем — оставляем прошлые, но только
                    // если тип гранаты не сменился.
                    if let Some(cached) = lock(&CACHE).as_ref() {
                        if cached.prediction.kind == kind {
                            prediction.damage = cached.prediction.damage.clone();
                            prediction.flashes = cached.prediction.flashes.clone();
                            prediction.checks = cached.prediction.checks.clone();
                            prediction.players = cached.prediction.players;
                            prediction.alive = cached.prediction.alive;
                        }
                    }
                }
                if recalc {
                    report::print(&prediction);
                }
                // Подписи пересобираем только вместе с целями: имена и родство
                // между пересчётами не меняются, а чтение игроков в кадре дорого.
                let labels = if full {
                    build_labels(&prediction)
                } else {
                    lock(&CACHE)
                        .as_ref()
                        .map(|cached| cached.labels.clone())
                        .filter(|labels| !labels.is_empty())
                        .unwrap_or_else(|| build_labels(&prediction))
                };
                *lock(&CACHE) = Some(Cached {
                    prediction,
                    at: now,
                    targets_at: if full {
                        now
                    } else {
                        previous_targets_at.unwrap_or(now)
                    },
                    labels,
                });
                *lock(&LAST_ERROR) = None;
            }
            Err(error) => {
                if recalc {
                    report::error(&error);
                }
                log_error_once(&error);
                // Старое предсказание другого типа рисовать нельзя.
                let mut cache = lock(&CACHE);
                if cache
                    .as_ref()
                    .map_or(false, |cached| cached.prediction.kind != kind)
                {
                    *cache = None;
                }
            }
        }
        if !recalc {
            let elapsed = started.elapsed();
            if elapsed > Duration::from_millis(8) {
                let mut last = lock(&SLOW_LOG);
                if last.map_or(true, |at| now.duration_since(at) >= Duration::from_secs(2)) {
                    golden_sdk::log!(
                        "grenade: кадр с целями занял {:.1} мс",
                        elapsed.as_secs_f32() * 1000.0
                    );
                    *last = Some(now);
                }
            }
        }
    }

    draw();
}

/// Подписи целей: текст считается один раз на пересчёт целей.
///
/// В подписи только суть: сколько урона прилетит или на сколько секунд
/// ослепит. Ник и пометку команды не пишем — они мешают читать цифру, цвет
/// подписи уже говорит, свой это или враг.
///
/// Себя не подписываем: когда идёшь спиной к своей же гранате, расчёт честно
/// считает ослепление, но подпись над собственной головой только путает.
fn build_labels(prediction: &Prediction) -> Vec<(Vec3, String, Color)> {
    let mut labels = Vec::new();
    match prediction.kind {
        Kind::He => {
            for damage in &prediction.damage {
                if damage.index == prediction.local {
                    continue;
                }
                let relation = report::relation(prediction, damage.index);
                labels.push((
                    damage.origin + Vec3::new(0.0, 0.0, 36.0),
                    format!("-{:.0}", damage.damage),
                    relation_color(relation, Color::YELLOW, Color::RED),
                ));
            }
        }
        Kind::Flash => {
            for flash in &prediction.flashes {
                if flash.index == prediction.local {
                    continue;
                }
                let relation = report::relation(prediction, flash.index);
                labels.push((
                    flash.eye + Vec3::new(0.0, 0.0, 10.0),
                    format!("{:.1}с", flash.duration),
                    relation_color(relation, Color::CYAN, Color::CYAN),
                ));
            }
        }
        Kind::Smoke => {}
    }
    labels
}

/// Рисует траекторию и подписи целей по последнему расчёту.
fn draw() {
    let guard = lock(&CACHE);
    let Some(cached) = guard.as_ref() else {
        return;
    };
    let Some(overlay) = Overlay::new() else {
        return;
    };

    let prediction = &cached.prediction;
    let color = match prediction.kind {
        Kind::He => Color::RED,
        Kind::Flash => Color::CYAN,
        Kind::Smoke => Color::WHITE,
    };

    let segments = prediction.flight.segments();
    overlay.xray(&segments, color, 2.0);

    let explosion = prediction.flight.explosion;

    // Кого заденет: подписи прямо над игроками — одним проходом отрисовки.
    let items: Vec<(Vec3, &str, Color)> = cached
        .labels
        .iter()
        .map(|(at, text, color)| (*at, text.as_str(), *color))
        .collect();
    overlay.labels_at(&items, true);

    let summary = match prediction.kind {
        Kind::He => format!("HE: взрыв, заденет {}", prediction.damage.len()),
        Kind::Flash => format!("флешка: взрыв, ослепит {}", prediction.flashes.len()),
        Kind::Smoke => format!("смок: дым через {:.1} с", prediction.flight.time),
    };
    overlay.label(explosion + Vec3::new(0.0, 0.0, 12.0), &summary, color, true);
}

/// Цвет метки: враг — `hostile`, свой — зелёный, иначе исходный.
fn relation_color(relation: &str, base: Color, hostile: Color) -> Color {
    if relation == ", враг" {
        hostile
    } else if relation == ", свой" {
        Color::GREEN
    } else {
        base
    }
}

/// Ошибку расчёта пишем в лог один раз, а не каждый кадр.
fn log_error_once(error: &str) {
    let mut last = lock(&LAST_ERROR);
    if last.as_deref() != Some(error) {
        golden_sdk::log!("grenade: расчёт не удался: {error}");
        *last = Some(error.to_string());
    }
}

/// Блокировка без паники на испорченном мьютексе.
fn lock<T>(mutex: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
