//! Прямоугольный бокс (AABB) и пересечение с ним лучом.
//!
//! Мелкая геометрия, общая для адаптеров: тела игроков, габариты моделей,
//! области вокруг точки. Расчёт идёт без обхода BSP — это просто математика,
//! поэтому проверять десяток боксов дешевле, чем один луч по карте.

use golden_sdk::math::Vec3;

/// Прямоугольный параллелепипед, выровненный по осям.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Box3 {
    /// Минимальный угол.
    pub mins: Vec3,
    /// Максимальный угол.
    pub maxs: Vec3,
}

/// Попадание луча в бокс.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    /// Точка входа внутрь бокса.
    pub point: Vec3,
    /// Нормаль грани, через которую луч вошёл.
    pub normal: Vec3,
    /// Доля пройденного пути: `0.5` — попадание ровно на середине отрезка.
    pub fraction: f32,
}

impl Box3 {
    /// Бокс по двум углам (порядок не важен).
    pub fn new(first: Vec3, second: Vec3) -> Self {
        Self {
            mins: Vec3::new(
                first.x.min(second.x),
                first.y.min(second.y),
                first.z.min(second.z),
            ),
            maxs: Vec3::new(
                first.x.max(second.x),
                first.y.max(second.y),
                first.z.max(second.z),
            ),
        }
    }

    /// Бокс стоящего игрока вокруг точки `origin` (ноги).
    ///
    /// `VEC_HULL_MIN`/`VEC_HULL_MAX` движка: `±16` по горизонтали и `72` в
    /// высоту. Присед не учитывается — клиенту он для чужих игроков неизвестен.
    pub fn player(origin: Vec3) -> Self {
        Self {
            mins: origin + Vec3::new(-16.0, -16.0, 0.0),
            maxs: origin + Vec3::new(16.0, 16.0, 72.0),
        }
    }

    /// Бокс вокруг точки: `radius` по горизонтали и `height` вверх.
    pub fn around(origin: Vec3, radius: f32, height: f32) -> Self {
        Self {
            mins: origin + Vec3::new(-radius, -radius, 0.0),
            maxs: origin + Vec3::new(radius, radius, height),
        }
    }

    /// Центр бокса.
    pub fn center(&self) -> Vec3 {
        (self.mins + self.maxs) * 0.5
    }

    /// Размеры бокса.
    pub fn size(&self) -> Vec3 {
        self.maxs - self.mins
    }

    /// Лежит ли точка внутри.
    pub fn contains(&self, point: Vec3) -> bool {
        point.x >= self.mins.x
            && point.x <= self.maxs.x
            && point.y >= self.mins.y
            && point.y <= self.maxs.y
            && point.z >= self.mins.z
            && point.z <= self.maxs.z
    }

    /// Пересечение с отрезком `from..to` (метод плит).
    ///
    /// `None` — луч прошёл мимо или начался внутри бокса: тело, из которого
    /// предмет вылетел, не должно его же и останавливать.
    pub fn ray(&self, from: Vec3, to: Vec3) -> Option<Hit> {
        let direction = to - from;
        let mut enter = 0.0f32;
        let mut leave = 1.0f32;
        let mut axis = usize::MAX;

        for index in 0..3 {
            let start = axis_of(from, index);
            let step = axis_of(direction, index);
            let low = axis_of(self.mins, index);
            let high = axis_of(self.maxs, index);

            if step.abs() < 1e-6 {
                // Луч идёт вдоль этой оси: либо всё время внутри плиты, либо мимо.
                if start < low || start > high {
                    return None;
                }
                continue;
            }

            let inverse = 1.0 / step;
            let mut near = (low - start) * inverse;
            let mut far = (high - start) * inverse;
            if near > far {
                core::mem::swap(&mut near, &mut far);
            }
            if near > enter {
                enter = near;
                axis = index;
            }
            leave = leave.min(far);
            if enter > leave {
                return None;
            }
        }

        if axis == usize::MAX || enter <= 0.0 {
            return None;
        }

        let sign = if axis_of(direction, axis) > 0.0 {
            -1.0
        } else {
            1.0
        };
        Some(Hit {
            point: from + direction * enter,
            normal: unit(axis) * sign,
            fraction: enter,
        })
    }
}

/// Координата вектора по номеру оси.
fn axis_of(vector: Vec3, index: usize) -> f32 {
    match index {
        0 => vector.x,
        1 => vector.y,
        _ => vector.z,
    }
}

/// Единичный вектор оси.
fn unit(index: usize) -> Vec3 {
    match index {
        0 => Vec3::new(1.0, 0.0, 0.0),
        1 => Vec3::new(0.0, 1.0, 0.0),
        _ => Vec3::new(0.0, 0.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_box_is_hull_around_feet() {
        let body = Box3::player(Vec3::new(100.0, 0.0, 0.0));
        assert_eq!(body.mins, Vec3::new(84.0, -16.0, 0.0));
        assert_eq!(body.maxs, Vec3::new(116.0, 16.0, 72.0));
        assert!(body.contains(Vec3::new(100.0, 0.0, 36.0)));
    }

    #[test]
    fn ray_hits_side_face() {
        let body = Box3::player(Vec3::ZERO);
        let hit = body
            .ray(Vec3::new(-100.0, 0.0, 36.0), Vec3::new(100.0, 0.0, 36.0))
            .expect("луч должен упереться в бокс");
        assert!((hit.point.x + 16.0).abs() < 0.01, "{:?}", hit.point);
        assert!((hit.normal.x + 1.0).abs() < 0.01, "{:?}", hit.normal);
        assert!((hit.fraction - 0.42).abs() < 0.01, "{}", hit.fraction);
    }

    #[test]
    fn ray_hits_top_face() {
        let body = Box3::player(Vec3::ZERO);
        let hit = body
            .ray(Vec3::new(0.0, 0.0, 200.0), Vec3::new(0.0, 0.0, 0.0))
            .expect("луч сверху должен попасть");
        assert!((hit.point.z - 72.0).abs() < 0.01, "{:?}", hit.point);
        assert!(hit.normal.z > 0.99, "{:?}", hit.normal);
    }

    #[test]
    fn ray_misses_and_ignores_inside() {
        let body = Box3::player(Vec3::ZERO);
        assert!(
            body.ray(
                Vec3::new(-100.0, 100.0, 36.0),
                Vec3::new(100.0, 100.0, 36.0)
            )
            .is_none()
        );
        // Начало внутри бокса — промах: своё тело предмет не держит.
        assert!(body.ray(Vec3::ZERO, Vec3::new(100.0, 0.0, 0.0)).is_none());
    }
}
