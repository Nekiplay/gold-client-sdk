# golden-client SDK

SDK для разработки модов под [Gold Client](https://goldclient.ru) — клиент Counter-Strike 1.6.

Крейты дают доступ к движку игры (`hw.dll`) и клиенту (`client.dll`) из своей DLL:
память, сущности, studio-модели, хуки, отрисовка поверх кадра, консольные команды,
ввод, планировщик по кадрам и корректная выгрузка без перезапуска игры.

Готовые примеры лежат в `examples/`, ими же проверяется работа SDK в живой игре.

## Что внутри

| Крейт | std | Зачем |
|---|---|---|
| `crates/golden-sdk` | **нет** (`#![no_std]` + `alloc`) | Всё, что нужно от игры: адреса и смещения, чтение/запись памяти, сущности, разбор `.mdl`, интерфейс studio-рендерера, OpenGL, хуки (`retour`), консольные команды, лог, Win32 вызовы напрямую. |
| `crates/golden-std` | да | Удобный слой поверх SDK: **`esp` (готовый ESP одной строкой)**, `Overlay` (контур и подписи игроков), позы и кэш поз, реестр хуков с автоматическим снятием, реестр консольных команд, блокировка клавиш, планировщик по кадрам, запуск игры с ботами, выгрузка DLL. |

SDK намеренно не тянет `std`: его можно собирать и использовать там, где `std`
недоступен, а всё системное сделано прямыми вызовами Win32 (`GetTickCount64`,
`CreateRemoteThread`, `LoadLibraryA`, `EnumWindows`, `WriteFile` и т.д.).

## Требования

* Rust (edition 2024) и цель `i686-pc-windows-msvc` — игра 32-битная.
* MSVC build tools.
* Windows.

```powershell
rustup target add i686-pc-windows-msvc
```

## Сборка

```powershell
cargo build --workspace --release --target i686-pc-windows-msvc
cargo test --workspace
```

DLL примеров появятся в `target/i686-pc-windows-msvc/release/`.

## Быстрый старт

Минимальный мод — перехват отрисовки игрока и контур вокруг модели:

```rust
use golden_std::{hooks, Color, OutlineStyle, Overlay};

// Свой перехват: макрос объявляет функцию с нужным ABI и аксессор к оригиналу.
golden_std::hook! {
    unsafe extern "thiscall" fn on_setup_bones(this: *mut core::ffi::c_void, flags: i32)
        original: setup_bones_original
        -> i32
    {
        let bones = setup_bones_original()(this, flags);
        // ... своя работа с костями ...
        bones
    }
}

// Установка: хук снимется сам при `golden_std::unload::unload()`.
unsafe { hooks::install(target_address, on_setup_bones as *const ()) }.ok();
```

## ESP одной строкой

Готовый ESP живёт в `golden_std::esp`: контур модели и подписи, видимые сквозь
любую геометрию. Пример настраивает только то, чем один ESP отличается от другого.

```rust
use golden_sdk::player::Team;
use golden_std::{Color, Player};

golden_std::esp::Esp::new()
    .width(2.0)
    .enemies_only(true)
    .color(|player: &Player| player.team().map_or(Color::WHITE, Team::color))
    .visible_color(|_: &Player| Color::GREEN)
    .label(|player: &Player| (Some(player.name()), player.weapon()))
    .install()?;
```

* `enemies_only` — рисовать только врагов (по умолчанию включено); своих не
  обводим ни контуром, ни подписями;
* `color` — цвет контура и подписей, по умолчанию цвет команды;
* `visible_color` — цвет видимой части: контур рисуется вторым проходом с
  тестом глубины, так что закрытые стеной и чужими телами части остаются в
  базовом цвете, а видимые становятся зелёными;
* `label` — первая строка над головой, вторая под ногами;
* `width` — толщина линий.

Внутри: подмена слотов интерфейса клиента, перехват `StudioRenderModel`
(подписи ставятся там, где кости принадлежат рисуемой модели), перехват конца
3D-сцены (`HUD_DrawNormalTriangles`), где и обводка, и подписи рисуются уже
поверх мира, ящиков и контейнеров, и отложенная перерисовка модели движком с
подстановкой «текущей сущности».

Счётчики для проверки в игре — `golden_std::esp::drawn()` и
`golden_std::esp::labels()`.

Контур и подписи вручную, если нужен свой порядок отрисовки:

```rust
let overlay = Overlay::new();
overlay.on_draw_model(|model| {
    // цвет выбирается по модели: команда игрока, а не локального игрока
    overlay.outline_of(|model| OutlineStyle::new(team_color(model)).width(2.0));
});
```

## Гранаты: траектория и точка взрыва

`golden_sdk::grenade` повторяет формулы сервера, а не «красивую физику»:
углы и скорость броска из `WeaponIdle`, полёт с отскоками из
`SV_Physics_Bounce`, точку срабатывания из `CGrenade::Detonate`, урон из
`RadiusDamage` и ослепление из `RadiusFlash`.

```rust
use golden_sdk::grenade::{Kind, he_damage, flash_list, simulate, throw_velocity};

let velocity = throw_velocity(view_angles, player_velocity);
let flight = simulate(Kind::He, throw_origin(eye, view_angles), velocity, |from, to| {
    map_bsp.trace(from, to).map(Into::into)
});
let victims = he_damage(flight.explosion, &targets);
```

`golden_std::grenade::predict(kind)` подставляет в эти формулы живое
состояние игры: глаза и скорость локального игрока, позиции и углы остальных,
трассировку по текущей карте. Возвращает `Prediction` с траекторией, уроном и
списком ослеплённых.

Пример `examples/grenade` рисует траекторию в конце 3D-сцены и печатает отчёт
по командам `grenade_he` / `grenade_flash` / `grenade_smoke`; `grenade_off`
выключает отрисовку. Траектория показывается и автоматически — как только
граната оказалась в руках.

Своя консольная команда — со своим выводом и без мусора в консоли:

```rust
use golden_std::concommand::{self, Command, CommandArgs};

unsafe extern "C" fn status(_args: *const CommandArgs) {
    concommand::print("мод работает");
}

// `hidden()` — не показывать строку выполнения, `bind()` — звать по клавише.
Command::new("my_status", status)
    .help("состояние мода")
    .hidden()
    .register()
    .ok();
```

Выгрузка: всё, что зарегистрировано через `golden_std`, снимается само —
хуки, команды, перехват ввода, потоки — и только потом освобождается DLL.

```rust
// Из кода мода: разгружаемся через 300 мс, чтобы не выгружать себя из своего же стека.
golden_std::unload::defer_unload(300);
```

## Примеры

| Пример | Что показывает |
|---|---|
| `examples/esp` | ESP: контур модели и подписи (имя, оружие) по команде игрока, видно сквозь стены; всё настраивается через `golden_std::esp::Esp`. |
| `examples/auto-bhop` | Автопрыжок без потери скорости: инъекция колеса мыши в окно игры. |
| `examples/model-viewer` | Разбор и просмотр studio-моделей. |
| `examples/hitboxes` | Хитбоксы игроков по нарисованной позе: боксы строятся в перехвате отрисовки модели, по тем же матрицам костей, из которых движок строит hull для трассировки выстрела. |
| `examples/diagnostics` | Счётчики и проверки состояния движка. |
| `examples/console` | Вывод в консоль Gold Client: цветной отчёт билдером, обычная строка и своя команда. |
| `examples/grenade` | Траектория гранаты и точка взрыва: для HE — урон по игрокам, для флешки — кого и на сколько ослепит, для смока — где он задымит. Формулы сервера в `golden_sdk::grenade`, состояние игры — в `golden_std::grenade`. |
| `examples/triggerbot` | Триггербот без обводки: цели — враги из перехвата отрисовки ESP с выключенным рисованием (`Esp::outline(false)`), хитбоксы — из позы движкового рендера, «не закрыта ли» — по буферу глубины; огонь — бит `IN_ATTACK` в исходящей команде игрока (`CL_CreateMove`), без командного буфера; кнопка активации — `Trigger::activation` (в примере — `mouse5`, опрос системный через `input::Key`). Механика в `golden_std::trigger`. |
| `examples/aimbot` | Аим на ближайшую к прицелу часть тела врага: хитбоксы — из того же перехвата отрисовки ESP, угол доводится плавно (`aim_smooth`, сектор поиска `aim_fov`), пишется в `cl.viewangles` из перехвата `CL_CreateMove` — то самое поле, из которого движок собирает и камеру, и команду серверу. Не стреляет, пока прицел не на враге: огонь разрешает триггербот с той же кнопкой активации. Механика в `golden_std::aim`. |

## Инъектор

Инъектор — `tools/inject`:

```powershell
target\i686-pc-windows-msvc\release\golden-inject.exe <pid hl.exe> <путь к dll>
```

## Документация по движку

`docs/STUDIO_REVERSE.md` — разбор studio-пути Gold Client: цепочка
`HUD_GetStudioModelInterface`, таблица `engine_studio_api_t`, layout объекта
`CStudioModelRenderer` и его vtable, адреса `Mod_*`/`R_Studio*`, формат `.mdl`,
рецепты хуков.

## Для агентов

`AGENTS.md` — раскладка репозитория, правила сборки и проверки в живой игре,
проверенные грабли движка. В `skills/` лежат рабочие инструкции: разбор движка
(Ghidra/IDA), автоматизация игры и скриншоты, общий цикл моддинга.

## Область применения

SDK для модов, которые вы запускаете на своей машине и на своих серверах: своя
игра с ботами, локальные серверы, одиночные режимы.

Чего здесь нет и не будет: обхода античита, DRM и проверок владения игрой,
а также читов против других игроков (аимботы, ESP на чужих серверах,
ускорение игры и т.п.).

Игровые файлы, извлечённые ассеты и декомпилированный код в репозиторий не
попадают — только исходники SDK.

## Лицензия

MIT.