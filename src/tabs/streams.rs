//! [3] Streams — the per-app table, with the room the Lite screen never had.
//!
//! Same question as the Lite table (`what has my microphone`, `what is making
//! that noise`) with the columns it wanted: the process, both its identifiers,
//! the device, the format, latency and how long it has been holding on.

use crate::app::App;
use crate::fmt;
use crate::grid::{Canvas, Field};
use crate::layout::Layout;
use crate::meter;
use crate::theme;

pub fn draw(c: &mut Canvas, l: &Layout, app: &App) {
    let top = l.body_top;
    let x0 = l.content.x0;
    let x1 = l.content.x1;

    // Laid out right to left from the fixed columns, so the two elastic name
    // fields get exactly what is left and the table always ends on the margin.
    // Laying it out left to right from guessed widths overflowed from 80 to
    // about 104 columns: HELD fell off the screen and the sparkline was never
    // drawn at all.
    const PID_W: u16 = 7;
    const RATE_W: u16 = 9;
    const LAT_W: u16 = 8;
    const HELD_W: u16 = 9;
    const SPARK_W: u16 = 9;
    const GAP: u16 = 2;

    let total = l.content.width();
    // Everything except the two names, including the gaps between them.
    let fixed = PID_W + RATE_W + LAT_W + HELD_W + 5 * GAP;
    let with_spark = fixed + SPARK_W + GAP;
    let spark = total >= with_spark + 24;
    let reserved = if spark { with_spark } else { fixed };
    let names = total.saturating_sub(reserved).max(12);
    let app_w = (names / 3).clamp(8, 24);
    let dev_w = names.saturating_sub(app_w).max(8);

    let f_app = Field::at(x0, app_w);
    let f_pid = Field::at(f_app.x1 + GAP, PID_W);
    let f_dev = Field::at(f_pid.x1 + GAP, dev_w);
    let f_rate = Field::at(f_dev.x1 + GAP, RATE_W);
    let f_lat = Field::at(f_rate.x1 + GAP, LAT_W);
    let f_held = Field::at(f_lat.x1 + GAP, HELD_W);
    let f_spark = spark.then(|| Field::new(f_held.x1 + GAP, x1));

    super::table_head(
        c,
        l,
        top,
        &[
            (f_app, "APP", false),
            (f_pid, "PID", true),
            (f_dev, "DEVICE", false),
            (f_rate, "RATE", true),
            (f_lat, "LAT", true),
            (f_held, "HELD", true),
        ],
    );

    let visible = app.visible();
    if visible.is_empty() {
        let msg = if !app.snap.caps.per_app_streams {
            "per-app streams need macOS 14 or newer"
        } else if app.applied.is_some() {
            "no streams match"
        } else {
            "no audio streams active"
        };
        super::empty(c, l.content, top + 2, msg);
        return;
    }

    let rows = app.list_rows();
    for (i, s) in visible.iter().skip(app.scroll).take(rows).enumerate() {
        let y = top + 2 + i as u16;
        let idx = app.scroll + i;
        if idx == app.sel {
            c.tint(l.content, y, theme::SEL_BG);
        }
        let is_in = s.direction.is_input();
        let base = theme::direction(is_in);

        c.left(f_app, y, &s.app, theme::FG);
        c.right(f_pid, y, &s.pid.map(|p| p.to_string()).unwrap_or(fmt::NA.into()), theme::DIM);

        if is_in {
            // The mic-in-use marker, as on the Lite screen: this is the column
            // that answers the question people actually open the tool for.
            c.text(f_dev, f_dev.x0, y, c.g.dot, base, false);
            c.left(Field::new(f_dev.x0 + 2, f_dev.x1), y, &s.device, theme::DIM);
        } else {
            c.left(f_dev, y, &s.device, theme::DIM);
        }

        let rate = if s.format.rate == 0 {
            fmt::NA.to_string()
        } else {
            fmt::rate_bits(s.format.rate, s.format.bits)
        };
        c.right(f_rate, y, &rate, theme::DIM);
        c.right(
            f_lat,
            y,
            &s.latency_ms.map(|v| fmt::ms(v, 8)).unwrap_or(fmt::NA.into()),
            theme::DIM,
        );
        c.right(f_held, y, &fmt::hms(app.snap.at.secs_since(s.first_seen)), theme::FAINT);
    }

    // The 60s sparkline column, when the width was there to reserve one.
    if let Some(f_spark) = f_spark {
        c.left(f_spark, top, "60s", theme::DIM);
        for (i, s) in visible.iter().skip(app.scroll).take(rows).enumerate() {
            let y = top + 2 + i as u16;
            if let Some(h) = app.history_for(&s.key).filter(|h| h.has_data()) {
                let cells = h.columns(f_spark.width() as usize);
                for (j, d) in cells.iter().enumerate() {
                    c.set(
                        f_spark.x0 + j as u16,
                        y,
                        meter::spark(*d, &c.g.blocks, app.cfg.meter_floor),
                        theme::FAINT,
                    );
                }
            }
        }
    }

    if visible.len() > rows {
        let last = (app.scroll + rows).min(visible.len());
        let range = format!("{}-{} of {}", app.scroll + 1, last, visible.len());
        c.right(l.content, l.body_top + l.body_rows - 1, &range, theme::FAINT);
    }
}
