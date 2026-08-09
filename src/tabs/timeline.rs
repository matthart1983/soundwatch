//! [9] Timeline — what changed, and when.
//!
//! The hard audio faults are the ones where something changed and nobody saw
//! it: a device woke and stole the default, a format shifted under a running
//! stream, an interface dropped off the bus for a second and came back. No
//! snapshot can show any of that. Only the difference between two of them can,
//! which is what the backend records and this displays.

use crate::app::App;
use crate::grid::{Canvas, Field};
use crate::layout::Layout;
use crate::theme;

pub fn draw(c: &mut Canvas, l: &Layout, app: &App) {
    let f = l.content;
    let mut y = l.body_top;

    super::section(c, f, y, "this session");
    y += 1;

    if app.snap.events.is_empty() {
        super::empty(c, f, y, "nothing has changed since the tool started");
        return;
    }

    let f_when = Field::at(f.x0, 9);
    let f_kind = Field::at(f_when.x1 + 2, 8);
    let f_what = Field::new(f_kind.x1 + 2, f.x1);
    super::table_head(
        c,
        l,
        y,
        &[(f_when, "AGO", true), (f_kind, "EVENT", false), (f_what, "WHAT", false)],
    );
    y += 2;

    let rows = (l.body_top + l.body_rows).saturating_sub(y) as usize;
    // Newest first. A log you have to scroll to the bottom of to see the most
    // recent thing is a log nobody reads. `scroll` walks back through it.
    for (i, e) in app.snap.events.iter().rev().skip(app.scroll).take(rows).enumerate() {
        let row = y + i as u16;
        let secs = app.snap.at.secs_since(e.at);
        let ago = if secs < 60 {
            format!("{secs}s")
        } else if secs < 3600 {
            format!("{}m{}s", secs / 60, secs % 60)
        } else {
            format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
        };
        c.right(f_when, row, &ago, theme::FAINT);
        let fg = if e.kind.is_fault() { theme::RED } else { theme::CYAN };
        c.left(f_kind, row, e.kind.label(), fg);
        c.left(f_what, row, &e.what, theme::FG);
    }

    if app.snap.events.len() > rows + app.scroll {
        c.right(
            f,
            l.body_top + l.body_rows - 1,
            &format!("{} older", app.snap.events.len() - rows - app.scroll),
            theme::FAINT,
        );
    }
}
