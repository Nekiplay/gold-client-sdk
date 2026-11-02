//! Запуск локальной игры с ботами и инъекция своей DLL.
//!
//! Стенд для проверки модов без ручных действий: поднять оффлайн-матч с ботами,
//! дождаться окна и карты, загрузить пример, потом погасить процесс.
//!
//! Запуск и инъекция лежат в [`golden_sdk::host`] (там же — Win32 без `std`),
//! а здесь удобный слой: конфигурация командной строки, поиск установленной игры,
//! ожидание готовности и сборка типичных аргументов для CS 1.6.
//!
//! ```no_run
//! use golden_std::launch::{Game, GameConfig};
//!
//! # fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = GameConfig::find()?.with_map("de_dust2").with_bots(4);
//! let game = Game::launch(&config)?;
//! game.wait_for_window(std::time::Duration::from_secs(30))?;
//! game.inject("example_auto_bhop.dll")?;
//! # Ok(())
//! # }
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use golden_sdk::host::{self, Child, HostError};

/// Ошибка стенда.
#[derive(Debug)]
pub enum LaunchError {
    /// Игра не найдена: не указан путь и нет её в известных местах.
    NotFound,
    /// Процесс не запустился.
    Host(HostError),
    /// Окно не появилось за отведённое время.
    NoWindow,
    /// Карта не загрузилась за отведённое время.
    NoMap,
    /// Не удалось загрузить DLL (нет файла или процесс её не принял).
    Inject(String),
}

impl core::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotFound => write!(f, "игра не найдена: укажите путь к hl.exe"),
            Self::Host(error) => write!(f, "запуск игры: {error}"),
            Self::NoWindow => write!(f, "окно игры не появилось"),
            Self::NoMap => write!(f, "карта не загрузилась"),
            Self::Inject(detail) => write!(f, "инъекция DLL: {detail}"),
        }
    }
}

impl std::error::Error for LaunchError {}

impl From<HostError> for LaunchError {
    fn from(value: HostError) -> Self {
        Self::Host(value)
    }
}

/// Что запускать и с какими настройками.
#[derive(Debug, Clone)]
pub struct GameConfig {
    /// Путь к `hl.exe`.
    pub exe: PathBuf,
    /// Каталог игры (там лежат `cstrike`, `hw.dll`).
    pub dir: PathBuf,
    /// Карта, которую грузим сразу.
    pub map: String,
    /// Сколько ботов добавить.
    pub bots: u32,
    /// Максимум игроков на сервере.
    pub max_players: u32,
    /// Запускать в окне без рамки: только в окне работают скриншоты и
    /// `PostMessage`, а `DirectX` в монопольном полноэкранном режиме GDI не снять.
    pub windowed: bool,
    /// Размер окна в пикселях (ширина, высота).
    pub size: (u32, u32),
    /// Дополнительные аргументы командной строки.
    pub extra: Vec<String>,
}

impl GameConfig {
    /// Конфигурация для конкретного `hl.exe`: каталог игры — его родитель.
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        let exe = exe.into();
        let dir = exe
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            exe,
            dir,
            map: "de_dust2".to_string(),
            bots: 4,
            max_players: 16,
            windowed: true,
            size: (1920, 1080),
            extra: Vec::new(),
        }
    }

    /// Ищет установленную игру в известных местах.
    ///
    /// Сначала переменная окружения `GOLDCLIENT_DIR`, затем каталоги, где игра
    /// чаще всего и лежит на машинах разработки.
    pub fn find() -> Result<Self, LaunchError> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(dir) = std::env::var("GOLDCLIENT_DIR") {
            candidates.push(PathBuf::from(dir));
        }
        candidates.push(PathBuf::from(r"C:\Games\CS 1.6 GoldClient"));
        for dir in candidates {
            let exe = dir.join("hl.exe");
            if exe.is_file() {
                return Ok(Self::new(exe));
            }
        }
        Err(LaunchError::NotFound)
    }

    /// Карта, которая грузится при старте.
    pub fn with_map(mut self, map: impl Into<String>) -> Self {
        self.map = map.into();
        self
    }

    /// Сколько ботов поднять.
    pub fn with_bots(mut self, bots: u32) -> Self {
        self.bots = bots;
        self
    }

    /// Максимум игроков.
    pub fn with_max_players(mut self, max_players: u32) -> Self {
        self.max_players = max_players;
        self
    }

    /// Дополнительные аргументы (как есть, через пробел).
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.extra.extend(args.into_iter().map(Into::into));
        self
    }

    /// Оконный режим (`-windowed -noborder`) и размер окна.
    pub fn with_window(mut self, windowed: bool, size: (u32, u32)) -> Self {
        self.windowed = windowed;
        self.size = size;
        self
    }

    /// Командная строка: оффлайн-сервер с ботами, консоль, окно без рамки.
    ///
    /// `-steam -noforcemparms` — то, с чем игра запускается штатным ярлыком;
    /// без них GoldClient ведёт себя иначе (`steam_api` в поставке).
    /// `-insecure` обязателен: без него клиент GoldSrc не даёт поднять
    /// локальный сервер без авторизации, а `+sv_lan 1` убирает проверки.
    pub fn command_line(&self) -> String {
        let mut args = vec![
            "-console".to_string(),
            "-insecure".to_string(),
            "-steam".to_string(),
            "-noforcemparms".to_string(),
            "-game".to_string(),
            "cstrike".to_string(),
            "+sv_lan".to_string(),
            "1".to_string(),
            "+maxplayers".to_string(),
            self.max_players.to_string(),
            "+map".to_string(),
            self.map.clone(),
            "+bot_quota".to_string(),
            self.bots.to_string(),
            "+bot_join_after_player".to_string(),
            "0".to_string(),
            "+bot_auto_vacate".to_string(),
            "1".to_string(),
        ];
        if self.windowed {
            args.extend([
                "-windowed".to_string(),
                "-noborder".to_string(),
                "-w".to_string(),
                self.size.0.to_string(),
                "-h".to_string(),
                self.size.1.to_string(),
            ]);
        }
        args.extend(self.extra.iter().cloned());
        args.join(" ")
    }
}

/// Запущенная игра.
#[derive(Debug)]
pub struct Game {
    child: Child,
    config: GameConfig,
}

impl Game {
    /// Запускает игру по конфигурации.
    pub fn launch(config: &GameConfig) -> Result<Self, LaunchError> {
        let exe = config.exe.to_string_lossy().into_owned();
        let dir = config.dir.to_string_lossy().into_owned();
        let args = config.command_line();
        let child = Child::launch(&exe, &args, &dir)?;
        Ok(Self {
            child,
            config: config.clone(),
        })
    }

    /// PID запущенного процесса.
    pub fn pid(&self) -> u32 {
        self.child.pid()
    }

    /// Конфигурация, с которой игра запущена.
    pub fn config(&self) -> &GameConfig {
        &self.config
    }

    /// Ждёт окно игры не дольше `timeout`.
    pub fn wait_for_window(&self, timeout: Duration) -> Result<usize, LaunchError> {
        self.child
            .wait_for_window(timeout.as_millis() as u32)
            .ok_or(LaunchError::NoWindow)
    }

    /// Ждёт, пока в логе рядом с DLL появится строка `needle`.
    ///
    /// Готовность карты снаружи иначе не увидеть: `ref_params` лежат в чужом
    /// процессе, а SDK читает свою копию памяти. Зато мод пишет в лог всё, что
    /// нужно, и по этим строкам удобно ждать: «перехват Key_Event включён»,
    /// «в игре true».
    pub fn wait_for_log(
        &self,
        log: impl AsRef<Path>,
        needle: &str,
        timeout: Duration,
    ) -> Result<(), LaunchError> {
        let log = log.as_ref();
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(text) = std::fs::read_to_string(log) {
                if text.contains(needle) {
                    return Ok(());
                }
            }
            if !self.child.is_running() {
                return Err(LaunchError::Host(HostError::Spawn));
            }
            if Instant::now() >= deadline {
                return Err(LaunchError::NoMap);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Загружает `dll` в процесс игры.
    ///
    /// После загрузки свежей сборки игру нужно перезапускать: копия DLL,
    /// загруженная в прошлый раз, остаётся в процессе и дублирует работу.
    pub fn inject(&self, dll: impl AsRef<Path>) -> Result<(), LaunchError> {
        let dll = dll.as_ref();
        if !dll.is_file() {
            return Err(LaunchError::Inject(format!(
                "файл не найден: {}",
                dll.display()
            )));
        }
        // Путь обязан быть абсолютным: `LoadLibraryA` в процессе игры разрешает
        // относительные пути от своего рабочего каталога, а не от нашего.
        let dll = dll.canonicalize().unwrap_or_else(|_| dll.to_path_buf());
        let path = dll.to_string_lossy().into_owned();
        host::inject(self.child.pid(), &path)
            .map_err(|error| LaunchError::Inject(error.to_string()))
    }

    /// Процесс ещё жив.
    pub fn is_running(&self) -> bool {
        self.child.is_running()
    }

    /// Текущее окно игры.
    pub fn window(&self) -> Option<usize> {
        self.child.window()
    }

    /// Пишет строку в консоль игры, как будто её набрали руками, и жмёт Enter.
    ///
    /// Так стенд умеет то, что снаружи иначе не сделать: посадить игрока на
    /// команду (`jointeam 2`, `joinclass 1`), включить ботов, выдать оружие.
    /// Символы уходят сообщениями `WM_CHAR`, поэтому попадают в консоль GoldSrc
    /// даже когда окно не в фокусе.
    ///
    /// `Escape` перед набором закрывает меню выбора команды: иначе консоль
    /// открыта, а ввод уходит в интерфейс.
    pub fn console(&self, command: &str) -> Result<(), LaunchError> {
        let window = self.child.window().ok_or(LaunchError::NoWindow)?;
        for byte in command.bytes() {
            if byte == b'\n' {
                continue;
            }
            host::post_char(window, byte);
            std::thread::sleep(Duration::from_millis(4));
        }
        host::post_char(window, b'\r');
        std::thread::sleep(Duration::from_millis(30));
        host::post_key(window, true, host::VK_RETURN);
        host::post_key(window, false, host::VK_RETURN);
        Ok(())
    }

    /// Закрывает меню/диалог игры клавишей `Escape`.
    pub fn escape(&self) -> Result<(), LaunchError> {
        let window = self.child.window().ok_or(LaunchError::NoWindow)?;
        host::post_key(window, true, host::VK_ESCAPE);
        std::thread::sleep(Duration::from_millis(30));
        host::post_key(window, false, host::VK_ESCAPE);
        Ok(())
    }

    /// Завершает игру.
    pub fn kill(&self) -> bool {
        self.child.terminate()
    }
}
