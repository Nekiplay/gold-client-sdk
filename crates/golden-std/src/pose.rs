//! Позы игроков: снимок костей в момент, когда их посчитал движок.
//!
//! Зачем это нужно. Кости лежат в объекте рендерера (`m_pbonetransform`), и
//! движок пересчитывает их **внутри** отрисовки каждого игрока — в
//! `CStudioModelRenderer::StudioSetupBones`. После возврата из
//! `StudioDrawPlayer` в памяти остаются кости *последнего* отрисованного
//! игрока, поэтому читать их «снаружи» нельзя: скелет повиснет на чужой позе.
//!
//! Рабочая схема из двух шагов:
//!
//! 1. [`PoseCache::capture`] вызывается в перехвате `StudioSetupBones` — в этот
//!    момент кости принадлежат именно этой сущности, и мы кладём их в кэш;
//! 2. [`PoseCache::get`] вызывается после отрисовки тела (в `StudioDrawPlayer`)
//!    и отдаёт готовый снимок: рисовать поверх модели можно по нему.
//!
//! Рисовать в шаге 1 нельзя — тело рисуется следом и закрывает линии.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use golden_sdk::math::Vec3;
use golden_sdk::renderer::StudioRenderer;
use golden_sdk::studio::StudioModel;

/// Сколько снимок считается свежим.
pub const POSE_TTL: Duration = Duration::from_millis(500);

/// Насколько корень скелета может отстоять от позиции сущности.
///
/// У игрока корневая кость (`origins[0]`) стоит у ног, а позиция сущности —
/// тоже у ног, поэтому расхождение в пределах шага нормально (бег, лестница,
/// прыжок). Десятки юнитов — уже чужая система координат: так выглядят
/// матрицы, посчитанные для вьюмодели оружия.
pub const POSE_MAX_ROOT_OFFSET: f32 = 48.0;

/// Поза одной сущности: где в мире находятся её кости и голова.
#[derive(Debug, Clone)]
pub struct Pose {
    /// Имя модели, к которой относится поза: по нему проверяем, что модель
    /// у сущности не сменилась.
    pub model: String,
    /// Мировые позиции костей в порядке модели.
    pub origins: Vec<Vec3>,
    /// Центр головы и радиус — из хитбокса головы, а не «на глаз».
    pub head: Option<(Vec3, f32)>,
    /// Мировой объём тела по хитбоксам: `(минимум, максимум)`.
    ///
    /// Снят в том же проходе, где считались кости, поэтому относится именно к
    /// этому игроку. Верх — макушка, низ — подошвы: по ним встают подписи.
    pub bounds: Option<(Vec3, Vec3)>,
    /// Когда снимок сделан.
    pub at: Instant,
}

impl Pose {
    /// Отрезки скелета по этому снимку.
    ///
    /// Топология берётся из модели (`кость → родитель`), координаты — из
    /// снимка. Если модель и снимок не совпадают по числу костей, берётся
    /// снимок как есть: лишние рёбра просто не нарисуются.
    pub fn segments(&self, model: &StudioModel) -> Vec<(Vec3, Vec3)> {
        model.skeleton_lines_with(&self.origins)
    }

    /// Самая низкая и самая высокая точки скелета — к ним привязываются
    /// подписи.
    pub fn extremes(&self) -> Option<(Vec3, Vec3)> {
        let mut lowest: Option<Vec3> = None;
        let mut highest: Option<Vec3> = None;
        for point in &self.origins {
            if lowest.is_none_or(|q| point.z < q.z) {
                lowest = Some(*point);
            }
            if highest.is_none_or(|q| point.z > q.z) {
                highest = Some(*point);
            }
        }
        Some((lowest?, highest?))
    }

    /// Свежий ли снимок.
    pub fn is_fresh(&self) -> bool {
        self.at.elapsed() < POSE_TTL
    }
}

/// Кэш поз по адресу сущности.
///
/// Один экземпляр на модуль: `static POSES: PoseCache = PoseCache::new();`.
pub struct PoseCache {
    inner: Mutex<Option<HashMap<usize, Pose>>>,
}

impl PoseCache {
    /// Пустой кэш.
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// Снимает позу сущности из рендерера.
    ///
    /// Вызывать **только** из перехвата `StudioSetupBones`: в другой момент
    /// кости в памяти принадлежат другому игроку.
    ///
    /// `false`, если кости ещё не посчитаны или не конечны.
    pub fn capture(
        &self,
        entity: usize,
        renderer: &StudioRenderer,
        model: &StudioModel,
    ) -> bool {
        if entity == 0 {
            return false;
        }
        let Some(origins) = renderer.bone_origins_checked(model) else {
            return false;
        };
        // Корень скелета у игрока стоит у его ног: в мире он совпадает с
        // позицией сущности в пределах шага. Матрицы, посчитанные для
        // вьюмодели оружия (руки от первого лица), лежат у камеры, и подписи по
        // ним уезжают в середину экрана — такую позу не запоминаем.
        let Some(origin) = golden_sdk::engine::EntityRef::from_raw(entity).map(|e| e.origin()) else {
            return false;
        };
        if origins[0].distance_to(origin) > POSE_MAX_ROOT_OFFSET {
            return false;
        }
        let bounds = renderer
            .body_bounds(model)
            .filter(|(lowest, highest)| (40.0..=120.0).contains(&(highest.z - lowest.z)));
        let pose = Pose {
            model: model.name().to_string(),
            origins,
            head: renderer.head_marker(model),
            bounds,
            at: Instant::now(),
        };
        let Ok(mut cache) = self.inner.lock() else {
            return false;
        };
        let map = cache.get_or_insert_with(HashMap::new);
        map.retain(|_, pose| pose.is_fresh());
        map.insert(entity, pose);
        true
    }

    /// Снимок сущности, если он свежий и относится к той же модели.
    pub fn get(&self, entity: usize, model: &StudioModel) -> Option<Pose> {
        let mut cache = self.inner.lock().ok()?;
        let map = cache.as_mut()?;
        map.retain(|_, pose| pose.is_fresh());
        map.get(&entity)
            .filter(|pose| pose.model.eq_ignore_ascii_case(model.name()))
            .cloned()
    }

    /// Сколько поз в кэше (для отчётов).
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .ok()
            .and_then(|cache| cache.as_ref().map(HashMap::len))
            .unwrap_or(0)
    }

    /// Пуст ли кэш.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for PoseCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PoseCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoseCache")
            .field("len", &self.len())
            .finish()
    }
}