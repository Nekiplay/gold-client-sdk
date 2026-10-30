//! Bind-поза модели: матрицы костей и мировые коробки хитбоксов.
//!
//! В `mstudiobone_t` (`0x70` байт) после имени, родителя, флагов и шести
//! контроллеров лежат 12 float-ов bind-данных. Проверено на реальных моделях
//! (`gign.mdl`, `v_ak47.mdl`): первые три — смещение кости относительно
//! родителя в её собственных осях (в юнитах), следующие три — поворот в
//! радианах (`-1.5708` = −90°, как у корневой кости), остальные шесть — мелкие
//! значения масштаба (`1/256`), для скелета не нужны.
//!
//! Поворот собирается как в движке (`AngleMatrix` из HLSDK): `Y(yaw) * X(pitch) * Z(roll)`.

use golden_sdk::math::Vec3;
use golden_sdk::studio::StudioModel;

/// Матрица 3x4: поворот `3x3` плюс смещение в последней колонке.
#[derive(Debug, Clone, Copy)]
pub struct Matrix34 {
    pub m: [f32; 12],
}

impl Matrix34 {
    pub const IDENTITY: Self = Self { m: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0] };

    /// Смещение в мировых координатах.
    pub fn origin(&self) -> Vec3 {
        Vec3::new(self.m[3], self.m[7], self.m[11])
    }

    /// Перенос на вектор.
    fn translated(position: Vec3) -> Self {
        let mut m = Self::IDENTITY;
        m.m[3] = position.x;
        m.m[7] = position.y;
        m.m[11] = position.z;
        m
    }

    /// Поворот по углам Эйлера (радианы, порядок движка).
    ///
    /// `Y(yaw) * X(pitch) * Z(roll)` — та же последовательность, что в
    /// `AngleMatrix` из HLSDK; собирается явным умножением трёх матриц, чтобы не
    /// путать строки и столбцы в развёрнутой формуле.
    fn rotation(pitch: f32, yaw: f32, roll: f32) -> Self {
        let (sp, cp) = pitch.sin_cos();
        let (sy, cy) = yaw.sin_cos();
        let (sr, cr) = roll.sin_cos();

        let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
        let rx = [[1.0, 0.0, 0.0], [0.0, cp, -sp], [0.0, sp, cp]];
        let rz = [[cr, -sr, 0.0], [sr, cr, 0.0], [0.0, 0.0, 1.0]];

        let m = mul3(&mul3(&ry, &rx), &rz);
        Self {
            m: [
                m[0][0], m[0][1], m[0][2], 0.0,
                m[1][0], m[1][1], m[1][2], 0.0,
                m[2][0], m[2][1], m[2][2], 0.0,
            ],
        }
    }

    /// `self * other` — обычное умножение 3x4 (без проективной части).
    pub fn mul(&self, other: &Self) -> Self {
        let a = &self.m;
        let b = &other.m;
        let mut out = [0.0f32; 12];
        for row in 0..3 {
            for column in 0..3 {
                out[row * 4 + column] = a[row * 4] * b[column]
                    + a[row * 4 + 1] * b[4 + column]
                    + a[row * 4 + 2] * b[8 + column];
            }
        }
        for row in 0..3 {
            out[row * 4 + 3] = a[row * 4] * b[3]
                + a[row * 4 + 1] * b[7]
                + a[row * 4 + 2] * b[11]
                + a[row * 4 + 3];
        }
        Self { m: out }
    }

    /// Точка из системы координат кости в мировые.
    pub fn transform_point(&self, point: Vec3) -> Vec3 {
        Vec3::new(
            self.m[0] * point.x + self.m[1] * point.y + self.m[2] * point.z + self.m[3],
            self.m[4] * point.x + self.m[5] * point.y + self.m[6] * point.z + self.m[7],
            self.m[8] * point.x + self.m[9] * point.y + self.m[10] * point.z + self.m[11],
        )
    }
}

/// Умножение матриц 3x3.
fn mul3(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (column, cell) in out_row.iter_mut().enumerate() {
            *cell = a[row][0] * b[0][column] + a[row][1] * b[1][column] + a[row][2] * b[2][column];
        }
    }
    out
}

/// Матрицы всех костей в bind-позе (мировые).
pub fn bind_pose(model: &StudioModel) -> Vec<Matrix34> {
    let bones = model.bones();
    let mut out = vec![Matrix34::IDENTITY; bones.len()];
    for (index, bone) in bones.iter().enumerate() {
        let local = Matrix34::translated(Vec3::new(
            bone.bind_pose[0],
            bone.bind_pose[1],
            bone.bind_pose[2],
        ))
        .mul(&Matrix34::rotation(
            bone.bind_pose[3],
            bone.bind_pose[4],
            bone.bind_pose[5],
        ));
        let parent = bone.parent;
        out[index] = if parent >= 0 && (parent as usize) < index {
            out[parent as usize].mul(&local)
        } else {
            local
        };
    }
    out
}

/// Коробка хитбокса в мировых координатах bind-позы.
#[derive(Debug, Clone, Copy)]
pub struct Box3 {
    pub min: Vec3,
    pub max: Vec3,
}

impl Box3 {
    /// Восемь углов коробки.
    pub fn corners(&self) -> [Vec3; 8] {
        let (a, b) = (self.min, self.max);
        [
            Vec3::new(a.x, a.y, a.z), Vec3::new(b.x, a.y, a.z),
            Vec3::new(b.x, b.y, a.z), Vec3::new(a.x, b.y, a.z),
            Vec3::new(a.x, a.y, b.z), Vec3::new(b.x, a.y, b.z),
            Vec3::new(b.x, b.y, b.z), Vec3::new(a.x, b.y, b.z),
        ]
    }
}

/// Хитбокс в bind-позе: группа и коробка.
#[derive(Debug, Clone, Copy)]
pub struct PlacedHitbox {
    pub group: golden_sdk::studio::HitGroup,
    pub bounds: Box3,
}

/// Переводит хитбоксы модели в bind-позу.
pub fn hitboxes(model: &StudioModel, poses: &[Matrix34]) -> Vec<PlacedHitbox> {
    model
        .hitboxes()
        .iter()
        .filter_map(|hitbox| {
            let bone = hitbox.bone;
            if bone < 0 {
                return None;
            }
            let matrix = poses.get(bone as usize)?;
            let mut min = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
            let mut max = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
            for x in [hitbox.bbmin.x, hitbox.bbmax.x] {
                for y in [hitbox.bbmin.y, hitbox.bbmax.y] {
                    for z in [hitbox.bbmin.z, hitbox.bbmax.z] {
                        let world = matrix.transform_point(Vec3::new(x, y, z));
                        min = Vec3::new(min.x.min(world.x), min.y.min(world.y), min.z.min(world.z));
                        max = Vec3::new(max.x.max(world.x), max.y.max(world.y), max.z.max(world.z));
                    }
                }
            }
            Some(PlacedHitbox {
                group: golden_sdk::studio::HitGroup::from_number(hitbox.group),
                bounds: Box3 { min, max },
            })
        })
        .collect()
}

/// Рёбра коробки: пары индексов точек из [`Box3::corners`].
pub const BOX_EDGES: [(usize, usize); 12] = [
    (0, 1), (1, 2), (2, 3), (3, 0),
    (4, 5), (5, 6), (6, 7), (7, 4),
    (0, 4), (1, 5), (2, 6), (3, 7),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a.x - b.x).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4 && (a.z - b.z).abs() < 1e-4
    }

    #[test]
    fn roll_rotates_around_z() {
        // −90° вокруг Z: ось X уходит в −Y, как у корневой кости player-моделей.
        let m = Matrix34::rotation(0.0, 0.0, -core::f32::consts::FRAC_PI_2);
        assert!(close(m.transform_point(Vec3::new(1.0, 0.0, 0.0)), Vec3::new(0.0, -1.0, 0.0)));
        assert!(close(m.transform_point(Vec3::new(0.0, 0.0, 1.0)), Vec3::new(0.0, 0.0, 1.0)));
    }

    #[test]
    fn pitch_rotates_around_x() {
        let m = Matrix34::rotation(core::f32::consts::FRAC_PI_2, 0.0, 0.0);
        assert!(close(m.transform_point(Vec3::new(0.0, 1.0, 0.0)), Vec3::new(0.0, 0.0, 1.0)));
    }

    #[test]
    fn yaw_rotates_around_y() {
        let m = Matrix34::rotation(0.0, core::f32::consts::FRAC_PI_2, 0.0);
        assert!(close(m.transform_point(Vec3::new(1.0, 0.0, 0.0)), Vec3::new(0.0, 0.0, -1.0)));
    }

    #[test]
    fn translation_applies_after_rotation() {
        // Смещение родителя переносит точку дочерней кости целиком.
        let parent = Matrix34::translated(Vec3::new(10.0, 0.0, 0.0))
            .mul(&Matrix34::rotation(0.0, 0.0, -core::f32::consts::FRAC_PI_2));
        let child = parent.mul(&Matrix34::translated(Vec3::new(5.0, 0.0, 0.0)));
        assert!(close(child.origin(), Vec3::new(10.0, -5.0, 0.0)));
    }

    #[test]
    fn identity_leaves_point_untouched() {
        let point = Vec3::new(1.5, -2.5, 3.5);
        assert!(close(Matrix34::IDENTITY.transform_point(point), point));
    }

    #[test]
    fn box_corners_cover_bounds() {
        let b = Box3 { min: Vec3::new(-1.0, -2.0, -3.0), max: Vec3::new(4.0, 5.0, 6.0) };
        let corners = b.corners();
        assert_eq!(corners.len(), 8);
        assert!(corners.iter().any(|c| close(*c, Vec3::new(-1.0, -2.0, -3.0))));
        assert!(corners.iter().any(|c| close(*c, Vec3::new(4.0, 5.0, 6.0))));
    }
}