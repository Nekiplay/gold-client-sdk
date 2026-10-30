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
//! * [`Overlay`] — отрисовка: линии, скелет, круг у головы, текст.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod game;
pub mod pose;
pub mod render;

pub use game::{Entity, Game, Player, Players, View, MAX_PLAYERS};
pub use golden_sdk::math::{Camera, Fov, ScreenPos, Vec3};
pub use golden_sdk::player::Team;
pub use pose::{Pose, PoseCache};
pub use render::{Color, Overlay};