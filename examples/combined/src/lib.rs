//! ESP, автоматический прыжок и триггербот с общим меню. Для игры с ботами.
//!
//! Insert — меню; unload_combined — остановка мода. Для bhop нужен бинд
//! `bind mwheelup +jump`; прыжки выполняются при удержании пробела.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use golden_egui::egui;
use golden_sdk::state::EngineState;
use golden_std::Color;
use golden_std::input::{self, Key};

static BHOP: AtomicBool = AtomicBool::new(false);
static LAST_FRAME: Mutex<Option<f32>> = Mutex::new(None);

struct Settings {
    esp: bool,
    width: f32,
    color: [u8; 3],
    visible_color: [u8; 3],
    visible_enabled: bool,
    bhop: bool,
    trigger: bool,
    head_only: bool,
    delay: u32,
    range: u32,
    activation: bool,
    activation_vk: u32,
}

static SETTINGS: Mutex<Settings> = Mutex::new(Settings {
    esp: true,
    width: 2.0,
    color: [255, 51, 51],
    visible_color: [51, 255, 77],
    visible_enabled: true,
    bhop: false,
    trigger: false,
    head_only: false,
    delay: 0,
    range: 4096,
    activation: true,
    activation_vk: 0x06,
});

#[unsafe(no_mangle)]
pub extern "system" fn DllMain(module: *mut c_void, reason: u32, _: *mut c_void) -> i32 {
    if reason == 1 {
        golden_sdk::log::init(golden_sdk::log::path_next_to(module));
        let _ = std::thread::Builder::new()
            .name("combined".into())
            .spawn(bootstrap);
    }
    1
}

fn bootstrap() {
    let _token = golden_sdk::module::ThreadToken::new();
    let deadline = Instant::now() + Duration::from_secs(180);
    while golden_sdk::process::hw_base().is_none() || golden_sdk::process::client_base().is_none() {
        if golden_sdk::module::should_stop() || Instant::now() >= deadline {
            golden_sdk::log!("combined: не дождались модулей игры");
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Err(error) = input::install() {
        golden_sdk::log!("combined: ошибка ввода: {error}");
        return;
    }
    let handle = match golden_egui::install() {
        Ok(handle) => handle,
        Err(error) => {
            golden_sdk::log!("combined: ошибка egui: {error}");
            golden_std::unload::defer_unload(300);
            return;
        }
    };
    golden_std::unload::on_unload(move || drop(handle));
    golden_egui::set_toggle_key(golden_egui::keys::VK_INSERT);
    golden_egui::set_ui(menu);
    golden_egui::set_frame_callback(frame);
    // Ждём готовности графического контекста; интерфейс ESP появится при входе на карту.
    let deadline = Instant::now() + Duration::from_secs(180);
    while !golden_egui::ready() {
        if golden_sdk::module::should_stop() {
            return;
        }
        if Instant::now() >= deadline {
            golden_sdk::log!("combined: не дождались первого кадра egui");
            golden_std::unload::defer_unload(300);
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Err(error) = golden_std::esp::Esp::new()
        .color(|_| Color::rgb(1.0, 0.2, 0.2))
        .visible_color(|_| Color::rgb(0.2, 1.0, 0.3))
        .install()
    {
        golden_sdk::log!("combined: ошибка ESP: {error}");
        golden_std::unload::defer_unload(300);
        return;
    }
    if let Err(error) = golden_std::trigger::Trigger::new()
        .enabled(false)
        .activation(Key::Mouse5)
        .install()
    {
        golden_sdk::log!("combined: ошибка триггербота: {error}");
        golden_std::unload::defer_unload(300);
        return;
    }
    if let Ok(s) = SETTINGS.lock() {
        golden_std::esp::set_outline(s.esp);
        golden_std::esp::set_width(s.width);
        golden_std::esp::set_color(color(s.color));
        golden_std::esp::set_visible_color(s.visible_enabled.then(|| color(s.visible_color)));
        golden_std::trigger::set_activation_vk(if s.activation { s.activation_vk } else { 0 });
        golden_std::trigger::set_head_only(s.head_only);
        golden_std::trigger::set_delay_ms(s.delay);
        golden_std::trigger::set_range(s.range);
    }
    if let Err(error) = golden_std::concommand::Command::new("unload_combined", unload)
        .help("остановить комбинированный мод")
        .register()
    {
        golden_sdk::log!("combined: команда выгрузки: {error}");
    }
    if let Some(dir) = golden_sdk::log::path()
        .map(std::path::PathBuf::from)
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
    {
        golden_std::commands::watch(golden_std::commands::path_next_to(&dir));
    }
    golden_sdk::log!("combined: Insert — меню, unload_combined — остановка");
    while !golden_sdk::module::should_stop() {
        std::thread::sleep(Duration::from_millis(250));
        golden_std::hooks::reinstall_interfaces();
    }
}

unsafe extern "C" fn unload(_: *const golden_sdk::concommand::CommandArgs) {
    golden_std::unload::defer_unload(300);
}

fn frame() {
    if let Ok(settings) = SETTINGS.lock() {
        golden_std::trigger::set_enabled(settings.trigger && !golden_egui::visible());
    }
    let active = BHOP.load(Ordering::Acquire) && !golden_egui::visible();
    // egui снимает свои блокировки при закрытии, поэтому восстанавливаем
    // блокировку пробела на следующем кадре, не вмешиваясь в открытое меню.
    if !golden_egui::visible() {
        if active {
            input::blocker().suppress(input::VK_SPACE);
        } else {
            input::blocker().release(input::VK_SPACE);
        }
    }
    if !active || !EngineState::in_game() {
        return;
    }
    let time = EngineState::time();
    let Ok(mut last) = LAST_FRAME.lock() else {
        return;
    };
    if *last == Some(time) {
        return;
    }
    *last = Some(time);
    if EngineState::on_ground() && input::is_down(input::VK_SPACE) {
        input::scroll(input::WHEEL_UP);
    }
}

fn menu(ctx: &egui::Context) {
    egui::Window::new("Golden Client — ESP / Bhop / Trigger")
        .default_width(360.0)
        .show(ctx, |ui| {
            let Ok(mut s) = SETTINGS.lock() else {
                return;
            };
            ui.heading("ESP");
            if ui.checkbox(&mut s.esp, "Контур противников").changed() {
                golden_std::esp::set_outline(s.esp);
            }
            if ui
                .add(egui::Slider::new(&mut s.width, 1.0..=8.0).text("Толщина"))
                .changed()
            {
                golden_std::esp::set_width(s.width);
            }
            ui.horizontal(|ui| {
                ui.label("Цвет сквозь стены");
                if ui.color_edit_button_srgb(&mut s.color).changed() {
                    golden_std::esp::set_color(color(s.color));
                }
            });
            let visible_changed = ui
                .checkbox(&mut s.visible_enabled, "Отдельный цвет видимой части")
                .changed();
            let mut color_changed = false;
            ui.add_enabled_ui(s.visible_enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Цвет видимой части");
                    color_changed = ui.color_edit_button_srgb(&mut s.visible_color).changed();
                });
            });
            if visible_changed || color_changed {
                golden_std::esp::set_visible_color(
                    s.visible_enabled.then(|| color(s.visible_color)),
                );
            }
            ui.separator();
            ui.heading("Bhop");
            if ui
                .checkbox(&mut s.bhop, "Автопрыжок при удержании пробела")
                .changed()
            {
                BHOP.store(s.bhop, Ordering::Release);
            }
            ui.label("Требуется bind mwheelup +jump");
            ui.separator();
            ui.heading("Triggerbot");
            ui.checkbox(&mut s.trigger, "Автострельба");
            if ui
                .checkbox(&mut s.activation, "Только при удержании клавиши")
                .changed()
            {
                golden_std::trigger::set_activation_vk(if s.activation {
                    s.activation_vk
                } else {
                    0
                });
            }
            ui.add_enabled_ui(s.activation, |ui| {
                let name = Key::from_win_vk(s.activation_vk)
                    .map(|key| key.to_string())
                    .unwrap_or_else(|| format!("VK {}", s.activation_vk));
                egui::ComboBox::from_label("Клавиша активации")
                    .selected_text(name)
                    .show_ui(ui, |ui| {
                        for vk in 1..=255 {
                            if let Some(key) = Key::from_win_vk(vk) {
                                if ui
                                    .selectable_value(&mut s.activation_vk, vk, key.to_string())
                                    .changed()
                                {
                                    golden_std::trigger::set_activation_vk(vk);
                                }
                            }
                        }
                    });
            });
            if ui.checkbox(&mut s.head_only, "Только голова").changed() {
                golden_std::trigger::set_head_only(s.head_only);
            }
            if ui
                .add(egui::Slider::new(&mut s.delay, 0..=500).text("Реакция, мс"))
                .changed()
            {
                golden_std::trigger::set_delay_ms(s.delay);
            }
            if ui
                .add(egui::Slider::new(&mut s.range, 100..=8192).text("Дальность"))
                .changed()
            {
                golden_std::trigger::set_range(s.range);
            }
            ui.separator();
            if ui.button("Закрыть меню").clicked() {
                golden_egui::set_visible(false);
            }
            if ui.button("Остановить мод").clicked() {
                golden_std::unload::defer_unload(300);
            }
        });
}

fn color(rgb: [u8; 3]) -> Color {
    Color::rgb(
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    )
}
