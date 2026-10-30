//! Игра и её объекты: [`Game`], [`Player`], [`Entity`].
//!
//! Все чтения идут через `golden-sdk`; здесь только удобный синтаксис.
//! Смещения не дублируются — они лежат в `golden_sdk::offsets`.

use golden_sdk::engine::{EngineStudioApi, EntityRef};
use golden_sdk::math::{Camera, Vec3};
use golden_sdk::player::{self, PlayerExtra, Team};
use golden_sdk::studio::StudioModel;

/// Точка входа в игру.
///
/// `None`, если движок ещё не отдал клиенту studio-интерфейс (например,
/// вызов из `DllMain` до `HUD_GetStudioModelInterface`).
#[derive(Debug, Clone, Copy)]
pub struct Game {
    api: &'static EngineStudioApi,
}

impl Game {
    /// Хендл игры текущего процесса.
    pub fn get() -> Option<Self> {
        Some(Self {
            api: EngineStudioApi::for_engine()?,
        })
    }

    /// Все сущности мира, у которых есть модель.
    pub fn get_entities(&self) -> impl Iterator<Item = Entity> {
        (1..=EngineStudioApi::MAX_EDICTS).filter_map(move |index| self.get_entity(index))
    }

    /// Все игроки в игре.
    ///
    /// Игрок определяется по записи `g_PlayerExtraInfo` — так же, как это делает
    /// сам клиент для таблицы счёта: запись есть у всех, кого сервер представил
    /// сообщением `TeamInfo`/`ScoreAttrib`, а не только у нарисованных на экране.
    ///
    /// ```
    /// # use golden_std::Game;
    /// # fn example(game: Game) {
    /// for name in game.get_players().map(|p| p.name()) { /* ... */ }
    /// # }
    /// ```
    pub fn get_players(&self) -> Players {
        Players { game: *self, next: -1 }
    }

    /// Игрок по номеру сущности.
    pub fn get_player(&self, index: i32) -> Option<Player> {
        let player = Player::by_index(index)?;
        let entity = self.get_entity(index);
        Some(match entity {
            Some(entity) => player.with_entity(entity),
            None => player,
        })
    }

    /// Сущность по номеру.
    pub fn get_entity(&self, index: i32) -> Option<Entity> {
        let raw = self.api.entity_by_index(index)?;
        Entity::from_raw(index, raw)
    }

    /// Локальный игрок — тот, из чьих глаз смотрит камера.
    pub fn get_local_player(&self) -> Option<Player> {
        self.get_player(self.view_entity()?)
    }

    /// Номер сущности, из которой смотрит камера.
    pub fn view_entity(&self) -> Option<i32> {
        Some(self.api.times()?.view_entity)
    }

    /// Камера кадра: позиция, направление и базис.
    pub fn get_view(&self) -> Option<View> {
        let (origin, up, right, forward) = self.api.view_info()?;
        Some(View {
            origin,
            up,
            right,
            forward,
        })
    }

    /// Камера в виде, готовом для проецирования мировых точек на экран.
    pub fn get_camera(&self) -> Option<Camera> {
        let view = self.get_view()?;
        Some(Camera::new(view.origin, view.forward, view.right, view.up))
    }

    /// Выполнить консольную команду: `game.run_command("+jump")`.
    ///
    /// Команда уходит в командный буфер движка и выполняется на ближайшем
    /// кадре — как если бы её ввели в консоли.
    pub fn run_command(&self, command: &str) -> bool {
        golden_sdk::engine::run_command(command)
    }

    /// Состояние движка текущего кадра: `game.state().ducking()`.
    ///
    /// Отдельный хендл не нужен — это снимок глобалов, а не объект с полями.
    pub fn state(&self) -> State {
        State
    }

    /// Клиент загрузил карту и мир идёт.
    pub fn is_in_game(&self) -> bool {
        State::in_game()
    }

    /// Текущий FOV с поправкой на аспект окна: `game.fov()`.
    ///
    /// Возвращает горизонтальный угол в градусах для фактического разрешения —
    /// тот, что видит игрок, а не `90` из соглашения 4:3.
    pub fn fov(&self) -> f32 {
        golden_sdk::state::horizontal_fov(State::fov())
    }
}

/// Состояние движка: `Game::state()`.
pub use golden_sdk::state::EngineState as State;

/// Камера кадра в мировых координатах.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    /// Позиция глаза.
    pub origin: Vec3,
    /// Направление вверх в экранных координатах.
    pub up: Vec3,
    /// Направление вправо.
    pub right: Vec3,
    /// Направление взгляда.
    pub forward: Vec3,
}

/// Сущность мира с моделью.
#[derive(Debug, Clone, Copy)]
pub struct Entity {
    index: i32,
    raw: EntityRef,
}

impl Entity {
    /// Обёртка над сущностью движка.
    ///
    /// `None`, если у сущности нет модели: невидимые маркеры и триггеры никому
    /// не нужны, а `model_t` у них пустой.
    pub fn from_raw(index: i32, raw: EntityRef) -> Option<Self> {
        if raw.model().is_null() {
            return None;
        }
        Some(Self { index, raw })
    }

    /// Номер сущности в движке.
    pub fn index(&self) -> i32 {
        self.index
    }

    /// Позиция в мире.
    pub fn origin(&self) -> Vec3 {
        self.raw.origin()
    }

    /// Заголовок студийной модели сущности.
    pub fn studio_model(&self) -> Option<StudioModel> {
        self.raw.studio_model()
    }

    /// Имя модели: `models/player/urban/urban.mdl`.
    pub fn model_name(&self) -> String {
        self.studio_model()
            .map(|model| model.name().to_string())
            .unwrap_or_default()
    }

    /// Модель игрока, а не оружия/пропа.
    pub fn is_player_model(&self) -> bool {
        self.studio_model().is_some_and(|model| model.is_player_model())
    }

    /// Низкоуровневый доступ — через `golden_sdk`, если чего-то не хватает.
    pub fn raw(&self) -> EntityRef {
        self.raw
    }
}

/// Сколько слотов игроков ведёт клиент (`g_PlayerExtraInfo`).
pub const MAX_PLAYERS: i32 = 63;

/// Обход всех игроков: [`Game::get_players`].
///
/// Ленивый: каждая запись читается в момент обращения, поэтому итерировать
/// можно в любом месте кадра.
#[derive(Debug, Clone, Copy)]
pub struct Players {
    game: Game,
    next: i32,
}

impl Players {
    /// Только живые.
    pub fn alive(self) -> impl Iterator<Item = Player> {
        self.filter(|player| player.is_alive())
    }

    /// Только указанная команда.
    pub fn team(self, team: Team) -> impl Iterator<Item = Player> {
        self.filter(move |player| player.team() == Some(team))
    }
}

impl Iterator for Players {
    type Item = Player;

    fn next(&mut self) -> Option<Self::Item> {
        while self.next < MAX_PLAYERS {
            self.next += 1;
            let index = self.next;
            if let Some(player) = self.game.get_player(index) {
                if player.is_known() {
                    return Some(player);
                }
            }
        }
        None
    }
}

/// Игрок.
///
/// Всё, что меняется медленно — ник, команда, счёт, деньги, флаги бомбы и
/// кита — читается из `g_PlayerExtraInfo`: той же записи, из которой сам клиент
/// собирает таблицу счёта. Поэтому данные стабильны и не зависят от того, что
/// и в каком порядке движок рисует в этом кадре.
///
/// Сущность (`entity`) появляется только тогда, когда движок её уже отдал;
/// в хуке отрисовки это сущность, которую он рисует прямо сейчас.
#[derive(Debug, Clone, Copy)]
pub struct Player {
    index: i32,
    entity: Option<Entity>,
    weapon_model: Option<i32>,
}

impl Player {
    /// Игрок по номеру: запись в `g_PlayerExtraInfo` уже заполнена сервером.
    ///
    /// `None`, если слот пуст — ни имени, ни команды.
    pub fn by_index(index: i32) -> Option<Self> {
        let extra = PlayerExtra::for_index(index)?;
        let known = !extra.name().is_empty() || extra.team().is_some();
        known.then_some(Self {
            index,
            entity: None,
            weapon_model: None,
        })
    }

    /// Игрок из сущности, которую движок отдаёт прямо сейчас.
    ///
    /// Нужен в хуке отрисовки: только у этой сущности есть модель оружия в руках.
    pub fn from_raw(index: i32, raw: EntityRef) -> Option<Self> {
        Some(Self {
            index,
            entity: Entity::from_raw(index, raw),
            weapon_model: None,
        })
    }

    /// Номер модели оружия, которую движок рисует в руках прямо сейчас.
    pub fn with_weapon_model(mut self, index: Option<i32>) -> Self {
        self.weapon_model = index;
        self
    }

    /// Добавляет сущность, если её ещё нет.
    pub fn with_entity(mut self, entity: Entity) -> Self {
        self.entity = Some(entity);
        self
    }

    /// Запись `g_PlayerExtraInfo[index]` — ник, команда, счёт, флаги.
    fn extra(&self) -> Option<PlayerExtra> {
        PlayerExtra::for_index(self.index)
    }

    /// Сервер уже рассказал про этот слот: есть ник или команда.
    ///
    /// Пустые слоты (до первого `TeamInfo`) отсеиваются по этому признаку.
    pub fn is_known(&self) -> bool {
        let Some(extra) = self.extra() else {
            return false;
        };
        !extra.name().is_empty() || extra.team_number() != 0
    }

    /// Номер сущности — он же номер игрока в движке.
    pub fn index(&self) -> i32 {
        self.index
    }

    /// Ник.
    pub fn name(&self) -> String {
        let name = self.extra().map(|extra| extra.name()).unwrap_or_default();
        if name.is_empty() {
            self.entity
                .map(|entity| entity.raw().player_name())
                .unwrap_or_default()
        } else {
            name
        }
    }

    /// Команда: сначала `g_PlayerExtraInfo`, затем имя модели.
    pub fn team(&self) -> Option<Team> {
        self.extra()
            .and_then(|extra| extra.team())
            .or_else(|| Team::from_model_name(&self.model_name()))
    }

    /// Здоровье; у мёртвого отрицательное.
    pub fn health(&self) -> i32 {
        self.extra().map(|extra| extra.health()).unwrap_or_default()
    }

    /// Убийства.
    pub fn frags(&self) -> i32 {
        self.extra().map(|extra| extra.frags()).unwrap_or_default()
    }

    /// Смерти.
    pub fn deaths(&self) -> i32 {
        self.extra().map(|extra| extra.deaths()).unwrap_or_default()
    }

    /// Деньги.
    pub fn money(&self) -> i32 {
        self.extra().map(|extra| extra.money()).unwrap_or_default()
    }

    /// Несёт бомбу.
    pub fn has_bomb(&self) -> bool {
        self.extra().is_some_and(|extra| extra.has_bomb())
    }

    /// Несёт дефуз-кит.
    pub fn has_defuser(&self) -> bool {
        self.extra().is_some_and(|extra| extra.has_defuser())
    }

    /// Жив.
    pub fn is_alive(&self) -> bool {
        !self.extra().is_some_and(|extra| extra.is_dead())
    }

    /// Мёртв (в том числе «ждёт респавна»).
    pub fn is_dead(&self) -> bool {
        !self.is_alive()
    }

    /// Имя команды как его пишет сервер: `TERRORIST`, `CT`, `UNASSIGNED`.
    pub fn team_name(&self) -> String {
        self.extra().map(|extra| extra.team_name()).unwrap_or_default()
    }

    /// Цвет команды для отрисовки; `None` — команда неизвестна.
    pub fn color(&self) -> Option<golden_sdk::draw::Color> {
        self.team().map(|team| team.color())
    }

    /// Позиция в мире.
    pub fn origin(&self) -> Vec3 {
        self.entity.map(|entity| entity.origin()).unwrap_or(Vec3::ZERO)
    }

    /// Имя модели тела.
    pub fn model_name(&self) -> String {
        self.entity
            .map(|entity| entity.model_name())
            .unwrap_or_default()
    }

    /// Оружие в руках, как его называет игра: `AK-47`, `Knife`.
    ///
    /// Источник — модель оружия из `player_info` (её движок и рисует), с
    /// запасным вариантом в виде поля сущности.
    pub fn weapon(&self) -> Option<String> {
        let index = self
            .weapon_model
            .or_else(|| self.entity.map(|entity| entity.raw().weapon_model_index()))?;
        let api = EngineStudioApi::for_engine()?;
        let model = api.model_name_by_index(index)?;
        Some(player::weapon_display_name(&model))
    }

    /// Сущность игрока, если движок её уже отдал.
    pub fn entity(&self) -> Option<Entity> {
        self.entity
    }
}