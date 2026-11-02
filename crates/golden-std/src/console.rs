//! Консоль Gold Client: скрытие эха и свой вывод.
//!
//! Консоль Gold Client — клиентская: историю держит `GameUI.dll`, и набранная
//! команда попадает в неё как строка `] имя`. Печать `hw.dll` про это ничего
//! не знает, поэтому фильтр на стороне движка эхо не ловит.
//!
//! Здесь мы перехватываем сам приёмник строк консоли и не пускаем в него те,
//! что просили скрыть. Всё остальное — вывод движка, сообщения сервера, отчёты
//! мода — проходит без изменений.
//!
//! ```ignore
//! // один раз при старте мода
//! golden_std::console::install("unload_esp", "unload_weapons");
//! ```
//!
//! ## Свой вывод
//!
//! `Con_Printf` из `hw.dll` до этой консоли не доходит, поэтому [`Builder`]
//! пишет прямо в историю `GameUI.dll` и умеет красить отдельные куски строки:
//!
//! ```ignore
//! use golden_std::console::{self, Color};
//!
//! console::builder()
//!     .title("состояние мода")
//!     .kv("игроков", "3")
//!     .text("загрузка: ")
//!     .colored(Color::GREEN, "готово")
//!     .text("\n")
//!     .send();
//! ```
//!
//! Перехват снимается сам при [`crate::unload`] — как и все остальные.

use std::ffi::c_void;
use std::sync::Mutex;

use golden_sdk::{console, log};

pub use golden_sdk::console::{Color, Writer};

use crate::hooks;

/// Имена, чьи строки не показываем.
static HIDDEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Сколько строк уже отфильтровано (для отчёта `status`).
static DROPPED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Перехват ставится один раз.
static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ставит перехват приёмника строк и скрывает перечисленные команды.
///
/// Возвращает `true`, если перехват стоит. `false` — если консоль в этой
/// сборке игры не нашлась (например, `GameUI.dll` ещё не загружен); мод при
/// этом работает, просто эхо остаётся видимым.
pub fn install(names: &[&str]) -> bool {
    for name in names {
        hide(name);
    }
    if INSTALLED.load(std::sync::atomic::Ordering::Acquire) {
        return true;
    }
    let Some(target) = console::add_line() else {
        log::line("консоль: приёмник строк не найден, эхо останется видимым");
        return false;
    };
    // SAFETY: адрес приёмника проверен по прологу, сигнатура совпадает.
    match unsafe { hooks::install(target, on_line as *const ()) } {
        Ok(hook) => {
            INSTALLED.store(true, std::sync::atomic::Ordering::Release);
            // Приёмник теперь пропатчен: свой вывод должен идти в трамплин,
            // иначе `Writer` либо не откроется, либо попадёт в наш же фильтр.
            console::set_receiver_original(hook.original());
            log::line(&format!(
                "консоль: перехвачен приёмник строк {target:#x} (оригинал {:#x})",
                hook.original()
            ));
            true
        }
        Err(error) => {
            log::line(&format!("консоль: перехват не встал: {error:?}"));
            false
        }
    }
}

/// Добавляет имя команды в список скрытых.
pub fn hide(name: &str) {
    if let Ok(mut list) = HIDDEN.lock() {
        if !list.iter().any(|entry| entry == name) {
            list.push(name.to_string());
        }
    }
}

/// Убирает имя из списка скрытых.
pub fn show(name: &str) {
    if let Ok(mut list) = HIDDEN.lock() {
        list.retain(|entry| entry != name);
    }
}

/// Сколько строк уже не пустили в консоль.
pub fn dropped() -> u32 {
    DROPPED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Нужно ли скрыть эту строку консоли.
///
/// Скрываем и саму команду, и эхо `] команда` — с аргументами или без.
///
/// Клиент печатает эхо двумя строками: сначала голый префикс `] `, отдельным
/// вызовом, потом сам текст. Поэтому пустой префикс тоже считается эхом —
/// но только пока список скрытых имён не пуст, иначе мод глушил бы чужие `] `.
pub fn hidden(text: &str) -> bool {
    let Ok(list) = HIDDEN.lock() else {
        return false;
    };
    let trimmed = text.trim_start();
    let body = trimmed.strip_prefix(']').unwrap_or(trimmed).trim_start();
    let Some(first) = body.split_whitespace().next() else {
        // Пустое тело после `]` — это префикс эха, а не чужой вывод.
        return !list.is_empty();
    };
    list.iter()
        .any(|name| first.eq_ignore_ascii_case(name.as_str()))
}

// Перехват приёмника строк консоли.
//
// Приёмник получает строку в ANSI (это `__thiscall(this, const char *)`,
// сам забирает аргумент со стека) и переводит её в UTF-16.
crate::hook! {
    unsafe extern "thiscall" fn on_line(this: *mut c_void, text: *const u8)
        original: on_line_original
    {
        // SAFETY: `text` — строка приёмника, параметры совпадают с оригиналом.
        let line = unsafe { console::line_bytes(text) };
        if let Some(bytes) = line {
            let text = String::from_utf8_lossy(bytes);
            if hidden(&text) {
                DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return;
            }
        }
        // SAFETY: вызываем оригинал с теми же аргументами.
        unsafe { on_line_original()(this, text) }
    }
}

// ------------------------------------------------------------------ вывод ---

/// Один фрагмент строки со своим цветом.
#[derive(Debug, Clone)]
struct Part {
    color: Option<Color>,
    text: String,
}

/// Собирает сообщение для консоли Gold Client.
///
/// Билдер копит фрагменты и печатает их одним [`send`](Self::send): сначала
/// ставит цвет фрагмента, потом дописывает текст. Цвет действует только на
/// свои фрагменты; следующий фрагмент без цвета пишется обычным цветом консоли.
///
/// ```ignore
/// use golden_std::console::{self, Color};
///
/// console::builder()
///     .title("мод")
///     .text("загружено ")
///     .colored(Color::GREEN, "успешно")
///     .text("\n")
///     .send();
/// ```
#[derive(Debug, Clone, Default)]
pub struct Builder {
    parts: Vec<Part>,
    current: Option<Color>,
}

impl Builder {
    /// Пустое сообщение.
    pub fn new() -> Self {
        Self::default()
    }

    /// Делает `color` цветом последующего текста.
    pub fn color(mut self, color: Color) -> Self {
        self.current = Some(color);
        self
    }

    /// Возвращает обычный цвет консоли для последующего текста.
    pub fn plain(mut self) -> Self {
        self.current = None;
        self
    }

    /// Дописывает текст текущим цветом.
    pub fn text(mut self, text: impl AsRef<str>) -> Self {
        self.push(text.as_ref());
        self
    }

    /// Дописывает текст заданным цветом, не меняя цвет следующих фрагментов.
    pub fn colored(mut self, color: Color, text: impl AsRef<str>) -> Self {
        let previous = self.current;
        self.current = Some(color);
        self.push(text.as_ref());
        self.current = previous;
        self
    }

    /// Дописывает текст и перевод строки.
    pub fn line(self, text: impl AsRef<str>) -> Self {
        self.text(text).text("\n")
    }

    /// Пустая строка.
    pub fn blank(self) -> Self {
        self.text("\n")
    }

    /// Заголовок: золотая строка с переводом строки.
    pub fn title(self, text: impl AsRef<str>) -> Self {
        self.colored(Color::GOLD, text).text("\n")
    }

    /// Строка успеха: зелёная.
    pub fn ok(self, text: impl AsRef<str>) -> Self {
        self.colored(Color::GREEN, text).text("\n")
    }

    /// Предупреждение: жёлтое.
    pub fn warn(self, text: impl AsRef<str>) -> Self {
        self.colored(Color::YELLOW, text).text("\n")
    }

    /// Ошибка: красная.
    pub fn error(self, text: impl AsRef<str>) -> Self {
        self.colored(Color::RED, text).text("\n")
    }

    /// Пара `ключ: значение`: ключ серый, значение белое.
    pub fn kv(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        let previous = self.current;
        self.current = Some(Color::GREY);
        self.push(key.as_ref());
        self.current = previous;
        self.push(": ");
        self.current = Some(Color::WHITE);
        self.push(value.as_ref());
        self.current = previous;
        self.push("\n");
        self
    }

    /// Печатает собранное сообщение.
    ///
    /// Возвращает `true`, если консоль приняла все фрагменты. Если консоли
    /// клиента ещё нет (например, рано при старте), сообщение уходит через
    /// обычную печать движка — уже без цветов.
    pub fn send(self) -> bool {
        let Some(writer) = Writer::open() else {
            log::line("консоль: история Gold Client не открылась — вывод идёт через Con_Printf");
            let mut plain = String::new();
            for part in &self.parts {
                plain.push_str(&part.text);
            }
            return crate::concommand::print(&plain);
        };

        let mut written = true;
        for part in &self.parts {
            let color = part.color.unwrap_or(writer.default_color());
            written &= writer.set_color(color);
            written &= writer.write(&part.text);
        }
        // Возвращаем обычный цвет: следующая строка движка не должна
        // унаследовать цвет нашего последнего фрагмента.
        writer.set_color(writer.default_color());
        written
    }

    fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.parts.last_mut() {
            if last.color == self.current {
                last.text.push_str(text);
                return;
            }
        }
        self.parts.push(Part {
            color: self.current,
            text: text.to_string(),
        });
    }
}

/// Новое сообщение для консоли.
pub fn builder() -> Builder {
    Builder::new()
}

/// Печатает строку обычным цветом консоли.
pub fn print(text: &str) -> bool {
    console::print(text)
}

/// Печатает строку заданным цветом.
pub fn print_colored(color: Color, text: &str) -> bool {
    console::print_colored(color, text)
}