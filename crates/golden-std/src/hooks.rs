//! Реестр перехватов и подмен интерфейса, которые снимает сам `unload`.
//!
//! Правило простое: мод не держит хуки у себя. Ставит через [`install`] или
//! [`interface`] — std запоминает и снимает их сам в [`detach_all`], который
//! вызывается из [`crate::unload`]. Так выгрузка не зависит от того, вспомнил ли
//! пример про свой `shutdown`: живой хук на освобождённую страницу — гарантированный
//! краш движка.
//!
//! Обычный хук удобнее объявлять макросом [`crate::hook!`] — он берёт на себя
//! ABI и трамплин, а установка остаётся одной строкой:
//!
//! ```ignore
//! use std::ffi::c_void;
//! use golden_std::hooks;
//!
//! // Способ 1: свой extern-функцией.
//! unsafe extern "C" fn on_call() {}
//! let _ = hooks::install(0x1000_1234, on_call as *const ());
//!
//! // Способ 2: макросом — он объявляет и хук, и аксессор к оригиналу.
//! golden_std::hook! {
//!     unsafe extern "thiscall" fn on_bones(this: *mut c_void)
//!         original: on_bones_original
//!         -> i32
//!     {
//!         unsafe { on_bones_original()(this) }
//!     }
//! }
//! let _ = hooks::install(0x1000_5678, on_bones as *const ());
//! ```
//!
//! Порядок снятия задан в [`crate::unload`]: сначала перехваты, потом команды,
//! потом ввод, потом очистки SDK — так к моменту `FreeLibrary` в игре не
//! остаётся ни одной точки входа в нашу DLL.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use golden_sdk::hook::{Detour, HookError, SwappedInterface};
use golden_sdk::log;
use golden_sdk::renderer::StudioInterface;

pub use golden_sdk::hook::HookError as Error;

/// Установленный перехват.
///
/// Хранит адрес оригинала (трамплина), чтобы мод мог вызвать исходную функцию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hook {
    original: usize,
}

impl Hook {
    /// Адрес оригинальной функции: вызывайте его из своего перехвата.
    pub fn original(&self) -> usize {
        self.original
    }
}

/// Что снимает реестр.
enum Managed {
    /// Inline-хук: перед выгрузкой нужно выключить патч, а трамплин оставить
    /// живым (в него мог войти поток игры).
    Detour(Detour),
    /// Подмена `g_pStudioInterface`: `Drop` вернёт движку указатель.
    Interface(Box<SwappedInterface>),
}

// SAFETY: и детур, и подмена — объекты процесса игры, они не привязаны к потоку.
unsafe impl Send for Managed {}

static MANAGED: Mutex<Vec<Managed>> = Mutex::new(Vec::new());

/// Сколько перехватов реестр помнит для быстрого поиска оригинала.
///
/// Массив вместо карты: `original_of` зовётся из горячего пути отрисовки, а
/// хуков у мода единицы.
const SLOTS: usize = 16;

/// Адреса наших хуков.
static HOOKS: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
/// Адреса их трамплинов: `ORIGINALS[i]` — оригинал для `HOOKS[i]`.
static ORIGINALS: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];

/// Запоминает пару «хук → оригинал» для [`original_of`].
fn remember_original(hook: *const (), original: usize) {
    for index in 0..SLOTS {
        // Тот же хук ставим второй раз (сняли и поставили заново): адрес
        // трамплина новый, старый мог остаться от прошлой установки.
        if HOOKS[index].load(Ordering::Acquire) == hook as usize {
            ORIGINALS[index].store(original, Ordering::Release);
            return;
        }
        if HOOKS[index]
            .compare_exchange(0, hook as usize, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            ORIGINALS[index].store(original, Ordering::Release);
            return;
        }
    }
    log::line("перехват: реестр оригиналов переполнен, макрос не найдёт трамплин");
}

/// Забывает все пары «хук → оригинал».
#[allow(dead_code)]
fn forget_originals() {
    for index in 0..SLOTS {
        ORIGINALS[index].store(0, Ordering::Release);
        HOOKS[index].store(0, Ordering::Release);
    }
}

/// Ставит inline-хук на `target` и включает его.
///
/// `hook` — адрес вашей функции (`мое_имя as *const ()`), `target` — адрес
/// подменяемой функции игры.
///
/// Снимается автоматически при [`crate::unload::unload`]: выключать и
/// освобождать вручную ничего не нужно.
///
/// # Safety
/// `hook` должен указывать на функцию с сигнатурой и ABI, совпадающими с
/// `target`, и жить до выгрузки модуля.
pub unsafe fn install(target: usize, hook: *const ()) -> Result<Hook, Error> {
    // SAFETY: контракт функции.
    let installed = unsafe { Detour::new(target, hook) }?;
    installed.enable()?;
    let original = installed.trampoline() as usize;
    match MANAGED.lock() {
        Ok(mut list) => {
            remember_original(hook, original);
            log::line(&format!(
                "перехват: {target:#x} -> оригинал {original:#x}, всего {}",
                list.len() + 1
            ));
            list.push(Managed::Detour(installed));
            Ok(Hook { original })
        }
        Err(poisoned) => {
            drop(poisoned.into_inner());
            // Реестр отравлен: снимаем сразу, чтобы не оставить бесхозный хук.
            installed.disable().ok();
            Err(HookError::BadTarget(target))
        }
    }
}

/// Адрес оригинала для конкретного хука — то, что нужно макросу [`crate::hook!`].
///
/// `None`, если такой перехват не ставили.
pub fn original_of(hook: *const ()) -> Option<usize> {
    let key = hook as usize;
    for index in 0..SLOTS {
        if HOOKS[index].load(Ordering::Acquire) == key {
            let original = ORIGINALS[index].load(Ordering::Acquire);
            return (original != 0).then_some(original);
        }
    }
    None
}

/// Подменяет интерфейс студийного рендерера и берёт подмену под контроль.
///
/// # Safety
/// `ours` должен жить до выгрузки модуля и совпадать по ABI с
/// [`StudioInterface`].
pub unsafe fn interface(ours: *mut StudioInterface) -> Result<(), Error> {
    // SAFETY: контракт функции.
    let swap = unsafe { SwappedInterface::install(ours) }?;
    match MANAGED.lock() {
        Ok(mut list) => {
            log::line(&format!(
                "перехват: интерфейс рендерера подменён на {ours:p}, всего {}",
                list.len() + 1
            ));
            list.push(Managed::Interface(Box::new(swap)));
            Ok(())
        }
        Err(_) => {
            // `Drop` вернёт движку исходный интерфейс.
            drop(swap);
            Err(HookError::NoInterface)
        }
    }
}

/// Сколько перехватов в реестре.
pub fn count() -> usize {
    MANAGED.lock().map(|list| list.len()).unwrap_or(0)
}

/// Сколько хуков включено прямо сейчас (для отчётов).
pub fn enabled() -> usize {
    MANAGED
        .lock()
        .map(|list| {
            list.iter()
                .filter(|entry| matches!(entry, Managed::Detour(detour) if detour.is_enabled()))
                .count()
        })
        .unwrap_or(0)
}

/// Возвращает подмены интерфейса, если движок или клиент их сбросили.
///
/// Смена карты переинициализирует рендерер, и `g_pStudioInterface` снова
/// начинает указывать на клиентский интерфейс. Возвращает `true`, если хоть
/// одну подмену пришлось вернуть.
pub fn reinstall_interfaces() -> bool {
    let Ok(list) = MANAGED.lock() else {
        return false;
    };
    let mut reinstalled = false;
    for entry in list.iter() {
        if let Managed::Interface(swap) = entry {
            reinstalled |= swap.reinstall();
        }
    }
    reinstalled
}

/// Снимает все перехваты и возвращает интерфейсы на место.
///
/// Возвращает число снятых. Вызывается из [`crate::unload`]; повторный вызов
/// вернёт `0`.
///
/// Трамплины детуров не освобождаются: они выделены в куче процесса, а не в
/// образе DLL, и поток игры мог войти в трамплин за миг до снятия патча.
pub fn detach_all() -> usize {
    let pending = match MANAGED.lock() {
        Ok(mut list) => std::mem::take(&mut *list),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };
    // Оригиналы не обнуляем: поток игры мог войти в наш хук прямо перед
    // снятием патча, и ему нужно вернуться через трамплин. Сами трамплины
    // остаются в куче процесса, пока живёт мод.
    let count = pending.len();
    for entry in pending.into_iter().rev() {
        match entry {
            Managed::Detour(detour) => {
                if detour.is_enabled() {
                    detour.disable().ok();
                }
                std::mem::forget(detour);
            }
            // `Drop` возвращает движку исходный интерфейс.
            Managed::Interface(swap) => drop(swap),
        }
    }
    if count > 0 {
        log::line(&format!("перехваты: снято {count}"));
    }
    count
}
