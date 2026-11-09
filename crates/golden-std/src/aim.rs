//! Аим: плавная доводка прицела до ближайшей к нему части тела врага.
//!
//! Источник целей — тот же перехват отрисовки игрока, что и у
//! [`crate::trigger`]: движок рисует игрока, [`crate::esp::Esp`] снимает с него
//! хитбоксы из той же позы, в которой нарисовано тело. Цели — живые враги
//! (фильтр ESP), сбор работает и при выключенной обводке ([`crate::esp::Esp::outline`]`(false)`).
//!
//! «Ближайшая часть тела» — хитбокс с наименьшей **угловой** ошибкой от
//! прицела: сколько градусов не хватает до его центра. Такому хитбокку надо
//! меньше всего двигать камеру — доводка не мечется между дальними частями и
//! выглядит естественно. При одинаковой ошибке берётся ближайший по миру.
//!
//! Аим ведёт угол — и только. «Стрелять, лишь когда прицел на враге» — это
//! логика [`crate::trigger`]: у них с аимом общий источник целей, поэтому в
//! одном моде они складываются в одно поведение — аим ведёт прицел, триггербот
//! молчит, пока прицел едет, и нажимает кнопку, когда тот встал на тело и ему
//! ничего не загораживает.
//!
//! Куда пишется угол: [`EngineState::set_view_angles`] — поле `cl.viewangles`.
//! Это то самое поле, из которого движок собирает и камеру, и отправляемую на
//! сервер команду: правка `usercmd_t::viewangles` в перехвате бесполезна,
//! движок переписывает её из `cl.viewangles` уже после `CL_CreateMove`
//! (см. [`golden_sdk::offsets::cl::VIEWANGLES`]). Пишем после вызова оригинала
//! в перехвате `CL_CreateMove` (подмена слота `pfnCreateMove`, тот же приём,
//! что у [`crate::trigger`]). Мышь при этом не глушится:
//! её движение следующего кадра добавится к исправленному углу сверху, а
//! доводка просто снова скорректирует.
//!
//! Плавность (`[Aim::smooth]`) — доля остатка, добираемая за одну команду:
//! `0` — щелчок на цель, `100` — 5% остатка за тик. Сектор поиска
//! ([`Aim::fov`]) ограничивает, как далеко от прицела берётся цель: вне
//! сектора обзор остаётся игроку.
//!
//! ```no_run
//! # fn main() -> Result<(), String> {
//! // Перехваты отрисовки — источник целей; обводку можно не рисовать.
//! golden_std::esp::Esp::new().outline(false).install()?;
//! golden_std::aim::Aim::new()
//!     .smooth(35)
//!     .fov(30.0)
//!     .activation(golden_std::input::Key::Mouse5) // вести, пока зажата кнопка
//!     .install()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Границы
//!
//! Как ESP и триггербот, это оффлайн-инструмент для игры с ботами: наведение
//! по живым игрокам в мультиплеере — нечестная игра.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use golden_sdk::grenade::angle_vectors;
use golden_sdk::log;
use golden_sdk::math::Vec3;
use golden_sdk::module::InFlight;
use golden_sdk::offsets::hw;
use golden_sdk::process;
use golden_sdk::state::EngineState;
use golden_sdk::studio::WorldHitbox;

use crate::hooks;

/// Сектор поиска цели по умолчанию, градусы от прицела.
const DEFAULT_FOV: f32 = 30.0;
/// Пределы сектора, градусы: меньше одного — мод видит только отдельно
/// стоящую точку, больше 180 — цели за спиной.
const MIN_FOV: f32 = 1.0;
const MAX_FOV: f32 = 180.0;

/// Дальше этой дистанции (юнитов) наводиться не стоит: хитбокс уже крошечный,
/// а точность на полкарты не помогает.
const DEFAULT_RANGE: f32 = 4096.0;

/// Меньше этой угловой ошибки (градусы) цель считается достигнутой: не
/// спорим с мышью на месте и не дёргаем угол без причины.
const ON_TARGET_DEG: f32 = 0.05;

/// Как часто писать статус доводки в лог: не чаще раза в две секунды.
const STATUS_EVERY: Duration = Duration::from_secs(2);

/// Сколько ждать заполнения слота `pfnCreateMove`: движок кладёт туда адрес
/// клиентского `CL_CreateMove` при инициализации клиента, а мод может
/// нагрузиться раньше.
const HOOK_WAIT: Duration = Duration::from_secs(10);

/// Мод установлен: перехваты ESP уже кормят его целями.
static INSTALLED: AtomicBool = AtomicBool::new(false);
/// Выключен из консоли: цели собираются, но угол не правится.
static ENABLED: AtomicBool = AtomicBool::new(true);
/// Плавность 0..=100: доля остатка, которую добираем за одну команду.
static SMOOTH: AtomicU32 = AtomicU32::new(0);
/// Сектор поиска цели, градусы (`f32::to_bits`).
static FOV_BITS: AtomicU32 = AtomicU32::new(DEFAULT_FOV.to_bits());
/// Дистанция наведения, юниты.
static MAX_RANGE: AtomicU32 = AtomicU32::new(DEFAULT_RANGE as u32);
/// VK-код кнопки активации (`0` — кнопки нет, доводка разрешена постоянно).
static ACTIVATION_VK: AtomicU32 = AtomicU32::new(0);

/// Адрес функции, стоявшей в слоте `pfnCreateMove` до нашей подмены:
/// клиентский `CL_CreateMove` либо чужой перехват из этого же модуля
/// (например [`crate::trigger`] в этой же DLL).
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// Сколько раз правили угол (диагностика для watchdog'а).
static CORRECTED: AtomicU64 = AtomicU64::new(0);
/// Видел ли перехват `CL_CreateMove` хоть один вызов: одноразовый признак
/// в логе, что движок ходит через нашу подмену.
static SEEN_CREATE_MOVE: AtomicBool = AtomicBool::new(false);
/// Сколько команд подряд догнал «предохранитель» — сняли огонь игрока, пока
/// прицел ехал к цели (диагностика для `aim_status`).
static SUPPRESSED: AtomicU64 = AtomicU64::new(0);
/// Когда последний раз писали статус (не чаще [`STATUS_EVERY`]).
static STATUS_LOG: Mutex<Option<Instant>> = Mutex::new(None);

/// Выбранная цель доводки: часть тела в мировых координатах.
#[derive(Debug, Clone, Copy)]
struct Chosen {
    /// Слот игрока — для лога.
    index: i32,
    /// Часть тела.
    group: golden_sdk::studio::HitGroup,
    /// Точка наведения (центр хитбокса).
    point: Vec3,
    /// Сколько градусов от прицела до неё.
    error_deg: f32,
    /// Дистанция до неё, юниты.
    distance: f32,
}

impl Chosen {
    /// Лучшая ли цель прежней: меньше угловая ошибка, при равной — ближе.
    fn better_than(self, current: Option<Self>) -> bool {
        match current {
            None => true,
            Some(prior) => {
                self.error_deg < prior.error_deg
                    || (self.error_deg == prior.error_deg && self.distance < prior.distance)
            }
        }
    }
}

/// Настройки аима.
///
/// Механику ставит [`Aim::install`] — сами перехваты отрисовки ставит ESP,
/// аим лишь читает его цели; ставится свой перехват `CL_CreateMove`,
/// которым мод правит `cl.viewangles`.
#[derive(Debug)]
pub struct Aim {
    smooth: u32,
    fov: f32,
    range: f32,
    enabled: bool,
    activation: Option<crate::input::Key>,
}

impl Default for Aim {
    fn default() -> Self {
        Self::new()
    }
}

impl Aim {
    /// Аим без плавности (щелчок на цель), сектор 30°, до 4096 юнитов.
    pub fn new() -> Self {
        Self {
            smooth: 0,
            fov: DEFAULT_FOV,
            range: DEFAULT_RANGE,
            enabled: true,
            activation: None,
        }
    }

    /// Плавность 0..=100: `0` — доводка в ту же команду, `100` — 5% остатка
    /// за команду. Большая плавность мягче выглядит и дольше едет.
    pub fn smooth(mut self, smooth: u32) -> Self {
        self.smooth = smooth.min(100);
        self
    }

    /// Сектор поиска цели, градусы от прицела: цели дальше сектора ведение не
    /// трогает. `180` — весь круг.
    pub fn fov(mut self, fov: f32) -> Self {
        self.fov = fov.clamp(MIN_FOV, MAX_FOV);
        self
    }

    /// Дальше этой дистанции (в юнитах) не наводиться.
    pub fn range(mut self, range: f32) -> Self {
        self.range = range.max(0.0);
        self
    }

    /// Установить ли мод включённым (`false` — «поставить молча», включить
    /// потом командой из консоли, см. [`set_enabled`]).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Кнопка активации: вести прицел, только пока игрок держит эту клавишу
    /// ([`crate::input::Key::held`]). Без кнопки (`None`, по умолчанию)
    /// доводка разрешена постоянно, пока мод включён.
    ///
    /// Опрос системный, не движковый: боковые кнопки мыши работают, даже если
    /// игра их не переваривает. Кнопка только разрешает ведение, обзором между
    /// командами игрок владеет сам — его мышь добавляется к исправленному
    /// углу, доводка поправляет заново.
    pub fn activation(mut self, key: crate::input::Key) -> Self {
        self.activation = Some(key);
        self
    }

    /// Настраивает мод. Перехваты отрисовки не ставятся: их ставит
    /// [`crate::esp::Esp`], и цели приходят только от него. Ставится свой
    /// перехват `CL_CreateMove` — им мод ведёт угол.
    ///
    /// # Errors
    /// Строка с причиной, если мод уже установлен или не встал перехват.
    pub fn install(self) -> Result<(), String> {
        if INSTALLED.swap(true, Ordering::AcqRel) {
            return Err("аим: уже установлен".to_string());
        }
        ENABLED.store(self.enabled, Ordering::Release);
        SMOOTH.store(self.smooth.min(100), Ordering::Release);
        FOV_BITS.store(self.fov.clamp(MIN_FOV, MAX_FOV).to_bits(), Ordering::Release);
        MAX_RANGE.store(self.range.max(0.0) as u32, Ordering::Release);
        ACTIVATION_VK.store(
            self.activation.map_or(0, crate::input::Key::win_vk),
            Ordering::Release,
        );
        if let Err(error) = install_hook(HOOK_WAIT) {
            INSTALLED.store(false, Ordering::Release);
            return Err(error);
        }
        match self.activation {
            Some(key) => log!(
                "аим: установлен, плавность {}, сектор {:.0}°, доводка пока зажата {key}",
                self.smooth,
                self.fov
            ),
            None => log!(
                "аим: установлен, плавность {}, сектор {:.0}°",
                self.smooth,
                self.fov
            ),
        }
        Ok(())
    }
}

/// Включает/выключает доводку. Возвращает прежнее состояние.
pub fn set_enabled(on: bool) -> bool {
    let previous = ENABLED.swap(on, Ordering::AcqRel);
    if previous != on {
        log!(
            "аим: {}",
            if on {
                "включён"
            } else {
                "выключен"
            }
        );
    }
    previous
}

/// Меняет кнопку активации по VK-коду. `0` — вести всегда, без кнопки.
///
/// Тот же код применяет и триггербот
/// ([`crate::trigger::set_activation_vk`]): кнопка должна разрешать огонь и
/// доводку одинаково, иначе прицел ведёт, а выстрел стоит на паузе.
pub fn set_activation_vk(vk: u32) {
    ACTIVATION_VK.store(vk & 0xFF, Ordering::Release);
}

/// Текущая кнопка активации, VK (`0` — вести всегда).
pub fn activation_vk() -> u32 {
    ACTIVATION_VK.load(Ordering::Relaxed)
}

/// Разрешена ли доводка прямо сейчас.
pub fn enabled() -> bool {
    INSTALLED.load(Ordering::Acquire) && ENABLED.load(Ordering::Acquire)
}

/// Меняет плавность (0..=100). Возвращает применённое значение.
pub fn set_smooth(smooth: u32) -> u32 {
    let smooth = smooth.min(100);
    SMOOTH.store(smooth, Ordering::Release);
    log!("аим: плавность {smooth}");
    smooth
}

/// Меняет сектор поиска цели, градусы. Возвращает применённое значение.
pub fn set_fov(degrees: f32) -> f32 {
    let degrees = degrees.clamp(MIN_FOV, MAX_FOV);
    FOV_BITS.store(degrees.to_bits(), Ordering::Release);
    log!("аим: сектор {degrees:.0}°");
    degrees
}

/// Текущая плавность 0..=100 (для статуса в консоли).
pub fn smooth() -> u32 {
    SMOOTH.load(Ordering::Relaxed).min(100)
}

/// Текущий сектор поиска цели, градусы (для статуса в консоли).
pub fn fov() -> f32 {
    f32::from_bits(FOV_BITS.load(Ordering::Relaxed))
}

/// Сколько раз правили угол.
pub fn corrections() -> u64 {
    CORRECTED.load(Ordering::Relaxed)
}

/// Сколько команд подряд предохранитель снимал огонь, пока прицел ехал
/// к цели (для статуса в консоли).
pub fn suppressions() -> u64 {
    SUPPRESSED.load(Ordering::Relaxed)
}

/// Моду нужны (установлен) цели: им пользуется [`crate::esp`], чтобы не
/// дёргать перехват отрисовки впустую, когда аима нет.
pub(crate) fn armed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

/// Статус доводки в лог: не чаще [`STATUS_EVERY`], чтобы 100 команд в секунду
/// не заливали его.
fn status(stage: &str) {
    if let Ok(mut at) = STATUS_LOG.lock() {
        if at.map_or(true, |moment| moment.elapsed() >= STATUS_EVERY) {
            *at = Some(Instant::now());
            log!("аим: {stage}");
        }
    }
}

/// Наш `CL_CreateMove`: движок зовёт его вместо клиентской функции, адрес
/// которой в слоте [`hw::P_CREATE_MOVE`] подменён (см. [`install_hook`]).
///
/// Сначала оригинал: клиент собирает команду, включая движение мыши в
/// `cl.viewangles`. После него доводка правит `cl.viewangles` — движок
/// перепишет полем и камеру, и отправляемую команду (см. модульную доку).
///
/// # Safety
/// Сигнатура и ABI (`cdecl`) совпадают с клиентской функцией: движок зовёт её
/// через указатель и сам чистит стек.
unsafe extern "C" fn create_move(frametime: f32, cmd: *mut c_void, active: i32) {
    let _inside = InFlight::enter();
    let original = ORIGINAL.load(Ordering::Acquire);
    if original == 0 || cmd.is_null() {
        return;
    }
    // SAFETY: адрес из слота — клиентский `CL_CreateMove` либо чужой перехват
    // из этого же модуля; сигнатура та же.
    let original: unsafe extern "C" fn(f32, *mut c_void, i32) =
        unsafe { core::mem::transmute(original) };
    unsafe { original(frametime, cmd, active) };
    if !SEEN_CREATE_MOVE.swap(true, Ordering::AcqRel) {
        log!("аим: перехват CL_CreateMove работает (active={active})");
    }
    if active == 0 {
        return;
    }
    // Безусловная строка состояния: какие кнопки видит система, что говорят
    // гейты и есть ли цели — до всех гейтов, чтобы один прогон отвечал на все
    // вопросы сразу, даже когда доводка молчит.
    heartbeat();
    if steer() == Steer::Steering {
        // Доводка не дошла до цели: гасим огонь игрока, пока прицел едет, —
        // иначе выстрел уйдёт мимо. Бит возвращается движку в
        // [`suppress_fire`] до сборки отправляемой команды.
        suppress_fire();
    }
}

/// Строка состояния всех входов и гейтов: раз в [`STATUS_EVERY`], до того как
/// гейти начнут отсекать вызовы.
///
/// По маске мыши видно, жмёт ли игрок то, чего ждёт мод, и что именно
/// система считает зажатым.
fn heartbeat() {
    let mouse = crate::input::MouseMask::held();
    let activation = ACTIVATION_VK.load(Ordering::Relaxed);
    let held = activation == 0 || crate::input::held_vk(activation);
    status(&format!(
        "кадр: мышь [лкм={}, пкм={}, скм={}, xb1={}, xb2={}], \
         активация VK 0x{activation:02X} зажата={held}, целей={}, \
         время={:.1}, меню={}, чат={}, hp={}, правок={}, снятий={}, \
         cl.buttons=0x{:04X}",
        mouse.left,
        mouse.right,
        mouse.middle,
        mouse.x1,
        mouse.x2,
        crate::trigger::snapshot_count(),
        EngineState::time(),
        EngineState::in_menu(),
        EngineState::chat_opened(),
        EngineState::health(),
        CORRECTED.load(Ordering::Relaxed),
        SUPPRESSED.load(Ordering::Relaxed),
        cl_buttons(),
    ));
}

/// Живые кнопки `cl.buttons` — что клиент держит прямо сейчас (диагностика).
fn cl_buttons() -> u16 {
    use golden_sdk::offsets::cl;
    let Some(base) = process::hw_base() else {
        return 0;
    };
    let address = base + cl::BUTTONS;
    if !process::is_readable(address) {
        return 0;
    }
    // SAFETY: поле движка `unsigned short`, читаем из перехвата `CL_CreateMove`.
    unsafe { process::read(address) }
}

/// Итог доводки одного тика.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Steer {
    /// Не ведём совсем: гейты, кнопка или цели закрыли доводку.
    Idle,
    /// Прицел дошёл до цели — огонь игроку разрешён.
    OnTarget,
    /// Ведём, но ещё не навели: огонь игрока под глушилкой.
    Steering,
}

/// Одна команда: выбрать ближайшую часть тела и на шаг приблизить прицел.
fn steer() -> Steer {
    if !INSTALLED.load(Ordering::Acquire) || !ENABLED.load(Ordering::Acquire) {
        return Steer::Idle;
    }
    if !EngineState::playable() {
        // Точная причина молчания важнее тишины: каждое поле гейта в логе,
        // чтобы следующий прогон сразу назвал виновника.
        status(&format!(
            "не играем: время={:.1}, меню={}, чат={}, здоровье={}",
            EngineState::time(),
            EngineState::in_menu(),
            EngineState::chat_opened(),
            EngineState::health()
        ));
        return Steer::Idle;
    }
    // Кнопка активации: не зажата — не ведём, обзор остаётся игроку.
    let activation = ACTIVATION_VK.load(Ordering::Relaxed);
    if activation != 0 && !crate::input::held_vk(activation) {
        status(&format!(
            "на паузе: кнопка активации (VK 0x{activation:02X}) не зажата"
        ));
        return Steer::Idle;
    }
    let view = EngineState::view_angles_live();
    if !valid_angles(view) {
        return Steer::Idle;
    }
    let (forward, _, _) = angle_vectors(view);
    let origin = EngineState::view_origin();
    let Some(chosen) = best_part(origin, forward) else {
        if crate::trigger::snapshot_count() > 0 {
            status("цели есть, но в секторе нет ни одной части тела");
        } else {
            // Не фильтр и не сектор — снимок кадра вообще пуст: рендер не
            // принёс ни одного живого врага (или ESP не кормит цели).
            status("целей нет: снимок кадра пуст");
        }
        return Steer::Idle;
    };
    if chosen.error_deg <= ON_TARGET_DEG {
        status(&format!(
            "на цели: игрок #{}, {}",
            chosen.index,
            chosen.group.name()
        ));
        return Steer::OnTarget;
    }
    let desired = direction_to_angles(chosen.point - origin);
    let corrected = step_toward(view, desired, smooth_share());
    if corrected == view || !valid_angles(corrected) {
        return Steer::Steering;
    }
    // SAFETY: правка угла допускается только из `CreateMove`, до отправки
    // пакета — мы внутри перехвата `CreateMove`.
    EngineState::set_view_angles(corrected);
    CORRECTED.fetch_add(1, Ordering::Relaxed);
    status(&format!(
        "веду прицел: игрок #{}, {}, доводка {:.1}°, дистанция {:.0}",
        chosen.index,
        chosen.group.name(),
        chosen.error_deg,
        chosen.distance
    ));
    Steer::Steering
}

/// Предохранитель доводки: снимает «огонь» из `cl.buttons`, пока прицел
/// едет к цели.
///
/// `cmd->buttons` чистить бесполезно — движок OR-ит в отправляемую команду
/// живые кнопки из `cl.buttons` уже после `CL_CreateMove` (см.
/// [`golden_sdk::offsets::cl::BUTTONS`]). На цели предохранитель не трогает:
/// игроки и триггербот стреляют как обычно.
fn suppress_fire() {
    use golden_sdk::offsets::{cl, usercmd};
    let Some(base) = process::hw_base() else {
        return;
    };
    let address = base + cl::BUTTONS;
    if !process::is_readable(address) {
        return;
    }
    // SAFETY: поле движка `unsigned short`, запись по выровненному адресу из
    // перехвата `CL_CreateMove` — один и тот же поток каждый тик.
    let held: u16 = unsafe { process::read(address) };
    let cleaned = held & !usercmd::IN_ATTACK;
    if cleaned != held {
        // SAFETY: адрес тот же, читаемость проверена.
        unsafe { process::write(address, cleaned) };
        SUPPRESSED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Ищет часть тела для наведения: минимальная угловая ошибка от прицела
/// среди хитбоксов целей кадра (список наполняет [`crate::esp`], его держит
/// [`crate::trigger`]).
fn best_part(origin: Vec3, forward: Vec3) -> Option<Chosen> {
    let fov = fov_deg();
    let range = MAX_RANGE.load(Ordering::Relaxed) as f32;
    let mut best: Option<Chosen> = None;
    crate::trigger::for_each_snapshot(|index, hitbox| {
        if let Some(candidate) = aim_at(index, hitbox, origin, forward, fov, range) {
            if candidate.better_than(best) {
                best = Some(candidate);
            }
        }
    });
    best
}

/// Оценка одного хитбокса как цели доводки.
///
/// `None` — хитбокс вне сектора или дистанции. Точка наведения — центр
/// хитбокса через матрицу кости: центр AABB у повёрнутого хитбокса смещён от
/// настоящего.
fn aim_at(
    index: i32,
    hitbox: &WorldHitbox,
    origin: Vec3,
    forward: Vec3,
    fov_deg: f32,
    range: f32,
) -> Option<Chosen> {
    let point = hitbox.center();
    let to_aim = point - origin;
    let distance = to_aim.length();
    // Ближе 8 юнитов части тела не бывают: это мусорная поза или хитбокс
    // рядом с глазами — наводиться нельзя.
    if !to_aim.is_finite() || distance < 8.0 || distance > range {
        return None;
    }
    let direction = to_aim / distance;
    let dot = forward.dot(direction).clamp(-1.0, 1.0);
    let error_deg = dot.acos().to_degrees();
    if !(0.0..=fov_deg).contains(&error_deg) {
        return None;
    }
    Some(Chosen {
        index,
        group: hitbox.group,
        point,
        error_deg,
        distance,
    })
}

/// Доля остатка доводки за одну команду: `smooth = 0` → вся сразу,
/// `smooth = 100` → 5% за тик, иначе ряд не сходится вовсе.
fn smooth_share() -> f32 {
    let smooth = SMOOTH.load(Ordering::Relaxed).min(100);
    (1.0 - smooth as f32 / 100.0).clamp(0.05, 1.0)
}

/// Текущий сектор поиска, градусы.
fn fov_deg() -> f32 {
    f32::from_bits(FOV_BITS.load(Ordering::Relaxed))
}

/// Углы GoldSrc для направления в мировых координатах:
/// `pitch` (x) положителен вниз, `yaw` (y) против часовой в плоскости XY.
///
/// Обратное преобразование к [`angle_vectors`]: у того
/// `forward = (cos p · cos y, cos p · sin y, −sin p)`.
fn direction_to_angles(direction: Vec3) -> Vec3 {
    let length = direction.length();
    if !direction.is_finite() || length < 1e-6 {
        return Vec3::ZERO;
    }
    let unit = direction / length;
    let pitch = (-unit.z).asin().to_degrees();
    let yaw = unit.y.atan2(unit.x).to_degrees();
    Vec3::new(pitch, yaw, 0.0)
}

/// Нормализует угол в `(-180, 180]`: у `yaw` шов на 180°, переход через
/// который обязан идти кратчайшей стороной, а не через ноль.
fn normalize_deg180(angle: f32) -> f32 {
    let shifted = (angle + 180.0) % 360.0;
    let shifted = if shifted < 0.0 { shifted + 360.0 } else { shifted };
    shifted - 180.0
}

/// Шаг доводки: доля `share` от разницы до желаемого угла.
///
/// `yaw` идёт кратчайшей стороной (разница нормализуется в ±180°), итог
/// укладывается в `(-180, 180]`; `pitch` зажимается в границы ±89°; roll не
/// трогаем.
fn step_toward(view: Vec3, desired: Vec3, share: f32) -> Vec3 {
    let pitch = (view.x + (desired.x - view.x) * share).clamp(-89.0, 89.0);
    let yaw = normalize_deg180(view.y + normalize_deg180(desired.y - view.y) * share);
    Vec3::new(pitch, yaw, view.z)
}

/// Похожи ли углы на живой `cl.viewangles` (а не на мусор при смене карты).
fn valid_angles(angles: Vec3) -> bool {
    angles.is_finite()
        && angles.x.abs() <= 90.0
        && angles.y.abs() <= 360.0
        && angles.z.abs() <= 180.0
}

/// Ставит подмену слота `pfnCreateMove`: движок начнёт звать наш
/// [`create_move`], а прежний адрес сохранится в [`ORIGINAL`] как оригинал.
fn install_hook(timeout: Duration) -> Result<(), String> {
    if hook_live() {
        return Ok(());
    }
    let hw_base = process::hw_base().ok_or("hw.dll не загружен")?;
    let client = process::client_base().ok_or("client.dll не загружен")?;
    let slot = hw_base + hw::P_CREATE_MOVE;
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: слот — глобал hw.dll; читаем указатель как есть.
        if let Some(previous) = unsafe { process::read_ptr(slot) } {
            if !callable_create_move(previous, client) {
                return Err(format!(
                    "в pfnCreateMove не клиентский код и не перехват наших модов: {previous:#x}"
                ));
            }
            // Оригинал запоминаем до подмены: движок может вызвать хук сразу
            // после записи указателя, и хук должен знать, куда идти.
            ORIGINAL.store(previous, Ordering::Release);
            // SAFETY: слот — проверенное поле hw.dll; наша функция совпадает
            // по ABI с клиентской, запись выровненного указателя атомарна.
            if let Err(error) = unsafe { hooks::pointer(slot, create_move as *const ()) } {
                ORIGINAL.store(0, Ordering::Release);
                return Err(error.to_string());
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("pfnCreateMove пуст дольше {timeout:?}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Годится ли прежний адрес из слота `pfnCreateMove` в качестве оригинала:
/// либо код `client.dll` (нетронутый движковый путь), либо перехват из
/// нашего модуля — своего или чужого: у цепочки наши моды (аим поверх
/// триггербота из другой DLL, наоборот — любой порядок установки).
///
/// Выгрузка цепочки безопасна в порядке «сверху вниз»: аим ставит свой
/// студио-интерфейс (ESP) поверх чужого, поэтому нижний мод, пока аим жив,
/// выгрузиться не сможет ([`crate::unload::can_unload`] это проверяет).
fn callable_create_move(previous: usize, client: usize) -> bool {
    process::is_code(previous, client) || golden_sdk::module::is_our_kind(previous)
}

/// Стоит ли в слоте движка наша функция.
fn hook_live() -> bool {
    let Some(hw_base) = process::hw_base() else {
        return false;
    };
    let slot = hw_base + hw::P_CREATE_MOVE;
    // SAFETY: слот — глобал hw.dll; читаем указатель.
    unsafe { process::read_ptr(slot) == Some(create_move as *const () as usize) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use golden_sdk::studio::{BoneMatrix, HitGroup, WorldHitbox};

    /// Хитбокс-точка с центром в `center` (габариты ±2 юнита, кость перенесена
    /// в центр): `center()` возвращает ровно `center`.
    fn hitbox(group: HitGroup, center: Vec3) -> WorldHitbox {
        WorldHitbox {
            group,
            bone: 0,
            min: center - Vec3::new(2.0, 2.0, 2.0),
            max: center + Vec3::new(2.0, 2.0, 2.0),
            matrix: BoneMatrix::translated(center),
            local_min: Vec3::new(-2.0, -2.0, -2.0),
            local_max: Vec3::new(2.0, 2.0, 2.0),
        }
    }

    #[test]
    fn direction_angles_roundtrip() {
        let cases = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(20.0, 45.0, 0.0),
            Vec3::new(-89.5, 179.0, 0.0),
            Vec3::new(80.0, -90.0, 0.0),
            Vec3::new(45.0, -170.0, 0.0),
        ];
        for angles in cases {
            let (forward, _, _) = angle_vectors(angles);
            let back = direction_to_angles(forward);
            assert!((back.x - angles.x).abs() < 1e-3, "{angles:?} -> {back:?}");
            let yaw_gap = normalize_deg180(angles.y - back.y);
            assert!(yaw_gap.abs() < 1e-3, "{angles:?} -> {back:?}");
            assert!(back.z.abs() < 1e-6);
        }
        // Мусор не превращается в углы.
        assert_eq!(direction_to_angles(Vec3::ZERO), Vec3::ZERO);
    }

    #[test]
    fn normalize_wraps_all_quadrants() {
        assert!((normalize_deg180(0.0)).abs() < 1e-6);
        assert!((normalize_deg180(170.0) - 170.0).abs() < 1e-6);
        assert!((normalize_deg180(181.0) - (-179.0)).abs() < 1e-6);
        assert!((normalize_deg180(-181.0) - 179.0).abs() < 1e-6);
        assert!((normalize_deg180(350.0) - (-10.0)).abs() < 1e-6);
        assert!((normalize_deg180(-350.0) - 10.0).abs() < 1e-6);
        assert!((normalize_deg180(540.0) - (-180.0)).abs() < 1e-6);
    }

    #[test]
    fn step_takes_short_side_of_yaw() {
        // Шов yaw: от 179° до −179° на самом деле два градуса вперёд.
        let view = Vec3::new(0.0, 179.0, 0.0);
        let desired = Vec3::new(0.0, -179.0, 0.0);
        let full = step_toward(view, desired, 1.0);
        assert!((full.y - (-179.0)).abs() < 1e-4, "y = {}", full.y);
        // Середина шва: нормализация даёт −180, что то же направление, что
        // и +180.
        let half = step_toward(view, desired, 0.5);
        assert!(
            (half.y.abs() - 180.0).abs() < 1e-4,
            "y = {}",
            half.y
        );
        // Обычный случай без шва и без зажима.
        let plain = step_toward(Vec3::new(0.0, 10.0, 0.0), Vec3::new(0.0, 20.0, 0.0), 0.25);
        assert!((plain.y - 12.5).abs() < 1e-4);
        // Pitch не вылезает за границы ±89° даже при перелёте.
        let clamped = step_toward(Vec3::new(80.0, 0.0, 0.0), Vec3::new(88.0, 0.0, 0.0), 5.0);
        assert_eq!(clamped.x, 89.0);
        // Roll не трогаем.
        assert_eq!(step_toward(Vec3::new(0.0, 0.0, 5.0), Vec3::new(1.0, 2.0, 5.0), 0.5).z, 5.0);
    }

    #[test]
    fn nearest_part_wins_over_farther_one() {
        // Прицел смотрит в горизонт (pitch 0) вдоль +X, глаза на высоте 64.
        let origin = Vec3::new(0.0, 0.0, 64.0);
        let forward = angle_vectors(Vec3::new(0.0, 0.0, 0.0)).0;
        // Голова в 5.7° от прицела, грудь в 0.6°, нога в 17.7°: выбрать грудь.
        let head = aim_at(
            1,
            &hitbox(HitGroup::Head, Vec3::new(500.0, 50.0, 64.0)),
            origin,
            forward,
            30.0,
            4096.0,
        )
        .unwrap();
        let chest = aim_at(
            1,
            &hitbox(HitGroup::Chest, Vec3::new(500.0, 5.0, 64.0)),
            origin,
            forward,
            30.0,
            4096.0,
        )
        .unwrap();
        let leg = aim_at(
            1,
            &hitbox(HitGroup::LeftLeg, Vec3::new(250.0, 80.0, 64.0)),
            origin,
            forward,
            30.0,
            4096.0,
        )
        .unwrap();
        let best = [head, chest, leg]
            .into_iter()
            .reduce(|acc, candidate| if candidate.better_than(Some(acc)) { candidate } else { acc })
            .unwrap();
        assert_eq!(best.group, HitGroup::Chest);
    }

    #[test]
    fn ties_prefer_closer_target() {
        let near = Chosen {
            index: 1,
            group: HitGroup::Chest,
            point: Vec3::ZERO,
            error_deg: 1.0,
            distance: 300.0,
        };
        let far = Chosen {
            index: 2,
            group: HitGroup::Chest,
            point: Vec3::ZERO,
            error_deg: 1.0,
            distance: 900.0,
        };
        assert!(near.better_than(Some(far)));
        assert!(!far.better_than(Some(near)));
        assert!(far.better_than(None));

        let sharper = Chosen {
            index: 3,
            group: HitGroup::Head,
            point: Vec3::ZERO,
            error_deg: 0.5,
            distance: 900.0,
        };
        assert!(sharper.better_than(Some(near)));
        assert!(!near.better_than(Some(sharper)));
    }

    #[test]
    fn out_of_sector_or_range_is_rejected() {
        let origin = Vec3::new(0.0, 0.0, 64.0);
        let forward = angle_vectors(Vec3::new(0.0, 0.0, 0.0)).0;
        // За спиной — 180°, всегда вне сектора.
        let behind = hitbox(HitGroup::Chest, Vec3::new(-500.0, 0.0, 64.0));
        assert!(aim_at(1, &behind, origin, forward, 30.0, 4096.0).is_none());
        // Боковая цель: 45° от прицела — вне сектора 30°, но внутри 60°.
        let side = hitbox(HitGroup::Chest, Vec3::new(500.0, 500.0, 64.0));
        assert!(aim_at(1, &side, origin, forward, 30.0, 4096.0).is_none());
        assert!(aim_at(1, &side, origin, forward, 60.0, 4096.0).is_some());
        // Дальше лимита дистанции — не цель.
        let far = hitbox(HitGroup::Chest, Vec3::new(5000.0, 0.0, 64.0));
        assert!(aim_at(1, &far, origin, forward, 30.0, 4096.0).is_none());
        // Слишком близко — тоже: это мы сами.
        let close = hitbox(HitGroup::Chest, Vec3::new(4.0, 0.0, 64.0));
        assert!(aim_at(1, &close, origin, forward, 30.0, 4096.0).is_none());
    }
}
