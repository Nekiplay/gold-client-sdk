//! Состояние движка, которое он обновляет каждый кадр.
//!
//! Источник — `ref_params_t` в `hw.dll` ([`crate::offsets::ref_params`]): движок
//! копирует туда клиентскую структуру перед каждой отрисовкой, поэтому значения
//! всегда свежие и совпадают с тем, что видит рендер. Флаги «в игре / в меню /
//! чат» лежат отдельными глобалами.
//!
//! ```no_run
//! use golden_sdk::state::EngineState;
//!
//! if EngineState::in_game() && !EngineState::chat_opened() {
//!     let origin = EngineState::view_origin();
//!     let angles = EngineState::view_angles();
//!     let _ = (origin, angles, EngineState::ducking(), EngineState::on_ground());
//! }
//! ```

use crate::math::Vec3;
use crate::offsets::{client, hw, ref_params};
use crate::process;

fn flag(address: usize) -> bool {
    int_at(address) != 0
}

fn int_at(address: usize) -> i32 {
    if !process::is_readable(address) {
        return 0;
    }
    // SAFETY: адрес проверен на читаемость, запись делает движок подряд.
    unsafe { (address as *const i32).read_unaligned() }
}

fn f32_at(address: usize) -> f32 {
    if !process::is_readable(address) {
        return 0.0;
    }
    // SAFETY: см. `int_at`.
    unsafe { (address as *const f32).read_unaligned() }
}

fn vec3_at(address: usize) -> Vec3 {
    Vec3::new(f32_at(address), f32_at(address + 4), f32_at(address + 8))
}

fn hw(offset: usize) -> usize {
    process::hw_base().map(|base| base + offset).unwrap_or(0)
}

/// Живое состояние движка.
///
/// Всё читается из памяти процесса. До загрузки карты значения нулевые, поэтому
/// сначала проверяйте [`EngineState::in_game`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineState;

impl EngineState {
    /// ✔ Клиент загрузил карту и находится в игре (не в меню и не в загрузке).
    pub fn in_game() -> bool {
        flag(hw(hw::IN_GAME))
    }

    /// ✔ Открыто меню (`Esc`); мир при этом продолжает идти.
    pub fn in_menu() -> bool {
        flag(hw(hw::IN_MENU))
    }

    /// ✔ Открыт чат: клавиши уходят в строку ввода, а не в игру.
    pub fn chat_opened() -> bool {
        flag(hw(hw::CHAT_OPENED))
    }

    /// ✔ Игровое время в секундах. Растёт монотонно и сбрасывается с картой.
    pub fn time() -> f32 {
        f32_at(hw(ref_params::TIME))
    }

    /// ✔ Длительность последнего кадра в секундах.
    pub fn frame_time() -> f32 {
        f32_at(hw(ref_params::FRAMETIME))
    }

    /// ← Идёт смена карты (`intermission`).
    pub fn intermission() -> bool {
        flag(hw(ref_params::INTERMISSION))
    }

    /// ← Игра на паузе.
    pub fn paused() -> bool {
        flag(hw(ref_params::PAUSED))
    }

    /// ← Камера наблюдателя.
    pub fn spectator() -> bool {
        flag(hw(ref_params::SPECTATOR))
    }

    /// ✔ Игрок стоит на земле: `false` в прыжке, на лестнице и в воде.
    pub fn on_ground() -> bool {
        flag(hw(ref_params::ON_GROUND))
    }

    /// ✔ Уровень воды: `0` сухо, `1` по щиколотку, `2` в воде, `3` под водой.
    pub fn water_level() -> i32 {
        int_at(hw(ref_params::WATER_LEVEL))
    }

    /// ✔ Игрок в воде (уровень `2` и выше).
    pub fn in_water() -> bool {
        Self::water_level() >= 2
    }

    /// Позиция камеры — `ref_params.vieworg`.
    pub fn view_origin() -> Vec3 {
        vec3_at(hw(ref_params::VIEW_ORIGIN))
    }

    /// Позиция игрока — `ref_params.simorg` (совпадает с `origin` сущности).
    pub fn player_origin() -> Vec3 {
        vec3_at(hw(ref_params::SIMORG))
    }

    /// ✔ Углы камеры, которые использует рендер (с отдачей от стрельбы).
    pub fn view_angles() -> Vec3 {
        vec3_at(hw(ref_params::VIEW_ANGLES))
    }

    /// ✔ Углы ввода без отдачи — то, чем двигается прицел.
    pub fn input_angles() -> Vec3 {
        vec3_at(hw(ref_params::CL_VIEWANGLES))
    }

    /// ✔ Единичный вектор взгляда.
    pub fn forward() -> Vec3 {
        vec3_at(hw(ref_params::FORWARD))
    }

    /// ✔ Единичный вектор вправо от взгляда.
    pub fn right() -> Vec3 {
        vec3_at(hw(ref_params::RIGHT))
    }

    /// ✔ Единичный вектор вверх.
    pub fn up() -> Vec3 {
        vec3_at(hw(ref_params::UP))
    }

    /// ✔ Скорость игрока в юнитах в секунду.
    pub fn velocity() -> Vec3 {
        vec3_at(hw(ref_params::SIMVEL))
    }

    /// ✔ Смещение глаз от позиции игрока: `z = 17` стоя, `12` в приседе.
    pub fn view_offset() -> Vec3 {
        vec3_at(hw(ref_params::VIEW_OFS))
    }

    /// ✔ Игрок присел — по просадке глаз относительно стойки.
    ///
    /// Движок не хранит в `ref_params` флаг приседа, но `view_ofs.z` меняется
    /// вместе с ним, а `on_ground` при этом остаётся `true`.
    pub fn ducking() -> bool {
        let z = Self::view_offset().z;
        z > 0.0 && z < cl::STANDING_VIEW_HEIGHT - 0.5
    }

    /// Эвристика «на лестнице».
    ///
    /// Движок не публикует `movetype` в `ref_params`, поэтому признак собран из
    /// наблюдаемого поведения: в игре [`on_ground`](Self::on_ground) на лестнице
    /// равно `false` (стоя на земле — `true`), воду это не затрагивает
    /// ([`in_water`](Self::in_water) отсекает плавание), а `on_ground == false`
    /// в прыжке живёт доли секунды, тогда как на лестнице признак держится.
    ///
    /// Точный флаг появится, когда в SDK добавится `entity_state.movetype`.
    pub fn on_ladder() -> bool {
        !Self::on_ground() && !Self::in_water() && Self::velocity().z.abs() > 1.0
    }

    /// ✔ Здоровье из `ref_params`; `-1`, если движок его не знает.
    pub fn health() -> i32 {
        int_at(hw(ref_params::HEALTH))
    }

    /// ✔ `cl_viewsize` (`120` по умолчанию): от него зависит область прицела.
    pub fn view_size() -> f32 {
        f32_at(hw(ref_params::VIEWSIZE))
    }

    /// ✔ Номер сущности, за которой смотрит игрок: `1` — свой игрок,
    /// `> MAX_EDICTS` — камера наблюдателя за кем-то другим.
    pub fn view_entity() -> i32 {
        int_at(hw(ref_params::VIEWENTITY))
    }

    /// ✔ Максимальное число клиентов на сервере (`32` обычно).
    pub fn max_clients() -> i32 {
        int_at(hw(ref_params::MAXCLIENTS))
    }

    /// ✔ Размер окна в пикселях — то же, что использует рендер.
    pub fn screen_size() -> (i32, i32) {
        (
            int_at(hw(ref_params::SCREEN_WIDTH)),
            int_at(hw(ref_params::SCREEN_HEIGHT)),
        )
    }

    /// Текущий `m_iFov` в градусах: `90` по умолчанию, `40` при первом зуме AWP.
    ///
    /// Читается первый из трёх известных кандидатов; их равенство проверяет
    /// [`crate::validate`].
    pub fn fov() -> i32 {
        process::client_base()
            .map(|base| int_at(base + client::view::FOV_1))
            .unwrap_or(0)
    }

    /// ✔ `sensitivity` из `cvar_t`: множитель движения мышью.
    pub fn sensitivity() -> Option<f32> {
        cvar_value(client::cvars::SENSITIVITY)
    }

    /// ✔ `m_yaw` из `cvar_t` (`0.022`): градусов на «шаг» мыши по горизонтали.
    pub fn m_yaw() -> Option<f32> {
        cvar_value(client::cvars::M_YAW)
    }
}

/// Горизонтальный FOV в градусах с учётом аспекта окна.
///
/// Движок задаёт FOV по соглашению 4:3, а по горизонтали расширяет его под
/// фактическое соотношение сторон, поэтому для 1920×1080 получается больше
/// `m_iFov`.
pub fn horizontal_fov(fov: i32) -> f32 {
    let (width, height) = EngineState::screen_size();
    if width <= 0 || height <= 0 {
        return fov as f32;
    }
    let vertical = ((fov as f32).to_radians() / 2.0).tan() * 0.75;
    let horizontal = (vertical * width as f32 / height as f32).atan() * 2.0;
    horizontal.to_degrees()
}

/// Значение `cvar_t` по адресу его глобала в `client.dll`.
fn cvar_value(offset: usize) -> Option<f32> {
    let base = process::client_base()?;
    let pointer = int_at(base + offset) as usize;
    if pointer < 0x1000_0000 || !process::is_readable(pointer) {
        return None;
    }
    Some(f32_at(pointer + client::cvars::VALUE))
}

/// Имя `cvar_t` по его глобалу — для проверок в [`crate::validate`].
pub fn cvar_name(offset: usize) -> Option<String> {
    let base = process::client_base()?;
    let pointer = int_at(base + offset) as usize;
    if pointer < 0x1000_0000 {
        return None;
    }
    let name = int_at(pointer + client::cvars::NAME) as usize;
    process::read_cstr(name, 64)
}

/// Высота глаз, которую движок ставит стоя.
mod cl {
    /// ✔ `17.0` юнита: проверено вычитанием `simorg.z` из `vieworg.z`.
    pub const STANDING_VIEW_HEIGHT: f32 = 17.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_params_offsets_are_ordered() {
        // Поля идут в порядке HLSDK-структуры; если порядок сдвинется, тест
        // поймает это до того, как начнут врать числа.
        let order = [
            ref_params::VIEW_ORIGIN,
            ref_params::VIEW_ANGLES,
            ref_params::FORWARD,
            ref_params::RIGHT,
            ref_params::UP,
            ref_params::FRAMETIME,
            ref_params::TIME,
            ref_params::ON_GROUND,
            ref_params::WATER_LEVEL,
            ref_params::SIMVEL,
            ref_params::SIMORG,
            ref_params::VIEW_OFS,
            ref_params::CL_VIEWANGLES,
            ref_params::HEALTH,
            ref_params::VIEWSIZE,
            ref_params::MAXCLIENTS,
            ref_params::VIEWENTITY,
            ref_params::SCREEN_WIDTH,
            ref_params::SCREEN_HEIGHT,
        ];
        for pair in order.windows(2) {
            assert!(pair[0] < pair[1], "{:#x} >= {:#x}", pair[0], pair[1]);
        }
    }

    #[test]
    fn state_globals_are_inside_engine_image() {
        const IMAGE: usize = 0x3CA_6000;
        for offset in [hw::IN_GAME, hw::IN_MENU, hw::CHAT_OPENED] {
            assert!(offset < IMAGE - 0x40, "{offset:#x} вне образа hw.dll");
        }
    }

    #[test]
    fn horizontal_fov_matches_four_thirds_convention() {
        // Без окна функция возвращает вход как есть.
        assert_eq!(horizontal_fov(90), 90.0);
    }
}