//! Стресс-прогон трассировки по настоящей карте: ищет лучи, на которых обход
//! дерева расходится или заметно тормозит. Нужны файлы игры, поэтому `ignored`.

use golden_sdk::bsp::Bsp;
use golden_sdk::math::Vec3;
use std::path::PathBuf;
use std::time::Instant;

/// Каталог игры: `GOLDCLIENT_DIR`, иначе стандартная установка.
fn game_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("GOLDCLIENT_DIR") {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            return dir;
        }
    }
    PathBuf::from(r"C:\Games\CS 1.6 GoldClient")
}

/// Файл карты в каталоге игры.
fn map_file(name: &str) -> Option<PathBuf> {
    let dir = game_dir();
    let direct = dir.join("maps").join(name);
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let candidate = entry.path().join("maps").join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[test]
#[ignore = "нужны файлы игры"]
fn stress_trace_real_map() {
    let path = map_file("de_dust2.bsp").expect("нет de_dust2.bsp");
    let bytes = std::fs::read(&path).expect("не прочитать карту");
    let bsp = Bsp::parse(&bytes).expect("карта не разобралась");
    println!(
        "карта: {} байт, ветвей {}, brush {}, моделей {}",
        bytes.len(),
        bsp.brushes().len(),
        bsp.brushes().len(),
        bsp.models().len()
    );

    // Простой детерминированный генератор: результат воспроизводим.
    let mut seed = 0x2545_F491_4F6C_DD1D_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f32 / (1u64 << 53) as f32
    };

    let mut slowest = (0.0_f64, 0_usize);
    let total = Instant::now();
    for index in 0..4000 {
        let origin = Vec3::new(
            -1200.0 + next() * 2400.0,
            -1200.0 + next() * 2400.0,
            -200.0 + next() * 600.0,
        );
        let direction = Vec3::new(
            -1.0 + next() * 2.0,
            -1.0 + next() * 2.0,
            -1.0 + next() * 2.0,
        );
        let end = origin + direction * 2048.0;
        let started = Instant::now();
        let trace = bsp.trace_all(origin, end);
        if trace.hit() {
            let _ = bsp.texture_at(trace.end, trace.normal);
        }
        let _ = bsp.point_contents(origin);
        let spent = started.elapsed().as_secs_f64();
        if spent > slowest.0 {
            slowest = (spent, index);
        }
        if index % 200 == 0 {
            println!(
                "луч {index}: {:.3} мке, худший {:.3}",
                spent * 1e6,
                slowest.0 * 1e6
            );
        }
    }
    println!(
        "всего {} лучей за {:.1} мс, худший луч {} — {:.3} мс",
        4000,
        total.elapsed().as_secs_f64() * 1e3,
        slowest.1,
        slowest.0 * 1e3
    );
    assert!(
        slowest.0 < 0.05,
        "луч {} считался {:.3} с — обход расходится",
        slowest.1,
        slowest.0
    );
}
