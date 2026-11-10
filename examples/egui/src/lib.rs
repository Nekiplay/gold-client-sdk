//! Демо egui-меню поверх игры.
//!
//! `Insert` — открыть/закрыть меню (также команда `egui_menu` в консоли),
//! `unload_egui` — выгрузить мод без перезапуска игры.
//!
//! Пока меню открыто, ввод не доходит до игры: камера не крутится, курсор
//! свободен. Подробности — в документации крейта `golden-egui`.

use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use golden_egui::egui;

/// `DllMain` — загрузка.
const DLL_PROCESS_ATTACH: u32 = 1;

/// Сколько ждём появления `hw.dll`/`client.dll` после инъекции.
const MODULE_WAIT: Duration = Duration::from_secs(180);

/// Состояние демо-окна.
struct Demo {
    clicks: u32,
    text: String,
    slider: f32,
    checkbox: bool,
    color: egui::Color32,
}

static DEMO: Mutex<Demo> = Mutex::new(Demo {
    clicks: 0,
    text: String::new(),
    slider: 25.0,
    checkbox: true,
    color: egui::Color32::LIGHT_BLUE,
});

/// Точка входа DLL.
///
/// # Safety
/// Вызывается загрузчиком Windows; `module` — HMODULE, `reason` — код события.
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(module: *mut c_void, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        golden_sdk::log::init(golden_sdk::log::path_next_to(module));
        // Работа в отдельном потоке: в DllMain нельзя блокировать загрузчик.
        let _ = std::thread::Builder::new()
            .name("example-egui".to_string())
            .spawn(bootstrap);
    }
    1
}

/// Стартовый сценарий: дождаться модулей, поднять egui, зарегистрировать команды.
fn bootstrap() {
    // Поток принадлежит DLL: при выгрузке он должен выйти первым.
    let _token = golden_sdk::module::ThreadToken::new();
    golden_sdk::log!("example-egui {} запускается", env!("CARGO_PKG_VERSION"));

    if !wait_for_modules(MODULE_WAIT) {
        golden_sdk::log!("не дождались hw.dll/client.dll за {MODULE_WAIT:?} — выходим");
        return;
    }

    if let Err(error) = golden_std::input::install() {
        golden_sdk::log!("перехват Key_Event не установлен: {error} — egui не сможет блокировать ввод");
        return;
    }

    // Перехваты окна и wglSwapBuffers. Хендл держим до выгрузки: его drop
    // прячет меню, возвращает ввод игре и освобождает GL-объекты.
    let egui_handle = match golden_egui::install() {
        Ok(handle) => handle,
        Err(error) => {
            golden_sdk::log!("egui не установлен: {error} — выходим");
            return;
        }
    };
    golden_std::unload::on_unload(move || drop(egui_handle));

    golden_egui::set_toggle_key(golden_egui::keys::VK_INSERT);
    golden_egui::set_ui(ui);

    match golden_std::concommand::Command::new("egui_menu", toggle_menu)
        .help("открыть/закрыть egui-меню")
        .register()
    {
        Ok(()) => {}
        Err(error) => golden_sdk::log!("egui_menu не зарегистрирована: {error}"),
    }
    match golden_std::concommand::Command::new("unload_egui", cmd_unload)
        .help("выгрузить мод")
        .hidden()
        .register()
    {
        Ok(()) => {}
        Err(error) => golden_sdk::log!("unload_egui не зарегистрирована: {error}"),
    }

    // Эхо команд скрываем на стороне клиента, как в примере esp.
    if golden_std::console::install(&["egui_menu", "unload_egui"]) {
        golden_sdk::log!("консоль: эхо команд скрыто");
    }

    // Управление извне: строки из файла рядом с логом исполняются как команды.
    if let Some(dir) = golden_sdk::log::path()
        .map(std::path::PathBuf::from)
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
    {
        golden_std::commands::watch(golden_std::commands::path_next_to(&dir));
    }

    golden_sdk::log!("Insert — меню; консольная команда: egui_menu");

    // Выгрузку обслуживает golden_std::unload; здесь просто живём до стопа.
    while !golden_sdk::module::should_stop() {
        std::thread::sleep(Duration::from_millis(250));
    }
    golden_sdk::log!("выгрузка: поток вышел");
}

/// `egui_menu` в консоли.
unsafe extern "C" fn toggle_menu(_: *const golden_sdk::concommand::CommandArgs) {
    golden_egui::toggle();
}

/// `unload_egui` в консоли: выгрузка без перезапуска игры.
unsafe extern "C" fn cmd_unload(_: *const golden_sdk::concommand::CommandArgs) {
    golden_sdk::log!("команда unload_egui");
    golden_std::unload::defer_unload(300);
}

/// UI мода: вызывается каждый кадр, пока меню открыто.
fn ui(ctx: &egui::Context) {
    egui::Window::new("golden-egui")
        .default_width(360.0)
        .show(ctx, |ui| {
            let mut demo = DEMO.lock().unwrap();

            ui.heading("egui в Counter-Strike 1.6");
            ui.label("Кадр рисуется перехватом wglSwapBuffers, ввод — из оконной процедуры.");
            ui.separator();

            game_info(ui);

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Кнопка").clicked() {
                    demo.clicks += 1;
                }
                ui.label(format!("нажатий: {}", demo.clicks));
            });
            ui.checkbox(&mut demo.checkbox, "Переключатель");
            ui.add(egui::Slider::new(&mut demo.slider, 0.0..=100.0).text("Слайдер"));
            ui.horizontal(|ui| {
                ui.label("Текст:");
                ui.text_edit_singleline(&mut demo.text);
            });
            ui.horizontal(|ui| {
                ui.label("Цвет:");
                ui.color_edit_button_srgba(&mut demo.color);
            });
            ui.separator();
            ui.hyperlink_to("goldclient.ru", "https://goldclient.ru");
            ui.horizontal(|ui| {
                if ui.button("Скрыть (Insert)").clicked() {
                    golden_egui::set_visible(false);
                }
                if ui.button("Выгрузить мод").clicked() {
                    golden_std::unload::defer_unload(300);
                }
            });
        });
}

/// Живые данные игры: ник, здоровье, счёт.
fn game_info(ui: &mut egui::Ui) {
    let fps = ui.ctx().input(|input| 1.0 / input.stable_dt.max(1e-4));
    ui.label(format!("Кадр: {fps:.0} fps"));

    static INFO: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);
    let mut cached = INFO.lock().unwrap();
    if cached
        .as_ref()
        .is_none_or(|(time, _)| time.elapsed() >= Duration::from_millis(500))
    {
        *cached = Some((Instant::now(), collect_game_info()));
    }
    if let Some((_, lines)) = cached.as_ref() {
        for line in lines {
            ui.label(line);
        }
    }
}

fn collect_game_info() -> Vec<String> {
    let Some(game) = golden_std::game::Game::get() else {
        return vec!["studio-интерфейс ещё не отдан — мы в меню игры".into()];
    };
    let players = game.get_players().count();
    match game.get_local_player() {
        Some(me) => vec![
            format!("Игрок: {}", me.name()),
            format!(
                "HP: {}   Счёт: {}:{}   Игроков: {players}",
                me.health(),
                me.frags(),
                me.deaths()
            ),
        ],
        None => vec![format!("Свободная камера; игроков на сервере: {players}")],
    }
}

/// Ждёт `hw.dll` и `client.dll`, как в примере esp.
fn wait_for_modules(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if golden_sdk::process::hw_base().is_some() && golden_sdk::process::client_base().is_some()
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
