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
| `crates/golden-std` | да | Удобный слой поверх SDK: `Overlay` (контур и подписи игроков), позы и кэш поз, реестр хуков с автоматическим снятием, реестр консольных команд, блокировка клавиш, планировщик по кадрам, запуск игры с ботами, выгрузка DLL. |

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

Контур и подписи:

```rust
let overlay = Overlay::new();
overlay.on_draw_model(|model| {
    // цвет выбирается по модели: команда игрока, а не локального игрока
    overlay.outline_of(|model| OutlineStyle::new(team_color(model)).width(2.0));
});
```

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
| `examples/skeleton` | Контур модели и подписи (имя, оружие) по команде игрока; команды `gc_status`, `unload_esp`. |
| `examples/auto-bhop` | Автопрыжок без потери скорости: инъекция колеса мыши в окно игры. |
| `examples/model-viewer` | Разбор и просмотр studio-моделей. |
| `examples/diagnostics` | Счётчики и проверки состояния движка. |

## Стенд: запуск игры с ботами

`tools/stand` поднимает игру, ставит ботов, инжектит DLL и ждёт отчёт в логе —
так примеры проверяются без ручной возни с меню:

```powershell
target\i686-pc-windows-msvc\release\golden-stand.exe run `
    target\i686-pc-windows-msvc\release\example_skeleton.dll `
    --map de_dust2 --bots 3 --seconds 120 --needle "скелетов" `
    --console "jointeam 2" --console "joinclass 1" --keep
```

Инъектор отдельно — `tools/inject`:

```powershell
target\i686-pc-windows-msvc\release\golden-inject.exe <pid hl.exe> <путь к dll>
```

## Документация по движку

`docs/STUDIO_REVERSE.md` — разбор studio-пути Gold Client: цепочка
`HUD_GetStudioModelInterface`, таблица `engine_studio_api_t`, layout объекта
`CStudioModelRenderer` и его vtable, адреса `Mod_*`/`R_Studio*`, формат `.mdl`,
рецепты хуков.

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