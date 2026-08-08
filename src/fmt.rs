//! Width-aware value formatting.
//!
//! Rule 3 of the grid contract: a right-aligned number is never truncated. These
//! helpers shed precision, then switch units, and finally give up and return
//! `--`. A number that has been sliced to fit its column is a wrong number, and
//! wrong numbers in a diagnostic tool are worse than absent ones.

/// The universal "this field has no value" marker.
pub const NA: &str = "--";

/// Sample rate: `48k`, `44.1k`, `192k`.
pub fn rate(hz: u32) -> String {
    if hz == 0 {
        return NA.to_string();
    }
    if hz.is_multiple_of(1000) {
        format!("{}k", hz / 1000)
    } else {
        let k = hz as f32 / 1000.0;
        let s = format!("{k:.1}");
        format!("{}k", s.trim_end_matches(".0"))
    }
}

/// Rate plus bit depth: `48k/24`. Bit depth of 0 means unknown.
pub fn rate_bits(hz: u32, bits: u8) -> String {
    if bits == 0 { rate(hz) } else { format!("{}/{}", rate(hz), bits) }
}

/// A signal level. Always one decimal, which is what the meter labels show.
pub fn dbfs(d: f32) -> String {
    if !d.is_finite() {
        return NA.to_string();
    }
    format!("{d:.1}")
}

/// Milliseconds, fitted into `max` display columns. Sheds precision, then
/// switches to seconds, then returns `--`.
pub fn ms(v: f32, max: u16) -> String {
    if !v.is_finite() || v < 0.0 {
        return NA.to_string();
    }
    let candidates = [
        format!("{v:.1}ms"),
        format!("{v:.0}ms"),
        format!("{:.1}s", v / 1000.0),
        format!("{:.0}s", v / 1000.0),
    ];
    for c in candidates {
        if c.chars().count() <= max as usize {
            return c;
        }
    }
    NA.to_string()
}

/// Buffer size in frames: `128fr`.
pub fn frames(f: u32) -> String {
    if f == 0 { NA.to_string() } else { format!("{f}fr") }
}

/// Hold duration for the mic audit: `00:42:18`.
pub fn hms(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// A format conversion, e.g. `44.1k→48k`. Returns `None` when nothing converts.
///
/// LITE.md encodes conversion as a rate-to-rate arrow only, so a same-rate
/// bit-depth truncation — 24-bit source into a 16-bit sink, which the full
/// tool calls out with `insight_bit_depth_downgrade` — renders as unremarkable
/// dim text. Bit depth is included here so that case is visible too.
pub fn conversion(from: (u32, u8), to: (u32, u8), arrow: &str, max: u16) -> Option<String> {
    let rate_changed = from.0 != to.0 && from.0 != 0 && to.0 != 0;
    let bits_dropped = from.1 > to.1 && to.1 != 0;
    if !rate_changed && !bits_dropped {
        return None;
    }
    // Most specific rendering that fits the column. A rate-to-rate arrow is only
    // offered when the rate actually changed — `48k→48k` would be a lie.
    let candidates: Vec<String> = match (rate_changed, bits_dropped) {
        (true, true) => vec![
            format!("{}{arrow}{}", rate_bits(from.0, from.1), rate_bits(to.0, to.1)),
            format!("{}{arrow}{}", rate(from.0), rate(to.0)),
        ],
        (true, false) => vec![format!("{}{arrow}{}", rate(from.0), rate(to.0))],
        (false, true) => vec![format!("{}{arrow}{}", from.1, to.1)],
        (false, false) => return None,
    };
    for c in candidates {
        if c.chars().count() <= max as usize {
            return Some(c);
        }
    }
    Some("conv".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_render_the_way_engineers_say_them() {
        assert_eq!(rate(48000), "48k");
        assert_eq!(rate(44100), "44.1k");
        assert_eq!(rate(192000), "192k");
        assert_eq!(rate(0), NA);
        assert_eq!(rate_bits(48000, 24), "48k/24");
        assert_eq!(rate_bits(48000, 0), "48k");
    }

    #[test]
    fn ms_never_returns_a_truncated_number() {
        // LAT column is 7 wide.
        assert_eq!(ms(11.8, 7), "11.8ms");
        assert_eq!(ms(112.4, 7), "112.4ms");
        // 1234.5ms cannot fit as milliseconds; it must not become "1234.5m".
        let v = ms(1234.5, 7);
        assert!(v.chars().count() <= 7, "{v} overflows");
        assert_eq!(v, "1234ms");
        let v = ms(123456.0, 7);
        assert!(v.chars().count() <= 7, "{v} overflows");
        assert_eq!(v, "123.5s");
        assert_eq!(ms(f32::NAN, 7), NA);
    }

    #[test]
    fn conversion_surfaces_bit_depth_not_just_rate() {
        // Rate change, the case the spec covers.
        assert_eq!(conversion((44100, 24), (48000, 24), "→", 10).as_deref(), Some("44.1k→48k"));
        // Same rate, 24 -> 16. The spec would render this as plain dim text.
        assert_eq!(conversion((48000, 24), (48000, 16), "→", 10).as_deref(), Some("24→16"));
        // Nothing is converting.
        assert_eq!(conversion((48000, 24), (48000, 24), "→", 10), None);
        // Upsampling bit depth is not a loss.
        assert_eq!(conversion((48000, 16), (48000, 24), "→", 10), None);
        // Whatever it picks must fit the 10-column RATE field.
        let c = conversion((44100, 24), (192000, 16), "→", 10).unwrap();
        assert!(c.chars().count() <= 10, "{c} overflows");
    }

    #[test]
    fn hold_duration() {
        assert_eq!(hms(2538), "00:42:18");
        assert_eq!(hms(0), "00:00:00");
        assert_eq!(hms(90061), "25:01:01");
    }
}
