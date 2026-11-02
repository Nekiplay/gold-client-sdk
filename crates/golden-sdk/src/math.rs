//! Векторная математика и проекция мира на экран.
//!
//! Главная тонкость GoldSrc — соглашение о FOV: движок задаёт **горизонтальный**
//! FOV для соотношения 4:3 (`default_fov = 90`), вертикаль выводится из него
//! (`tan_y = tan(fov_x / 2) * 3 / 4`, для 90° это 73.74°), а горизонталь потом
//! расширяется под фактический аспект окна. Если взять вертикаль напрямую из
//! `fov_x` без множителя `3/4`, картинка сжимается по вертикали примерно на треть.

use core::ops::{Add, AddAssign, Div, Mul, Sub};

/// Трёхмерный вектор (`vec3_t`).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    pub fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    pub fn distance_to(self, other: Self) -> f32 {
        (self - other).length()
    }

    /// Линейная интерполяция: `t = 0` → `self`, `t = 1` → `other`.
    pub fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl Add for Vec3 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl AddAssign for Vec3 {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for Vec3 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl Mul<f32> for Vec3 {
    type Output = Self;
    fn mul(self, rhs: f32) -> Self {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs)
    }
}

impl Div<f32> for Vec3 {
    type Output = Self;
    fn div(self, rhs: f32) -> Self {
        Self::new(self.x / rhs, self.y / rhs, self.z / rhs)
    }
}

/// Точка на экране в пикселях.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ScreenPos {
    pub x: f32,
    pub y: f32,
}

/// Тангенсы половинных углов обзора по осям.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fov {
    pub tan_x: f32,
    pub tan_y: f32,
}

impl Fov {
    /// Из горизонтального FOV, заданного по соглашению 4:3 (то, что лежит в
    /// `default_fov` / `fov` движка).
    pub fn from_horizontal(fov_x_deg: f32, aspect: f32) -> Self {
        let tan_y = (fov_x_deg.to_radians() * 0.5).tan() * 0.75;
        Self {
            tan_x: tan_y * aspect,
            tan_y,
        }
    }

    /// Из вертикального FOV.
    pub fn from_vertical(fov_y_deg: f32, aspect: f32) -> Self {
        let tan_y = (fov_y_deg.to_radians() * 0.5).tan();
        Self {
            tan_x: tan_y * aspect,
            tan_y,
        }
    }

    /// Горизонтальный FOV в градусах (для логов).
    pub fn horizontal_deg(&self) -> f32 {
        self.tan_x.atan().to_degrees() * 2.0
    }

    /// Вертикальный FOV в градусах (для логов).
    pub fn vertical_deg(&self) -> f32 {
        self.tan_y.atan().to_degrees() * 2.0
    }
}

/// Система координат камеры: то, что отдаёт `engine_studio_api_t::GetViewInfo`.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub origin: Vec3,
    /// Вперёд (куда смотрит игрок).
    pub forward: Vec3,
    /// Вправо.
    pub right: Vec3,
    /// Вверх.
    pub up: Vec3,
}

impl Camera {
    pub fn new(origin: Vec3, forward: Vec3, right: Vec3, up: Vec3) -> Self {
        Self {
            origin,
            forward,
            right,
            up,
        }
    }

    /// Проекция точки мира в пиксели экрана. `None`, если точка за камерой.
    ///
    /// `size` — размер вьюпорта в пикселях.
    pub fn project(&self, point: Vec3, fov: Fov, size: (f32, f32)) -> Option<ScreenPos> {
        let rel = point - self.origin;
        let depth = rel.dot(self.forward);
        if depth <= 0.01 || !depth.is_finite() {
            return None;
        }
        let x = rel.dot(self.right);
        let y = rel.dot(self.up);

        let (w, h) = size;
        let sx = w * 0.5 + (x / depth) / fov.tan_x * (w * 0.5);
        let sy = h * 0.5 - (y / depth) / fov.tan_y * (h * 0.5);
        Some(ScreenPos { x: sx, y: sy })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: Camera = Camera {
        origin: Vec3::ZERO,
        forward: Vec3::new(0.0, 0.0, 1.0),
        right: Vec3::new(1.0, 0.0, 0.0),
        up: Vec3::new(0.0, 1.0, 0.0),
    };

    #[test]
    fn fov_matches_engine_convention() {
        let fov = Fov::from_horizontal(90.0, 16.0 / 9.0);
        assert!((fov.tan_y - 0.75).abs() < 1e-6, "tan_y = {}", fov.tan_y);
        assert!(
            (fov.vertical_deg() - 73.739_79).abs() < 1e-3,
            "{}",
            fov.vertical_deg()
        );
        assert!(
            (fov.horizontal_deg() - 106.260_21).abs() < 1e-3,
            "{}",
            fov.horizontal_deg()
        );
    }

    #[test]
    fn point_ahead_lands_in_center() {
        let fov = Fov::from_horizontal(90.0, 16.0 / 9.0);
        let p = IDENTITY
            .project(Vec3::new(0.0, 0.0, 100.0), fov, (1280.0, 720.0))
            .unwrap();
        assert!((p.x - 640.0).abs() < 1e-3);
        assert!((p.y - 360.0).abs() < 1e-3);
    }

    #[test]
    fn point_behind_camera_is_culled() {
        let fov = Fov::from_horizontal(90.0, 16.0 / 9.0);
        assert!(
            IDENTITY
                .project(Vec3::new(0.0, 0.0, -5.0), fov, (1280.0, 720.0))
                .is_none()
        );
    }

    #[test]
    fn right_edge_matches_tan_x() {
        let fov = Fov::from_horizontal(90.0, 16.0 / 9.0);
        // На дистанции 1 единица по right при tan_x = 1.333 точка уходит за край 16:9.
        let p = IDENTITY
            .project(Vec3::new(fov.tan_x, 0.0, 1.0), fov, (1280.0, 720.0))
            .unwrap();
        assert!((p.x - 1280.0).abs() < 1e-2, "x = {}", p.x);
    }

    #[test]
    fn vec3_ops() {
        let a = Vec3::new(1.0, 2.0, 3.0);
        let b = Vec3::new(4.0, 5.0, 6.0);
        assert_eq!(a.dot(b), 32.0);
        assert_eq!(a.lerp(b, 0.5), Vec3::new(2.5, 3.5, 4.5));
        assert!((a.distance_to(b) - 27.0_f32.sqrt()).abs() < 1e-6);
    }
}
