//! Подмена интерфейса студийного рендера и рисование скелета.

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use golden_sdk::draw::{Color, Gl};
use golden_sdk::engine::EngineStudioApi;
use golden_sdk::hook::SwappedInterface;
use golden_sdk::player::{PlayerExtra, Team};
use golden_sdk::renderer::{self, StudioInterface, StudioRenderer, interface_slot};

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
/// Сколько моделей пропущено (не игрок / нет костей).
static SKIPPED: AtomicU64 = AtomicU64::new(0);
/// Сколько вызовов без разбираемой модели.
static NO_MODEL: AtomicU64 = AtomicU64::new(0);
/// Сколько моделей-игроков отброшено проверкой геометрии.
static REJECTED: AtomicU64 = AtomicU64::new(0);
/// Сколько скелетов потеряли команду (нет элемента `g_PlayerExtraInfo`).
static NO_TEAM: AtomicU64 = AtomicU64::new(0);
/// Сущность → индекс игрока. Индекс ищется один раз на сущность.
static ENTITY_INDEX: Mutex<Option<std::collections::BTreeMap<usize, i32>>> = Mutex::new(None);
/// Команда, которую удалось определить в последний раз (для отчёта).
static LAST_TEAM: Mutex<Option<&'static str>> = Mutex::new(None);
/// Писали ли уже снимок состояния.
static DUMPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Костей в последней модели.
static BONES: AtomicI32 = AtomicI32::new(0);
/// Сколько вызовов пришло через `DrawModel`.
static CALLS_MODEL: AtomicU32 = AtomicU32::new(0);
/// Сколько вызовов пришло через `DrawPlayer`.
static CALLS_PLAYER: AtomicU32 = AtomicU32::new(0);
/// Имя последней рассмотренной модели.
static LAST_MODEL: Mutex<Option<String>> = Mutex::new(None);
/// Модели, про которые уже писали строку в лог.
static SEEN: Mutex<std::collections::BTreeSet<String>> = Mutex::new(std::collections::BTreeSet::new());
/// Когда последний раз отчитывались.
static LAST_REPORT: Mutex<Option<Instant>> = Mutex::new(None);

/// Цвет, если команду определить не удалось (кастомный сервер без `TeamInfo`).
const UNKNOWN_COLOR: Color = Color::WHITE;
/// Толщина линий в пикселях.
const LINE_WIDTH: f32 = 1.6;
/// Показывать скелет сквозь модель (и стены).
///
/// `false` — линии честно прячутся за телом и стенами, видно только торчащие
/// снаружи части. `true` — скелет виден всегда (по умолчанию: иначе на боте
/// видно одни кончики).
const XRAY: bool = true;

/// Ставит подмену интерфейса (ждёт, пока клиент отдаст свой рендерер).
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
    let drawn = DRAWN.load(Ordering::Relaxed);
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
    let team = LAST_TEAM.lock().ok()?.unwrap_or("?");
    Some(format!(
        "вызовов: {} (DrawModel {} / DrawPlayer {}), скелетов нарисовано: {drawn} [команда: {team}], пропущено: {}, без модели: {}, без костей/отброшено: {}, без команды: {}, костей: {}, модель: {model}",
        calls,
        CALLS_MODEL.load(Ordering::Relaxed),
        CALLS_PLAYER.load(Ordering::Relaxed),
        SKIPPED.load(Ordering::Relaxed),
        NO_MODEL.load(Ordering::Relaxed),
        REJECTED.load(Ordering::Relaxed),
        NO_TEAM.load(Ordering::Relaxed),
        BONES.load(Ordering::Relaxed)
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

/// Наш `StudioDrawModel`: оригинал, затем скелет.
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
    draw_skeleton(false);
    result
}

/// Наш `StudioDrawPlayer`: если движок зовёт игроков этим путём — рисуем так же.
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
    draw_skeleton(true);
    result
}

/// Рисует скелет модели, которую клиент только что нарисовал.
///
/// `via_player` — пришли ли мы сюда через `StudioDrawPlayer` (тогда это точно
/// сущность игрока, так решил сам движок).
fn draw_skeleton(via_player: bool) {
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    if !renderer.is_initialized() {
        return;
    }

    // Один раз за процесс пишем полный снимок: по нему видно, какое поле не то.
    if !DUMPED.swap(true, Ordering::Relaxed) {
        let snap = renderer.snapshot();
        golden_sdk::log!("снимок рендерера: {snap:#x?}");
    }

    // Заголовок берём из `model_t` сущности: поле рендерера `+0x4C` клиент может
    // чистить сразу после отрисовки, а `model_t` живёт, пока живёт модель.
    let model = match renderer.entity_model().or_else(|| renderer.studio_model()) {
        Some(model) => model,
        None => {
            NO_MODEL.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    // Про каждую новую модель — одна строка со всеми кандидатами на «игрок».
    // Нужно, чтобы выбрать признак по факту, а не по догадке.
    if !SEEN.lock().map(|s| s.contains(model.name())).unwrap_or(false) {
        if let Ok(mut seen) = SEEN.lock() {
            if seen.insert(model.name().to_string()) {
                golden_sdk::log!(
                    "модель {}: костей {}, кость1..3={:?}, путь={}",
                    model.name(),
                    model.bones().len(),
                    model.bones().iter().take(3).map(|b| b.name()).collect::<Vec<_>>(),
                    if via_player { "draw_player" } else { "draw_model" }
                );
            }
        }
    }

    if !via_player && !model.is_player_model() {
        SKIPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // Проверка геометрии: отбрасывает матрицы, посчитанные для другой модели, и
    // скелеты в системе координат вьюмодели (из-за них были линии к своему оружию).
    let segments = renderer.skeleton_lines_checked(&model);
    if segments.is_empty() {
        REJECTED.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // Команда — из `g_PlayerExtraInfo` (работает и на кастомных моделях).
    let team = team_of(&renderer, &model);
    let color = match team {
        Some(team) => team.color(),
        None => {
            NO_TEAM.fetch_add(1, Ordering::Relaxed);
            UNKNOWN_COLOR
        }
    };

    BONES.store(model.bones().len() as i32, Ordering::Relaxed);
    if let Ok(mut last) = LAST_MODEL.lock() {
        *last = Some(model.name().to_string());
    }
    if let Ok(mut last) = LAST_TEAM.lock() {
        *last = Some(team.map(Team::short_name).unwrap_or("?"));
    }

    // SAFETY: хук отрисовки модели — рендер-поток игры, GL-контекст активен,
    // мировая система координат текущая.
    unsafe {
        if let Some(gl) = Gl::get() {
            if XRAY {
                gl.lines_xray(color, LINE_WIDTH, &segments);
            } else {
                gl.lines(color, LINE_WIDTH, &segments);
            }
            DRAWN.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Команда сущности, которую сейчас рисуем.
///
/// Основной путь — `g_PlayerExtraInfo[индекс].teamnumber`: массив заполняет
/// обработчик `TeamInfo`, поэтому он верен и для ботов, и для кастомных моделей.
/// Если индекс ещё не найден (первый кадр) — по имени модели как запасной вариант.
fn team_of(renderer: &StudioRenderer, model: &golden_sdk::StudioModel) -> Option<Team> {
    if let Some(extra) = player_extra(renderer) {
        if extra.team().is_some() {
            return extra.team();
        }
    }
    Team::from_model_name(model.name())
}

/// `g_PlayerExtraInfo` для текущей сущности (индекс ищется один раз на сущность).
fn player_extra(renderer: &StudioRenderer) -> Option<PlayerExtra> {
    let entity = renderer.current_entity()?;
    let addr = entity.addr();
    let index = {
        let mut map = ENTITY_INDEX.lock().ok()?;
        let map = map.get_or_insert_with(std::collections::BTreeMap::new);
        match map.get(&addr) {
            Some(&index) => index,
            None => {
                // `GetEntityByIndex` — официальный путь движка; сравнение адресов
                // не зависит от того, где в структуре лежит поле `index`.
                let index = EngineStudioApi::for_engine()?.entity_index(entity)?;
                golden_sdk::log!("сущность {addr:#x} — индекс {index}");
                map.insert(addr, index);
                index
            }
        }
    };
    PlayerExtra::for_index(index)
}