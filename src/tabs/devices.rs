//! [2] Devices — every audio device on the machine, not just the two defaults.
//!
//! The Lite screen shows the current default output and input, which is the
//! right answer to "what am I hearing" and no help at all with "why did it
//! change" or "which of these three interfaces is the one with the problem".
//! This is the inventory: what exists, how it is attached, what it is running
//! at, and whether it is alive.

use crate::app::App;
use crate::fmt;
use crate::grid::{Canvas, Field};
use crate::layout::Layout;
use crate::model::Transport;
use crate::theme;

pub fn draw(c: &mut Canvas, l: &Layout, app: &App) {
    let top = l.body_top;
    let x0 = l.content.x0;
    let x1 = l.content.x1;

    // Columns sized from the terminal, with the name absorbing the slack —
    // real device names are the long ones and the numbers have fixed formats.
    // Capped as well as floored: a name column that eats forty columns of
    // slack pushes every number to the far edge and reads as two tables.
    let name_w = l.content.width().saturating_sub(56).clamp(18, 34);
    let f_name = Field::at(x0, name_w);
    let f_dir = Field::at(f_name.x1 + 2, 3);
    let f_transport = Field::at(f_dir.x1 + 2, 11);
    let f_rate = Field::at(f_transport.x1 + 2, 9);
    let f_ch = Field::at(f_rate.x1 + 2, 4);
    let f_buf = Field::at(f_ch.x1 + 2, 7);
    let f_lat = Field::at(f_buf.x1 + 2, 8);
    let f_state = Field::new(f_lat.x1 + 2, x1);

    super::table_head(
        c,
        l,
        top,
        &[
            (f_name, "DEVICE", false),
            (f_dir, "DIR", false),
            (f_transport, "TRANSPORT", false),
            (f_rate, "RATE", true),
            (f_ch, "CH", true),
            (f_buf, "BUFFER", true),
            (f_lat, "LATENCY", true),
            (f_state, "STATE", false),
        ],
    );

    if app.snap.devices.is_empty() {
        super::empty(c, l.content, top + 2, "no devices reported by the audio system");
        return;
    }

    // Windowed on `app.scroll`, which `clamp_selection` keeps the selection
    // inside. Drawing a fixed first-N window while the selection moved past it
    // left the highlight invisible and the list frozen.
    let rows = app.list_rows();
    for (i, d) in app.snap.devices.iter().skip(app.scroll).take(rows).enumerate() {
        let y = top + 2 + i as u16;
        let selected = app.scroll + i == app.sel;
        if selected {
            c.tint(l.content, y, theme::sel_bg());
        }

        let base = theme::direction(d.direction.is_input());
        // The default pair is what everything else on every other screen is
        // talking about, so it is the one thing marked here.
        if d.is_default {
            c.text(f_name, f_name.x0, y, c.g.dot, base, false);
            c.left(Field::new(f_name.x0 + 2, f_name.x1), y, &d.name, theme::fg());
        } else {
            c.left(Field::new(f_name.x0 + 2, f_name.x1), y, &d.name, theme::dim());
        }
        // The maker, when it fits and adds something the name does not. Two
        // interfaces from different vendors can carry the same model name.
        if selected
            && let Some(m) = d.manufacturer.as_deref()
            && !d.name.contains(m)
        {
            let used = 2 + crate::grid::width(&d.name);
            if used + 2 + crate::grid::width(m) <= f_name.width() {
                c.text(f_name, f_name.x0 + used + 2, y, m, theme::faint(), false);
            }
        }

        c.left(f_dir, y, if d.direction.is_input() { "in" } else { "out" }, base);

        // A lossy transport is the answer to "why does this sound wrong" often
        // enough to earn a colour of its own.
        let t_fg = if d.transport.is_lossy_path() { theme::yellow() } else { theme::dim() };
        c.left(f_transport, y, d.transport.name(), t_fg);

        c.right(f_rate, y, &fmt::rate_bits(d.format.rate, d.format.bits), theme::dim());
        c.right(f_ch, y, &d.format.channels.to_string(), theme::dim());
        c.right(f_buf, y, &fmt::frames(d.buffer_frames), theme::dim());
        c.right(
            f_lat,
            y,
            &d.latency_ms().map(|v| fmt::ms(v, 8)).unwrap_or(fmt::NA.into()),
            theme::dim(),
        );

        let (state, fg) = if !d.is_alive {
            ("dead", theme::red())
        } else if d.is_running {
            ("running", theme::green())
        } else {
            ("idle", theme::faint())
        };
        c.left(f_state, y, state, fg);
    }

    if app.snap.devices.len() > rows {
        let last = (app.scroll + rows).min(app.snap.devices.len());
        let range = format!("{}-{} of {}", app.scroll + 1, last, app.snap.devices.len());
        c.right(l.content, l.body_top + l.body_rows - 1, &range, theme::faint());
    }
}

/// A one-line summary for the Overview tab.
pub fn summary(app: &App) -> String {
    let n = app.snap.devices.len();
    let running = app.snap.devices.iter().filter(|d| d.is_running).count();
    let lossy = app.snap.devices.iter().filter(|d| d.transport.is_lossy_path()).count();
    if lossy > 0 {
        format!("{n} devices, {running} running, {lossy} on a lossy transport")
    } else {
        format!("{n} devices, {running} running")
    }
}

/// Devices whose transport degrades the signal, for Insights.
///
/// Running, not just default: something playing to a Bluetooth device is worth
/// saying whether or not it is the machine's default, and a headset that has
/// grabbed a stream while the default sits elsewhere is exactly the confusing
/// case worth naming.
pub fn lossy(app: &App) -> Vec<(&str, Transport)> {
    app.snap
        .devices
        .iter()
        .filter(|d| d.transport.is_lossy_path() && (d.is_default || d.is_running))
        .map(|d| (d.name.as_str(), d.transport))
        .collect()
}
