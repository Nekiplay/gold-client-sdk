//! Баллистика CS 1.6: пробитие стен и урон.
//!
//! Всё повторяет `CBaseEntity::FireBullets3` из `mp.dll` (см. открытый
//! ReGameDLL как эталон): луч идёт отрезками, каждая поверхность тратит
//! «силу пробития» пули и множитель урона, а урон падает с расстоянием по
//! `range_modifier^(дистанция / 500)`.
//!
//! ```no_run
//! # use golden_sdk::ballistics::{self, Material, Surface, Ray};
//! # use golden_sdk::Vec3;
//! # struct Стена;
//! # impl ballistics::Tracer for Стена {
//! #     fn trace(&mut self, _start: Vec3, end: Vec3) -> Ray {
//! #         Ray { fraction: 1.0, end, surface: Surface::Nothing }
//! #     }
//! # }
//! let weapon = ballistics::weapon("ak47").unwrap();
//! let mut tracer = Стена;
//! let shot = ballistics::simulate(weapon, Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), &mut tracer);
//! assert!(shot.damage.is_none());
//! # let _ = Material::Concrete;
//! ```
//!
//! ## Что здесь считается
//!
//! * [`Material`] — материал поверхности и его множители (как `switch` по
//!   `CHAR_TEX_*` в `FireBullets3`);
//! * [`Materials`] — таблица «префикс имени текстуры → материал» из
//!   `sound/materials.txt` (её же читает движок для звуков шагов);
//! * [`BulletType`] — сила пробития и предельная дистанция для типа пули;
//! * [`Weapon`] — урон, `range_modifier`, число пробиваемых поверхностей;
//! * [`simulate`] — сам выстрел: сколько поверхностей пробьёт пуля и сколько
//!   урона дойдёт до игрока.
//!
//! Чего здесь нет: разброса (он случаен и к тому же у каждой пули свой),
//! множителей по зонам попадания и брони — это считает `TraceAttack` уже
//! после того, как урон дошёл. Значения урона здесь «по корпусу».

use alloc::string::String;
use alloc::vec::Vec;

use crate::math::Vec3;

/// Сколько всего несёт выстрел: предел шагов, чтобы битая карта не зациклила.
const MAX_STEPS: u32 = 32;

/// Дальность выстрела в единицах карты (`8192` из `FireBullets3`).
pub const MAX_DISTANCE: f32 = 8192.0;

/// Материал поверхности: от него зависят пробитие и урон.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Material {
    /// Бетон и всё, что не попало в таблицу: сила ×0.25.
    Concrete,
    /// Металл: сила ×0.15, урон ×0.2.
    Metal,
    /// Решётка: сила ×0.5, урон ×0.4.
    Grate,
    /// Вентиляция: сила ×0.5, урон ×0.45.
    Vent,
    /// Кафель: сила ×0.65, урон ×0.3.
    Tile,
    /// Компьютер/техника: сила ×0.4, урон ×0.45.
    Computer,
    /// Дерево: сила не меняется, урон ×0.6.
    Wood,
    /// Стекло, земля, трава, снег, плоть: множители по умолчанию.
    Soft,
}

impl Material {
    /// Во сколько раз материал ослабляет силу пробития.
    pub fn power_scale(self) -> f32 {
        match self {
            Self::Metal => 0.15,
            Self::Concrete => 0.25,
            Self::Grate | Self::Vent => 0.5,
            Self::Tile => 0.65,
            Self::Computer => 0.4,
            Self::Wood | Self::Soft => 1.0,
        }
    }

    /// Множитель урона после пробития, если материал его задаёт.
    pub fn damage_scale(self) -> Option<f32> {
        match self {
            Self::Metal => Some(0.2),
            Self::Grate => Some(0.4),
            Self::Vent | Self::Computer => Some(0.45),
            Self::Tile => Some(0.3),
            Self::Wood => Some(0.6),
            Self::Concrete | Self::Soft => None,
        }
    }

    /// Материал по букве `CHAR_TEX_*` из `sound/materials.txt`.
    fn from_char(letter: char) -> Self {
        match letter.to_ascii_uppercase() {
            'M' => Self::Metal,
            'G' => Self::Grate,
            'V' => Self::Vent,
            'T' => Self::Tile,
            'P' => Self::Computer,
            'W' => Self::Wood,
            'Y' | 'D' | 'S' | 'X' | 'N' | 'F' => Self::Soft,
            _ => Self::Concrete,
        }
    }
}

/// Таблица материалов: `sound/materials.txt` мода.
///
/// Формат строки — `M metal1`: буква материала, пробел, префикс имени
/// текстуры. Движок сравнивает первые 12 символов без учёта регистра, так же
/// делает и [`Materials::classify`].
#[derive(Debug, Default, Clone)]
pub struct Materials {
    entries: Vec<(String, Material)>,
}

impl Materials {
    /// Пустая таблица: любая текстура — [`Material::Concrete`].
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Разбирает `sound/materials.txt`.
    ///
    /// Строки с `//` и строки, где первым идёт не буква, пропускаются — как в
    /// `TEXTURETYPE_Init`.
    pub fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            let mut chars = line.chars();
            let Some(letter) = chars.next() else {
                continue;
            };
            if !letter.is_ascii_alphabetic() {
                continue;
            }
            let name = chars.as_str().trim();
            if name.is_empty() {
                continue;
            }
            entries.push((name.to_ascii_lowercase(), Material::from_char(letter)));
        }
        Self { entries }
    }

    /// Сколько материалов в таблице.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Таблица пуста.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Материал текстуры: префиксное сравнение до 12 символов.
    pub fn classify(&self, texture: &str) -> Material {
        let name = trim_texture_prefix(texture).to_ascii_lowercase();
        if name.is_empty() {
            return Material::Concrete;
        }
        for (prefix, material) in &self.entries {
            if prefix_starts_with(&name, prefix) {
                return *material;
            }
        }
        Material::Concrete
    }
}

/// Снимает префиксы, которые движок пропускает перед поиском материала:
/// `-0`, `+a`, затем `{`, `!`, `~`, пробел.
fn trim_texture_prefix(texture: &str) -> &str {
    let mut name = texture.trim();
    let bytes = name.as_bytes();
    if bytes.len() > 2 && (bytes[0] == b'-' || bytes[0] == b'+') {
        name = &name[2..];
    }
    let bytes = name.as_bytes();
    if matches!(bytes.first(), Some(b'{' | b'!' | b'~' | b' ')) {
        name = &name[1..];
    }
    name
}

/// Совпадает ли имя текстуры с префиксом таблицы (до 12 символов).
fn prefix_starts_with(name: &str, prefix: &str) -> bool {
    let name = name.as_bytes();
    let prefix = prefix.as_bytes();
    let length = prefix.len().min(12).min(name.len());
    length == prefix.len().min(12) && name[..length].eq_ignore_ascii_case(&prefix[..length])
}

/// Тип пули: задаёт силу пробития и предельную дистанцию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulletType {
    /// `BULLET_PLAYER_9MM`.
    NineMillimeter,
    /// `BULLET_PLAYER_45ACP`.
    FourFiveAcp,
    /// `BULLET_PLAYER_50AE`.
    FiftyAe,
    /// `BULLET_PLAYER_762MM`.
    SevenSixTwo,
    /// `BULLET_PLAYER_556MM`.
    FiveFiveSix,
    /// `BULLET_PLAYER_338MAG`.
    ThreeThreeEight,
    /// `BULLET_PLAYER_57MM`.
    FiveSeven,
    /// `BULLET_PLAYER_357SIG`.
    ThreeFiveSevenSig,
}

impl BulletType {
    /// Сила пробития: сколько единиц пути пуля проходит за поверхностью.
    pub fn power(self) -> i32 {
        match self {
            Self::NineMillimeter => 21,
            Self::FourFiveAcp => 15,
            Self::FiftyAe => 30,
            Self::SevenSixTwo => 39,
            Self::FiveFiveSix => 35,
            Self::ThreeThreeEight => 45,
            Self::FiveSeven => 30,
            Self::ThreeFiveSevenSig => 25,
        }
    }

    /// Дистанция, после которой пробитие прекращается.
    pub fn penetration_distance(self) -> f32 {
        match self {
            Self::NineMillimeter => 800.0,
            Self::FourFiveAcp => 500.0,
            Self::FiftyAe => 1000.0,
            Self::SevenSixTwo => 5000.0,
            Self::FiveFiveSix => 4000.0,
            Self::ThreeThreeEight => 8000.0,
            Self::FiveSeven => 2000.0,
            Self::ThreeFiveSevenSig => 800.0,
        }
    }
}

/// Оружие, для которого есть данные о выстреле.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weapon {
    /// Имя, как в `weapons.h`: `ak47`, `m4a1`.
    pub key: &'static str,
    /// Имя, как его зовёт игра: `AK-47`, `Desert Eagle`.
    pub display: &'static str,
    /// Урон одной пули по корпусу.
    pub damage: f32,
    /// Во сколько раз падает урон на 500 единиц пути.
    pub range_modifier: f32,
    /// Сколько поверхностей пуля пробивает (`iPenetration`).
    pub penetration: i32,
    /// Тип пули.
    pub bullet: BulletType,
}

/// Оружие по имени модели: `weapon("ak47")`, `weapon("v_ak47.mdl")`.
///
/// Дробовики и ножи не перечислены: у дробовика девять дробин со своим
/// разбросом, и «урон через стену» у него не одно число.
pub fn weapon(name: &str) -> Option<&'static Weapon> {
    let key = weapon_key(name);
    WEAPONS.iter().find(|weapon| weapon.key == key)
}

/// Все оружия из таблицы.
pub fn weapons() -> &'static [Weapon] {
    WEAPONS
}

/// Имя оружия из имени модели: `models/v_ak47.mdl` → `ak47`.
pub fn weapon_key(name: &str) -> String {
    let stem = name
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .split('.')
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    let key = stem
        .strip_prefix("v_")
        .or_else(|| stem.strip_prefix("w_"))
        .or_else(|| stem.strip_prefix("p_"))
        .unwrap_or(&stem);
    match key {
        // В игре модель называется `mp5.mdl`, в таблице — `mp5navy`.
        "mp5" => String::from("mp5navy"),
        other => String::from(other),
    }
}

/// Таблица оружия: `weapons.h` из `mp.dll` (см. ReGameDLL) и вызовы
/// `FireBullets3` в `wpn_*.cpp`.
static WEAPONS: &[Weapon] = &[
    Weapon {
        key: "ak47",
        display: "AK-47",
        damage: 36.0,
        range_modifier: 0.98,
        penetration: 2,
        bullet: BulletType::SevenSixTwo,
    },
    Weapon {
        key: "aug",
        display: "AUG",
        damage: 32.0,
        range_modifier: 0.96,
        penetration: 2,
        bullet: BulletType::FiveFiveSix,
    },
    Weapon {
        key: "sg552",
        display: "SG552",
        damage: 33.0,
        range_modifier: 0.955,
        penetration: 2,
        bullet: BulletType::FiveFiveSix,
    },
    Weapon {
        key: "galil",
        display: "Galil",
        damage: 30.0,
        range_modifier: 0.98,
        penetration: 2,
        bullet: BulletType::FiveFiveSix,
    },
    Weapon {
        key: "famas",
        display: "FAMAS",
        damage: 30.0,
        range_modifier: 0.96,
        penetration: 2,
        bullet: BulletType::FiveFiveSix,
    },
    Weapon {
        key: "m4a1",
        display: "M4A1",
        damage: 32.0,
        range_modifier: 0.97,
        penetration: 2,
        bullet: BulletType::FiveFiveSix,
    },
    Weapon {
        key: "m249",
        display: "M249",
        damage: 32.0,
        range_modifier: 0.97,
        penetration: 2,
        bullet: BulletType::FiveFiveSix,
    },
    Weapon {
        key: "sg550",
        display: "SG550",
        damage: 70.0,
        range_modifier: 0.98,
        penetration: 2,
        bullet: BulletType::FiveFiveSix,
    },
    Weapon {
        key: "g3sg1",
        display: "G3SG1",
        damage: 80.0,
        range_modifier: 0.98,
        penetration: 3,
        bullet: BulletType::SevenSixTwo,
    },
    Weapon {
        key: "scout",
        display: "Scout",
        damage: 75.0,
        range_modifier: 0.98,
        penetration: 3,
        bullet: BulletType::SevenSixTwo,
    },
    Weapon {
        key: "awp",
        display: "AWP",
        damage: 115.0,
        range_modifier: 0.99,
        penetration: 3,
        bullet: BulletType::ThreeThreeEight,
    },
    Weapon {
        key: "mp5navy",
        display: "MP5",
        damage: 26.0,
        range_modifier: 0.84,
        penetration: 1,
        bullet: BulletType::NineMillimeter,
    },
    Weapon {
        key: "tmp",
        display: "TMP",
        damage: 20.0,
        range_modifier: 0.85,
        penetration: 1,
        bullet: BulletType::NineMillimeter,
    },
    Weapon {
        key: "p90",
        display: "P90",
        damage: 21.0,
        range_modifier: 0.885,
        penetration: 1,
        bullet: BulletType::FiveSeven,
    },
    Weapon {
        key: "mac10",
        display: "MAC-10",
        damage: 29.0,
        range_modifier: 0.82,
        penetration: 1,
        bullet: BulletType::FourFiveAcp,
    },
    Weapon {
        key: "ump45",
        display: "UMP45",
        damage: 30.0,
        range_modifier: 0.82,
        penetration: 1,
        bullet: BulletType::FourFiveAcp,
    },
    Weapon {
        key: "deagle",
        display: "Desert Eagle",
        damage: 54.0,
        range_modifier: 0.81,
        penetration: 2,
        bullet: BulletType::FiftyAe,
    },
    Weapon {
        key: "usp",
        display: "USP",
        damage: 34.0,
        range_modifier: 0.79,
        penetration: 1,
        bullet: BulletType::FourFiveAcp,
    },
    Weapon {
        key: "glock18",
        display: "Glock-18",
        damage: 25.0,
        range_modifier: 0.75,
        penetration: 1,
        bullet: BulletType::NineMillimeter,
    },
    Weapon {
        key: "p228",
        display: "P228",
        damage: 32.0,
        range_modifier: 0.8,
        penetration: 1,
        bullet: BulletType::ThreeFiveSevenSig,
    },
    Weapon {
        key: "fiveseven",
        display: "Five-SeveN",
        damage: 20.0,
        range_modifier: 0.885,
        penetration: 1,
        bullet: BulletType::FiveSeven,
    },
    Weapon {
        key: "elite",
        display: "Elite",
        damage: 36.0,
        range_modifier: 0.75,
        penetration: 1,
        bullet: BulletType::NineMillimeter,
    },
];

/// Что стоит на пути луча.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Луч ни во что не попал.
    Nothing,
    /// Мир или brush-сущность: материал поверхности.
    World(Material),
    /// Игрок: номер сущности.
    Player(i32),
}

/// Один отрезок трассировки: как `TraceResult` движка, только с материалом.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray {
    /// Доля пройденного пути: `1.0` — не попали.
    pub fraction: f32,
    /// Точка удара.
    pub end: Vec3,
    /// Что стоит на пути.
    pub surface: Surface,
}

/// Кто умеет трассировать луч. Так [`simulate`] не зависит от карты.
pub trait Tracer {
    /// Ближайший удар на отрезке `start..end`.
    fn trace(&mut self, start: Vec3, end: Vec3) -> Ray;
}

/// Урон, дошедший до игрока.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Damage {
    /// Номер сущности игрока.
    pub player: i32,
    /// Урон: уже с множителем зоны попадания, но без брони.
    pub damage: i32,
    /// Зона попадания и её множитель.
    pub hitgroup: Hitgroup,
    /// Точка попадания.
    pub point: Vec3,
    /// Расстояние от глаза стрелка.
    pub distance: f32,
}

/// Зона попадания: множитель урона как в `CBaseEntity::TraceAttack` из `mp.dll`.
///
/// Значения — `GetHitgroupDamageMultiplier`: голова ×4, живот ×1.25, руки и
/// грудь ×1, ноги ×0.75. При попадании по хитбоксу модели зона известна точно;
/// по высоте корпуса руку от груди не отличить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hitgroup {
    /// Голова.
    Head,
    /// Грудь.
    Chest,
    /// Живот.
    Stomach,
    /// Рука: в игре считается как грудь.
    Arm,
    /// Ноги.
    Leg,
}

impl Hitgroup {
    /// Множитель урона.
    pub const fn multiplier(self) -> f32 {
        match self {
            Self::Head => 4.0,
            Self::Chest | Self::Arm => 1.0,
            Self::Stomach => 1.25,
            Self::Leg => 0.75,
        }
    }

    /// Зона по высоте попадания над ступнями (в единицах карты).
    ///
    /// Рост игрока — 72 единицы: голова в верхних 12, живот — середина,
    /// остальное — ноги. Границы не по хитбоксам модели, а по высоте корпуса:
    /// чужие скины меняют модель, но не рост.
    pub fn from_height(height: f32) -> Self {
        if height >= 60.0 {
            Self::Head
        } else if height >= 40.0 {
            Self::Chest
        } else if height >= 28.0 {
            Self::Stomach
        } else {
            Self::Leg
        }
    }

    /// Русское название для подписей.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Head => "голова",
            Self::Chest => "грудь",
            Self::Stomach => "живот",
            Self::Arm => "рука",
            Self::Leg => "ноги",
        }
    }

    /// Куда попала пуля — для подписи у прицела («в голову», «в грудь»).
    pub const fn in_zone(self) -> &'static str {
        match self {
            Self::Head => "в голову",
            Self::Chest => "в грудь",
            Self::Stomach => "в живот",
            Self::Arm => "в руку",
            Self::Leg => "в ноги",
        }
    }
}

/// Чем закончился выстрел.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shot {
    /// Урон первому игроку на пути, если пуля до него дошла.
    pub damage: Option<Damage>,
    /// Сколько сплошных поверхностей пробито до этого.
    pub walls: u32,
    /// Урон после каждой пробитой стены — для отладки и подписей.
    pub after_walls: [i32; 8],
}

impl Default for Shot {
    fn default() -> Self {
        Self::new()
    }
}

impl Shot {
    /// Пустой выстрел: ни стены, ни цели.
    pub const fn new() -> Self {
        Self {
            damage: None,
            walls: 0,
            after_walls: [0; 8],
        }
    }
}

/// Считает выстрел: `FireBullets3` без разброса.
///
/// Луч начинается в `start`, идёт по `direction` (единичный вектор), пробивает
/// поверхности и возвращает урон первому игроку, до которого дошёл.
pub fn simulate(
    weapon: &Weapon,
    start: Vec3,
    direction: Vec3,
    tracer: &mut impl Tracer,
) -> Shot {
    simulate_with(weapon, start, direction, tracer, |_, _| Hitgroup::Chest)
}

/// То же, но зону попадания называет `hit`: по игроку и точке удара.
///
/// Так считает движок: `FireBullets3` даёт урон, а `TraceAttack` умножает его на
/// множитель зоны. Голова — ×4, поэтому выстрел через дверь в голову убивает
/// там, где выстрел в корпус оставляет половину здоровья.
pub fn simulate_with(
    weapon: &Weapon,
    start: Vec3,
    direction: Vec3,
    tracer: &mut impl Tracer,
    hit: impl Fn(i32, Vec3) -> Hitgroup,
) -> Shot {
    let mut damage = weapon.damage as i32;
    let mut power = weapon.bullet.power();
    let penetration_distance = weapon.bullet.penetration_distance();
    let mut penetration = weapon.penetration;
    let mut damage_modifier = 0.5f32;
    let mut distance = MAX_DISTANCE;
    let mut source = start;
    let mut shot = Shot::new();

    let mut steps = 0;
    while penetration != 0 && steps < MAX_STEPS {
        steps += 1;
        let end = source + direction * distance;
        let ray = tracer.trace(source, end);

        // Материал поверхности ослабляет пулю ещё до того, как считается урон.
        if let Surface::World(material) = ray.surface {
            let scale = material.power_scale();
            power = (power as f32 * scale) as i32;
            if let Some(scale) = material.damage_scale() {
                damage_modifier = scale;
            }
        }

        if ray.fraction >= 1.0 {
            // Дальше пусто: цель так и не нашлась.
            break;
        }

        penetration -= 1;
        let current_distance = ray.fraction * distance;
        damage = (damage as f32 * pow(weapon.range_modifier, current_distance / 500.0)) as i32;
        if current_distance > penetration_distance {
            penetration = 0;
        }

        let is_world = matches!(ray.surface, Surface::World(_));
        let distance_modifier = if !is_world || penetration == 0 {
            power = 42;
            damage_modifier = 0.75;
            0.75
        } else {
            0.5
        };

        if let Surface::Player(player) = ray.surface {
            let hitgroup = hit(player, ray.end);
            let damage = (damage as f32 * hitgroup.multiplier()) as i32;
            shot.damage = Some(Damage {
                player,
                damage,
                hitgroup,
                point: ray.end,
                distance: start.distance_to(ray.end),
            });
            return shot;
        }

        if shot.walls < shot.after_walls.len() as u32 {
            shot.after_walls[shot.walls as usize] = damage;
        }
        shot.walls += 1;

        source = ray.end + direction * power as f32;
        distance = (distance - current_distance) * distance_modifier;
        damage = (damage as f32 * damage_modifier) as i32;
    }
    shot
}

/// `pow` для `f32`: в `no_std` своего нет (`powf` требует `std`).
fn pow(base: f32, exponent: f32) -> f32 {
    if exponent == 0.0 {
        return 1.0;
    }
    exp(exponent * ln(base))
}

/// `e^x` без `std`: приведение к `2^k · e^r` и ряд для `e^r`.
fn exp(x: f32) -> f32 {
    let k = (x * core::f32::consts::LOG2_E) as i32;
    let remainder = x - k as f32 * core::f32::consts::LN_2;
    let mut term = 1.0f32;
    let mut sum = 1.0f32;
    for n in 1..=12 {
        term *= remainder / n as f32;
        sum += term;
    }
    sum * pow2(k)
}

/// `2^k` прямо из экспоненты числа с плавающей точкой.
fn pow2(k: i32) -> f32 {
    let k = k.clamp(-126, 126);
    f32::from_bits(((127 + k) as u32) << 23)
}

/// Натуральный логарифм для `f32` без `std`: ряд по `atanh`.
fn ln(x: f32) -> f32 {
    if x <= 0.0 {
        return f32::NEG_INFINITY;
    }
    // Приводим к диапазону около 1 и раскладываем в ряд по atanh.
    let mut mantissa = x;
    let mut exponent = 0i32;
    while mantissa >= 2.0 {
        mantissa *= 0.5;
        exponent += 1;
    }
    while mantissa < 1.0 {
        mantissa *= 2.0;
        exponent -= 1;
    }
    let z = (mantissa - 1.0) / (mantissa + 1.0);
    let z2 = z * z;
    // 2·atanh(z) = ln(mantissa): двойка уже внутри многочлена.
    let series = z * (2.0 + z2 * (1.0 / 1.5 + z2 * (1.0 / 2.5 + z2 * (1.0 / 3.5 + z2 * (1.0 / 4.5)))));
    series + exponent as f32 * core::f32::consts::LN_2
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Стена на заданном `x`: материал и толщина не важны, важен сам факт удара.
    struct Wall {
        x: f32,
        material: Material,
    }

    impl Tracer for Wall {
        fn trace(&mut self, start: Vec3, end: Vec3) -> Ray {
            if start.x < self.x && end.x >= self.x {
                Ray {
                    fraction: (self.x - start.x) / (end.x - start.x),
                    end: Vec3::new(self.x, 0.0, 0.0),
                    surface: Surface::World(self.material),
                }
            } else {
                Ray {
                    fraction: 1.0,
                    end,
                    surface: Surface::Nothing,
                }
            }
        }
    }

    #[test]
    fn ak47_through_concrete() {
        let ak = weapon("ak47").expect("ak47");
        let mut wall = Wall {
            x: 100.0,
            material: Material::Concrete,
        };
        let shot = simulate(ak, Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), &mut wall);
        assert!(shot.damage.is_none());
        // Стена съела силу 39 → 9, и её же хватает только на одну поверхность.
        assert_eq!(shot.walls, 1);
    }

    #[test]
    fn ak47_hits_player_behind_wood() {
        struct Wood(i32);
        impl Tracer for Wood {
            fn trace(&mut self, start: Vec3, end: Vec3) -> Ray {
                if start.x < 100.0 && end.x >= 100.0 && self.0 == 0 {
                    self.0 += 1;
                    Ray {
                        fraction: (100.0 - start.x) / (end.x - start.x),
                        end: Vec3::new(100.0, 0.0, 0.0),
                        surface: Surface::World(Material::Wood),
                    }
                } else if start.x < 200.0 && end.x >= 200.0 && self.0 == 1 {
                    self.0 += 1;
                    Ray {
                        fraction: (200.0 - start.x) / (end.x - start.x),
                        end: Vec3::new(200.0, 0.0, 0.0),
                        surface: Surface::Player(7),
                    }
                } else {
                    Ray {
                        fraction: 1.0,
                        end,
                        surface: Surface::Nothing,
                    }
                }
            }
        }
        let ak = weapon("ak47").expect("ak47");
        let shot = simulate(ak, Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), &mut Wood(0));
        let damage = shot.damage.expect("урон дошёл");
        assert_eq!(damage.player, 7);
        assert_eq!(shot.walls, 1);
        assert!(damage.damage > 10 && damage.damage < 36, "{damage:?}");
    }

    #[test]
    fn metal_stops_ak47() {
        let ak = weapon("ak47").expect("ak47");
        let mut wall = Wall {
            x: 100.0,
            material: Material::Metal,
        };
        let shot = simulate(ak, Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), &mut wall);
        assert_eq!(shot.walls, 1);
        assert!(shot.damage.is_none());
    }

    #[test]
    fn materials_table_matches_engine_rules() {
        let table = Materials::parse(
            "// комментарий\nM metal1\nW crate\nG grate\nC wall_concrete\n",
        );
        assert_eq!(table.len(), 4);
        assert_eq!(table.classify("metal1_side"), Material::Metal);
        assert_eq!(table.classify("crate_top"), Material::Wood);
        assert_eq!(table.classify("-0metal1"), Material::Metal);
        assert_eq!(table.classify("+0{metal1"), Material::Metal);
        assert_eq!(table.classify("неизвестно"), Material::Concrete);
    }

    #[test]
    fn weapon_key_reads_model_names() {
        assert_eq!(weapon_key("models/v_ak47.mdl"), "ak47");
        assert_eq!(weapon_key("models/w_mp5.mdl"), "mp5navy");
        assert_eq!(weapon_key("deagle"), "deagle");
    }

    #[test]
    fn logarithm_is_close_to_right() {
        assert!((ln(core::f32::consts::E) - 1.0).abs() < 1e-4);
        assert!((ln(0.98) + 0.0202027).abs() < 1e-4);
        assert!((pow(0.98, 1.0) - 0.98).abs() < 1e-4);
    }
}