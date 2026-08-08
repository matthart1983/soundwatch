//! [6] Latency — where the milliseconds actually went.
//!
//! `latency 69.7ms` on the Lite screen is a true number that tells you nothing
//! about what to change. The figure is a sum of four things with very different
//! remedies: the buffer (yours to set), the device's own latency and the
//! driver's safety offset (the hardware's, and the reason a cheap interface
//! feels worse than its buffer size suggests), and the stream latency. This
//! breaks the sum apart on both paths, and shows the round trip as the sum it
//! actually is.

use crate::app::App;
use crate::fmt;
use crate::grid::{Canvas, Field};
use crate::layout::Layout;
use crate::model::Device;
use crate::theme;

/// A buffer this small is where xruns come from on a busy machine.
pub const TIGHT_BUFFER_FRAMES: u32 = 128;

pub fn draw(c: &mut Canvas, l: &Layout, app: &App) {
    let mut y = l.body_top;
    let f = l.content;

    for (title, dev) in [
        ("output path", app.snap.default_out.as_ref()),
        ("input path", app.snap.default_in.as_ref()),
    ] {
        super::section(c, f, y, title);
        y += 1;
        match dev {
            None => {
                super::empty(c, f, y, "nothing is the default on this path");
                y += 2;
            }
            Some(d) => {
                y = path(c, f, y, d);
                y += 1;
            }
        }
        if y >= l.body_top + l.body_rows {
            return;
        }
    }

    // The round trip, which is the number a musician actually cares about.
    super::section(c, f, y, "round trip");
    y += 1;
    let total = app.snap.latency_ms();
    let text = match total {
        Some(ms) if !app.snap.latency_is_one_way() => {
            format!("capture + playback = {}", fmt::ms(ms, 10))
        }
        Some(ms) => format!("{} \u{2014} output path only, nothing is capturing", fmt::ms(ms, 10)),
        None => fmt::NA.to_string(),
    };
    let fg = match total {
        Some(ms) if ms > 40.0 => theme::YELLOW,
        Some(_) => theme::GREEN,
        None => theme::DIM,
    };
    c.left(f, y, &text, fg);
    y += 2;

    if y < l.body_top + l.body_rows {
        let note = match total {
            Some(ms) if ms > 40.0 => {
                "over 40ms is audible when monitoring yourself; lower the buffer"
            }
            Some(_) => "comfortable for live monitoring",
            None => "no device on either path",
        };
        c.left(f, y, note, theme::FAINT);
    }
}

/// One path's breakdown. Returns the row after it.
fn path(c: &mut Canvas, f: Field, mut y: u16, d: &Device) -> u16 {
    let rate = d.format.rate.max(1) as f32;
    let ms = |frames: u32| frames as f32 * 1000.0 / rate;

    c.left(f, y, &format!("{}  {}", d.name, d.transport.name()), theme::DIM);
    y += 1;

    let parts: [(&str, u32, bool); 4] = [
        ("buffer", d.buffer_frames, true),
        ("device latency", d.device_latency, false),
        ("safety offset", d.safety_offset, false),
        ("stream latency", d.stream_latency, false),
    ];
    // Longest bar in the panel scales the rest, so the eye can see which term
    // dominates without reading four numbers.
    let peak = parts.iter().map(|(_, v, _)| *v).max().unwrap_or(1).max(1);
    let bar_w = f.width().saturating_sub(46).clamp(6, 30);

    for (label, frames, yours) in parts {
        c.left(Field::at(f.x0 + 2, 16), y, label, theme::DIM);
        c.right(Field::at(f.x0 + 19, 8), y, &fmt::frames(frames), theme::FG);
        c.right(Field::at(f.x0 + 29, 9), y, &fmt::ms(ms(frames), 9), theme::FG);
        let filled = (frames as f32 / peak as f32 * bar_w as f32).round() as u16;
        // Only the buffer is yours to change; the rest is the hardware's, and
        // colouring them the same would imply otherwise.
        let fg = if yours { theme::CYAN } else { theme::FAINT };
        for i in 0..filled.min(bar_w) {
            c.set(f.x0 + 40 + i, y, c.g.blocks[7], fg);
        }
        y += 1;
    }

    let total_frames = d.buffer_frames + d.latency_frames;
    c.left(Field::at(f.x0 + 2, 16), y, "total", theme::DIM);
    c.right(Field::at(f.x0 + 19, 8), y, &fmt::frames(total_frames), theme::FG);
    c.right(Field::at(f.x0 + 29, 9), y, &fmt::ms(ms(total_frames), 9), theme::BR_WHITE);
    y += 1;

    if let Some((lo, hi)) = d.buffer_range {
        let hint = if d.buffer_frames <= TIGHT_BUFFER_FRAMES {
            format!("buffer range {lo}-{hi}fr \u{b7} already tight; xruns here mean raise it")
        } else {
            format!("buffer range {lo}-{hi}fr \u{b7} lower it to trade headroom for latency")
        };
        c.left(Field::new(f.x0 + 2, f.x1), y, &hint, theme::FAINT);
        y += 1;
    }
    y
}
