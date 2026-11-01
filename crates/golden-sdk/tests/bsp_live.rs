//! Живая проверка на настоящей карте: нужна установленная игра.
//!
//! ```text
//! cargo test -p golden-sdk --test bsp_live -- --ignored --nocapture
//! ```
//!
//! Путь к игре берётся из `GOLDCLIENT_DIR`, иначе — стандартная установка.

use golden_sdk::ballistics::{self, Material, Materials, Ray, Surface, Tracer};
use golden_sdk::bsp::{Bsp, CONTENTS_EMPTY};
use golden_sdk::Vec3;

const MAP_NAME: &str = "de_dust2";

/// Каталог игры: `GOLDCLIENT_DIR` (как в `golden-std::launch`), иначе стандартный.
fn game_dir() -> std::path::PathBuf {
    let dir = std::env::var("GOLDCLIENT_DIR").unwrap_or_else(|_| r"C:\Games\CS 1.6 GoldClient".into());
    std::path::PathBuf::from(dir)
}

/// Точки спавна из текста сущностей — искать, куда стрелять, вслепую не надо.
fn spawns(bytes: &[u8]) -> Vec<Vec3> {
    let start = i32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let length = i32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let text = String::from_utf8_lossy(&bytes[start..start + length]);
    let mut points = Vec::new();
    let mut origin = None;
    let mut spawn = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('{') {
            origin = None;
            spawn = false;
        } else if line.starts_with('"') {
            let mut parts = line.split('"').filter(|part| !part.trim().is_empty());
            let key = parts.next().unwrap_or_default();
            let value = parts.next().unwrap_or_default();
            match key {
                "classname" => spawn = value.starts_with("info_player"),
                "origin" => {
                    let mut numbers = value
                        .split_whitespace()
                        .filter_map(|part| part.parse::<f32>().ok());
                    if let (Some(x), Some(y), Some(z)) = (numbers.next(), numbers.next(), numbers.next())
                    {
                        origin = Some(Vec3::new(x, y, z));
                    }
                }
                _ => {}
            }
        }
        if line.starts_with('}') {
            if spawn {
                if let Some(point) = origin {
                    points.push(point);
                }
            }
            spawn = false;
        }
    }
    points
}

/// Трассировщик для [`ballistics::simulate`] поверх карты и таблицы материалов.
struct MapTracer<'a> {
    bsp: &'a Bsp,
    materials: &'a Materials,
}

impl Tracer for MapTracer<'_> {
    fn trace(&mut self, start: Vec3, end: Vec3) -> Ray {
        let hit = self.bsp.trace_all(start, end);
        if !hit.hit() {
            return Ray {
                fraction: 1.0,
                end,
                surface: Surface::Nothing,
            };
        }
        let material = self
            .bsp
            .texture_at(hit.end, hit.normal)
            .map(|name| self.materials.classify(name))
            .unwrap_or(Material::Concrete);
        Ray {
            fraction: hit.fraction,
            end: hit.end,
            surface: Surface::World(material),
        }
    }
}

#[test]
#[ignore]
fn traces_real_map() {
    let map = game_dir()
        .join("cstrike")
        .join("maps")
        .join(format!("{MAP_NAME}.bsp"));
    let bytes = match std::fs::read(&map) {
        Ok(bytes) => bytes,
        Err(error) => {
            println!("карты нет ({}): {error}", map.display());
            return;
        }
    };
    let bsp = Bsp::parse(&bytes).expect("разбор");

    let materials_file = game_dir().join("cstrike").join("sound").join("materials.txt");
    let materials = std::fs::read_to_string(&materials_file)
        .map(|text| Materials::parse(&text))
        .unwrap_or_else(|_| Materials::empty());
    println!(
        "моделей {}, brush {}, текстур {}, материалов {}",
        bsp.models().len(),
        bsp.brushes().len(),
        bsp.textures().len(),
        materials.len()
    );
    assert!(bsp.brushes().len() > 10, "brush-модели не разобрались");
    assert!(!materials.is_empty(), "materials.txt не прочитан");

    let points = spawns(&bytes);
    println!("точек спавна {}", points.len());
    assert!(!points.is_empty(), "спавны не нашлись");

    let mut floors = 0;
    for point in &points {
        let eye = *point + Vec3::new(0.0, 0.0, 36.0);
        assert_eq!(
            bsp.point_contents(eye),
            CONTENTS_EMPTY,
            "глаз в сплошном на {point:?}"
        );
        let down = bsp.trace(eye, eye + Vec3::new(0.0, 0.0, -4096.0));
        if down.hit() {
            floors += 1;
            if floors <= 3 {
                let name = bsp.texture_at(down.end, down.normal).unwrap_or("<нет>");
                println!(
                    "  {point:?} → пол z={:.0}, {name} → {:?}",
                    down.end.z,
                    materials.classify(name)
                );
            }
        }
    }
    println!("пол найден у {floors}/{} спавнов", points.len());
    assert!(floors * 2 > points.len(), "под спавнами нет пола");

    // Луч от спавна через карту обязан во что-то попасть.
    let start = points[0] + Vec3::new(0.0, 0.0, 36.0);
    let across = bsp.trace(start, start + Vec3::new(4000.0, 0.0, 0.0));
    println!("от спавна по оси X: {:.3} → {:?}", across.fraction, across.end);
    assert!(across.hit());

    // Сквозь стены до последнего спавна: сколько поверхностей и сколько урона.
    let target = *points.last().unwrap() + Vec3::new(0.0, 0.0, 36.0);
    let direction = (target - start) * (1.0 / start.distance_to(target));
    let ak = ballistics::weapon("ak47").expect("ak47");
    for name in ["ak47", "awp", "glock18"] {
        let weapon = ballistics::weapon(name).expect(name);
        let mut tracer = MapTracer {
            bsp: &bsp,
            materials: &materials,
        };
        let shot = ballistics::simulate(weapon, start, direction, &mut tracer);
        let walls = shot.walls.min(8) as usize;
        println!(
            "{name}: стен {}, урон {:?}, по стенам {:?}",
            shot.walls,
            shot.damage.map(|damage| damage.damage),
            &shot.after_walls[..walls]
        );
        assert!(shot.walls > 0, "{name} прошёл сквозь карту без единой стены");
    }
    let _ = ak;
}