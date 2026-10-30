//! Р”РѕСЃС‚СѓРї Рє РєР»РёРµРЅС‚СЃРєРѕРјСѓ СЂРµРЅРґРµСЂРµСЂСѓ СЃС‚СѓРґРёР№РЅС‹С… РјРѕРґРµР»РµР№.
//!
//! РћР±СЉРµРєС‚ `CStudioModelRenderer` (`client.dll + 0x1F51C8`) вЂ” С‚Рѕ СЃР°РјРѕРµ РјРµСЃС‚Рѕ, РіРґРµ
//! РїРѕСЃР»Рµ РєР°Р¶РґРѕРіРѕ РєР°РґСЂР° Р»РµР¶Р°С‚ РіРѕС‚РѕРІС‹Рµ РјР°С‚СЂРёС†С‹ РєРѕСЃС‚РµР№ РЅР°СЂРёСЃРѕРІР°РЅРЅРѕР№ РјРѕРґРµР»Рё. Р§РёС‚Р°С‚СЊ
//! РёС… РґРµС€РµРІР»Рµ Рё Р±РµР·РѕРїР°СЃРЅРµРµ, С‡РµРј СЃС‡РёС‚Р°С‚СЊ СЃРєРµР»РµС‚ СЃР°РјРѕРјСѓ: РґРІРёР¶РѕРє СѓР¶Рµ РїРѕСЃС‡РёС‚Р°Р»
//! РёРЅС‚РµСЂРїРѕР»СЏС†РёСЋ Р°РЅРёРјР°С†РёРё, РєРѕРЅС‚СЂРѕР»Р»РµСЂС‹ Рё РїСЂРёРІСЏР·РєСѓ Рє `attachment`.
//!
//! РџРѕСЂСЏРґРѕРє РёСЃРїРѕР»СЊР·РѕРІР°РЅРёСЏ РІ РєР°РґСЂРµ:
//!
//! 1. [`StudioRenderer::global`] вЂ” РѕР±СЉРµРєС‚ (Р¶РёРІС‘С‚ СЃ Р·Р°РіСЂСѓР·РєРё РєР»РёРµРЅС‚Р°).
//! 2. [`StudioRenderer::is_initialized`] вЂ” РґРІРёР¶РѕРє СѓР¶Рµ РѕС‚РґР°Р» РёРЅС‚РµСЂС„РµР№СЃ.
//! 3. [`StudioRenderer::studio_model`] вЂ” РјРѕРґРµР»СЊ, РєРѕС‚РѕСЂСѓСЋ СЃРµР№С‡Р°СЃ СЂРёСЃРѕРІР°Р»Рё.
//! 4. [`StudioRenderer::bone_origin`] вЂ” РїРѕР·РёС†РёРё РєРѕСЃС‚РµР№ РІ РјРёСЂРµ.

use core::ffi::{c_void, CStr};

use crate::engine::EntityRef;
use crate::math::Vec3;
use crate::offsets::{client, hw, renderer};
use crate::process;

/// Слот vtable, который рисует геометрию модели.
///
/// Разбор vtable (`client.dll`): `[19]` `0x100B0410` — «нарисовать тело»,
/// внутри по `+0x50` вызывает `[20]` `0x100B0AC0`, а тот уже гонит точки через
/// `[21]`/`[22]` (`0x100B07B0` / `0x100B08E0`). Перехватывать надо `[20]`:
/// это единственная точка входа отрисовки меша.
pub const SLOT_RENDER_MODEL: usize = 20;
use crate::studio::{
    BoneMatrix, HitGroup, StudioHeader, StudioModel, WorldHitbox, MAXSTUDIOBONES,
};

/// РРЅС‚РµСЂС„РµР№СЃ СЂРµРЅРґРµСЂРµСЂР°, РєРѕС‚РѕСЂС‹Р№ РєР»РёРµРЅС‚ РѕС‚РґР°С‘С‚ РґРІРёР¶РєСѓ (`r_studio_interface_t`).
#[repr(C)]
pub struct StudioInterface {
    /// Р”РІРёР¶РѕРє РїСЂРёРЅРёРјР°РµС‚ С‚РѕР»СЊРєРѕ `1`.
    pub version: i32,
    pub draw_model: Option<unsafe extern "C" fn(i32) -> i32>,
    pub draw_player: Option<unsafe extern "C" fn(i32, *mut c_void) -> i32>,
}

impl core::fmt::Debug for StudioInterface {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StudioInterface")
            .field("version", &self.version)
            .field("draw_model", &self.draw_model.map(|p| p as usize))
            .field("draw_player", &self.draw_player.map(|p| p as usize))
            .finish()
    }
}

impl StudioInterface {
    pub const VERSION: i32 = 1;

    /// Р’С‹Р·С‹РІР°РµС‚ `StudioDrawModel` СЌС‚РѕРіРѕ РёРЅС‚РµСЂС„РµР№СЃР°.
    ///
    /// РўР°Рє РёР· СЃРІРѕРµР№ РїРѕРґРјРµРЅС‹ Р·РѕРІСѓС‚ РѕСЂРёРіРёРЅР°Р»: `prev.call_draw_model(flags)`.
    ///
    /// # Safety
    /// РўРѕР»СЊРєРѕ РёР· С…СѓРєР° РѕС‚СЂРёСЃРѕРІРєРё РјРѕРґРµР»РµР№ (СЂРµРЅРґРµСЂ-РїРѕС‚РѕРє СЃ Р°РєС‚РёРІРЅС‹Рј GL-РєРѕРЅС‚РµРєСЃС‚РѕРј);
    /// `self` РґРѕР»Р¶РµРЅ СѓРєР°Р·С‹РІР°С‚СЊ РЅР° Р¶РёРІРѕР№ РёРЅС‚РµСЂС„РµР№СЃ.
    pub unsafe fn call_draw_model(&self, flags: i32) -> i32 {
        match self.draw_model {
            // SAFETY: РєРѕРЅС‚СЂР°РєС‚ РїРµСЂРµРґР°РЅ РІС‹Р·С‹РІР°СЋС‰РёРј.
            Some(f) => unsafe { f(flags) },
            None => 0,
        }
    }

    /// Р’С‹Р·С‹РІР°РµС‚ `StudioDrawPlayer` СЌС‚РѕРіРѕ РёРЅС‚РµСЂС„РµР№СЃР°.
    ///
    /// # Safety
    /// РЎРј. [`StudioInterface::call_draw_model`]; `player_info` вЂ” С‚Рѕ, С‡С‚Рѕ РїРµСЂРµРґР°Р»
    /// РґРІРёР¶РѕРє.
    pub unsafe fn call_draw_player(&self, flags: i32, player_info: *mut c_void) -> i32 {
        match self.draw_player {
            // SAFETY: РєРѕРЅС‚СЂР°РєС‚ РїРµСЂРµРґР°РЅ РІС‹Р·С‹РІР°СЋС‰РёРј.
            Some(f) => unsafe { f(flags, player_info) },
            None => 0,
        }
    }
}

/// РђРґСЂРµСЃ РїРѕР»СЏ `g_pStudioInterface` РІ `hw.dll` вЂ” С‚РѕС‡РєР° РїРѕРґРјРµРЅС‹ СЂРµРЅРґРµСЂРµСЂР°.
pub fn interface_slot() -> Option<*mut *mut StudioInterface> {
    let base = process::hw_base()?;
    Some((base + hw::P_STUDIO_INTERFACE) as *mut *mut StudioInterface)
}

/// РўРµРєСѓС‰РёР№ РёРЅС‚РµСЂС„РµР№СЃ, РєРѕС‚РѕСЂС‹Р№ РІРёРґРёС‚ РґРІРёР¶РѕРє.
pub fn studio_interface() -> Option<&'static StudioInterface> {
    let slot = interface_slot()?;
    // SAFETY: СЃР»РѕС‚ СЃСѓС‰РµСЃС‚РІСѓРµС‚, Р·РЅР°С‡РµРЅРёРµ РјРѕР¶РµС‚ Р±С‹С‚СЊ null вЂ” РїСЂРѕРІРµСЂСЏРµРј.
    let ptr = unsafe { *slot };
    if ptr.is_null() {
        return None;
    }
    // SAFETY: РґРІРёР¶РѕРє РѕР¶РёРґР°РµС‚ СЂРѕРІРЅРѕ С‚Р°РєСѓСЋ СЃС‚СЂСѓРєС‚СѓСЂСѓ.
    Some(unsafe { &*ptr })
}

/// РђРєС‚РёРІРµРЅ Р»Рё РєР»РёРµРЅС‚СЃРєРёР№ СЂРµРЅРґРµСЂРµСЂ (Р° РЅРµ РґРµС„РѕР»С‚РЅС‹Р№ РґРІРёР¶РєРѕРІС‹Р№).
///
/// РџРѕРєР° РєР»РёРµРЅС‚ РЅРµ Р·Р°РіСЂСѓР·РёР»СЃСЏ, РІ СЃР»РѕС‚Рµ Р»РµР¶РёС‚ `hw::DEFAULT_STUDIO_INTERFACE` вЂ” Сѓ
/// РЅРµРіРѕ РЅРµС‚ РєРѕСЃС‚РµР№ (`m_pbonetransform` РєР»РёРµРЅС‚СЃРєРѕРіРѕ РѕР±СЉРµРєС‚Р° РїСѓСЃС‚), РїРѕСЌС‚РѕРјСѓ
/// СЂРёСЃРѕРІР°С‚СЊ РїРѕ РЅРµРјСѓ РЅРµС‡РµРіРѕ. РЈРґРѕР±РЅРѕ РєР°Рє СѓСЃР»РѕРІРёРµ В«РјРѕР¶РЅРѕ РїРѕРґРјРµРЅСЏС‚СЊВ».
pub fn is_client_renderer_active() -> bool {
    let slot = match interface_slot() {
        Some(slot) => slot,
        None => return false,
    };
    let base = match process::hw_base() {
        Some(base) => base,
        None => return false,
    };
    // SAFETY: СЃР»РѕС‚ вЂ” СЃС‚Р°С‚РёС‡РµСЃРєРѕРµ РїРѕР»Рµ РґРІРёР¶РєР°, Р·РЅР°С‡РµРЅРёРµ Р»РёР±Рѕ null, Р»РёР±Рѕ РёРЅС‚РµСЂС„РµР№СЃ.
    let current = unsafe { *slot } as usize;
    // РџРѕСЃР»Рµ РїРѕРґРјРµРЅС‹ РІ СЃР»РѕС‚Рµ СЃС‚РѕРёС‚ СѓР¶Рµ РЅР°С€ РёРЅС‚РµСЂС„РµР№СЃ, РїРѕСЌС‚РѕРјСѓ СЃСЂР°РІРЅРёРІР°РµРј С‚РѕР»СЊРєРѕ
    // СЃ РґРµС„РѕР»С‚РЅС‹Рј РґРІРёР¶РєРѕРІС‹Рј: РёРЅР°С‡Рµ РїСЂРѕРІРµСЂРєР° Р»РѕР¶РЅРѕ РіРѕРІРѕСЂРёС‚ В«РЅРµ РіРѕС‚РѕРІРѕВ».
    current != 0 && current != base + hw::DEFAULT_STUDIO_INTERFACE
}

/// РћР±СЉРµРєС‚ `CStudioModelRenderer`.
#[derive(Debug, Clone, Copy)]
pub struct StudioRenderer {
    ptr: *mut u8,
}

/// РЎРЅРёРјРѕРє СЃРѕСЃС‚РѕСЏРЅРёСЏ СЂРµРЅРґРµСЂРµСЂР° (СЃРј. [`StudioRenderer::snapshot`]).
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    /// РђРґСЂРµСЃ РѕР±СЉРµРєС‚Р°.
    pub renderer: usize,
    /// РЈРґР°Р»РѕСЃСЊ Р»Рё РїСЂРѕС‡РёС‚Р°С‚СЊ СЃР°Рј РѕР±СЉРµРєС‚.
    pub readable: bool,
    /// РџРµСЂРІРѕРµ РїРѕР»Рµ РѕР±СЉРµРєС‚Р° вЂ” vtable.
    pub vtable: usize,
    /// РџСЂРѕС€Р»Рё Р»Рё РїСЂРѕРІРµСЂРєРё РёРЅРёС†РёР°Р»РёР·Р°С†РёРё.
    pub initialized: bool,
    /// `+0x30`: СЃСѓС‰РЅРѕСЃС‚СЊ, РєРѕС‚РѕСЂСѓСЋ СЂРёСЃСѓРµРј.
    pub current_entity: usize,
    /// РўРёРї/СЃРѕСЃС‚РѕСЏРЅРёРµ РјРѕРґРµР»Рё СЃСѓС‰РЅРѕСЃС‚Рё (`+0x304`).
    pub entity_state: i32,
    /// `model_t *` СЃСѓС‰РЅРѕСЃС‚Рё (`+0xB94`).
    pub entity_model: usize,
    /// `studiohdr_t *`, РІР·СЏС‚С‹Р№ РёР· `model_t` СЃСѓС‰РЅРѕСЃС‚Рё (`+0x184`).
    pub entity_header: usize,
    /// `studiohdr_t *` РёР· РїРѕР»СЏ СЂРµРЅРґРµСЂРµСЂР° `+0x4C`.
    pub studio_header: usize,
    /// `IDST` Р·Р°РіРѕР»РѕРІРєР° (`0x54534449` вЂ” РїСЂР°РІРёР»СЊРЅС‹Р№).
    pub header_id: i32,
    /// Р’РµСЂСЃРёСЏ Р·Р°РіРѕР»РѕРІРєР° (РґРѕР»Р¶РЅР° Р±С‹С‚СЊ 10).
    pub header_version: i32,
    /// Р§РёСЃР»Рѕ РєРѕСЃС‚РµР№ РІ Р·Р°РіРѕР»РѕРІРєРµ.
    pub header_bones: i32,
    /// `m_pbonetransform` вЂ” РЅР°С‡Р°Р»Рѕ РјР°СЃСЃРёРІР° РјР°С‚СЂРёС†.
    pub bone_base: usize,
    /// РџРµСЂРІР°СЏ РјР°С‚СЂРёС†Р° РєРѕСЃС‚РµР№ (С‚СЂР°РЅСЃР»СЏС†РёСЏ РІ `m[3]`, `m[7]`, `m[11]`).
    pub first_bone: BoneMatrix,
}

/// Вызов метода объекта, который в GoldSrc использует соглашение `thiscall`.
///
/// На x86 это настоящий `thiscall` (указатель на объект в `ECX`); на других
/// архитектурах — только для сборки тестов, поэтому `C`.
#[cfg(target_arch = "x86")]
type ThiscallVoid = unsafe extern "thiscall" fn(*mut core::ffi::c_void);
/// См. [`ThiscallVoid`].
#[cfg(not(target_arch = "x86"))]
type ThiscallVoid = unsafe extern "C" fn(*mut core::ffi::c_void);

impl StudioRenderer {
    /// Р“Р»РѕР±Р°Р»СЊРЅС‹Р№ РѕР±СЉРµРєС‚ РєР»РёРµРЅС‚Р° РёР»Рё `None`, РµСЃР»Рё РєР»РёРµРЅС‚ РµС‰С‘ РЅРµ Р·Р°РіСЂСѓР¶РµРЅ.
    pub fn global() -> Option<Self> {
        let base = process::client_base()?;
        Some(Self { ptr: (base + client::STUDIO_RENDERER) as *mut u8 })
    }

    pub fn addr(&self) -> usize {
        self.ptr as usize
    }

    /// РђРґСЂРµСЃ РјРµС‚РѕРґР° РёР· vtable (РїСЂРѕРІРµСЂСЏСЏ, С‡С‚Рѕ vtable вЂ” РѕР¶РёРґР°РµРјР°СЏ).
    pub fn method(&self, slot: usize) -> Option<usize> {
        let vtable = self.vtable();
        if vtable == 0 || slot > 0x40 {
            return None;
        }
        // SAFETY: vtable РїСЂРѕС‡РёС‚Р°РЅ РёР· РѕР±СЉРµРєС‚Р°; С‡РёС‚Р°РµРј СЃР»РѕС‚.
        let f = unsafe { process::read::<usize>(vtable + slot * core::mem::size_of::<usize>()) };
        (f != 0).then_some(f)
    }

    pub fn vtable(&self) -> usize {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№, РїРµСЂРІС‹Р№ dword вЂ” vtable.
        unsafe { process::read(self.addr() + renderer::VTABLE) }
    }

    /// Перерисовывает модель, которую рендерер уже подготовил, в виде каркаса.
    ///
    /// Вызывать только внутри перехвата отрисовки игрока, после оригинала:
    /// к этому моменту у рендерера выставлены заголовок модели, кости и светило,
    /// поэтому слот [`SLOT_RENDER_MODEL`] рисует ровно ту же геометрию, что и
    /// тело, — получаются контуры самой модели, а не коробка вокруг неё.
    ///
    /// `false`, если слот недоступен или GL ещё не поднят.
    pub fn render_wireframe(&self, color: crate::draw::Color, width: f32) -> bool {
        let Some(entry) = self.method(SLOT_RENDER_MODEL) else {
            return false;
        };
        // SAFETY: `entry` — `void StudioRenderModel(void)` этого объекта; адрес
        // взят из живой vtable, сигнатура без аргументов (thiscall).
        let render: ThiscallVoid = unsafe { core::mem::transmute(entry) };
        let Some(gl) = (unsafe { crate::draw::Gl::get() }) else {
            return false;
        };
        let this = self.ptr as *mut core::ffi::c_void;
        // SAFETY: GL-контекст активен в этом потоке.
        unsafe {
            gl.wireframe(color, width, || {
                // SAFETY: тот же контракт, что и у перехвата отрисовки.
                render(this);
            });
        }
        true
    }

    /// РџРѕС…РѕР¶ Р»Рё РѕР±СЉРµРєС‚ РЅР° РёРЅРёС†РёР°Р»РёР·РёСЂРѕРІР°РЅРЅС‹Р№ СЂРµРЅРґРµСЂРµСЂ.
    ///
    /// РџСЂРёР·РЅР°Рє: vtable СѓРєР°Р·С‹РІР°РµС‚ РІРЅСѓС‚СЂСЊ РєР»РёРµРЅС‚Р° Рё РЅР°С‡РёРЅР°РµС‚СЃСЏ СЃ `Init`, РєРѕС‚РѕСЂС‹Р№
    /// Р»РµР¶РёС‚ РІ `.text` РєР»РёРµРЅС‚Р°.
    pub fn is_initialized(&self) -> bool {
        let client_base = match process::client_base() {
            Some(base) => base,
            None => return false,
        };
        let vtable = self.vtable();
        if !process::in_range(vtable, client_base, 0x20_0000) {
            return false;
        }
        self.method(client::VT_INIT)
            .is_some_and(|f| process::is_code(f, client_base))
    }

    /// РЎСѓС‰РЅРѕСЃС‚СЊ, РєРѕС‚РѕСЂСѓСЋ СЂРµРЅРґРµСЂРµСЂ РѕР±СЂР°Р±Р°С‚С‹РІР°РµС‚ СЃРµР№С‡Р°СЃ.
    pub fn current_entity(&self) -> Option<EntityRef> {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        let ptr: *mut c_void = unsafe { process::read(self.addr() + renderer::CURRENT_ENTITY) };
        // SAFETY: СѓРєР°Р·Р°С‚РµР»СЊ РёР· РґРІРёР¶РєР° Р»РёР±Рѕ null.
        unsafe { EntityRef::from_ptr(ptr) }
    }

    /// `true`, РµСЃР»Рё СЂРёСЃСѓРµС‚СЃСЏ РјРѕРґРµР»СЊ Р»РѕРєР°Р»СЊРЅРѕРіРѕ РёРіСЂРѕРєР° (view entity).
    pub fn is_player(&self) -> bool {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        unsafe { process::read::<u8>(self.addr() + renderer::IS_PLAYER) != 0 }
    }

    /// `model_t *`, РєРѕС‚РѕСЂС‹Р№ СЃРµР№С‡Р°СЃ СЂРёСЃСѓРµС‚СЃСЏ.
    pub fn render_model(&self) -> *mut c_void {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        unsafe { process::read(self.addr() + renderer::RENDER_MODEL) }
    }

    /// РЎС‹СЂРѕР№ СѓРєР°Р·Р°С‚РµР»СЊ РЅР° `studiohdr_t`.
    pub fn studio_header(&self) -> *const StudioHeader {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        unsafe { process::read(self.addr() + renderer::STUDIO_HEADER) }
    }

    /// Р Р°Р·РѕР±СЂР°РЅРЅР°СЏ РјРѕРґРµР»СЊ, РєРѕС‚РѕСЂСѓСЋ СЂРёСЃРѕРІР°Р»Рё РІ СЌС‚РѕРј РєР°РґСЂРµ.
    pub fn studio_model(&self) -> Option<StudioModel> {
        // SAFETY: Р·Р°РіРѕР»РѕРІРѕРє РґРІРёР¶РѕРє РґРµСЂР¶РёС‚ РІР°Р»РёРґРЅС‹Рј РЅР° РІСЂРµРјСЏ СЂРµРЅРґРµСЂР°.
        unsafe { StudioModel::from_ptr(self.studio_header()) }
    }

    /// РџРѕС…РѕР¶Рµ Р»Рё С‚Рѕ, С‡С‚Рѕ СЃРµР№С‡Р°СЃ СЂРёСЃСѓРµС‚СЃСЏ, РЅР° РјРѕРґРµР»СЊ РёРіСЂРѕРєР° (Р° РЅРµ РѕСЂСѓР¶РёСЏ/РїСЂРѕРїР°).
    ///
    /// РљСЂРёС‚РµСЂРёР№ вЂ” РїСѓС‚СЊ РјРѕРґРµР»Рё: Сѓ РёРіСЂРѕРєРѕРІ РѕРЅ `models/player/<РјРѕРґРµР»СЊ>/<РјРѕРґРµР»СЊ>.mdl`,
    /// Сѓ view-model вЂ” `models/v_*.mdl`, Сѓ РїСЂРѕРїРѕРІ вЂ” С‡С‚Рѕ СѓРіРѕРґРЅРѕ РµС‰С‘. РџСЂРѕРІРµСЂРєР° РїРѕ
    /// РїСѓС‚Рё РЅРµ Р·Р°РІРёСЃРёС‚ РѕС‚ РёРЅРґРµРєСЃРѕРІ СЃСѓС‰РЅРѕСЃС‚РµР№ Рё РїРѕС‚РѕРјСѓ РїРµСЂРµР¶РёРІР°РµС‚ СЃРјРµРЅСѓ РєР°СЂС‚С‹.
    pub fn is_player_model(&self) -> bool {
        self.studio_model().is_some_and(|m| m.is_player_model())
    }

    /// РўРѕ Р¶Рµ, РЅРѕ РґР»СЏ РєРѕРЅРєСЂРµС‚РЅРѕР№ РјРѕРґРµР»Рё (РєРѕРіРґР° Р·Р°РіРѕР»РѕРІРѕРє РІР·СЏС‚ РёР· СЃСѓС‰РЅРѕСЃС‚Рё).
    pub fn is_player_model_for(&self, model: &StudioModel) -> bool {
        model.is_player_model()
    }

    /// РќР°С‡Р°Р»Рѕ РјР°СЃСЃРёРІР° РјР°С‚СЂРёС† РєРѕСЃС‚РµР№ (`m_pbonetransform`).
    ///
    /// РњР°СЃСЃРёРІ Р¶РёРІС‘С‚ РІ РѕР±СЉРµРєС‚Рµ СЂРµРЅРґРµСЂРµСЂР° Рё РїРµСЂРµР·Р°РїРёСЃС‹РІР°РµС‚СЃСЏ РєР°Р¶РґС‹Рј
    /// `StudioSetupBones`, РїРѕСЌС‚РѕРјСѓ С‡РёС‚Р°С‚СЊ РµРіРѕ РЅСѓР¶РЅРѕ СЃСЂР°Р·Сѓ РїРѕСЃР»Рµ РѕС‚СЂРёСЃРѕРІРєРё РјРѕРґРµР»Рё.
    pub fn bone_matrix_base(&self) -> Option<*const BoneMatrix> {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        let ptr: *const BoneMatrix = unsafe { process::read(self.addr() + renderer::BONE_TRANSFORM) };
        (!ptr.is_null()).then_some(ptr)
    }

    /// РџРѕР·РёС†РёСЏ РєРѕСЃС‚Рё РІ РјРёСЂРµ, РµСЃР»Рё РєРѕСЃС‚СЊ РµСЃС‚СЊ РІ РјРѕРґРµР»Рё.
    pub fn bone_origin(&self, bone: usize) -> Option<Vec3> {
        let model = self.studio_model()?;
        if bone >= model.bones().len() || bone >= MAXSTUDIOBONES {
            return None;
        }
        let base = self.bone_matrix_base()?;
        // SAFETY: РёРЅРґРµРєСЃ РїСЂРѕРІРµСЂРµРЅ РїРѕ С‡РёСЃР»Сѓ РєРѕСЃС‚РµР№ Рё РїРѕ РїСЂРµРґРµР»Сѓ РґРІРёР¶РєР°.
        let matrix = unsafe { crate::studio::bone_matrix(base, bone) };
        matrix.is_finite().then(|| matrix.origin())
    }

    /// Р’СЃРµ РїРѕР·РёС†РёРё РєРѕСЃС‚РµР№ С‚РµРєСѓС‰РµР№ РјРѕРґРµР»Рё.
    pub fn bone_origins(&self) -> Vec<Vec3> {
        let model = match self.studio_model() {
            Some(m) => m,
            None => return Vec::new(),
        };
        let base = match self.bone_matrix_base() {
            Some(b) => b,
            None => return Vec::new(),
        };
        let count = model.bones().len().min(MAXSTUDIOBONES);
        (0..count)
            .filter_map(|i| {
                // SAFETY: РёРЅРґРµРєСЃ РІРЅСѓС‚СЂРё Рё РјР°СЃСЃРёРІР°, Рё Р»РёРјРёС‚Р° РґРІРёР¶РєР°.
                let m = unsafe { crate::studio::bone_matrix(base, i) };
                m.is_finite().then(|| m.origin())
            })
            .collect()
    }

    /// РџРѕР·РёС†РёРё РєРѕСЃС‚РµР№, СЃРіСЂСѓРїРїРёСЂРѕРІР°РЅРЅС‹Рµ РІ СЂС‘Р±СЂР° СЃРєРµР»РµС‚Р°: `(СЂРѕРґРёС‚РµР»СЊ, СЂРµР±С‘РЅРѕРє)`.
    ///
    /// Р“РѕС‚РѕРІРѕ Рє РѕС‚СЂРёСЃРѕРІРєРµ Р»РёРЅРёСЏРјРё.
    pub fn skeleton_lines(&self) -> Vec<(Vec3, Vec3)> {
        let model = match self.studio_model() {
            Some(m) => m,
            None => return Vec::new(),
        };
        let origins = self.bone_origins();
        if origins.len() != model.bones().len() {
            return Vec::new();
        }
        model
            .skeleton_edges()
            .filter_map(|(child, parent)| Some((*origins.get(parent)?, *origins.get(child)?)))
            .collect()
    }

    pub fn render_origin(&self) -> Vec3 {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        unsafe { process::read(self.addr() + renderer::VIEW_ORIGIN) }
    }

    /// РњРѕРґРµР»СЊ СЃСѓС‰РЅРѕСЃС‚Рё, РєРѕС‚РѕСЂСѓСЋ СЂРµРЅРґРµСЂРµСЂ РѕР±СЂР°Р±Р°С‚С‹РІР°РµС‚ СЃРµР№С‡Р°СЃ (С‡РµСЂРµР· РµС‘ `model_t`).
    ///
    /// РќРµ Р·Р°РІРёСЃРёС‚ РѕС‚ РїРѕР»СЏ `+0x4C`: С‚Рѕ РїРѕР»Рµ РєР»РёРµРЅС‚ РјРѕР¶РµС‚ С‡РёСЃС‚РёС‚СЊ СЃСЂР°Р·Сѓ РїРѕСЃР»Рµ
    /// РѕС‚СЂРёСЃРѕРІРєРё, Р° `model_t` СЃСѓС‰РЅРѕСЃС‚Рё Р¶РёРІС‘С‚, РїРѕРєР° Р¶РёРІС‘С‚ РјРѕРґРµР»СЊ.
    pub fn entity_model(&self) -> Option<StudioModel> {
        self.current_entity()?.studio_model()
    }

    /// Позиции всех костей модели, если они посчитаны и конечны.
    ///
    /// Снимок делается один раз — внутри `StudioSetupBones`, где кости этого
    /// игрока гарантированно готовы; рисовать по нему можно позже.
    pub fn bone_origins_checked(&self, model: &StudioModel) -> Option<Vec<Vec3>> {
        let base = self.bone_matrix_base()?;
        let count = model.bones().len().min(MAXSTUDIOBONES);
        if count == 0 {
            return None;
        }
        let mut origins = Vec::with_capacity(count);
        for index in 0..count {
            // SAFETY: индекс внутри модели и лимита движка.
            let matrix = unsafe { crate::studio::bone_matrix(base, index) };
            if !matrix.is_finite() {
                return None;
            }
            origins.push(matrix.origin());
        }
        Some(origins)
    }

    /// Метка головы: `(центр хитбокса, радиус)`.
    ///
    /// Радиус — половина меньшей стороны объёма головы, то есть круг остаётся
    /// в габаритах головы, а не рисуется «на глаз».
    pub fn head_marker(&self, model: &StudioModel) -> Option<(Vec3, f32)> {
        let hitbox = self.hitbox(model, HitGroup::Head)?;
        let size = hitbox.size();
        let radius = (size.x.min(size.y) * 0.5).clamp(2.0, 12.0);
        Some((hitbox.center(), radius))
    }

    /// Р С‘Р±СЂР° СЃРєРµР»РµС‚Р° РґР»СЏ РєРѕРЅРєСЂРµС‚РЅРѕР№ РјРѕРґРµР»Рё (РїРѕР·РёС†РёРё вЂ” РёР· `m_pbonetransform`).
    pub fn skeleton_lines_for(&self, model: &StudioModel) -> Vec<(Vec3, Vec3)> {
        let base = match self.bone_matrix_base() {
            Some(base) => base,
            None => return Vec::new(),
        };
        let count = model.bones().len().min(MAXSTUDIOBONES);
        let mut origins = Vec::with_capacity(count);
        for i in 0..count {
            // SAFETY: РёРЅРґРµРєСЃ РІРЅСѓС‚СЂРё РјР°СЃСЃРёРІР° РјР°С‚СЂРёС† Рё Р»РёРјРёС‚Р° РґРІРёР¶РєР°.
            let m = unsafe { crate::studio::bone_matrix(base, i) };
            if !m.is_finite() {
                return Vec::new();
            }
            origins.push(m.origin());
        }
        model
            .skeleton_edges()
            .filter_map(|(child, parent)| Some((*origins.get(parent)?, *origins.get(child)?)))
            .collect()
    }

    /// РџСЂРµРґРµР»СЊРЅС‹Р№ СЂР°Р·Р±СЂРѕСЃ РєРѕСЃС‚РµР№ РѕС‚РЅРѕСЃРёС‚РµР»СЊРЅРѕ РєРѕСЂРЅРµРІРѕР№.
    ///
    /// Р РѕСЃС‚ РјРѕРґРµР»Рё РёРіСЂРѕРєР° CS 1.6 вЂ” РѕРєРѕР»Рѕ 72 СЋРЅРёС‚РѕРІ, СЂСѓРєРё СЃ РѕСЂСѓР¶РёРµРј РґРѕР±Р°РІР»СЏСЋС‚ РµС‰С‘
    /// ~30. РЎРєРµР»РµС‚, Сѓ РєРѕС‚РѕСЂРѕРіРѕ РєРѕСЃС‚Рё СЂР°Р·Р»РµС‚РµР»РёСЃСЊ РґР°Р»СЊС€Рµ, РїРѕСЃС‡РёС‚Р°РЅ РЅРµ РґР»СЏ СЌС‚РѕР№
    /// РјРѕРґРµР»Рё: С‚Р°Рє РІС‹РіР»СЏРґРёС‚ РјР°СЃСЃРёРІ РјР°С‚СЂРёС†, РѕСЃС‚Р°РІС€РёР№СЃСЏ РѕС‚ РґСЂСѓРіРѕРіРѕ РїСЂРѕС…РѕРґР° СЂРµРЅРґРµСЂР°.
    pub const MAX_BONE_EXTENT: f32 = 140.0;

    /// Р Р°РґРёСѓСЃ Сѓ РєР°РјРµСЂС‹, РІ РєРѕС‚РѕСЂРѕРј СЃРєРµР»РµС‚ СЃС‡РёС‚Р°РµС‚СЃСЏ В«РІСЊСЋРјРѕРґРµР»СЊСЋВ».
    ///
    /// РњР°С‚СЂРёС†С‹ РєРѕСЃС‚РµР№ РѕСЂСѓР¶РёСЏ РІ СЂСѓРєР°С… СЃС‡РёС‚Р°СЋС‚СЃСЏ РІ СЃРёСЃС‚РµРјРµ РєРѕРѕСЂРґРёРЅР°С‚ РєР°РјРµСЂС‹, Рё РµСЃР»Рё
    /// РґРІРёР¶РѕРє РЅРµ РїРµСЂРµСЃС‡РёС‚Р°Р» РёС… РґР»СЏ С‚РµРєСѓС‰РµР№ РјРѕРґРµР»Рё, РІ РјР°СЃСЃРёРІРµ РѕСЃС‚Р°СЋС‚СЃСЏ РїРѕР·РёС†РёРё Сѓ
    /// СЃР°РјРѕРіРѕ РіР»Р°Р·Р° вЂ” РїРѕ РЅРёРј РїРѕР»СѓС‡Р°СЋС‚СЃСЏ Р»РёРЅРёРё РёР· РЅРёРѕС‚РєСѓРґР° (РѕР±С‹С‡РЅРѕ Рє СЃРІРѕРµРјСѓ
    /// РѕСЂСѓР¶РёСЋ). РўР°РєРёРµ СЃРєРµР»РµС‚С‹ РѕС‚Р±СЂР°СЃС‹РІР°РµРј.
    pub const VIEWMODEL_RADIUS: f32 = 40.0;

    /// Р С‘Р±СЂР° СЃРєРµР»РµС‚Р° СЃ РїСЂРѕРІРµСЂРєРѕР№ РіРµРѕРјРµС‚СЂРёРё.
    ///
    /// РћС‚Р»РёС‡Р°РµС‚СЃСЏ РѕС‚ [`StudioRenderer::skeleton_lines_for`] С‚РµРј, С‡С‚Рѕ РѕС‚Р±СЂР°СЃС‹РІР°РµС‚
    /// Р·Р°РІРµРґРѕРјРѕ С‡СѓР¶РёРµ РјР°С‚СЂРёС†С‹: СЂР°Р·Р»РµС‚РµРІС€РёРµСЃСЏ РєРѕСЃС‚Рё Рё СЃРєРµР»РµС‚ РІ СЃРёСЃС‚РµРјРµ РєРѕРѕСЂРґРёРЅР°С‚
    /// РІСЊСЋРјРѕРґРµР»Рё. Р”Р»СЏ РѕС‚СЂРёСЃРѕРІРєРё РІ РёРіСЂРµ РЅСѓР¶РµРЅ РёРјРµРЅРЅРѕ СЌС‚РѕС‚ РІР°СЂРёР°РЅС‚.
    pub fn skeleton_lines_checked(&self, model: &StudioModel) -> Vec<(Vec3, Vec3)> {
        let base = match self.bone_matrix_base() {
            Some(base) => base,
            None => return Vec::new(),
        };
        let count = model.bones().len().min(MAXSTUDIOBONES);
        if count == 0 {
            return Vec::new();
        }
        let mut origins = Vec::with_capacity(count);
        for i in 0..count {
            // SAFETY: РёРЅРґРµРєСЃ РІРЅСѓС‚СЂРё РјР°СЃСЃРёРІР° РјР°С‚СЂРёС† Рё Р»РёРјРёС‚Р° РґРІРёР¶РєР°.
            let m = unsafe { crate::studio::bone_matrix(base, i) };
            if !m.is_finite() {
                return Vec::new();
            }
            origins.push(m.origin());
        }
        let root = origins[0];
        if origins
            .iter()
            .any(|p| p.distance_to(root) > Self::MAX_BONE_EXTENT)
        {
            return Vec::new();
        }
        if root.distance_to(self.render_origin()) < Self::VIEWMODEL_RADIUS {
            return Vec::new();
        }
        model
            .skeleton_edges()
            .filter_map(|(child, parent)| Some((*origins.get(parent)?, *origins.get(child)?)))
            .collect()
    }

    /// РќРѕРјРµСЂ РіСЂСѓРїРїС‹ С…РёС‚Р±РѕРєСЃРѕРІ В«РіРѕР»РѕРІР°В» (`HITGROUP_HEAD`) РІ `mstudiohitboxset_t`.
    pub const HITGROUP_HEAD: i32 = 1;

    /// Р¦РµРЅС‚СЂ РіРѕР»РѕРІС‹ РјРѕРґРµР»Рё РІ РјРёСЂРѕРІС‹С… РєРѕРѕСЂРґРёРЅР°С‚Р°С….
    ///
    /// РЎС‡РёС‚Р°РµС‚СЃСЏ РїРѕ С…РёС‚Р±РѕРєСЃСѓ РіСЂСѓРїРїС‹ [`Self::HITGROUP_HEAD`]: РІРѕСЃРµРјСЊ РµРіРѕ СѓРіР»РѕРІ
    /// РїРµСЂРµРІРѕРґСЏС‚СЃСЏ РІ РјРёСЂ, Рё Р±РµСЂС‘С‚СЃСЏ СЃРµСЂРµРґРёРЅР° РїРѕР»СѓС‡РёРІС€РµРіРѕСЃСЏ РѕР±СЉС‘РјР°. РўР°Рє С‚РѕС‡РєР°
    /// РѕСЃС‚Р°С‘С‚СЃСЏ РЅР° РіРѕР»РѕРІРµ РІ Р»СЋР±РѕР№ РїРѕР·Рµ вЂ” РІ РїСЂРёСЃРµРґРµ, РїСЂРё РЅР°РєР»РѕРЅРµ, СЃ РїРѕРґРЅСЏС‚РѕР№ СЂСѓРєРѕР№.
    /// РљРѕСЃС‚СЊ РіРѕР»РѕРІС‹ РґР»СЏ СЌС‚РѕРіРѕ РЅРµ РіРѕРґРёС‚СЃСЏ: РµС‘ РЅР°С‡Р°Р»Рѕ Р»РµР¶РёС‚ Сѓ РѕСЃРЅРѕРІР°РЅРёСЏ С‡РµСЂРµРїР°, Рё
    /// РєСЂСѓРі РїРѕ РЅРµР№ СѓРµР·Р¶Р°РµС‚ РЅР° РїР»РµС‡Рё.
    ///
    /// Р•СЃР»Рё С…РёС‚Р±РѕРєСЃРѕРІ РІ РјРѕРґРµР»Рё РЅРµС‚ ([`Self::head_bounds`] РІРµСЂРЅСѓР» `None`) вЂ”
    /// Р±РµСЂС‘С‚СЃСЏ СЃР°РјР°СЏ РІС‹СЃРѕРєР°СЏ РєРѕСЃС‚СЊ СЃРєРµР»РµС‚Р°.
    pub fn head_point(&self, model: &StudioModel) -> Option<Vec3> {
        self.head_bounds(model).map(|(center, _top)| center)
    }

    /// Р’СЃРµ С…РёС‚Р±РѕРєСЃС‹ РјРѕРґРµР»Рё РІ РјРёСЂРѕРІС‹С… РєРѕРѕСЂРґРёРЅР°С‚Р°С….
    ///
    /// РљР°Р¶РґС‹Р№ С…РёС‚Р±РѕРєСЃ вЂ” СЌС‚Рѕ `bbmin`/`bbmax` РІ РїСЂРѕСЃС‚СЂР°РЅСЃС‚РІРµ СЃРІРѕРµР№ РєРѕСЃС‚Рё; РІРѕСЃРµРјСЊ
    /// СѓРіР»РѕРІ РїРµСЂРµРІРѕРґСЏС‚СЃСЏ РјР°С‚СЂРёС†РµР№ РєРѕСЃС‚Рё (`m_pbonetransform[bone]`) РІ РјРёСЂ, РёР· РЅРёС…
    /// Р±РµСЂС‘С‚СЃСЏ AABB. Р­С‚Рѕ СЂРѕРІРЅРѕ С‚Рѕ, С‡С‚Рѕ РґРµР»Р°РµС‚ РґРІРёР¶РѕРє, РєРѕРіРґР° СЃС‡РёС‚Р°РµС‚ РїРѕРїР°РґР°РЅРёРµ.
    ///
    /// РџРѕСЂСЏРґРѕРє С‚РѕС‚ Р¶Рµ, С‡С‚Рѕ РІ РјРѕРґРµР»Рё; РіСЂСѓРїРїС‹ вЂ” [`HitGroup`].
    pub fn hitboxes_world(&self, model: &StudioModel) -> Vec<WorldHitbox> {
        let bones = model.bones().len();
        let Some(base) = self.bone_matrix_base() else {
            return Vec::new();
        };
        model
            .hitboxes()
            .iter()
            .filter_map(|hitbox| {
                let bone = hitbox.bone;
                if bone < 0 || bone as usize >= bones {
                    return None;
                }
                // SAFETY: РЅРѕРјРµСЂ РєРѕСЃС‚Рё РІРЅСѓС‚СЂРё РјРѕРґРµР»Рё Рё Р»РёРјРёС‚Р° РґРІРёР¶РєР°.
                let m = unsafe { crate::studio::bone_matrix(base, bone as usize) };
                let (min, max) = Self::world_bounds(&m, hitbox)?;
                Some(WorldHitbox {
                    group: HitGroup::from_number(hitbox.group),
                    bone: bone as usize,
                    min,
                    max,
                })
            })
            .collect()
    }

    /// РҐРёС‚Р±РѕРєСЃ РЅСѓР¶РЅРѕР№ РіСЂСѓРїРїС‹ РІ РјРёСЂРѕРІС‹С… РєРѕРѕСЂРґРёРЅР°С‚Р°С….
    pub fn hitbox(&self, model: &StudioModel, group: HitGroup) -> Option<WorldHitbox> {
        self.hitboxes_world(model)
            .into_iter()
            .find(|hitbox| hitbox.group == group)
    }

    /// РњРёСЂРѕРІРѕР№ AABB С…РёС‚Р±РѕРєСЃР°: РІРѕСЃРµРјСЊ СѓРіР»РѕРІ `bbmin`/`bbmax` РїРµСЂРµРІРѕРґСЏС‚СЃСЏ РјР°С‚СЂРёС†РµР№
/// РєРѕСЃС‚Рё РІ РјРёСЂ, РёР· РЅРёС… Р±РµСЂС‘С‚СЃСЏ РѕР±СЉС‘Рј.
///
/// `None`, РµСЃР»Рё РјР°С‚СЂРёС†Р° РЅРµ РіРѕС‚РѕРІР° (РјСѓСЃРѕСЂ/РЅСѓР»Рё) вЂ” С‚Р°РєРѕР№ С…РёС‚Р±РѕРєСЃ СЂРёСЃРѕРІР°С‚СЊ РЅРµР»СЊР·СЏ.
fn world_bounds(
    matrix: &BoneMatrix,
    hitbox: &crate::studio::StudioHitbox,
) -> Option<(Vec3, Vec3)> {
    if !matrix.is_finite() {
        return None;
    }
    let mut min = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
    let mut max = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for corner in 0..8 {
        let local = Vec3::new(
            if corner & 1 == 0 { hitbox.bbmin.x } else { hitbox.bbmax.x },
            if corner & 2 == 0 { hitbox.bbmin.y } else { hitbox.bbmax.y },
            if corner & 4 == 0 { hitbox.bbmin.z } else { hitbox.bbmax.z },
        );
        let world = matrix.transform_point(local);
        if !world.is_finite() {
            return None;
        }
        min = Vec3::new(min.x.min(world.x), min.y.min(world.y), min.z.min(world.z));
        max = Vec3::new(max.x.max(world.x), max.y.max(world.y), max.z.max(world.z));
    }
    (min.is_finite() && max.is_finite()).then_some((min, max))
}

/// РњРёСЂРѕРІРѕР№ РѕР±СЉС‘Рј РіРѕР»РѕРІС‹: `(С†РµРЅС‚СЂ, РІРµСЂС…РЅСЏСЏ С‚РѕС‡РєР°)`.
    ///
    /// Р’РµСЂС…РЅСЏСЏ С‚РѕС‡РєР° вЂ” СЃРµСЂРµРґРёРЅР° РІРµСЂС…РЅРµР№ РіСЂР°РЅРё, РїРѕ РЅРµР№ СЂРёСЃСѓРµС‚СЃСЏ РєСЂСѓРі: РѕРЅ Р»РѕР¶РёС‚СЃСЏ
    /// РЅР° РјР°РєСѓС€РєСѓ, Р° РЅРµ РЅР° С€РµСЋ.
    pub fn head_bounds(&self, model: &StudioModel) -> Option<(Vec3, Vec3)> {
        let base = self.bone_matrix_base()?;
        let bones = model.bones().len();

        if let Some(hitbox) = model
            .hitboxes()
            .iter()
            .find(|h| HitGroup::from_number(h.group) == HitGroup::Head)
        {
            let bone = hitbox.bone;
            if bone >= 0 && (bone as usize) < bones {
                // SAFETY: РЅРѕРјРµСЂ РєРѕСЃС‚Рё РІРЅСѓС‚СЂРё РјРѕРґРµР»Рё Рё Р»РёРјРёС‚Р° РґРІРёР¶РєР°.
                let m = unsafe { crate::studio::bone_matrix(base, bone as usize) };
                if let Some((min, max)) = Self::world_bounds(&m, hitbox) {
                    let center = (min + max) * 0.5;
                    let top = Vec3::new(center.x, center.y, max.z);
                    return Some((center, top));
                }
            }
        }

        // Р—Р°РїР°СЃРЅРѕР№ РїСѓС‚СЊ: СЃР°РјР°СЏ РІС‹СЃРѕРєР°СЏ РєРѕСЃС‚СЊ СЃРєРµР»РµС‚Р°.
        let count = bones.min(MAXSTUDIOBONES);
        let mut best: Option<Vec3> = None;
        for i in 0..count {
            // SAFETY: РёРЅРґРµРєСЃ РІРЅСѓС‚СЂРё РјР°СЃСЃРёРІР° РјР°С‚СЂРёС† Рё Р»РёРјРёС‚Р° РґРІРёР¶РєР°.
            let m = unsafe { crate::studio::bone_matrix(base, i) };
            if !m.is_finite() {
                continue;
            }
            let p = m.origin();
            if best.is_none_or(|b| p.z > b.z) {
                best = Some(p);
            }
        }
        best.map(|p| (p, p))
    }

    /// Р”РёР°РіРЅРѕСЃС‚РёС‡РµСЃРєРёР№ СЃРЅРёРјРѕРє: РІСЃС‘, С‡С‚Рѕ С‡РёС‚Р°РµС‚СЃСЏ РёР· РїР°РјСЏС‚Рё РїСЂСЏРјРѕ СЃРµР№С‡Р°СЃ.
    ///
    /// РќСѓР¶РµРЅ, РєРѕРіРґР° В«СЃРєРµР»РµС‚ РЅРµ РІРёРґРЅРѕВ»: РїРѕ РЅРµРјСѓ РІРёРґРЅРѕ, РєР°РєРѕРµ РёРјРµРЅРЅРѕ РїРѕР»Рµ РЅРµ С‚Рѕ вЂ”
    /// vtable, СѓРєР°Р·Р°С‚РµР»СЊ РЅР° СЃСѓС‰РЅРѕСЃС‚СЊ, Р·Р°РіРѕР»РѕРІРѕРє РјРѕРґРµР»Рё РёР»Рё РјР°СЃСЃРёРІ РєРѕСЃС‚РµР№.
    pub fn snapshot(&self) -> Snapshot {
        let addr = self.addr();
        let readable = process::is_readable(addr);
        let vtable = if readable { self.vtable() } else { 0 };
        let current_entity = if readable {
            // SAFETY: РѕР±СЉРµРєС‚ РїСЂРѕРІРµСЂРµРЅ РЅР° С‡РёС‚Р°РµРјРѕСЃС‚СЊ.
            unsafe { process::read::<usize>(addr + renderer::CURRENT_ENTITY) }
        } else {
            0
        };
        let studio_header = if readable {
            // SAFETY: С‚Рѕ Р¶Рµ.
            unsafe { process::read::<usize>(addr + renderer::STUDIO_HEADER) }
        } else {
            0
        };
        let bone_base = if readable {
            // SAFETY: С‚Рѕ Р¶Рµ.
            unsafe { process::read::<usize>(addr + renderer::BONE_TRANSFORM) }
        } else {
            0
        };

        let entity = EntityRef::from_raw(current_entity);
        let entity_model = entity.map(|e| e.model() as usize).unwrap_or(0);
        let entity_header = entity
            .and_then(|e| e.studio_model())
            .map(|m| m.header() as *const StudioHeader as usize)
            .unwrap_or(0);
        let entity_state = entity.map(|e| e.model_state()).unwrap_or(-1);

        let header_ptr = if studio_header != 0 { studio_header } else { entity_header };
        let (header_id, header_version, header_bones) = if process::is_readable(header_ptr) {
            // SAFETY: Р·Р°РіРѕР»РѕРІРѕРє С‡РёС‚Р°РµРј, РїРѕР»СЏ Р»РµР¶Р°С‚ РІ РЅР°С‡Р°Р»Рµ СЃС‚СЂСѓРєС‚СѓСЂС‹.
            unsafe {
                let h = header_ptr as *const StudioHeader;
                ((*h).id, (*h).version, (*h).numbones)
            }
        } else {
            (0, 0, 0)
        };
        let first_bone = if bone_base != 0 && process::is_readable(bone_base) {
            // SAFETY: РјР°СЃСЃРёРІ РјР°С‚СЂРёС† С‡РёС‚Р°РµРј.
            unsafe { crate::studio::bone_matrix(bone_base as *const BoneMatrix, 0) }
        } else {
            BoneMatrix::default()
        };

        Snapshot {
            renderer: addr,
            readable,
            vtable,
            initialized: self.is_initialized(),
            current_entity,
            entity_state,
            entity_model,
            entity_header,
            studio_header,
            header_id,
            header_version,
            header_bones,
            bone_base,
            first_bone,
        }
    }

    pub fn view_up(&self) -> Vec3 {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        unsafe { process::read(self.addr() + renderer::VIEW_UP) }
    }

    pub fn view_right(&self) -> Vec3 {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        unsafe { process::read(self.addr() + renderer::VIEW_RIGHT) }
    }

    pub fn view_forward(&self) -> Vec3 {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        unsafe { process::read(self.addr() + renderer::VIEW_NORMAL) }
    }

    /// РљР°РјРµСЂР°, РєРѕС‚РѕСЂРѕР№ РґРІРёР¶РѕРє СЂРёСЃРѕРІР°Р» СЌС‚РѕС‚ РєР°РґСЂ: `origin`, `forward`, `right`, `up`.
    ///
    /// Р“РѕС‚РѕРІР° Рє [`crate::math::Camera::project`] вЂ” РІРјРµСЃС‚Рµ СЃ [`crate::math::Fov`]
    /// РґР°С‘С‚ СЌРєСЂР°РЅРЅС‹Рµ РєРѕРѕСЂРґРёРЅР°С‚С‹ С‚РѕС‡РµРє РјРёСЂР° Р±РµР· РѕР±СЂР°С‰РµРЅРёСЏ Рє РґРІРёР¶РєСѓ.
    pub fn camera(&self) -> crate::math::Camera {
        crate::math::Camera::new(
            self.render_origin(),
            self.view_forward(),
            self.view_right(),
            self.view_up(),
        )
    }

    /// Р¦РІРµС‚Р° РјРѕРґРµР»Рё РёРіСЂРѕРєР° `(top, bottom)` РІ РІРёРґРµ РјРЅРѕР¶РёС‚РµР»РµР№.
    pub fn player_colors(&self) -> (f32, f32) {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        unsafe {
            (
                process::read(self.addr() + renderer::PLAYER_TOP_COLOR),
                process::read(self.addr() + renderer::PLAYER_BOTTOM_COLOR),
            )
        }
    }

    /// `m_nRenderAmt` вЂ” В«РїСЂРѕР·СЂР°С‡РЅРѕСЃС‚СЊВ»/РєРѕР»РёС‡РµСЃС‚РІРѕ РїСЂРѕС…РѕРґРѕРІ СЂРµРЅРґРµСЂР°.
    pub fn render_amt(&self) -> i32 {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        unsafe { process::read(self.addr() + renderer::RENDER_AMT) }
    }

    /// РЎРєРѕР»СЊРєРѕ РјРѕРґРµР»РµР№ РЅР°СЂРёСЃРѕРІР°РЅРѕ РІ СЌС‚РѕРј РєР°РґСЂРµ.
    pub fn models_drawn(&self) -> i32 {
        // SAFETY: РѕР±СЉРµРєС‚ СЃС‚Р°С‚РёС‡РµСЃРєРёР№.
        unsafe { process::read(self.addr() + renderer::MODELS_DRAWN) }
    }

    /// Р—Р°РіСЂСѓР¶Р°РµС‚ РјРѕРґРµР»СЊ РїРѕ РёРјРµРЅРё С‡РµСЂРµР· РґРІРёР¶РѕРє (`Mod_ForName`).
    ///
    /// РЈРґРѕР±РЅРѕ РґР»СЏ РїСЂРѕСЃРјРѕС‚СЂС‰РёРєР° РјРѕРґРµР»РµР№ Рё РѕС‚Р»Р°РґРѕС‡РЅС‹С… СЃС†РµРЅР°СЂРёРµРІ; РІ РёРіСЂРµ РјРѕРґРµР»СЊ
    /// РґРѕР»Р¶РЅР° Р±С‹С‚СЊ СЃРїСЂРµРєРµС€РёСЂРѕРІР°РЅР°, РёРЅР°С‡Рµ РґРІРёР¶РѕРє РѕС‚РєР°Р¶РµС‚СЃСЏ РµС‘ РіСЂСѓР·РёС‚СЊ.
    pub fn load_model(&self, name: &CStr) -> Option<*mut c_void> {
        crate::engine::EngineStudioApi::for_engine()?.mod_for_name(true, name)
    }
}
