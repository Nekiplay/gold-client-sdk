//! # golden-std
//!
//! Удобный слой над [`golden_sdk`]: вместо смещений и чтения памяти —
//! `Game::get()`, `game.get_players()`, `Overlay::skeleton(..)`.
//!
//! ```no_run
//! use golden_std::Game;
//!
//! # fn example() -> Option<()> {
//! let game = Game::get()?;
//! for name in game.get_players().map(|player| player.name()) {
//!     println!("{name}");
//! }
//! # Some(())
//! # }
//! ```
//!
//! Что где:
//!
//! * [`Game`] — точка входа: сущности, игроки, локальный игрок, консоль;
//! * [`Player`] — игрок: ник, команда, HP, фраги, бомба, кит, оружие;
//! * [`Entity`] — любая сущность мира с моделью;
//! * [`Overlay`] — отрисовка: линии, скелет, круг у головы, текст;
//! * [`Scheduler`] — отложенные действия, которые двигает кадр;
//! * [`input`] — блокировка клавиш перехватом `Key_Event`;
//! * [`hooks`] — реестр перехватов: [`hooks::install`], [`hook_macro::hook!`],
//!   всё снимается само при [`unload`];
//! * [`concommand`] — свои консольные команды, которые снимаются при выгрузке;
//! * [`launch`] — запуск локальной игры с ботами и инъекция своей DLL;
//! * [`unload`] — выгрузка мода из игры без перезапуска.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod commands;
pub mod concommand;
pub mod game;
pub mod hook_macro;
pub mod hooks;
pub mod input;
pub mod launch;
pub mod pose;
pub mod render;
pub mod schedule;
pub mod unload;

pub use game::{Entity, Game, Player, Players, State, View, MAX_PLAYERS};
pub use golden_sdk::math::{Camera, Fov, ScreenPos, Vec3};
pub use golden_sdk::player::Team;
pub use golden_sdk::state::horizontal_fov;
pub use hooks::Hook;
pub use input::{InputError, KeyBlocker};
pub use launch::{Game as LaunchedGame, GameConfig, LaunchError};
pub use pose::{Pose, PoseCache};
pub use render::{Color, OutlineStyle, Overlay, outline};
pub use schedule::Scheduler;
pub use unload::{defer_detach, defer_unload, detach, on_unload, unload};