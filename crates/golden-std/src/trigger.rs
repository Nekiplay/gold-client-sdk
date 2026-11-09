//! Триггербот: стреляет, когда прицел стоит на игроке.
//!
//! Источник целей — перехват отрисовки игрока, который ставит
//! [`crate::esp::Esp`]: игроки, фильтр которых прошёл ESP («только враги»),
//! отдаются движковым рендером, и в момент рисования меша [`crate::esp`]
//! снимает с каждого хитбоксы — из той же позы, в которой движок нарисовал
//! тело. Видимость проверяется буфером глубины, тем же, по которому обводка
//! ESP красит видимую часть: точка входа прицельного луча сравнивается с
//! глубиной пикселя под перекрестием. Никаких собственных трассировок
//! геометрии карты.
//!
//! ESP при этом может не рисовать ничего ([`crate::esp::Esp::outline`]`(
//! false)`): перехваты остаются, цели продолжают приходить, а триггербот не
//! выдает себя ни единым пикселем.
//!
//! Огонь — кнопка `IN_ATTACK` прямо в исходящей команде игрока: подмена
//! слота `pfnCreateMove` перехватывает
//! `CL_CreateMove`, и после оригинала добавляет бит в `cmd->buttons`.
//! Команда уходит на сервер с этой же кнопкой — без командного буфера
//! (`+attack`/`-attack` разбирается движком лишь на следующем кадре, отсюда
//! лишняя задержка), без перехвата `Key_Event` и без риска оставить кнопку
//! зажатой: бит пишется только в команды при живом перехвате, поэтому
//! выгрузка «отпускает» кнопку сама.
//!
//! Кнопка активации ([`Trigger::activation`]) — по желанию: с ней мод
//! стреляет, только пока игрок держит выбранную клавишу (например,
//! [`crate::input::Key::Mouse5`] — боковая кнопка мыши). Опрос системный,
//! поэтому работает даже для клавиш, которые игра сама не переваривает.
//!
//! ```no_run
//! # fn main() -> Result<(), String> {
//! // Перехваты отрисовки — источник целей; обводку можно не рисовать.
//! golden_std::esp::Esp::new().outline(false).install()?;
//! golden_std::trigger::Trigger::new()
//!     .delay(std::time::Duration::from_millis(80)) // «реакция» игрока
//!     .activation(golden_std::input::Key::Mouse5) // огонь, пока зажата mouse5
//!     .install()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Границы
//!
//! Как и ESP, это оффлайн-инструмент для игры с ботами: автострельба по
//! игрокам в мультиплеере — нечестная игра.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use golden_sdk::draw::Gl;
use golden_sdk::engine::EntityRef;
use golden_sdk::log;
use golden_sdk::math::Vec3;
use golden_sdk::module::{InFlight, Module};
use golden_sdk::offsets::{hw, usercmd};
use golden_sdk::process;
use golden_sdk::state::EngineState;
use golden_sdk::studio::{HitGroup, WorldHitbox};

use crate::hooks;

/// Дальше этой дистанции (в юнитах) стрелять не стоит: точность мала, а буфер
/// глубины на дальних стенах уже почти неотличим.
const DEFAULT_RANGE: u32 = 4096;

/// Запас при сравнении глубин: хитбокс чуть выпирает из меша тела, и точка
/// входа законно «ближе» записанной в буфер. 24-битный буфер даёт кванты того
/// же порядка рядом с игроком.
const DEPTH_EPSILON: f32 = 0.002;

/// Сколько не писать в лог про один и тот же выстрел.
const LOG_THROTTLE: Duration = Duration::from_millis(500);

/// Сколько ждать заполнения слота `pfnCreateMove`: движок кладёт туда адрес
/// клиентского `CL_CreateMove` при инициализации клиента, а мод может
/// нагрузиться раньше.
const HOOK_WAIT: Duration = Duration::from_secs(10);

/// Моду установлен: перехваты ESP уже кормят его целями.
static INSTALLED: AtomicBool = AtomicBool::new(false);
/// Выключен из консоли: цели собираются, но кнопка не нажимается.
static ENABLED: AtomicBool = AtomicBool::new(true);
/// «Время реакции», мс.
static DELAY_MS: AtomicU32 = AtomicU32::new(0);
/// Стрелять только по голове.
static HEAD_ONLY: AtomicBool = AtomicBool::new(false);
/// VK-код кнопки активации (`0` — кнопки нет, огонь разрешён постоянно).
static ACTIVATION_VK: AtomicU32 = AtomicU32::new(0);
/// Дистанция огня, юниты.
static MAX_RANGE: AtomicU32 = AtomicU32::new(DEFAULT_RANGE);

/// Цели текущего кадра: их наполняет обычный рендер сущностей, а чистит
/// [`frame_started`] в конце сцены.
static TARGETS: Mutex<Vec<Target>> = Mutex::new(Vec::new());
/// С какой поры цель стоит под прицелом: `(момент, игрок)`. Ключ — сам игрок:
/// переход прицела между его хитбоксами (грудь → голова) отсчёт не
/// перезапускает, смена цели или сход с неё — перезапускает.
static ACQUIRED: Mutex<Option<(Instant, i32)>> = Mutex::new(None);
/// Зажата ли кнопка огня: решение кадра, его исполняет [`create_move`],
/// добавляя `IN_ATTACK` в команды игрока.
static WANT_FIRE: AtomicBool = AtomicBool::new(false);
/// Адрес функции, стоявшей в слоте `pfnCreateMove` до нашей подмены:
/// клиентский `CL_CreateMove` либо чужой перехват из этого же модуля
/// (например [`crate::aim`]).
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
/// Сколько раз стреляли.
static FIRED: AtomicU64 = AtomicU64::new(0);
/// Когда последний раз писали статус решения (диагностика цепочки).
static STATUS_LOG: Mutex<Option<Instant>> = Mutex::new(None);

/// Как часто писать статус решения в лог: не чаще раза в две секунды.
const STATUS_EVERY: Duration = Duration::from_secs(2);

/// Сколько целей собрано в текущем кадре.
pub(crate) fn target_count() -> usize {
    TARGETS.lock().map(|targets| targets.len()).unwrap_or(0)
}

/// Диагностика решения: одна строка раз в [`STATUS_EVERY`], по ней видно, на
/// каком шаге стоит цепочка «цели → прицел → видимость → реакция → огонь».
fn status(stage: &str) {
    if let Ok(mut at) = STATUS_LOG.lock() {
        if at.map_or(true, |moment| moment.elapsed() >= STATUS_EVERY) {
            *at = Some(Instant::now());
            log!("триггербот: {stage}: целей в кадре {}", target_count());
        }
    }
}
/// Матрица проекции кадра: снимается в перехвате отрисовки игрока.
static PROJECTION: Mutex<Option<[f32; 16]>> = Mutex::new(None);
/// Когда последний раз писали в лог об огне.
static LAST_LOG: Mutex<Option<Instant>> = Mutex::new(None);

/// Одна цель кадра: игрок и его хитбоксы в мировых координатах.
#[derive(Clone)]
struct Target {
    /// Адрес сущности: по нему обновляется запись при повторном заходе.
    entity: usize,
    /// Слот игрока — для лога и отладки.
    index: i32,
    /// Хитбоксы, снятые из позы обводки.
    hitboxes: Vec<WorldHitbox>,
}

/// Настройки триггербота.
///
/// Механику ставит [`Trigger::install`] — сами перехваты ставит ESP,
/// триггербот лишь подписывается на его данные.
#[derive(Debug)]
pub struct Trigger {
    delay: Duration,
    head_only: bool,
    range: f32,
    enabled: bool,
    activation: Option<crate::input::Key>,
}

impl Default for Trigger {
    fn default() -> Self {
        Self::new()
    }
}

impl Trigger {
    /// Триггербот без задержки, по всем видимым частям, до 4096 юнитов.
    pub fn new() -> Self {
        Self {
            delay: Duration::ZERO,
            head_only: false,
            range: DEFAULT_RANGE as f32,
            enabled: true,
            activation: None,
        }
    }

    /// «Время реакции»: цель должна постоять под прицелом столько, прежде чем
    /// мод нажмёт кнопку. Смена цели или части тела запускает отсчёт заново.
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Стрелять только по голове.
    pub fn head_only(mut self, head_only: bool) -> Self {
        self.head_only = head_only;
        self
    }

    /// Дальше этого (в юнитах) не стрелять.
    pub fn range(mut self, range: f32) -> Self {
        self.range = range;
        self
    }

    /// Установить ли мод включённым (`false` — «поставить молча», включить
    /// потом командой из консоли, см. [`set_enabled`]).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Кнопка активации: стрелять, только пока игрок держит эту клавишу
    /// ([`crate::input::Key::held`]). Без кнопки (`None`, по умолчанию) огонь
    /// разрешён постоянно, пока мод включён.
    ///
    /// Опрос системный, не движковый: боковые кнопки мыши работают даже если
    /// игра их не переваривает, а собственный выстрел игрока кнопку не
    /// занимает — кнопка только разрешает огонь, но не нажимает его.
    pub fn activation(mut self, key: crate::input::Key) -> Self {
        self.activation = Some(key);
        self
    }

    /// Настраивает мод. Перехваты отрисовки не ставятся: их ставит
    /// [`crate::esp::Esp`], и цели приходят только от него. Ставится свой
    /// перехват `CL_CreateMove` — им мод нажимает кнопку огня.
    ///
    /// # Errors
    /// Строка с причиной, если мод уже установлен или не встал перехват.
    pub fn install(self) -> Result<(), String> {
        if INSTALLED.swap(true, Ordering::AcqRel) {
            return Err("триггербот: уже установлен".to_string());
        }
        ENABLED.store(self.enabled, Ordering::Release);
        HEAD_ONLY.store(self.head_only, Ordering::Release);
        ACTIVATION_VK.store(
            self.activation.map_or(0, crate::input::Key::win_vk),
            Ordering::Release,
        );
        DELAY_MS.store(
            self.delay.as_millis().min(u128::from(u32::MAX)) as u32,
            Ordering::Release,
        );
        MAX_RANGE.store(self.range.max(0.0) as u32, Ordering::Release);
        // Чистка при выгрузке: отпустить кнопку до того, как реестр снимет
        // перехват, — перестать писать `IN_ATTACK` в команды немедленно.
        // «Залипание» всё равно исключено: после снятия хука бита в команды
        // попасть не может, чистка лишь обрывает огонь на границе выгрузки.
        if !crate::unload::on_unload(ceasefire_cleanup) {
            INSTALLED.store(false, Ordering::Release);
            return Err("триггербот: выгрузка уже идёт".to_string());
        }
        // Кнопка огня: перехват `CL_CreateMove` добавляет `IN_ATTACK` в
        // команду того кадра, когда цель под прицелом, — командный буфер не
        // парсится, задержки `+attack`/`-attack` нет.
        if let Err(error) = install_hook(HOOK_WAIT) {
            INSTALLED.store(false, Ordering::Release);
            return Err(error);
        }
        let delay = self.delay.as_millis();
        match self.activation {
            Some(key) => log!(
                "триггербот: установлен, реакция {delay} мс, огонь пока зажата {key}, кнопка — IN_ATTACK в CL_CreateMove"
            ),
            None => log!(
                "триггербот: установлен, реакция {delay} мс, кнопка — IN_ATTACK в CL_CreateMove"
            ),
        }
        Ok(())
    }
}

/// Чистка для [`crate::unload`]: отпустить кнопку — [`ceasefire`] как
/// `FnOnce`.
fn ceasefire_cleanup() {
    ceasefire();
}

/// Включает/выключает огонь. Возвращает прежнее состояние.
pub fn set_enabled(on: bool) -> bool {
    let previous = ENABLED.swap(on, Ordering::AcqRel);
    if previous != on {
        log!(
            "триггербот: {}",
            if on {
                "включён"
            } else {
                "выключен"
            }
        );
    }
    previous
}

/// Меняет кнопку активации по VK-коду. `0` — огонь разрешён всегда, без
/// кнопки.
///
/// Код применяют тот же, что у аима ([`crate::aim::set_activation_vk`]):
/// кнопка должна разрешать огонь и доводку одинаково, иначе прицел ведёт,
/// а выстрел стоит на паузе.
pub fn set_activation_vk(vk: u32) {
    ACTIVATION_VK.store(vk & 0xFF, Ordering::Release);
}

/// Разрешён ли огонь прямо сейчас.
pub fn enabled() -> bool {
    INSTALLED.load(Ordering::Acquire) && ENABLED.load(Ordering::Acquire)
}

/// Сколько раз стреляли.
pub fn fired() -> u64 {
    FIRED.load(Ordering::Relaxed)
}

/// Моду нужен (установлен и его данные собирают): им пользуется [`crate::esp`],
/// чтобы не дёргать лишний раз GL и замки, когда триггербота нет.
pub(crate) fn armed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

/// Конец кадра: список целей следующего кадра стартует пустым.
///
/// Зовётся в конец 3D-сцены **после** [`decide`]: цели собирает обычный рендер
/// сущностей в начале кадра, решение читает их в конце сцены, и только потом
/// список чистится. Чистка до решения оставляла бы его с пустым списком.
///
/// Списком пользуются и триггербот, и аим ([`crate::aim`]): чистим, когда
/// установлен хотя бы один из них.
pub(crate) fn frame_started() {
    if !INSTALLED.load(Ordering::Acquire) && !crate::aim::armed() {
        return;
    }
    if let Ok(mut targets) = TARGETS.lock() {
        targets.clear();
    }
}

/// Забирает хитбоксы игрока, которого ESP обводит прямо сейчас.
///
/// Один игрок рисуется несколькими вызовами `StudioRenderModel` (тело,
/// оружие) — запись обновляется по адресу сущности, дубликатов нет.
///
/// Потребитель списка — не только триггербот, но и аим ([`crate::aim`]):
/// хитбоксы сохраняются, если установлен хотя бы один из них.
pub(crate) fn add_target(entity: EntityRef, hitboxes: Vec<WorldHitbox>) {
    if (!INSTALLED.load(Ordering::Acquire) && !crate::aim::armed()) || hitboxes.is_empty() {
        return;
    }
    let target = Target {
        entity: entity.addr(),
        index: entity.player_index(),
        hitboxes,
    };
    if let Ok(mut targets) = TARGETS.lock() {
        // Один игрок рисуется несколькими вызовами `StudioRenderModel` (тело,
        // оружие): запись обновляется по адресу сущности, дубликатов нет.
        if let Some(existing) = targets.iter_mut().find(|slot| slot.entity == target.entity) {
            *existing = target;
        } else {
            targets.push(target);
        }
    }
}

/// Даёт перебрать цели кадра: `(номер игрока, хитбокс в мировых координатах)`.
///
/// Так триггербот решает выстрел в конце сцены, где список ещё жив.
///
/// # Safety / Потоки
/// Создан для вызова из перехватов отрисовки; аиму из `CL_CreateMove`
/// предназначен [`for_each_snapshot`] — рабочий список в это время пуст
/// (см. [`publish_snapshot`]).
pub(crate) fn for_each_hitbox(mut visit: impl FnMut(i32, &WorldHitbox)) {
    let Ok(targets) = TARGETS.lock() else {
        return;
    };
    for target in targets.iter() {
        for hitbox in &target.hitboxes {
            visit(target.index, hitbox);
        }
    }
}

/// Снимок целей кадра — для потребителей, работающих между кадрами.
///
/// `CL_CreateMove` движок зовёт в промежутке «после чистки рабочего списка
/// — до заполнения на отрисовке», и аим ([`crate::aim`]) там всегда видел бы
/// пустоту. Снимок публикуется в конце сцены (см. [`publish_snapshot`]) и
/// живёт весь следующий кадр; тиры отрисовки при этом не дублируются: боты
/// за кадр не телепортируются.
static SNAPSHOT: Mutex<Vec<Target>> = Mutex::new(Vec::new());

/// Публикует снимок целей: копирует рабочий список **после** [`decide`] и
/// **до** [`frame_started`] — когда цели ещё живы.
///
/// Зовётся один раз в конце сцены (рендер-поток); клону пары десятков
/// хитбоксов цена копеечна против пользы.
pub(crate) fn publish_snapshot() {
    if !crate::aim::armed() {
        return;
    }
    if let (Ok(targets), Ok(mut snapshot)) = (TARGETS.lock(), SNAPSHOT.lock()) {
        snapshot.clear();
        snapshot.extend(targets.iter().cloned());
    }
}

/// Даёт перебрать снимок целей — то, что видел конец кадра.
///
/// Потокобезопасно: зовётся аимом из `CL_CreateMove` (клиентский поток),
/// обновляется рендер-потоком раз в кадр.
pub(crate) fn for_each_snapshot(mut visit: impl FnMut(i32, &WorldHitbox)) {
    let Ok(snapshot) = SNAPSHOT.lock() else {
        return;
    };
    for target in snapshot.iter() {
        for hitbox in &target.hitboxes {
            visit(target.index, hitbox);
        }
    }
}

/// Сколько целей в снимке (для диагностики аима):
/// видно из `CL_CreateMove`, когда рабочий список уже пуст.
pub(crate) fn snapshot_count() -> usize {
    SNAPSHOT.lock().map_or(0, |s| s.len())
}

/// Номер кадра, для которого матрица проекции уже снята: `glGet*`-запрос на
/// каждого игрока — лишняя работа, за кадр достаточно одного снимка.
static NOTED_SEQ: AtomicU64 = AtomicU64::new(u64::MAX);

/// Запоминает матрицу проекции текущего кадра.
///
/// Зовётся из перехвата отрисовки игрока — рендер-поток с живым GL-контекстом.
/// Матрица одна на весь кадр и меняется только при смене FOV/соотношения
/// сторон, поэтому первый вызов кадра делает снимок, а остальные по номеру
/// кадра пропускаются.
pub(crate) fn note_projection(frame_seq: u64) {
    if !INSTALLED.load(Ordering::Acquire) {
        return;
    }
    if NOTED_SEQ.load(Ordering::Acquire) == frame_seq {
        return;
    }
    // SAFETY: вызов идёт из рендер-потока игры, где контекст активен.
    let Some(gl) = (unsafe { Gl::get() }) else {
        return;
    };
    // SAFETY: тот же контракт — активный контекст рендер-потока.
    let Some(matrix) = (unsafe { gl.projection_matrix() }) else {
        return;
    };
    NOTED_SEQ.store(frame_seq, Ordering::Release);
    if let Ok(mut slot) = PROJECTION.lock() {
        *slot = Some(matrix);
    }
}

/// Решение «стрелять или нет»: конец перерисовки, буфер глубины ещё движковый
/// (обводка глубину не пишет), кости целей уже сняты.
pub(crate) fn decide() {
    if !INSTALLED.load(Ordering::Acquire) {
        return;
    }
    if !ENABLED.load(Ordering::Acquire) || !EngineState::playable() {
        // Причина паузы — в лог: те же поля, что в аиме, чтобы один прогон
        // сразу показал, какой гейт закрыт.
        if ENABLED.load(Ordering::Acquire) && !EngineState::playable() {
            status(&format!(
                "не играем: время={:.1}, меню={}, чат={}, здоровье={}",
                EngineState::time(),
                EngineState::in_menu(),
                EngineState::chat_opened(),
                EngineState::health()
            ));
        }
        ceasefire();
        return;
    }
    // Кнопка активации: не зажата — не стреляем, счётчик «под прицелом»
    // при этом не плывёт: отпустил и снова зажал — отсчёт продолжается.
    let activation = ACTIVATION_VK.load(Ordering::Relaxed);
    if activation != 0 && !crate::input::held_vk(activation) {
        ceasefire();
        return;
    }
    let origin = EngineState::view_origin();
    let forward = EngineState::forward();
    if !forward.is_finite() || forward.length() < 0.5 {
        ceasefire();
        return;
    }
    let Some((index, group, distance)) = best_hit(&origin, &forward) else {
        // Цели есть, но луч мимо: жертвы на экране, стрелять не по кому.
        if target_count() > 0 {
            status("прицел мимо");
        }
        ceasefire();
        return;
    };
    let aim = origin + forward * distance;
    if !visible(aim) {
        status(&format!("точка прицела закрыта: игрок #{index}"));
        ceasefire();
        return;
    }

    // «Реакция»: отсчёт с первого кадра, как цель оказалась под прицелом;
    // смена цели — заново, переход между хитбоксами того же игрока — нет.
    let delay = Duration::from_millis(DELAY_MS.load(Ordering::Relaxed) as u64);
    let since = match ACQUIRED.lock() {
        Ok(mut acquired) => match *acquired {
            Some((at, held_index)) if held_index == index => at,
            _ => {
                let now = Instant::now();
                *acquired = Some((now, index));
                now
            }
        },
        Err(_) => return,
    };
    let waited = since.elapsed();
    if waited < delay {
        status(&format!(
            "реакция {}/{} мс: игрок #{index}, {}",
            waited.as_millis(),
            delay.as_millis(),
            group.name()
        ));
        return;
    }
    fire(index, group, distance);
}

/// Ближайший хитбокс под прицельным лучом среди целей кадра.
fn best_hit(origin: &Vec3, forward: &Vec3) -> Option<(i32, HitGroup, f32)> {
    let range = MAX_RANGE.load(Ordering::Relaxed) as f32;
    let head_only = HEAD_ONLY.load(Ordering::Relaxed);
    let targets = TARGETS.lock().ok()?;
    let mut best: Option<(i32, HitGroup, f32)> = None;
    for target in targets.iter() {
        for hitbox in &target.hitboxes {
            if head_only && hitbox.group != HitGroup::Head {
                continue;
            }
            let Some(distance) = hitbox.ray(*origin, *forward) else {
                continue;
            };
            if distance > range {
                continue;
            }
            if best.is_none_or(|(_, _, nearest)| distance < nearest) {
                best = Some((target.index, hitbox.group, distance));
            }
        }
    }
    best
}

/// Видна ли точка тела под перекрестием: сравнение с буфером глубины.
///
/// Точка прицельного луча переводится в оконную глубину той же матрицей
/// проекции, по которой движок рисовал кадр; буфер глубины под перекрестием
/// держит ближайшую поверхность мира или тела. Точка видима, если она не
/// дальше записанной глубины — то же сравнение (`GL_LEQUAL`), которым обводка
/// ESP красит видимую часть контура.
fn visible(point: Vec3) -> bool {
    let Some(projection) = PROJECTION.lock().ok().and_then(|slot| *slot) else {
        return false;
    };
    // SAFETY: вызов идёт из рендер-потока игры, контекст активен.
    let Some(gl) = (unsafe { Gl::get() }) else {
        return false;
    };
    // SAFETY: тот же контракт.
    let Some((vx, vy, vw, vh)) = (unsafe { gl.viewport() }) else {
        return false;
    };
    // Точка в системе камеры: взгляд вдоль -Z, как в GL.
    let rel = point - EngineState::view_origin();
    let eye = Vec3::new(
        rel.dot(EngineState::right()),
        rel.dot(EngineState::up()),
        -rel.dot(EngineState::forward()),
    );
    let Some(depth_of_point) = clip_depth(projection, eye) else {
        return false;
    };
    let window = depth_of_point * 0.5 + 0.5;
    // Прицел — центр вьюпорта; пиксель в GL считается от нижнего угла.
    let cx = (vx + vw * 0.5).floor().clamp(vx, vx + vw - 1.0) as i32;
    let cy = (vy + vh * 0.5).floor().clamp(vy, vy + vh - 1.0) as i32;
    // SAFETY: тот же контракт; один пиксель за кадр.
    let Some(buffered) = (unsafe { gl.read_depth(cx, cy) }) else {
        return false;
    };
    window <= buffered + DEPTH_EPSILON
}

/// Глубина точки в окне: `0` — ближняя плоскость, `1` — дальняя.
///
/// `eye` — точка в системе координат камеры (взгляд вдоль `-Z`). Матрица GL
/// хранится колонками: `projection[col * 4 + row]`. Возвращается дробное
/// `clip.z / clip.w` (нормализованная глубина), `None` — точка за камерой.
fn clip_depth(projection: [f32; 16], eye: Vec3) -> Option<f32> {
    let clip_z =
        projection[2] * eye.x + projection[6] * eye.y + projection[10] * eye.z + projection[14];
    let clip_w =
        projection[3] * eye.x + projection[7] * eye.y + projection[11] * eye.z + projection[15];
    if !clip_z.is_finite() || clip_w <= f32::EPSILON {
        return None;
    }
    Some(clip_z / clip_w)
}

/// Зажимает кнопку огня: [`create_move`] добавит `IN_ATTACK` в команды
/// игрока, пока она зажата.
fn fire(index: i32, group: HitGroup, distance: f32) {
    if WANT_FIRE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    FIRED.fetch_add(1, Ordering::Relaxed);
    let fresh = match LAST_LOG.lock() {
        Ok(mut last) => match *last {
            Some(at) if at.elapsed() < LOG_THROTTLE => false,
            _ => {
                *last = Some(Instant::now());
                true
            }
        },
        Err(_) => true,
    };
    if fresh {
        log!(
            "триггербот: огонь: игрок #{index}, {}, {:.0} юнитов",
            group.name(),
            distance
        );
    }
}

/// Отпускает кнопку и сбрасывает «реакцию». Парный к [`fire`]; безопасен и без
/// зажатой кнопки.
fn ceasefire() {
    if let Ok(mut acquired) = ACQUIRED.lock() {
        *acquired = None;
    }
    WANT_FIRE.store(false, Ordering::Release);
}

/// Видел ли перехват `CL_CreateMove` хоть один вызов: для одноразового
/// подтверждения в логе, что движок ходит через нашу подмену.
static SEEN_CREATE_MOVE: AtomicBool = AtomicBool::new(false);

/// Наш `CL_CreateMove`: движок зовёт его вместо клиентской функции, адрес
/// которой в слоте [`hw::P_CREATE_MOVE`] подменён (см. [`install_hook`]).
///
/// Сначала оригинал собирает команду — кнопки игрока в том числе, — и уже
/// собранной команде добавляется `IN_ATTACK`, если решение кадра зажало
/// огонь. Бит только добавляется: своя кнопка игрока не затирается, а после
/// выгрузки хука бита в командах быть не может — кнопка не «залипает».
///
/// # Safety
/// Сигнатура и ABI (`cdecl`) совпадают с клиентской функцией: движок зовёт
/// её через указатель и сам чистит стек.
unsafe extern "C" fn create_move(frametime: f32, cmd: *mut c_void, active: i32) {
    let _inside = InFlight::enter();
    let original = ORIGINAL.load(Ordering::Acquire);
    if original == 0 || cmd.is_null() {
        return;
    }
    // SAFETY: адрес из слота — клиентский `CL_CreateMove`, сигнатура та же.
    let original: unsafe extern "C" fn(f32, *mut c_void, i32) =
        unsafe { core::mem::transmute(original) };
    unsafe { original(frametime, cmd, active) };
    if !SEEN_CREATE_MOVE.swap(true, Ordering::AcqRel) {
        log!("триггербот: перехват CL_CreateMove работает (active={active})");
    }
    if active == 0 || !enabled() || !WANT_FIRE.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: `cmd` — живая команда, собранная оригиналом; поле `buttons`
    // (`u16` на `usercmd::BUTTONS`) лежит внутри записи `cl.cmds`
    // (`usercmd::SIZE`), см. тест `buttons_field_fits_command_record`.
    let buttons = unsafe { cmd.byte_add(usercmd::BUTTONS) } as *mut u16;
    let held = unsafe { buttons.read_unaligned() };
    unsafe { buttons.write_unaligned(held | usercmd::IN_ATTACK) };
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
                    "в pfnCreateMove не клиентский код и не перехват нашего модуля: {previous:#x}"
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
/// либо код `client.dll` (нетронутый движковый путь), либо чужой перехват из
/// нашего модуля (например [`crate::aim`] в этой же DLL) — цепочку
/// продолжаем, реестр разберёт её при выгрузке в обратном порядке.
fn callable_create_move(previous: usize, client: usize) -> bool {
    if process::is_code(previous, client) {
        return true;
    }
    Module::current().is_some_and(|own| own.contains(previous))
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
    use super::clip_depth;
    use golden_sdk::math::Vec3;
    use golden_sdk::offsets::usercmd;

    /// `gluPerspective(fov_y, aspect, near, far)`, column-major `[f32; 16]`.
    fn perspective(fov_y_degrees: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
        let f = 1.0 / (fov_y_degrees.to_radians() * 0.5).tan();
        let mut m = [0.0f32; 16];
        m[0] = f / aspect;
        m[5] = f;
        m[10] = (far + near) / (near - far);
        m[11] = -1.0;
        m[14] = 2.0 * far * near / (near - far);
        m
    }

    #[test]
    fn clip_depth_matches_glu_perspective() {
        let matrix = perspective(90.0, 1.0, 4.0, 4096.0);
        // Нормализованная глубина: near -> -1, far -> +1.
        let near = clip_depth(matrix, Vec3::new(0.0, 0.0, -4.0)).unwrap();
        let far = clip_depth(matrix, Vec3::new(0.0, 0.0, -4096.0)).unwrap();
        let mid = clip_depth(matrix, Vec3::new(0.0, 0.0, -8.0)).unwrap();
        assert!((near + 1.0).abs() < 1e-6, "near: {near}");
        assert!((far - 1.0).abs() < 1e-6, "far: {far}");
        // Глубина монотонна по дистанции.
        assert!(near < mid && mid < far);
        // За камерой глубины нет.
        assert!(clip_depth(matrix, Vec3::new(0.0, 0.0, 4.0)).is_none());
    }

    /// Перехват `create_move` пишет `u16` по `cmd + usercmd::BUTTONS`:
    /// поле обязано помещаться в запись команды `usercmd::SIZE`, иначе
    /// движок получит бит кнопки за границей команды.
    #[test]
    fn buttons_field_fits_command_record() {
        assert!(
            usercmd::BUTTONS + std::mem::size_of::<u16>() <= usercmd::SIZE,
            "buttons на {:#x} не влезает в команду {:#x}",
            usercmd::BUTTONS,
            usercmd::SIZE
        );
        assert_ne!(usercmd::IN_ATTACK, 0, "пустой бит огня ничего не нажмёт");
    }
}
