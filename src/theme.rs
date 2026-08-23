//! Palette. Four accent colors, four neutrals, one selection tint.
//!
//! Tokens are inherited from the *Watch family without modification. The audio
//! semantics are additive: green is the playback path, cyan is the capture path,
//! yellow is "not broken, but you should know", red is clipping or dropouts.
//!
//! Two independent axes, deliberately kept apart:
//!
//! - **Palette** (`--palette`) is *which colours*. `terminal` is the default and
//!   pins none of its own: every slot resolves to an ANSI entry and
//!   foreground/background use `Reset`, so a terminal profile, pywal, matugen or
//!   a system-wide rice carries straight through. `spec` is the handoff's fixed
//!   hexes.
//! - **Theme** (`--theme`) is *how charts are drawn* — flat columns coloured by
//!   meaning, or a btop-style height gradient in braille.
//!
//! They compose: `--palette spec --theme btop` is the original btop look.

use ratatui::style::Color;
use std::sync::RwLock;

/// One set of colour slots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub name: &'static str,
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    pub faint: Color,
    pub sel_bg: Color,
    /// Output / playback path.
    pub green: Color,
    /// Input / capture path.
    pub cyan: Color,
    /// Above -6 dBFS, or a format conversion is happening.
    pub yellow: Color,
    /// Clipping at 0 dBFS, or an xrun. See `RED_RULE` below.
    pub red: Color,
    /// Warm end of the btop gradient, between yellow and red.
    pub orange: Color,
    /// The two headline dBFS numbers only.
    pub br_white: Color,
}

/// The design handoff's palette, verbatim. Byte-identical to the hexes that
/// used to be consts here — do not "tidy" them.
pub const fn spec() -> Palette {
    Palette {
        name: "spec",
        bg: Color::Rgb(0x0c, 0x14, 0x18),
        fg: Color::Rgb(0xc5, 0xd1, 0xd6),
        dim: Color::Rgb(0x6b, 0x80, 0x88),
        faint: Color::Rgb(0x44, 0x56, 0x60),
        sel_bg: Color::Rgb(0x1a, 0x33, 0x40),
        green: Color::Rgb(0x5c, 0xd9, 0x89),
        cyan: Color::Rgb(0x5f, 0xdc, 0xff),
        yellow: Color::Rgb(0xf0, 0xc0, 0x60),
        red: Color::Rgb(0xff, 0x78, 0x78),
        orange: Color::Rgb(0xff, 0x9d, 0x4a),
        br_white: Color::Rgb(0xff, 0xff, 0xff),
    }
}

/// Defers entirely to the terminal's own palette: ANSI slots 0-15 for colour
/// and `Color::Reset` for foreground and background. Nothing here is a fixed
/// RGB value — that is the whole point.
///
/// Two compromises the 16-colour palette forces:
///
/// - `orange` has no ANSI slot. It takes bright red, the only entry warmer than
///   yellow and cooler than red, so the btop gradient still climbs in the right
///   direction. It is a step, not a blend.
/// - `sel_bg` uses `Indexed(8)` (bright black), the one slot conventionally
///   rendered as a neutral mid-grey in both light and dark themes. A terminal
///   theme that maps slot 8 close to its background shows a faint selection
///   bar; that is a property of that theme, and a saturated slot is worse
///   everywhere else.
pub const fn terminal() -> Palette {
    Palette {
        name: "terminal",
        bg: Color::Reset,
        fg: Color::Reset,
        dim: Color::Gray,
        faint: Color::DarkGray,
        sel_bg: Color::Indexed(8),
        green: Color::Green,
        cyan: Color::Cyan,
        yellow: Color::Yellow,
        red: Color::Red,
        orange: Color::LightRed,
        br_white: Color::White,
    }
}

/// What soundwatch uses when the user has not asked for another palette.
/// `Config::default` and the `ACTIVE` initialiser both go through this, so
/// there is exactly one place that decides it.
pub const fn default_palette() -> Palette {
    terminal()
}

pub const PALETTE_NAMES: &[&str] = &["terminal", "spec"];

pub fn palette_by_name(name: &str) -> Option<Palette> {
    match name {
        // "system" and "ansi" are what users coming from other TUIs reach for.
        "terminal" | "system" | "ansi" => Some(terminal()),
        "spec" | "default" => Some(spec()),
        _ => None,
    }
}

/// Initialised through [`default_palette`], the same path `Config::default`
/// takes, so a fresh run and an unconfigured one cannot disagree.
static ACTIVE: RwLock<Palette> = RwLock::new(default_palette());

pub fn active() -> Palette {
    *ACTIVE.read().expect("palette lock poisoned")
}

pub fn set_palette(p: Palette) {
    *ACTIVE.write().expect("palette lock poisoned") = p;
}

// ── slot accessors ───────────────────────────────────────────────────────────
// Functions rather than consts purely so the values can vary at runtime.

pub fn bg() -> Color {
    active().bg
}
pub fn fg() -> Color {
    active().fg
}
pub fn dim() -> Color {
    active().dim
}
pub fn faint() -> Color {
    active().faint
}
pub fn sel_bg() -> Color {
    active().sel_bg
}
pub fn green() -> Color {
    active().green
}
pub fn cyan() -> Color {
    active().cyan
}
pub fn yellow() -> Color {
    active().yellow
}
pub fn red() -> Color {
    active().red
}
pub fn orange() -> Color {
    active().orange
}
pub fn br_white() -> Color {
    active().br_white
}

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
    if is_input { cyan() } else { green() }
}

/// Per-column meter color by level. This is the family's one documented
/// deviation: SoundWatch is the only member whose chart color varies within a
/// single chart, because a clip is a discrete event you must be able to spot in
/// history and there is no spare row to report it on.
pub fn level(dbfs: f32, base: Color) -> Color {
    if dbfs >= -0.1 {
        red()
    } else if dbfs >= -6.0 {
        yellow()
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

    // A 16-colour palette has no intermediate shades to blend through, so on
    // one the ramp becomes four discrete steps rather than silently collapsing
    // to a single flat colour — which is what falling through to `lerp` would
    // do, and which looks identical to the gradient being switched off.
    if !matches!(base, Color::Rgb(..)) {
        return match t {
            _ if t < 0.50 => base,
            _ if t < 0.75 => yellow(),
            _ if t < 0.90 => orange(),
            _ => red(),
        };
    }

    match t {
        _ if t < 0.35 => base,
        _ if t < 0.70 => lerp(base, yellow(), (t - 0.35) / 0.35),
        _ if t < 0.88 => lerp(yellow(), orange(), (t - 0.70) / 0.18),
        _ => lerp(orange(), red(), (t - 0.88) / 0.12),
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

/// Serialises tests that install a palette. Without it, `cargo test`'s thread
/// pool lets one test's `set_palette` leak into another's assertions — and
/// since the default is now `terminal`, a test that needs RGB to measure has
/// to pin `spec` explicitly rather than inherit it.
#[cfg(test)]
static ACTIVE_PALETTE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Install a palette for the duration of one test, serialised against the
/// others. The guard holds the lock until the test ends.
#[cfg(test)]
pub(crate) fn exclusive_palette(p: Palette) -> std::sync::MutexGuard<'static, ()> {
    // A panic in one such test poisons the mutex; that shouldn't cascade into
    // failures in the others, so recover the guard either way.
    let guard = ACTIVE_PALETTE_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    set_palette(p);
    guard
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
        let _g = exclusive_palette(spec());
        let (green, red, cyan) = (spec().green, spec().red, spec().cyan);
        assert_eq!(rgb(gradient(green, 0.0)), rgb(green), "the floor is not the base colour");
        assert_eq!(rgb(gradient(green, 1.0)), rgb(red), "the top of the chart never reaches red");
        // And it starts from whichever direction colour it was given.
        assert_eq!(rgb(gradient(cyan, 0.0)), rgb(cyan));
    }

    /// The failure this replaced: a three-row meter sampling row *centres*
    /// tops out at 0.83 and paints a clipping signal orange.
    #[test]
    fn even_a_three_row_meter_reaches_both_ends() {
        let _g = exclusive_palette(spec());
        assert_eq!(row_height(0, 3), 0.0);
        assert_eq!(row_height(2, 3), 1.0);
        assert_eq!(rgb(gradient(spec().green, row_height(2, 3))), rgb(spec().red));
        // A single-row chart is all there is, so it gets the top of the ramp.
        assert_eq!(row_height(0, 1), 1.0);
    }

    #[test]
    fn the_gradient_only_gets_warmer() {
        let _g = exclusive_palette(spec());
        // Red rises and blue falls, monotonically, all the way up.
        let mut prev = rgb(gradient(spec().green, 0.0));
        for i in 1..=100 {
            let c = rgb(gradient(spec().green, i as f32 / 100.0));
            assert!(c.0 >= prev.0, "red went backwards at {i}: {prev:?} -> {c:?}");
            prev = c;
        }
    }

    #[test]
    fn the_spec_theme_still_colours_by_meaning_not_height() {
        let _g = exclusive_palette(spec());
        let (green, red) = (spec().green, spec().red);
        // Height is ignored, level is not.
        assert_eq!(rgb(chart_cell(Theme::Spec, green, -30.0, 0.0)), rgb(green));
        assert_eq!(rgb(chart_cell(Theme::Spec, green, -30.0, 1.0)), rgb(green));
        assert_eq!(rgb(chart_cell(Theme::Spec, green, 0.0, 0.0)), rgb(red));
        // And the btop theme does the opposite.
        assert_eq!(rgb(chart_cell(Theme::Btop, green, 0.0, 0.0)), rgb(green));
        assert_eq!(rgb(chart_cell(Theme::Btop, green, -60.0, 1.0)), rgb(red));
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

    // ── the terminal palette ─────────────────────────────────────────────────

    #[test]
    fn the_terminal_palette_pins_no_rgb() {
        // Its whole contract is that soundwatch defines no colours of its own.
        // Debug-formatting the struct checks every slot at once, so a slot
        // added later cannot quietly acquire a fixed colour without failing.
        let rendered = format!("{:?}", terminal());
        assert!(!rendered.contains("Rgb"), "terminal palette pinned RGB: {rendered}");
    }

    #[test]
    fn the_terminal_palette_never_paints_a_background() {
        assert_eq!(terminal().bg, Color::Reset);
        assert_eq!(terminal().fg, Color::Reset);
    }

    #[test]
    fn the_gradient_degrades_to_steps_rather_than_leaking_rgb() {
        // A 16-colour palette has no shades to blend, so the ramp becomes
        // discrete — but it must still climb, and must still reach red.
        let _g = exclusive_palette(terminal());
        let base = terminal().green;
        for t in [0.0, 0.25, 0.5, 0.6, 0.8, 0.95, 1.0] {
            assert!(
                !matches!(gradient(base, t), Color::Rgb(..)),
                "gradient at {t} leaked RGB under the terminal palette"
            );
        }
        assert_eq!(gradient(base, 0.0), base, "the floor is not the base colour");
        assert_eq!(gradient(base, 1.0), terminal().red, "the ramp never reaches red");
    }

    #[test]
    fn level_and_direction_follow_the_active_palette() {
        let _g = exclusive_palette(terminal());
        assert_eq!(direction(true), Color::Cyan);
        assert_eq!(direction(false), Color::Green);
        // Clipping is still red, whichever palette is installed.
        assert_eq!(level(0.0, terminal().green), Color::Red);
        assert_eq!(level(-3.0, terminal().green), Color::Yellow);
        assert_eq!(level(-30.0, terminal().green), Color::Green);
    }

    #[test]
    fn palette_names_round_trip_and_accept_aliases() {
        for name in PALETTE_NAMES {
            assert_eq!(palette_by_name(name).map(|p| p.name), Some(*name));
        }
        for alias in ["system", "ansi"] {
            assert_eq!(palette_by_name(alias).map(|p| p.name), Some("terminal"));
        }
        assert_eq!(palette_by_name("default").map(|p| p.name), Some("spec"));
        assert!(palette_by_name("nonsense").is_none());
    }

    #[test]
    fn the_default_defers_to_the_terminal() {
        // There is one decision point; Config::default and the ACTIVE
        // initialiser both route through it.
        assert_eq!(default_palette().name, "terminal");
        assert_eq!(default_palette(), terminal());
    }
}
