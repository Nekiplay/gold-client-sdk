//! Просмотрщик студийных моделей GoldSrc (`.mdl`, версия 10).
//!
//! Работает отдельным процессом и не требует запущенной игры: читает файл с
//! диска и разбирает его теми же структурами, что и SDK (`golden_sdk::studio`).
//!
//! Что умеет:
//!
//! * отчёт по модели: имя, кости, хитбоксы, attachments, bodyparts, последовательности;
//! * скелет в bind-позе с учётом иерархии и поворотов костей;
//! * ASCII-превью (спереди/сбоку) прямо в терминале — для быстрой самопроверки;
//! * BMP с тремя проекциями: скелет, коробки хитбоксов, габариты.
//!
//! Запуск:
//!
//! ```text
//! model-viewer <путь к .mdl> [out.bmp]
//! ```

mod bind;

use std::io::Write;
use std::path::{Path, PathBuf};

use bind::{bind_pose, hitboxes, Box3, PlacedHitbox, BOX_EDGES};
use golden_sdk::studio::BoneMatrix;
use golden_sdk::math::Vec3;
use golden_sdk::studio::{HitGroup, StudioModel};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!("использование: model-viewer <путь к .mdl> [out.bmp]");
        std::process::exit(2);
    };
    let out = args.next().map(PathBuf::from);

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("не читается {}: {error}", path.display());
            std::process::exit(1);
        }
    };

    // Заголовок и все массивы формата выровнены по 4 байта, поэтому копируем
    // файл в буфер из u32 — так указатель заведомо выровнен.
    let mut aligned = vec![0u32; bytes.len().div_ceil(4)];
    // SAFETY: буфер достаточно велик (ceil), а u32 не имеет невалидных значений.
    unsafe {
        core::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            aligned.as_mut_ptr().cast::<u8>(),
            bytes.len(),
        );
    }

    // SAFETY: буфер выровнен и начинается с начала файла; `from_ptr` проверяет
    // магию, версию и длину, а все массивы читаются с проверкой границ.
    let model = match unsafe { StudioModel::from_ptr(aligned.as_ptr().cast()) } {
        Some(model) => model,
        None => {
            eprintln!("{} — не студийная модель версии 10", path.display());
            std::process::exit(1);
        }
    };

    print_report(&path, &model);

    let raw_poses = bind_pose(&model);
    let boxes = hitboxes(&model, &raw_poses);
    let poses: Vec<BoneMatrix> = raw_poses.iter().map(|pose| BoneMatrix { m: pose.m }).collect();

    if std::env::var_os("MV_DEBUG").is_some() {
        println!("\nотладка: bind-данные и мировые позиции костей");
        for (index, bone) in model.bones().iter().enumerate() {
            let pose = raw_poses[index];
            let (local, rotation) = (&bone.bind_pose[..3], &bone.bind_pose[3..6]);
            println!(
                "  [{index:>2}] {:<22} parent {:>3}  local ({:>9.2}, {:>9.2}, {:>9.2})  rot ({:>8.3}, {:>8.3}, {:>8.3})  world ({:>8.2}, {:>8.2}, {:>8.2})",
                bone.name(),
                bone.parent,
                local[0], local[1], local[2],
                rotation[0], rotation[1], rotation[2],
                pose.origin().x, pose.origin().y, pose.origin().z,
            );
        }
    }

    println!("\nbind-поза (скелет, вид спереди и сбоку):");
    print_ascii(&model, &poses, &boxes);

    if let Some(out) = out {
        match write_preview(&out, &model, &poses, &boxes) {
            Ok(()) => println!("\nпревью: {}", out.display()),
            Err(error) => eprintln!("не удалось записать {}: {error}", out.display()),
        }
    }
}

/// Текстовый отчёт по модели.
fn print_report(path: &Path, model: &StudioModel) {
    println!("файл: {}", path.display());
    println!("имя в заголовке: {}", model.name());
    println!(
        "версия: {} (IDST {:#x}), размер: {} байт",
        model.header_version(),
        model.header_id(),
        model.len()
    );

    let header = model.header();
    println!(
        "габариты из заголовка: min ({:.1}, {:.1}, {:.1}) max ({:.1}, {:.1}, {:.1})",
        header.min.x, header.min.y, header.min.z, header.max.x, header.max.y, header.max.z
    );

    let bones = model.bones();
    println!("\nкости: {}", bones.len());
    for (index, bone) in bones.iter().enumerate() {
        println!(
            "  [{index:>2}] {:<24} parent {:>3} flags {:#06x}",
            bone.name(),
            bone.parent,
            bone.flags
        );
    }

    let hitboxes = model.hitboxes();
    println!("\nхитбоксы: {}", hitboxes.len());
    for hitbox in hitboxes {
        let group = HitGroup::from_number(hitbox.group);
        println!(
            "  {:<12} кость {:>3}  ({:.1}, {:.1}, {:.1}) .. ({:.1}, {:.1}, {:.1})",
            group.name(),
            hitbox.bone,
            hitbox.bbmin.x, hitbox.bbmin.y, hitbox.bbmin.z,
            hitbox.bbmax.x, hitbox.bbmax.y, hitbox.bbmax.z
        );
    }

    let attachments = model.attachments();
    println!("\nattachments: {}", attachments.len());
    for attachment in attachments {
        println!(
            "  {:<20} кость {:>3}  ({:.2}, {:.2}, {:.2})",
            attachment.name(), attachment.bone, attachment.org.x, attachment.org.y, attachment.org.z
        );
    }

    let bodyparts = model.bodyparts();
    println!("\nbodyparts: {}", bodyparts.len());
    for part in bodyparts {
        let models: Vec<&str> = model
            .bodypart_models(part)
            .iter()
            .map(|model| model.name())
            .collect();
        println!(
            "  {:<16} вариантов {}: {}",
            part.name(),
            part.nummodels,
            models.join(", ")
        );
    }

    let sequences = model.sequences();
    println!("\nпоследовательности: {}", sequences.len());
    for sequence in sequences.iter().take(10) {
        println!(
            "  {:<24} кадров {:>3} fps {:.1}",
            sequence.name(), sequence.numframes, sequence.fps
        );
    }
    if sequences.len() > 10 {
        println!("  ... и ещё {}", sequences.len() - 10);
    }
}

/// ASCII-скелет: вид спереди (X/Z) и сбоку (Y/Z).
///
/// Один символ — примерно два юнита по вертикали: рост игрока CS 1.6 около 72
/// юнитов укладывается в 28 строк.
fn print_ascii(model: &StudioModel, poses: &[BoneMatrix], boxes: &[PlacedHitbox]) {
    let positions: Vec<Vec3> = poses.iter().map(|pose| pose.origin()).collect();
    let edges: Vec<(usize, usize)> = model.skeleton_edges().collect();

    let width = 46usize;
    let height = 28usize;

    for (label, axis_x, axis_y) in [("спереди", 0usize, 2usize), ("сбоку", 1usize, 2usize)] {
        let mut grid = vec![vec![' '; width]; height];

        let points: Vec<(f32, f32)> = positions
            .iter()
            .map(|p| (axis(*p, axis_x), axis(*p, axis_y)))
            .collect();

        // Центр сетки — середина габаритов модели, а не начало координат:
        // bind-поза игрока лежит в 0..72 юнитов по высоте, и «от нуля» она
        // уезжала бы в самый верх.
        let mut spanned = points.clone();
        for b in boxes {
            for corner in b.bounds.corners() {
                spanned.push((axis(corner, axis_x), axis(corner, axis_y)));
            }
        }
        let (min_a, max_a, min_b, max_b) = bounds(&spanned);
        let scale = 2.6_f32;
        let fits = ((max_a - min_a) / scale).max((max_b - min_b) / scale);
        let scale = (scale * (fits / (height as f32 - 2.0)).max(1.0)).max(scale);
        let center = (
            (min_a + max_a) * 0.5,
            (min_b + max_b) * 0.5,
        );

        // Коробка габаритов по хитбоксам: рамка, чтобы видеть масштаб.
        if let Some(bounds) = union(&boxes.iter().map(|b| b.bounds).collect::<Vec<_>>()) {
            let corners = [
                (axis(bounds.min, axis_x), axis(bounds.min, axis_y)),
                (axis(bounds.max, axis_x), axis(bounds.min, axis_y)),
                (axis(bounds.max, axis_x), axis(bounds.max, axis_y)),
                (axis(bounds.min, axis_x), axis(bounds.max, axis_y)),
            ];
            for pair in [[0, 1], [1, 2], [2, 3], [3, 0]] {
                plot_line(&mut grid, corners[pair[0]], corners[pair[1]], '.', center, scale);
            }
        }

        for (child, parent) in &edges {
            let (Some(a), Some(b)) = (points.get(*child), points.get(*parent)) else {
                continue;
            };
            plot_line(&mut grid, *a, *b, 'o', center, scale);
        }
        for point in &points {
            plot(&mut grid, *point, '#', center, scale);
        }

        println!("\n  {label}  (1 символ ≈ {scale:.1} юнита)");
        for row in &grid {
            let line: String = row.iter().collect();
            println!("  |{line}|");
        }
    }
}

fn axis(v: Vec3, index: usize) -> f32 {
    match index {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

fn union(boxes: &[Box3]) -> Option<Box3> {
    let mut iter = boxes.iter();
    let first = iter.next()?;
    let mut out = *first;
    for b in iter {
        out.min = Vec3::new(out.min.x.min(b.min.x), out.min.y.min(b.min.y), out.min.z.min(b.min.z));
        out.max = Vec3::new(out.max.x.max(b.max.x), out.max.y.max(b.max.y), out.max.z.max(b.max.z));
    }
    Some(out)
}

const ASCII_WIDTH: usize = 46;
const ASCII_HEIGHT: usize = 28;

fn plot(grid: &mut [Vec<char>], point: (f32, f32), glyph: char, center: (f32, f32), scale: f32) {
    let (x, y) = ascii_cell(point, center, scale);
    if let Some(row) = grid.get_mut(y) {
        if let Some(cell) = row.get_mut(x) {
            *cell = glyph;
        }
    }
}

fn plot_line(
    grid: &mut [Vec<char>],
    from: (f32, f32),
    to: (f32, f32),
    glyph: char,
    center: (f32, f32),
    scale: f32,
) {
    let (mut x0, mut y0) = ascii_cell(from, center, scale);
    let (x1, y1) = ascii_cell(to, center, scale);
    let dx = (x1 as i32 - x0 as i32).abs();
    let dy = -((y1 as i32 - y0 as i32).abs());
    let sx = if x0 < x1 { 1i32 } else { -1 };
    let sy = if y0 < y1 { 1i32 } else { -1 };
    let mut error = dx + dy;
    loop {
        if let Some(row) = grid.get_mut(y0) {
            if let Some(cell) = row.get_mut(x0) {
                if *cell == ' ' {
                    *cell = glyph;
                }
            }
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * error;
        if e2 >= dy {
            error += dy;
            x0 = (x0 as i32 + sx) as usize;
        }
        if e2 <= dx {
            error += dx;
            y0 = (y0 as i32 + sy) as usize;
        }
        if x0 >= ASCII_WIDTH || y0 >= ASCII_HEIGHT {
            break;
        }
    }
}

/// Точка модели в ячейку ASCII-сетки.
///
/// Масштаб общий на обе оси (пропорции сохраняются), центр — середина габаритов
/// модели; по вертикали `y` перевёрнут, потому что строки идут сверху вниз.
fn ascii_cell(point: (f32, f32), center: (f32, f32), scale: f32) -> (usize, usize) {
    let x = ((point.0 - center.0) / scale + ASCII_WIDTH as f32 * 0.5).round();
    let y = (ASCII_HEIGHT as f32 * 0.5 - (point.1 - center.1) / scale).round();
    (
        x.clamp(0.0, (ASCII_WIDTH - 1) as f32) as usize,
        y.clamp(0.0, (ASCII_HEIGHT - 1) as f32) as usize,
    )
}

/// Пишет BMP с тремя проекциями: спереди, сбоку, сверху.
fn write_preview(
    path: &Path,
    model: &StudioModel,
    poses: &[BoneMatrix],
    boxes: &[PlacedHitbox],
) -> std::io::Result<()> {
    let positions: Vec<Vec3> = poses.iter().map(|pose| pose.origin()).collect();
    let edges: Vec<(usize, usize)> = model.skeleton_edges().collect();

    let mut image = Canvas::new(1200, 448);
    image.clear([16, 18, 22]);

    let panels = [("спереди", 0usize, 2usize), ("сбоку", 1usize, 2usize), ("сверху", 0usize, 1usize)];
    let panel_width = image.width / panels.len();

    for (index, (_label, axis_x, axis_y)) in panels.iter().enumerate() {
        let area = Rect {
            x: index * panel_width + 10,
            y: 26,
            width: panel_width.saturating_sub(20),
            height: image.height.saturating_sub(44),
        };

        let mut points: Vec<(f32, f32)> = positions
            .iter()
            .map(|p| (axis(*p, *axis_x), axis(*p, *axis_y)))
            .collect();
        for b in boxes {
            for corner in b.bounds.corners() {
                points.push((axis(corner, *axis_x), axis(corner, *axis_y)));
            }
        }
        let (min_a, max_a, min_b, max_b) = bounds(&points);

        // Коробки хитбоксов.
        for b in boxes {
            let corners = b.bounds.corners();
            let color = group_color(b.group);
            for (from, to) in BOX_EDGES {
                let p0 = area.project(
                    axis(corners[from], *axis_x),
                    axis(corners[from], *axis_y),
                    min_a, max_a, min_b, max_b,
                );
                let p1 = area.project(
                    axis(corners[to], *axis_x),
                    axis(corners[to], *axis_y),
                    min_a, max_a, min_b, max_b,
                );
                image.line(p0, p1, color);
            }
        }

        // Скелет.
        for (child, parent) in &edges {
            let (Some(a), Some(b)) = (points.get(*child), points.get(*parent)) else {
                continue;
            };
            let p0 = area.project(a.0, a.1, min_a, max_a, min_b, max_b);
            let p1 = area.project(b.0, b.1, min_a, max_a, min_b, max_b);
            image.line(p0, p1, [120, 200, 255]);
        }
        for point in points.iter().take(positions.len()) {
            let p = area.project(point.0, point.1, min_a, max_a, min_b, max_b);
            image.dot(p, [255, 220, 120]);
        }

        image.frame(area, [60, 70, 84]);
    }

    image.save_bmp(path)
}

/// Цвет коробки по группе хитбоксов.
fn group_color(group: HitGroup) -> [u8; 3] {
    match group {
        HitGroup::Head => [255, 120, 120],
        HitGroup::Chest => [255, 200, 100],
        HitGroup::Stomach => [200, 220, 120],
        HitGroup::LeftArm | HitGroup::RightArm => [150, 200, 255],
        HitGroup::LeftLeg | HitGroup::RightLeg => [190, 160, 255],
        _ => [120, 130, 140],
    }
}

fn bounds(points: &[(f32, f32)]) -> (f32, f32, f32, f32) {
    let mut min_a = f32::INFINITY;
    let mut max_a = f32::NEG_INFINITY;
    let mut min_b = f32::INFINITY;
    let mut max_b = f32::NEG_INFINITY;
    for (a, b) in points {
        min_a = min_a.min(*a);
        max_a = max_a.max(*a);
        min_b = min_b.min(*b);
        max_b = max_b.max(*b);
    }
    if !min_a.is_finite() || !max_a.is_finite() || !min_b.is_finite() || !max_b.is_finite() {
        return (0.0, 1.0, 0.0, 1.0);
    }
    if (max_a - min_a) < 1.0 {
        max_a = min_a + 1.0;
    }
    if (max_b - min_b) < 1.0 {
        max_b = min_b + 1.0;
    }
    (min_a, max_a, min_b, max_b)
}

/// Прямоугольник панели, куда проецируются точки модели.
#[derive(Clone, Copy)]
struct Rect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

impl Rect {
    /// Точка модели в пикселях панели; `y` перевёрнут (в BMP растёт вниз).
    fn project(&self, a: f32, b: f32, min_a: f32, max_a: f32, min_b: f32, max_b: f32) -> (i32, i32) {
        let scale = (self.width as f32 / (max_a - min_a)).min(self.height as f32 / (max_b - min_b));
        let center_a = (min_a + max_a) * 0.5;
        let center_b = (min_b + max_b) * 0.5;
        let x = self.x as f32 + self.width as f32 * 0.5 + (a - center_a) * scale;
        let y = self.y as f32 + self.height as f32 * 0.5 - (b - center_b) * scale;
        (x.round() as i32, y.round() as i32)
    }
}

/// Простой RGB-холст с сохранением в 24-битный BMP.
struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<[u8; 3]>,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self { width, height, pixels: vec![[0, 0, 0]; width * height] }
    }

    fn clear(&mut self, color: [u8; 3]) {
        self.pixels.fill(color);
    }

    fn set(&mut self, x: i32, y: i32, color: [u8; 3]) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        self.pixels[y as usize * self.width + x as usize] = color;
    }

    fn dot(&mut self, p: (i32, i32), color: [u8; 3]) {
        for dy in -1..=1 {
            for dx in -1..=1 {
                self.set(p.0 + dx, p.1 + dy, color);
            }
        }
    }

    /// Отрезок по алгоритму Брезенхэма.
    fn line(&mut self, from: (i32, i32), to: (i32, i32), color: [u8; 3]) {
        let (mut x0, mut y0) = from;
        let (x1, y1) = to;
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        loop {
            self.set(x0, y0, color);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * error;
            if e2 >= dy {
                error += dy;
                x0 += sx;
            }
            if e2 <= dx {
                error += dx;
                y0 += sy;
            }
        }
    }

    fn frame(&mut self, area: Rect, color: [u8; 3]) {
        let x0 = area.x as i32;
        let y0 = area.y as i32;
        let x1 = (area.x + area.width) as i32;
        let y1 = (area.y + area.height) as i32;
        self.line((x0, y0), (x1, y0), color);
        self.line((x1, y0), (x1, y1), color);
        self.line((x1, y1), (x0, y1), color);
        self.line((x0, y1), (x0, y0), color);
    }

    fn save_bmp(&self, path: &Path) -> std::io::Result<()> {
        let row_size = (self.width * 3).div_ceil(4) * 4;
        let pixel_bytes = row_size * self.height;
        let file_size = 14 + 40 + pixel_bytes;

        let mut file = std::fs::File::create(path)?;
        file.write_all(b"BM")?;
        file.write_all(&(file_size as u32).to_le_bytes())?;
        file.write_all(&0u32.to_le_bytes())?;
        file.write_all(&54u32.to_le_bytes())?;
        file.write_all(&40u32.to_le_bytes())?;
        file.write_all(&(self.width as i32).to_le_bytes())?;
        file.write_all(&(self.height as i32).to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&24u16.to_le_bytes())?;
        file.write_all(&0u32.to_le_bytes())?;
        file.write_all(&(pixel_bytes as u32).to_le_bytes())?;
        file.write_all(&2835u32.to_le_bytes())?;
        file.write_all(&2835u32.to_le_bytes())?;
        file.write_all(&0u32.to_le_bytes())?;
        file.write_all(&0u32.to_le_bytes())?;

        let mut row = vec![0u8; row_size];
        for y in (0..self.height).rev() {
            for x in 0..self.width {
                let [r, g, b] = self.pixels[y * self.width + x];
                row[x * 3] = b;
                row[x * 3 + 1] = g;
                row[x * 3 + 2] = r;
            }
            for byte in row.iter_mut().skip(self.width * 3) {
                *byte = 0;
            }
            file.write_all(&row)?;
        }
        Ok(())
    }
}