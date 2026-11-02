//! Полёт гранаты, точка её взрыва и последствия взрыва.
//!
//! Формулы повторяют сервер (`mp.dll`), а не «красивую физику»: граната — это
//! `MOVETYPE_BOUNCE` с гравитацией и трением из `CGrenade::ShootTimed*`, бросок
//! считается той же формулой, что в `CHEGrenade::WeaponIdle` /
//! `CFlashbang::WeaponIdle`, урон — `RadiusDamage`, ослепление — `RadiusFlash`.
//! Поэтому предсказание совпадает с игрой настолько, насколько совпадают
//! геометрия карты и состояние игроков на клиенте.
//!
//! Ориентиры (ReGameDLL, `ggrenade.cpp`, `wpn_shared/wpn_hegrenade.cpp`,
//! `combat.cpp`):
//!
//! * HE: гравитация `0.55`, трение `0.7`, запал `1.5` с, `pev->dmg = 100`;
//! * флешка: гравитация `0.5`, трение `0.8`, запал `1.5` с, `pev->dmg = 35`;
//! * смок: гравитация `0.5`, трение `0.8`, запал `1.5` с, `pev->dmg = 35`, но
//!   дымит только коснувшись земли (`SG_TumbleThink`), поэтому в воздухе
//!   продолжает лететь и после запала;
//! * отскок: `overbounce = 2 - friction`, скорость гасится, когда её квадрат
//!   меньше `900` (`SV_Physics_Bounce` в ReHLDS);
//! * точка взрыва: луч вниз на 40 единиц от `origin + (0,0,8)`, затем
//!   выталкивание из стены на `(dmg - 24) * 0.6` по нормали (`Detonate`);
//! * HE: `damage = 100 - distance * (100 / 350)`, игроки с уроном `< 1`
//!   пропускаются (`RadiusDamage`);
//! * флешка: `damage = 4 - distance * (4 / 1500)`, проверка конуса взгляда и
//!   линии видимости, `alpha/fade/hold` — из `RadiusFlash`.
//!
//! Чего модуль не знает: других сущностей (игроки, двери, движущиеся объекты),
//! воды и фактической брони. Трассировку задаёт вызывающий — в игре это
//! [`crate::bsp::Bsp`], в тестах — плоскость или пустота.
//!
//! Все функции чистые: ничего не читают из памяти и не требуют игры, поэтому
//! проверяются обычными тестами на хосте.

use alloc::vec::Vec;

use crate::math::Vec3;

/// Гравитация мира `sv_gravity` по умолчанию.
pub const GRAVITY: f32 = 800.0;

/// Предел скорости сущности (`sv_maxvelocity`).
const MAX_SPEED: f32 = 2000.0;

/// Шаг интегрирования: сервер GoldSrc считает физику на `host_frametime`,
/// для 100 тиков это `0.01` с.
const STEP: f32 = 0.01;

/// Квадрат скорости, ниже которого граната на земле останавливается.
const REST_SPEED_SQ: f32 = 900.0;

/// Что бросили.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Осколочная (`weapon_hegrenade`).
    He,
    /// Светошумовая (`weapon_flashbang`).
    Flash,
    /// Дымовая (`weapon_smokegrenade`).
    Smoke,
}

impl Kind {
    /// Множитель гравитации сущности (`pev->gravity`).
    pub const fn gravity(self) -> f32 {
        match self {
            Self::He => 0.55,
            Self::Flash | Self::Smoke => 0.5,
        }
    }

    /// Трение сущности (`pev->friction`): от него зависит упругость отскока.
    pub const fn friction(self) -> f32 {
        match self {
            Self::He => 0.7,
            Self::Flash | Self::Smoke => 0.8,
        }
    }

    /// Запал: `ShootTimed(..., 1.5)`.
    pub const fn fuse(self) -> f32 {
        1.5
    }

    /// Сколько максимум считать полёт: смок ждёт земли и может висеть дольше
    /// запала, остальным хватает запала.
    pub const fn max_flight(self) -> f32 {
        match self {
            Self::Smoke => 6.0,
            _ => 1.5,
        }
    }

    /// Срабатывает только коснувшись земли (`SG_TumbleThink`).
    pub const fn needs_ground(self) -> bool {
        matches!(self, Self::Smoke)
    }

    /// `pev->dmg`: у HE это ещё и урон в центре, у флешки и смока — только для
    /// выталкивания из стены.
    pub const fn blast_damage(self) -> f32 {
        match self {
            Self::He => 100.0,
            Self::Flash | Self::Smoke => 35.0,
        }
    }

    /// Радиус поражения: `350` у HE, `1500` у флешки, у смока урона нет.
    pub const fn radius(self) -> f32 {
        match self {
            Self::He => 350.0,
            Self::Flash => 1500.0,
            Self::Smoke => 0.0,
        }
    }

    /// Название для отчётов.
    pub const fn name(self) -> &'static str {
        match self {
            Self::He => "HE",
            Self::Flash => "флешка",
            Self::Smoke => "смок",
        }
    }
}

/// Попадание луча в поверхность.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    /// Точка удара (или конец луча, если ни во что не попали).
    pub point: Vec3,
    /// Нормаль поверхности, смотрит в сторону луча.
    pub normal: Vec3,
    /// Доля пройденного пути: `1.0` — ни во что не попали.
    pub fraction: f32,
    /// Луч начался внутри сплошного.
    pub allsolid: bool,
}

impl From<crate::bsp::Trace> for Surface {
    fn from(trace: crate::bsp::Trace) -> Self {
        Self {
            point: trace.end,
            normal: trace.normal,
            fraction: trace.fraction,
            allsolid: trace.allsolid,
        }
    }
}

impl Surface {
    /// Переводит результат `Bsp::trace` в попадание: `None` — насквозь.
    pub fn from_trace(trace: Option<crate::bsp::Trace>) -> Option<Self> {
        trace.map(Self::from)
    }
}

/// Результат полёта до взрыва.
#[derive(Debug, Clone)]
pub struct Flight {
    /// Точки траектории с шагом [`STEP`] — их рисует пример.
    pub points: Vec<Vec3>,
    /// Где граната была в момент взрыва (ещё до выталкивания из стены).
    pub origin: Vec3,
    /// Точка, в которой сработал взрыв (после выталкивания из стены).
    pub explosion: Vec3,
    /// Сколько раз граната коснулась поверхности.
    pub bounces: u32,
    /// Граната остановилась до взрыва.
    pub stopped: bool,
    /// Время от броска до взрыва, секунды.
    pub time: f32,
}

impl Flight {
    /// Отрезки траектории для отрисовки.
    pub fn segments(&self) -> Vec<(Vec3, Vec3)> {
        self.points
            .windows(2)
            .map(|pair| (pair[0], pair[1]))
            .collect()
    }
}

/// Углы броска после поправки `WeaponIdle` (`angThrow`).
///
/// Движок берёт `v_angle + punchangle`, а затем подмешивает наклон так, чтобы
/// «прямо» давало `-10` градусов: граната летит чуть вверх, а не в пол.
pub fn throw_angles(view_angles: Vec3) -> Vec3 {
    let pitch = view_angles.x;
    let pitch = if pitch < 0.0 {
        -10.0 + pitch * (80.0 / 90.0)
    } else {
        -10.0 + pitch * (100.0 / 90.0)
    };
    Vec3::new(pitch, view_angles.y, view_angles.z)
}

/// Базис углов GoldSrc: `(forward, right, up)`.
pub fn angle_vectors(angles: Vec3) -> (Vec3, Vec3, Vec3) {
    let (sp, cp) = angles.x.to_radians().sin_cos();
    let (sy, cy) = angles.y.to_radians().sin_cos();
    let (sr, cr) = angles.z.to_radians().sin_cos();
    let forward = Vec3::new(cp * cy, cp * sy, -sp);
    let right = Vec3::new(-sr * sp * cy + cr * sy, -sr * sp * sy - cr * cy, -sr * cp);
    let up = Vec3::new(cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp);
    (forward, right, up)
}

/// Начальная скорость гранаты: `forward * flVel + velocity игрока`.
///
/// `flVel = (90 - angThrow.x) * 6`, но не больше `750`. Скорость игрока
/// прибавляется целиком, включая вертикаль: бросок на бегу/в прыжке летит
/// дальше, чем стоя.
pub fn throw_velocity(view_angles: Vec3, player_velocity: Vec3) -> Vec3 {
    let angles = throw_angles(view_angles);
    let (forward, _, _) = angle_vectors(angles);
    let speed = ((90.0 - angles.x) * 6.0).min(750.0);
    forward * speed + player_velocity
}

/// Точка появления гранаты: `origin + view_ofs + forward * 16`.
///
/// `eye` — уже готовый `origin + view_ofs` (в игре это
/// [`crate::state::EngineState::view_origin`]).
pub fn throw_origin(eye: Vec3, view_angles: Vec3) -> Vec3 {
    let angles = throw_angles(view_angles);
    let (forward, _, _) = angle_vectors(angles);
    eye + forward * 16.0
}

/// Считает полёт до взрыва и точку срабатывания.
///
/// `trace` — трассировка точки (`origin..origin + move`): `None` означает, что
/// луч прошёл насквозь. Для карты подойдёт
/// `|a, b| bsp.trace(a, b).into()` (см. [`Surface::from_trace`]).
pub fn simulate<F>(kind: Kind, start: Vec3, velocity: Vec3, mut trace: F) -> Flight
where
    F: FnMut(Vec3, Vec3) -> Option<Surface>,
{
    let fuse = kind.fuse();
    let steps = (kind.max_flight() / STEP).ceil() as usize;
    let mut points = Vec::with_capacity(steps + 1);
    let mut origin = start;
    let mut velocity = velocity;
    let mut bounces = 0;
    let mut stopped = false;
    // Приземлилась ли граната: обе ветки трассировки выставляют флаг до
    // проверки запала ниже.
    let mut grounded;
    let mut time = 0.0f32;

    points.push(origin);

    for _ in 0..steps {
        if stopped {
            break;
        }

        // SV_AddGravity: `v.z -= gravity * sv_gravity * dt`.
        velocity.z -= kind.gravity() * GRAVITY * STEP;
        velocity = clamp_speed(velocity);

        let step = velocity * STEP;
        let end = origin + step;

        match trace(origin, end) {
            None => {
                origin = end;
                // В воздухе земля «потеряна»: смок, скатившийся с края, снова
                // ждёт приземления.
                grounded = false;
            }
            Some(hit) => {
                if hit.allsolid {
                    stopped = true;
                    origin = hit.point;
                    points.push(origin);
                    break;
                }

                origin = hit.point;
                bounces += 1;
                grounded = hit.normal.z > 0.7;

                // SV_ClipVelocity: скорость отражается с overbounce.
                let overbounce = 2.0 - kind.friction();
                velocity = clip_velocity(velocity, hit.normal, overbounce);

                // SV_Physics_Bounce: на полу гасим вертикаль, а если скорость
                // уже мала — останавливаем гранату совсем. Обе проверки движок
                // делает по шагу **до** отскока (`move`), а не по новой
                // скорости, поэтому считаем по `step`.
                if hit.normal.z > 0.7 {
                    if step.z < GRAVITY * STEP {
                        velocity.z = 0.0;
                    }
                    if step.dot(step) < REST_SPEED_SQ {
                        velocity = Vec3::ZERO;
                        stopped = true;
                    } else {
                        let push = (1.0 - hit.fraction) * STEP * 0.9;
                        origin += velocity * push;
                    }
                }
            }
        }

        points.push(origin);

        time += STEP;

        // Запал истёк: HE и флешка срабатывают в воздухе, смок — только
        // коснувшись земли, иначе дымит там, куда долетел.
        if time + 1e-4 >= fuse && (grounded || stopped || !kind.needs_ground()) {
            break;
        }
    }

    let explosion = explosion_point(kind, origin, &mut trace);

    Flight {
        points,
        origin,
        explosion,
        bounces,
        stopped,
        time,
    }
}

/// Точка взрыва `CGrenade::Detonate`: луч вниз и выталкивание из стены.
///
/// `trace` вызывается ещё раз — вниз на 40 единиц от `origin + (0,0,8)`.
/// Если под гранатой ничего нет за эти 40 единиц, взрыв остаётся в её центре.
fn explosion_point<F>(kind: Kind, origin: Vec3, trace: &mut F) -> Vec3
where
    F: FnMut(Vec3, Vec3) -> Option<Surface>,
{
    let spot = origin + Vec3::new(0.0, 0.0, 8.0);
    let down = spot - Vec3::new(0.0, 0.0, 40.0);
    match trace(spot, down) {
        Some(hit) if hit.fraction < 1.0 => {
            hit.point + hit.normal * (kind.blast_damage() - 24.0) * 0.6
        }
        // Ничего не нашли — `Explode3` не трогает `pev->origin`: взрыв остаётся
        // в центре гранаты, а не в `vecSpot`.
        _ => origin,
    }
}

/// `SV_ClipVelocity` с ненулевым `overbounce`.
fn clip_velocity(velocity: Vec3, normal: Vec3, overbounce: f32) -> Vec3 {
    let backoff = velocity.dot(normal) * overbounce;
    velocity - normal * backoff
}

/// `SV_CheckVelocity`: компоненты не больше `sv_maxvelocity`.
fn clamp_speed(velocity: Vec3) -> Vec3 {
    Vec3::new(
        velocity.x.clamp(-MAX_SPEED, MAX_SPEED),
        velocity.y.clamp(-MAX_SPEED, MAX_SPEED),
        velocity.z.clamp(-MAX_SPEED, MAX_SPEED),
    )
}

/// Игрок, которого может задеть взрыв.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    /// Слот игрока — по нему отчёт связывается с [`crate::player`].
    pub index: i32,
    /// Центр тела (`pev->origin`) — от него считается урон.
    pub origin: Vec3,
    /// Позиция глаз (`EyePosition`) — от неё считается флешка.
    pub eye: Vec3,
    /// Куда смотрит игрок: `forward` из его углов.
    pub forward: Vec3,
}

/// Кому и сколько достанется от HE.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Damage {
    /// Слот игрока.
    pub index: i32,
    /// Центр тела — над ним рисуется подпись.
    pub origin: Vec3,
    /// Расстояние от центра взрыва до центра тела.
    pub distance: f32,
    /// Урон до брони и до `TakeDamage`.
    pub damage: f32,
}

/// Кого ослепит флешка и насколько.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flash {
    /// Слот игрока.
    pub index: i32,
    /// Позиция глаз — над ней рисуется подпись.
    pub eye: Vec3,
    /// Расстояние от взрыва до глаз (движок меряет до `EyePosition`).
    pub distance: f32,
    /// Ослабленная сила вспышки `0..4`.
    pub adjusted: f32,
    /// Прозрачность вспышки: `255` — смотрел на взрыв, `200` — спиной.
    pub alpha: u8,
    /// Время затухания (`fadeTime`).
    pub fade: f32,
    /// Время держания (`fadeHold`).
    pub hold: f32,
    /// Сколько игрок реально «слепой» (`Blind(fadeTime * 0.33)`).
    pub duration: f32,
    /// Взрыв был за спиной (проверка конуса взгляда).
    pub behind: bool,
}

/// Один проверенный игрок: даже если флешка его не берёт, видно почему.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlashCheck {
    /// Слот игрока.
    pub index: i32,
    /// Позиция глаз.
    pub eye: Vec3,
    /// Расстояние до глаз.
    pub distance: f32,
    /// `4 - distance * 4 / 1500`, уже обрезано до нуля.
    pub adjusted: f32,
    /// Между взрывом и глазами есть геометрия.
    pub blocked: bool,
    /// Игрок дальше 1500 единиц.
    pub too_far: bool,
    /// Взрыв за спиной.
    pub behind: bool,
    /// Итог: `Some`, если ослепит.
    pub flash: Option<Flash>,
}

/// Урон HE по игрокам: `CBaseMonster::RadiusDamage` для `flDamage > 80`.
///
/// Радиус — `350` (`damage * 3.5`), затухание линейное. Движок не проверяет
/// стену: по умолчанию `hegrenade_penetration 0`, поэтому урон проходит и через
/// стену. Игроки с уроном меньше `1` не получают ничего.
pub fn he_damage(center: Vec3, targets: &[Target]) -> Vec<Damage> {
    let source = center + Vec3::new(0.0, 0.0, 1.0);
    let falloff = 100.0 / 350.0;

    targets
        .iter()
        .filter_map(|target| {
            let distance = source.distance_to(target.origin);
            if distance > 350.0 {
                return None;
            }
            let damage = 100.0 - distance * falloff;
            (damage >= 1.0).then_some(Damage {
                index: target.index,
                origin: target.origin,
                distance,
                damage,
            })
        })
        .collect()
}

/// Ослепление от флешки: `RadiusFlash`.
///
/// `line_of_sight(from, to)` должен вернуть `true`, если между точками нет
/// геометрии. Игроков и их тела трассировка не учитывает — движок пропускает
/// попадание в собственный бокс, так что для карты одной проверки достаточно.
pub fn flash_effect<F>(center: Vec3, targets: &[Target], mut line_of_sight: F) -> Vec<Flash>
where
    F: FnMut(Vec3, Vec3) -> bool,
{
    flash_list(center, targets, &mut line_of_sight)
}

/// То же, но возвращает весь список — удобно для отчёта и отрисовки.
pub fn flash_list<F>(center: Vec3, targets: &[Target], mut line_of_sight: F) -> Vec<Flash>
where
    F: FnMut(Vec3, Vec3) -> bool,
{
    flash_checks(center, targets, &mut line_of_sight)
        .into_iter()
        .filter_map(|check| check.flash)
        .collect()
}

/// Проверяет **всех** игроков и сохраняет причину, по которой флешка кого-то
/// не берёт: стену, расстояние или взгляд в другую сторону.
///
/// Так отчёт показывает не только «кого ослепит», но и почему остальные не
/// попали в список — иначе пустой результат не отличить от ошибки расчёта.
pub fn flash_checks<F>(center: Vec3, targets: &[Target], mut line_of_sight: F) -> Vec<FlashCheck>
where
    F: FnMut(Vec3, Vec3) -> bool,
{
    let source = center + Vec3::new(0.0, 0.0, 1.0);
    let falloff = 4.0 / 1500.0;

    targets
        .iter()
        .map(|target| {
            // `RadiusFlash` с `REGAMEDLL_FIXES` меряет расстояние до
            // `EyePosition`, а не до `BodyTarget`: `vecSpot = EyePosition`,
            // затем `(vecSrc - tr.vecEndPos).Length()`, где `vecEndPos` —
            // точка на луче до глаз.
            let distance = source.distance_to(target.eye);
            let too_far = distance > 1500.0;
            let blocked = !too_far && !line_of_sight(source, target.eye);
            let adjusted = (4.0 - distance * falloff).max(0.0);
            let behind = (source - target.eye).dot(target.forward) < 0.0;

            let flash = if !too_far && !blocked && adjusted > 0.0 {
                let (alpha, fade, hold) = if behind {
                    (200, adjusted * 1.75, adjusted / 3.5)
                } else {
                    (255, adjusted * 3.0, adjusted / 1.5)
                };
                Some(Flash {
                    index: target.index,
                    eye: target.eye,
                    distance,
                    adjusted,
                    alpha,
                    fade,
                    hold,
                    duration: fade * 0.33,
                    behind,
                })
            } else {
                None
            };

            FlashCheck {
                index: target.index,
                eye: target.eye,
                distance,
                adjusted,
                blocked,
                too_far,
                behind,
                flash,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_hits(_from: Vec3, _to: Vec3) -> Option<Surface> {
        None
    }

    /// Пол `z = 0`: попадание, если луч опустился ниже.
    fn floor(from: Vec3, to: Vec3) -> Option<Surface> {
        if to.z > 0.0 {
            return None;
        }
        let drop = from.z - to.z;
        let fraction = if drop.abs() < 1e-6 {
            0.0
        } else {
            (from.z / drop).clamp(0.0, 1.0)
        };
        let point = Vec3::new(
            from.x + (to.x - from.x) * fraction,
            from.y + (to.y - from.y) * fraction,
            0.0,
        );
        Some(Surface {
            point,
            normal: Vec3::new(0.0, 0.0, 1.0),
            fraction,
            allsolid: false,
        })
    }

    #[test]
    fn smoke_waits_for_ground_while_he_explodes_in_air() {
        // Высокий бросок вверх: за 1.5 с граната не успевает упасть.
        let start = Vec3::new(0.0, 0.0, 1000.0);

        let he = simulate(Kind::He, start, Vec3::ZERO, floor);
        assert!((he.time - 1.5).abs() < 0.02, "{}", he.time);
        assert!(he.explosion.z > 100.0, "{:?}", he.explosion);

        // Смок в воздухе не дымит: ждёт земли и только потом срабатывает.
        let smoke = simulate(Kind::Smoke, start, Vec3::ZERO, floor);
        assert!(smoke.time > 2.0, "{}", smoke.time);
        assert!(smoke.explosion.z < 10.0, "{:?}", smoke.explosion);
    }

    #[test]
    fn level_throw_goes_slightly_up_and_forward() {
        let velocity = throw_velocity(Vec3::ZERO, Vec3::ZERO);
        assert!((velocity.x - 590.88).abs() < 0.1, "{velocity:?}");
        assert!(velocity.y.abs() < 0.001, "{velocity:?}");
        assert!((velocity.z - 104.19).abs() < 0.1, "{velocity:?}");
    }

    #[test]
    fn player_velocity_is_added_whole() {
        let base = throw_velocity(Vec3::ZERO, Vec3::ZERO);
        let pushed = throw_velocity(Vec3::ZERO, Vec3::new(0.0, 100.0, 50.0));
        assert!((pushed - (base + Vec3::new(0.0, 100.0, 50.0))).length() < 0.001);
    }

    #[test]
    fn speed_is_capped() {
        let velocity = throw_velocity(Vec3::new(-89.0, 0.0, 0.0), Vec3::ZERO);
        assert!((velocity.length() - 750.0).abs() < 0.01, "{velocity:?}");
    }

    #[test]
    fn free_flight_keeps_all_points_and_explodes_in_air() {
        let start = Vec3::new(0.0, 0.0, 64.0);
        let flight = simulate(Kind::He, start, Vec3::new(300.0, 0.0, 0.0), no_hits);
        assert_eq!(flight.points.len(), 151);
        assert_eq!(flight.bounces, 0);
        assert!(!flight.stopped);
        // x: 300 * 1.5; z: 64 - 0.5*440*1.5^2 - поправка Эйлера
        // (0.5 * 440 * 1.5 * 0.01). В воздухе взрыв остаётся в центре гранаты.
        assert!(
            (flight.explosion.x - 450.0).abs() < 0.5,
            "{:?}",
            flight.explosion
        );
        assert!(
            (flight.explosion.z - (64.0 - 498.3)).abs() < 1.0,
            "{:?}",
            flight.explosion
        );
    }

    #[test]
    fn floor_bounce_never_goes_through_the_floor() {
        let floor = |from: Vec3, to: Vec3| {
            if to.z < 0.0 {
                let fraction = from.z / (from.z - to.z);
                Some(Surface {
                    point: Vec3::new(0.0, 0.0, 0.0),
                    normal: Vec3::new(0.0, 0.0, 1.0),
                    fraction,
                    allsolid: false,
                })
            } else {
                None
            }
        };
        let flight = simulate(
            Kind::He,
            Vec3::new(0.0, 0.0, 64.0),
            Vec3::new(120.0, 0.0, -200.0),
            floor,
        );
        assert!(flight.bounces > 0);
        assert!(flight.points.iter().all(|point| point.z >= -0.01));
        // Взрыв приподнят над полом на (100-24)*0.6.
        assert!(
            (flight.explosion.z - 45.6).abs() < 0.01,
            "{:?}",
            flight.explosion
        );
    }

    #[test]
    fn he_damage_matches_radius_damage() {
        let targets = [
            Target {
                index: 1,
                origin: Vec3::ZERO,
                eye: Vec3::ZERO,
                forward: Vec3::ZERO,
            },
            Target {
                index: 2,
                origin: Vec3::new(300.0, 0.0, 0.0),
                eye: Vec3::ZERO,
                forward: Vec3::ZERO,
            },
            Target {
                index: 3,
                origin: Vec3::new(350.0, 0.0, 0.0),
                eye: Vec3::ZERO,
                forward: Vec3::ZERO,
            },
        ];
        let damage = he_damage(Vec3::ZERO, &targets);
        assert_eq!(damage.len(), 2);
        assert!((damage[0].damage - 99.71).abs() < 0.01, "{:?}", damage[0]);
        assert!((damage[1].damage - 14.28).abs() < 0.01, "{:?}", damage[1]);
    }

    #[test]
    fn flash_in_front_is_stronger_than_behind() {
        let front = Target {
            index: 1,
            origin: Vec3::new(0.0, 100.0, 0.0),
            eye: Vec3::new(0.0, 100.0, 0.0),
            forward: Vec3::new(0.0, -1.0, 0.0),
        };
        let back = Target {
            forward: Vec3::new(0.0, 1.0, 0.0),
            ..front
        };
        let flashes = flash_list(Vec3::ZERO, &[front, back], |_, _| true);
        assert_eq!(flashes.len(), 2);
        assert_eq!(flashes[0].alpha, 255);
        assert!(flashes[0].fade > flashes[1].fade);
        assert_eq!(flashes[1].alpha, 200);
        assert!(flashes[1].behind);
        assert!((flashes[0].duration - flashes[0].fade * 0.33).abs() < 0.001);
    }

    #[test]
    fn flash_without_line_of_sight_does_not_blind() {
        let target = Target {
            index: 1,
            origin: Vec3::new(0.0, 100.0, 0.0),
            eye: Vec3::new(0.0, 100.0, 0.0),
            forward: Vec3::ZERO,
        };
        assert!(flash_list(Vec3::ZERO, &[target], |_, _| false).is_empty());
    }
}
