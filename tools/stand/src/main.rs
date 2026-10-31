//! Стенд: поднять локальную игру с ботами, загрузить DLL, показать лог.
//!
//! Ручной цикл «перезапусти игру → инжектни → глянь лог» превращается в одну
//! команду, так что проверку мода можно повторять сколько угодно.
//!
//! ```text
//! golden-stand run <путь к dll> [--exe путь\hl.exe] [--map de_dust2] [--bots 4]
//!                              [--seconds 20] [--keep] [--needle текст]
//!                              [--console "jointeam 2"]...
//! ```
//!
//! `--console` можно повторять: строки уходят в консоль игры по порядку и
//! повторяются каждые несколько секунд, пока мод не отчитается (или пока не
//! выйдет время). Так стенд сам сажает игрока на команду и не зависит от того,
//! когда именно догрузилась карта.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use golden_std::launch::{Game, GameConfig};

/// Имя лога рядом с DLL — его пишет `golden_sdk::log`.
const LOG_NAME: &str = "golden-client.log";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some("console") => console(&args[1..]),
        _ => {
            eprintln!(
                "usage: golden-stand run <dll> [--exe <hl.exe>] [--map <map>] [--bots <n>] \
                 [--seconds <n>] [--needle <text>] [--keep] [--console <cmd>]...\n\
                        golden-stand console <dll> <cmd>..."
            );
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> ExitCode {
    let Some(dll) = args.first().filter(|arg| !arg.starts_with("--")) else {
        eprintln!("не указан путь к DLL");
        return ExitCode::from(2);
    };
    let dll = PathBuf::from(dll);

    let mut config = match option(args, "--exe") {
        Some(exe) => GameConfig::new(PathBuf::from(exe)),
        None => match GameConfig::find() {
            Ok(config) => config,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(1);
            }
        },
    };
    if let Some(map) = option(args, "--map") {
        config = config.with_map(map);
    }
    if let Some(bots) = option(args, "--bots").and_then(|value| value.parse().ok()) {
        config = config.with_bots(bots);
    }
    let seconds: u64 = option(args, "--seconds")
        .and_then(|value| value.parse().ok())
        .unwrap_or(20);
    // По умолчанию ждём строку отчёта мода: «скелетов: N» пишет пример
    // скелетов, но у остальных модов такой строки нет — тогда ждём любую
    // строку, которую пишет `golden_sdk::log` при загрузке.
    let needle = option(args, "--needle").unwrap_or_else(|| "golden-client".to_string());
    let keep = args.iter().any(|arg| arg == "--keep");
    let console = all_options(args, "--console");

    let log = dll
        .parent()
        .map(|dir| dir.join(LOG_NAME))
        .unwrap_or_else(|| PathBuf::from(LOG_NAME));
    let _ = std::fs::remove_file(&log);

    println!("запуск: {} {}", config.exe.display(), config.command_line());
    let game = match Game::launch(&config) {
        Ok(game) => game,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };
    println!("pid {}", game.pid());

    if let Err(error) = game.wait_for_window(Duration::from_secs(60)) {
        eprintln!("{error}");
        return finish(&game, keep);
    }

    if let Err(error) = game.inject(&dll) {
        eprintln!("{error}");
        return finish(&game, keep);
    }
    println!("загружено: {}", dll.display());

    if let Err(error) = wait_ready(&game, &log, &needle, &console, Duration::from_secs(150)) {
        eprintln!("мод не отчитался: {error}");
    }

    println!("ждём {seconds} с…");
    std::thread::sleep(Duration::from_secs(seconds));
    print_tail(&log, 40);
    finish(&game, keep)
}

/// Ждёт строку `needle` в логе, попутно повторяя команды в консоль игры.
///
/// Повтор нужен из-за гонки: карта грузится десятки секунд, а команды вроде
/// `jointeam 2` до её загрузки просто неизвестны. Вместо угадывания задержки
/// команды отправляются снова и снова, пока мод не отчитается.
fn wait_ready(
    game: &Game,
    log: &Path,
    needle: &str,
    console: &[String],
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut next_send = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(log) {
            if text.contains(needle) {
                return Ok(());
            }
        }
        if !game.is_running() {
            return Err("игра завершилась".to_string());
        }
        if Instant::now() >= deadline {
            return Err(format!("за {timeout:?} не дождались «{needle}»"));
        }
        if !console.is_empty() && Instant::now() >= next_send {
            for command in console {
                let _ = game.console(command);
            }
            next_send = Instant::now() + Duration::from_secs(3);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn finish(game: &Game, keep: bool) -> ExitCode {
    if keep {
        println!("игра оставлена запущенной (--keep), pid {}", game.pid());
        return ExitCode::SUCCESS;
    }
    let killed = game.kill();
    println!("игра завершена: {killed}");
    ExitCode::SUCCESS
}

/// Кладёт команды в файл рядом с DLL — мод исполнит их сам.
///
/// Это надёжнее, чем печатать символы в окно: не нужен фокус, не мешает
/// открытая или закрытая консоль, и команда исполняется движком изнутри, как
/// набранная руками. См. `golden_std::commands`.
fn console(args: &[String]) -> ExitCode {
    let Some(dll) = args.first().filter(|arg| !arg.starts_with("--")) else {
        eprintln!("не указан путь к DLL (рядом с ней лежит файл команд)");
        return ExitCode::from(2);
    };
    let commands: Vec<&str> = args
        .iter()
        .skip(1)
        .filter(|arg| !arg.starts_with("--"))
        .map(String::as_str)
        .collect();
    if commands.is_empty() {
        eprintln!("не указано ни одной команды");
        return ExitCode::from(2);
    }
    let dll = PathBuf::from(dll);
    let dir = dll.parent().unwrap_or_else(|| Path::new("."));
    let path = golden_std::commands::path_next_to(dir);
    let text = format!("{}\n", commands.join("\n"));
    match std::fs::write(&path, text) {
        Ok(()) => {
            println!("команды записаны в {}: {}", path.display(), commands.join(" · "));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("не удалось записать {}: {error}", path.display());
            ExitCode::from(1)
        }
    }
}

fn option(args: &[String], name: &str) -> Option<String> {
    let position = args.iter().position(|arg| arg == name)?;
    args.get(position + 1).cloned()
}

/// Все значения повторяющегося флага: `--console a --console b`.
fn all_options(args: &[String], name: &str) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter(|(_, arg)| arg.as_str() == name)
        .filter_map(|(index, _)| args.get(index + 1).cloned())
        .collect()
}

fn print_tail(log: &Path, lines: usize) {
    let Ok(text) = std::fs::read_to_string(log) else {
        println!("лог {} не создан", log.display());
        return;
    };
    let all: Vec<&str> = text.lines().collect();
    let from = all.len().saturating_sub(lines);
    println!("--- {} (последние {}) ---", log.display(), all.len() - from);
    for line in &all[from..] {
        println!("{line}");
    }
}