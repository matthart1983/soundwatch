//! Palette. Four accent colors, four neutrals, one selection tint.
//!
//! Tokens are inherited from the *Watch family without modification. The audio
//! semantics are additive: green is the playback path, cyan is the capture path,
//! yellow is "not broken, but you should know", red is clipping or dropouts.

use ratatui::style::Color;

pub const BG: Color = Color::Rgb(0x0c, 0x14, 0x18);
pub const FG: Color = Color::Rgb(0xc5, 0xd1, 0xd6);
pub const DIM: Color = Color::Rgb(0x6b, 0x80, 0x88);
pub const FAINT: Color = Color::Rgb(0x44, 0x56, 0x60);
pub const SEL_BG: Color = Color::Rgb(0x1a, 0x33, 0x40);

/// Output / playback path.
pub const GREEN: Color = Color::Rgb(0x5c, 0xd9, 0x89);
/// Input / capture path.
pub const CYAN: Color = Color::Rgb(0x5f, 0xdc, 0xff);
/// Above -6 dBFS, or a format conversion is happening.
pub const YELLOW: Color = Color::Rgb(0xf0, 0xc0, 0x60);
/// Clipping at 0 dBFS, or an xrun. See `RED_RULE` below.
pub const RED: Color = Color::Rgb(0xff, 0x78, 0x78);
/// The two headline dBFS numbers only.
pub const BR_WHITE: Color = Color::Rgb(0xff, 0xff, 0xff);

/// LITE.md calls red "RESERVED — clipping at 0 dBFS, or xruns/dropouts. Nothing
/// else", then paints `buffer`, `latency` and a stopped device red in the alert
/// state. Those cannot both hold. Resolved in favour of the broader rule, which
/// is what the alert state actually needs:
///
/// > red means the audio is wrong right now — clipping, a dropout, or a vital
/// > that is implicated in one.
///
/// Nothing decorative is ever red.
pub const RED_RULE: &str = "clipping, dropouts, or a vital implicated in one";

/// Direction color for a signal path.
pub fn direction(is_input: bool) -> Color {
    if is_input { CYAN } else { GREEN }
}

/// Per-column meter color by level. This is the family's one documented
/// deviation: SoundWatch is the only member whose chart color varies within a
/// single chart, because a clip is a discrete event you must be able to spot in
/// history and there is no spare row to report it on.
pub fn level(dbfs: f32, base: Color) -> Color {
    if dbfs >= -0.1 {
        RED
    } else if dbfs >= -6.0 {
        YELLOW
    } else {
        base
    }
}

// ── themes ───────────────────────────────────────────────────────────────────

/// How charts are coloured and drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    /// The handoff's palette, verbatim: four accent colours, one meaning each,
    /// and a chart column that is green (or cyan) until the signal is actually
    /// in trouble. Red is reserved.
    #[default]
    Spec,
    /// The `btop` look: every bar is a vertical gradient from its direction
    /// colour up through yellow and orange to red, and charts are drawn in
    /// braille for twice the horizontal resolution.
    ///
    /// Opt-in, because it breaks the palette's central rule on purpose. The
    /// spec says "red means the audio is wrong right now — nothing decorative
    /// is ever red", and a height gradient paints red for height alone. That is
    /// a real loss: under this theme a red-tipped bar no longer *means*
    /// anything, it is just tall. It is worth having anyway — the gradient
    /// makes level legible at a glance across a wide screen in a way a single
    /// flat colour does not — but it is not the default, and the alert colours
    /// on the header, the vitals and the verdict lines are left alone so the
    /// things that are genuinely wrong still announce themselves.
    Btop,
}

impl Theme {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "spec" | "default" => Some(Theme::Spec),
            "btop" => Some(Theme::Btop),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Theme::Spec => "spec",
            Theme::Btop => "btop",
        }
    }

    /// Does this theme draw charts in braille?
    pub fn braille(self) -> bool {
        self == Theme::Btop
    }
}

fn lerp(a: Color, b: Color, t: f32) -> Color {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return a;
    };
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

/// Warm end of the gradient, between yellow and red.
const ORANGE: Color = Color::Rgb(0xff, 0x9d, 0x4a);

/// The colour for a cell `t` of the way up a chart: `0.0` is the bottom row and
/// `1.0` is the top row.
///
/// The cool end is the direction colour, so green still reads as playback and
/// cyan as capture; only the hot end is new.
///
/// The stops spread across the whole chart rather than sitting at the spec's
/// dB thresholds, and that is a deliberate choice with a cost. Levels are
/// normalised in dB space, where -6 dBFS is already 90% of the way up, so a
/// ramp anchored to the meaning would leave every chart a single colour with a
/// hairline of yellow at the very top — a gradient that is not one. Spread out,
/// it makes level legible as colour across a wide screen, and means nothing
/// precise. That trade is the whole reason this theme is opt-in.
pub fn gradient(base: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match t {
        _ if t < 0.35 => base,
        _ if t < 0.70 => lerp(base, YELLOW, (t - 0.35) / 0.35),
        _ if t < 0.88 => lerp(YELLOW, ORANGE, (t - 0.70) / 0.18),
        _ => lerp(ORANGE, RED, (t - 0.88) / 0.12),
    }
}

/// Where a chart row sits on the gradient: bottom row `0.0`, top row `1.0`.
///
/// Anchoring on the row rather than its centre is what makes the ramp actually
/// reach both ends. With the centre of each row, a three-row meter samples
/// 0.17 / 0.50 / 0.83 and never reaches red at all — which is how the first
/// version of this shipped and looked, on a clipping fixture, merely orange.
pub fn row_height(row: u16, rows: u16) -> f32 {
    if rows <= 1 { 1.0 } else { row as f32 / (rows - 1) as f32 }
}

/// Colour for one chart cell under the active theme.
///
/// `level` is what the column is actually reading, and `height` is how far up
/// the chart this particular cell sits. The spec theme ignores the second: the
/// whole column is one colour, and that colour means something. The btop theme
/// ignores the first, which is the trade.
pub fn chart_cell(theme: Theme, base: Color, level_dbfs: f32, height: f32) -> Color {
    match theme {
        Theme::Spec => level(level_dbfs, base),
        Theme::Btop => gradient(base, height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: Color) -> (u8, u8, u8) {
        match c {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => panic!("not an rgb colour"),
        }
    }

    #[test]
    fn the_gradient_spans_the_whole_ramp() {
        assert_eq!(rgb(gradient(GREEN, 0.0)), rgb(GREEN), "the floor is not the base colour");
        assert_eq!(rgb(gradient(GREEN, 1.0)), rgb(RED), "the top of the chart never reaches red");
        // And it starts from whichever direction colour it was given.
        assert_eq!(rgb(gradient(CYAN, 0.0)), rgb(CYAN));
    }

    /// The failure this replaced: a three-row meter sampling row *centres*
    /// tops out at 0.83 and paints a clipping signal orange.
    #[test]
    fn even_a_three_row_meter_reaches_both_ends() {
        assert_eq!(row_height(0, 3), 0.0);
        assert_eq!(row_height(2, 3), 1.0);
        assert_eq!(rgb(gradient(GREEN, row_height(2, 3))), rgb(RED));
        // A single-row chart is all there is, so it gets the top of the ramp.
        assert_eq!(row_height(0, 1), 1.0);
    }

    #[test]
    fn the_gradient_only_gets_warmer() {
        // Red rises and blue falls, monotonically, all the way up.
        let mut prev = rgb(gradient(GREEN, 0.0));
        for i in 1..=100 {
            let c = rgb(gradient(GREEN, i as f32 / 100.0));
            assert!(c.0 >= prev.0, "red went backwards at {i}: {prev:?} -> {c:?}");
            prev = c;
        }
    }

    #[test]
    fn the_spec_theme_still_colours_by_meaning_not_height() {
        // Height is ignored, level is not.
        assert_eq!(rgb(chart_cell(Theme::Spec, GREEN, -30.0, 0.0)), rgb(GREEN));
        assert_eq!(rgb(chart_cell(Theme::Spec, GREEN, -30.0, 1.0)), rgb(GREEN));
        assert_eq!(rgb(chart_cell(Theme::Spec, GREEN, 0.0, 0.0)), rgb(RED));
        // And the btop theme does the opposite.
        assert_eq!(rgb(chart_cell(Theme::Btop, GREEN, 0.0, 0.0)), rgb(GREEN));
        assert_eq!(rgb(chart_cell(Theme::Btop, GREEN, -60.0, 1.0)), rgb(RED));
    }

    #[test]
    fn theme_names_round_trip() {
        for t in [Theme::Spec, Theme::Btop] {
            assert_eq!(Theme::parse(t.name()), Some(t));
        }
        assert_eq!(Theme::parse("nonsense"), None);
        assert_eq!(Theme::default(), Theme::Spec, "the spec look must stay the default");
        assert!(!Theme::Spec.braille(), "the default theme must not need braille");
    }
}
