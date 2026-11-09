//! Команды в движок из внешнего файла — чтобы внешние утилиты и скрипты
//! могли управлять игрой без фокуса её окна.
//!
//! Мод читает файл рядом с DLL и выполняет из него строки как консольные
//! команды движка ([`golden_sdk::engine::run_command`]). Файл после выполнения
//! удаляется, поэтому один и тот же путь можно использовать сколько угодно раз.
//!
//! Зачем это нужно: автоматическая проверка мода. После инъекции DLL внешнему
//! скрипту надо ещё сказать игре что-то вроде `bot_stop 1` или `snapshot`.
//! Писать символы в окно через `PostMessage` ненадёжно: консоль может быть
//! закрыта, фокус может быть не у игры, а часть команд до загрузки карты
//! вообще не существует. Файл же исполняется движком изнутри, ровно как
//! будто строку набрали руками.
//!
//! ```no_run
//! # let dll_dir = std::path::Path::new(".");
//! // в стартовом потоке мода:
//! golden_std::commands::watch(golden_std::commands::path_next_to(dll_dir));
//! ```
//!
//! Формат файла — обычный текст, одна команда на строку, `#` в начале строки
//! — комментарий (движок всё равно выругался бы на такую строку).

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

/// Имя файла команд рядом с DLL.
pub const FILE_NAME: &str = "golden-commands.txt";

/// Как часто проверять наличие файла.
pub const POLL: Duration = Duration::from_millis(200);

/// Путь к файлу команд в том же каталоге, что и DLL.
pub fn path_next_to(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// Запускает фоновый поток, который исполняет команды из файла.
///
/// Возвращает управление сразу: поток живёт до конца процесса. Ошибки чтения
/// файла и записи в лог не прерывают цикл — иначе одна неудачная запись
/// выключила бы управление игрой до перезапуска.
pub fn watch(path: PathBuf) {
    let _ = thread::Builder::new()
        .name("golden-commands".to_string())
        .spawn(move || {
            // Поток принадлежит DLL: при выгрузке он обязан выйти, иначе будет
            // исполнять освобождённый код.
            let _token = golden_sdk::module::ThreadToken::new();
            // Файл, оставшийся от прошлого запуска, не исполняем: в нём могла
            // лежать `!unload` из прошлой проверки, и мод выключил бы себя
            // через ноль секунд после загрузки.
            if path.exists() {
                let _ = std::fs::remove_file(&path);
                golden_sdk::log!("команды: старый {} удалён, ждём новые", path.display());
            }
            while !golden_sdk::module::should_stop() {
                thread::sleep(POLL);
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                // Удаляем до выполнения: если команда уронит игру, файл не будет
                // исполняться повторно при следующем запуске.
                let _ = std::fs::remove_file(&path);
                for line in text.lines() {
                    // Файл мог быть записан с BOM (PowerShell `Set-Content`,
                    // блокнот): без снятия BOM `!unload` не совпадёт ни с чем.
                    let command = line.trim().trim_start_matches('\u{feff}').trim();
                    if command.is_empty() || command.starts_with('#') {
                        continue;
                    }
                    if let Some(action) = command.strip_prefix('!') {
                        // Выгрузка запускается из этого потока:
                        // он не должен ждать сам себя в `wait_threads`.
                        let leaves = action.starts_with("unload") || action.starts_with("free");
                        if leaves {
                            drop(_token);
                            run_local(action);
                            return;
                        }
                        run_local(action);
                        continue;
                    }
                    golden_sdk::log!("команда из файла: {command}");
                    golden_sdk::engine::run_command(command);
                }
            }
        });
}

/// Локальные команды мода: строки с `!` не уходят в движок.
///
/// * `!unload` — выгрузить DLL (без аргумента — с паузой по умолчанию);
/// * `!unload 500` — выгрузить через 500 мс;
/// * `!ping` — ответить в лог, что мод жив.
///
/// Нужны стенду и ручной проверке: выгрузку иначе не запустить, не нажимая
/// клавишу в окне игры.
fn run_local(action: &str) {
    let mut parts = action.split_whitespace();
    match parts.next() {
        Some("unload") => {
            let delay = parts
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(golden_sdk::module::DEFAULT_DELAY_MS);
            golden_sdk::log!("локальная команда: выгрузка через {delay} мс");
            crate::unload::unload_after(delay);
        }
        Some("ping") => golden_sdk::log!("локальная команда: ping — мод жив"),
        Some("detach") => {
            golden_sdk::log!("локальная команда: detach");
            crate::unload::detach_only();
            return;
        }
        Some("free") => {
            let delay = parts
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(golden_sdk::module::DEFAULT_DELAY_MS);
            golden_sdk::log!("локальная команда: освобождение DLL через {delay} мс");
            crate::unload::defer_free(delay);
            return;
        }
        Some(other) => golden_sdk::log!("локальная команда: неизвестная «{other}»"),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_is_next_to_dll() {
        let path = path_next_to(Path::new("C:\\mods"));
        assert_eq!(path, PathBuf::from("C:\\mods").join(FILE_NAME));
    }
}
