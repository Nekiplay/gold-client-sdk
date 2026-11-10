//! Отрисовка egui в конце кадра: перехват `wglSwapBuffers` и `egui_glow`.
//!
//! Игровой GL-контекст — времён OpenGL 2.x, но `egui_glow` умеет работать с
//! такими: шейдеры GLSL 1.20, без VAO. Чтобы не сломать отрисовку движка
//! (он своё состояние не восстанавливает и считает его нетронутым), вокруг
//! нашей отрисовки стоит сторож [`GlGuard`], возвращающий всё, что мы могли
//! задеть: матрицы, атрибуты, привязки текстур/FBO/программы/VAO.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use egui_glow::Painter;
use egui_glow::glow;

use crate::{input, sys};

/// Контекст egui: GL ему не нужен, создаётся сразу.
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

/// GL-рендерер egui; создаётся и используется только в рендер-потоке.
static PAINTER: Mutex<Option<Painter>> = Mutex::new(None);

/// GL-функции сверх 1.1, которых касается сторож состояния.
static EXT: OnceLock<GlExts> = OnceLock::new();

/// GL-рендерер поднят.
static GL_READY: AtomicBool = AtomicBool::new(false);

/// Поднять GL-рендерер не удалось (старый GL) — не долбимся.
static GL_FAILED: AtomicBool = AtomicBool::new(false);

/// Идёт выгрузка: следующий кадр освобождает GL-объекты.
static UNINSTALL: AtomicBool = AtomicBool::new(false);

/// GL-объекты освобождены (или их не было).
static DESTROYED: AtomicBool = AtomicBool::new(false);

/// Момент первого кадра — эпоха для `RawInput::time`.
static START: OnceLock<Instant> = OnceLock::new();

/// Момент предыдущего кадра.
static LAST_FRAME: Mutex<Option<Instant>> = Mutex::new(None);
static INSTALL_LOCK: Mutex<()> = Mutex::new(());
static ORIGINAL_SDL_SWAP: AtomicUsize = AtomicUsize::new(0);
static SWAP_SLOT: AtomicUsize = AtomicUsize::new(0);

#[link(name = "kernel32")]
unsafe extern "system" {
    fn VirtualProtect(address: *mut c_void, size: usize, protection: u32, old: *mut u32) -> i32;
}

/// Меняет слот импорта, сохраняя исходную защиту страницы.
unsafe fn swap_import(slot: usize, expected: usize, replacement: usize) -> bool {
    let mut old = 0;
    if unsafe { VirtualProtect(slot as *mut c_void, size_of::<usize>(), 0x04, &mut old) } == 0 {
        return false;
    }
    let cell = unsafe { &*(slot as *const AtomicUsize) };
    let changed = cell.compare_exchange(expected, replacement, Ordering::AcqRel, Ordering::Acquire).is_ok();
    let mut ignored = 0;
    if unsafe { VirtualProtect(slot as *mut c_void, size_of::<usize>(), old, &mut ignored) } == 0 {
        golden_sdk::log!("egui: не удалось восстановить защиту страницы импорта");
    }
    changed
}

pub(crate) fn ready() -> bool {
    GL_READY.load(Ordering::Acquire) && !UNINSTALL.load(Ordering::Acquire)
}

unsafe extern "C" fn sdl_swap_window(window: *mut c_void) {
    let _inside = golden_sdk::module::InFlight::enter();
    frame();
    let original = ORIGINAL_SDL_SWAP.load(Ordering::Acquire);
    if original != 0 {
        let original: unsafe extern "C" fn(*mut c_void) = unsafe { std::mem::transmute(original) };
        unsafe { original(window) };
    }
}

/// Поток установки: ждёт окно игры и загрузку `opengl32.dll`.
pub(crate) fn spawn_installer() {
    let spawned = std::thread::Builder::new()
        .name("golden-egui".to_string())
        .spawn(installer_main);
    if let Err(error) = spawned {
        golden_sdk::log!("egui: поток установки не запустился: {error}");
    }
}

fn installer_main() {
    // Поток принадлежит DLL: при выгрузке он должен выйти первым.
    let _token = golden_sdk::module::ThreadToken::new();

    // Окно игры для подмены оконной процедуры.
    let hwnd = loop {
        if golden_sdk::module::should_stop() {
            return;
        }
        if let Some(hwnd) = golden_std::input::game_window() {
            break hwnd;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let _lock = INSTALL_LOCK.lock().unwrap();
    if golden_sdk::module::should_stop() || UNINSTALL.load(Ordering::Acquire) {
        return;
    }
    if input::install(hwnd) {
        golden_sdk::log!("egui: оконная процедура подменена");
    } else {
        golden_sdk::log!("egui: не удалось подменить оконную процедуру");
    }

    drop(_lock);
    // hw.dll вызывает SDL_GL_SwapWindow через импорт, в том числе в меню.
    // Подменяем указатель, не меняя исполняемые байты из фонового потока.
    loop {
        if golden_sdk::module::should_stop() || UNINSTALL.load(Ordering::Acquire) {
            return;
        }
        if let (Some(base), Some(original)) = (
            golden_sdk::process::hw_base(),
            golden_sdk::process::export("SDL2.dll", "SDL_GL_SwapWindow"),
        ) {
            let _lock = INSTALL_LOCK.lock().unwrap();
            if golden_sdk::module::should_stop() || UNINSTALL.load(Ordering::Acquire) {
                return;
            }
            let slot = base + 0x1F93E4;
            if !golden_sdk::process::is_readable(slot) {
                golden_sdk::log!("egui: импорт SDL_GL_SwapWindow недоступен для чтения");
                return;
            }
            let previous = unsafe { golden_sdk::process::read::<usize>(slot) };
            if previous != original {
                golden_sdk::log!("egui: импорт SDL_GL_SwapWindow уже изменён; установка отменена");
                return;
            }
            ORIGINAL_SDL_SWAP.store(previous, Ordering::Release);
            if unsafe { swap_import(slot, previous, sdl_swap_window as usize) } {
                SWAP_SLOT.store(slot, Ordering::Release);
                golden_sdk::log!("egui: импорт SDL_GL_SwapWindow подменён");
            } else {
                golden_sdk::log!("egui: не удалось подменить импорт SDL_GL_SwapWindow");
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Начало выгрузки: просим рендер-поток освободить GL-объекты и ждём.
///
/// GL-объекты удаляются только в контексте рендер-потока. Если кадр так и не
/// пришёл (свёрнутое окно), выходим по таймауту — объекты доживут до выхода
/// игры, это безвредно.
pub(crate) fn begin_uninstall() {
    UNINSTALL.store(true, Ordering::Release);
    drop(INSTALL_LOCK.lock().unwrap());
    if GL_READY.load(Ordering::Acquire) {
        let deadline = Instant::now() + Duration::from_millis(500);
        while !DESTROYED.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if DESTROYED.load(Ordering::Acquire) {
            golden_sdk::log!("egui: GL-объекты освобождены");
        } else {
            golden_sdk::log!("egui: кадр не пришёл — GL-объекты останутся в контексте до выхода игры");
        }
    }
    let slot = SWAP_SLOT.swap(0, Ordering::AcqRel);
    if slot != 0 && !unsafe { swap_import(slot, sdl_swap_window as usize, ORIGINAL_SDL_SWAP.load(Ordering::Acquire)) } {
        golden_sdk::log!("egui: не удалось восстановить импорт SDL_GL_SwapWindow");
    }
}

/// Один кадр наложения. Вызывается из перехвата `wglSwapBuffers`.
fn frame() {
    if UNINSTALL.load(Ordering::Acquire) {
        let mut painter = PAINTER.lock().unwrap();
        if let Some(mut painter) = painter.take() {
            painter.destroy();
        }
        DESTROYED.store(true, Ordering::Release);
        return;
    }
    if let Ok(mut callback) = crate::FRAME.lock() {
        if let Some(callback) = callback.as_mut() {
            callback();
        }
    }
    if GL_READY.load(Ordering::Acquire) && !crate::visible() {
        return;
    }
    EXT.get_or_init(GlExts::resolve);
    let _gl = GlGuard::enter();
    if !ensure_gl() {
        return;
    }
    if !crate::visible() {
        return;
    }
    let Some(hwnd) = golden_std::input::game_window() else {
        return;
    };
    let Some((w, h)) = sys::client_size(hwnd as sys::Hwnd) else {
        return;
    };
    input::ensure_game_mouse_released();
    input::install_set_cursor_pos_hook();
    input::ensure_sdl_mouse_released();
    // Игра могла спрятать курсор на переходах — возвращаем.
    if !sys::cursor_visible() {
        input::request_cursor_update();
    }
    // GoldSrc может не прислать WM_MOUSEMOVE после выхода из relative mode.
    // Читаем абсолютную позицию напрямую, чтобы курсор egui не зависел от
    // того, какой путь ввода был активен у игры до открытия меню.
    input::feed_cursor_now();

    let now = Instant::now();
    let start = START.get_or_init(|| now);
    let dt = {
        let mut last = LAST_FRAME.lock().unwrap();
        let dt = last.map(|t| (now - t).as_secs_f32()).unwrap_or(1.0 / 60.0);
        *last = Some(now);
        dt.clamp(0.0001, 0.25)
    };

    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(w as f32, h as f32),
        )),
        time: Some(start.elapsed().as_secs_f64()),
        predicted_dt: dt,
        events: input::drain_events(),
        ..Default::default()
    };

    let ctx = CONTEXT.get_or_init(egui::Context::default).clone();
    let profile_start = Instant::now();
    let mut full = {
        let mut slot = crate::ui_slot().lock().unwrap();
        let Some(ui) = slot.as_mut() else {
            return;
        };
        ctx.begin_pass(raw);
        ui(&ctx);
        ctx.end_pass()
    };

    // Вывод платформы: курсор, буфер обмена, ссылки.
    input::set_cursor_icon(full.platform_output.cursor_icon);
    for command in &full.platform_output.commands {
        match command {
            egui::OutputCommand::CopyText(text) => sys::clipboard_set(text),
            egui::OutputCommand::OpenUrl(open) => sys::open_url(&open.url),
            _ => {}
        }
    }

    let clipped = ctx.tessellate(full.shapes, full.pixels_per_point);
    let ui_time = profile_start.elapsed();
    let mut painter = PAINTER.lock().unwrap();
    let Some(painter) = painter.as_mut() else {
        return;
    };
    // GoldSrc оставляет состояния fixed-function и параметры распаковки пикселей.
    // Они не сбрасываются egui_glow и могут отсечь глифы или испортить атлас.
    unsafe {
        sys::glDisable(0x0BC0); // GL_ALPHA_TEST
        sys::glDisable(0x0B90); // GL_STENCIL_TEST
        sys::glPixelStorei(0x0CF2, 0); // GL_UNPACK_ROW_LENGTH
        sys::glPixelStorei(0x0CF3, 0); // GL_UNPACK_SKIP_ROWS
        sys::glPixelStorei(0x0CF4, 0); // GL_UNPACK_SKIP_PIXELS
    }
    painter.paint_and_update_textures(
        [w, h],
        full.pixels_per_point,
        &clipped,
        &mut full.textures_delta,
    );
    record_timing(ui_time, profile_start.elapsed().saturating_sub(ui_time));
}

/// Среднее время CPU в миллисекундах; ожидание GPU здесь не измеряется.
fn record_timing(ui: Duration, paint: Duration) {
    static TIMING: Mutex<Option<(Instant, u32, Duration, Duration)>> = Mutex::new(None);
    let mut timing = TIMING.lock().unwrap();
    let entry = timing.get_or_insert((Instant::now(), 0, Duration::ZERO, Duration::ZERO));
    entry.1 += 1;
    entry.2 += ui;
    entry.3 += paint;
    if entry.0.elapsed() >= Duration::from_secs(5) {
        let n = entry.1 as f64;
        golden_sdk::log!(
            "egui: CPU среднее за {} кадров: UI {:.2} мс, GL {:.2} мс",
            entry.1,
            entry.2.as_secs_f64() * 1000.0 / n,
            entry.3.as_secs_f64() * 1000.0 / n
        );
        *entry = (Instant::now(), 0, Duration::ZERO, Duration::ZERO);
    }
}

/// Поднимает GL-контекст `glow` и рендерер egui (один раз, в рендер-потоке).
fn ensure_gl() -> bool {
    if GL_READY.load(Ordering::Acquire) {
        return true;
    }
    if GL_FAILED.load(Ordering::Acquire) {
        return false;
    }
    // SAFETY: мы в рендер-потоке, GL-контекст игры текущий.
    let gl = Arc::new(unsafe { glow::Context::from_loader_function(sys::gl_proc_address) });
    match Painter::new(gl.clone(), "", None, false) {
        Ok(painter) => {
            use glow::HasContext;
            // SAFETY: контекст текущий, вызовы без указателей.
            let version = unsafe { gl.get_parameter_string(glow::VERSION) };
            let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) };
            golden_sdk::log!("egui: GL-рендерер поднят ({renderer}, {version})");
            *PAINTER.lock().unwrap() = Some(painter);
            EXT.get_or_init(GlExts::resolve);
            GL_READY.store(true, Ordering::Release);
            true
        }
        Err(error) => {
            golden_sdk::log!("egui: GL-рендерер не поднялся: {error}");
            GL_FAILED.store(true, Ordering::Release);
            false
        }
    }
}

/// Сторож GL-состояния: возвращает всё, что мог задеть egui_glow.
///
/// Движок игры после чужой отрисовки не переустанавливает ни матрицы, ни
/// привязки — поэтому без сторожа следующая модель рисуется с нашими
/// шейдером, текстурой и скашеными матрицами. `glPushAttrib/glPopAttrib`
/// покрывают биты состояния, а указатели (текстура, FBO, программа, VAO)
/// атрибутами не сохраняются — их возвращаем вручную.
struct GlGuard {
    texture_2d: i32,
    active_texture: u32,
    framebuffer: i32,
    program: i32,
    vertex_array: i32,
}

impl GlGuard {
    fn enter() -> Self {
        // SAFETY: вызовы GL 1.1 в текущем контексте рендер-потока.
        unsafe {
            sys::glPushAttrib(sys::GL_ALL_ATTRIB_BITS);
            sys::glPushClientAttrib(sys::GL_CLIENT_ALL_ATTRIB_BITS);
            sys::glMatrixMode(sys::GL_PROJECTION);
            sys::glPushMatrix();
            sys::glMatrixMode(sys::GL_MODELVIEW);
            sys::glPushMatrix();
            sys::glMatrixMode(sys::GL_TEXTURE);
            sys::glPushMatrix();

            let mut texture_2d = 0;
            let mut active_texture = 0;
            sys::glGetIntegerv(sys::GL_TEXTURE_BINDING_2D, &mut texture_2d);
            sys::glGetIntegerv(sys::GL_ACTIVE_TEXTURE, &mut active_texture);

            let mut framebuffer = 0;
            let mut program = 0;
            let mut vertex_array = 0;
            if let Some(ext) = EXT.get() {
                if ext.bind_framebuffer.is_some() {
                    sys::glGetIntegerv(sys::GL_FRAMEBUFFER_BINDING, &mut framebuffer);
                }
                if ext.use_program.is_some() {
                    sys::glGetIntegerv(sys::GL_CURRENT_PROGRAM, &mut program);
                }
                if ext.bind_vertex_array.is_some() {
                    sys::glGetIntegerv(sys::GL_VERTEX_ARRAY_BINDING, &mut vertex_array);
                }
            }
            Self {
                texture_2d,
                active_texture: active_texture as u32,
                framebuffer,
                program,
                vertex_array,
            }
        }
    }
}

impl Drop for GlGuard {
    fn drop(&mut self) {
        // SAFETY: симметрично `enter`, в том же контексте.
        unsafe {
            if let Some(ext) = EXT.get() {
                if let Some(bind_framebuffer) = ext.bind_framebuffer {
                    bind_framebuffer(sys::GL_FRAMEBUFFER, self.framebuffer as u32);
                }
                if let Some(use_program) = ext.use_program {
                    use_program(self.program as u32);
                }
                if let Some(bind_vertex_array) = ext.bind_vertex_array {
                    bind_vertex_array(self.vertex_array as u32);
                }
                if let Some(active_texture) = ext.active_texture {
                    active_texture(self.active_texture);
                }
            }
            sys::glBindTexture(sys::GL_TEXTURE_2D, self.texture_2d as u32);

            sys::glMatrixMode(sys::GL_TEXTURE);
            sys::glPopMatrix();
            sys::glMatrixMode(sys::GL_PROJECTION);
            sys::glPopMatrix();
            sys::glMatrixMode(sys::GL_MODELVIEW);
            sys::glPopMatrix();

            sys::glPopClientAttrib();
            sys::glPopAttrib();
        }
    }
}

/// GL-функции сверх 1.1, которые мог потревожить `egui_glow`.
struct GlExts {
    active_texture: Option<unsafe extern "system" fn(u32)>,
    bind_framebuffer: Option<unsafe extern "system" fn(u32, u32)>,
    use_program: Option<unsafe extern "system" fn(u32)>,
    bind_vertex_array: Option<unsafe extern "system" fn(u32)>,
}

impl GlExts {
    fn resolve() -> Self {
        /// `*const c_void` → типизированный указатель; `None` для отсутствующих.
        unsafe fn cast<T>(ptr: *const c_void) -> Option<T> {
            if ptr.is_null() {
                None
            } else {
                // SAFETY: оба — указатели одного размера; тип функции совпадает с
                // сигнатурой GL-расширения.
                Some(unsafe { std::mem::transmute_copy::<*const c_void, T>(&ptr) })
            }
        }
        unsafe {
            Self {
                active_texture: cast(sys::gl_proc_address("glActiveTexture")),
                bind_framebuffer: cast(sys::gl_proc_address("glBindFramebuffer")),
                use_program: cast(sys::gl_proc_address("glUseProgram")),
                bind_vertex_array: cast(sys::gl_proc_address("glBindVertexArray")),
            }
        }
    }
}

// Указатели на функции потокобезопасны.
unsafe impl Send for GlExts {}
unsafe impl Sync for GlExts {}
