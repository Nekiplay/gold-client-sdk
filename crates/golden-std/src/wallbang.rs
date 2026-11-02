//! Wallbang: рейкаст по геометрии карты из одной точки в другую.
//!
//! Механика — в SDK: [`golden_sdk::bsp`] трассирует луч по геометрии карты,
//! [`golden_sdk::ballistics`] сопоставляет текстуру поверхности с материалом.
//! Здесь только привязка к живой игре: где взять карту и что вернуть на луч.
//!
//! ```no_run
//! use golden_std::wallbang::Map;
//! use golden_std::Vec3;
//!
//! # fn example() -> Option<()> {
//! let map = Map::current()?;
//! let hit = map.trace(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1024.0, 0.0, 0.0))?;
//! println!("{:?} на {:.0} единиц", hit.material, hit.distance);
//! # Some(())
//! # }
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use golden_sdk::ballistics::{Material, Materials};
use golden_sdk::bsp::{Bsp, Trace};
use golden_sdk::engine::EngineStudioApi;
use golden_sdk::log;
use golden_sdk::math::Vec3;

use crate::game::Game;

/// Как часто повторять неудачную загрузку карты.
const RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// Как часто проверять, не сменилась ли карта.
const RECHECK: std::time::Duration = std::time::Duration::from_secs(1);

/// Попадание луча в геометрию карты.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// Точка удара (конец луча, если ни во что не попали).
    pub point: Vec3,
    /// Нормаль поверхности, смотрит в сторону луча.
    pub normal: Vec3,
    /// Расстояние от начала луча до точки удара.
    pub distance: f32,
    /// Материал поверхности по `sound/materials.txt`.
    pub material: Material,
    /// Имя текстуры, если удалось разобрать.
    pub texture: Option<String>,
    /// `0` — мир, иначе номер brush-модели.
    pub model: usize,
    /// Луч начался внутри сплошного.
    pub startsolid: bool,
}

/// Последняя попытка разобрать карту: имя, результат и время.
#[derive(Debug)]
struct Cache {
    name: String,
    map: Option<Arc<Map>>,
    attempted: std::time::Instant,
    checked: std::time::Instant,
}

/// Карта и материалы: всё, что нужно, чтобы пустить луч.
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
        let map_name =
            map_stem(&model).ok_or_else(|| format!("непонятное имя модели мира: {model}"))?;
        let dir = game_dir().ok_or("каталог игры не найден (задайте GOLDCLIENT_DIR)")?;
        let path = locate_map(&dir, &model)
            .ok_or_else(|| format!("нет файла {model} в каталоге {}", dir.display()))?;
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("не прочитать {}: {error}", path.display()))?;
        let bsp =
            Bsp::parse(&bytes).map_err(|error| format!("разбор {}: {error}", path.display()))?;
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
    /// Возвращается `Arc`: кадр держит карту, пока трассирует луч, и смена
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
        self.texture_at(trace)
            .map_or(Material::Concrete, |name| self.materials.classify(name))
    }

    /// Имя текстуры поверхности, в которую попал луч.
    ///
    /// Для мира ищется лицевая сторона грани, для brush-модели — грань её
    /// кисти: луч приходит с нормалью, по ней и выбирается сторона.
    pub fn texture_at(&self, trace: &Trace) -> Option<&str> {
        match self.bsp.brush_of_model(trace.model) {
            Some(brush) => self.bsp.texture_at_brush(brush, trace.end, trace.normal),
            None => self.bsp.texture_at(trace.end, trace.normal),
        }
    }

    /// Рейкаст из одной точки в другую.
    ///
    /// `None` — луч прошёл насквозь: отрезок `from..to` ни во что не упёрся.
    /// Иначе [`Hit`] с точкой удара, нормалью и материалом поверхности.
    pub fn trace(&self, from: Vec3, to: Vec3) -> Option<Hit> {
        let trace = self.bsp.trace(from, to);
        if !trace.hit() {
            return None;
        }
        Some(Hit {
            point: trace.end,
            normal: trace.normal,
            distance: from.distance_to(trace.end),
            material: self.material_at(&trace),
            texture: self.texture_at(&trace).map(str::to_string),
            model: trace.model,
            startsolid: trace.startsolid,
        })
    }
}

/// Рейкаст по текущей карте: короткий путь без `Map::current`.
///
/// `None` — карта ещё не разобралась или луч прошёл насквозь.
pub fn trace(from: Vec3, to: Vec3) -> Option<Hit> {
    Map::current()?.trace(from, to)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_map_name_from_model() {
        assert_eq!(map_stem("maps/de_dust2.bsp").as_deref(), Some("de_dust2"));
        assert_eq!(
            map_stem("maps\\cs_office.bsp").as_deref(),
            Some("maps\\cs_office")
        );
        assert_eq!(map_stem("de_aztec").as_deref(), Some("de_aztec"));
        assert_eq!(map_stem("maps/.bsp"), None);
    }

    #[test]
    fn hit_distance_matches_trace() {
        let from = Vec3::new(0.0, 0.0, 0.0);
        let to = Vec3::new(0.0, 0.0, 100.0);
        let hit = Hit {
            point: Vec3::new(0.0, 0.0, 40.0),
            normal: Vec3::new(0.0, 0.0, -1.0),
            distance: from.distance_to(Vec3::new(0.0, 0.0, 40.0)),
            material: Material::Concrete,
            texture: None,
            model: 0,
            startsolid: false,
        };
        assert!((hit.distance - 40.0).abs() < 0.001);
        assert!(to.length() > hit.distance);
    }
}
