//! The full product's ten tabs, and the chrome around them.
//!
//! Laid out to `design_handoff_syswatch`'s `sw-chrome.jsx`, which is the
//! family's full-product pattern: header on row 0, tab bar on row 1 with the
//! active tab *inset* into the rule on row 2, body, a rule, and a footer of
//! grouped hotkeys. There is no `design_handoff_soundwatch` — the tab list is
//! derived from the family's taxonomy (Overview first, Insights last, Timeline
//! next to it) applied to the audio domain, and is marked as derived rather
//! than passed off as specified.
//!
//! Each tab has to be honest about what CoreAudio will not tell it. A panel
//! with nothing in it is indistinguishable from a panel that is broken, so
//! every one of these says which it is.

use ratatui::style::Color;

use crate::app::App;
use crate::grid::{Canvas, Field, width};
use crate::layout::Layout;
use crate::theme;

mod devices;
mod insights;
mod latency;
mod meters;
mod overview;
mod routing;
mod streams;
mod timeline;
mod xruns;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Overview,
    Devices,
    Streams,
    Meters,
    Spectrum,
    Latency,
    Xruns,
    Routing,
    Timeline,
    Insights,
}

impl Tab {
    pub const ALL: [Tab; 10] = [
        Tab::Overview,
        Tab::Devices,
        Tab::Streams,
        Tab::Meters,
        Tab::Spectrum,
        Tab::Latency,
        Tab::Xruns,
        Tab::Routing,
        Tab::Timeline,
        Tab::Insights,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Devices => "Devices",
            Tab::Streams => "Streams",
            Tab::Meters => "Meters",
            Tab::Spectrum => "Spectrum",
            Tab::Latency => "Latency",
            Tab::Xruns => "Xruns",
            Tab::Routing => "Routing",
            Tab::Timeline => "Timeline",
            Tab::Insights => "Insights",
        }
    }

    /// The digit that selects it. Ten tabs, so the tenth is `0` — the same
    /// wrap the family uses when it runs past nine.
    pub fn digit(self) -> char {
        let i = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0);
        if i == 9 { '0' } else { (b'1' + i as u8) as char }
    }

    pub fn from_digit(c: char) -> Option<Tab> {
        let i = match c {
            '0' => 9,
            '1'..='9' => c as usize - '1' as usize,
            _ => return None,
        };
        Tab::ALL.get(i).copied()
    }

    pub fn next(self, delta: i32) -> Tab {
        let n = Tab::ALL.len() as i32;
        let i = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0) as i32;
        Tab::ALL[(((i + delta) % n + n) % n) as usize]
    }

    /// A count to show beside the tab name, when one is meaningful. `None`
    /// leaves the label bare rather than showing a zero, which reads as an
    /// error rather than as an absence.
    pub fn badge(self, app: &App) -> Option<usize> {
        match self {
            Tab::Streams => Some(app.snap.streams.len()),
            Tab::Devices => Some(app.snap.devices.len()),
            Tab::Xruns => (app.snap.xruns_60s > 0).then_some(app.snap.xruns_60s as usize),
            Tab::Insights => {
                let n = insights::count(app);
                (n > 0).then_some(n)
            }
            _ => None,
        }
    }
}

/// How much of each tab's label the bar has room for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BarStyle {
    /// `[2] Devices 6` — name and count, a space either side.
    Full,
    /// `[2] Devices` — the count dropped.
    Names,
    /// The same, packed to a single space between tabs.
    Packed,
    /// `[2]`, with only the active tab keeping its name.
    Digits,
}

impl BarStyle {
    fn pad(self) -> u16 {
        if self == BarStyle::Full || self == BarStyle::Names { 2 } else { 1 }
    }

    fn label(self, t: Tab, app: &App, active: bool) -> String {
        match self {
            BarStyle::Full => match t.badge(app) {
                Some(n) => format!("[{}] {} {n}", t.digit(), t.name()),
                None => format!("[{}] {}", t.digit(), t.name()),
            },
            BarStyle::Names | BarStyle::Packed => format!("[{}] {}", t.digit(), t.name()),
            BarStyle::Digits if active => format!("[{}] {}", t.digit(), t.name()),
            BarStyle::Digits => format!("[{}]", t.digit()),
        }
    }

    fn width_for(self, app: &App) -> u16 {
        Tab::ALL.iter().map(|t| width(&self.label(*t, app, *t == app.tab)) + self.pad()).sum()
    }
}

/// Draw the tab bar and inset the active tab into the rule beneath it.
///
/// The label degrades in stages rather than collapsing straight to digits: ten
/// bare numbers tell you how many tabs exist and nothing about what is in them,
/// and the tab bar is the only place that information appears at all. Our names
/// are longer than the family's (`Spectrum`, `Timeline` against `CPU`, `FS`), so
/// even at the reference 130 columns the full form does not quite fit — hence
/// the middle steps.
pub fn bar(c: &mut Canvas, l: &Layout, app: &App) {
    c.rule(Field::new(0, l.w - 1), l.row_tab_rule, theme::FAINT);

    let style = [BarStyle::Full, BarStyle::Names, BarStyle::Packed]
        .into_iter()
        .find(|s| s.width_for(app) <= l.w)
        .unwrap_or(BarStyle::Digits);

    let mut x = 0u16;
    for t in Tab::ALL {
        let active = t == app.tab;
        let label = style.label(t, app, active);
        let w = width(&label) + style.pad();
        if x + w > l.w {
            break;
        }
        let field = Field::new(x, x + w - 1);
        let indent = if style.pad() == 2 { 1 } else { 0 };
        if active {
            c.tint(field, l.row_tab_bar, theme::BG);
            c.text(field, x + indent, l.row_tab_bar, &label, theme::CYAN, true);
            // Inset: clear the rule under the active tab and turn the corners
            // up into it, so the tab reads as continuous with the body below.
            for i in 0..w {
                c.set(x + i, l.row_tab_rule, ' ', theme::BG);
            }
            let [_, _, _, _, br, bl] = c.g.box_chars;
            c.set(x, l.row_tab_rule, br, theme::FAINT);
            c.set(x + w - 1, l.row_tab_rule, bl, theme::FAINT);
        } else {
            let digits = format!("[{}]", t.digit());
            let dx = c.text(field, x + indent, l.row_tab_bar, &digits, theme::DIM, false);
            if style != BarStyle::Digits {
                let nx = c.text(field, dx + 1, l.row_tab_bar, t.name(), theme::FG, false);
                if style == BarStyle::Full
                    && let Some(n) = t.badge(app)
                {
                    c.text(field, nx, l.row_tab_bar, &format!(" {n}"), theme::DIM, false);
                }
            }
        }
        x += w;
    }
}

/// Dispatch to the active tab.
pub fn body(c: &mut Canvas, l: &Layout, app: &App) {
    match app.tab {
        Tab::Overview => overview::draw(c, l, app),
        Tab::Devices => devices::draw(c, l, app),
        Tab::Streams => streams::draw(c, l, app),
        Tab::Meters => meters::draw(c, l, app),
        Tab::Spectrum => crate::ui::spectrum_body(c, l, app),
        Tab::Latency => latency::draw(c, l, app),
        Tab::Xruns => xruns::draw(c, l, app),
        Tab::Routing => routing::draw(c, l, app),
        Tab::Timeline => timeline::draw(c, l, app),
        Tab::Insights => insights::draw(c, l, app),
    }
}

// ── shared furniture ─────────────────────────────────────────────────────────

/// A titled section rule: `── title ──────────`.
pub fn section(c: &mut Canvas, f: Field, y: u16, title: &str) {
    c.rule(f, y, theme::FAINT);
    c.text(f, f.x0 + 1, y, &format!(" {title} "), theme::DIM, true);
}

/// A KPI tile: a big value with a caption under it, in a fixed-width column.
///
/// The family's Overview leads with a row of these because the first question
/// is always "is anything wrong", and a number you have to read a table to
/// find is a number you did not read.
pub fn tile(c: &mut Canvas, f: Field, y: u16, caption: &str, value: &str, fg: Color) {
    c.text(f, f.x0, y, caption, theme::DIM, false);
    c.text(f, f.x0, y + 1, value, fg, true);
}

/// Header of a table: dim, bold, with a rule under it.
pub fn table_head(c: &mut Canvas, l: &Layout, y: u16, cols: &[(Field, &str, bool)]) {
    for (f, name, right) in cols {
        if *right {
            c.right(*f, y, name, theme::DIM);
        } else {
            c.left(*f, y, name, theme::DIM);
        }
    }
    c.rule(l.content, y + 1, theme::FAINT);
}

/// What to say when a panel has nothing to show, and why.
pub fn empty(c: &mut Canvas, f: Field, y: u16, msg: &str) {
    c.left(f, y, msg, theme::FAINT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tab_has_a_distinct_digit_that_round_trips() {
        let mut seen = std::collections::HashSet::new();
        for t in Tab::ALL {
            let d = t.digit();
            assert!(seen.insert(d), "{:?} reuses digit {d}", t);
            assert_eq!(Tab::from_digit(d), Some(t), "digit {d} does not select {:?}", t);
        }
        assert_eq!(seen.len(), 10);
        // Ten tabs run past nine, so the tenth is 0.
        assert_eq!(Tab::Insights.digit(), '0');
        assert_eq!(Tab::from_digit('x'), None);
    }

    #[test]
    fn tabs_cycle_in_both_directions_and_wrap() {
        assert_eq!(Tab::Overview.next(-1), Tab::Insights, "left from the first must wrap");
        assert_eq!(Tab::Insights.next(1), Tab::Overview, "right from the last must wrap");
        let mut t = Tab::Overview;
        for _ in 0..Tab::ALL.len() {
            t = t.next(1);
        }
        assert_eq!(t, Tab::Overview, "a full cycle does not return");
    }

    #[test]
    fn every_tab_is_named() {
        for t in Tab::ALL {
            assert!(!t.name().is_empty());
            // Names share the bar with nine others; long ones crowd it out.
            assert!(t.name().len() <= 9, "{:?} has a long name", t);
        }
    }
}
