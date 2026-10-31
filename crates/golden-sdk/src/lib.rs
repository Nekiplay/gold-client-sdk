//! # golden-sdk
//!
//! Переиспользуемый SDK для Gold Client (CS 1.6): всё, что не является фичей,
//! живёт здесь, чтобы новые функции (просмотрщик моделей, дебаг локального
//! игрока, боты в оффлайне, инструменты) не переписывали одно и то же.
//!
//! ## Что внутри
//!
//! * [`offsets`] — адреса и смещения, полученные статическим анализом
//!   (`docs/STUDIO_REVERSE.md`);
//! * [`process`] — базы модулей, чтение/запись памяти;
//! * [`studio`] — формат `.mdl` (заголовок, кости, последовательности, скелет);
//! * [`engine`] — `engine_studio_api_t` (48 функций движка) и сущности;
//! * [`renderer`] — объект `CStudioModelRenderer`, матрицы костей, интерфейс рендера;
//! * [`draw`] — рисование линий средствами OpenGL (мировые и экранные);
//! * [`player`] — команда игрока и проверка «это модель игрока»;
//! * [`log`] — лог в файл и в отладчик;
//! * [`math`] — векторы и проекция мира на экран с правильным FOV GoldSrc;
//! * [`hook`] — inline-хуки через `retour` и подмена интерфейса рендерера;
//! * [`module`] — свой модуль: путь, `HMODULE`, `FreeLibraryAndExitThread`;
//! * [`unload`] — реестр очисток и выгрузка DLL без перезапуска игры;
//! * [`validate`] — самопроверка смещений после обновления игры.
//!
//! ## Пример: скелет модели, которую только что нарисовали
//!
//! ```no_run
//! use golden_sdk::{renderer::StudioRenderer, math::Fov};
//!
//! # fn example() -> Option<()> {
//! let renderer = StudioRenderer::global()?;
//! if !renderer.is_initialized() {
//!     return None;
//! }
//! let model = renderer.studio_model()?;
//! println!("модель: {}", model.name());
//!
//! let fov = Fov::from_horizontal(90.0, 16.0 / 9.0);
//! let camera = renderer.camera();
//! for (parent, child) in renderer.skeleton_lines() {
//!     let a = camera.project(parent, fov, (1920.0, 1080.0));
//!     let b = camera.project(child, fov, (1920.0, 1080.0));
//!     let _ = (a, b);
//! }
//! # Some(())
//! # }
//! ```
//!
//! ## Границы
//!
//! SDK даёт доступ к движку и рендереру как таковому. Он не содержит и не
//! подразумевает ESP/аимбота по живым игрокам: правила проекта — никакого
//! раскрытия чужих игроков в мультиплеере, только модели локального игрока,
//! боты в оффлайне, просмотр ассетов и инструменты.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations)]

extern crate alloc;

/// Тесты гоняются на хосте: им нужен `std` (печать, assert-сообщения).
#[cfg(test)]
extern crate std;

pub mod concommand;
pub mod draw;
pub mod engine;
pub mod glcolor;
pub mod hook;
pub mod host;
pub mod log;
pub mod math;
pub mod module;
pub mod offsets;
pub mod player;
pub mod process;
pub mod renderer;
pub mod state;
pub mod studio;
pub mod sync;
pub mod time;
pub mod unload;
pub mod validate;

pub use draw::Color;
pub use math::{Camera, Fov, ScreenPos, Vec3};
pub use studio::{BoneMatrix, StudioBone, StudioHeader, StudioModel};

/// Запись в лог: `log!("x = {}", 1)`.
///
/// Работает без инициализации (тогда только отладчик).
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::line($crate::__private::format!($($arg)*))
    };
}

#[doc(hidden)]
pub mod __private {
    pub use alloc::format;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;
}

/// Сборка, под которую проверены смещения из [`offsets`].
pub const BUILD: &str = offsets::BUILD;