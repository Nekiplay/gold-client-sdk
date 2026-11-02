//! Макрос хука: объявляет перехват с нужным ABI и его аксессор к оригиналу.
//!
//! ```ignore
//! use std::ffi::c_void;
//!
//! golden_std::hook! {
//!     /// Перехват `StudioSetupBones`: кости нужны свежими.
//!     pub unsafe extern "thiscall" fn on_bones(this: *mut c_void, flags: i32)
//!         original: on_bones_original
//!         -> i32
//!     {
//!         let bones = on_bones_original()(this, flags);
//!         // ... своя работа ...
//!         bones
//!     }
//! }
//!
//! # fn example(target: usize) {
//! // Установка: перехват снимется сам при `unload`.
//! golden_std::hooks::install(target, on_bones as *const ()).ok();
//! # }
//! ```
//!
//! Зачем аксессор, а не переменная внутри тела: у `macro_rules!` гигиена
//! локальных переменных — имя, созданное макросом, телу из вызова не видно.
//! Функции-элементы гигиеной не закрыты, поэтому оригинал отдаётся функцией
//! `имя_оригинала()`, которую объявляет макрос.
//!
//! Что даёт макрос:
//!
//! * объявляет функцию с указанным `unsafe extern $abi`, так что её адрес
//!   годится для `retour` без ручных `transmute`;
//! * объявляет рядом аксессор, возвращающий типизированный указатель на
//!   оригинальную функцию (его выдаёт [`crate::hooks::install`] через
//!   [`crate::hooks::original_of`]);
//! * `#[doc]` и видимость переносятся на обе функции.
//!
//! Требование одно: перехват должен быть установлен до первого вызова. Детур
//! начинает перенаправлять управление только после [`crate::hooks::install`],
//! а трамплин к этому моменту уже известен — `expect` в аксессоре недостижим,
//! если не звать хук вручную.
//!
//! Соглашения (`__fastcall`, `__thiscall`) задаются строкой ABI так же, как в
//! обычном Rust: `"thiscall"` — `this` в `ECX`, `"fastcall"` — первые два
//! аргумента в `ECX`/`EDX`.

/// Объявляет хук и аксессор к его оригиналу.
///
/// Синтаксис:
///
/// ```text
/// [атрибуты] [видимость] unsafe extern "abi" fn имя(аргументы)
///     original: имя_аксессора
///     [-> тип]
/// { тело }
/// ```
#[macro_export]
macro_rules! hook {
    (
        $(#[$meta:meta])*
        $vis:vis unsafe extern $abi:literal fn $name:ident
        ( $( $arg:ident : $ty:ty ),* $(,)? )
        original : $orig:ident
        $( -> $ret:ty )?
        $body:block
    ) => {
        $(#[$meta])*
        #[allow(clippy::too_many_arguments)]
        $vis unsafe extern $abi fn $name( $( $arg: $ty ),* ) $( -> $ret )? {
            $body
        }

        $(#[$meta])*
        ///
        /// Оригинал перехвата: та же сигнатура, что у функции игры.
        ///
        /// До [`$crate::hooks::install`] оригинала ещё нет — тогда вызов паникует.
        #[allow(dead_code)]
        $vis fn $orig() -> unsafe extern $abi fn( $( $ty ),* ) $( -> $ret )? {
            unsafe {
                $crate::hooks::original_of($name as *const ())
                    .map(|address| {
                        ::core::mem::transmute_copy::<
                            usize,
                            unsafe extern $abi fn( $( $ty ),* ) $( -> $ret )?,
                        >(&address)
                    })
                    .expect("хук вызван до установки: сначала `hooks::install(target, hook)`")
            }
        }
    };
}
