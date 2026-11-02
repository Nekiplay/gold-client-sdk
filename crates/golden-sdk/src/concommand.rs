//! Свои консольные команды в движке Gold Client.
//!
//! Консоль здесь не HLSDK-шная: это `ConCommand` из Source (`tier1/convar.cpp`),
//! поэтому `Cmd_AddCommand` в `hw.dll` нет вовсе — регистрация идёт через
//! объект `ConCommandBase` и интерфейс `ICvar`. Всё ниже снято с самих бинарей,
//! адреса — RVA `hw.dll` (см. [`crate::offsets::hw`]).
//!
//! Раскладка объекта (проверено по инициализации `cmdlist` в `0x10008FF0`):
//!
//! | смещение | поле |
//! |---|---|
//! | `+0x00` | vftable (`ConCommand::vftable`, `0x20F6BC`) |
//! | `+0x04` | `m_pNext` — звено списка команд |
//! | `+0x08` | `m_bRegistered` (байт) |
//! | `+0x0C` | `m_pszName` |
//! | `+0x10` | `m_pszHelpString` |
//! | `+0x14` | `m_nFlags` |
//! | `+0x18` | не используется движком, копируется из шаблона |
//! | `+0x1C` | хелпер `0x19A50`, копируется из шаблона |
//! | `+0x20` | коллбек команды (`void (*)(const CCommand &)`) |
//! | `+0x24` | коллбек автодополнения (у движка — заглушка `0x19A40`) |
//! | `+0x28` | биты: `0x02` — «новый» коллбек `(const CCommand &)` |
//!
//! Регистрация = [`ConCommandBase::Add`](C_ADD): `g_pCVar->vtable[0]` →
//! `CCvar::RegisterConCommand` (`0xBC170`), которая ставит `m_bRegistered`,
//! кладёт объект в список команд `CCvar+0x28` и в хеш.
//! Снятие = [`ConCommandBase::Remove`](C_REMOVE): `CCvar::UnregisterConCommand`
//! (`0xBC310`) снимает флаг, вынимает из списка и из хеша.
//!
//! Объект команды живёт **не в образе DLL**, а в отдельной странице
//! `VirtualAlloc`: после выгрузки мода память образа освобождается, а страница
//! остаётся валидной. Поэтому даже если движок где-то закэшировал указатель,
//! он не попадёт в освобождённую память.

use core::ffi::{c_char, c_void};

use crate::offsets::hw;
use crate::process;

/// Коллбек команды: движок отдаёт аргументы разобранной строки.
///
/// Вызывается из игрового потока (там, где исполняется командный буфер), поэтому
/// внутри нельзя рисовать и брать блокировки движка.
pub type CommandCallback = unsafe extern "C" fn(args: *const CommandArgs);

/// Разобранная строка команды (`CCommand`).
///
/// Раскладка снята с обработчика `cmdlist` (`0x100B7230`): `argc` в `+0x00`,
/// указатели на аргументы — по `+0x1008 + i * 4` (то есть `argv[1]` в `+0x100C`).
#[repr(C)]
#[derive(Debug)]
pub struct CommandArgs {
    _private: [u8; 0],
}

impl CommandArgs {
    /// Сколько аргументов пришло, включая имя команды.
    pub fn argc(&self) -> i32 {
        // SAFETY: `self` — объект движка, `argc` лежит в его начале.
        unsafe { core::ptr::read_unaligned(self as *const Self as *const i32) }
    }

    /// Указатель на аргумент по индексу (`argv[0]` — имя команды).
    fn arg_ptr(&self, index: usize) -> *const c_char {
        // SAFETY: массив указателей аргументов начинается с `+0x1008` и живёт,
        // пока движок исполняет команду.
        unsafe {
            let slot = (self as *const Self as *const u8).add(0x1008 + index * 4);
            core::ptr::read_unaligned(slot as *const *const c_char)
        }
    }

    /// Аргумент по индексу как строка; `None`, если его нет.
    pub fn arg(&self, index: usize) -> Option<&str> {
        if index >= self.argc().max(0) as usize {
            return None;
        }
        let ptr = self.arg_ptr(index);
        if ptr.is_null() {
            return None;
        }
        // SAFETY: движок держит аргументы NUL-терминированными на всё время
        // исполнения команды.
        unsafe { core::ffi::CStr::from_ptr(ptr).to_str().ok() }
    }
}

/// Ошибка регистрации.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    /// `hw.dll` ещё не загружен — регистрировать некуда.
    NoEngine,
    /// Имя пустое или содержит нулевой байт.
    BadName,
    /// Не удалось выделить страницу под объект команды.
    NoMemory,
    /// `g_pCVar` ещё не создан: движок не дошёл до инициализации консоли.
    NoCVar,
}

impl core::fmt::Display for CommandError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Self::NoEngine => "hw.dll не загружен",
            Self::BadName => "пустое имя команды",
            Self::NoMemory => "не выделилась память под команду",
            Self::NoCVar => "g_pCVar ещё не создан",
        };
        f.write_str(text)
    }
}

/// `MEM_COMMIT | MEM_RESERVE`.
const MEM_COMMIT_RESERVE: u32 = 0x3000;
/// `PAGE_READWRITE`.
const PAGE_READWRITE: u32 = 0x04;
/// Сколько памяти просим под объект, имя и справку.
const BLOCK_SIZE: usize = 0x400;
/// Объект команды занимает 0x2C байт; остальное в блоке — под строки.
const OBJECT_SIZE: usize = 0x2C;
/// С этого смещения в блоке пишутся строки имени и справки.
const STRINGS_AT: usize = 0x40;

/// `ConCommandBase::Add()` — `0x10019FE0`: `g_pCVar->vtable[0](this)`.
const C_ADD: usize = 0x01_9FE0;
/// `ConCommandBase::Remove()` — `0x1001A000`: снимает регистрацию и вынимает
/// объект из глобального списка `ConCommandBase::s_pConCommandBases`.
const C_REMOVE: usize = 0x01_A000;

/// Своя консольная команда.
///
/// Живёт, пока объект не уничтожен: в [`Drop`] команда снимается с регистрации.
/// Память под неё намеренно не освобождается — страница `VirtualAlloc` переживёт
/// выгрузку DLL, так что «висячих» указателей у движка не останется.
#[derive(Debug)]
pub struct ConCommand {
    /// Начало страницы: объект, за ним строки.
    block: *mut u8,
    /// `ConCommandBase *` (тот же адрес, что `block`).
    object: *mut u8,
    registered: bool,
}

// SAFETY: команда не хранит ссылок на стек и не привязана к потоку.
unsafe impl Send for ConCommand {}

impl ConCommand {
    /// Регистрирует команду с именем `name`.
    ///
    /// # Safety
    /// `callback` должен оставаться валидным всё время жизни команды — то есть
    /// до [`ConCommand::remove`] или выгрузки DLL.
    pub unsafe fn install(
        name: &str,
        help: &str,
        callback: CommandCallback,
    ) -> Result<Self, CommandError> {
        if name.is_empty() || name.as_bytes().contains(&0) {
            return Err(CommandError::BadName);
        }
        let Some(base) = process::hw_base() else {
            return Err(CommandError::NoEngine);
        };
        // SAFETY: `g_pCVar` читается из .data движка тем же способом, что и
        // `ConCommandBase::Add`; нулевое значение значит «консоль ещё не встала».
        let cvar = unsafe { core::ptr::read_unaligned((base + hw::G_P_CVAR) as *const *mut u8) };
        if cvar.is_null() {
            return Err(CommandError::NoCVar);
        }

        // SAFETY: страница нужна нам навсегда — освобождать её после выгрузки
        // мода нельзя, иначе движок останется с указателем в никуда.
        let block = unsafe {
            windows_sys::Win32::System::Memory::VirtualAlloc(
                core::ptr::null(),
                BLOCK_SIZE,
                MEM_COMMIT_RESERVE,
                PAGE_READWRITE,
            ) as *mut u8
        };
        if block.is_null() {
            return Err(CommandError::NoMemory);
        }
        // SAFETY: блок наш и ещё не инициализирован.
        unsafe { core::ptr::write_bytes(block, 0, BLOCK_SIZE) };

        let mut command = Self {
            block,
            object: block,
            registered: false,
        };
        // SAFETY: блок наш, шаблон — статический объект движка.
        unsafe { command.fill(name, help, callback, base) };
        // SAFETY: объект заполнен по раскладке движка.
        unsafe { command.register(base) };
        command.registered = true;
        Ok(command)
    }

    /// Копирует заготовку из одноимённого объекта движка и подставляет свои поля.
    ///
    /// Копия нужна из-за полей `+0x18`/`+0x1C`/`+0x24`/`+0x28`: их смысл не
    /// документирован, но движок их читает, поэтому берём ровно те значения,
    /// что стоят у его собственной команды.
    ///
    /// # Safety
    /// `base` — адрес загруженного `hw.dll`, страница команды выделена.
    unsafe fn fill(&mut self, name: &str, help: &str, callback: CommandCallback, base: usize) {
        let template = (base + hw::CON_COMMAND_TEMPLATE) as *const u8;
        // SAFETY: `cmdlist` — статический объект движка, инициализируется при
        // старте консоли и живёт до конца процесса.
        unsafe { core::ptr::copy_nonoverlapping(template, self.object, OBJECT_SIZE) };

        // Строки кладём в тот же блок, сразу за объектом.
        // SAFETY: `STRINGS_AT` (0x40) + имя + справка укладываются в `BLOCK_SIZE`.
        let (name_at, help_at) = unsafe {
            let name_at = self.block.add(STRINGS_AT);
            let help_at = name_at.add(name.len() + 1);
            (name_at, help_at)
        };
        // SAFETY: в блоке 0x400 байт, строки заведомо короче.
        unsafe {
            core::ptr::copy_nonoverlapping(name.as_ptr(), name_at, name.len());
            *name_at.add(name.len()) = 0;
            core::ptr::copy_nonoverlapping(help.as_ptr(), help_at, help.len());
            *help_at.add(help.len()) = 0;
        }

        // SAFETY: поля объекта по раскладке из шапки модуля.
        unsafe {
            // Звено списка и флаг регистрации — свои, иначе движок решит, что
            // команда уже зарегистрирована, и попадёт в чужой список.
            core::ptr::write_unaligned(
                self.object.add(0x04) as *mut *mut u8,
                core::ptr::null_mut(),
            );
            *self.object.add(0x08) = 0;
            core::ptr::write_unaligned(self.object.add(0x0C) as *mut *const u8, name_at);
            core::ptr::write_unaligned(self.object.add(0x10) as *mut *const u8, help_at);
            core::ptr::write_unaligned(self.object.add(0x14) as *mut i32, 0);
            core::ptr::write_unaligned(self.object.add(0x20) as *mut usize, callback as usize);
        }
    }

    /// `ConCommandBase::Add()` — движок сам вызовет `CCvar::RegisterConCommand`.
    ///
    /// # Safety
    /// Объект заполнен, `hw.dll` загружен.
    unsafe fn register(&mut self, base: usize) {
        // SAFETY: соглашение `__fastcall`, `this` в `ECX` — так же, как объект
        // зовёт сам движок из конструктора.
        let add: unsafe extern "fastcall" fn(*mut u8) =
            unsafe { core::mem::transmute(base + C_ADD) };
        unsafe { add(self.object) };
    }

    /// Снимает команду с регистрации: движок перестаёт её находить.
    ///
    /// Память объекта остаётся занятой — она вне образа DLL и после выгрузки
    /// мода просто не используется.
    pub fn remove(&mut self) {
        if !self.registered {
            return;
        }
        self.registered = false;
        let Some(base) = process::hw_base() else {
            return;
        };
        // SAFETY: объект всё ещё зарегистрирован, `hw.dll` загружен.
        unsafe {
            let remove: unsafe extern "fastcall" fn(*mut u8) =
                core::mem::transmute(base + C_REMOVE);
            remove(self.object);
        }
    }

    /// Адрес объекта `ConCommandBase` в памяти игры.
    pub fn as_ptr(&self) -> *mut c_void {
        self.object.cast()
    }

    /// Зарегистрирована ли команда прямо сейчас.
    pub fn is_registered(&self) -> bool {
        self.registered
    }
}

impl Drop for ConCommand {
    fn drop(&mut self) {
        self.remove();
    }
}
