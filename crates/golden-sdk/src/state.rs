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

use alloc::string::String;

use crate::math::Vec3;
use crate::offsets::{cl, client, frames, hw, ref_params, usercmd};
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

fn u32_at(address: usize) -> u32 {
    if !process::is_readable(address) {
        return 0;
    }
    // SAFETY: см. `int_at`.
    unsafe { (address as *const u32).read_unaligned() }
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
    ///
    /// Определяется по живому `ref_params`: движок заполняет эту структуру
    /// каждый кадр, пока клиент в игре, и `ref_params::TIME` при этом растёт.
    /// Прежний кандидат `hw+0x274B18` не подошёл: в живой игре там лежит `0`
    /// (см. [`hw::IN_GAME_UNVERIFIED`]).
    pub fn in_game() -> bool {
        Self::time() > 0.0
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

    /// ✔ Можно ли играть прямо сейчас: в игре, не в меню и чате, жив.
    /// Так решают триггербот и аим — стоять ли на паузе в меню, пока мир под
    /// ним продолжает двигаться.
    ///
    /// Сюда не входят `intermission` и `spectator`: их смещения в `ref_params`
    /// не проверены в живой игре (маркер `←`), и ложное срабатывание такого
    /// флага отключало бы решение навсегда и молча. Здоровье `-1` означает
    /// «движок не знает» (Gold Client часто так и оставляет в `ref_params`) —
    /// это не признак смерти и ведению не мешает.
    pub fn playable() -> bool {
        Self::in_game() && !Self::in_menu() && !Self::chat_opened() && Self::alive()
    }

    /// ✔ Жив ли по мнению движка. `0` — мёртв, `-1` — движок не знает
    /// (не блокируем), `> 0` — жив.
    pub fn alive() -> bool {
        let health = Self::health();
        health == -1 || health > 0
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

    /// ← Отдача, которую движок добавил к камере, — `ref_params.punchangle`.
    ///
    /// Это значение последнего **отрисованного** кадра: рендер копирует его из
    /// клиента. В `CreateMove` оно отстаёт от текущего выстрела на один тик,
    /// зато не требует читать состояние оружия клиента.
    pub fn punch_angle() -> Vec3 {
        vec3_at(hw(ref_params::PUNCHANGLE))
    }

    /// ✔ Отдача из последнего снапшота — `cl.punchangle`.
    ///
    /// В отличие от [`Self::punch_angle`] это не копия из `ref_params`, а
    /// исходное поле клиента: в `CreateMove` оно уже содержит отдачу текущего
    /// тика. Именно его движок прибавит к углам обзора, поэтому вычитать его
    /// надо из [`Self::view_angles_live`].
    pub fn punch_angle_live() -> Vec3 {
        vec3_at(hw(cl::PUNCHANGLE))
    }

    /// ✔ Угол обзора клиента — `cl.viewangles`.
    ///
    /// Это исходное поле, из которого движок берёт углы для отправляемой
    /// команды и для камеры. Правка `usercmd_t::viewangles` в перехвате
    /// бесполезна: движок переписывает её отсюда после `CL_CreateMove`.
    pub fn view_angles_live() -> Vec3 {
        vec3_at(hw(cl::VIEWANGLES))
    }

    /// Записывает угол обзора клиента `cl.viewangles` **каноническим путём**.
    ///
    /// Движковый `pfnSetViewAngles` (слот `cl_enginefuncs_t`, заполняется при
    /// загрузке клиента) обновляет не только камеру, но и кэш, из которого
    /// собирается отправляемая команда. Прямая запись в память двигала
    /// камеру, но не пуля (`recoilProbe`: прицел стоит — дырки по отдаче).
    /// Если слот недоступен (клиент не загружен), пишем напрямую —
    /// поведение старой версии.
    ///
    /// # Safety
    ///
    /// Меняет состояние движка: писать можно только из `CreateMove`, до
    /// отправки пакета.
    pub fn set_view_angles(angles: Vec3) {
        use crate::offsets::client;

        if let Some(base) = process::client_base() {
            let slot = base + client::SET_VIEW_ANGLES;
            if process::is_readable(slot) {
                // SAFETY: слот — `void (*pfnSetViewAngles)(vec3_t *)`; адрес
                // кладёт движок при загрузке клиента.
                let func = unsafe { process::read::<usize>(slot) };
                if func != 0 && process::is_readable(func) {
                    // SAFETY: `Vec3` — `repr(C)` из трёх `f32`, выравнивание 4;
                    // вызов cdecl по указателю движка, буфер живёт до возврата.
                    unsafe {
                        let set: unsafe extern "C" fn(*const Vec3) =
                            core::mem::transmute(func);
                        set(&angles);
                    }
                    return;
                }
            }
        }
        let base = hw(cl::VIEWANGLES);
        if !process::is_readable(base) {
            return;
        }
        // SAFETY: адрес — поле `vec3_t` движка; запись выровнена по 4.
        unsafe { process::write(base, angles) };
    }

    /// ✔ Номер последней созданной команды (`cl.cmdnumber`).
    pub fn command_number() -> u32 {
        u32_at(hw(frames::CMD_NUMBER))
    }

    /// ✔ Последняя команда, подтверждённая сервером (`cl.parsecount`).
    pub fn parse_count() -> u32 {
        u32_at(hw(frames::PARSE_COUNT))
    }

    /// ✔ Отдача в предсказанном кадре команды `command`.
    ///
    /// Кадры индексируются по `command & 0x3F`. Значение в кадре — то, с чем
    /// сервер посчитает **следующую** команду, поэтому оно опережает
    /// [`Self::punch_angle_live`] на интерполяцию.
    pub fn punch_angle_in_frame(command: u32) -> Vec3 {
        let index = command as usize & 0x3F;
        vec3_at(hw(frames::BASE
            + index * frames::STRIDE
            + frames::PUNCHANGLE))
    }

    /// ✔ Углы, лежащие в готовой команде `cl.cmds[command & 0x3F]`.
    ///
    /// Это то, что движок отправит на сервер. Если правка `usercmd_t`
    /// доезжает, здесь на следующем тике окажется исправленное значение.
    pub fn cmd_view_angles(command: u32) -> Vec3 {
        let index = command as usize & 0x3F;
        vec3_at(hw(cl::CMDS + index * cl::CMD_STRIDE + usercmd::VIEWANGLES))
    }

    /// ✔ Угол в готовой к отправке команде `cl.cmd`.
    ///
    /// `CL_SendCmd` копирует сюда `cl.cmds[cl.cmdnumber & 0x3F]` после
    /// `CL_CreateMove`. Сверка этого поля со своей поправкой показывает,
    /// пережила ли правка команды сборку пакета.
    pub fn wire_view_angles() -> Vec3 {
        vec3_at(hw(cl::CMD + usercmd::VIEWANGLES))
    }

    /// ✔ Номер слота `cl.cmds`, на который указывает `cmd` из `CreateMove`.
    ///
    /// `-1` — адрес вне массива (тогда правка команды уходит в пустоту).
    pub fn command_slot(cmd: usize) -> i32 {
        let base = hw(cl::CMDS);
        if cmd < base {
            return -1;
        }
        let offset = cmd - base;
        if offset % cl::CMD_STRIDE != 0 || offset / cl::CMD_STRIDE >= 64 {
            return -1;
        }
        (offset / cl::CMD_STRIDE) as i32
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
        z > 0.0 && z < eye::STANDING_VIEW_HEIGHT - 0.5
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
mod eye {
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
        for offset in [hw::IN_GAME_UNVERIFIED, hw::IN_MENU, hw::CHAT_OPENED] {
            assert!(offset < IMAGE - 0x40, "{offset:#x} вне образа hw.dll");
        }
    }

    #[test]
    fn horizontal_fov_matches_four_thirds_convention() {
        // Без окна функция возвращает вход как есть.
        assert_eq!(horizontal_fov(90), 90.0);
    }
}
