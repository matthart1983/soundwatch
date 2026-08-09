//! The screen.
//!
//! Row and column numbers come from [`Layout`], which reproduces the spec's
//! numbers exactly at 80x24 and grows from there. Everything is drawn through
//! [`Field`]-bounded writes, so no value can reach outside its region however
//! long the underlying string turns out to be, at any size.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::app::{App, Mode};
use crate::chart;
use crate::fmt;
use crate::grid::{Canvas, Field, MIN_COLS, MIN_ROWS, width};
use crate::layout::Layout;
use crate::meter;
use crate::model::{Stream, Verdict};
use crate::theme;

pub const VERSION: &str = concat!("soundwatch-lite ", env!("CARGO_PKG_VERSION"));
/// What the footer falls back to when the full string will not fit.
pub const SHORT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A compact meter for the Overview tab: the label row and a short chart.
/// Returns the row after it.
pub fn mini_meter(c: &mut Canvas, l: &Layout, app: &App, top: u16, rows: u16, input: bool) -> u16 {
    let (dev, hist, base, live, unit) = if input {
        (
            app.snap.default_in.as_ref(),
            &app.in_hist,
            theme::CYAN,
            app.snap.caps.input_levels,
            "dBFS in",
        )
    } else {
        (
            app.snap.default_out.as_ref(),
            &app.out_hist,
            theme::GREEN,
            app.snap.caps.device_levels,
            "dBFS out",
        )
    };
    let glyph = if input { c.g.inp } else { c.g.out };
    let live = live && dev.is_some();
    meter_label(
        c,
        l,
        app,
        top,
        glyph,
        base,
        unit,
        hist,
        live,
        dev.map(|d| fmt::rate_bits(d.format.rate, d.format.bits)),
        dev.map(|d| d.name.clone()),
    );
    meter_body(c, l, app, MeterView { top: top + 1, h: rows, hist, base, live });
    top + 1 + rows
}

/// The spectrum graph, for the Spectrum tab.
pub fn spectrum_body(c: &mut Canvas, l: &Layout, app: &App) {
    spectrum(c, l, app);
}

pub fn render(app: &App, buf: &mut Buffer, area: Rect) {
    if area.width < MIN_COLS || area.height < MIN_ROWS {
        too_small(buf, area);
        return;
    }
    // The layout is the App's, set from the terminal size before the frame, so
    // that the selection was clamped against the same list height being drawn.
    let l = app.layout;
    let mut c =
        Canvas::new(buf, area.x, area.y, l.w.min(area.width), l.h.min(area.height), app.glyphs);

    c.clear(theme::BG);

    // The full product: chrome plus whichever tab is active. Lite keeps the
    // handoff's single screen, which is what its row numbers are pinned to.
    if l.chrome == crate::layout::Chrome::Tabs {
        header_full(&mut c, &l, app);
        crate::tabs::bar(&mut c, &l, app);
        crate::tabs::body(&mut c, &l, app);
        c.rule(l.content, l.row_footer_rule, theme::FAINT);
        prompt(&mut c, &l, app);
        footer(&mut c, &l, app);
        if app.mode == Mode::Help {
            help(&mut c, &l, app);
        }
        if app.mode == Mode::Settings {
            settings(&mut c, &l, app);
        }
        return;
    }

    header(&mut c, &l, app);
    if app.spectrum.source.is_on() {
        // Spectrum replaces the meters, the axis, the vitals, the note and the
        // table: the question has changed from *which app* to *what is in this
        // signal*. The header and footer stay, so the machine's health is still
        // visible while you stare at the graph.
        spectrum(&mut c, &l, app);
    } else {
        meters(&mut c, &l, app);
        axis(&mut c, &l, app);
        vitals(&mut c, &l, app);
        note(&mut c, &l, app);
        table(&mut c, &l, app);
        if app.mode == Mode::Detail {
            detail(&mut c, &l, app);
        }
    }
    prompt(&mut c, &l, app);
    footer(&mut c, &l, app);
    if app.mode == Mode::Help {
        help(&mut c, &l, app);
    }
    if app.mode == Mode::Settings {
        settings(&mut c, &l, app);
    }
}

fn too_small(buf: &mut Buffer, area: Rect) {
    let msg = format!(
        "soundwatch-lite needs {MIN_COLS}x{MIN_ROWS} (have {}x{})",
        area.width, area.height
    );
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

/// The full product's header: identity on the left, state on the right.
fn header_full(c: &mut Canvas, l: &Layout, app: &App) {
    let f = l.content;
    let live = !app.paused;
    let dot_fg = if app.verdict.is_alert() {
        theme::RED
    } else if live {
        theme::GREEN
    } else {
        theme::YELLOW
    };
    let x = c.text(f, f.x0, l.row_header, c.g.dot, dot_fg, false);
    let x = c.text(f, x + 1, l.row_header, "SoundWatch", theme::CYAN, true);
    let x = c.text(f, x + 1, l.row_header, SHORT_VERSION, theme::DIM, false);
    let x = c.text(f, x + 2, l.row_header, "\u{2502}", theme::FAINT, false);
    let x = c.text(f, x + 2, l.row_header, "host", theme::DIM, false);
    let x = c.text(f, x + 1, l.row_header, &app.snap.host, theme::FG, false);
    let x = c.text(f, x + 2, l.row_header, app.snap.backend, theme::DIM, false);

    // Right group: the verdict, then the state word.
    let (word, fg) = if app.paused {
        ("PAUSED", theme::YELLOW)
    } else if let Verdict::Alert { .. } = &app.verdict {
        ("ALERT", theme::RED)
    } else {
        ("LIVE", theme::GREEN)
    };
    c.right(f, l.row_header, word, fg);
    if let Verdict::Alert { headline, .. } = &app.verdict {
        let room = f.x1.saturating_sub(width(word) + 2);
        c.right(Field::new(x + 2, room), l.row_header, headline, theme::RED);
    }
}

fn header(c: &mut Canvas, l: &Layout, app: &App) {
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
    let right_x = l.content.x1.saturating_sub(right_w).saturating_add(1);
    let left = l.content.ending_before(right_x, 2);

    if let Some(dc) = dot {
        c.text(l.content, right_x, l.row_header, c.g.dot, dc, false);
        c.text(l.content, right_x + dot_w, l.row_header, &right, right_fg, false);
    } else {
        c.right_bold(Field::new(right_x, l.content.x1), l.row_header, &right, right_fg);
    }

    let name = "soundwatch";
    let backend = format!(" \u{b7} {}", app.snap.backend);
    let mut x = c.text(left, left.x0, l.row_header, name, theme::FG, true);
    // The hostname is the elastic element: a studio box reached over SSH can
    // easily carry a 40-character FQDN, and the spec bounds neither it nor the
    // backend name.
    let host_room = left.width().saturating_sub(width(name) + 2 + width(&backend));
    let host = format!("  {}", crate::grid::truncate(&app.snap.host, host_room));
    x = c.text(left, x, l.row_header, &host, theme::CYAN, false);
    c.text(left, x, l.row_header, &backend, theme::DIM, false);
}

// ── rows 2-8 ─────────────────────────────────────────────────────────────────

fn meters(c: &mut Canvas, l: &Layout, app: &App) {
    let out_dev = app.snap.default_out.as_ref();
    let in_dev = app.snap.default_in.as_ref();
    // The backend declares whether levels are readable; the UI never guesses.
    // Output and input are declared separately: on macOS they come from
    // different APIs with different consent, and one commonly works alone.
    let out_live = app.snap.caps.device_levels;

    meter_label(
        c,
        l,
        app,
        l.row_out_label,
        c.g.out,
        theme::GREEN,
        "dBFS out",
        &app.out_hist,
        out_live,
        out_dev.map(|d| fmt::rate_bits(d.format.rate, d.format.bits)),
        out_dev.map(|d| d.name.clone()),
    );
    meter_body(
        c,
        l,
        app,
        MeterView {
            top: l.row_out_meter,
            h: l.out_meter_h,
            hist: &app.out_hist,
            base: theme::GREEN,
            live: out_live,
        },
    );

    let has_input = in_dev.is_some();
    let in_live = app.snap.caps.input_levels && has_input;
    meter_label(
        c,
        l,
        app,
        l.row_in_label,
        c.g.inp,
        theme::CYAN,
        if has_input { "dBFS in" } else { "no input" },
        &app.in_hist,
        in_live,
        None,
        in_dev.map(|d| d.name.clone()),
    );
    meter_body(
        c,
        l,
        app,
        MeterView {
            top: l.row_in_meter,
            h: l.in_meter_h,
            hist: &app.in_hist,
            base: theme::CYAN,
            live: in_live,
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn meter_label(
    c: &mut Canvas,
    l: &Layout,
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
    c.text(l.content, 1, row, glyph, arrow_fg, true);

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
    let unit_end = c.text(l.content, 9, row, unit, theme::DIM, false);

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
    let right = Field::new(unit_end + 2, l.content.x1);
    let text = match device {
        Some(dev) => {
            let room = right.width().saturating_sub(width(&fixed) + 2);
            format!("{fixed}  {}", crate::grid::truncate(&dev, room))
        }
        None => fixed,
    };
    c.right(right, row, &text, theme::DIM);
}

/// One meter chart: where it goes, what it plots, and whether it is real.
struct MeterView<'a> {
    top: u16,
    h: u16,
    hist: &'a meter::History,
    base: Color,
    live: bool,
}

fn meter_body(c: &mut Canvas, l: &Layout, app: &App, v: MeterView) {
    let MeterView { top, h, hist, base, live } = v;
    let blocks = c.g.blocks;
    if !live || !hist.has_data() {
        // Degraded: a flat faint baseline. Distinct from silence (which draws a
        // baseline in the direction colour) and from an unpainted region.
        for x in l.content.x0..=l.content.x1 {
            c.set(x, top + h - 1, blocks[0], theme::FAINT);
        }
        return;
    }

    // The ring records at its own resolution; reduce it to the chart's, which
    // is two values per cell under a braille theme.
    let cols = l.content.width();
    let sub = app.sub_columns();
    let series = hist.columns(cols as usize * sub);
    let floor = app.cfg.meter_floor;
    let norm: Vec<f32> = series.iter().map(|d| meter::norm(*d, floor)).collect();
    let grid = chart::bars(&norm, cols, h, sub == 2, &blocks);

    for (y, row) in grid.iter().enumerate() {
        for (x, cell) in row.iter().enumerate() {
            let Some(ch) = cell else { continue };
            // Level for the spec theme is the loudest of the sub-columns this
            // cell covers, so a clip can never be averaged out of existence.
            let d = series[x * sub..(x * sub + sub).min(series.len())]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);
            let height = theme::row_height(y as u16, h);
            let fg = if app.paused {
                theme::FAINT
            } else {
                theme::chart_cell(app.theme(), base, d, height)
            };
            c.set(l.content.x0 + x as u16, top + h - 1 - y as u16, *ch, fg);
        }
    }
}

// ── row 9 ────────────────────────────────────────────────────────────────────

fn axis(c: &mut Canvas, l: &Layout, _app: &App) {
    c.rule(l.content, l.row_axis, theme::FAINT);
    c.text(l.content, 1, l.row_axis, " 60s ago ", theme::DIM, false);
    c.right(l.content, l.row_axis, " now ", theme::DIM);
}

// ── row 10 ───────────────────────────────────────────────────────────────────

fn vitals(c: &mut Canvas, l: &Layout, app: &App) {
    let snap = &app.snap;
    let alert = app.verdict.is_alert();

    let verdict_text = match &app.verdict {
        Verdict::Alert { reason, .. } => reason.clone(),
        Verdict::Nominal => "all nominal".into(),
    };
    let verdict_x = l.content.x1.saturating_sub(width(&verdict_text)).saturating_add(1);
    c.right(
        Field::new(verdict_x, l.content.x1),
        l.row_vitals,
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
    let field = l.content.ending_before(verdict_x, 2);
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
        x = c.text(field, x, l.row_vitals, label, theme::DIM, false);
        x = c.text(
            field,
            x + 1,
            l.row_vitals,
            value,
            if *bad { theme::RED } else { theme::FG },
            false,
        );
    }
}

// ── row 11 ───────────────────────────────────────────────────────────────────

fn note(c: &mut Canvas, l: &Layout, app: &App) {
    // A backend that has not answered outranks anything it might have said.
    if let Some(stall) = app.backend_stall() {
        c.left(l.content, l.row_note, stall, theme::FAINT);
        return;
    }
    let mut msg = app.snap.caps.note.clone();
    if app.snap.latency_is_one_way() && msg.is_none() {
        msg = Some("latency is the output path only: nothing is capturing".into());
    }
    if let Some(m) = msg {
        c.left(l.content, l.row_note, &m, theme::FAINT);
    }
}

// ── rows 12-21 ───────────────────────────────────────────────────────────────

fn table(c: &mut Canvas, l: &Layout, app: &App) {
    c.left(l.f_app, l.row_table_head, "APP", theme::DIM);
    c.left(l.f_device, l.row_table_head, "DEVICE", theme::DIM);
    c.right(l.f_level, l.row_table_head, "LEVEL", theme::DIM);
    c.right(l.f_rate, l.row_table_head, "RATE", theme::DIM);
    c.right(l.f_lat, l.row_table_head, "LAT", theme::DIM);
    c.left(l.f_spark, l.row_table_head, "60s", theme::DIM);
    c.rule(l.content, l.row_table_rule, theme::FAINT);

    let visible = app.visible();
    if visible.is_empty() {
        let msg = if !app.snap.caps.per_app_streams {
            "per-app streams need macOS 14 or newer"
        } else if app.applied.is_some() || app.mode == Mode::Filter {
            "no streams match"
        } else {
            "no audio streams active"
        };
        c.left(l.content, l.row_list, msg, theme::FAINT);
        return;
    }

    let rows = app.list_rows();
    for (i, s) in visible.iter().skip(app.scroll).take(rows).enumerate() {
        let idx = app.scroll + i;
        let selected = idx == app.sel && app.mode != Mode::Filter;
        stream_row(c, l, app, l.row_list + i as u16, s, selected);
    }
}

fn stream_row(c: &mut Canvas, l: &Layout, app: &App, y: u16, s: &Stream, selected: bool) {
    if selected {
        c.tint(l.content, y, theme::SEL_BG);
    }
    let is_in = s.direction.is_input();
    let base = theme::direction(is_in);
    let dim_if_paused = |col: Color| if app.paused { theme::FAINT } else { col };

    c.left(l.f_app, y, &s.app, theme::FG);

    if is_in {
        // The mic-in-use marker. This is why there is no DIRECTION column.
        c.text(l.f_device, l.f_device.x0, y, c.g.dot, dim_if_paused(theme::CYAN), false);
        c.left(l.f_device_in, y, &s.device, theme::DIM);
    } else {
        c.left(l.f_device, y, &s.device, theme::DIM);
    }

    match s.level_dbfs.filter(|_| app.snap.caps.stream_levels) {
        Some(d) => {
            let fg = if app.paused { theme::FAINT } else { theme::level(d, base) };
            c.right(l.f_level, y, &fmt::dbfs(d), fg);
        }
        None => {
            c.right(l.f_level, y, fmt::NA, theme::DIM);
        }
    }

    // A conversion is worth a colour; a plain format is not.
    let conv = s.requested.and_then(|r| {
        fmt::conversion(
            (r.rate, r.bits),
            (s.format.rate, s.format.bits),
            c.g.arrow,
            l.f_rate.width(),
        )
    });
    match conv {
        Some(text) => c.right(l.f_rate, y, &text, dim_if_paused(theme::YELLOW)),
        None => {
            let t = if s.format.rate == 0 {
                fmt::NA.to_string()
            } else {
                fmt::rate_bits(s.format.rate, s.format.bits)
            };
            c.right(l.f_rate, y, &t, theme::DIM)
        }
    };

    let lat = s.latency_ms.map(|v| fmt::ms(v, l.f_lat.width())).unwrap_or(fmt::NA.into());
    c.right(l.f_lat, y, &lat, theme::DIM);

    let spark_fg = if selected && !app.paused { base } else { theme::FAINT };
    if let Some(h) = app.history_for(&s.key).filter(|h| h.has_data()) {
        let cells = h.columns(l.f_spark.width() as usize);
        for (i, d) in cells.iter().enumerate() {
            c.set(
                l.f_spark.x0 + i as u16,
                y,
                meter::spark(*d, &c.g.blocks, app.cfg.meter_floor),
                spark_fg,
            );
        }
    }
}

// ── rows 19-21, detail ───────────────────────────────────────────────────────

fn detail(c: &mut Canvas, l: &Layout, app: &App) {
    let Some(s) = app.selected() else { return };
    c.text(l.content, 3, l.row_detail(), c.g.corner, theme::FAINT, false);

    let body = Field::new(6, l.content.x1);
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
        l.row_detail(),
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
    c.left(body, l.row_detail() + 1, &format!("{rate}{conv}{buffer}"), theme::DIM);

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
    c.left(body, l.row_detail() + 2, &format!("{lat}   {peak}   {held}"), theme::DIM);
}

// ── row 22 ───────────────────────────────────────────────────────────────────

fn prompt(c: &mut Canvas, l: &Layout, app: &App) {
    let total = app.snap.streams.len();
    let shown = app.visible().len();

    if app.mode == Mode::Filter {
        c.text(l.content, 1, l.row_prompt, "/", theme::YELLOW, true);
        let query_field = Field::new(3, l.content.x1.saturating_sub(18));
        let x = c.text(query_field, 3, l.row_prompt, &app.query, theme::FG, false);
        c.text(l.content, x, l.row_prompt, c.g.cursor, theme::FG, false);
        // Derived from the filtered set, never hardcoded.
        let count = format!("{shown} of {total} match");
        c.text(l.content, x + width(c.g.cursor) + 2, l.row_prompt, &count, theme::DIM, false);
        return;
    }

    if let Some(q) = &app.applied {
        c.text(l.content, 1, l.row_prompt, "/", theme::YELLOW, false);
        let query_field = Field::new(3, l.content.x1.saturating_sub(18));
        let x = c.text(query_field, 3, l.row_prompt, q, theme::DIM, false);
        c.text(
            l.content,
            x + 2,
            l.row_prompt,
            &format!("{shown} of {total} match"),
            theme::FAINT,
            false,
        );
    }

    // The list is eight rows and a desktop routinely has more. The spec has no
    // overflow story at all; row 22 is otherwise blank, so it carries the range.
    //
    // On the tabbed screens each tab draws its own range in its own body, and
    // this row would otherwise report a *stream* count while you scrolled a
    // device list — a number computed from one list and labelled with another.
    if l.chrome != crate::layout::Chrome::Tabs {
        let rows = app.list_rows();
        if shown > rows {
            let last = (app.scroll + rows).min(shown);
            let range = format!("{}-{} of {}", app.scroll + 1, last, shown);
            c.right(l.content, l.row_prompt, &range, theme::FAINT);
        }
    }
}

// ── row 23 ───────────────────────────────────────────────────────────────────

fn footer(c: &mut Canvas, l: &Layout, app: &App) {
    let all: &[(&str, &str)] = if app.mode == Mode::Filter {
        &[("\u{21B5}", "apply"), ("esc", "cancel")]
    } else {
        &[
            ("q", "quit"),
            ("p", "pause"),
            ("1-0", "tab"),
            (",", "settings"),
            ("/", "filter"),
            ("\u{21B5}", "detail"),
            ("?", "help"),
        ]
    };
    // Lite has no tabs, so it advertises the spectrum key in that slot.
    let lite_keys: Vec<(&str, &str)> =
        all.iter().map(|(k, v)| if *k == "1-0" { ("s", "spectrum") } else { (*k, *v) }).collect();
    let all: &[(&str, &str)] =
        if l.chrome == crate::layout::Chrome::Lite { &lite_keys } else { all };

    // Seven keys and a version string do not fit 80 columns, and the spec puts
    // both on this row. Two things give way in order: the version drops its
    // name to a bare number (which still answers "which build is this?"), and
    // then keys are shed — from the *second* to last, never the last, because
    // the last one is `?` and it is the key that explains all the others. A
    // footer that has quietly dropped `? help` is a tool with no way in.
    let width_of = |ks: &[(&str, &str)]| -> u16 {
        if ks.is_empty() {
            return 0;
        }
        ks.iter().map(|(k, v)| width(k) + 1 + width(v)).sum::<u16>() + 3 * (ks.len() as u16 - 1)
    };

    let mut keys: Vec<(&str, &str)> = all.to_vec();
    let mut version = Some(VERSION);
    let fits = |keys: &[(&str, &str)], v: Option<&str>| {
        width_of(keys) + v.map(|v| 2 + width(v)).unwrap_or(0) <= l.content.width()
    };
    if !fits(&keys, version) {
        version = Some(SHORT_VERSION);
    }
    while !fits(&keys, version) && keys.len() > 1 {
        keys.remove(keys.len() - 2);
    }

    let field = match version {
        Some(v) => l.content.ending_before(l.content.x1 + 2 - width(v), 2),
        None => l.content,
    };
    let mut x = l.content.x0;
    for (i, (k, label)) in keys.iter().enumerate() {
        if i > 0 {
            x += 3;
        }
        let key = if *k == "\u{21B5}" { c.g.enter } else { k };
        x = c.text(field, x, l.row_footer, key, theme::CYAN, true);
        x = c.text(field, x, l.row_footer, &format!(" {label}"), theme::DIM, false);
    }
    if let Some(v) = version {
        c.right(l.content, l.row_footer, v, theme::FAINT);
    }
}

// ── the spectrum screen ──────────────────────────────────────────────────────

fn spectrum(c: &mut Canvas, l: &Layout, app: &App) {
    let sp = &app.spectrum;
    let base = match sp.source {
        crate::spectrum::Source::Input => theme::CYAN,
        _ => theme::GREEN,
    };
    let glyph = if sp.source == crate::spectrum::Source::Input { c.g.inp } else { c.g.out };
    let (top, rows) = l.spectrum();

    // ── the label row ───────────────────────────────────────────────────────
    let arrow_fg = if app.paused { theme::FAINT } else { base };
    let mut x = c.text(l.content, 1, l.row_out_label, glyph, arrow_fg, true);
    x = c.text(l.content, x + 1, l.row_out_label, sp.source.label(), theme::FG, true);

    let rate = sp.analysis().map(|a| a.rate).unwrap_or(0);
    let detail = if rate > 0 {
        format!(
            "   {}pt hann   {:.1} Hz/bin",
            sp.window_len(),
            rate as f32 / sp.window_len() as f32
        )
    } else {
        "   waiting for audio".into()
    };
    c.text(l.content, x, l.row_out_label, &detail, theme::DIM, false);

    // The peak readout is the elastic element: dropped whole rather than
    // truncated, because a truncated frequency is a wrong frequency.
    if let Some((hz, db)) = sp.peak() {
        let text = if hz >= 1000.0 {
            format!("peak {:.2} kHz  {}", hz / 1000.0, fmt::dbfs(db))
        } else {
            format!("peak {hz:.0} Hz  {}", fmt::dbfs(db))
        };
        let right = Field::new(x + width(&detail) + 2, l.content.x1);
        if right.width() >= width(&text) {
            c.right(right, l.row_out_label, &text, theme::DIM);
        }
    }

    // ── the graph ───────────────────────────────────────────────────────────
    let cols = sp.columns();
    if cols.is_empty() {
        for cx in l.content.x0..=l.content.x1 {
            c.set(cx, top + rows - 1, c.g.blocks[0], theme::FAINT);
        }
    } else {
        let blocks = c.g.blocks;
        let sub = app.sub_columns();
        let cells = l.content.width();
        let norm: Vec<f32> =
            cols.iter().map(|col| crate::spectrum::norm(col.level, sp.floor())).collect();
        let grid = chart::bars(&norm, cells, rows, sub == 2, &blocks);

        for (y, row) in grid.iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                let Some(ch) = cell else { continue };
                let d = cols[(x * sub).min(cols.len() - 1)..(x * sub + sub).min(cols.len())]
                    .iter()
                    .map(|col| col.level)
                    .fold(f32::NEG_INFINITY, f32::max);
                let height = theme::row_height(y as u16, rows);
                let fg = if app.paused {
                    theme::FAINT
                } else {
                    theme::chart_cell(app.theme(), base, d, height)
                };
                c.set(l.content.x0 + x as u16, top + rows - 1 - y as u16, *ch, fg);
            }
        }

        // Peak-hold caps, above the live bars, so held transients stay
        // readable after the bar has fallen away from them.
        for (x, chunk) in cols.chunks(sub).enumerate().take(cells as usize) {
            let hold = chunk.iter().map(|c| c.hold).fold(f32::NEG_INFINITY, f32::max);
            let level = chunk.iter().map(|c| c.level).fold(f32::NEG_INFINITY, f32::max);
            if hold > level + 1.0 {
                let hcell = (crate::spectrum::norm(hold, sp.floor()) * rows as f32).round() as u16;
                if hcell > 0 && hcell <= rows {
                    // Match the glyph family the bars are drawn in, or the cap
                    // sits a pixel low and reads as a different kind of mark.
                    let cap = if sub == 2 { '\u{28C0}' } else { blocks[0] };
                    c.set(l.content.x0 + x as u16, top + rows - hcell, cap, theme::FAINT);
                }
            }
        }
    }

    // ── the frequency axis ──────────────────────────────────────────────────
    let arow = l.row_spectrum_axis();
    c.rule(l.content, arow, theme::FAINT);
    if rate > 0 {
        let axis = crate::spectrum::Axis::new(l.content.width(), rate as f32 / 2.0);
        for (hz, label) in [(100.0f32, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
            if let Some(col) = axis.column_of(hz) {
                let cx = l.content.x0 + col;
                let half = width(label) / 2;
                let start = cx.saturating_sub(half).max(l.content.x0);
                c.text(l.content, start, arow, label, theme::DIM, false);
            }
        }
        c.text(l.content, l.content.x0, arow, "20", theme::DIM, false);
    }

    // ── the verdict ─────────────────────────────────────────────────────────
    let vrow = l.row_spectrum_verdict();
    if let Some(v) = sp.verdict() {
        c.left(l.content, vrow, &v, theme::RED);
    } else if let Some(stall) = app.backend_stall() {
        c.left(l.content, vrow, stall, theme::FAINT);
    } else if !sp.has_data() {
        let msg = match sp.source {
            crate::spectrum::Source::Input if !app.snap.caps.input_levels => {
                "input is not metered \u{2014} restart with --meter-input"
            }
            crate::spectrum::Source::Output if !app.snap.caps.device_levels => {
                "output is not metered \u{2014} run --probe-tap to find out why"
            }
            _ => "waiting for audio\u{2026}",
        };
        c.left(l.content, vrow, msg, theme::FAINT);
    } else {
        c.left(l.content, vrow, "no faults in this signal", theme::FAINT);
    }
}

// ── the settings menu ────────────────────────────────────────────────────────

/// Everything the tool can be told to do differently, on one screen.
///
/// Values are shown, not hidden behind a submenu: the point of a settings
/// screen in a diagnostic tool is to answer "what is this thing currently
/// doing?" as much as to change it. Each row carries one line saying what the
/// setting *costs*, because every one of these is a trade and none of them has
/// a right answer.
fn settings(c: &mut Canvas, l: &Layout, app: &App) {
    use crate::config::Setting;

    let rows = Setting::ALL.len() as u16;
    let sections = Setting::ALL.iter().filter(|s| s.section().is_some()).count() as u16;
    // Two borders, one row per setting, one per heading, a blank, the note for
    // the selected row, and the key hints.
    let panel_h = (rows + sections + 5).min(l.h.saturating_sub(2));
    let panel_w = 64u16.min(l.content.width());
    let x0 = l.content.x0 + (l.content.width().saturating_sub(panel_w)) / 2;
    let outer = Field::at(x0, panel_w);
    let y0 = (l.h.saturating_sub(panel_h)) / 2;
    let y1 = y0 + panel_h - 1;
    c.panel(outer, y0, y1, theme::FAINT, theme::BG);
    c.text(outer, outer.x0 + 2, y0, " settings ", theme::FG, true);

    let inner = Field::new(outer.x0 + 2, outer.x1 - 2);
    let value_x = inner.x1.saturating_sub(15);
    let mut y = y0 + 1;

    for (i, s) in Setting::ALL.iter().enumerate() {
        if y >= y1 - 2 {
            break;
        }
        if let Some(section) = s.section() {
            c.text(inner, inner.x0, y, section, theme::DIM, true);
            y += 1;
        }
        let selected = i == app.setting;
        if selected {
            c.tint(Field::new(inner.x0 - 1, inner.x1 + 1), y, theme::SEL_BG);
            // A marker as well as the tint: the selection has to be findable
            // in a screenshot, over SSH to a terminal with no colour, and by
            // anyone who cannot pick a dark blue background out of a dark one.
            c.text(inner, inner.x0, y, c.g.sel, theme::CYAN, true);
        }
        let label_fg = if selected { theme::FG } else { theme::DIM };
        c.text(inner, inner.x0 + 2, y, s.label(), label_fg, selected);
        // Values are right-aligned in their own column so the eye can run down
        // them; a changed setting is the one thing you want to spot instantly.
        let value_fg = if selected { theme::CYAN } else { theme::FG };
        c.right(Field::new(value_x, inner.x1), y, &s.value(&app.cfg), value_fg);
        y += 1;
    }

    // The selected row's explanation, on its own line rather than crammed
    // beside the value, because these are sentences.
    let note_y = y1 - 2;
    if let Some(s) = Setting::ALL.get(app.setting) {
        c.left(inner, note_y, s.note(), theme::FAINT);
    }

    let footer = match &app.save_status {
        Some(msg) => msg.clone(),
        None => {
            let keys =
                "\u{2191}\u{2193} choose   \u{2190}\u{2192} change   w write   r reset   esc close";
            keys.into()
        }
    };
    let fg = if app.save_status.is_some() { theme::YELLOW } else { theme::DIM };
    c.left(inner, y1 - 1, crate::grid::truncate(&footer, inner.width()), fg);
}

// ── the help overlay ─────────────────────────────────────────────────────────

/// `?` is in the footer, in the five keys, and promised by the handoff README as
/// "an overlay that returns to the same screen" — but no version of the spec
/// says what it contains or how it is dismissed. This is that screen.
fn help(c: &mut Canvas, l: &Layout, app: &App) {
    // Centred on the canvas rather than pinned to the spec's columns, so the
    // panel stays a panel instead of drifting into the top-left corner of a
    // large terminal. Width and height are capped: a help panel that grows to
    // fill a 200-column screen is a wall of text, not an overlay.
    let panel_w = 62u16.min(l.content.width().saturating_sub(4));
    // Nine key rows, a blank, four legend rows, a blank and the note, plus two
    // borders: twenty. At eighteen the note landed exactly on the bottom
    // border and overwrote it.
    let panel_h = 20u16.min(l.h.saturating_sub(2));
    let x0 = l.content.x0 + (l.content.width().saturating_sub(panel_w)) / 2;
    let outer = Field::at(x0, panel_w);
    let y0 = (l.h.saturating_sub(panel_h)) / 2;
    let y1 = y0 + panel_h - 1;
    c.panel(outer, y0, y1, theme::FAINT, theme::BG);
    c.text(outer, outer.x0 + 2, y0, " soundwatch-lite ", theme::FG, true);

    let inner = Field::new(outer.x0 + 2, outer.x1 - 2);
    let keys: &[(&str, &str)] = &[
        ("q", "quit"),
        ("p", "pause \u{2014} freezes the meters and peak holds"),
        ("s", "spectrum \u{2014} cycles output, input, off"),
        (",", "settings \u{2014} themes, floors, ballistics, and saving them"),
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
    // The legend describes what a colour *means*, and that changes with the
    // theme. Under `spec` a bar's colour is its level; under `btop` it is the
    // bar's height and means nothing on its own. Printing the spec legend
    // under the btop theme would be a lie on the one screen whose job is to
    // explain the screen.
    let legend: &[(&str, Color, &str)] = match app.theme() {
        theme::Theme::Spec => &[
            ("green", theme::GREEN, "output / playback path"),
            ("cyan", theme::CYAN, "input / capture path"),
            ("yellow", theme::YELLOW, "above -6 dBFS, or a conversion"),
            ("red", theme::RED, theme::RED_RULE),
        ],
        theme::Theme::Btop => &[
            ("green", theme::GREEN, "output / playback path"),
            ("cyan", theme::CYAN, "input / capture path"),
            ("bars", theme::YELLOW, "shaded by height, not by meaning"),
            ("red", theme::RED, "in text and headers only: something is wrong"),
        ],
    };
    for (name, col, desc) in legend {
        c.text(inner, inner.x0, y, name, *col, false);
        c.text(inner, inner.x0 + 7, y, desc, theme::DIM, false);
        y += 1;
    }

    y += 1;
    let last = app.snap.caps.note.clone().unwrap_or_else(|| {
        format!(
            "read-only \u{b7} theme {} \u{b7} nothing here changes your audio",
            app.theme().name()
        )
    });
    c.left(inner, y, &last, theme::FAINT);
}
