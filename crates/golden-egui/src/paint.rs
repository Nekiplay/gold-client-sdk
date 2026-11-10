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
static ORIGINAL_PLAYER: AtomicUsize = AtomicUsize::new(0);
static SWAP_INSTALLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn ready() -> bool {
    GL_READY.load(Ordering::Acquire) && !UNINSTALL.load(Ordering::Acquire)
}

unsafe extern "C" fn draw_player(flags: i32, player: *mut c_void) -> i32 {
    let _inside = golden_sdk::module::InFlight::enter();
    if !SWAP_INSTALLED.load(Ordering::Acquire) && !golden_sdk::module::should_stop() {
        let _lock = INSTALL_LOCK.lock().unwrap();
        if !UNINSTALL.load(Ordering::Acquire) && !SWAP_INSTALLED.load(Ordering::Acquire) {
            if let Some(target) = sys::opengl_export("wglSwapBuffers") {
                match unsafe { golden_std::hooks::install(target, hook_swap_buffers as *const ()) }
                {
                    Ok(_) => {
                        SWAP_INSTALLED.store(true, Ordering::Release);
                        golden_sdk::log!("egui: wglSwapBuffers перехвачен");
                    }
                    Err(error) => {
                        SWAP_INSTALLED.store(true, Ordering::Release);
                        golden_sdk::log!("egui: ошибка перехвата: {error}");
                    }
                }
            }
        }
    }
    let original = ORIGINAL_PLAYER.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    let original: unsafe extern "C" fn(i32, *mut c_void) -> i32 =
        unsafe { std::mem::transmute(original) };
    unsafe { original(flags, player) }
}

golden_std::hook! {
    /// Конец кадра: поверх сцены игры рисуем egui, затем смена буферов.
    unsafe extern "system" fn hook_swap_buffers(hdc: *mut c_void) original: swap_buffers_original -> i32 {
        let _inside = golden_sdk::module::InFlight::enter();
        frame();
        // SAFETY: трамплин выставлен `hooks::install`; сигнатура совпадает.
        unsafe { swap_buffers_original()(hdc) }
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
    // Ждём интерфейс; детур будет установлен только в рендер-потоке.
    loop {
        if golden_sdk::module::should_stop() || UNINSTALL.load(Ordering::Acquire) {
            return;
        }
        if golden_sdk::renderer::StudioRenderer::global().is_some_and(|r| r.is_initialized()) {
            let _lock = INSTALL_LOCK.lock().unwrap();
            if golden_sdk::module::should_stop() || UNINSTALL.load(Ordering::Acquire) {
                return;
            }
            if let Some(base) = golden_sdk::process::client_base() {
                let table = (base + golden_sdk::offsets::client::STUDIO_INTERFACE)
                    as *mut golden_sdk::renderer::StudioInterface;
                let slot = unsafe { std::ptr::addr_of_mut!((*table).draw_player) } as usize;
                if !golden_sdk::process::is_writable(slot) {
                    golden_sdk::log!("egui: слот StudioDrawPlayer недоступен для записи");
                    return;
                }
                let original = unsafe { (*table).draw_player };
                if let Some(original) = original {
                    ORIGINAL_PLAYER.store(original as usize, Ordering::Release);
                    if let Err(error) =
                        unsafe { golden_std::hooks::interface_player(draw_player as *const ()) }
                    {
                        golden_sdk::log!("egui: слот StudioDrawPlayer не установлен: {error}");
                    } else {
                        golden_sdk::log!("egui: слот StudioDrawPlayer подменён; ждём рендер-поток");
                    }
                    return;
                }
            }
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
    if !GL_READY.load(Ordering::Acquire) {
        return;
    }
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
        sys::show_cursor(true);
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
