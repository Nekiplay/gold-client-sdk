//! Игрок: команда и «это модель игрока или нет».
//!
//! В GoldSrc нет отдельного поля «команда» ни в `cl_entity_t`, ни в
//! `player_info_t`: CS 1.6 возит команду отдельным сетевым сообщением и держит
//! её в клиентском HUD, куда SDK лезть не должен. Зато модель игрока однозначно
//! говорит о команде: у террористов это `terror`, `leet`, `arctic`, `guerilla`,
//! у контров — `urban`, `gsg9`, `sas`, `gign`. По имени модели цвет выбирается
//! без чтения чужих структур и переживает смену карты.
//!
//! Имя в заголовке `.mdl` хранится **без** каталога `models/`, например
//! `terror\terror.mdl`, поэтому путь в игре и путь в заголовке — разные вещи.

use alloc::string::{String, ToString};

use crate::draw::Color;

/// Номер команды из `g_PlayerExtraInfo` (`TeamInfo`).
pub const TEAM_UNASSIGNED: i32 = 0;
/// Террористы.
pub const TEAM_TERRORIST: i32 = 1;
/// Спецназ.
pub const TEAM_CT: i32 = 2;
/// Наблюдатель.
pub const TEAM_SPECTATOR: i32 = 3;

/// Команда игрока.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Team {
    /// Террористы.
    Terrorist,
    /// Спецназ.
    CounterTerrorist,
}

impl Team {
    /// Модели террористов CS 1.6 (только для резервного определения: на серверах
    /// с кастомными скинами не работает — основной путь это [`PlayerExtra`]).
    pub const TERRORIST_MODELS: [&'static str; 4] = ["terror", "leet", "arctic", "guerilla"];
    /// Модели спецназа CS 1.6.
    pub const CT_MODELS: [&'static str; 4] = ["urban", "gsg9", "sas", "gign"];

    /// Команда по номеру из `g_PlayerExtraInfo`.
    ///
    /// `0` (не назначена) и `3` (наблюдатель) команды не дают.
    pub fn from_number(number: i32) -> Option<Self> {
        match number {
            TEAM_TERRORIST => Some(Self::Terrorist),
            TEAM_CT => Some(Self::CounterTerrorist),
            _ => None,
        }
    }

    /// Команда по имени из сетевого сообщения: `"CT"`, `"TERRORIST"`.
    pub fn from_team_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("CT") {
            return Some(Self::CounterTerrorist);
        }
        if name.eq_ignore_ascii_case("TERRORIST") {
            return Some(Self::Terrorist);
        }
        None
    }

    /// Команда по имени модели — резервный путь для случая, когда
    /// `g_PlayerExtraInfo` ещё не заполнен (первый кадр после подключения).
    pub fn from_model_name(name: &str) -> Option<Self> {
        let stem = model_stem(name).to_ascii_lowercase();
        if Self::TERRORIST_MODELS.contains(&stem.as_str()) {
            return Some(Self::Terrorist);
        }
        if Self::CT_MODELS.contains(&stem.as_str()) {
            return Some(Self::CounterTerrorist);
        }
        None
    }

    /// Цвет скелета этой команды.
    pub fn color(self) -> Color {
        match self {
            Self::Terrorist => Color::rgba(1.0, 0.35, 0.15, 1.0),
            Self::CounterTerrorist => Color::rgba(0.3, 0.65, 1.0, 1.0),
        }
    }

    /// Короткое имя для логов.
    pub fn short_name(self) -> &'static str {
        match self {
            Self::Terrorist => "T",
            Self::CounterTerrorist => "CT",
        }
    }
}

/// Запись `g_PlayerExtraInfo[index]` — то, что клиент знает об игроке по счёту.
///
/// Массив заполняет обработчик сообщения `TeamInfo`, поэтому команда верна и для
/// кастомных моделей, и для ботов.
#[derive(Debug, Clone, Copy)]
pub struct PlayerExtra {
    addr: usize,
}

impl PlayerExtra {
    /// Элемент по индексу игрока (номер слота клиента, 0..=63).
    pub fn for_index(index: i32) -> Option<Self> {
        if !(0..=63).contains(&index) {
            return None;
        }
        let base = crate::process::client_base()?;
        let addr = base
            + crate::offsets::client::PLAYER_EXTRA_INFO
            + index as usize * crate::offsets::client::PLAYER_EXTRA_STRIDE;
        crate::process::is_readable(addr).then_some(Self { addr })
    }

    /// Номер команды: [`TEAM_TERRORIST`], [`TEAM_CT`], [`TEAM_SPECTATOR`] или `0`.
    pub fn team_number(&self) -> i32 {
        // SAFETY: адрес проверен в `for_index`.
        unsafe { crate::process::read(self.addr + crate::offsets::client::PLAYER_EXTRA_TEAM) }
    }

    /// Команда, если она уже известна.
    pub fn team(&self) -> Option<Team> {
        Team::from_number(self.team_number())
    }

    /// Имя команды из сообщения `TeamInfo` (`"CT"`, `"TERRORIST"`, `"SPECTATOR"`).
    pub fn team_name(&self) -> String {
        // SAFETY: не нужно — `read_cstr` сам проверяет читаемость страниц.
        crate::process::read_cstr(
            self.addr + crate::offsets::client::PLAYER_EXTRA_TEAMNAME,
            16,
        )
        .unwrap_or_default()
    }

    /// Байт-флаг внутри записи.
    fn flag(&self, offset: usize) -> bool {
        let addr = self.addr + offset;
        // SAFETY: страница проверяется; читаем один байт внутри того же блока.
        crate::process::is_readable(addr) && unsafe { crate::process::read::<u8>(addr) } != 0
    }

    /// Несёт ли игрок бомбу (C4).
    pub fn has_bomb(&self) -> bool {
        self.flag(crate::offsets::client::PLAYER_EXTRA_BOMB)
    }

    /// Есть ли у игрока дефуз-кит.
    pub fn has_defuser(&self) -> bool {
        self.flag(crate::offsets::client::PLAYER_EXTRA_DEFUSER)
    }

    /// VIP в режиме `as_*`.
    pub fn is_vip(&self) -> bool {
        self.flag(crate::offsets::client::PLAYER_EXTRA_VIP)
    }

    /// Мёртв ли игрок (по данным клиента, обновляется таблицей счёта).
    pub fn is_dead(&self) -> bool {
        self.flag(crate::offsets::client::PLAYER_EXTRA_DEAD)
    }

    /// Имя игрока: в записи лежит указатель на строку (`client+0x0BEC00` передаёт
    /// его в таблицу счёта как значение ключа `"name"`, а `client+0x0BA530`
    /// копирует его в первый слот выходной структуры).
    pub fn name(&self) -> String {
        // SAFETY: страница проверяется `read_cstr`.
        let ptr: usize = unsafe { crate::process::read(self.addr) };
        if ptr < 0x10000 {
            return String::new();
        }
        crate::process::read_cstr(ptr, 32).unwrap_or_default()
    }

    /// Убийства — ключ `"frags"` в таблице счёта.
    pub fn frags(&self) -> i32 {
        // SAFETY: адрес проверен в `for_index`.
        unsafe { crate::process::read(self.addr + crate::offsets::client::PLAYER_EXTRA_FRAGS) }
    }

    /// Смерти — ключ `"deaths"` в таблице счёта.
    pub fn deaths(&self) -> i32 {
        // SAFETY: адрес проверен в `for_index`.
        unsafe { crate::process::read(self.addr + crate::offsets::client::PLAYER_EXTRA_DEATHS) }
    }

    /// Здоровье — ключ `"health"`.
    pub fn health(&self) -> i32 {
        // SAFETY: адрес проверен в `for_index`.
        unsafe { crate::process::read(self.addr + crate::offsets::client::PLAYER_EXTRA_HEALTH) }
    }

    /// Деньги — ключ `"account"`.
    pub fn money(&self) -> i32 {
        // SAFETY: адрес проверен в `for_index`.
        unsafe { crate::process::read(self.addr + crate::offsets::client::PLAYER_EXTRA_MONEY) }
    }
}

/// Имя модели без каталогов и расширения.
///
/// `models/player/arctic/arctic.mdl` → `arctic`,
/// `terror\terror.mdl` → `terror`, `v_m4a1.mdl` → `v_m4a1`.
pub fn model_stem(name: &str) -> &str {
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match file.rfind('.') {
        Some(dot) if file[dot..].eq_ignore_ascii_case(".mdl") => &file[..dot],
        _ => file,
    }
}

/// Каталог модели — предпоследняя часть пути (`terror\terror.mdl` → `terror`).
fn model_dir(name: &str) -> Option<&str> {
    let mut parts = name.rsplit(['/', '\\']);
    parts.next()?;
    parts.next()
}

/// Похожа ли модель на модель игрока.
///
/// Признаки (любой достаточен):
/// * в пути есть часть `player` (`models/player/...`);
/// * каталог совпадает с именем файла — так уложены все модели игроков CS
///   (`terror\terror.mdl`, `arctic\arctic.mdl`), а оружие и пропы лежат иначе.
///
/// Модели в руках (`v_`) и на земле (`w_`) отбрасываются всегда: у них тоже
/// есть кости, но к игроку они отношения не имеют.
pub fn is_player_model_name(name: &str) -> bool {
    let stem = model_stem(name).to_ascii_lowercase();
    if stem.starts_with("v_") || stem.starts_with("w_") {
        return false;
    }
    if name
        .split(['/', '\\'])
        .any(|p| p.eq_ignore_ascii_case("player"))
    {
        return true;
    }
    model_dir(name).is_some_and(|dir| dir.eq_ignore_ascii_case(&stem))
}

/// Читаемое имя оружия по имени модели: `w_ak47.mdl` → `AK-47`.
///
/// Неизвестное имя не выбрасывается: возвращается в верхнем регистре без
/// префикса, чтобы на экране было видно хотя бы «что-то осмысленное».
pub fn weapon_display_name(model_name: &str) -> String {
    let stem = model_stem(model_name).to_ascii_lowercase();
    let bare = stem
        .strip_prefix("w_")
        .or_else(|| stem.strip_prefix("p_"))
        .or_else(|| stem.strip_prefix("v_"))
        .unwrap_or(&stem);

    let known = match bare {
        "knife" => "Knife",
        "ak47" => "AK-47",
        "m4a1" => "M4A1",
        "aug" => "AUG",
        "sg552" => "SG 552",
        "sg550" => "SG 550",
        "g3sg1" => "G3SG1",
        "awp" => "AWP",
        "scout" => "Scout",
        "m3" => "M3",
        "xm1014" => "XM1014",
        "mp5" | "mp5navy" => "MP5",
        "tmp" => "TMP",
        "p90" => "P90",
        "mac10" => "MAC-10",
        "ump45" => "UMP-45",
        "galil" => "Galil",
        "famas" => "FAMAS",
        "deagle" => "Deagle",
        "usp" => "USP",
        "glock18" => "Glock-18",
        "p228" => "P228",
        "elite" => "Berettas",
        "fiveseven" => "Five-SeveN",
        "hegrenade" => "HE",
        "flashbang" => "Flash",
        "smokegrenade" => "Smoke",
        "c4" => "C4",
        _ => return bare.to_ascii_uppercase(),
    };
    known.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_strips_dirs_and_extension() {
        assert_eq!(model_stem("models/player/arctic/arctic.mdl"), "arctic");
        assert_eq!(model_stem("terror\\terror.mdl"), "terror");
        assert_eq!(model_stem("v_m4a1.mdl"), "v_m4a1");
        assert_eq!(model_stem("noext"), "noext");
        assert_eq!(model_stem("weird.MDL"), "weird");
    }

    #[test]
    fn player_models_are_recognised() {
        assert!(is_player_model_name("terror\\terror.mdl"));
        assert!(is_player_model_name("arctic\\arctic.mdl"));
        assert!(is_player_model_name("models/player/urban/urban.mdl"));
        assert!(!is_player_model_name("v_m4a1.mdl"));
        assert!(!is_player_model_name("w_glock18.mdl"));
        assert!(!is_player_model_name("models/w_awp.mdl"));
        assert!(!is_player_model_name("crate.mdl"));
        assert!(!is_player_model_name("models/props/crate/crate2.mdl"));
    }

    #[test]
    fn team_comes_from_model() {
        assert_eq!(
            Team::from_model_name("terror\\terror.mdl"),
            Some(Team::Terrorist)
        );
        assert_eq!(
            Team::from_model_name("models/player/leet/leet.mdl"),
            Some(Team::Terrorist)
        );
        assert_eq!(
            Team::from_model_name("arctic\\arctic.mdl"),
            Some(Team::Terrorist)
        );
        assert_eq!(
            Team::from_model_name("guerilla\\guerilla.mdl"),
            Some(Team::Terrorist)
        );
        assert_eq!(
            Team::from_model_name("urban\\urban.mdl"),
            Some(Team::CounterTerrorist)
        );
        assert_eq!(
            Team::from_model_name("gsg9\\gsg9.mdl"),
            Some(Team::CounterTerrorist)
        );
        assert_eq!(
            Team::from_model_name("sas\\sas.mdl"),
            Some(Team::CounterTerrorist)
        );
        assert_eq!(
            Team::from_model_name("gign\\gign.mdl"),
            Some(Team::CounterTerrorist)
        );
        assert_eq!(Team::from_model_name("v_m4a1.mdl"), None);
        assert_eq!(Team::from_model_name("custom\\custom.mdl"), None);
    }

    #[test]
    fn teams_have_distinct_colors() {
        assert_ne!(Team::Terrorist.color().r, Team::CounterTerrorist.color().r);
    }
}
