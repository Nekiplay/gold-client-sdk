//! ESP: обводка игроков и подписи.
//!
//! Механика целиком живёт здесь, моду предлагается только выбрать цвет, ширину
//! и тексты:
//!
//! ```no_run
//! # fn main() -> Result<(), String> {
//! golden_std::esp::Esp::new()
//!     .width(2.0)
//!     .label(|player: &golden_std::Player| (Some(player.name()), player.weapon()))
//!     .install()
//! # }
//! ```
//!
//! Почему так сложно внутри: движок рисует brush-модели (ящики, контейнеры) в
//! том же проходе сущностей, что и игроков, и часто **после** них, поэтому
//! контур, нарисованный рядом с моделью, ими закрашивается. Моду приходится
//! откладывать отрисовку до конца 3D-сцены (`HUD_DrawNormalTriangles`), а там
//! заново звать движковую отрисовку модели, подставив «текущую сущность» —
//! без неё `StudioDrawPlayer` возвращает ноль полигонов.
//!
//! ## Видимая часть контура
//!
//! [`Esp::visible_color`] включает второй проход: контур рисуется дважды —
//! сначала сквозь стены базовым цветом, потом с тестом глубины цветом видимой
//! части. Тест глубины здесь — тот же GL-буфер, который заполнили мир и тела:
//! пиксели, закрытые стеной или чужим телом, во второй проход не проходят и
//! остаются в базовом цвете, а видимые становятся зелёными (или какими задали).
//! Никаких лучей и трассировок для этого не нужно.
//!
//! ## Только враги
//!
//! По умолчанию рисуются только враги: команда игрока сравнивается с командой
//! того, из чьих глаз смотрит камера. Своих не обводим ни контуром, ни
//! подписями. Игрок, чью команду определить не удалось (наблюдатель, первые
//! кадры после подключения), врагом не считается — лучше пропустить, чем
//! обвести своего. Всё это выключается [`Esp::enemies_only`].

use core::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use golden_sdk::draw::Color;
use golden_sdk::log;
use golden_sdk::math::Vec3;
use golden_sdk::module::InFlight;
use golden_sdk::renderer::{interface_slot, StudioInterface, StudioRenderer};
use golden_sdk::player::Team;

use crate::game::{Game, Player};

use crate::hooks;
use crate::render::{self, OutlineStyle};

/// Смещение `HUD_DrawNormalTriangles` — общее с [`crate::render`].
use crate::render::DRAW_NORMAL_TRIANGLES_RVA;

/// Флаги `StudioDrawPlayer`: рисовать тело и оружие в руках.
const STUDIO_RENDER: i32 = 1;
const STUDIO_WEAPON: i32 = 2;

/// Какой рост тела считаем настоящим: по нему отсекаются чужие кости.
const REAL_HEIGHT: core::ops::RangeInclusive<f32> = 40.0..=120.0;

/// Что рисовать и каким цветом.
struct Settings {
    color: Option<Arc<dyn Fn(&Player) -> Color + Send + Sync>>,
    /// Цвет видимой части: `None` — весь контур одним цветом.
    visible: Option<Arc<dyn Fn(&Player) -> Color + Send + Sync>>,
    label: Option<Arc<dyn Fn(&Player) -> (Option<String>, Option<String>) + Send + Sync>>,
    width: f32,
    /// Рисовать только врагов локального игрока.
    enemies_only: bool,
}

/// Кого перерисовать в конце кадра.
struct Pending {
    /// Указатель, который движок передал в `StudioDrawPlayer`.
    info: usize,
    /// Сущность игрока: её движок читает как «текущую».
    entity: usize,
    /// Цвет контура сквозь стены (и всей обводки, если видимая не задана).
    color: Color,
    /// Цвет видимой части: рисуется вторым проходом с тестом глубины.
    visible: Option<Color>,
}

static SETTINGS: Mutex<Option<Settings>> = Mutex::new(None);
static REPLAY: Mutex<Vec<Pending>> = Mutex::new(Vec::new());

/// Мы внутри перерисовки: свои же перехваты не должны ни обводить, ни копить.
static IN_REPLAY: AtomicBool = AtomicBool::new(false);
static SCENE_HOOKED: AtomicBool = AtomicBool::new(false);
static RENDER_MODEL_HOOKED: AtomicBool = AtomicBool::new(false);
static ORIGINAL_PLAYER: AtomicUsize = AtomicUsize::new(0);
static DRAWN: AtomicU64 = AtomicU64::new(0);
static LABELS: AtomicU32 = AtomicU32::new(0);

/// Команда локального игрока и когда её выясняли.
///
/// Её спрашивают на каждого рисуемого игрока и на каждую подпись, а поиск идёт
/// по всем сущностям — поэтому ответ живёт полсекунды.
static LOCAL: Mutex<(Option<Instant>, Option<Team>)> = Mutex::new((None, None));

/// Готовый ESP.
pub struct Esp {
    color: Option<Arc<dyn Fn(&Player) -> Color + Send + Sync>>,
    visible: Option<Arc<dyn Fn(&Player) -> Color + Send + Sync>>,
    label: Option<Arc<dyn Fn(&Player) -> (Option<String>, Option<String>) + Send + Sync>>,
    width: f32,
    enemies_only: bool,
}

impl Default for Esp {
    fn default() -> Self {
        Self::new()
    }
}

impl Esp {
    /// ESP с цветом команды и шириной линии 2.
    pub fn new() -> Self {
        Self {
            color: None,
            visible: None,
            label: None,
            width: 2.0,
            enemies_only: true,
        }
    }

    /// Цвет контура и подписей. По умолчанию — цвет команды игрока.
    ///
    /// Это же цвет закрытой части контура, если задан [`Esp::visible_color`].
    pub fn color<F>(mut self, color: F) -> Self
    where
        F: Fn(&Player) -> Color + Send + Sync + 'static,
    {
        self.color = Some(Arc::new(color));
        self
    }

    /// Цвет видимой части контура: остальное остаётся в [`Esp::color`].
    ///
    /// Включает второй проход отрисовки — модель рисуется ещё раз с тестом
    /// глубины. Закрытые стеной, ящиком или чужим телом пиксели до него не
    /// доходят, поэтому зелёными становятся ровно те части, которые видно:
    ///
    /// ```
    /// # use golden_std::{Color, esp::Esp};
    /// Esp::new().color(|_| Color::RED).visible_color(|_| Color::GREEN);
    /// ```
    pub fn visible_color<F>(mut self, color: F) -> Self
    where
        F: Fn(&Player) -> Color + Send + Sync + 'static,
    {
        self.visible = Some(Arc::new(color));
        self
    }

    /// Тексты: первая строка над головой, вторая — под ногами.
    pub fn label<F>(mut self, label: F) -> Self
    where
        F: Fn(&Player) -> (Option<String>, Option<String>) + Send + Sync + 'static,
    {
        self.label = Some(Arc::new(label));
        self
    }

    /// Ширина линий контура.
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Рисовать только врагов локального игрока. По умолчанию включено.
    ///
    /// `false` рисует всех: и своих, и чужих.
    pub fn enemies_only(mut self, enemies_only: bool) -> Self {
        self.enemies_only = enemies_only;
        self
    }

    /// Ставит перехваты и подмену интерфейса. Ждёт, пока движок отдаст рендерер.
    ///
    /// # Errors
    /// Строка с причиной, если интерфейс так и не появился или перехват не встал.
    pub fn install(self) -> Result<(), String> {
        {
            let mut settings = SETTINGS
                .lock()
                .map_err(|_| "esp: настройки заняты".to_string())?;
            *settings = Some(Settings {
                color: self.color,
                visible: self.visible,
                label: self.label,
                width: self.width,
                enemies_only: self.enemies_only,
            });
        }
        let previous = wait_for_interface(Duration::from_secs(120))
            .ok_or_else(|| "esp: движок так и не отдал интерфейс рендерера".to_string())?;
        let ours = our_interface(previous);
        // SAFETY: `ours` живёт в куче до конца процесса, поля совпадают по ABI.
        unsafe { hooks::interface(ours) }.map_err(|error| error.to_string())?;
        // Сами перехваты ставим из рендер-потока, в первом же вызове
        // `StudioDrawPlayer`: править байты функции, которую движок сейчас
        // исполняет из другого потока, нельзя — это роняет игру.
        Ok(())
    }

    /// Сколько моделей обведено всего.
    pub fn drawn() -> u64 {
        drawn()
    }
}

/// Сколько моделей обведено всего.
pub fn drawn() -> u64 {
    DRAWN.load(Ordering::Relaxed)
}

/// Сколько подписей поставлено всего.
pub fn labels() -> u32 {
    LABELS.load(Ordering::Relaxed)
}

/// Ждёт, пока в `hw.dll` появится указатель на интерфейс клиента.
fn wait_for_interface(timeout: Duration) -> Option<*mut StudioInterface> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(slot) = interface_slot() {
            // SAFETY: слот — статическое поле движка.
            let current = unsafe { *slot };
            if !current.is_null()
                && StudioRenderer::global().is_some_and(|renderer| renderer.is_initialized())
            {
                return Some(current);
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Наш интерфейс: тело рисует клиент, игрока перехватываем.
fn our_interface(previous: *const StudioInterface) -> *mut StudioInterface {
    // SAFETY: `previous` — живой интерфейс движка/клиента.
    let draw_player = unsafe { (*previous).draw_player };
    match draw_player {
        Some(function) => ORIGINAL_PLAYER.store(function as usize, Ordering::Release),
        None => ORIGINAL_PLAYER.store(0, Ordering::Release),
    }
    let ours = StudioInterface {
        version: StudioInterface::VERSION,
        // `draw_model` остаётся клиентским: ESP нужны только игроки.
        draw_model: unsafe { (*previous).draw_model },
        draw_player: Some(hook_draw_player),
    };
    let leaked = Box::leak(Box::new(ours));
    leaked as *mut StudioInterface
}

// Клиентский `StudioDrawPlayer`: сначала обычная отрисовка, потом запоминаем,
// кого перерисовать в конце кадра.
crate::hook! {
    unsafe extern "C" fn hook_draw_player(flags: i32, player_info: *mut c_void)
        original: draw_player_original
        -> i32
    {
        let _inside = InFlight::enter();
        // Оригинал берём из своего статика, а не через аксессор макроса:
        // подмена интерфейса регистрирует функцию в таблице клиента, а не в
        // реестре детуров, поэтому аксессор там паникует и роняет процесс.
        let result = unsafe { call_original_draw_player(flags, player_info) };
        if IN_REPLAY.load(Ordering::Acquire) {
            return result;
        }
        if !RENDER_MODEL_HOOKED.load(Ordering::Acquire) {
            install_render_model();
        }
        if !SCENE_HOOKED.load(Ordering::Acquire) {
            install_scene_hook();
        }
        if SCENE_HOOKED.load(Ordering::Acquire) {
            if let Some(renderer) = StudioRenderer::global() {
                if let Some(entity) = renderer.current_entity() {
                    // Своих не обводим: ESP существует, чтобы видеть врагов.
                    if player_of(&renderer).is_some_and(|player| wanted(&player)) {
                        let color = player_color(&renderer).unwrap_or(Color::WHITE);
                        let visible = visible_color(&renderer);
                        // Именно этот указатель движок просит у клиента:
                        // подставить вместо него адрес поля из сущности
                        // нельзя — отрисовка тогда возвращает ноль полигонов.
                        stash(entity.addr(), player_info as usize, color, visible);
                    }
                }
            }
        }
        result
    }
}

/// Вызов движкового `StudioDrawPlayer` через указатель из интерфейса клиента.
///
/// # Safety
/// `ORIGINAL_PLAYER` должен указывать на настоящий `StudioDrawPlayer`.
unsafe fn call_original_draw_player(flags: i32, player_info: *mut c_void) -> i32 {
    let original = ORIGINAL_PLAYER.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    // SAFETY: сигнатура из HLSDK: `int StudioDrawPlayer(int flags, player_info_s*)`.
    let function: unsafe extern "C" fn(i32, *mut c_void) -> i32 =
        unsafe { core::mem::transmute(original) };
    unsafe { function(flags, player_info) }
}

// Клиентский `StudioRenderModel`: после обычной отрисовки ставим подписи.
//
// Только здесь кости движка принадлежат именно этой модели: после возврата из
// `StudioDrawPlayer` они уже чужие, и подписи уезжают к другому объекту.
crate::hook! {
    unsafe extern "thiscall" fn hook_render_model(this: *mut c_void)
        original: render_model_original
    {
        let _inside = InFlight::enter();
        unsafe { render_model_original()(this) };
        if IN_REPLAY.load(Ordering::Acquire) {
            return;
        }
        labels_for_current_model();
    }
}

/// Перехват `StudioRenderModel` через слот vtable объекта рендерера.
fn install_render_model() {
    if RENDER_MODEL_HOOKED.load(Ordering::Acquire) {
        return;
    }
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let Some(entry) = renderer.method(golden_sdk::renderer::SLOT_RENDER_MODEL) else {
        log!("esp: в vtable рендерера нет слота отрисовки меша");
        return;
    };
    if RENDER_MODEL_HOOKED.swap(true, Ordering::AcqRel) {
        return;
    }
    // SAFETY: `hook_render_model` — функция того же ABI, что `StudioRenderModel`.
    match unsafe { hooks::install(entry, hook_render_model as *const ()) } {
        Ok(_) => log!("esp: перехват отрисовки модели включён"),
        Err(error) => {
            RENDER_MODEL_HOOKED.store(false, Ordering::Release);
            log!("esp: не удалось перехватить отрисовку модели: {error}");
        }
    }
}

// Перехват конца 3D-сцены: движок зовёт его после мира и всех сущностей.
crate::hook! {
    unsafe extern "system" fn hook_scene_end()
        original: scene_end_original
    {
        let _inside = InFlight::enter();
        unsafe { draw_replay() };
        render::flush_labels();
        unsafe { scene_end_original()() };
    }
}

/// Вешает перехват конца сцены.
fn install_scene_hook() {
    if SCENE_HOOKED.load(Ordering::Acquire) {
        return;
    }
    let Some(module) = golden_sdk::process::client_base() else {
        log!("esp: client.dll ещё не загружен");
        return;
    };
    let target = module + DRAW_NORMAL_TRIANGLES_RVA;
    match unsafe { hooks::install(target, hook_scene_end as *const ()) } {
        Ok(_) => {
            SCENE_HOOKED.store(true, Ordering::Release);
            log!("esp: перехват конца сцены установлен ({target:#x})");
        }
        Err(error) => {
            SCENE_HOOKED.store(false, Ordering::Release);
            log!("esp: не удалось перехватить конец сцены: {error}");
        }
    }
}

/// Запоминает игрока для отрисовки в конце кадра (без повторов).
fn stash(entity: usize, info: usize, color: Color, visible: Option<Color>) {
    let Ok(mut replay) = REPLAY.lock() else {
        return;
    };
    if replay.iter().any(|entry| entry.info == info) {
        return;
    }
    replay.push(Pending {
        info,
        entity,
        color,
        visible,
    });
}

/// Рисует контуры в конце кадра, когда все укрытия уже выведены.
///
/// # Safety
/// Вызывается из перехвата `HUD_DrawNormalTriangles` с активным GL-контекстом.
unsafe fn draw_replay() {
    let entries = match REPLAY.lock() {
        Ok(mut replay) => core::mem::take(&mut *replay),
        Err(_) => return,
    };
    if entries.is_empty() {
        return;
    }
    let original = ORIGINAL_PLAYER.load(Ordering::Acquire);
    if original == 0 {
        return;
    }
    // SAFETY: оригинал `StudioDrawPlayer(int, player_info_s*)` из HLSDK.
    let draw_player: unsafe extern "C" fn(i32, *mut c_void) -> i32 =
        unsafe { core::mem::transmute(original) };
    let width = SETTINGS
        .lock()
        .ok()
        .and_then(|settings| settings.as_ref().map(|settings| settings.width))
        .unwrap_or(2.0);

    // Без «текущей сущности» клиент отказывается рисовать игрока. Движок держит
    // её в `hw+0x78E470`; на время отрисовки подставляем свою и возвращаем
    // прежнюю, чтобы не сбить движок.
    let current = golden_sdk::process::hw_base()
        .map(|base| (base + golden_sdk::offsets::hw::P_CURRENT_ENTITY) as *mut usize);

    // Один проход: подставляет «текущую сущность», обводит нарисованную ею
    // модель и возвращает прежнюю, чтобы не сбить движок.
    let outline_entry = |entry: &Pending, style: OutlineStyle| {
        let previous = current.map(|slot| unsafe {
            let previous = *slot;
            *slot = entry.entity;
            previous
        });
        let drawn = render::outline(style, || unsafe {
            draw_player(
                STUDIO_RENDER | STUDIO_WEAPON,
                entry.info as *mut c_void,
            );
        });
        if let (Some(slot), Some(previous)) = (current, previous) {
            unsafe { *slot = previous };
        }
        drawn
    };

    IN_REPLAY.store(true, Ordering::Release);
    if entries.iter().any(|entry| entry.visible.is_some()) {
        // Сначала все контуры сквозь стены, потом видимые части у всех. Иначе
        // закрытая стеной обводка одного игрока перекрыла бы зелёную обводку
        // того, кто стоит перед ним: первый проход рисуется без глубины.
        for entry in &entries {
            outline_entry(entry, OutlineStyle::new(entry.color).width(width));
        }
        for entry in &entries {
            if let Some(visible) = entry.visible {
                outline_entry(
                    entry,
                    OutlineStyle::new(visible)
                        .width(width)
                        .through_walls(false),
                );
            }
        }
    } else {
        for entry in &entries {
            outline_entry(entry, OutlineStyle::new(entry.color).width(width));
        }
    }
    DRAWN.fetch_add(entries.len() as u64, Ordering::Relaxed);
    IN_REPLAY.store(false, Ordering::Release);
}

/// Ставит подписи для модели, которую движок рисует прямо сейчас.
fn labels_for_current_model() {
    let Some(settings) = SETTINGS.lock().ok().and_then(|s| s.as_ref().map(|s| {
        (s.label.clone(), s.color.clone())
    })) else {
        return;
    };
    let (label, _) = settings;
    let Some(label) = label else {
        return;
    };
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let Some(model) = renderer.studio_model() else {
        return;
    };
    if !model.is_player_model() {
        return;
    }
    let Some(entity) = renderer.current_entity() else {
        return;
    };
    let index = entity.player_index();
    if !(0..64).contains(&index) {
        return;
    }
    let Some((lowest, highest)) = renderer.body_bounds(&model) else {
        return;
    };
    // Кости могли не дойти до этой модели: тогда объём выходит в разы больше
    // игрока (в логах был рост 329 при настоящем 72). Лучше пусто, чем текст в
    // случайном месте.
    if !REAL_HEIGHT.contains(&(highest.z - lowest.z)) {
        return;
    }
    let Some(player) = Player::from_raw(index, entity) else {
        return;
    };
    if !wanted(&player) {
        return;
    }
    let color = player_color(&renderer).unwrap_or(Color::WHITE);
    let (above, below) = label(&player);

    // Углы AABB — края тела, а не ось: подпись у правого верхнего угла уезжает
    // вправо от головы. Берём середину по горизонтали.
    let axis = Vec3::new(
        (lowest.x + highest.x) * 0.5,
        (lowest.y + highest.y) * 0.5,
        0.0,
    );
    if let Some(text) = above.filter(|text| !text.is_empty()) {
        render::defer_label(
            Vec3::new(axis.x, axis.y, highest.z),
            &text,
            color,
            true,
        );
    }
    if let Some(text) = below.filter(|text| !text.is_empty()) {
        render::defer_label(
            Vec3::new(axis.x, axis.y, lowest.z),
            &text,
            color,
            false,
        );
    }
    LABELS.fetch_add(1, Ordering::Relaxed);
}

/// Рисовать ли этого игрока (учитывает настройку «только враги»).
fn wanted(player: &Player) -> bool {
    if !enemies_only() {
        return true;
    }
    is_enemy(player)
}

/// Включён ли режим «только враги». По умолчанию — да.
fn enemies_only() -> bool {
    SETTINGS
        .lock()
        .ok()
        .and_then(|settings| settings.as_ref().map(|settings| settings.enemies_only))
        .unwrap_or(true)
}

/// Номер сущности, из которой смотрит камера, — для лога и разбора.
pub fn local_index() -> Option<i32> {
    Game::get()?.view_entity()
}

/// Команда того, из чьих глаз смотрит камера.
///
/// `None`, если обе стороны неизвестны — тогда [`is_enemy`] никого не рисует.
pub fn local_team() -> Option<Team> {
    const TTL: Duration = Duration::from_millis(500);
    if let Ok(local) = LOCAL.lock() {
        if let (Some(at), team) = &*local {
            if at.elapsed() < TTL {
                return *team;
            }
        }
    }
    let team = find_local_team();
    if let Ok(mut local) = LOCAL.lock() {
        *local = (Some(Instant::now()), team);
    }
    team
}

/// Ищет команду локального игрока.
///
/// Сначала по `view_entity` из движка. Если движок индекс не отдал (или слот
/// пуст) — по камере: она стоит в голове локального игрока, поэтому он и есть
/// ближайший игрок к её точке.
fn find_local_team() -> Option<Team> {
    let game = Game::get()?;
    if let Some(index) = game.view_entity() {
        if let Some(team) = Player::by_index(index).and_then(|player| player.team()) {
            return Some(team);
        }
    }
    let origin = game.get_camera()?.origin;
    let mut best: Option<(f32, Team)> = None;
    for player in game.get_players() {
        let (Some(team), Some(entity)) = (player.team(), player.entity()) else {
            continue;
        };
        let distance = entity.origin().distance_to(origin);
        // Дальше пары шагов камера от своего тела не отходит; наблюдателю этот
        // путь не поможет, и это осознанно: в чужую голову он и не попадёт.
        if distance > 128.0 {
            continue;
        }
        if best.map_or(true, |(closest, _)| distance < closest) {
            best = Some((distance, team));
        }
    }
    best.map(|(_, team)| team)
}

/// Враг ли игрок: обе команды известны и различаются.
///
/// Если команду не определить (наблюдатель, первые кадры после подключения),
/// игрок врагом не считается — лучше пропустить, чем обвести своего.
fn is_enemy(player: &Player) -> bool {
    match (player.team(), local_team()) {
        (Some(theirs), Some(mine)) => theirs != mine,
        _ => false,
    }
}

/// Игрок, которого движок рисует прямо сейчас.
fn player_of(renderer: &StudioRenderer) -> Option<Player> {
    let entity = renderer.current_entity()?;
    let index = entity.player_index();
    if !(0..=63).contains(&index) {
        return None;
    }
    Player::from_raw(index, entity)
}

/// Цвет для игрока, которого рисует движок: своя функция, иначе цвет команды.
fn player_color(renderer: &StudioRenderer) -> Option<Color> {
    let model = renderer.studio_model()?;
    let player = player_of(renderer);
    let custom = SETTINGS
        .lock()
        .ok()
        .and_then(|settings| settings.as_ref().and_then(|s| s.color.clone()));
    if let Some(color) = custom {
        if let Some(player) = player {
            return Some(color(&player));
        }
    }
    // Резерв: команда из `g_PlayerExtraInfo`, иначе по имени модели.
    if let Some(team) = player.and_then(|player| player.team()) {
        return Some(team.color());
    }
    Some(
        Team::from_model_name(model.name())
            .map_or(Color::WHITE, Team::color),
    )
}

/// Цвет видимой части контура, если он задан.
fn visible_color(renderer: &StudioRenderer) -> Option<Color> {
    let custom = SETTINGS
        .lock()
        .ok()
        .and_then(|settings| settings.as_ref().and_then(|s| s.visible.clone()))?;
    let player = player_of(renderer)?;
    Some(custom(&player))
}