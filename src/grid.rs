//! Character-grid drawing with an enforced clipping contract.
//!
//! The handoff's reference renderer draws with an unclipped `drawText` and pads
//! right-aligned fields with `padStart(n).slice(0, n)`, which keeps the *head* of
//! an over-long string — so `44.1k->48k/24` renders as `44.1k->48k/` and a long
//! device name silently overwrites its neighbour. Neither is survivable with real
//! data: PulseAudio ships device descriptions like
//! "Family 17h/19h HD Audio Controller Analog Stereo" (47 chars).
//!
//! The contract here:
//!
//! 1. Every write is bounded by a [`Field`]. Nothing can reach outside it.
//! 2. Left-aligned text truncates from the right (names are front-loaded).
//! 3. Right-aligned *values* are never truncated — they are re-formatted to fit
//!    by [`crate::fmt`], and if they still don't fit they render as `--`.
//!    A truncated number is a wrong number.
//! 4. Widths are display widths, not byte or char counts.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// An inclusive column span on a single row. All drawing is bounded by one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub x0: u16,
    pub x1: u16,
}

impl Field {
    pub const fn new(x0: u16, x1: u16) -> Self {
        Self { x0, x1 }
    }

    /// A field starting at `x0` that is `w` columns wide.
    pub const fn at(x0: u16, w: u16) -> Self {
        Self { x0, x1: x0 + w - 1 }
    }

    pub const fn width(&self) -> u16 {
        if self.x1 >= self.x0 { self.x1 - self.x0 + 1 } else { 0 }
    }

    /// Shrink from the right so this field ends `n` columns before `x`.
    pub fn ending_before(self, x: u16, gutter: u16) -> Self {
        let limit = x.saturating_sub(gutter);
        Self { x0: self.x0, x1: self.x1.min(limit.saturating_sub(1)) }
    }
}

/// Display width of a string, in terminal cells.
pub fn width(s: &str) -> u16 {
    UnicodeWidthStr::width(s) as u16
}

/// Truncate from the right to at most `max` display columns.
pub fn truncate(s: &str, max: u16) -> &str {
    if width(s) <= max {
        return s;
    }
    let mut used = 0u16;
    for (i, ch) in s.char_indices() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if used + w > max {
            return &s[..i];
        }
        used += w;
    }
    s
}

/// The glyph set. `⯈`/`⯇` (U+2BC8/U+2BC7) are the least universal characters in the
/// spec, and `●` (U+25CF) is East-Asian-Ambiguous — it renders double-width under
/// a fair number of terminal configurations, which would shift the DEVICE column
/// on every input row and break the one contract the whole Lite family rests on.
/// `--ascii` swaps the risky ones for guaranteed single-width stand-ins.
#[derive(Clone, Copy)]
pub struct Glyphs {
    pub out: &'static str,
    pub inp: &'static str,
    pub dot: &'static str,
    pub paused: &'static str,
    pub cursor: &'static str,
    pub rule: &'static str,
    pub corner: &'static str,
    pub arrow: &'static str,
    pub enter: &'static str,
    pub updown: &'static str,
    /// Marks the selected row in the settings menu.
    pub sel: &'static str,
    pub blocks: [char; 8],
    /// Box drawing for the help panel: horizontal, vertical, then the four
    /// corners clockwise from top-left.
    pub box_chars: [char; 6],
}

pub const UNICODE: Glyphs = Glyphs {
    // U+2BC8/U+2BC7 — black medium right/left-pointing triangle centred.
    out: "\u{2BC8}",
    inp: "\u{2BC7}",
    dot: "\u{25CF}",
    paused: "\u{25C6}",
    cursor: "\u{2588}",
    rule: "\u{2500}",
    corner: "\u{2514}\u{2500}",
    arrow: "\u{2192}",
    enter: "\u{21B5}",
    updown: "\u{2191}\u{2193}",
    sel: "\u{203a}",
    blocks: ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'],
    box_chars: ['─', '│', '┌', '┐', '┘', '└'],
};

pub const ASCII: Glyphs = Glyphs {
    out: ">",
    inp: "<",
    dot: "*",
    paused: "#",
    cursor: "_",
    rule: "-",
    corner: "\\-",
    arrow: ">",
    enter: "e",
    updown: "^v",
    sel: ">",
    blocks: ['_', '.', '.', '-', '-', '=', '=', '#'],
    box_chars: ['-', '|', '+', '+', '+', '+'],
};

/// The logical canvas, mapped onto the terminal buffer.
///
/// LITE.md fixes the grid at 80x24 with content in columns 1-78 and says nothing
/// about other terminal sizes. This used to letterbox: an 80x24 island in the
/// middle of whatever you actually had, which on a full-screen terminal wasted
/// most of the screen and made the meters — the whole point of the tool —
/// smaller than they needed to be.
///
/// The canvas now takes the whole terminal, and 80x24 is the *minimum* rather
/// than the size. Every column number the spec fixes is preserved exactly at 80
/// columns and grows from there; see [`crate::ui::Layout`]. Below the minimum
/// there is still an explicit message rather than a mangled layout.
pub struct Canvas<'a> {
    buf: &'a mut Buffer,
    ox: u16,
    oy: u16,
    w: u16,
    h: u16,
    pub g: Glyphs,
}

/// The smallest terminal that gets a layout instead of an apology.
pub const MIN_COLS: u16 = 80;
pub const MIN_ROWS: u16 = 24;

/// Content columns for a canvas `w` wide: 1..=w-2, leaving the spec's one-column
/// margin on each side. At w=80 this is the spec's `1..=78`, exactly.
pub const fn content(w: u16) -> Field {
    Field::new(1, w.saturating_sub(2))
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut Buffer, ox: u16, oy: u16, w: u16, h: u16, g: Glyphs) -> Self {
        Self { buf, ox, oy, w, h, g }
    }

    fn cell(&mut self, x: u16, y: u16) -> Option<&mut ratatui::buffer::Cell> {
        if x >= self.w || y >= self.h {
            return None;
        }
        self.buf.cell_mut((self.ox + x, self.oy + y))
    }

    pub fn set(&mut self, x: u16, y: u16, ch: char, fg: Color) {
        if let Some(c) = self.cell(x, y) {
            c.set_char(ch);
            c.set_fg(fg);
        }
    }

    pub fn set_bold(&mut self, x: u16, y: u16, ch: char, fg: Color) {
        if let Some(c) = self.cell(x, y) {
            c.set_char(ch);
            c.set_fg(fg);
            c.set_style(Style::default().add_modifier(Modifier::BOLD));
        }
    }

    /// Write `s` starting at `x`, clipped to `f`. Returns the next free column.
    pub fn text(&mut self, f: Field, x: u16, y: u16, s: &str, fg: Color, bold: bool) -> u16 {
        if x > f.x1 {
            return x;
        }
        let room = f.x1 - x + 1;
        let s = truncate(s, room);
        let mut cx = x;
        for ch in s.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
            if w == 0 {
                continue;
            }
            if cx > f.x1 {
                break;
            }
            if bold {
                self.set_bold(cx, y, ch, fg);
            } else {
                self.set(cx, y, ch, fg);
            }
            cx += w;
        }
        cx
    }

    /// Left-aligned within `f`, truncated from the right.
    pub fn left(&mut self, f: Field, y: u16, s: &str, fg: Color) -> u16 {
        self.text(f, f.x0, y, s, fg, false)
    }

    /// Right-aligned within `f`. Over-long input is truncated from the *right*
    /// and pushed flush-left in the field — it never spills past `f.x0`.
    /// Numeric values should be pre-fitted by [`crate::fmt`] so this never bites.
    pub fn right(&mut self, f: Field, y: u16, s: &str, fg: Color) -> u16 {
        let s = truncate(s, f.width());
        let x = f.x1 + 1 - width(s).min(f.width());
        self.text(f, x, y, s, fg, false)
    }

    pub fn right_bold(&mut self, f: Field, y: u16, s: &str, fg: Color) -> u16 {
        let s = truncate(s, f.width());
        let x = f.x1 + 1 - width(s).min(f.width());
        self.text(f, x, y, s, fg, true)
    }

    /// Horizontal rule across the whole field.
    pub fn rule(&mut self, f: Field, y: u16, fg: Color) {
        let ch = self.g.rule.chars().next().unwrap_or('-');
        for x in f.x0..=f.x1 {
            self.set(x, y, ch, fg);
        }
    }

    /// Background tint across a field on one row, preserving glyphs.
    pub fn tint(&mut self, f: Field, y: u16, bg: Color) {
        for x in f.x0..=f.x1 {
            if let Some(c) = self.cell(x, y) {
                c.set_bg(bg);
            }
        }
    }

    /// An opaque bordered panel. Clears its interior so whatever it covers does
    /// not bleed through — an overlay that lets the meters show between its
    /// lines is unreadable.
    pub fn panel(&mut self, f: Field, y0: u16, y1: u16, border: Color, bg: Color) {
        let [h, v, tl, tr, br, bl] = self.g.box_chars;
        for y in y0..=y1 {
            for x in f.x0..=f.x1 {
                self.set(x, y, ' ', crate::theme::FG);
            }
            self.tint(f, y, bg);
        }
        for x in f.x0..=f.x1 {
            self.set(x, y0, h, border);
            self.set(x, y1, h, border);
        }
        for y in y0..=y1 {
            self.set(f.x0, y, v, border);
            self.set(f.x1, y, v, border);
        }
        self.set(f.x0, y0, tl, border);
        self.set(f.x1, y0, tr, border);
        self.set(f.x1, y1, br, border);
        self.set(f.x0, y1, bl, border);
    }

    /// Fill the whole canvas with the background color.
    pub fn clear(&mut self, bg: Color) {
        for y in 0..self.h {
            for x in 0..self.w {
                if let Some(c) = self.cell(x, y) {
                    c.set_char(' ');
                    c.set_bg(bg);
                    c.set_fg(crate::theme::FG);
                    c.set_style(Style::default());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_width_and_bounds() {
        assert_eq!(Field::at(70, 9).x1, 78);
        assert_eq!(Field::new(1, 78).width(), 78);
        assert_eq!(Field::new(5, 4).width(), 0);
    }

    #[test]
    fn truncate_is_display_width_aware() {
        assert_eq!(truncate("MacBook Speakers", 7), "MacBook");
        // A 47-char real-world PulseAudio description must fit its field.
        let long = "Family 17h/19h HD Audio Controller Analog Stereo";
        assert_eq!(width(truncate(long, 22)), 22);
        // Double-width characters never half-render.
        assert_eq!(truncate("日本語オーディオ", 5), "日本");
        assert!(width(truncate("日本語オーディオ", 5)) <= 5);
    }

    #[test]
    fn ending_before_reserves_a_gutter() {
        let f = Field::new(1, 78).ending_before(52, 2);
        assert_eq!(f.x1, 49);
    }
}
