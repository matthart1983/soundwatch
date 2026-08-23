//! [1] Overview — the one screen that answers "is anything wrong".
//!
//! The family's Overview leads with KPI tiles because the first question is
//! never "what is the buffer size", it is "do I need to care right now". Then
//! the two meters, then the streams, then whatever Insights has to say — so
//! that reading top to bottom goes from verdict to evidence.

use crate::app::App;
use crate::fmt;
use crate::grid::{Canvas, Field};
use crate::layout::Layout;
use crate::theme;

pub fn draw(c: &mut Canvas, l: &Layout, app: &App) {
    let f = l.content;
    let bottom = l.body_top + l.body_rows;
    let mut y = l.body_top;

    // ── KPI tiles ───────────────────────────────────────────────────────────
    let snap = &app.snap;
    let out = snap.default_out.as_ref();
    let na = || fmt::NA.to_string();

    let level = app
        .out_hist
        .current()
        .filter(|_| snap.caps.device_levels && app.out_hist.has_data())
        .map(fmt::dbfs)
        .unwrap_or_else(na);
    let level_fg = match app.out_hist.current().filter(|_| snap.caps.device_levels) {
        Some(d) => theme::level(d, theme::green()),
        None => theme::dim(),
    };
    let xrun_fg = if snap.xruns_60s >= app.cfg.xrun_alert { theme::red() } else { theme::fg() };
    let lat = snap.latency_ms().map(|v| fmt::ms(v, 9)).unwrap_or_else(na);

    let tiles: [(&str, String, ratatui::style::Color); 5] = [
        ("OUTPUT", level, level_fg),
        (
            "RATE",
            out.map(|d| fmt::rate_bits(d.format.rate, d.format.bits)).unwrap_or_else(na),
            theme::fg(),
        ),
        ("BUFFER", out.map(|d| fmt::frames(d.buffer_frames)).unwrap_or_else(na), theme::fg()),
        ("LATENCY", lat, theme::fg()),
        ("XRUNS 60s", snap.xruns_60s.to_string(), xrun_fg),
    ];
    let tile_w = (f.width() / tiles.len() as u16).max(10);
    for (i, (caption, value, fg)) in tiles.iter().enumerate() {
        let x = f.x0 + i as u16 * tile_w;
        super::tile(c, Field::at(x, tile_w - 1), y, caption, value, *fg);
    }
    y += 3;

    // ── the meters ──────────────────────────────────────────────────────────
    if y >= bottom {
        return;
    }
    super::section(c, f, y, "levels \u{b7} 60s");
    y += 1;
    // Capped low: the meters are the headline but the stream list under
    // them is why anyone scrolled down, and it must not be pushed off.
    let meter_rows = ((bottom - y) / 5).clamp(1, 4);
    y = crate::ui::mini_meter(c, l, app, y, meter_rows, false);
    y = crate::ui::mini_meter(c, l, app, y, meter_rows.max(1), true);
    y += 1;

    // ── what is playing ─────────────────────────────────────────────────────
    if y + 2 >= bottom {
        return;
    }
    super::section(c, f, y, &format!("streams \u{b7} {}", super::devices::summary(app)));
    y += 1;

    let visible = app.visible();
    if visible.is_empty() {
        super::empty(c, f, y, "no audio streams active");
        return;
    }
    let f_app = Field::at(f.x0, 18);
    let f_dev = Field::at(f_app.x1 + 2, (f.width() / 3).max(16));
    let f_rate = Field::at(f_dev.x1 + 2, 9);
    let f_lat = Field::new(f_rate.x1 + 2, (f_rate.x1 + 10).min(f.x1));

    let room = bottom.saturating_sub(y) as usize;
    // Leave the last two rows for the insights strip when there is anything in
    // it — a warning under the fold is a warning nobody read.
    let insights = super::insights::collect(app);
    let strip = if insights.is_empty() { 0 } else { 2 };
    let rows = room.saturating_sub(strip);

    for (i, s) in visible.iter().take(rows).enumerate() {
        let row = y + i as u16;
        let is_in = s.direction.is_input();
        c.left(f_app, row, &s.app, theme::fg());
        if is_in {
            c.text(f_dev, f_dev.x0, row, c.g.dot, theme::cyan(), false);
            c.left(Field::new(f_dev.x0 + 2, f_dev.x1), row, &s.device, theme::dim());
        } else {
            c.left(f_dev, row, &s.device, theme::dim());
        }
        let rate = if s.format.rate == 0 {
            fmt::NA.to_string()
        } else {
            fmt::rate_bits(s.format.rate, s.format.bits)
        };
        c.right(f_rate, row, &rate, theme::dim());
        c.right(f_lat, row, &s.latency_ms.map(|v| fmt::ms(v, 8)).unwrap_or_else(na), theme::dim());
    }

    // ── insights strip ──────────────────────────────────────────────────────
    if let Some(top) = insights.first() {
        let row = bottom - 1;
        c.rule(f, row - 1, theme::faint());
        let x = c.text(f, f.x0, row, top.severity.badge(), top.severity.colour(), true);
        let x = c.text(f, x + 2, row, &top.title, theme::fg(), false);
        let more = insights.len().saturating_sub(1);
        let tail =
            if more > 0 { format!("  [0] Insights (+{more})") } else { "  [0] Insights".into() };
        c.text(f, x, row, &tail, theme::cyan(), false);
    }
}
