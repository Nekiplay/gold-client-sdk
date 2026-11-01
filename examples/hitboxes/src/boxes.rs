//! Хитбоксы игрока по той позе, которой движок рисует тело.
//!
//! Движок строит hull игрока (`hw.dll + 0x93440`) не сам: он вызывает расчёт
//! костей через таблицу движка, передавая ему `sequence`, `frame`, `angles`,
//! `origin`, а затем читает матрицы из общего буфера `hw + 0x29F2A58`. На
//! клиенте этим расчётом оказывается клиентский `StudioSetupBones`, то есть
//! hull строится из той же позы, в которой тело нарисовано.
//!
//! Поэтому боксы снимаются в перехвате отрисовки модели
//! (`StudioRenderModel`): сразу после оригинала буфер костей принадлежит
//! именно этой модели, и в нём лежит ровно та поза, что видна на экране.
//!
//! 1. `draw_player` — точка входа: оттуда ставятся остальные перехваты;
//! 2. `StudioRenderModel` — по матрицам нарисованной модели строятся боксы;
//! 3. конец 3D-сцены — боксы рисуются поверх кадра.
//!
//! Дальше — только сбор и рисование; установка перехватов в [`install`].

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use golden_sdk::engine::EntityRef;
use golden_sdk::module::InFlight;
use golden_sdk::renderer::{StudioInterface, StudioRenderer};
use golden_sdk::studio::{HitGroup, WorldHitbox};
use golden_std::render::DRAW_NORMAL_TRIANGLES_RVA;
use golden_std::{Color, Overlay};

/// Ширина линий хитбоксов.
const WIDTH: f32 = 1.5;

/// Как часто писать отчёт в лог.
const REPORT_EVERY: Duration = Duration::from_secs(5);

/// Тело, нарисованное в этом кадре, и его хитбоксы.
#[derive(Debug, Clone)]
struct Body {
    /// Слот игрока.
    index: i32,
    /// Адрес сущности — по нему тело опознаётся в кадре.
    addr: usize,
    /// Путь модели — по нему видно, чьи кости попали в набор.
    model: String,
    /// Середина корпуса (`origin` сущности).
    origin: golden_std::Vec3,
    /// Боксы по позе, которой тело нарисовано, — по ней игра трассирует выстрел.
    boxes: Vec<WorldHitbox>,
    /// Кости нарисованной позы.
    bones: Vec<golden_std::Vec3>,
    /// Рёбра скелета (родитель → кость) по этим костям.
    skeleton: Vec<(golden_std::Vec3, golden_std::Vec3)>,
}

impl Body {
    /// Кости нарисованной позы — их и рисуем точками.
    fn trace_bones(&self) -> impl Iterator<Item = &golden_std::Vec3> {
        self.bones.iter()
    }

    /// Боксы, которые рисуются.
    fn boxes(&self) -> &[WorldHitbox] {
        &self.boxes
    }
}

/// Тела, нарисованные с прошлого конца кадра.
static DRAWN: Mutex<Vec<Body>> = Mutex::new(Vec::new());

/// Попросили печать боксов (команда `hitboxes_dump`).
static DUMP: AtomicBool = AtomicBool::new(false);

/// Попросили печать костей (команда `hitboxes_bones`).
static BONE_DUMP: AtomicBool = AtomicBool::new(false);

/// Сколько тел и боксов нарисовано в последнем кадре: `(тела, боксы)`.
static LAST_FRAME: Mutex<(usize, usize)> = Mutex::new((0, 0));

/// Сколько раз движок позвал конец 3D-сцены.
static SCENE_FRAMES: AtomicU32 = AtomicU32::new(0);

/// Сколько раз клиент рисовал игрока через наш `draw_player`.
static DRAW_CALLS: AtomicU32 = AtomicU32::new(0);

/// Сколько тел нарисовано за всё время — для отчёта.
static BODIES: AtomicU32 = AtomicU32::new(0);

/// Указатель на клиентский `StudioDrawPlayer`, который мы оборачиваем.
static ORIGINAL_PLAYER: AtomicUsize = AtomicUsize::new(0);

/// Поставлен ли перехват конца сцены.
static SCENE_HOOKED: AtomicBool = AtomicBool::new(false);

/// Поставлен ли перехват отрисовки модели.
static RENDER_HOOKED: AtomicBool = AtomicBool::new(false);

/// Тела, которые движок нарисовал в этом кадре, с их хитбоксами.
static FRAME_BODIES: Mutex<Vec<Body>> = Mutex::new(Vec::new());

// Клиентский `StudioRenderModel`: сразу после него буфер костей принадлежит
// именно этой модели — тут и снимаем позу, которой она нарисована.
golden_std::hook! {
    unsafe extern "thiscall" fn hook_render_model(this: *mut c_void)
        original: render_model_original
    {
        let _inside = InFlight::enter();
        unsafe { render_model_original()(this) };
        remember_drawn_bones();
    }
}

/// Собирает тело по той позе, которой движок его только что нарисовал.
///
/// Кости берём сразу после оригинала: до этого вызова буфер ещё чужой, после —
/// он принадлежит этой модели и остаётся таким до следующей модели в кадре.
fn remember_drawn_bones() {
    let Some(renderer) = StudioRenderer::global() else {
        return;
    };
    let Some(entity) = renderer.current_entity() else {
        return;
    };
    let Some(model) = renderer.studio_model() else {
        return;
    };
    if !model.is_player_model() {
        return;
    }
    let Some(base) = renderer.bone_matrix_base() else {
        return;
    };
    let bones: Vec<golden_std::Vec3> = (0..model.bones().len())
        .filter_map(|bone| {
            // SAFETY: индекс ограничен числом костей модели.
            let matrix = unsafe { golden_sdk::studio::bone_matrix(base, bone) };
            matrix.is_finite().then(|| matrix.origin())
        })
        .collect();
    if bones.is_empty() {
        return;
    }
    let addr = entity.addr();
    // Боксы строим здесь же: по этим матрицам движок и трассирует выстрел, а
    // после смены модели в буфере уже чужая поза.
    let boxes = renderer.hitboxes_world_from(&model, base);
    let Some(raw) = EntityRef::from_raw(addr) else {
        return;
    };
    let body = Body {
        index: raw.player_index(),
        addr,
        model: raw.model_path().unwrap_or_default(),
        origin: raw.origin(),
        boxes,
        skeleton: model.skeleton_lines_with(&bones),
        bones,
    };
    if let Ok(mut list) = FRAME_BODIES.lock() {
        match list.iter_mut().find(|other| other.addr == addr) {
            Some(slot) => *slot = body,
            None => list.push(body),
        }
    }
}

/// Перехват `StudioRenderModel` через слот vtable объекта рендерера.
fn install_render_model() {
    if RENDER_HOOKED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(renderer) = StudioRenderer::global() else {
        RENDER_HOOKED.store(false, Ordering::Release);
        return;
    };
    let Some(entry) = renderer.method(golden_sdk::renderer::SLOT_RENDER_MODEL) else {
        RENDER_HOOKED.store(false, Ordering::Release);
        golden_sdk::log!("hitboxes: в vtable рендерера нет слота отрисовки меша");
        return;
    };
    // SAFETY: `hook_render_model` — функция того же ABI (`thiscall`).
    match unsafe { golden_std::hooks::install(entry, hook_render_model as *const ()) } {
        Ok(_) => golden_sdk::log!("hitboxes: перехват отрисовки модели установлен"),
        Err(error) => {
            RENDER_HOOKED.store(false, Ordering::Release);
            golden_sdk::log!("hitboxes: перехват отрисовки модели не встал: {error}");
        }
    }
}

static LAST_REPORT: Mutex<Option<Instant>> = Mutex::new(None);

/// Команда `hitboxes_dump`: печать уйдёт в ближайшем кадре.
pub fn request_dump() {
    DUMP.store(true, Ordering::Release);
}

/// Команда `hitboxes_bones`: печатает заодно все кости тела.
pub fn request_bone_dump() {
    DUMP.store(true, Ordering::Release);
    BONE_DUMP.store(true, Ordering::Release);
}

/// Подменяет интерфейс рендерера: наш `draw_player` — точка входа.
///
/// Интерфейс меняется целиком (`draw_player` — наш, `draw_model` — клиентский):
/// это единственный путь, который движок сам подхватывает и не кэширует по
/// адресу функции. Патчить байты `StudioDrawModel` нельзя — перехват на нём
/// роняет игру.
///
/// Остальные перехваты ставятся из первого вызова [`hook_draw_player`]: они
/// патчат байты функций, а делать это можно только из рендер-потока.
///
/// # Errors
/// Строка с причиной, если движок ещё не отдал интерфейс клиента.
pub fn install() -> Result<(), String> {
    let Some(current) = golden_sdk::renderer::studio_interface().cloned() else {
        return Err("движок ещё не выставил интерфейс рендерера".to_string());
    };
    let Some(original) = current.draw_player else {
        return Err("у интерфейса нет draw_player: клиентский рендерер ещё не активен".to_string());
    };
    ORIGINAL_PLAYER.store(original as usize, Ordering::Release);
    let ours = StudioInterface {
        version: StudioInterface::VERSION,
        // Модель в руках оставляем клиенту: она нам не нужна.
        draw_model: current.draw_model,
        draw_player: Some(hook_draw_player),
    };
    let ours = Box::leak(Box::new(ours)) as *mut StudioInterface;
    // SAFETY: интерфейс того же типа, что у клиента; `hooks::interface`
    // вернёт слот при выгрузке.
    unsafe { golden_std::hooks::interface(ours) }
        .map_err(|error| format!("подмена интерфейса не встала: {error}"))?;
    Ok(())
}

/// Ставит перехват конца 3D-сцены: там GL свободен и brush-модели уже нарисованы.
///
/// `HUD_DrawNormalTriangles` — обычная функция клиента, поэтому здесь годится
/// inline-детур ([`golden_std::hooks::install`]), а не подмена интерфейса.
fn install_scene_end() {
    if SCENE_HOOKED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(module) = golden_sdk::process::client_base() else {
        SCENE_HOOKED.store(false, Ordering::Release);
        golden_sdk::log!("hitboxes: client.dll ещё не загружен, конец сцены не перехвачен");
        return;
    };
    let target = module + DRAW_NORMAL_TRIANGLES_RVA;
    // SAFETY: `hook_scene_end` — функция того же ABI (`void(void)`).
    match unsafe { golden_std::hooks::install(target, hook_scene_end as *const ()) } {
        Ok(_) => golden_sdk::log!("hitboxes: перехват конца сцены установлен ({target:#x})"),
        Err(error) => {
            SCENE_HOOKED.store(false, Ordering::Release);
            golden_sdk::log!("hitboxes: перехват конца сцены не встал: {error}");
        }
    }
}

// Клиентский `StudioDrawPlayer`: единственная точка входа, которую движок
// подхватывает сам. Отсюда ставим остальные перехваты — мы уже в рендер-потоке.
//
// Оригинал берём из своей статики: интерфейс подменяется не через
// `hooks::install`, и в реестре оригиналов его нет.
unsafe extern "C" fn hook_draw_player(flags: i32, player_info: *mut c_void) -> i32 {
    let _inside = InFlight::enter();
    // SAFETY: `ORIGINAL_PLAYER` выставляется в `install` клиентским указателем.
    let result = unsafe { call_original_draw_player(flags, player_info) };
    if DRAW_CALLS.fetch_add(1, Ordering::Relaxed) == 0 {
        // Перехваты ставим из рендер-потока: править байты функций, которые
        // движок исполняет из другого потока, нельзя.
        install_scene_end();
        install_render_model();
    }
    result
}

/// Вызов клиентского `StudioDrawPlayer` по сохранённому указателю.
///
/// # Safety
/// [`ORIGINAL_PLAYER`] должен указывать на настоящий `StudioDrawPlayer`.
unsafe fn call_original_draw_player(flags: i32, player_info: *mut c_void) -> i32 {
    let original = ORIGINAL_PLAYER.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    // SAFETY: сигнатура из HLSDK: `int StudioDrawPlayer(int, player_info_s *)`.
    let function: unsafe extern "C" fn(i32, *mut c_void) -> i32 =
        unsafe { core::mem::transmute(original) };
    unsafe { function(flags, player_info) }
}

// Конец кадра: движок уже нарисовал мир, ящики и игроков, GL свободен.
//
// Рисуем **до** оригинала, как ESP: сразу после `HUD_DrawNormalTriangles`
// движок оставляет GL в своём состоянии (включённые массивы вершин и цветов),
// и сторонний вызов из драйвера на этом состоянии падает.
golden_std::hook! {
    unsafe extern "system" fn hook_scene_end()
        original: scene_end_original
    {
        let _inside = InFlight::enter();
        collect_bodies();
        draw_frame();
        unsafe { scene_end_original()() };
    }
}

/// Забирает тела, которые движок нарисовал в этом кадре.
///
/// Боксы уже построены в перехвате отрисовки по тем матрицам, которыми тело
/// нарисовано: буфер костей принадлежит модели только внутри её отрисовки.
fn collect_bodies() {
    let bodies = {
        let mut list = match FRAME_BODIES.lock() {
            Ok(list) => list,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut *list)
    };
    if bodies.is_empty() {
        return;
    }
    BODIES.fetch_add(bodies.len() as u32, Ordering::Relaxed);
    if let Ok(mut drawn) = DRAWN.lock() {
        *drawn = bodies;
    }
}

/// Рисует хитбоксы и в конце кадра чистит набор.
///
/// Вызывать в конце 3D-сцены, до оригинала: там GL свободен, а brush-модели
/// (ящики, контейнеры) уже нарисованы и не закрывают линии.
pub fn draw_frame() {
    let frame = SCENE_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    let bodies = match DRAWN.lock() {
        Ok(mut slot) => std::mem::take(&mut *slot),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };
    if bodies.is_empty() {
        if frame == 1 || frame % 600 == 0 {
            golden_sdk::log!(
                "hitboxes: конец сцены {frame}, тел нет, вызовов draw_player {}, overlay {}",
                DRAW_CALLS.load(Ordering::Relaxed),
                if Overlay::new().is_some() { "есть" } else { "нет" }
            );
        }
        return;
    }
    if DUMP.swap(false, Ordering::AcqRel) {
        dump(&bodies);
    }
    let boxes: usize = bodies.iter().map(|body| body.boxes().len()).sum();
    if let Ok(mut last) = LAST_FRAME.lock() {
        *last = (bodies.len(), boxes);
    }
    let Some(overlay) = Overlay::new() else {
        if frame == 1 || frame % 600 == 0 {
            golden_sdk::log!(
                "hitboxes: конец сцены {frame}, тел {}, боксов {boxes}, overlay не поднялся",
                bodies.len()
            );
        }
        return;
    };
    if frame == 1 || frame % 600 == 0 {
        golden_sdk::log!(
            "hitboxes: конец сцены {frame}, тел {}, боксов {boxes}, рисуем",
            bodies.len()
        );
    }
    for body in &bodies {
        // Рёбра скелета из тех же матриц, что и боксы: если линии лягут в
        // нарисованное тело, поза верная и расходится только расчёт боксов.
        for (from, to) in &body.skeleton {
            overlay.xray(&[(*from, *to)], Color::WHITE, WIDTH);
        }
        for hitbox in body.boxes() {
            // Голову рисуем толще: на тёмном шлеме тонкая линия теряется.
            let width = if hitbox.group == HitGroup::Head { WIDTH * 3.0 } else { WIDTH };
            overlay.xray(&hitbox.edges(), color(hitbox.group), width);
        }
        // Точки костей из тех же матриц: если они лежат внутри тела, матрицы
        // правильные и расходится именно расчёт боксов.
        for bone in body.trace_bones() {
            overlay.xray(&cross(*bone), Color::WHITE, WIDTH);
        }
    }
}

/// Маленький крестик в точке кости.
fn cross(point: golden_std::Vec3) -> [(golden_std::Vec3, golden_std::Vec3); 3] {
    const LEN: f32 = 0.8;
    [
        (
            golden_std::Vec3::new(point.x - LEN, point.y, point.z),
            golden_std::Vec3::new(point.x + LEN, point.y, point.z),
        ),
        (
            golden_std::Vec3::new(point.x, point.y - LEN, point.z),
            golden_std::Vec3::new(point.x, point.y + LEN, point.z),
        ),
        (
            golden_std::Vec3::new(point.x, point.y, point.z - LEN),
            golden_std::Vec3::new(point.x, point.y, point.z + LEN),
        ),
    ]
}

/// Цвет по группе попадания: голова — сиреневый (красный занят подписями
/// HUD), руки — голубые, ноги — синие.
fn color(group: HitGroup) -> Color {
    match group {
        HitGroup::Head => Color::rgba(1.0, 0.1, 1.0, 1.0),
        HitGroup::Chest => Color::YELLOW,
        HitGroup::Stomach => Color::rgba(1.0, 0.6, 0.2, 1.0),
        HitGroup::LeftArm | HitGroup::RightArm => Color::CYAN,
        HitGroup::LeftLeg | HitGroup::RightLeg => Color::rgba(0.4, 0.6, 1.0, 1.0),
        HitGroup::Generic | HitGroup::Other(_) => Color::WHITE,
    }
}

/// Печатает боксы всех нарисованных тел.
fn dump(bodies: &[Body]) {
    let overlay = Overlay::new();
    let screen = |point: golden_std::Vec3| match &overlay {
        Some(overlay) => overlay
            .project(point)
            .map(|pos| format!("{:.0},{:.0}", pos.x, pos.y))
            .unwrap_or_else(|| "-".to_string()),
        None => "нет overlay".to_string(),
    };
    for body in bodies {
        let trace_ptr = StudioRenderer::trace_bones().map(|ptr| ptr as usize).unwrap_or(0);
        golden_sdk::log!("hitboxes_dump: буфер костей движка {trace_ptr:08x}");
        golden_sdk::log!(
            "hitboxes_dump: слот {}, модель {}, тело {:.0} {:.0} {:.0} (экран {}), боксов {}",
            body.index,
            body.model,
            body.origin.x,
            body.origin.y,
            body.origin.z,
            screen(body.origin),
            body.boxes().len()
        );
        if let Some(head) = body.boxes().iter().find(|hitbox| hitbox.group == HitGroup::Head) {
            golden_sdk::log!(
                "hitboxes_dump:   голова центр {:.1} {:.1} {:.1}, кость {}, экран {}",
                head.center().x,
                head.center().y,
                head.center().z,
                head.bone,
                screen(head.center())
            );
            // Сверка с файлом модели: в `arctic.mdl` у бокса головы
            // `bbmin=(-0.3,-4.5,-2.5)`, `bbmax=(8.0,4.5,2.5)`, `bone=7`.
            golden_sdk::log!(
                "hitboxes_dump:   голова локально min {:.1} {:.1} {:.1}, max {:.1} {:.1} {:.1}, \
                 оси кости x {:.2} {:.2} {:.2} | y {:.2} {:.2} {:.2} | z {:.2} {:.2} {:.2}",
                head.local_min.x,
                head.local_min.y,
                head.local_min.z,
                head.local_max.x,
                head.local_max.y,
                head.local_max.z,
                head.matrix.m[0],
                head.matrix.m[1],
                head.matrix.m[2],
                head.matrix.m[4],
                head.matrix.m[5],
                head.matrix.m[6],
                head.matrix.m[8],
                head.matrix.m[9],
                head.matrix.m[10]
            );
        }
        if BONE_DUMP.swap(false, Ordering::AcqRel) {
            dump_bones(body);
        }
        // Чья это поза: корень скелета должен стоять там же, где тело.
        if let Some(root) = body.bones.first() {
            let dx = root.x - body.origin.x;
            let dy = root.y - body.origin.y;
            golden_sdk::log!(
                "hitboxes_dump:   кость 0 в {:.1} {:.1} {:.1}, от тела по горизонтали {:.1}",
                root.x,
                root.y,
                root.z,
                (dx * dx + dy * dy).sqrt()
            );
        }
        for hitbox in body.boxes() {
            let center = hitbox.center();
            let size = hitbox.size();
            golden_sdk::log!(
                "hitboxes_dump:   {} кость {}, центр {:.1} {:.1} {:.1}, aabb {:.1}x{:.1}x{:.1}",
                hitbox.group.name(),
                hitbox.bone,
                center.x,
                center.y,
                center.z,
                size.x,
                size.y,
                size.z
            );
        }
    }
}

/// Печатает кости тела: индекс и высоту относительно `origin`.
fn dump_bones(body: &Body) {
    let origin = body.origin;
    let mut highest = (0usize, f32::MIN);
    for (index, point) in body.bones.iter().enumerate() {
        let up = point.z - origin.z;
        golden_sdk::log!("hitboxes_dump:   кость {index:2}: над телом {up:+.1}");
        if up > highest.1 {
            highest = (index, up);
        }
    }
    golden_sdk::log!(
        "hitboxes_dump: самая высокая кость {} на {:+.1} выше тела",
        highest.0,
        highest.1
    );
}

/// Строка отчёта, если с прошлого раза прошло достаточно времени.
pub fn take_report() -> Option<String> {
    {
        let mut last = LAST_REPORT.lock().ok()?;
        if last.is_some_and(|at: Instant| at.elapsed() < REPORT_EVERY) {
            return None;
        }
        *last = Some(Instant::now());
    }
    let (count, boxes) = LAST_FRAME.lock().map(|last| *last).unwrap_or((0, 0));
    Some(format!(
        "hitboxes: тел {}, боксов {}, кадров {}",
        count,
        boxes,
        BODIES.load(Ordering::Relaxed)
    ))
}