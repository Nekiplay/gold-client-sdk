//! Диагностика: прогон самопроверок SDK и вывод отчёта в лог.
//!
//! Это первая «фича» крейта и одновременно проверка, что SDK видит игру:
//! если отчёт зелёный, значит смещения в [`golden_sdk::offsets`] актуальны для
//! этой сборки и на них можно строить остальное.

use std::time::{Duration, Instant};

use golden_sdk::validate::{self, Check};

/// Отчёт одного прогона.
#[derive(Debug, Clone)]
pub struct Report {
    pub checks: Vec<Check>,
    pub elapsed: Duration,
}

impl Report {
    /// Все проверки прошли.
    pub fn is_ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    /// Число упавших проверок.
    pub fn failures(&self) -> usize {
        self.checks.iter().filter(|c| !c.ok).count()
    }

    /// Многострочный текст отчёта.
    pub fn text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "диагностика: {}/{} ок, {} упало ({:?})\n",
            self.checks.len().saturating_sub(self.failures()),
            self.checks.len(),
            self.failures(),
            self.elapsed
        ));
        for check in &self.checks {
            out.push_str(&format!(
                "  [{}] {:<48} {}\n",
                if check.ok { "ok" } else { "FAIL" },
                check.name,
                check.detail
            ));
        }
        out
    }
}

/// Прогоняет все проверки SDK.
pub fn capture() -> Report {
    let start = Instant::now();
    let checks = validate::run();
    Report { checks, elapsed: start.elapsed() }
}

/// Прогоняет проверки и пишет отчёт в лог.
pub fn run() -> Report {
    let report = capture();
    for line in report.text().lines() {
        golden_sdk::log::line(line);
    }
    report
}

/// Ждёт, пока клиент инициализирует рендерер, и прогоняет проверки.
///
/// Имеет смысл вызывать до загрузки карты: часть смещений (например
/// `g_pStudioInterface`) валидна только после инициализации клиента.
pub fn run_when_ready(timeout: Duration) -> Report {
    let deadline = Instant::now() + timeout;
    loop {
        if golden_sdk::renderer::StudioRenderer::global()
            .is_some_and(|renderer| renderer.is_initialized())
        {
            return run();
        }
        if Instant::now() >= deadline {
            golden_sdk::log::line("диагностика: рендерер так и не инициализировался, отчёт по текущему состоянию");
            return run();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}