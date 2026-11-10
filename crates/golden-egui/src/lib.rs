//! `golden-egui`: интерфейс [egui](https://docs.rs/egui) поверх Gold Client.
//!
//! Крейт рисует egui в конце кадра штатным `egui_glow`-рендерером (перехват
//! `wglSwapBuffers`) и подменяет оконную процедуру `hl.exe`, чтобы кормить egui
//! событиями мыши и клавиатуры. Пока интерфейс открыт, оконные сообщения ввода
//! перехватываются; поле состояния чата движка не изменяется.
//!
//! Для установки детура крейт подменяет слот `StudioDrawPlayer` в таблице
//! клиента. Моды следует выгружать в обратном порядке их загрузки; несколько
//! модов с `golden-egui` одновременно не поддерживаются.
//!
//! Минимальный мод:
//!
//! ```no_run
//! fn bootstrap() -> Result<(), Box<dyn std::error::Error>> {
//!     let egui = golden_egui::install()?;
//!     golden_egui::set_toggle_key(golden_egui::keys::VK_INSERT);
//!     golden_egui::set_ui(|ctx| {
//!         golden_egui::egui::Window::new("мод").show(ctx, |ui| {
//!             ui.label("привет");
//!         });
//!     });
//!     golden_std::unload::on_unload(move || drop(egui));
//!     Ok(())
//! }
//! ```

use std::error::Error;
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub use egui;

mod input;
mod paint;
mod sys;

/// Часто используемые коды клавиш для [`set_toggle_key`].
///
/// Полный список — `VK_*` в Win32; сюда можно передать любой код.
pub mod keys {
    /// `Insert` — традиционная клавиша меню.
    pub const VK_INSERT: u32 = 0x2D;
}

/// UI-колбэк мода: вызывается из рендер-потока каждый кадр, пока меню видимо.
static UI: Mutex<Option<Box<dyn FnMut(&egui::Context) + Send>>> = Mutex::new(None);
static FRAME: Mutex<Option<Box<dyn FnMut() + Send>>> = Mutex::new(None);

/// Меню открыто.
static VISIBLE: AtomicBool = AtomicBool::new(false);

/// Клавиша-переключатель (`WM_KEYDOWN`), 0 — отключена.
static TOGGLE_KEY: AtomicU32 = AtomicU32::new(0);

/// Установка уже была в этой копии DLL.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Ошибка установки.
#[derive(Debug)]
pub enum InstallError {
    /// `golden-egui` в этой копии DLL уже установлен.
    AlreadyInstalled,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => write!(f, "golden-egui уже установлен в этом процессе"),
        }
    }
}

impl Error for InstallError {}

/// Установленный egui. При `drop` прячет меню и запрашивает освобождение
/// GL-объектов; хуки и оконную процедуру снимает `golden_std::unload`.
pub struct Egui {
    _private: (),
}

/// Ставит перехваты: оконную процедуру и `wglSwapBuffers`.
///
/// Установка асинхронная: оконная процедура подменяется после появления окна,
/// детур устанавливается в рендер-потоке при первом `StudioDrawPlayer`.
/// До входа на сервер и появления отрисованных игроков меню недоступно.
pub fn install() -> Result<Egui, InstallError> {
    if INSTALLED.swap(true, Ordering::AcqRel) {
        return Err(InstallError::AlreadyInstalled);
    }
    paint::spawn_installer();
    Ok(Egui { _private: () })
}

impl Drop for Egui {
    fn drop(&mut self) {
        set_visible(false);
        paint::begin_uninstall();
        golden_sdk::log!("egui: выгружен");
    }
}

/// Задаёт UI-колбэк. Вызывается каждый кадр из рендер-потока, пока меню видимо.
pub fn set_ui(callback: impl FnMut(&egui::Context) + Send + 'static) {
    *UI.lock().unwrap() = Some(Box::new(callback));
}

/// Кадровый колбэк вызывается из рендер-потока даже при закрытом меню.
pub fn set_frame_callback(callback: impl FnMut() + Send + 'static) {
    *FRAME.lock().unwrap() = Some(Box::new(callback));
}

/// Открывает или прячет меню.
///
/// При открытии перехватываются оконные сообщения ввода. Поле состояния чата
/// движка не меняется.
pub fn set_visible(visible: bool) {
    if visible && !paint::ready() {
        golden_sdk::log!("egui: меню ещё не готово, ввод остаётся игре");
        return;
    }
    if VISIBLE.swap(visible, Ordering::AcqRel) == visible {
        return;
    }
    if visible {
        input::release_game_mouse();
        input::capture_game_input();
        input::request_cursor_update();
        input::feed_cursor_now();
    } else {
        input::request_cursor_update();
        input::feed_pointer_gone();
        input::release_game_input();
        input::restore_game_mouse();
        input::restore_sdl_mouse();
    }
}

/// Открыто ли меню.
pub fn visible() -> bool {
    VISIBLE.load(Ordering::Acquire)
}

/// Готов ли рендерер меню к работе.
pub fn ready() -> bool {
    paint::ready()
}

/// Переключает видимость меню.
pub fn toggle() {
    set_visible(!visible());
}

/// Клавиша, которая переключает меню из оконной процедуры (`VK_*`, 0 — нет).
///
/// Работает и когда меню скрыто: поллинг `GetAsyncKeyState` не нужен.
pub fn set_toggle_key(vk: u32) {
    TOGGLE_KEY.store(vk, Ordering::Release);
}

/// Текущая клавиша-переключатель (0 — отключена).
pub(crate) fn toggle_key() -> u32 {
    TOGGLE_KEY.load(Ordering::Acquire)
}

/// Доступ к UI-колбэку для рендер-потока.
pub(crate) fn ui_slot() -> &'static Mutex<Option<Box<dyn FnMut(&egui::Context) + Send>>> {
    &UI
}
