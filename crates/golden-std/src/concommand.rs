//! Реестр своих консольных команд на стороне std.
//!
//! SDK умеет зарегистрировать команду ([`golden_sdk::concommand`]), но держать
//! их в примере неудобно: команда должна исчезнуть из консоли ровно тогда,
//! когда мод выгружается. Поэтому список живёт здесь, а [`shutdown`] зовётся из
//! [`crate::unload`] — до снятия перехватов и до освобождения модуля.
//!
//! ```no_run
//! use golden_std::concommand::{self, CommandArgs};
//!
//! unsafe extern "C" fn hello(args: *const CommandArgs) {
//!     let args = unsafe { &*args };
//!     golden_sdk::log!("привет из консоли: {:?}", args.arg(0));
//! }
//!
//! # fn main() {
//! concommand::register("gc_hello", "поздороваться", hello).ok();
//! # }
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use golden_sdk::concommand::{CommandError, ConCommand};
use golden_sdk::log;
use golden_sdk::process;

pub use golden_sdk::concommand::{CommandArgs, CommandCallback};

/// `Con_Printf` в `hw.dll`: `cdecl`, `(fmt, ...)`.
const CON_PRINTF_OFFSET: usize = 0xBA780;

/// Приёмник готовой строки консоли в `hw.dll`: `__fastcall(text)`.
///
/// Через него проходит всё, что печатается в консоль и в `qconsole.log`,
/// поэтому фильтр эха стоит именно здесь: функция принимает одну строку и не
/// использует списки аргументов, так что её можно перехватить безопасно.
const CON_SINK_OFFSET: usize = 0xB2D80;

/// Имена команд, строку выполнения которых в консоли показывать не нужно.
static HIDDEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Поставлен ли фильтр эха.
static FILTER: AtomicBool = AtomicBool::new(false);

/// Зарегистрированные команды. Держим их здесь, чтобы объекты не умерли раньше
/// движка: `Drop` снимает команду с регистрации.
static COMMANDS: Mutex<Vec<Owned>> = Mutex::new(Vec::new());

/// Своя команда: имя для поиска и сам объект движка.
struct Owned {
    name: String,
    /// Объект живёт ровно столько, сколько запись: `Drop` снимает регистрацию.
    #[allow(dead_code)]
    command: ConCommand,
}

// SAFETY: объект команды живёт в памяти процесса игры и не привязан к потоку.
unsafe impl Send for Owned {}

/// Регистрирует команду `name` с описанием `help`.
///
/// `callback` исполняется в игровом потоке, поэтому внутри нельзя рисовать и
/// брать блокировки движка.
pub fn register(
    name: &str,
    help: &str,
    callback: CommandCallback,
) -> Result<(), CommandError> {
    // SAFETY: коллбек обязан жить, пока жива команда; он у нас `'static`, а
    // команду мы снимаем при выгрузке.
    let command = unsafe { ConCommand::install(name, help, callback)? };
    match COMMANDS.lock() {
        Ok(mut list) => {
            log::line(&format!("команда: зарегистрирована `{name}`"));
            list.push(Owned {
                name: name.to_owned(),
                command,
            });
            Ok(())
        }
        Err(_) => {
            // Замок отравлен: команда уже в движке, но не в реестре. Снимаем
            // сами, иначе она переживёт выгрузку.
            let mut command = command;
            command.remove();
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- вывод ---

/// Адрес функции `hw.dll` по её смещению.
fn hw_address(offset: usize) -> Option<usize> {
    process::hw_base().map(|base| base + offset)
}

/// Пишет строку в консоль игры.
///
/// Сначала пробует консоль Gold Client (она живёт в `GameUI.dll` и не видит
/// `Con_Printf`), затем — обычную печать движка. Перевод строки добавляется
/// сам, поэтому звать можно по разу на строку. Возвращает `false`, если не
/// вышло ни то, ни другое.
pub fn print(text: &str) -> bool {
    if golden_sdk::console::print(text) {
        return true;
    }
    let Some(target) = hw_address(CON_PRINTF_OFFSET) else {
        return false;
    };
    // Управляющие символы заменяем: `Con_Printf` читает строку до нуля, а
    // переводы строк он и так добавляет.
    let mut line: Vec<u8> = text
        .bytes()
        .map(|byte| if byte == 0 || byte == b'\n' || byte == b'\r' { b' ' } else { byte })
        .collect();
    line.push(0);
    // SAFETY: адрес проверен по базе модуля, формат константный, строка
    // завершена нулём и живёт до конца вызова.
    let con_printf: unsafe extern "C" fn(*const u8, *const u8) =
        unsafe { core::mem::transmute(target) };
    unsafe { con_printf(b"%s\n\0".as_ptr(), line.as_ptr()) };
    true
}

// ------------------------------------------------------------- фильтр эха ---

/// Строка консоли, которую нужно проглотить: `] gc_unload лишнее`.
fn hidden_line(line: &str) -> bool {
    let line = line.trim_start();
    let line = line.strip_prefix(']').unwrap_or(line).trim_start();
    let Some(first) = line.split_whitespace().next() else {
        return false;
    };
    match HIDDEN.lock() {
        Ok(list) => list.iter().any(|name| name.eq_ignore_ascii_case(first)),
        // Отравленный замок лучше пропустить: скрыть строку не так важно, как
        // не потерять остальной вывод игры.
        Err(_) => false,
    }
}

// Перехват приёмника консоли. Одна строка в аргументе, поэтому пересылка
// оригинала безопасна.
crate::hook! {
    /// Прячет из консоли строку выполнения скрытых команд.
    unsafe extern "fastcall" fn on_console_line(text: *const u8)
        original: console_line_original
    {
        if !text.is_null() {
            // SAFETY: консоль всегда отдаёт завершённую нулём строку.
            if let Some(line) = process::read_cstr(text as usize, 512) {
                if hidden_line(&line) {
                    return;
                }
            }
        }
        // SAFETY: оригинал получен из `hooks::install` и имеет ту же сигнатуру.
        // Если выгрузка уже сняла перехват, вызывать некуда: строку просто не
        // печатаем. Паниковать здесь нельзя — мы на стеке движка, и паника
        // через FFI роняет игру.
        match crate::hooks::original_of(on_console_line as *const ()) {
            Some(address) => {
                let original: unsafe extern "fastcall" fn(*const u8) =
                    unsafe { core::mem::transmute(address) };
                unsafe { original(text) };
            }
            None => {}
        }
    }
}

/// Ставит фильтр эха. Повторные вызовы ничего не делают.
fn ensure_filter() -> bool {
    if FILTER.load(Ordering::Acquire) {
        return true;
    }
    let Some(target) = hw_address(CON_SINK_OFFSET) else {
        return false;
    };
    // SAFETY: сигнатура совпадает с приёмником консоли, функция живёт до
    // выгрузки модуля.
    let installed = unsafe { crate::hooks::install(target, on_console_line as *const ()) };
    match installed {
        Ok(_) => {
            FILTER.store(true, Ordering::Release);
            log::line("консоль: фильтр эха установлен");
            true
        }
        Err(error) => {
            log::line(&format!("консоль: фильтр эха не встал: {error}"));
            false
        }
    }
}

/// Убирает строку выполнения команды из консоли.
///
/// Движок печатает `] имя` для любой набранной строки — это история ввода
/// консоли, а не вывод команды. В `hw.dll` такая строка идёт через
/// `Con_Printf`, и её глушит фильтр. У Gold Client консоль рисуется на стороне
/// клиента и до `hw.dll` не доходит: там эха нет только при запуске не из
/// консоли — [`Command::bind`].
pub fn hide_echo(name: &str) -> bool {
    let prepared = {
        match HIDDEN.lock() {
            Ok(mut list) => {
                if !list.iter().any(|hidden| hidden.eq_ignore_ascii_case(name)) {
                    list.push(name.to_owned());
                }
                true
            }
            Err(poisoned) => {
                let mut list = poisoned.into_inner();
                if !list.iter().any(|hidden| hidden.eq_ignore_ascii_case(name)) {
                    list.push(name.to_owned());
                }
                true
            }
        }
    };
    prepared && ensure_filter()
}

/// Снимает пометку «не показывать строку выполнения».
pub fn show_echo(name: &str) -> bool {
    match HIDDEN.lock() {
        Ok(mut list) => {
            if let Some(index) = list.iter().position(|hidden| hidden.eq_ignore_ascii_case(name)) {
                list.remove(index);
                return true;
            }
            false
        }
        Err(_) => false,
    }
}

// ------------------------------------------------------------ конструктор ---

/// Описание своей команды: имя, справка, скрывать ли строку выполнения.
///
/// ```no_run
/// use golden_std::concommand::{self, CommandArgs};
///
/// unsafe extern "C" fn status(_args: *const CommandArgs) {
///     concommand::print("мод работает");
/// }
///
/// # fn main() {
/// concommand::Command::new("gc_status", status)
///     .help("состояние мода")
///     .hidden()
///     .register()
///     .ok();
/// # }
/// ```
pub struct Command {
    name: String,
    help: String,
    hidden: bool,
    callback: CommandCallback,
}

impl Command {
    /// Новая команда `name` с коллбеком `callback`.
    pub fn new(name: &str, callback: CommandCallback) -> Self {
        Self {
            name: name.to_owned(),
            help: String::new(),
            hidden: false,
            callback,
        }
    }

    /// Описание для `cmdlist`.
    pub fn help(mut self, help: &str) -> Self {
        self.help = help.to_owned();
        self
    }

    /// Не показывать в консоли строку выполнения команды.
    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    /// Регистрирует команду в движке.
    pub fn register(self) -> Result<(), CommandError> {
        if self.hidden {
            hide_echo(&self.name);
        }
        register(&self.name, &self.help, self.callback)
    }

    /// Как [`register`](Self::register), но ещё и вешает команду на клавишу.
    ///
    /// Это единственный способ позвать команду вообще без строки в консоли:
    /// нажатие клавиши идёт мимо ввода консоли, поэтому и эха нет.
    pub fn bind(self, key: &str) -> Result<(), CommandError> {
        let name = self.name.clone();
        self.register()?;
        let line = format!("bind {key} {name}");
        golden_sdk::engine::run_command(&line);
        Ok(())
    }
}

/// Сколько команд зарегистрировано.
pub fn count() -> usize {
    COMMANDS.lock().map(|list| list.len()).unwrap_or(0)
}

/// Снимает команду по имени. Возвращает `false`, если такой не было.
pub fn unregister(name: &str) -> bool {
    let mut list = match COMMANDS.lock() {
        Ok(list) => list,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(index) = list.iter().position(|owned| owned.name == name) {
        // `Drop` у `Owned` -> `ConCommand::remove`.
        list.remove(index);
        log::line(&format!("команда: снята `{name}`"));
        return true;
    }
    false
}

/// Имена зарегистрированных команд.
pub fn names() -> Vec<String> {
    COMMANDS
        .lock()
        .map(|list| list.iter().map(|owned| owned.name.clone()).collect())
        .unwrap_or_default()
}

/// Снимает все команды мода с регистрации в движке.
///
/// Возвращает число снятых. Вызывается из [`crate::unload`] до освобождения
/// модуля: после этого в консоли ни одной нашей команды не остаётся, и движок
/// уже не может прыгнуть в код выгружаемой DLL.
pub fn shutdown() -> usize {
    let pending = match COMMANDS.lock() {
        Ok(mut list) => std::mem::take(&mut *list),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };
    let count = pending.len();
    // Снимаем в обратном порядке регистрации — как и остальные очистки.
    for owned in pending.into_iter().rev() {
        drop(owned);
    }
    if count > 0 {
        log::line(&format!("команда: снято при выгрузке {count}"));
    }
    // Пометки скрытых строк больше не нужны без команд.
    if let Ok(mut hidden) = HIDDEN.lock() {
        hidden.clear();
    }
    count
}