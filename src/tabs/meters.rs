//! [4] Meters — peak and RMS together, which is the pair that means something.
//!
//! The Lite screen shows peak, because peak is what tells you whether you are
//! about to clip. It does not tell you how *loud* something is: a peak-limited
//! master and a live take can share a peak and be twenty decibels apart in
//! perceived level. The difference between them — the crest factor — is the
//! number that separates the two, and it needs both meters to exist.

use crate::app::App;
use crate::chart;
use crate::fmt;
use crate::grid::{Canvas, Field};
use crate::layout::Layout;
use crate::meter::{self, History};
use crate::theme;

/// Below this, the signal has been squashed: a limiter, a codec, or a
/// compressor left on. Live material and music sit well above it.
pub const SQUASHED_CREST_DB: f32 = 6.0;

pub fn draw(c: &mut Canvas, l: &Layout, app: &App) {
    let f = l.content;
    let bottom = l.body_top + l.body_rows;
    // Two panels, sharing the body.
    let half = l.body_rows / 2;
    let mut y = l.body_top;

    for (title, base, live, peak_h, rms_h, dev) in [
        (
            "output",
            theme::green(),
            app.snap.caps.device_levels,
            &app.out_hist,
            &app.out_rms,
            app.snap.default_out.as_ref(),
        ),
        (
            "input",
            theme::cyan(),
            app.snap.caps.input_levels,
            &app.in_hist,
            &app.in_rms,
            app.snap.default_in.as_ref(),
        ),
    ] {
        if y >= bottom {
            return;
        }
        let name = dev.map(|d| d.name.as_str()).unwrap_or("no device");
        super::section(c, f, y, &format!("{title} \u{b7} {name}"));
        y += 1;
        y = panel(
            c,
            l,
            app,
            y,
            (y + half).min(bottom).saturating_sub(1),
            base,
            live,
            peak_h,
            rms_h,
        );
        y += 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn panel(
    c: &mut Canvas,
    l: &Layout,
    app: &App,
    top: u16,
    bottom: u16,
    base: Color,
    live: bool,
    peak_h: &History,
    rms_h: &History,
) -> u16 {
    let f = l.content;
    if bottom <= top {
        return bottom;
    }

    let floor = app.cfg.meter_floor;
    let peak = peak_h.current().filter(|_| live && peak_h.has_data());
    let rms = rms_h.current().filter(|_| live && rms_h.has_data());
    let hold = peak_h.peak().filter(|_| live && peak_h.has_data());

    // ── the numbers ─────────────────────────────────────────────────────────
    let na = || fmt::NA.to_string();
    super::tile(
        c,
        Field::at(f.x0, 12),
        top,
        "PEAK",
        &peak.map(fmt::dbfs).unwrap_or_else(na),
        theme::br_white(),
    );
    super::tile(
        c,
        Field::at(f.x0 + 13, 12),
        top,
        "RMS",
        &rms.map(fmt::dbfs).unwrap_or_else(na),
        theme::fg(),
    );

    // Crest factor: the whole reason RMS is here.
    let crest = match (peak, rms) {
        (Some(p), Some(r)) if p > floor && r > floor => Some(p - r),
        _ => None,
    };
    let crest_fg = match crest {
        Some(cf) if cf < SQUASHED_CREST_DB => theme::yellow(),
        Some(_) => theme::green(),
        None => theme::dim(),
    };
    super::tile(
        c,
        Field::at(f.x0 + 26, 12),
        top,
        "CREST",
        &crest.map(|v| format!("{v:.1} dB")).unwrap_or_else(na),
        crest_fg,
    );
    super::tile(
        c,
        Field::at(f.x0 + 39, 14),
        top,
        "PEAK HOLD",
        &hold.map(fmt::dbfs).unwrap_or_else(na),
        theme::dim(),
    );
    if let Some(cf) = crest
        && cf < SQUASHED_CREST_DB
    {
        c.left(
            Field::new(f.x0 + 55, f.x1),
            top + 1,
            "squashed \u{2014} a limiter, a codec, or a compressor left on",
            theme::yellow(),
        );
    }

    // ── the chart ───────────────────────────────────────────────────────────
    let chart_top = top + 3;
    if chart_top >= bottom {
        return bottom;
    }
    let rows = bottom - chart_top;
    if !live || !peak_h.has_data() {
        for x in f.x0..=f.x1 {
            c.set(x, bottom - 1, c.g.blocks[0], theme::faint());
        }
        c.left(f, chart_top, "not metered on this path", theme::faint());
        return bottom;
    }

    let sub = app.sub_columns();
    let cols = f.width();
    // RMS is the filled bar and peak is a cap above it — the conventional
    // arrangement, and the one where the gap between the two *is* the crest
    // factor, drawn to scale. Peak as the fill with RMS inside it puts a dotted
    // line in the middle of a solid block, which is unreadable.
    let rms_cols = rms_h.columns(cols as usize * sub);
    let norm: Vec<f32> = rms_cols.iter().map(|d| meter::norm(*d, floor)).collect();
    let grid = chart::bars(&norm, cols, rows, sub == 2, &c.g.blocks);
    for (gy, row) in grid.iter().enumerate() {
        for (gx, cell) in row.iter().enumerate() {
            let Some(ch) = cell else { continue };
            let d = rms_cols[gx * sub..(gx * sub + sub).min(rms_cols.len())]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);
            let height = theme::row_height(gy as u16, rows);
            let fg = if app.paused {
                theme::faint()
            } else {
                theme::chart_cell(app.theme(), base, d, height)
            };
            c.set(f.x0 + gx as u16, bottom - 1 - gy as u16, *ch, fg);
        }
    }

    let peaks = peak_h.columns(cols as usize);
    for (i, d) in peaks.iter().enumerate() {
        let h = (meter::norm(*d, floor) * rows as f32).round() as u16;
        if h > 0 && h <= rows {
            // Red at the top of the scale whatever the theme: a peak cap is
            // the one mark on this screen that means "you are about to clip".
            let fg = if *d >= -0.1 { theme::red() } else { theme::br_white() };
            c.set(f.x0 + i as u16, bottom - h, c.g.rms, fg);
        }
    }
    bottom
}

use ratatui::style::Color;
