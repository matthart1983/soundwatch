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
        if h == 0 || h > rows {
            continue;
        }

        // Ask the grid what it actually painted rather than recomputing the
        // bar's height a second way.
        let bar_top = (0..rows).rev().find(|&gy| grid[gy as usize][i].is_some());
        let Some(gy) = cap_row(h, rows, bar_top) else { continue };

        // Red at the top of the scale whatever the theme: a peak cap is
        // the one mark on this screen that means "you are about to clip".
        let fg = if *d >= -0.1 { theme::red() } else { theme::br_white() };
        c.set(f.x0 + i as u16, bottom - 1 - gy, c.g.rms, fg);
    }
    bottom
}

/// Which row the peak cap goes on, in bottom-up cell coordinates, or `None`
/// if it cannot be drawn without damaging the bar.
///
/// `h` is the cap's height in whole cells and `bar_top` is the topmost cell
/// the RMS bar actually painted.
///
/// The cap belongs *above* the bar, never inside it. The bar is drawn in
/// eighths of a cell while the cap is placed to the nearest whole cell, so a
/// small crest factor rounds both into the same cell — and the caller would
/// then `set` the cap glyph straight through a solid block, perforating every
/// bar on screen. That was the bug this exists to make untestable-by-accident.
fn cap_row(h: u16, rows: u16, bar_top: Option<u16>) -> Option<u16> {
    if h == 0 || h > rows {
        return None;
    }
    let gy = h - 1;
    let Some(top) = bar_top else { return Some(gy) };
    if gy > top {
        return Some(gy);
    }
    // A full-scale bar leaves nowhere above it to sit. The bar already reads
    // as "at the top"; punching a hole in it to say so again tells you less
    // than the solid bar already does.
    if top + 1 >= rows {
        return None;
    }
    Some(top + 1)
}

use ratatui::style::Color;

#[cfg(test)]
mod cap_tests {
    use super::cap_row;

    #[test]
    fn a_cap_clear_of_the_bar_stays_where_it_lands() {
        // Bar tops out at cell 1, cap wants cell 3: nothing to resolve.
        assert_eq!(cap_row(4, 8, Some(1)), Some(3));
    }

    #[test]
    fn a_cap_landing_inside_the_bar_is_lifted_clear_of_it() {
        // This is the perforation. Peak and RMS round into the same cell, so
        // the cap wants a cell the bar has already painted; drawing it there
        // punches the cap glyph through a solid block.
        for (h, top) in [(1u16, 0u16), (2, 1), (3, 3), (4, 5)] {
            let got = cap_row(h, 8, Some(top)).expect("a cap should still be drawn");
            assert!(
                got > top,
                "cap at h={h} over a bar topping at {top} landed on {got}, inside the bar"
            );
            assert_eq!(got, top + 1, "the cap should sit directly above the bar, not float");
        }
    }

    #[test]
    fn a_full_scale_bar_drops_the_cap_rather_than_holing_itself() {
        // Nowhere above the bar to put it. A hole would say less than the
        // solid full-height bar already does.
        assert_eq!(cap_row(8, 8, Some(7)), None);
        assert_eq!(cap_row(4, 8, Some(7)), None);
    }

    #[test]
    fn an_unpainted_column_keeps_the_cap_where_it_lands() {
        assert_eq!(cap_row(5, 8, None), Some(4));
    }

    #[test]
    fn out_of_range_heights_draw_nothing() {
        assert_eq!(cap_row(0, 8, Some(0)), None, "a silent column has no cap");
        assert_eq!(cap_row(9, 8, Some(0)), None, "a cap past the top is not drawn");
    }
}
