//! The 80x24 screen.
//!
//! Row and column numbers here are the spec's, verbatim. Everything is drawn
//! through [`Field`]-bounded writes, so no value can reach outside its region
//! however long the underlying string turns out to be.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::app::{App, Mode};
use crate::fmt;
use crate::grid::{COLS, CONTENT, Canvas, Field, ROWS, width};
use crate::meter;
use crate::model::{Stream, Verdict};
use crate::theme;

pub const ROW_HEADER: u16 = 0;
pub const ROW_OUT_LABEL: u16 = 2;
pub const ROW_OUT_METER: u16 = 3;
pub const OUT_METER_H: u16 = 3;
pub const ROW_IN_LABEL: u16 = 6;
pub const ROW_IN_METER: u16 = 7;
pub const IN_METER_H: u16 = 2;
pub const ROW_AXIS: u16 = 9;
pub const ROW_VITALS: u16 = 10;
/// Blank in the spec. Carries the single degradation line when one is needed —
/// `TECHNICAL.md` requires "`--` plus a one-line explanation" and never says
/// where the line goes.
pub const ROW_NOTE: u16 = 11;
pub const ROW_TABLE_HEAD: u16 = 12;
pub const ROW_TABLE_RULE: u16 = 13;
pub const ROW_LIST: u16 = 14;
pub const ROW_DETAIL: u16 = 19;
pub const ROW_PROMPT: u16 = 22;
pub const ROW_FOOTER: u16 = 23;

/// Table columns — identical across all four Lites: 1 / 17 / 40 / 51 / 62 / 70.
pub const F_APP: Field = Field::at(1, 15);
pub const F_DEVICE: Field = Field::at(17, 22);
/// Input rows spend two columns of DEVICE on the mic-in-use dot.
pub const F_DEVICE_IN: Field = Field::at(19, 20);
pub const F_LEVEL: Field = Field::at(40, 10);
pub const F_RATE: Field = Field::at(51, 10);
pub const F_LAT: Field = Field::at(62, 7);
pub const F_SPARK: Field = Field::at(70, 9);

pub const VERSION: &str = concat!("soundwatch-lite ", env!("CARGO_PKG_VERSION"));

pub fn render(app: &App, buf: &mut Buffer, area: Rect) {
    if area.width < COLS || area.height < ROWS {
        too_small(buf, area);
        return;
    }
    let ox = area.x + (area.width - COLS) / 2;
    let oy = area.y + (area.height - ROWS) / 2;
    let mut c = Canvas::new(buf, ox, oy, app.glyphs);

    c.clear(theme::BG);
    header(&mut c, app);
    meters(&mut c, app);
    axis(&mut c, app);
    vitals(&mut c, app);
    note(&mut c, app);
    table(&mut c, app);
    if app.mode == Mode::Detail {
        detail(&mut c, app);
    }
    prompt(&mut c, app);
    footer(&mut c, app);
    if app.mode == Mode::Help {
        help(&mut c, app);
    }
}

fn too_small(buf: &mut Buffer, area: Rect) {
    let msg = format!("soundwatch-lite needs {COLS}x{ROWS} (have {}x{})", area.width, area.height);
    let x = area.x + area.width.saturating_sub(width(&msg)) / 2;
    let y = area.y + area.height / 2;
    for (i, ch) in msg.chars().enumerate() {
        if let Some(cell) = buf.cell_mut((x + i as u16, y)) {
            cell.set_char(ch);
            cell.set_fg(theme::DIM);
        }
    }
}

// ── row 0 ────────────────────────────────────────────────────────────────────

fn header(c: &mut Canvas, app: &App) {
    let (right, right_fg, dot) = if app.paused {
        (format!("{} PAUSED", c.g.paused), theme::YELLOW, None)
    } else if let Verdict::Alert { headline, .. } = &app.verdict {
        (headline.clone(), theme::RED, Some(theme::RED))
    } else {
        let d = app.snap.default_out.as_ref();
        let f = d.map(|d| fmt::rate_bits(d.format.rate, d.format.bits)).unwrap_or(fmt::NA.into());
        let b = d.map(|d| fmt::frames(d.buffer_frames)).unwrap_or(fmt::NA.into());
        (format!("{f} \u{b7} {b}"), theme::DIM, Some(theme::GREEN))
    };

    // Reserve the right group first, then fit the left into what remains.
    let dot_w = if dot.is_some() { width(c.g.dot) + 1 } else { 0 };
    let right_w = width(&right) + dot_w;
    let right_x = 78u16.saturating_sub(right_w).saturating_add(1);
    let left = CONTENT.ending_before(right_x, 2);

    if let Some(dc) = dot {
        c.text(CONTENT, right_x, ROW_HEADER, c.g.dot, dc, false);
        c.text(CONTENT, right_x + dot_w, ROW_HEADER, &right, right_fg, false);
    } else {
        c.right_bold(Field::new(right_x, 78), ROW_HEADER, &right, right_fg);
    }

    let name = "soundwatch";
    let backend = format!(" \u{b7} {}", app.snap.backend);
    let mut x = c.text(left, left.x0, ROW_HEADER, name, theme::FG, true);
    // The hostname is the elastic element: a studio box reached over SSH can
    // easily carry a 40-character FQDN, and the spec bounds neither it nor the
    // backend name.
    let host_room = left.width().saturating_sub(width(name) + 2 + width(&backend));
    let host = format!("  {}", crate::grid::truncate(&app.snap.host, host_room));
    x = c.text(left, x, ROW_HEADER, &host, theme::CYAN, false);
    c.text(left, x, ROW_HEADER, &backend, theme::DIM, false);
}

// ── rows 2-8 ─────────────────────────────────────────────────────────────────

fn meters(c: &mut Canvas, app: &App) {
    let out_dev = app.snap.default_out.as_ref();
    let in_dev = app.snap.default_in.as_ref();
    // The backend declares whether levels are readable; the UI never guesses.
    // Output and input are declared separately: on macOS they come from
    // different APIs with different consent, and one commonly works alone.
    let out_live = app.snap.caps.device_levels;

    meter_label(
        c,
        app,
        ROW_OUT_LABEL,
        c.g.out,
        theme::GREEN,
        "dBFS out",
        &app.out_hist,
        out_live,
        out_dev.map(|d| fmt::rate_bits(d.format.rate, d.format.bits)),
        out_dev.map(|d| d.name.clone()),
    );
    meter_body(c, app, ROW_OUT_METER, OUT_METER_H, &app.out_hist, theme::GREEN, out_live);

    let has_input = in_dev.is_some();
    let in_live = app.snap.caps.input_levels && has_input;
    meter_label(
        c,
        app,
        ROW_IN_LABEL,
        c.g.inp,
        theme::CYAN,
        if has_input { "dBFS in" } else { "no input" },
        &app.in_hist,
        in_live,
        None,
        in_dev.map(|d| d.name.clone()),
    );
    meter_body(c, app, ROW_IN_METER, IN_METER_H, &app.in_hist, theme::CYAN, in_live);
}

#[allow(clippy::too_many_arguments)]
fn meter_label(
    c: &mut Canvas,
    app: &App,
    row: u16,
    glyph: &str,
    base: Color,
    unit: &str,
    hist: &meter::History,
    live: bool,
    fmt_str: Option<String>,
    device: Option<String>,
) {
    let arrow_fg = if app.paused { theme::FAINT } else { base };
    c.text(CONTENT, 1, row, glyph, arrow_fg, true);

    // Current level, right-aligned in cols 3..=7 so "-8.2" and "-31.6" agree.
    let cur = hist.current().filter(|_| live && hist.has_data());
    let level_fg = if app.paused { theme::FAINT } else { theme::BR_WHITE };
    match cur {
        Some(d) => {
            c.right_bold(Field::at(3, 5), row, &fmt::dbfs(d), level_fg);
        }
        None => {
            c.right(Field::at(3, 5), row, fmt::NA, theme::DIM);
        }
    }
    let unit_end = c.text(CONTENT, 9, row, unit, theme::DIM, false);

    // Right group: peak, format, device. The device name is the elastic part and
    // absorbs the truncation — real PulseAudio and CoreAudio descriptions run to
    // 40+ characters and the spec caps neither.
    let peak = match hist.peak().filter(|_| live && hist.has_data()) {
        Some(p) => format!("peak {}", fmt::dbfs(p)),
        None => format!("peak {}", fmt::NA),
    };
    let mut fixed = peak;
    if let Some(f) = fmt_str {
        fixed.push_str("  ");
        fixed.push_str(&f);
    }
    let right = Field::new(unit_end + 2, 78);
    let text = match device {
        Some(dev) => {
            let room = right.width().saturating_sub(width(&fixed) + 2);
            format!("{fixed}  {}", crate::grid::truncate(&dev, room))
        }
        None => fixed,
    };
    c.right(right, row, &text, theme::DIM);
}

fn meter_body(
    c: &mut Canvas,
    app: &App,
    top: u16,
    h: u16,
    hist: &meter::History,
    base: Color,
    live: bool,
) {
    let blocks = c.g.blocks;
    if !live || !hist.has_data() {
        // Degraded: a flat faint baseline. Distinct from silence (which draws a
        // baseline in the direction colour) and from an unpainted region.
        for x in CONTENT.x0..=CONTENT.x1 {
            c.set(x, top + h - 1, blocks[0], theme::FAINT);
        }
        return;
    }
    let series = hist.series();
    for (i, &d) in series.iter().enumerate().take(meter::HISTORY_COLS) {
        let x = CONTENT.x0 + i as u16;
        let fg = if app.paused { theme::FAINT } else { theme::level(d, base) };
        for (j, cell) in meter::column(d, h, &blocks).into_iter().enumerate() {
            if let Some(ch) = cell {
                c.set(x, top + h - 1 - j as u16, ch, fg);
            }
        }
    }
}

// ── row 9 ────────────────────────────────────────────────────────────────────

fn axis(c: &mut Canvas, _app: &App) {
    c.rule(CONTENT, ROW_AXIS, theme::FAINT);
    c.text(CONTENT, 1, ROW_AXIS, " 60s ago ", theme::DIM, false);
    c.right(CONTENT, ROW_AXIS, " now ", theme::DIM);
}

// ── row 10 ───────────────────────────────────────────────────────────────────

fn vitals(c: &mut Canvas, app: &App) {
    let snap = &app.snap;
    let alert = app.verdict.is_alert();

    let verdict_text = match &app.verdict {
        Verdict::Alert { reason, .. } => reason.clone(),
        Verdict::Nominal => "all nominal".into(),
    };
    let verdict_x = 78u16.saturating_sub(width(&verdict_text)).saturating_add(1);
    c.right(
        Field::new(verdict_x, 78),
        ROW_VITALS,
        &verdict_text,
        if alert { theme::RED } else { theme::FAINT },
    );

    let out = snap.default_out.as_ref();
    let lat = snap.latency_ms().map(|v| fmt::ms(v, 8)).unwrap_or(fmt::NA.into());
    let mut pairs: Vec<(&str, String, bool)> = vec![
        ("rate", out.map(|d| fmt::rate(d.format.rate)).unwrap_or(fmt::NA.into()), false),
        (
            "buffer",
            out.map(|d| fmt::frames(d.buffer_frames)).unwrap_or(fmt::NA.into()),
            alert
                && matches!(&app.verdict, Verdict::Alert { reason, .. } if reason.contains("buffer")),
        ),
        ("latency", lat, false),
        (
            "xruns",
            if snap.caps.xruns { snap.xruns_60s.to_string() } else { fmt::NA.into() },
            alert && snap.xruns_60s > 0,
        ),
    ];

    // The vitals row and the verdict share one line with no gutter guaranteed by
    // the spec: at `xruns 14` the mockup's own values leave exactly zero columns
    // between them, and `xruns 147` or `rate 44.1k` would overlap. Shed the
    // least load-bearing pair first — rate is already in the header.
    let field = CONTENT.ending_before(verdict_x, 2);
    let measure = |ps: &[(&str, String, bool)]| -> u16 {
        ps.iter().map(|(l, v, _)| width(l) + 1 + width(v)).sum::<u16>()
            + 3 * ps.len().saturating_sub(1) as u16
    };
    while pairs.len() > 1 && measure(&pairs) > field.width() {
        pairs.remove(0);
    }

    let mut x = field.x0;
    for (i, (label, value, bad)) in pairs.iter().enumerate() {
        if i > 0 {
            x += 3;
        }
        x = c.text(field, x, ROW_VITALS, label, theme::DIM, false);
        x = c.text(
            field,
            x + 1,
            ROW_VITALS,
            value,
            if *bad { theme::RED } else { theme::FG },
            false,
        );
    }
}

// ── row 11 ───────────────────────────────────────────────────────────────────

fn note(c: &mut Canvas, app: &App) {
    let mut msg = app.snap.caps.note.clone();
    if app.snap.latency_is_one_way() && msg.is_none() {
        msg = Some("latency is the output path only: nothing is capturing".into());
    }
    if let Some(m) = msg {
        c.left(CONTENT, ROW_NOTE, &m, theme::FAINT);
    }
}

// ── rows 12-21 ───────────────────────────────────────────────────────────────

fn table(c: &mut Canvas, app: &App) {
    c.left(F_APP, ROW_TABLE_HEAD, "APP", theme::DIM);
    c.left(F_DEVICE, ROW_TABLE_HEAD, "DEVICE", theme::DIM);
    c.right(F_LEVEL, ROW_TABLE_HEAD, "LEVEL", theme::DIM);
    c.right(F_RATE, ROW_TABLE_HEAD, "RATE", theme::DIM);
    c.right(F_LAT, ROW_TABLE_HEAD, "LAT", theme::DIM);
    c.left(F_SPARK, ROW_TABLE_HEAD, "60s", theme::DIM);
    c.rule(CONTENT, ROW_TABLE_RULE, theme::FAINT);

    let visible = app.visible();
    if visible.is_empty() {
        let msg = if !app.snap.caps.per_app_streams {
            "per-app streams need macOS 14 or newer"
        } else if app.applied.is_some() || app.mode == Mode::Filter {
            "no streams match"
        } else {
            "no audio streams active"
        };
        c.left(CONTENT, ROW_LIST, msg, theme::FAINT);
        return;
    }

    let rows = app.list_rows();
    for (i, s) in visible.iter().skip(app.scroll).take(rows).enumerate() {
        let idx = app.scroll + i;
        let selected = idx == app.sel && app.mode != Mode::Filter;
        stream_row(c, app, ROW_LIST + i as u16, s, selected);
    }
}

fn stream_row(c: &mut Canvas, app: &App, y: u16, s: &Stream, selected: bool) {
    if selected {
        c.tint(CONTENT, y, theme::SEL_BG);
    }
    let is_in = s.direction.is_input();
    let base = theme::direction(is_in);
    let dim_if_paused = |col: Color| if app.paused { theme::FAINT } else { col };

    c.left(F_APP, y, &s.app, theme::FG);

    if is_in {
        // The mic-in-use marker. This is why there is no DIRECTION column.
        c.text(F_DEVICE, F_DEVICE.x0, y, c.g.dot, dim_if_paused(theme::CYAN), false);
        c.left(F_DEVICE_IN, y, &s.device, theme::DIM);
    } else {
        c.left(F_DEVICE, y, &s.device, theme::DIM);
    }

    match s.level_dbfs.filter(|_| app.snap.caps.stream_levels) {
        Some(d) => {
            let fg = if app.paused { theme::FAINT } else { theme::level(d, base) };
            c.right(F_LEVEL, y, &fmt::dbfs(d), fg);
        }
        None => {
            c.right(F_LEVEL, y, fmt::NA, theme::DIM);
        }
    }

    // A conversion is worth a colour; a plain format is not.
    let conv = s.requested.and_then(|r| {
        fmt::conversion((r.rate, r.bits), (s.format.rate, s.format.bits), c.g.arrow, F_RATE.width())
    });
    match conv {
        Some(text) => c.right(F_RATE, y, &text, dim_if_paused(theme::YELLOW)),
        None => {
            let t = if s.format.rate == 0 {
                fmt::NA.to_string()
            } else {
                fmt::rate_bits(s.format.rate, s.format.bits)
            };
            c.right(F_RATE, y, &t, theme::DIM)
        }
    };

    let lat = s.latency_ms.map(|v| fmt::ms(v, F_LAT.width())).unwrap_or(fmt::NA.into());
    c.right(F_LAT, y, &lat, theme::DIM);

    let spark_fg = if selected && !app.paused { base } else { theme::FAINT };
    if let Some(h) = app.history_for(&s.key).filter(|h| h.has_data()) {
        let cells = meter::downsample_max(&h.series(), F_SPARK.width() as usize);
        for (i, d) in cells.iter().enumerate() {
            c.set(F_SPARK.x0 + i as u16, y, meter::spark(*d, &c.g.blocks), spark_fg);
        }
    }
}

// ── rows 19-21, detail ───────────────────────────────────────────────────────

fn detail(c: &mut Canvas, app: &App) {
    let Some(s) = app.selected() else { return };
    c.text(CONTENT, 3, ROW_DETAIL, c.g.corner, theme::FAINT, false);

    let body = Field::new(6, 78);
    let layout = match s.format.channels {
        0 => fmt::NA.to_string(),
        1 => "1ch mono".into(),
        2 => "2ch stereo".into(),
        n => format!("{n}ch"),
    };
    let pid = s.pid.map(|p| p.to_string()).unwrap_or(fmt::NA.into());
    let node = s.node_id.map(|n| n.to_string()).unwrap_or(fmt::NA.into());
    let dir = if s.direction.is_input() { "input" } else { "output" };
    c.left(
        body,
        ROW_DETAIL,
        &format!("pid {pid}   node {node}   direction {dir}   {layout}"),
        theme::DIM,
    );

    let rate = if s.format.rate == 0 {
        fmt::NA.to_string()
    } else {
        format!("{} Hz / {}-bit", s.format.rate, s.format.bits)
    };
    let conv = match s.requested {
        Some(r) if r.rate != s.format.rate => {
            format!("   requested {} {} resampling", r.rate, c.g.arrow)
        }
        Some(r) if r.bits > s.format.bits => {
            format!("   requested {}-bit {} truncated", r.bits, c.g.arrow)
        }
        _ if !app.snap.caps.requested_format => "   requested format not reported".into(),
        _ => String::new(),
    };
    let buffer = app
        .snap
        .default_out
        .as_ref()
        .map(|d| format!("   {}", fmt::frames(d.buffer_frames)))
        .unwrap_or_default();
    c.left(body, ROW_DETAIL + 1, &format!("{rate}{conv}{buffer}"), theme::DIM);

    let lat = s
        .latency_ms
        .map(|v| format!("latency {}", fmt::ms(v, 8)))
        .unwrap_or(format!("latency {}", fmt::NA));
    let peak = app
        .history_for(&s.key)
        .and_then(|h| h.peak())
        .map(|p| format!("peak {} dBFS", fmt::dbfs(p)))
        .unwrap_or(format!("peak {}", fmt::NA));
    // "held" would claim we know when the app opened the stream. On CoreAudio we
    // only know when this process first saw it.
    let held_label = if app.snap.caps.hold_is_since_launch { "observed" } else { "held" };
    let held = format!("{held_label} {}", fmt::hms(app.snap.at.secs_since(s.first_seen)));
    c.left(body, ROW_DETAIL + 2, &format!("{lat}   {peak}   {held}"), theme::DIM);
}

// ── row 22 ───────────────────────────────────────────────────────────────────

fn prompt(c: &mut Canvas, app: &App) {
    let total = app.snap.streams.len();
    let shown = app.visible().len();

    if app.mode == Mode::Filter {
        c.text(CONTENT, 1, ROW_PROMPT, "/", theme::YELLOW, true);
        let x = c.text(Field::new(3, 60), 3, ROW_PROMPT, &app.query, theme::FG, false);
        c.text(CONTENT, x, ROW_PROMPT, c.g.cursor, theme::FG, false);
        // Derived from the filtered set, never hardcoded.
        let count = format!("{shown} of {total} match");
        c.text(CONTENT, x + width(c.g.cursor) + 2, ROW_PROMPT, &count, theme::DIM, false);
        return;
    }

    if let Some(q) = &app.applied {
        c.text(CONTENT, 1, ROW_PROMPT, "/", theme::YELLOW, false);
        let x = c.text(Field::new(3, 60), 3, ROW_PROMPT, q, theme::DIM, false);
        c.text(
            CONTENT,
            x + 2,
            ROW_PROMPT,
            &format!("{shown} of {total} match"),
            theme::FAINT,
            false,
        );
    }

    // The list is eight rows and a desktop routinely has more. The spec has no
    // overflow story at all; row 22 is otherwise blank, so it carries the range.
    let rows = app.list_rows();
    if shown > rows {
        let last = (app.scroll + rows).min(shown);
        let range = format!("{}-{} of {}", app.scroll + 1, last, shown);
        c.right(CONTENT, ROW_PROMPT, &range, theme::FAINT);
    }
}

// ── row 23 ───────────────────────────────────────────────────────────────────

fn footer(c: &mut Canvas, app: &App) {
    let keys: &[(&str, &str)] = if app.mode == Mode::Filter {
        &[("\u{21B5}", "apply"), ("esc", "cancel")]
    } else {
        &[("q", "quit"), ("p", "pause"), ("/", "filter"), ("\u{21B5}", "detail"), ("?", "help")]
    };
    let mut x = CONTENT.x0;
    let field = CONTENT.ending_before(78 - width(VERSION) + 1, 2);
    for (i, (k, label)) in keys.iter().enumerate() {
        if i > 0 {
            x += 3;
        }
        let key = if *k == "\u{21B5}" { c.g.enter } else { k };
        x = c.text(field, x, ROW_FOOTER, key, theme::CYAN, true);
        x = c.text(field, x, ROW_FOOTER, &format!(" {label}"), theme::DIM, false);
    }
    c.right(CONTENT, ROW_FOOTER, VERSION, theme::FAINT);
}

// ── the help overlay ─────────────────────────────────────────────────────────

/// `?` is in the footer, in the five keys, and promised by the handoff README as
/// "an overlay that returns to the same screen" — but no version of the spec
/// says what it contains or how it is dismissed. This is that screen.
fn help(c: &mut Canvas, app: &App) {
    let outer = Field::new(9, 70);
    let (y0, y1) = (3u16, 20u16);
    c.panel(outer, y0, y1, theme::FAINT, theme::BG);
    c.text(outer, outer.x0 + 2, y0, " soundwatch-lite ", theme::FG, true);

    let inner = Field::new(outer.x0 + 2, outer.x1 - 2);
    let keys: &[(&str, &str)] = &[
        ("q", "quit"),
        ("p", "pause \u{2014} freezes the meters and peak holds"),
        ("/", "filter by app or device name"),
        (c.g.enter, "expand the selected stream"),
        (c.g.updown, "move the selection (j and k also work)"),
        ("esc", "close detail, or clear the filter"),
        ("?", "this overlay \u{2014} any key returns"),
    ];
    let mut y = y0 + 2;
    for (k, desc) in keys {
        c.text(inner, inner.x0, y, k, theme::CYAN, true);
        c.text(inner, inner.x0 + 5, y, desc, theme::DIM, false);
        y += 1;
    }

    y += 1;
    let legend: &[(&str, Color, &str)] = &[
        ("green", theme::GREEN, "output / playback path"),
        ("cyan", theme::CYAN, "input / capture path"),
        ("yellow", theme::YELLOW, "above -6 dBFS, or a conversion"),
        ("red", theme::RED, theme::RED_RULE),
    ];
    for (name, col, desc) in legend {
        c.text(inner, inner.x0, y, name, *col, false);
        c.text(inner, inner.x0 + 7, y, desc, theme::DIM, false);
        y += 1;
    }

    y += 1;
    let last = app
        .snap
        .caps
        .note
        .clone()
        .unwrap_or_else(|| "read-only: nothing here changes your audio".into());
    c.left(inner, y, &last, theme::FAINT);
}
