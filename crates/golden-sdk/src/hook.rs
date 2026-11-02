//! Inline-хуки. Правило проекта: только `retour`, без самописных трамплинов.
//!
//! Важно про сборку: у `retour` 0.3 макрос `static_detour!` включён в фичу
//! `static-detour`, а она тянет `#![feature(unboxed_closures)]` — то есть
//! требует nightly. Проект собирается на stable, поэтому используется
//! [`retour::RawDetour`] напрямую (он на stable работает) через обёртку
//! [`Detour`]; в `Cargo.toml` фича отключена (`default-features = false`).
//!
//! Три сценария, которые реально нужны:
//!
//! * [`Detour`] — обычный inline-хук на функцию по адресу;
//! * [`swap_interface`] — подмена `r_studio_interface_t`, самый дешёвый способ
//!   вклиниться в отрисовку моделей (движок проверяет только `version == 1`);
//! * [`Detour::trampoline`] — вызов оригинала из своего кода.

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::renderer::{StudioInterface, interface_slot};

/// Ошибка установки/включения хука.
#[derive(Debug)]
pub enum HookError {
    /// `retour` не смог поставить детур (обычно — недоступная память).
    Retour(retour::Error),
    /// Целевой адрес нулевой или не код.
    BadTarget(usize),
    /// Нет слота интерфейса (движок ещё не загружен).
    NoInterface,
}

impl core::fmt::Display for HookError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Retour(e) => write!(f, "retour: {e}"),
            Self::BadTarget(addr) => write!(f, "некорректный адрес хука: {addr:#x}"),
            Self::NoInterface => write!(f, "нет g_pStudioInterface"),
        }
    }
}

impl core::error::Error for HookError {}

impl From<retour::Error> for HookError {
    fn from(value: retour::Error) -> Self {
        Self::Retour(value)
    }
}

/// Обёртка над [`retour::RawDetour`].
pub struct Detour {
    inner: retour::RawDetour,
}

impl core::fmt::Debug for Detour {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Detour")
            .field("enabled", &self.inner.is_enabled())
            .field(
                "trampoline",
                &(self.inner.trampoline() as *const () as usize),
            )
            .finish()
    }
}

impl Detour {
    /// Ставит детур (без включения).
    ///
    /// # Safety
    /// `target` должен указывать на начало функции, `detour` — на функцию с
    /// совместимой сигнатурой и ABI.
    pub unsafe fn new(target: usize, detour: *const ()) -> Result<Self, HookError> {
        if target == 0 {
            return Err(HookError::BadTarget(target));
        }
        // SAFETY: контракт функции.
        let inner = unsafe { retour::RawDetour::new(target as *const (), detour)? };
        Ok(Self { inner })
    }

    /// Указатель на трамплин: так вызывается оригинальная функция.
    pub fn trampoline(&self) -> *const () {
        self.inner.trampoline()
    }

    pub fn enable(&self) -> Result<(), HookError> {
        // SAFETY: детур создан нами и живёт, пока живёт `self`.
        unsafe { self.inner.enable() }.map_err(Into::into)
    }

    pub fn disable(&self) -> Result<(), HookError> {
        // SAFETY: см. `enable`.
        unsafe { self.inner.disable() }.map_err(Into::into)
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_enabled()
    }
}

/// Подмена интерфейса студийного рендерера с восстановлением при `Drop`.
///
/// Движок читает `g_pStudioInterface` в начале каждого кадра отрисовки моделей,
/// поэтому подмену видно сразу, а после смены карты клиент может переинициализировать
/// интерфейс — тогда [`SwappedInterface::reinstall`] вернёт нашу подмену.
pub struct SwappedInterface {
    slot: *mut *mut StudioInterface,
    ours: *mut StudioInterface,
    previous: *mut StudioInterface,
}

/// Слот занят чужим модом, и мы это уже сообщили в лог.
static YIELDED: AtomicBool = AtomicBool::new(false);

impl core::fmt::Debug for SwappedInterface {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SwappedInterface")
            .field("slot", &(self.slot as usize))
            .field("ours", &(self.ours as usize))
            .field("previous", &(self.previous as usize))
            .finish()
    }
}

impl SwappedInterface {
    /// # Safety
    /// `ours` должен жить дольше подмены, а поля структуры — совпадать по ABI.
    pub unsafe fn install(ours: *mut StudioInterface) -> Result<Self, HookError> {
        let slot = interface_slot().ok_or(HookError::NoInterface)?;
        // SAFETY: слот получен из `hw.dll`.
        let previous = unsafe { *slot };
        if previous.is_null() || ours.is_null() {
            return Err(HookError::NoInterface);
        }
        // SAFETY: слот валиден, `ours` живёт дольше.
        unsafe { *slot = ours };
        Ok(Self {
            slot,
            ours,
            previous,
        })
    }

    /// Стоит ли над нами ещё один мод.
    ///
    /// Верхний мод подменил слот своим интерфейсом и зовёт наш как `previous`,
    /// то есть держит указатели в нашу таблицу. Пока он жив, выгружаться
    /// нельзя: после освобождения модуля он прыгнет в пустую память.
    pub fn stacked(&self) -> bool {
        // SAFETY: слот валиден на всё время жизни объекта.
        let current = unsafe { *self.slot };
        !current.is_null() && current != self.ours && current != self.previous
    }

    /// Интерфейс, который был до подмены.
    pub fn previous(&self) -> *mut StudioInterface {
        self.previous
    }

    /// Возвращает подмену, если движок/клиент её сбросили.
    ///
    /// Возвращает `true`, если подмену пришлось вернуть.
    ///
    /// Если слот занял другой мод, подмена не отбирается: иначе два мода будут
    /// перетягивать интерфейс каждый кадр, и всё, что рисуется через хуки
    /// (скелеты, траектория), начнёт мигать. Такой мод просто ждёт: когда
    /// верхний выгрузится, слот вернётся к нашему `previous` или к нам самим.
    pub fn reinstall(&self) -> bool {
        // SAFETY: слот валиден на всё время жизни объекта.
        let current = unsafe { *self.slot };
        if current == self.ours {
            return false;
        }
        if current != self.previous {
            if !YIELDED.swap(true, Ordering::Relaxed) {
                crate::log::line(
                    "перехват: интерфейс занят другим модом — подмена не возвращается, пока он загружен",
                );
            }
            return false;
        }
        if YIELDED.swap(false, Ordering::Relaxed) {
            crate::log::line("перехват: интерфейс освободился — подмена возвращена");
        }
        // SAFETY: см. `install`.
        unsafe { *self.slot = self.ours };
        true
    }
}

impl Drop for SwappedInterface {
    fn drop(&mut self) {
        // SAFETY: слот валиден на всё время жизни объекта. Указатель движка
        // возвращается к клиентской таблице: иначе он останется указывать в
        // нашу DLL и после освобождения прыгнет в пустую память.
        //
        // Если в слоте уже стоит интерфейс другого мода, ничего не трогаем: он
        // сам вызовет нас как свой `previous`, и подмена под ним должна
        // оставаться. Такой мод надо выгружать первым — иначе его цепочка
        // останется указывать в освобождённую DLL.
        if unsafe { *self.slot } == self.ours {
            unsafe { *self.slot = self.previous };
        }
    }
}

/// Хелпер: оборачивает `extern "C"`-указатель в `*const ()` для [`Detour::new`].
pub fn as_hook_target(f: *const c_void) -> *const () {
    f.cast()
}
