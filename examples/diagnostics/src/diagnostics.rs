//! Р”РёР°РіРЅРѕСЃС‚РёРєР°: РїСЂРѕРіРѕРЅ СЃР°РјРѕРїСЂРѕРІРµСЂРѕРє SDK Рё РІС‹РІРѕРґ РѕС‚С‡С‘С‚Р° РІ Р»РѕРі.
//!
//! Р­С‚Рѕ РїРµСЂРІР°СЏ В«С„РёС‡Р°В» РєСЂРµР№С‚Р° Рё РѕРґРЅРѕРІСЂРµРјРµРЅРЅРѕ РїСЂРѕРІРµСЂРєР°, С‡С‚Рѕ SDK РІРёРґРёС‚ РёРіСЂСѓ:
//! РµСЃР»Рё РѕС‚С‡С‘С‚ Р·РµР»С‘РЅС‹Р№, Р·РЅР°С‡РёС‚ СЃРјРµС‰РµРЅРёСЏ РІ [`golden_sdk::offsets`] Р°РєС‚СѓР°Р»СЊРЅС‹ РґР»СЏ
//! СЌС‚РѕР№ СЃР±РѕСЂРєРё Рё РЅР° РЅРёС… РјРѕР¶РЅРѕ СЃС‚СЂРѕРёС‚СЊ РѕСЃС‚Р°Р»СЊРЅРѕРµ.

use std::time::{Duration, Instant};

use golden_sdk::validate::{self, Check};

/// РћС‚С‡С‘С‚ РѕРґРЅРѕРіРѕ РїСЂРѕРіРѕРЅР°.
#[derive(Debug, Clone)]
pub struct Report {
    pub checks: Vec<Check>,
    pub elapsed: Duration,
}

impl Report {
    /// Р’СЃРµ РїСЂРѕРІРµСЂРєРё РїСЂРѕС€Р»Рё.
    pub fn is_ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    /// Р§РёСЃР»Рѕ СѓРїР°РІС€РёС… РїСЂРѕРІРµСЂРѕРє.
    pub fn failures(&self) -> usize {
        self.checks.iter().filter(|c| !c.ok).count()
    }

    /// РњРЅРѕРіРѕСЃС‚СЂРѕС‡РЅС‹Р№ С‚РµРєСЃС‚ РѕС‚С‡С‘С‚Р°.
    pub fn text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "РґРёР°РіРЅРѕСЃС‚РёРєР°: {}/{} РѕРє, {} СѓРїР°Р»Рѕ ({:?})\n",
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

/// РџСЂРѕРіРѕРЅСЏРµС‚ РІСЃРµ РїСЂРѕРІРµСЂРєРё SDK.
pub fn capture() -> Report {
    let start = Instant::now();
    let checks = validate::run();
    Report { checks, elapsed: start.elapsed() }
}

/// РџСЂРѕРіРѕРЅСЏРµС‚ РїСЂРѕРІРµСЂРєРё Рё РїРёС€РµС‚ РѕС‚С‡С‘С‚ РІ Р»РѕРі.
pub fn run() -> Report {
    let report = capture();
    for line in report.text().lines() {
        golden_sdk::log::line(line);
    }
    report
}

/// Р–РґС‘С‚, РїРѕРєР° РєР»РёРµРЅС‚ РёРЅРёС†РёР°Р»РёР·РёСЂСѓРµС‚ СЂРµРЅРґРµСЂРµСЂ, Рё РїСЂРѕРіРѕРЅСЏРµС‚ РїСЂРѕРІРµСЂРєРё.
///
/// РРјРµРµС‚ СЃРјС‹СЃР» РІС‹Р·С‹РІР°С‚СЊ РґРѕ Р·Р°РіСЂСѓР·РєРё РєР°СЂС‚С‹: С‡Р°СЃС‚СЊ СЃРјРµС‰РµРЅРёР№ (РЅР°РїСЂРёРјРµСЂ
/// `g_pStudioInterface`) РІР°Р»РёРґРЅР° С‚РѕР»СЊРєРѕ РїРѕСЃР»Рµ РёРЅРёС†РёР°Р»РёР·Р°С†РёРё РєР»РёРµРЅС‚Р°.
pub fn run_when_ready(timeout: Duration) -> Report {
    let deadline = Instant::now() + timeout;
    loop {
        if golden_sdk::renderer::StudioRenderer::global()
            .is_some_and(|renderer| renderer.is_initialized())
        {
            return run();
        }
        if Instant::now() >= deadline {
            golden_sdk::log::line("РґРёР°РіРЅРѕСЃС‚РёРєР°: СЂРµРЅРґРµСЂРµСЂ С‚Р°Рє Рё РЅРµ РёРЅРёС†РёР°Р»РёР·РёСЂРѕРІР°Р»СЃСЏ, РѕС‚С‡С‘С‚ РїРѕ С‚РµРєСѓС‰РµРјСѓ СЃРѕСЃС‚РѕСЏРЅРёСЋ");
            return run();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
