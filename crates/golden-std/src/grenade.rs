//! Предсказание броска гранаты: траектория, точка взрыва и последствия.
//!
//! Формулы и константы живут в [`golden_sdk::grenade`]; здесь только чтение
//! состояния игры: откуда бросает локальный игрок, куда он смотрит, где стоят
//! остальные, и трассировка по текущей карте
//! ([`crate::wallbang::Map`]).
//!
//! ## Точность
//!
//! * Полёт считается на клиенте от **текущего** положения и углов; если игрок
//!   побежит или повернётся, предсказание сразу устареет — пример
//!   пересчитывает его несколько раз в секунду.
//! * Другие игроки стоят там, где их видит клиент: интерполяция и пинг дают
//!   ошибку в несколько единиц.
//! * Угол взгляда чужих игроков берётся из `entity_state.angles` — это то, что
//!   сервер присылает клиенту. Наклон (`pitch`) в записи может отсутствовать,
//!   поэтому конус флешки считается прежде всего по горизонту.
//! * Трассировка — по миру и ближайшим brush-сущностям карты. Игроки, двери и
//!   движущиеся объекты не учитываются.
//! * Броня, `hegrenade_penetration` и вода не моделируются: урон — «сырой», до
//!   `TakeDamage`.

use golden_sdk::bsp::Bsp;
use golden_sdk::grenade::{
    self, Damage, Flash, FlashCheck, Flight, Kind, Surface, Target, throw_angles, throw_origin,
    throw_velocity,
};
use golden_sdk::math::Vec3;
use golden_sdk::state::EngineState;

use crate::game::Game;
use crate::wallbang::Map;

/// Высота глаз стоящего игрока.
///
/// Точное значение для локального игрока даёт
/// [`EngineState::view_offset`]; для остальных присед неизвестен, поэтому
/// берём `VEC_VIEW` стоя (`28`) — так же считает `EyePosition` в движке.
const EYE_HEIGHT: f32 = 28.0;

/// Насколько далеко от луча может стоять brush-сущность, чтобы её ещё
/// проверять. Отсекает сотни дальних моделей, которые всё равно не пересекут
/// короткий отрезок.
const BRUSH_REACH: f32 = 1024.0;

/// Поза игрока, снятая в `draw_player`.
///
/// В конце 3D-сцены чужие сущности уже не найти (`get_entity(slot)` отдаёт не
/// ту запись, а сетевой `entity_state` — мусор), зато каждый нарисованный
/// игрок проходит через наш `draw_player` с готовой записью: слот, origin и
/// углы. Пример складывает их в таблицу и отдаёт сюда.
#[derive(Debug, Clone, Copy)]
pub struct Seen {
    /// Слот игрока.
    pub index: i32,
    /// Позиция тела.
    pub origin: Vec3,
    /// Углы тела `(pitch, yaw, roll)`.
    pub angles: Vec3,
}

/// Всё, что нужно примеру для отрисовки и отчёта.
#[derive(Debug, Clone)]
pub struct Prediction {
    /// Что бросили.
    pub kind: Kind,
    /// Карта, по которой считали.
    pub map: String,
    /// Углы броска после поправки движка.
    pub angles: Vec3,
    /// Начальная скорость гранаты.
    pub velocity: Vec3,
    /// Полёт: точки, отскоки, точка взрыва.
    pub flight: Flight,
    /// Урон HE: пустой для флешки.
    pub damage: Vec<Damage>,
    /// Ослепление: пустое для HE.
    pub flashes: Vec<Flash>,
    /// Все проверенные игроки и причина отказа (для отчёта).
    pub checks: Vec<FlashCheck>,
    /// Сколько живых игроков было учтено.
    pub players: usize,
    /// Сколько живых игроков всего (остальные вне вида — позу клиент не знает).
    pub alive: usize,
    /// Слот локального игрока.
    pub local: i32,
}

/// Считает бросок из глаз локального игрока по его текущим углам.
///
/// Ошибка — это не сбой, а причина, по которой предсказать нельзя: нет карты,
/// камера не на игроке, движок ещё не отдал углы.
pub fn predict(kind: Kind, seen: &[Seen]) -> Result<Prediction, String> {
    predict_inner(kind, seen, false, true)
}

/// То же, но с диагностикой целей в лог (по команде, не каждый кадр).
pub fn inspect(kind: Kind, seen: &[Seen]) -> Result<Prediction, String> {
    predict_inner(kind, seen, true, true)
}

/// Только полёт, без расчёта урона и ослепления.
///
/// LOS-лучи по игрокам стоят несколько миллисекунд, а траектория нужна каждый
/// кадр. Пример считает цели реже и подмешивает их к этому результату.
pub fn predict_flight(kind: Kind) -> Result<Prediction, String> {
    predict_inner(kind, &[], false, false)
}

fn predict_inner(
    kind: Kind,
    seen: &[Seen],
    verbose: bool,
    with_targets: bool,
) -> Result<Prediction, String> {
    let game = Game::get().ok_or("движок не отдал studio-интерфейс")?;
    let map = Map::current().ok_or("карта ещё не разобрана")?;

    let local = game
        .view_entity()
        .filter(|index| *index > 0)
        .ok_or("камера не на игроке")?;
    let player = game.get_player(local).ok_or("локальный игрок не найден")?;

    let angles = EngineState::view_angles();
    if !finite(angles) {
        return Err("движок ещё не отдал углы камеры".to_string());
    }

    // Точка глаз: `origin + view_ofs` — так же, как в `WeaponIdle`.
    let eye = {
        let origin = EngineState::player_origin();
        if origin == Vec3::ZERO {
            player.origin() + Vec3::new(0.0, 0.0, EYE_HEIGHT)
        } else {
            origin + EngineState::view_offset()
        }
    };

    let velocity = throw_velocity(angles, EngineState::velocity());
    let start = throw_origin(eye, angles);
    let flight = grenade::simulate(kind, start, velocity, |from, to| {
        trace_solid(&map.bsp(), from, to)
    });

    let mut targets = Vec::new();
    let alive;
    if with_targets && kind != Kind::Smoke {
        // Список целей собираем из уже снятых поз (`SEEN`) и локального игрока.
        // `Game::get_players()` перебирает все 64 слота через
        // `entity_by_index` и стоит ~20 мс на кадр — для предсказания это
        // слишком дорого и не нужно: у невидимых игроков всё равно нет позы.
        let mut add = |index: i32, origin: Vec3, angles: Vec3, eye: Vec3| {
            if index <= 0 || origin == Vec3::ZERO || !finite(origin) || !finite(angles) {
                return;
            }
            if verbose {
                golden_sdk::log!(
                    "grenade: #{} origin {:.0} {:.0} {:.0}, angles {:.1} {:.1} {:.1}",
                    index,
                    origin.x,
                    origin.y,
                    origin.z,
                    angles.x,
                    angles.y,
                    angles.z
                );
            }
            targets.push(Target {
                index,
                origin,
                eye,
                forward: grenade::angle_vectors(angles).0,
            });
        };

        add(
            local,
            EngineState::player_origin(),
            EngineState::view_angles(),
            eye,
        );
        for seen in seen {
            if seen.index == local {
                continue;
            }
            add(
                seen.index,
                seen.origin,
                seen.angles,
                seen.origin + Vec3::new(0.0, 0.0, EYE_HEIGHT),
            );
        }
        alive = targets.len();
    } else {
        alive = 0;
    }

    let center = flight.explosion;
    let damage = if with_targets && kind == Kind::He {
        grenade::he_damage(center, &targets)
    } else {
        Vec::new()
    };
    let checks = if with_targets && kind == Kind::Flash {
        grenade::flash_checks(center, &targets, |from, to| {
            trace_solid(&map.bsp(), from, to).is_none()
        })
    } else {
        Vec::new()
    };
    let flashes = checks
        .iter()
        .filter_map(|check| check.flash)
        .collect::<Vec<_>>();

    Ok(Prediction {
        kind,
        map: map.name().to_string(),
        angles: throw_angles(angles),
        velocity,
        flight,
        damage,
        flashes,
        checks,
        players: targets.len(),
        alive,
        local,
    })
}

/// Луч по миру и ближайшим brush-моделям.
///
/// `Bsp::trace_all` перебирает все brush-сущности карты; на 150 шагов полёта
/// это слишком дорого для каждого кадра. Поэтому brush-модели, чей центр
/// дальше [`BRUSH_REACH`] от отрезка, пропускаются: короткий отрезок гранаты
/// они всё равно не пересекут.
fn trace_solid(bsp: &Bsp, from: Vec3, to: Vec3) -> Option<Surface> {
    let mut best = bsp.trace(from, to);
    for brush in bsp.brushes() {
        if distance_to_segment(brush.origin, from, to) > BRUSH_REACH {
            continue;
        }
        let trace = bsp.trace_brush(brush, from, to);
        if trace.fraction < best.fraction {
            best = trace;
        }
    }
    best.hit().then(|| Surface::from(best))
}

/// Расстояние от точки до отрезка.
fn distance_to_segment(point: Vec3, from: Vec3, to: Vec3) -> f32 {
    let segment = to - from;
    let length_sq = segment.dot(segment);
    if length_sq <= f32::EPSILON {
        return point.distance_to(from);
    }
    let t = ((point - from).dot(segment) / length_sq).clamp(0.0, 1.0);
    point.distance_to(from + segment * t)
}

fn finite(vector: Vec3) -> bool {
    vector.x.is_finite() && vector.y.is_finite() && vector.z.is_finite()
}

/// Кто попал в отчёт: имя и команда подтягиваются из `g_PlayerExtraInfo`.
#[derive(Debug, Clone)]
pub struct Named<T> {
    /// Данные расчёта.
    pub value: T,
    /// Ник игрока.
    pub name: String,
    /// Слот.
    pub index: i32,
    /// Здоровье на клиенте.
    pub health: i32,
}

impl Prediction {
    /// Урон с никами, по убыванию.
    pub fn named_damage(&self) -> Vec<Named<Damage>> {
        let mut rows: Vec<Named<Damage>> = self
            .damage
            .iter()
            .map(|damage| Named {
                value: *damage,
                name: name_of(damage.index),
                index: damage.index,
                health: health_of(damage.index),
            })
            .collect();
        rows.sort_by(|a, b| b.value.damage.total_cmp(&a.value.damage));
        rows
    }

    /// Ослепление с никами, по убыванию.
    pub fn named_flashes(&self) -> Vec<Named<Flash>> {
        let mut rows: Vec<Named<Flash>> = self
            .flashes
            .iter()
            .map(|flash| Named {
                value: *flash,
                name: name_of(flash.index),
                index: flash.index,
                health: health_of(flash.index),
            })
            .collect();
        rows.sort_by(|a, b| b.value.duration.total_cmp(&a.value.duration));
        rows
    }

    /// Все проверенные игроки: сначала ослеплённые, затем остальные по
    /// расстоянию. Нужно, чтобы отчёт объяснял, почему кого-то нет в списке.
    pub fn named_checks(&self) -> Vec<Named<FlashCheck>> {
        let mut rows: Vec<Named<FlashCheck>> = self
            .checks
            .iter()
            .map(|check| Named {
                value: *check,
                name: name_of(check.index),
                index: check.index,
                health: health_of(check.index),
            })
            .collect();
        rows.sort_by(|a, b| {
            b.value
                .flash
                .map_or(0.0, |flash| flash.duration)
                .total_cmp(&a.value.flash.map_or(0.0, |flash| flash.duration))
                .then_with(|| a.value.distance.total_cmp(&b.value.distance))
        });
        rows
    }
}

fn name_of(index: i32) -> String {
    Game::get()
        .and_then(|game| game.get_player(index))
        .map(|player| player.name())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("#{index}"))
}

fn health_of(index: i32) -> i32 {
    Game::get()
        .and_then(|game| game.get_player(index))
        .map(|player| player.health())
        .unwrap_or(0)
}
