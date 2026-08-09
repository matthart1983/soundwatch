//! [7] Xruns — the dropout log, with when and which device.
//!
//! A count says the machine dropped audio. A log says *which interface* and
//! *when*, which is the difference between knowing you have a problem and
//! knowing where it is. The distribution over the last minute matters too: five
//! xruns spread over a minute is a machine under load, five in one second is
//! something that happened.

use crate::app::App;
use crate::grid::{Canvas, Field};
use crate::layout::Layout;
use crate::model::XrunEvent;
use crate::theme;

pub fn draw(c: &mut Canvas, l: &Layout, app: &App) {
    let f = l.content;
    let mut y = l.body_top;

    // ── the headline ────────────────────────────────────────────────────────
    let total: u32 = app.snap.xrun_log.iter().map(|e| e.count).sum();
    let fg = if app.snap.xruns_60s >= app.cfg.xrun_alert { theme::RED } else { theme::GREEN };
    super::tile(c, Field::at(f.x0, 14), y, "LAST 60s", &app.snap.xruns_60s.to_string(), fg);
    super::tile(c, Field::at(f.x0 + 16, 14), y, "SESSION", &total.to_string(), theme::FG);
    let devices: std::collections::BTreeSet<&str> =
        app.snap.xrun_log.iter().map(|e| e.device.as_str()).collect();
    super::tile(
        c,
        Field::at(f.x0 + 32, 24),
        y,
        "DEVICES AFFECTED",
        &if devices.is_empty() { "--".into() } else { devices.len().to_string() },
        theme::FG,
    );
    super::tile(
        c,
        Field::new(f.x0 + 58, f.x1),
        y,
        "ALERTS AT",
        &format!("{} in 60s", app.cfg.xrun_alert),
        theme::DIM,
    );
    y += 3;

    if !app.snap.caps.xruns {
        super::empty(c, f, y, "this backend does not report dropouts");
        return;
    }

    // ── the log ─────────────────────────────────────────────────────────────
    super::section(c, f, y, "dropouts");
    y += 1;

    if app.snap.xrun_log.is_empty() {
        super::empty(
            c,
            f,
            y,
            "none this session \u{2014} the overload listener is attached and quiet",
        );
        return;
    }

    let f_when = Field::at(f.x0, 10);
    let f_count = Field::at(f_when.x1 + 2, 6);
    let f_dev = Field::new(f_count.x1 + 2, f.x1);
    super::table_head(
        c,
        l,
        y,
        &[(f_when, "AGO", true), (f_count, "COUNT", true), (f_dev, "DEVICE", false)],
    );
    y += 2;

    let rows = (l.body_top + l.body_rows).saturating_sub(y) as usize;
    // Newest first: the one you are chasing is the one that just happened.
    for (i, e) in app.snap.xrun_log.iter().rev().skip(app.scroll).take(rows).enumerate() {
        let row = y + i as u16;
        c.right(f_when, row, &ago(app, e), theme::DIM);
        let fg = if e.count >= app.cfg.xrun_alert { theme::RED } else { theme::YELLOW };
        c.right(f_count, row, &e.count.to_string(), fg);
        c.left(f_dev, row, &e.device, theme::FG);
    }
}

fn ago(app: &App, e: &XrunEvent) -> String {
    let secs = app.snap.at.secs_since(e.at);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}
