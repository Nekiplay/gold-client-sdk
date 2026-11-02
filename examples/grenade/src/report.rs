//! Отчёт в консоль Gold Client и в лог.
//!
//! Печатается по команде (`grenade_he` / `grenade_flash`), а не каждый кадр:
//! живая отрисовка обновляется молча.

use golden_sdk::grenade::Kind;
use golden_std::Game;
use golden_std::console;
use golden_std::console::Color;
use golden_std::grenade::Prediction;

/// Печатает полный отчёт: траектория, точка взрыва и последствия.
pub fn print(prediction: &Prediction) {
    let title = format!("граната {} — {}", prediction.kind.name(), prediction.map);
    let mut lines = vec![title.clone()];
    lines.push(format!(
        "точка взрыва: {:.0} {:.0} {:.0}",
        prediction.flight.explosion.x, prediction.flight.explosion.y, prediction.flight.explosion.z
    ));
    lines.push(format!(
        "полёт: {:.2} с, отскоков {}{}",
        prediction.flight.time,
        prediction.flight.bounces,
        if prediction.flight.stopped {
            ", остановилась"
        } else {
            ""
        }
    ));
    lines.push(format!("учтено игроков: {}", prediction.players));

    let mut builder = console::builder().title(&title);
    for line in &lines[1..] {
        builder = builder.line(line);
    }

    let enemies = |index: i32| enemy(prediction, index);

    match prediction.kind {
        Kind::He => {
            let rows = prediction.named_damage();
            if rows.is_empty() {
                builder = builder.warn("  урон: никого не заденет");
            } else {
                builder = builder.blank().line("  урон (до брони):");
                for row in &rows {
                    let lethal = row.health > 0 && row.value.damage >= row.health as f32;
                    let health = if row.health > 0 {
                            format!("hp {}", row.health)
                        } else {
                            "hp ?".to_string()
                        };
                    let text = format!(
                        "  {} — -{:.0} ({}){}{}",
                        row.name,
                        row.value.damage,
                        health,
                        if lethal { ", смертельно" } else { "" },
                        relation(prediction, row.index)
                    );
                    builder = builder
                        .colored(if lethal { Color::RED } else { Color::YELLOW }, &text)
                        .text("\n");
                }
            }
            builder = builder.line("  броня, вода и hegrenade_penetration не учтены");
            if prediction.alive > prediction.players {
                builder = builder.line(format!(
                    "  учтено {} из {} живых: остальные вне вида",
                    prediction.players, prediction.alive
                ));
            }
            builder.send();

            golden_sdk::log::line(&format!("grenade: {} (урон {})", title, rows.len()));
            for row in &rows {
                let health = if row.health > 0 {
                    format!("hp {}", row.health)
                } else {
                    "hp ?".to_string()
                };
                golden_sdk::log::line(&format!(
                    "grenade:   урон {} -{:.0} ({})",
                    row.name, row.value.damage, health
                ));
            }
        }
        Kind::Flash => {
            let rows = prediction.named_flashes();
            if rows.is_empty() {
                builder = builder.warn("  ослепит: никого — стена или дальше 1500 единиц");
            } else {
                builder = builder
                    .blank()
                    .line(format!("  ослепит игроков: {}", rows.len()));
                for row in &rows {
                    let text = format!(
                        "  {} — {:.1} с ({}){}",
                        row.name,
                        row.value.duration,
                        if row.value.behind {
                            "взрыв за спиной"
                        } else {
                            "смотрел на взрыв"
                        },
                        relation(prediction, row.index)
                    );
                    builder = builder.colored(Color::CYAN, &text).text("\n");
                }
            }

            // Кого именно накрывает: отдельный счёт по врагам, своим и неизвестным
            // (наблюдатель/первые кадры после подключения).
            let enemy_rows = rows.iter().filter(|row| enemies(row.index)).count();
            let friend_rows = rows
                .iter()
                .filter(|row| teammate(prediction, row.index))
                .count();
            builder = builder.line(format!(
                "  из них врагов: {}, своих: {}, неизвестно: {}",
                enemy_rows,
                friend_rows,
                rows.len() - enemy_rows - friend_rows
            ));
            if prediction.alive > prediction.players {
                builder = builder.line(format!(
                    "  учтено {} из {} живых: остальные вне вида, клиент не знает их позу",
                    prediction.players, prediction.alive
                ));
            }
            let skipped: Vec<String> = prediction
                .named_checks()
                .into_iter()
                .filter(|row| row.value.flash.is_none())
                .map(|row| {
                    format!(
                        "{} ({})",
                        row.name,
                        if row.value.too_far {
                            "дальше 1500"
                        } else if row.value.blocked {
                            "за стеной"
                        } else {
                            "вне радиуса"
                        }
                    )
                })
                .collect();
            if !skipped.is_empty() {
                builder = builder.line(format!("  не заденет: {}", skipped.join(", ")));
            }
            builder = builder.line("  без брони, воды и teamflash; длительность — fade * 0.33");
            builder.send();

            golden_sdk::log::line(&format!(
                "grenade: {} (флешка ослепит {} игроков)",
                title,
                rows.len()
            ));
            for row in &rows {
                golden_sdk::log::line(&format!(
                    "grenade:   ослепление {} {:.1} с{}",
                    row.name,
                    row.value.duration,
                    if row.value.behind {
                        " (взрыв за спиной)"
                    } else {
                        ""
                    }
                ));
            }
            for row in prediction.named_checks() {
                if row.value.flash.is_none() {
                    golden_sdk::log::line(&format!(
                        "grenade:   не заденет {} — dist {:.0}, {}",
                        row.name,
                        row.value.distance,
                        if row.value.too_far {
                            "дальше 1500"
                        } else if row.value.blocked {
                            "за стеной"
                        } else {
                            "вне радиуса"
                        }
                    ));
                }
            }
        }
    }

    // Стенд и `golden-client.log` читают лог, а не консоль: точка взрыва и
    // параметры полёта дублируются для них отдельными строками.
    for line in &lines[1..] {
        golden_sdk::log::line(&format!("grenade: {line}"));
    }
}

/// Ошибка расчёта: в консоль и лог.
pub fn error(text: &str) {
    console::builder().error(format!("граната: {text}")).send();
    golden_sdk::log!("grenade: расчёт не удался: {text}");
}

/// Игрок из той же команды, что и локальный.
fn teammate(prediction: &Prediction, index: i32) -> bool {
    let team = |slot: i32| {
        Game::get()
            .and_then(|game| game.get_player(slot))
            .and_then(|player| player.team())
    };
    let local = team(prediction.local);
    local.is_some() && local == team(index)
}

/// Игрок из чужой команды (обе команды известны).
fn enemy(prediction: &Prediction, index: i32) -> bool {
    let team = |slot: i32| {
        Game::get()
            .and_then(|game| game.get_player(slot))
            .and_then(|player| player.team())
    };
    let local = team(prediction.local);
    let Some(local) = local else {
        return false;
    };
    team(index).is_some_and(|team| team != local)
}

/// Пометка в строке отчёта: свой, враг или ничего.
pub(crate) fn relation(prediction: &Prediction, index: i32) -> &'static str {
    if teammate(prediction, index) {
        ", свой"
    } else if enemy(prediction, index) {
        ", враг"
    } else {
        ""
    }
}
