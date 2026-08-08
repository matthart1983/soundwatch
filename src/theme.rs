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
