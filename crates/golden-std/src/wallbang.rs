//! Wallbang: сколько урона дойдёт до игрока под прицелом через стены.
//!
//! Механика — в SDK: [`golden_sdk::bsp`] трассирует луч по геометрии карты,
//! [`golden_sdk::ballistics`] считает пробитие и урон так же, как
//! `CBaseEntity::FireBullets3` из `mp.dll`. Здесь — привязка к живой игре:
//! где взять карту, кто стоит под прицелом, что нарисовать у прицела.
//!
//! ```no_run
//! # fn example() -> Result<(), String> {
//! golden_std::wallbang::Wallbang::new().install()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Что считается
//!
//! * луч идёт из глаза игрока по направлению взгляда (`Game::get_view`);
//! * цель — первый живой игрок, в чью модель попадает луч: хитбоксы снимаются
//!   в перехвате отрисовки по матрицам костей той позы, которой движок нарисовал
//!   тело (как в ESP), и перебираются так же, как в `SV_HullForStudioModel`;
//!   до цели пуля должна дойти раньше, чем во что-то упрётся;
//! * урон — тот, что получит корпус цели: без брони, без зон попадания и без
//!   разброса (разброс у каждой пули свой, для оценки он не нужен);
//! * стены — поверхности мира и brush-моделей, через которые пуля уже прошла.
//!
//! ## Границы
//!
//! Разброс, броня, зоны попадания и `sv_penetration_type` не учитываются:
//! здесь ответ на вопрос «пробьёт ли и сколько останется», а не точный расчёт
//! урона конкретной пули. Игра считает то же самое своим `FireBullets3`.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use golden_sdk::ballistics::{self, Hitgroup, Material, Materials, Ray, Surface, Weapon};
use golden_sdk::bsp::{Bsp, Trace};
use golden_sdk::draw::Color;
use golden_sdk::engine::EngineStudioApi;
use golden_sdk::log;
use golden_sdk::math::Vec3;
use golden_sdk::module::InFlight;
use golden_sdk::renderer::{
    interface_slot, StudioInterface, StudioRenderer, SLOT_RENDER_MODEL,
};
use golden_sdk::studio::{BoneMatrix, HitGroup, WorldHitbox};

use crate::game::{self, Game, Player};
use crate::hooks;
use crate::render::{Overlay, DRAW_NORMAL_TRIANGLES_RVA};

/// Полувысота стоящего игрока: в GoldSrc `origin` — середина корпуса
/// (`VEC_HULL_MIN/MAX` = ±36 по высоте), а не ступни.
const BODY_HALF: f32 = 36.0;

/// Где искать цель по высоте в координатах `origin`: от ступней до макушки.
const BODY: core::ops::RangeInclusive<f32> = -BODY_HALF..=BODY_HALF;

/// Радиус, в котором луч считается попавшим в корпус.
const BODY_RADIUS: f32 = 14.0;

/// Сколько точек корпуса проверять (лучше больше — не упустить наклон).
const BODY_SAMPLES: u32 = 12;

/// Масштаб подписи у прицела.
const TEXT_SCALE: f32 = 3.0;

/// Отступ подписи от центра экрана: справа-снизу, чтобы не закрывать прицел.
const TEXT_OFFSET: (f32, f32) = (18.0, 52.0);

/// Как часто повторять неудачную загрузку карты.
const RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// Как часто проверять, не сменилась ли карта.
const RECHECK: std::time::Duration = std::time::Duration::from_secs(1);

/// Игрок под прицелом и что с ним будет.
#[derive(Debug, Clone, PartialEq)]
pub struct Aim {
    /// Номер сущности цели.
    pub player: i32,
    /// Ник цели.
    pub name: String,
    /// Оружие в руках, как его зовёт игра (`AK-47`).
    pub weapon: Option<String>,
    /// Урон по корпусу, если пуля дошла. `None` — не пробьёт.
    pub damage: Option<i32>,
    /// Зона попадания и её множитель; `None` — пуля не дошла.
    pub hitgroup: Option<Hitgroup>,
    /// Сколько стен пуля пробьёт до цели.
    pub walls: u32,
    /// Поверхности на пути пули: расстояние, материал, начало внутри.
    pub surfaces: Vec<Wall>,
    /// Урон после каждой стены, ближняя первой.
    pub after_walls: Vec<i32>,
    /// Расстояние от глаза стрелка до цели.
    pub distance: f32,
    /// Сколько HP останется у цели, если урон дойдёт.
    pub health_left: Option<i32>,
}

impl Aim {
    /// Подпись для подписи у прицела.
    pub fn text(&self) -> String {
        let weapon = self.weapon.as_deref().unwrap_or("без оружия");
        let walls = match self.walls {
            0 => String::from("прямая видимость"),
            1 => String::from("через 1 стену"),
            walls => format!("через {walls} стены"),
        };
        match self.damage {
            Some(damage) => {
                let zone = match self.hitgroup {
                    Some(hitgroup) => format!(" {}", hitgroup.in_zone()),
                    None => String::new(),
                };
                let left = match self.health_left {
                    Some(0) => String::from(", убьёт"),
                    Some(left) => format!(", останется {left}"),
                    None => String::new(),
                };
                format!("{weapon}: {walls}, урон {damage}{zone}{left}")
            }
            None => format!("{weapon}: {walls}, не пробьёт"),
        }
    }
}

/// Последняя попытка разобрать карту: имя, результат и время.
#[derive(Debug)]
struct Cache {
    name: String,
    map: Option<Arc<Map>>,
    attempted: std::time::Instant,
    checked: std::time::Instant,
}

/// Карта и материалы: всё, что нужно, чтобы считать выстрел.
#[derive(Debug)]
pub struct Map {
    bsp: Bsp,
    materials: Materials,
    name: String,
}

impl Map {
    /// Загружает карту, на которой идёт игра.
    ///
    /// Путь карты берётся у мира (`model_t.name` — `maps/de_dust2.bsp`),
    /// каталог игры — из `GOLDCLIENT_DIR` или стандартной установки. Внутри
    /// каталога мод ищется по наличию файла карты, поэтому `cstrike` и его
    /// копии (например `cstrike_russian`) равно годятся.
    pub fn load() -> Result<Self, String> {
        let game = Game::get().ok_or("движок не отдал studio-интерфейс")?;
        let model = world_model(&game).ok_or("мир без модели карты")?;
        let map_name = map_stem(&model).ok_or_else(|| format!("непонятное имя модели мира: {model}"))?;
        let dir = game_dir().ok_or("каталог игры не найден (задайте GOLDCLIENT_DIR)")?;
        let path = locate_map(&dir, &model)
            .ok_or_else(|| format!("нет файла {model} в каталоге {}", dir.display()))?;
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("не прочитать {}: {error}", path.display()))?;
        let bsp = Bsp::parse(&bytes).map_err(|error| format!("разбор {}: {error}", path.display()))?;
        let materials = load_materials(path.parent().and_then(Path::parent));
        log!(
            "wallbang: карта {map_name} ({} КБ, {} brush), материалов {}, текстур {}",
            bytes.len() / 1024,
            bsp.brushes().len(),
            materials.len(),
            bsp.textures().len()
        );
        Ok(Self {
            bsp,
            materials,
            name: map_name,
        })
    }

    /// Имя карты без пути и расширения (`de_dust2`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Таблица материалов карты.
    pub fn materials(&self) -> &Materials {
        &self.materials
    }

    /// Геометрия карты.
    pub fn bsp(&self) -> &Bsp {
        &self.bsp
    }

    /// Текущая карта: разбор кешируется и пересобирается при смене карты.
    ///
    /// Возвращается `Arc`: кадр держит карту, пока считает выстрел, и смена
    /// карты не освобождает её под ногами. Неудачная попытка повторяется раз в
    /// [`RETRY`], а причина пишется в лог один раз — иначе кадр залил бы лог.
    pub fn current() -> Option<Arc<Self>> {
        static CACHE: OnceLock<Mutex<Option<Cache>>> = OnceLock::new();
        let game = Game::get()?;
        let cache = CACHE.get_or_init(|| Mutex::new(None));
        let mut slot = cache.lock().ok()?;
        if let Some(entry) = slot.as_mut() {
            // Имя карты стоит чтения памяти и `VirtualQuery` на каждый байт:
            // проверяем смену карты раз в секунду, а не каждый кадр.
            if entry.checked.elapsed() < RECHECK {
                return entry.map.clone();
            }
            entry.checked = std::time::Instant::now();
            match world_model(&game).and_then(|model| map_stem(&model)) {
                Some(name) if name != entry.name => {
                    if entry.attempted.elapsed() < RETRY {
                        return None;
                    }
                }
                _ => return entry.map.clone(),
            }
        }
        let name = world_model(&game)
            .and_then(|model| map_stem(&model))
            .unwrap_or_default();
        let loaded = match Map::load() {
            Ok(map) => Some(Arc::new(map)),
            Err(reason) => {
                log!("wallbang: карта не загрузилась: {reason}");
                None
            }
        };
        *slot = Some(Cache {
            name,
            map: loaded.clone(),
            attempted: std::time::Instant::now(),
            checked: std::time::Instant::now(),
        });
        loaded
    }

    /// Материал поверхности, в которую попал луч.
    pub fn material_at(&self, trace: &Trace) -> Material {
        let name = match self.bsp.brush_of_model(trace.model) {
            Some(brush) => self.bsp.texture_at_brush(brush, trace.end, trace.normal),
            None => self.bsp.texture_at(trace.end, trace.normal),
        };
        name.map_or(Material::Concrete, |name| self.materials.classify(name))
    }

    /// Считает выстрел по игроку под прицелом.
    ///
    /// `None` — под прицелом никого: цель ищется по корпусу в радиусе
    /// [`BODY_RADIUS`], поэтому промах мимо игрока остаётся промахом.
    pub fn aim(&self, weapon: Option<&'static Weapon>) -> Option<Aim> {
        self.aim_with(weapon, take_hit())
    }

    /// Считает выстрел по попаданию, снятому в проходе отрисовки.
    ///
    /// Зона приходит из перехвата отрисовки: там поза игрока настоящая.
    /// Пересчёт по сохранённым хитбоксам давал зону прошлого кадра — на бегущего
    /// игрока это промах в целый бокс.
    pub fn aim_with(&self, weapon: Option<&'static Weapon>, hit: Option<FreshHit>) -> Option<Aim> {
        let view = Game::get()?.get_view()?;
        if view.forward.length() < 0.5 {
            return None;
        }
        let direction = view.forward * (1.0 / view.forward.length());
        match hit {
            Some(hit) => Some(self.shot_at(
                view.origin,
                direction,
                hit.index,
                hit.point,
                hit.hitgroup,
                weapon,
            )),
            None => self.aim_at(view.origin, direction, weapon),
        }
    }

    /// Считает выстрел из `origin` по `direction` (единичному).
    ///
    /// Так же считает и `wallbang_probe`: он наводит луч на каждого игрока,
    /// чтобы показать работу без ручного прицеливания.
    pub fn aim_at(
        &self,
        origin: Vec3,
        direction: Vec3,
        weapon: Option<&'static Weapon>,
    ) -> Option<Aim> {
        let Some((target, point, hitgroup)) = self.target(origin, direction) else {
            return None;
        };
        Some(self.shot_at(origin, direction, target, point, hitgroup, weapon))
    }

    /// Считает выстрел по уже известной цели и зоне.
    fn shot_at(
        &self,
        origin: Vec3,
        direction: Vec3,
        target: i32,
        point: Vec3,
        hitgroup: Hitgroup,
        weapon: Option<&'static Weapon>,
    ) -> Aim {
        let distance = origin.distance_to(point);
        let name = target_name(target);
        let Some(weapon) = weapon else {
            return Aim {
                player: target,
                name,
                weapon: None,
                damage: None,
                hitgroup: Some(hitgroup),
                walls: 0,
                surfaces: Vec::new(),
                after_walls: Vec::new(),
                distance,
                health_left: None,
            };
        };
        let mut tracer = AimTracer {
            map: self,
            target: Some((target, point)),
            walls: Vec::new(),
        };
        // Зона попадания уже известна из выбора точки на теле: уточнять её по
        // точке удара луча нельзя, луч идёт от глаза и попадает чуть иначе.
        let shot =
            ballistics::simulate_with(weapon, origin, direction, &mut tracer, |_, _| hitgroup);
        let damage = shot.damage.map(|damage| damage.damage);
        let hitgroup = shot.damage.map(|damage| damage.hitgroup);
        // Здоровье движок отдаёт не всегда (`-1`): тогда «сколько останется» —
        // не ответ, а выдумка, и показывать его нельзя.
        let health_left = damage.and_then(|damage| {
            let health = target_health(target);
            (health > 0).then(|| (health - damage).max(0))
        });
        Aim {
            player: target,
            name,
            weapon: Some(weapon.display.to_string()),
            damage,
            hitgroup,
            walls: shot.walls,
            surfaces: tracer.walls,
            after_walls: shot.after_walls[..shot.walls.min(8) as usize].to_vec(),
            distance,
            health_left,
        }
    }

    /// Первый игрок на луче: номер слота, точка попадания и зона.
    ///
    /// Позиции и хитбоксы берутся из перехвата отрисовки: движок выставляет
    /// «текущую сущность» (`hw+0x78E470`) и матрицы костей только пока рисует
    /// модель. Сетевой массив (`GetEntityState`) в Gold Client для этого не
    /// годится: его записи идут с шагом `0x280` и позиций игроков не содержат.
    fn target(&self, origin: Vec3, direction: Vec3) -> Option<(i32, Vec3, Hitgroup)> {
        let mut best: Option<(f32, i32, Hitgroup)> = None;
        for drawn in drawn_players() {
            // Попадание считается по хитбоксам модели — по той позе, которой
            // движок нарисовал тело: их снимает перехват отрисовки, как в ESP.
            // Перебор тот же, что в `SV_HullForStudioModel`: побеждает ближайший
            // к глазу бокс, а его группа и есть зона попадания. Если боксов нет
            // (поза не прочиталась), цель не показывается.
            let Some((distance, zone)) = zone_hit(&drawn.hitboxes, origin, direction) else {
                continue;
            };
            if best.is_none_or(|(best_distance, _, _)| distance < best_distance) {
                best = Some((distance, drawn.index, zone));
            }
        }
        let (distance, index, zone) = best?;
        Some((index, origin + direction * distance, zone))
    }
}

/// Зона попадания по группе хитбокса модели.
fn zone_of(group: HitGroup) -> Hitgroup {
    match group {
        HitGroup::Head => Hitgroup::Head,
        HitGroup::Stomach => Hitgroup::Stomach,
        HitGroup::LeftArm | HitGroup::RightArm => Hitgroup::Arm,
        HitGroup::LeftLeg | HitGroup::RightLeg => Hitgroup::Leg,
        _ => Hitgroup::Chest,
    }
}

/// Попадание по хитбоксам: ближайший бокс, как в движке.
///
/// `SV_HullForStudioModel` из `hw.dll` перебирает хитбоксы по одному
/// (смещение `hitboxindex`, шаг `0x20`), переводит `bbmin`/`bbmax` матрицей
/// кости и трассирует каждый бокс отдельно; побеждает тот, у которого доля
/// пути меньше, а группа берётся из поля `group` этого бокса
/// (`(&DAT_12987a00)[индекс] = hitbox->group`).
///
/// Ещё движок пропускает один последний бокс: при флаге он не обходит хитбокс
/// с индексом `0x14` (`SV_HullForStudioModel` уменьшает и число боксов).
/// В player-моделях это бокс с группой `8` — «gear» (оружие в руке): он
/// огромный и перекрывает всё тело, поэтому в трассировке не участвует.
fn zone_hit(hitboxes: &[WorldHitbox], origin: Vec3, direction: Vec3) -> Option<(f32, Hitgroup)> {
    hitboxes
        .iter()
        .filter(|hitbox| !matches!(hitbox.group, HitGroup::Other(_)))
        .filter_map(|hitbox| {
            hitbox
                .ray(origin, direction)
                .map(|distance| (distance, zone_of(hitbox.group)))
        })
        .fold(None, |best: Option<(f32, Hitgroup)>, hit| match best {
            Some(previous) if previous.0 <= hit.0 => Some(previous),
            _ => Some(hit),
        })
}

/// Попадание в игрока, посчитанное в момент его отрисовки.
///
/// Матрицы костей живут только внутри отрисовки модели, поэтому и зона, и точка
/// попадания считаются прямо там, где поза настоящая. Кадр, в котором это было,
/// хранится рядом: поза прошлого кадра — уже не та.
#[derive(Clone, Copy, Debug)]
pub struct FreshHit {
    pass: u64,
    /// Номер слота игрока.
    pub index: i32,
    /// Точка попадания луча в хитбокс.
    pub point: Vec3,
    /// Зона попадания.
    pub hitgroup: Hitgroup,
    distance: f32,
}

/// Лучшее попадание текущего прохода отрисовки.
static FRESH: Mutex<Option<FreshHit>> = Mutex::new(None);

/// Номер прохода отрисовки: попадания помечаются им, а кадр забирает свои.
static PASS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Запоминает попадание, если оно ближе уже найденного в этом проходе.
fn remember_hit(hit: FreshHit) {
    let Ok(mut slot) = FRESH.lock() else {
        return;
    };
    match *slot {
        Some(previous) if previous.pass == hit.pass && previous.distance <= hit.distance => {}
        _ => *slot = Some(hit),
    }
}

/// Забирает попадание прохода отрисовки; чужой проход не отдаёт.
fn take_hit() -> Option<FreshHit> {
    FRESH.lock().ok().and_then(|mut slot| slot.take())
}

/// Последнее снятое попадание и его проход: подписи нельзя мигать, если
/// проход отрисовки в этом кадре не успел записать своё.
static LAST_HIT: Mutex<Option<FreshHit>> = Mutex::new(None);

/// Сколько проходов подряд допустимо показывать попадание прошлого кадра.
const HIT_TTL: u64 = 3;

/// Попадание для подписи: свежее, а если его нет — недавнее.
fn hit_for_label(pass: u64) -> Option<FreshHit> {
    match FRESH.lock().ok().and_then(|mut slot| slot.take()) {
        Some(hit) => {
            if let Ok(mut last) = LAST_HIT.lock() {
                *last = Some(hit);
            }
            Some(hit)
        }
        None => LAST_HIT
            .lock()
            .ok()
            .and_then(|last| *last)
            .filter(|hit| pass - hit.pass <= HIT_TTL),
    }
}

/// Начинает новый проход отрисовки: попадания прошлого больше не нужны.
fn next_pass() {
    PASS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Ник игрока по номеру слота.
fn target_name(index: i32) -> String {
    Player::by_index(index).map_or_else(|| format!("слот {index}"), |player| player.name())
}

/// Имя модели игрока так, как его видит движок: по нему видно, чьи кости
/// попали в массив при отрисовке.
fn model_name(index: i32) -> String {
    Player::by_index(index).map_or_else(|| "?".to_string(), |player| player.model_name())
}

/// Здоровье цели; `-1` — движок его не отдал.
fn target_health(index: i32) -> i32 {
    Player::by_index(index).map_or(-1, |player| player.health())
}

/// Игрок, которого движок нарисовал в этом кадре.
#[derive(Clone, Debug)]
struct Drawn {
    index: i32,
    /// Середина корпуса (`origin` сущности).
    origin: Vec3,
    /// Хитбоксы модели в мировых координатах: сняты в перехвате отрисовки по
    /// той позе, которой движок нарисовал тело. По ним и считается попадание.
    hitboxes: Vec<WorldHitbox>,
    /// Габариты модели из заголовка (bind-поза) — «бокс модели в рендере».
    hull: Option<(Vec3, Vec3)>,
    /// Голова так, как её видит движок (`head_bounds`): с чем сверять хитбокс.
    engine_head: Option<Vec3>,
    /// Разброс позиций костей по высоте: анимирована ли поза.
    bone_span: f32,
    /// Начало и конец каждой кости по высоте — для разбора позы.
    bone_z: Vec<f32>,
}

/// Кого движок нарисовал с прошлого конца кадра.
static DRAWN: Mutex<Vec<Drawn>> = Mutex::new(Vec::new());

/// Игроки, нарисованные в этом кадре.
fn drawn_players() -> Vec<Drawn> {
    DRAWN.lock().map(|slot| slot.clone()).unwrap_or_default()
}

/// Модель в руках стрелка: её движок рисует отдельной моделью `models/v_*`.
static VIEW_MODEL: Mutex<Option<String>> = Mutex::new(None);

/// Оружие в руках, как его назвал движок (`v_ak47.mdl`).
pub fn view_model() -> Option<String> {
    VIEW_MODEL.lock().ok().and_then(|slot| slot.clone())
}

/// Номер слота игрока из аргумента `StudioDrawPlayer`.
fn player_index_of(player_info: *mut c_void) -> Option<i32> {
    if player_info.is_null() {
        return None;
    }
    // SAFETY: `player_info_s` движка; номер слота — документированное поле +4.
    let index: i32 =
        unsafe { golden_sdk::process::read(player_info as usize + golden_sdk::offsets::player_info::INDEX) };
    (0..64).contains(&index).then_some(index)
}

/// Запоминает игрока, которого движок рисует прямо сейчас.
fn remember_drawn(player_info: *mut c_void) {
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let Some(entity) = renderer.current_entity() else {
        return;
    };
    let index = player_index_of(player_info).unwrap_or_else(|| entity.player_index());
    // Модель оружия из `player_info` — та же, что движок берёт в руки.
    if let Some(weapon) = golden_sdk::engine::player_weapon_model(player_info) {
        game::remember_weapon(index, weapon);
    }
    if !(0..64).contains(&index) {
        return;
    }
    let origin = entity.origin();
    let view = Game::get().and_then(|game| game.get_view());
    // Своя модель — вид от первого лица и модель в руках — рисуется рядом с
    // камерой. Её хитбоксы не должны становиться целью: иначе мод «видит»
    // собственное оружие.
    if let Some(view) = &view {
        if (origin - view.origin).length() < 48.0 {
            return;
        }
    }
    // Корпус `±16/±36` от `origin` — только запасной путь: он постоянный, ничего
    // не знает про позу, и по нему «тело» оказывается там, где пули не пройдут.
    // Мировые хитбоксы считаются в перехвате отрисовки модели тела: только там
    // матрицы костей принадлежат этому игроку (в конце `StudioDrawPlayer` в
    // буфере уже лежат кости оружия). Здесь они берутся готовыми.
    let previous = DRAWN
        .lock()
        .ok()
        .and_then(|slot| slot.iter().find(|drawn| drawn.index == index).cloned());
    let model_boxes = previous
        .as_ref()
        .map(|drawn| drawn.hitboxes.clone())
        .unwrap_or_default();
    // Попадание считается только по хитбоксам модели: это та поза, которую
    // видно на экране. Поза не прочиталась (труп, чужая модель, устаревший
    // буфер) — цели нет: лучше промолчать, чем показать корпус, где пули не
    // пройдут.
    let hitboxes = if plausible_body(origin, &model_boxes) {
        model_boxes
    } else {
        Vec::new()
    };
    let hull = previous.as_ref().and_then(|drawn| drawn.hull);
    let engine_head = previous.as_ref().and_then(|drawn| drawn.engine_head);
    let bone_span = previous.as_ref().map(|drawn| drawn.bone_span).unwrap_or(0.0);
    let bone_z = previous
        .as_ref()
        .map(|drawn| drawn.bone_z.clone())
        .unwrap_or_default();
    // Зона считается здесь: игрок уже нарисован, его `origin` этого кадра.
    if let Some(view) = &view {
        if view.forward.length() >= 0.5 {
            let direction = view.forward * (1.0 / view.forward.length());
            if let Some((distance, hitgroup)) = zone_hit(&hitboxes, view.origin, direction) {
                let point = view.origin + direction * distance;
                remember_hit(FreshHit {
                    pass: PASS.load(std::sync::atomic::Ordering::Relaxed),
                    index,
                    point,
                    hitgroup,
                    distance,
                });
            }
        }
    }
    if let Ok(mut slot) = DRAWN.lock() {
        match slot.iter_mut().find(|drawn| drawn.index == index) {
            Some(drawn) => {
                drawn.origin = origin;
                drawn.hitboxes = hitboxes;
                drawn.hull = hull;
                drawn.engine_head = engine_head;
                drawn.bone_span = bone_span;
                drawn.bone_z = bone_z;
            }
            None => slot.push(Drawn {
                index,
                origin,
                hitboxes,
                hull,
                engine_head,
                bone_span,
                bone_z,
            }),
        }
    }
}

/// Полувысота корпуса игрока: `VEC_HULL_MAX.z` из `hw.dll`.
const HULL_HALF: f32 = 36.0;

/// Полуширина корпуса игрока: `VEC_HULL_MAX.x/y` из `hw.dll`.
const HULL_SIDE: f32 = 16.0;

/// Боксы игрока — ровно корпус, по которому работает трассировка движка.
///
/// `VEC_HULL_MIN/MAX` лежат в `hw.dll` по `0x10272800`/`0x102727d0`: `±16` по
/// горизонтали и `±36` по высоте от `origin` (он же середина корпуса). Именно
/// по этому корпусу клиент показывает надпись «Противник», и присед его не
/// уменьшает. Внутри корпуса зона делится по высоте, причём граница головы
/// берётся из головы модели, чтобы прицел в голову и назывался головой.
fn hull_boxes(origin: Vec3, head_from: f32) -> Vec<WorldHitbox> {
    let bottom = origin.z - HULL_HALF;
    let top = origin.z + HULL_HALF;
    let head_from = head_from.clamp(bottom + 20.0, top - 2.0);
    let chest_from = bottom + (head_from - bottom) * 0.55;
    let matrix = BoneMatrix::identity();
    let zones = [
        (bottom, chest_from, HitGroup::LeftLeg),
        (chest_from, head_from, HitGroup::Chest),
        (head_from, top, HitGroup::Head),
    ];
    zones
        .into_iter()
        .filter(|(low, high, _)| high > low)
        .map(|(low, high, group)| {
            let min = Vec3::new(origin.x - HULL_SIDE, origin.y - HULL_SIDE, low);
            let max = Vec3::new(origin.x + HULL_SIDE, origin.y + HULL_SIDE, high);
            WorldHitbox {
                group,
                bone: 0,
                min,
                max,
                matrix,
                local_min: min,
                local_max: max,
            }
        })
        .collect()
}

/// Нижняя граница головы по хитбоксу модели, если он правдоподобен.
fn head_boundary(origin: Vec3, model_boxes: &[WorldHitbox]) -> f32 {
    model_boxes
        .iter()
        .filter(|hitbox| hitbox.group == HitGroup::Head)
        .map(|hitbox| hitbox.min.z)
        .filter(|z| (z - origin.z).abs() <= HULL_HALF + 8.0)
        .fold(None, |best: Option<f32>, z| Some(best.map_or(z, |b| b.min(z))))
        .unwrap_or(origin.z + 20.0)
}

/// Похожа ли поза на тело игрока: объём должен накрывать `origin`, быть в рост
/// и стоять на месте игрока.
///
/// Отбраковка нужна из-за общего буфера костей: в нём может оказаться оружие,
/// нарисованное движком после тела. У оружия объём мелкий и висит у руки, и
/// такие боксы давали промахи «смотрю мимо, а мод считает, что попал».
fn plausible_body(origin: Vec3, hitboxes: &[WorldHitbox]) -> bool {
    if hitboxes.is_empty() {
        return false;
    }
    let mut min = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
    let mut max = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for hitbox in hitboxes {
        min = Vec3::new(min.x.min(hitbox.min.x), min.y.min(hitbox.min.y), min.z.min(hitbox.min.z));
        max = Vec3::new(max.x.max(hitbox.max.x), max.y.max(hitbox.max.y), max.z.max(hitbox.max.z));
    }
    if !min.is_finite() || !max.is_finite() {
        return false;
    }
    let height = max.z - min.z;
    if !(30.0..=85.0).contains(&height) {
        return false;
    }
    // Тело накрывает `origin` по высоте: ноги ниже, голова выше.
    if min.z > origin.z + 4.0 || max.z < origin.z - 4.0 {
        return false;
    }
    // И стоит там же, где игрок: смещение по горизонтали не больше корпуса.
    let center = (min + max) * 0.5;
    let flat = Vec3::new(center.x - origin.x, center.y - origin.y, 0.0).length();
    flat < 24.0
}

// Клиентский `StudioDrawPlayer`: сначала обычная отрисовка, потом запоминаем
// позицию. Читать её заранее нельзя — движок выставляет «текущую сущность»
// перед вызовом, но заполняет поля уже в своей отрисовке.
crate::hook! {
    unsafe extern "C" fn hook_draw_player(flags: i32, player_info: *mut c_void)
        original: draw_player_original
        -> i32
    {
        let _inside = InFlight::enter();
        let result = unsafe { call_original_draw_player(flags, player_info) };
        remember_drawn(player_info);
        result
    }
}

// Клиентский `StudioDrawModel`: через него движок рисует модель в руках
// (`models/v_ak47.mdl`). Своей модели в первой камере не рисуется, поэтому
// «оружие стрелка» берётся только здесь.
crate::hook! {
    unsafe extern "C" fn hook_draw_model(flags: i32)
        original: draw_model_original
        -> i32
    {
        let _inside = InFlight::enter();
        let result = unsafe { call_original_draw_model(flags) };
        remember_view_model();
        result
    }
}

// Движковый `StudioRenderModel`: только здесь матрицы костей принадлежат
// рисуемой модели. После возврата из `StudioDrawPlayer` в буфере уже кости
// оружия, а к моменту `StudioDrawModel` поза тела ещё не выставлена — поэтому
// хитбоксы снимаются ровно в отрисовке модели.
crate::hook! {
    unsafe extern "thiscall" fn hook_render_model(this: *mut c_void)
        original: render_model_original
    {
        let _inside = InFlight::enter();
        unsafe { render_model_original()(this) };
        remember_body_boxes();
    }
}

/// Ставит перехват отрисовки модели: там движок выставляет кости.
fn install_render_model() {
    if RENDER_MODEL_HOOKED.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let Some(entry) = renderer.method(SLOT_RENDER_MODEL) else {
        return;
    };
    if RENDER_MODEL_HOOKED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    // SAFETY: `hook_render_model` — функция того же ABI, что `StudioRenderModel`.
    match unsafe { hooks::install(entry, hook_render_model as *const ()) } {
        Ok(_) => log!("wallbang: перехват отрисовки модели включён ({entry:#x})"),
        Err(error) => {
            RENDER_MODEL_HOOKED.store(false, std::sync::atomic::Ordering::Release);
            log!("wallbang: не удалось перехватить отрисовку модели: {error}");
        }
    }
}

/// Поставлен ли перехват отрисовки модели.
static RENDER_MODEL_HOOKED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Снимает хитбоксы тела игрока в момент отрисовки его модели.
///
/// Матрицы костей лежат в одном общем буфере движка (`m_pbonetransform`), и
/// после возврата из отрисовки игрока там остаются кости оружия в руках —
/// именно поэтому боксы «цеплялись» за ствол. В отрисовке модели буфер
/// принадлежит ровно этому телу, и его хитбоксы ложатся на ту позу, которую
/// видно на экране. По ним и считается попадание.
fn remember_body_boxes() {
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let Some(model) = renderer.studio_model() else {
        return;
    };
    // Только тело игрока: модель в руках (`v_*`) и мировые модели не в счёт.
    if !model.is_player_model() {
        return;
    }
    let Some(entity) = renderer.current_entity() else {
        return;
    };
    let boxes = renderer.hitboxes_world(&model);
    if boxes.len() < 3 {
        return;
    }
    let index = entity.player_index();
    let origin = entity.origin();
    // Кости этой позы — для отчётов: по ним видно, чью позу поймал перехват.
    let mut low = f32::INFINITY;
    let mut high = f32::NEG_INFINITY;
    let mut bone_z = Vec::new();
    for point in renderer.bone_origins() {
        if !point.is_finite() {
            continue;
        }
        bone_z.push(point.z);
        low = low.min(point.z);
        high = high.max(point.z);
    }
    let bone_span = if bone_z.is_empty() { 0.0 } else { high - low };
    // Поза должна быть телом в рост: у трупа и у чужой модели кости лежат
    // кучей у земли. Такие боксы в цель не ставим и хороший снимок не затираем.
    if !plausible_body(origin, &boxes) {
        return;
    }
    let hull = model.hull();
    let engine_head = renderer.head_bounds(&model).map(|(center, _)| center);
    if let Ok(mut slot) = DRAWN.lock() {
        match slot.iter_mut().find(|drawn| drawn.index == index) {
            Some(drawn) => {
                drawn.origin = origin;
                drawn.hitboxes = boxes;
                drawn.hull = hull;
                drawn.engine_head = engine_head;
                drawn.bone_span = bone_span;
                drawn.bone_z = bone_z;
            }
            None => slot.push(Drawn {
                index,
                origin,
                hitboxes: boxes,
                hull,
                engine_head,
                bone_span,
                bone_z,
            }),
        }
    }
}

/// Вызов клиентского `StudioDrawModel` по сохранённому указателю.
///
/// # Safety
/// `ORIGINAL_MODEL` должен указывать на настоящий `StudioDrawModel`.
unsafe fn call_original_draw_model(flags: i32) -> i32 {
    let original = ORIGINAL_MODEL.load(std::sync::atomic::Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    // SAFETY: сигнатура из HLSDK: `int StudioDrawModel(int flags)`.
    let function: unsafe extern "C" fn(i32) -> i32 = unsafe { core::mem::transmute(original) };
    unsafe { function(flags) }
}

/// Указатель на клиентский `StudioDrawModel`, который мы оборачиваем.
static ORIGINAL_MODEL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Запоминает модель в руках, если движок рисует именно её.
fn remember_view_model() {
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let Some(entity) = renderer.current_entity() else {
        return;
    };
    let Some(path) = entity.model_path() else {
        return;
    };
    if !path.starts_with("models/v_") {
        return;
    }
    if VIEW_LOGGED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        // Уже напечатали — дальше только обновляем кеш.
    } else {
        log!("wallbang: модель в руках {path}");
    }
    if let Ok(mut slot) = VIEW_MODEL.lock() {
        *slot = Some(path);
    }
}

/// Напечатана ли уже модель в руках.
static VIEW_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Указатель на клиентский `StudioDrawPlayer`, который мы оборачиваем.
static ORIGINAL_PLAYER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Вызов клиентского `StudioDrawPlayer` по сохранённому указателю.
///
/// # Safety
/// `ORIGINAL_PLAYER` должен указывать на настоящий `StudioDrawPlayer`.
unsafe fn call_original_draw_player(flags: i32, player_info: *mut c_void) -> i32 {
    let original = ORIGINAL_PLAYER.load(std::sync::atomic::Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    // SAFETY: сигнатура из HLSDK: `int StudioDrawPlayer(int, player_info_s *)`.
    let function: unsafe extern "C" fn(i32, *mut c_void) -> i32 =
        unsafe { core::mem::transmute(original) };
    unsafe { function(flags, player_info) }
}

/// Наш интерфейс: всё как у клиента, но `draw_player` и `draw_model` — наши.
fn our_interface(previous: *const StudioInterface) -> *mut StudioInterface {
    // SAFETY: `previous` — живой интерфейс клиента.
    let draw_player = unsafe { (*previous).draw_player };
    let draw_model = unsafe { (*previous).draw_model };
    match draw_player {
        Some(function) => {
            ORIGINAL_PLAYER.store(function as usize, std::sync::atomic::Ordering::Release);
        }
        None => ORIGINAL_PLAYER.store(0, std::sync::atomic::Ordering::Release),
    }
    match draw_model {
        Some(function) => {
            ORIGINAL_MODEL.store(function as usize, std::sync::atomic::Ordering::Release);
        }
        None => ORIGINAL_MODEL.store(0, std::sync::atomic::Ordering::Release),
    }
    let ours = StudioInterface {
        version: StudioInterface::VERSION,
        // Нет клиентской отрисовки меша — нечего и оборачивать.
        draw_model: draw_model.map(|_| hook_draw_model as unsafe extern "C" fn(i32) -> i32),
        draw_player: Some(hook_draw_player),
    };
    Box::leak(Box::new(ours)) as *mut StudioInterface
}

/// Ставит перехват игроков и модели в руках.
///
/// Ставится из рендер-потока на первом кадре: к этому моменту интерфейс клиента
/// уже существует, а другие потоки байты функций не трогают.
fn install_players() {
    if PLAYER_HOOKED.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    let Some(slot) = interface_slot() else {
        return;
    };
    // SAFETY: слот — поле движка с указателем на интерфейс клиента.
    let previous = unsafe { *slot };
    if previous.is_null() {
        return;
    }
    if PLAYER_HOOKED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let ours = our_interface(previous);
    // SAFETY: интерфейс того же типа, что у клиента; `hooks::interface`
    // восстанавливает слот при выгрузке.
    match unsafe { hooks::interface(ours) } {
        Ok(()) => log!("wallbang: перехват отрисовки игроков включён"),
        Err(error) => {
            PLAYER_HOOKED.store(false, std::sync::atomic::Ordering::Release);
            log!("wallbang: не удалось перехватить игроков: {error}");
        }
    }
}

/// Поставлен ли перехват игроков.
static PLAYER_HOOKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Стена на пути пули: где и во что попали.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wall {
    /// Расстояние от точки, из которой пошёл луч.
    pub distance: f32,
    /// Материал поверхности: от него зависят потери урона.
    pub material: Material,
    /// Луч начался внутри этого сплошного: поверхность не пробивалась.
    pub startsolid: bool,
    /// Модель карты: `0` — мир, иначе brush-сущность.
    pub model: usize,
    /// Нормаль поверхности.
    pub normal: Vec3,
}

/// Трассировщик, который знает про цель: мир из карты, игрок — на своём месте.
struct AimTracer<'a> {
    map: &'a Map,
    target: Option<(i32, Vec3)>,
    /// Поверхности, которые пуля пробила по пути.
    walls: Vec<Wall>,
}

impl ballistics::Tracer for AimTracer<'_> {
    fn trace(&mut self, start: Vec3, end: Vec3) -> Ray {
        let world = self.map.bsp.trace_all(start, end);
        if let Some((index, point)) = self.target {
            let length = start.distance_to(end);
            if length > 0.1 {
                let fraction = start.distance_to(point) / length;
                if fraction < world.fraction.min(1.0) {
                    return Ray {
                        fraction,
                        end: point,
                        surface: Surface::Player(index),
                    };
                }
            }
        }
        if !world.hit() {
            return Ray {
                fraction: 1.0,
                end,
                surface: Surface::Nothing,
            };
        }
        let material = self.map.material_at(&world);
        self.walls.push(Wall {
            distance: start.distance_to(world.end),
            material,
            startsolid: world.startsolid,
            model: world.model,
            normal: world.normal,
        });
        Ray {
            fraction: world.fraction,
            end: world.end,
            surface: Surface::World(material),
        }
    }
}

/// Точка корпуса под лучом: расстояние по лучу и высота над ступнями.
///
/// Корпус — отрезок по вертикали от колен до шеи; проверяем несколько точек на
/// нём. Выбирается не ближайшая по лучу точка, а та, что ближе всех к лучу:
/// прицел смотрит в конкретное место тела, и зона попадания — именно оно. Иначе
/// луч, проходящий мимо ног, но задевающий их раньше головы, считался бы
/// выстрелом в ноги.
fn body_hit(origin: Vec3, direction: Vec3, base: Vec3) -> Option<(f32, f32)> {
    let bottom = base + Vec3::new(0.0, 0.0, *BODY.start());
    let top = base + Vec3::new(0.0, 0.0, *BODY.end());
    let mut best: Option<(f32, f32, f32)> = None;
    for step in 0..=BODY_SAMPLES {
        let t = step as f32 / BODY_SAMPLES as f32;
        let point = bottom.lerp(top, t);
        let along = (point - origin).dot(direction);
        if along < 0.0 {
            continue;
        }
        let closest = origin + direction * along;
        let miss = closest.distance_to(point);
        if miss > BODY_RADIUS {
            continue;
        }
        if best.is_none_or(|(best_miss, _, _)| miss < best_miss) {
            best = Some((miss, along, t));
        }
    }
    let (_, along, t) = best?;
    // Высота над ступнями: `origin` — середина корпуса, поэтому прибавляем
    // полувысоту, а не отсчитываем от неё.
    let height = *BODY.start() + (*BODY.end() - *BODY.start()) * t + BODY_HALF;
    Some((along, height))
}

/// Подпись у прицела: настройка примера.
#[derive(Debug, Clone)]
pub struct Wallbang {
    color: fn(&Aim) -> Color,
    scale: f32,
    text: fn(&Aim) -> String,
    show_targets: bool,
}

impl Default for Wallbang {
    fn default() -> Self {
        Self::new()
    }
}

impl Wallbang {
    /// Подпись белым, текст — [`Aim::text`].
    pub fn new() -> Self {
        Self {
            color: |_| Color::WHITE,
            scale: TEXT_SCALE,
            text: Aim::text,
            show_targets: true,
        }
    }

    /// Цвет подписи по цели: видно — зелёный, за стеной — жёлтый, не пробить —
    /// красный (так делает пример).
    pub fn color(mut self, color: fn(&Aim) -> Color) -> Self {
        self.color = color;
        self
    }

    /// Масштаб шрифта.
    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    /// Свой текст подписи.
    pub fn text(mut self, text: fn(&Aim) -> String) -> Self {
        self.text = text;
        self
    }

    /// Подсвечивать корпус цели рамкой (в конце 3D-сцены).
    pub fn show_targets(mut self, show: bool) -> Self {
        self.show_targets = show;
        self
    }

    /// Ставит перехват и начинает подписывать прицел.
    ///
    /// Карта читается на первом кадре: в момент установки движок может ещё не
    /// отдать studio-интерфейс, а без него нельзя узнать имя карты.
    ///
    /// # Errors
    /// Строка с причиной, если `client.dll` ещё не загружен или перехват не встал.
    pub fn install(self) -> Result<(), String> {
        {
            let mut settings = SETTINGS
                .lock()
                .map_err(|_| "wallbang: настройки заняты".to_string())?;
            *settings = Some(self);
        }
        let module = golden_sdk::process::client_base()
            .ok_or_else(|| "wallbang: client.dll ещё не загружен".to_string())?;
        let target = module + DRAW_NORMAL_TRIANGLES_RVA;
        // SAFETY: `hook_scene_end` — функция того же ABI, что `HUD_DrawNormalTriangles`
        // (`void(void)`), и ставится из того же потока, что её исполняет.
        unsafe { hooks::install(target, hook_scene_end as *const ()) }
            .map_err(|error| format!("wallbang: перехват не встал: {error}"))?;
        HOOKED.store(true, std::sync::atomic::Ordering::Release);
        log!("wallbang: перехват конца сцены установлен ({target:#x})");
        Ok(())
    }

    /// Последний посчитанный прицел (для отчётов и команд).
    pub fn last() -> Option<Aim> {
        LAST.lock().ok().and_then(|slot| slot.clone())
    }

    /// Сколько раз прицел находил цель.
    pub fn hits() -> u64 {
        HITS.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Сколько кадров обработал перехват: `0` — он не срабатывает.
    pub fn frames() -> u64 {
        FRAMES.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Настройки подписи; `None` — перехват ещё не поставлен.
static SETTINGS: Mutex<Option<Wallbang>> = Mutex::new(None);

/// Последний прицел этого кадра.
static LAST: Mutex<Option<Aim>> = Mutex::new(None);

/// Сколько кадров с целью.
static HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Сколько раз перехват конца сцены позвал нас: видно, работает ли он.
static FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Запрос пробы: считается на следующем кадре, из рендер-потока.
static PROBE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Просит посчитать выстрел по каждому живому игроку и написать это в лог.
///
/// Считать надо в рендер-потоке: там обновлены вид игрока и позиции сущностей.
/// Колбэк команды работает на другом потоке, поэтому он только ставит запрос.
pub fn request_probe() {
    log!("wallbang_probe: запрос принят");
    PROBE.store(true, std::sync::atomic::Ordering::Release);
}

/// Просит один раз напечатать то, что сейчас видно под прицелом.
pub fn request_aim_report() {
    AIM_REPORT.store(true, std::sync::atomic::Ordering::Release);
}

/// Поставлен ли запрос на печать прицела.
static AIM_REPORT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Проба: наводит луч на каждого нарисованного игрока от своего глаза.
///
/// Прицеливаться не надо — так видно работу мода без ручной возни, и стенд
/// может подать команду, прочитать лог и снять кадр.
fn run_probe(game: &Game, map: &Map) {
    let Some(view) = game.get_view() else {
        log!("wallbang_probe: движок не отдал вид игрока");
        return;
    };
    let weapon = local_weapon_info();
    log!(
        "wallbang_probe: глаз {:.0} {:.0} {:.0}, взгляд {:.2} {:.2} {:.2}, оружие {}",
        view.origin.x,
        view.origin.y,
        view.origin.z,
        view.forward.x,
        view.forward.y,
        view.forward.z,
        local_weapon().unwrap_or_else(|| "модель не прочиталась".to_string())
    );
    let drawn = drawn_players();
    if drawn.is_empty() {
        log!("wallbang_probe: движок в этом кадре никого не нарисовал — цели нет");
    }
    let mut checked = 0_u32;
    for entry in &drawn {
        log!(
            "wallbang_probe: слот {} модель {} костей {}, в {:.0} {:.0} {:.0}, хитбоксов {}, здоровье {}, разброс костей {:.0}",
            entry.index,
            model_name(entry.index),
            entry.bone_z.len(),
            entry.origin.x,
            entry.origin.y,
            entry.origin.z,
            entry.hitboxes.len(),
            target_health(entry.index),
            entry.bone_span
        );
        if entry.bone_span > 0.0 {
            let mut sorted = entry.bone_z.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
            log!(
                "wallbang_probe:   кости по высоте: от {:.0} до {:.0}, середина {:.0}",
                sorted[0],
                sorted[sorted.len() - 1],
                sorted[sorted.len() / 2]
            );
        }
        for hitbox in &entry.hitboxes {
            let center = hitbox.center();
            let size = hitbox.size();
            log!(
                "wallbang_probe:   {:?}: центр {:.0} {:.0} {:.0}, размер {:.0} {:.0} {:.0}",
                hitbox.group,
                center.x,
                center.y,
                center.z,
                size.x,
                size.y,
                size.z
            );
        }
        if let Some(head) = entry.engine_head {
            log!(
                "wallbang_probe:   голова движка {:.0} {:.0} {:.0}",
                head.x,
                head.y,
                head.z
            );
        }
        // Стреляем в центр каждого хитбокса: голова бьёт вчетверо сильнее
        // груди, живот в 1.25, ноги в 0.75 — как в `TraceAttack`.
        let groups = [
            (HitGroup::Head, "голова"),
            (HitGroup::Chest, "грудь"),
            (HitGroup::Stomach, "живот"),
            (HitGroup::LeftLeg, "левая нога"),
        ];
        let mut points: Vec<(&str, Vec3)> = Vec::new();
        for (group, title) in groups {
            if let Some(hitbox) = entry.hitboxes.iter().find(|hitbox| hitbox.group == group) {
                points.push((title, hitbox.center()));
            }
        }
        if points.is_empty() {
            // Хитбоксы не разобрались — остаётся грубая оценка по высоте.
            log!(
                "wallbang_probe: слот {} — хитбоксы не разобрались, считаю по высоте корпуса",
                entry.index
            );
            points.push(("голова", entry.origin + Vec3::new(0.0, 0.0, 28.0)));
            points.push(("грудь", entry.origin + Vec3::new(0.0, 0.0, 4.0)));
        }
        for (title, aim_point) in points {
            let delta = aim_point - view.origin;
            let length = delta.length();
            if length < 1.0 {
                continue;
            }
            let direction = delta * (1.0 / length);
            // Какой хитбокс стоит на луче первым: у головы это должно быть
            // `Head`, иначе объём впереди перекрывает её.
            let nearest = entry
                .hitboxes
                .iter()
                .filter_map(|hitbox| hitbox.ray(view.origin, direction).map(|t| (t, hitbox.group)))
                .fold(None, |best: Option<(f32, HitGroup)>, hit| match best {
                    Some(previous) if previous.0 <= hit.0 => Some(previous),
                    _ => Some(hit),
                });
            if let Some((distance, group)) = nearest {
                log!(
                    "wallbang_probe:   луч до {title} входит первым в {:?} на {:.1} из {:.1}",
                    group,
                    distance,
                    length
                );
            }
            match map.aim_at(view.origin, direction, weapon) {
                Some(aim) => {
                    log!(
                        "wallbang_probe: слот {} ({}) {} в {:.0} единицах (мод увидел {}), стен {}, урон {}, у цели останется {}",
                        entry.index,
                        aim.name,
                        title,
                        aim.distance,
                        aim.hitgroup.map_or("-", |group| group.in_zone()),
                        aim.walls,
                        aim.damage
                            .map_or_else(|| "не пробьёт".to_string(), |damage| damage.to_string()),
                        aim.health_left
                            .map_or_else(|| "-".to_string(), |left| left.to_string())
                    );
                    for (number, wall) in aim.surfaces.iter().enumerate() {
                        log!(
                            "wallbang_probe:   поверхность {} — {:.0} единиц, {:?}, модель {}, нормаль {:.2} {:.2} {:.2}, начало внутри {}",
                            number + 1,
                            wall.distance,
                            wall.material,
                            wall.model,
                            wall.normal.x,
                            wall.normal.y,
                            wall.normal.z,
                            wall.startsolid
                        );
                    }
                }
                None => log!(
                    "wallbang_probe: слот {} {title} — луч не попал в тело",
                    entry.index
                ),
            }
        }
        checked += 1;
    }
    log!(
        "wallbang_probe: карта {}, оружие {}, игроков проверено {checked}",
        map.name(),
        weapon.map_or("нет в руках", |weapon| weapon.display)
    );
}

/// Есть ли уже перехват: второй раз ставить нельзя.
/// Есть ли уже перехват отрисовки игроков.
static HOOKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// Конец кадра: движок уже нарисовал мир и игроков, GL свободен под текст.
//
// Рисуем **до** оригинала, как ESP: сразу после `HUD_DrawNormalTriangles`
// движок оставляет GL в своём состоянии (включённые массивы вершин и цветов),
// и сторонний вызов из драйвера на этом состоянии падает.
crate::hook! {
    unsafe extern "system" fn hook_scene_end()
        original: scene_end_original
    {
        let _inside = InFlight::enter();
        draw_frame();
        unsafe { scene_end_original()() };
    }
}

/// Один кадр: найти цель под прицелом, запомнить, подписать.
/// Печатает, во что и с какого расстояния входит луч прицела у цели.
///
/// Нужен, чтобы спор «голова или грудь» решали числа: видно, какой хитбокс
/// луч вошёл первым, насколько он опережает остальные, и насколько прицел
/// уходит от центра головы. Движок выбирает именно первый по расстоянию бокс.
fn report_entry_points(game: &Game, aim: &Aim) {
    let Some(view) = game.get_view() else {
        return;
    };
    if view.forward.length() < 0.5 {
        return;
    }
    let direction = view.forward * (1.0 / view.forward.length());
    let Some(drawn) = drawn_players()
        .into_iter()
        .find(|drawn| drawn.index == aim.player)
    else {
        return;
    };
    let mut entries: Vec<(HitGroup, f32)> = drawn
        .hitboxes
        .iter()
        .filter_map(|hitbox| Some((hitbox.group, hitbox.ray(view.origin, direction)?)))
        .collect();
    entries.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(core::cmp::Ordering::Equal));
    let text = entries
        .iter()
        .map(|(group, distance)| format!("{} {distance:.1}", group.name()))
        .collect::<Vec<_>>()
        .join(", ");
    log!("wallbang_aim: луч входит — {text}");
    // Сверка источников геометрии: габариты модели из заголовка, объём по
    // костям этого кадра и позиция сущности. Это показывает, совпадает ли
    // «бокс модели в рендере» с боксами по костям.
    match drawn.hull {
        Some((min, max)) => log!(
            "wallbang_aim: габариты модели {}..{} (высота {:.1}), origin {}",
            min.z,
            max.z,
            max.z - min.z,
            drawn.origin.z
        ),
        None => log!("wallbang_aim: габаритов в заголовке модели нет"),
    }
    if let (Some(first), Some(last)) = (
        drawn.hitboxes.iter().map(|h| h.min.z).fold(None, |acc: Option<f32>, z| {
            Some(acc.map_or(z, |a| a.min(z)))
        }),
        drawn.hitboxes.iter().map(|h| h.max.z).fold(None, |acc: Option<f32>, z| {
            Some(acc.map_or(z, |a| a.max(z)))
        }),
    ) {
        log!(
            "wallbang_aim: объём по костям {}..{} (высота {:.1}), рост костей {:.1}",
            first,
            last,
            last - first,
            drawn.bone_span
        );
    }
    // Все боксы цели рядом с лучом: видно, насколько прицел уходит от каждого и
    // какой из них движок взял бы первым (меньшая доля пути).
    let mut near: Vec<(HitGroup, f32, f32, Option<f32>)> = drawn
        .hitboxes
        .iter()
        .filter_map(|hitbox| {
            let to = hitbox.center() - view.origin;
            let along = to.dot(direction);
            if along <= 0.0 {
                return None;
            }
            let across = (to - direction * along).length();
            Some((hitbox.group, along, across, hitbox.ray(view.origin, direction)))
        })
        .collect();
    near.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(core::cmp::Ordering::Equal));
    for (group, along, across, entry) in near.iter().take(8) {
        match entry {
            Some(distance) => log!(
                "wallbang_aim:   {} центр на {along:.0}, мимо на {across:.1}, вход {distance:.1}",
                group.name()
            ),
            None => log!(
                "wallbang_aim:   {} центр на {along:.0}, мимо на {across:.1}, луч не задевает",
                group.name()
            ),
        }
    }
    if let Some(head) = drawn.hitboxes.iter().find(|hitbox| hitbox.group == HitGroup::Head) {
        let to_head = head.center() - view.origin;
        let along = to_head.dot(direction);
        let across = (to_head - direction * along).length();
        log!(
            "wallbang_aim: до центра головы вдоль луча {along:.1}, мимо на {across:.1}; размер головы {:.0}x{:.0}x{:.0}",
            head.size().x,
            head.size().y,
            head.size().z
        );
    }
}

/// Печатает, где прицел и насколько он проходит мимо нарисованных игроков.
///
/// Нужен для случая «под прицелом никого»: видно, есть ли вообще игроки в
/// отрисовке, куда смотрит глаз и на сколько луч расходится с целью.
fn report_misses(game: &Game) {
    let Some(view) = game.get_view() else {
        log!("wallbang_aim: движок не отдал вид игрока");
        return;
    };
    let drawn = drawn_players();
    log!(
        "wallbang_aim: глаз {:.0} {:.0} {:.0}, взгляд {:.2} {:.2} {:.2}, нарисовано игроков {}",
        view.origin.x,
        view.origin.y,
        view.origin.z,
        view.forward.x,
        view.forward.y,
        view.forward.z,
        drawn.len()
    );
    if view.forward.length() < 0.5 {
        log!("wallbang_aim: нулевой вектор взгляда");
        return;
    }
    let direction = view.forward * (1.0 / view.forward.length());
    for drawn in drawn.iter().take(6) {
        // Ближайший хитбокс по расстоянию вдоль луча и промах в стороне.
        let mut best = f32::INFINITY;
        let mut best_group = None;
        for hitbox in &drawn.hitboxes {
            let to = hitbox.center() - view.origin;
            let along = to.dot(direction);
            if along <= 0.0 {
                continue;
            }
            let across = (to - direction * along).length();
            if across < best {
                best = across;
                best_group = Some((hitbox.group, along));
            }
        }
        match best_group {
            Some((group, along)) => log!(
                "wallbang_aim:   слот {} ближайший бокс {} на {:.0}, мимо на {:.1}",
                drawn.index,
                group.name(),
                along,
                best
            ),
            None => log!("wallbang_aim:   слот {} позади луча", drawn.index),
        }
    }
}

/// Последняя найденная цель и кадр, в котором она нашлась.
static LAST_AIM: Mutex<Option<(Aim, u64)>> = Mutex::new(None);

/// Сколько кадров подряд подпись держится, когда луч ушёл с цели.
const AIM_TTL: u64 = 10;

/// Недавняя цель: подпись не должна мигать при малейшем уходе прицела.
fn recent_aim() -> Option<Aim> {
    let frame = FRAMES.load(std::sync::atomic::Ordering::Relaxed);
    LAST_AIM
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .filter(|(_, found)| frame.saturating_sub(*found) <= AIM_TTL)
        .map(|(aim, _)| aim)
}

/// Запоминает найденную цель для удержания подписи.
fn remember_aim(aim: &Aim) {
    if let Ok(mut slot) = LAST_AIM.lock() {
        *slot = Some((aim.clone(), FRAMES.load(std::sync::atomic::Ordering::Relaxed)));
    }
}

/// Рисует подпись у прицела. `recent` — показ устаревшей цели, когда луч ушёл.
fn draw_label(aim: &Aim, settings: &Wallbang, recent: bool) {
    let Some(overlay) = Overlay::new() else {
        return;
    };
    let (width, height) = overlay.viewport();
    let (dx, dy) = TEXT_OFFSET;
    let mut color = (settings.color)(aim);
    let text = (settings.text)(aim);
    if recent {
        color = Color::rgba(color.r, color.g, color.b, 0.55);
    }
    let x = width / 2.0 + dx;
    let y = height / 2.0 + dy;
    // Тень: через оптику прицела светлый текст теряется, а тёмная подложка под
    // ним держит контраст на любом фоне. Рисуется тем же шрифтом со сдвигом
    // по четырём направлениям — получается плотный контур.
    let ink = Color::rgba(0.0, 0.0, 0.0, 0.9);
    for (sx, sy) in [(2.0, 2.0), (2.0, -2.0), (-2.0, 2.0), (-2.0, -2.0)] {
        overlay.text_at(x + sx, y + sy, &text, ink, settings.scale);
    }
    overlay.text_at(x, y, &text, color, settings.scale);
}

/// Двенадцать рёбер хитбокса в мировых координатах.
///
/// Углы берутся из локальных границ и переводятся матрицей кости — так контур
/// ложится на настоящий, повёрнутый вместе с костью объём.
fn hitbox_edges(hitbox: &WorldHitbox) -> Vec<(Vec3, Vec3)> {
    let low = hitbox.local_min;
    let high = hitbox.local_max;
    let corner = |x: bool, y: bool, z: bool| {
        hitbox.matrix.transform_point(Vec3::new(
            if x { high.x } else { low.x },
            if y { high.y } else { low.y },
            if z { high.z } else { low.z },
        ))
    };
    let mut corners = Vec::with_capacity(8);
    for index in 0..8 {
        corners.push(corner(index & 4 != 0, index & 2 != 0, index & 1 != 0));
    }
    const EDGES: [(usize, usize); 12] = [
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
        (0, 1),
        (1, 3),
        (3, 2),
        (2, 0),
        (4, 5),
        (5, 7),
        (7, 6),
        (6, 4),
    ];
    EDGES
        .iter()
        .map(|(a, b)| (corners[*a], corners[*b]))
        .collect()
}

/// Цвет хитбокса по зоне: голова отдельно, остальное приглушено.
fn hitbox_color(group: HitGroup) -> Color {
    match group {
        HitGroup::Head => Color::rgba(1.0, 0.2, 0.2, 1.0),
        HitGroup::Chest => Color::rgba(0.2, 1.0, 0.2, 0.9),
        HitGroup::Stomach => Color::rgba(1.0, 1.0, 0.2, 0.8),
        _ => Color::rgba(0.4, 0.6, 1.0, 0.7),
    }
}

/// Здоровье всех нарисованных игроков в прошлом кадре.
static HEALTH: Mutex<Vec<(i32, i32)>> = Mutex::new(Vec::new());

/// Замечает падение здоровья у любого игрока и пишет сверку с предсказанием.
///
/// Мод не стреляет, поэтому попадание видно только по здоровью: как только
/// оно у кого-то упало, в лог уходит факт рядом с тем, что мод предсказывал
/// в предыдущем кадре. Так проверяется зона попадания без догадок.
fn watch_health(aim: Option<&Aim>) {
    let Ok(mut snapshot) = HEALTH.lock() else {
        return;
    };
    let predicted = aim.map(|aim| (aim.player, aim.damage.unwrap_or(0), aim.hitgroup));
    let players = drawn_players();
    for drawn in &players {
        let health = target_health(drawn.index);
        if health < 0 {
            continue;
        }
        let previous = snapshot
            .iter()
            .find(|(index, _)| *index == drawn.index)
            .map(|(_, health)| *health);
        if let Some(previous) = previous {
            if health < previous {
                match predicted {
                    Some((player, damage, zone)) if player == drawn.index => log!(
                        "wallbang_check: слот {}, урон по факту {} (было {}), мод предсказал {} ({})",
                        drawn.index,
                        previous - health,
                        previous,
                        damage,
                        zone.map_or("?", |group| group.in_zone())
                    ),
                    _ => log!(
                        "wallbang_check: слот {}, урон по факту {} (было {}), под прицелом была другая цель",
                        drawn.index,
                        previous - health,
                        previous
                    ),
                }
            }
        }
        match snapshot.iter_mut().find(|(index, _)| *index == drawn.index) {
            Some(slot) => slot.1 = health,
            None => snapshot.push((drawn.index, health)),
        }
    }
    snapshot.retain(|(index, _)| players.iter().any(|drawn| drawn.index == *index));
}

fn draw_frame() {
    FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Попадание снято в перехвате отрисовки этого кадра — забираем его один раз
    // на весь кадр и сразу начинаем новый проход.
    let pass = PASS.load(std::sync::atomic::Ordering::Relaxed);
    let fresh = hit_for_label(pass);
    next_pass();
    let Some(settings) = SETTINGS.lock().ok().and_then(|slot| slot.clone()) else {
        return;
    };
    install_players();
    install_render_model();
    let Some(game) = Game::get() else {
        return;
    };
    if PROBE.swap(false, std::sync::atomic::Ordering::AcqRel) {
        log!("wallbang_probe: проба пошла, кадр {}", FRAMES.load(std::sync::atomic::Ordering::Relaxed));
        match Map::current() {
            Some(map) => run_probe(&game, &map),
            None => log!("wallbang_probe: карта не прочиталась (см. строки wallbang выше)"),
        }
    }
    let Some(map) = Map::current() else {
        return;
    };
    let weapon = local_weapon_info();
    let aim = map.aim_with(weapon, fresh);
    if AIM_REPORT.swap(false, std::sync::atomic::Ordering::AcqRel) {
        match &aim {
            Some(aim) => {
                log!(
                    "wallbang_aim: {} в {:.0} единицах, стен {}, урон {}, подпись {:?}",
                    aim.name,
                    aim.distance,
                    aim.walls,
                    aim.damage
                        .map_or_else(|| "не пробьёт".to_string(), |damage| damage.to_string()),
                    (settings.text)(aim)
                );
                report_entry_points(&game, aim);
            }
            None => {
                log!("wallbang_aim: под прицелом никого");
                report_misses(&game);
            }
        }
    }
    if let Ok(mut slot) = LAST.lock() {
        *slot = aim.clone();
    }
    // Сверка предсказания с фактом: когда здоровье цели падает, в лог уходит
    // разница — так видно, ту ли зону насчитал мод.
    watch_health(LAST.lock().ok().and_then(|slot| slot.clone()).as_ref());
    // Игроки и модель в руках собраны для этого кадра; чистим в самом конце,
    // чтобы успеть по ним нарисовать.
    // Хитбоксы рисуются независимо от того, нашлась ли цель: именно по ним
    // видно, совпадает ли поза, по которой считает мод, с нарисованным телом.
    if settings.show_targets {
        draw_hitboxes(aim.as_ref().map(|aim| aim.player));
    }
    let Some(aim) = aim else {
        // Луч прошёл мимо: держим прошлую подпись короткое время, иначе текст
        // мигает при малейшем уходе прицела с хитбокса.
        if let Some(recent) = recent_aim() {
            draw_label(&recent, &settings, true);
        }
        if let Ok(mut slot) = DRAWN.lock() {
            slot.clear();
        }
        return;
    };
    HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    remember_aim(&aim);
    draw_label(&aim, &settings, false);

    if settings.show_targets {
        if let Some(overlay) = Overlay::new() {
            if let Some(drawn) = drawn_players()
                .into_iter()
                .find(|drawn| drawn.index == aim.player)
            {
                // Границы берутся из корпуса, а не из роста 72 от `origin`:
                // `origin` — середина корпуса, поэтому рамка `origin..origin+72`
                // висела над головой.
                let low = drawn
                    .hitboxes
                    .iter()
                    .map(|hitbox| hitbox.min.z)
                    .fold(drawn.origin.z, f32::min);
                let high = drawn
                    .hitboxes
                    .iter()
                    .map(|hitbox| hitbox.max.z)
                    .fold(drawn.origin.z, f32::max);
                let base = drawn.origin;
                let min = Vec3::new(base.x - 16.0, base.y - 16.0, low);
                let max = Vec3::new(base.x + 16.0, base.y + 16.0, high);
                overlay.box_xray(min, max, base, (settings.color)(&aim), 1.5);
            }
        }
    }
    // Подписи в очереди рисует тот, кто их поставил; ESP-подписи здесь не наши.
    if let Ok(mut slot) = DRAWN.lock() {
        slot.clear();
    }
}

/// Рисует хитбоксы игроков: красный — голова, зелёный — грудь.
///
/// Нужно, чтобы сверить позу: контуры должны лежать на нарисованном теле.
/// Рисуется ровно то, по чему считается попадание, — хитбоксы модели.
fn draw_hitboxes(aimed: Option<i32>) {
    let Some(overlay) = Overlay::new() else {
        return;
    };
    for drawn in drawn_players().iter().take(8) {
        if aimed.map_or(true, |player| player == drawn.index) {
            for hitbox in &drawn.hitboxes {
                overlay.xray(&hitbox_edges(hitbox), hitbox_color(hitbox.group), 1.5);
            }
        }
    }
}

/// Модель оружия локального игрока: по ней выбирается таблица урона.
///
/// Источник — модель в руках (`models/v_ak47.mdl`), которую движок рисует сам;
/// номер слота стрелка движок не отдаёт (`GetTimes` возвращает не его), поэтому
/// оружие берётся из отрисовки, а не из сущности игрока.
fn local_weapon() -> Option<String> {
    let path = view_model()?;
    Some(path.rsplit('/').next().unwrap_or(&path).to_string())
}

/// Оружие локального игрока из таблицы урона.
pub fn local_weapon_info() -> Option<&'static Weapon> {
    ballistics::weapon(&local_weapon()?)
}

/// Путь модели мира: `maps/de_dust2.bsp`.
///
/// У мира нет studio-заголовка, поэтому имя берётся из `model_t`. Сначала
/// сущность 0, затем модель 1 движка (мир прекешируется первым), затем — скан
/// ближайших сущностей. Так поиск не зависит от того, заполнил ли клиент
/// `cl_entities[0].model` к этому кадру.
fn world_model(game: &Game) -> Option<String> {
    if let Some(path) = game.get_entity(0).and_then(|world| world.model_path()) {
        if path.ends_with(".bsp") {
            return Some(path);
        }
    }
    let api = EngineStudioApi::for_engine()?;
    if let Some(path) = api.model_path_by_index(1) {
        if path.ends_with(".bsp") {
            return Some(path);
        }
    }
    (0..16).find_map(|index| {
        let path = game.get_entity(index)?.model_path()?;
        path.ends_with(".bsp").then_some(path)
    })
}

/// Каталог игры: `GOLDCLIENT_DIR`, иначе стандартная установка.
fn game_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("GOLDCLIENT_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    let standard = PathBuf::from(r"C:\Games\CS 1.6 GoldClient");
    standard.is_dir().then_some(standard)
}

/// Файл карты внутри каталога игры: `maps/<карта>.bsp` в любом каталоге мода.
pub fn locate_map(dir: &Path, model: &str) -> Option<PathBuf> {
    let direct = dir.join(model);
    if direct.is_file() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join(model);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Имя карты из имени модели мира (`maps/de_dust2.bsp` → `de_dust2`).
fn map_stem(model: &str) -> Option<String> {
    let name = model.rsplit('/').next().unwrap_or(model);
    let stem = name.strip_suffix(".bsp").unwrap_or(name);
    (!stem.is_empty() && stem != "maps").then(|| stem.to_ascii_lowercase())
}

/// `sound/materials.txt` мода: от каталога игры, от каталога карты и рядом с DLL.
fn load_materials(mod_dir: Option<&Path>) -> Materials {
    let mut candidates = Vec::new();
    if let Some(dir) = mod_dir {
        candidates.push(dir.join("sound").join("materials.txt"));
    }
    if let Some(game_dir) = game_dir() {
        candidates.push(game_dir.join("cstrike").join("sound").join("materials.txt"));
        candidates.push(game_dir.join("valve").join("sound").join("materials.txt"));
    }
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let materials = Materials::parse(&text);
            if !materials.is_empty() {
                return materials;
            }
        }
    }
    Materials::empty()
}

/// Забыть настройки при выгрузке: перехват снимет `golden_std::unload`.
pub fn reset() {
    if let Ok(mut settings) = SETTINGS.lock() {
        *settings = None;
    }
    if let Ok(mut last) = LAST.lock() {
        *last = None;
    }
    HOOKED.store(false, std::sync::atomic::Ordering::Release);
}

/// Был ли перехват уже поставлен (для диагностики из команды).
pub fn installed() -> bool {
    HOOKED.load(std::sync::atomic::Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_map_name_from_model() {
        assert_eq!(map_stem("maps/de_dust2.bsp").as_deref(), Some("de_dust2"));
        assert_eq!(map_stem("maps\\cs_office.bsp").as_deref(), Some("maps\\cs_office"));
        assert_eq!(map_stem("de_aztec").as_deref(), Some("de_aztec"));
        assert_eq!(map_stem("maps/.bsp"), None);
    }

    #[test]
    fn body_hit_picks_point_under_crosshair() {
        // `origin` игрока — середина корпуса (±36): прицел в голову даёт высоту
        // около 66 над ступнями, и это должна быть голова, а не ноги.
        let base = Vec3::new(100.0, 0.0, 0.0);
        let forward = Vec3::new(1.0, 0.0, 0.0);
        let (distance, height) =
            body_hit(Vec3::new(0.0, 0.0, 30.0), forward, base).expect("в голову");
        assert!((distance - 100.0).abs() < 1.0, "{distance}");
        assert_eq!(Hitgroup::from_height(height), Hitgroup::Head, "{height}");

        let (_, height) = body_hit(Vec3::new(0.0, 0.0, 12.0), forward, base).expect("в грудь");
        assert_eq!(Hitgroup::from_height(height), Hitgroup::Chest, "{height}");

        let (_, height) =
            body_hit(Vec3::new(0.0, 0.0, -30.0), forward, base).expect("в ноги");
        assert_eq!(Hitgroup::from_height(height), Hitgroup::Leg, "{height}");
    }

    #[test]
    fn body_hit_misses_far_rays() {
        let base = Vec3::new(100.0, 0.0, 0.0);
        let forward = Vec3::new(1.0, 0.0, 0.0);
        assert!(body_hit(Vec3::new(0.0, 40.0, 0.0), forward, base).is_none());
        assert!(body_hit(Vec3::new(0.0, 0.0, 90.0), forward, base).is_none());
    }

    #[test]
    fn hitgroup_multipliers_match_game() {
        assert_eq!(Hitgroup::from_height(70.0), Hitgroup::Head);
        assert_eq!(Hitgroup::Head.multiplier(), 4.0);
        assert_eq!(Hitgroup::Chest.multiplier(), 1.0);
        assert_eq!(Hitgroup::Stomach.multiplier(), 1.25);
        assert_eq!(Hitgroup::Leg.multiplier(), 0.75);
    }

    #[test]
    fn aim_text_reads_naturally() {
        let aim = Aim {
            player: 3,
            name: String::from("bot"),
            weapon: Some(String::from("AK47")),
            damage: Some(24),
            hitgroup: Some(Hitgroup::Chest),
            walls: 2,
            surfaces: Vec::new(),
            after_walls: vec![35, 15],
            distance: 900.0,
            health_left: Some(76),
        };
        assert_eq!(
            aim.text(),
            "AK47: через 2 стены, урон 24 в грудь, останется 76"
        );

        let lethal = Aim {
            damage: Some(232),
            hitgroup: Some(Hitgroup::Head),
            health_left: Some(0),
            ..aim
        };
        assert_eq!(
            lethal.text(),
            "AK47: через 2 стены, урон 232 в голову, убьёт"
        );
    }
}