//! Everything the settings menu can change, and where it is remembered.
//!
//! The values here were constants until there was a menu, and most of them
//! still read as constants everywhere else — `Config::default()` is exactly the
//! spec's numbers, so a fresh install behaves identically to the version that
//! had no configuration at all. That is the property to preserve: a setting is
//! somewhere to *depart* from the design, not a substitute for having one.
//!
//! ## Why a hand-rolled file format
//!
//! `key = value`, one per line, `#` for comments. It is a `.toml` file only in
//! the sense that TOML would parse it. Taking `serde` and `toml` for nine
//! scalars would roughly double the dependency tree of a program that hand-rolls
//! its CoreAudio bindings and its FFT to avoid exactly that.
//!
//! The parser is deliberately forgiving: an unknown key, a malformed value or a
//! value out of range is skipped and the default kept, and the rest of the file
//! still loads. A diagnostic tool that refuses to start because its config file
//! has a typo has failed at the one moment you needed it.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::theme::Theme;

/// Analysis sizes offered by the menu. Powers of two, because the transform is
/// radix-2, and bounded because the ring holds 16384 samples.
pub const FFT_SIZES: [usize; 4] = [1024, 2048, 4096, 8192];
/// Refresh rates. Above 30 the terminal is the bottleneck, not the audio.
pub const REFRESH_RATES: [u64; 4] = [10, 20, 30, 60];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub theme: Theme,
    /// ASCII stand-ins for glyphs that are not reliably single-width.
    pub ascii: bool,
    /// Bottom of the meters' scale, in dBFS.
    pub meter_floor: f32,
    /// Bottom of the spectrum's scale, in dBFS. Lower than the meters' because
    /// broadband energy divides across bins; see [`crate::dsp::FLOOR_DBFS`].
    pub spectrum_floor: f32,
    /// Transform size. Bigger is finer in frequency and blurrier in time.
    pub fft_size: usize,
    /// Spectrum bar fall, in dB per second.
    pub spectrum_decay: f32,
    /// Peak marker hold, in seconds. Zero switches the markers off.
    pub peak_hold: f32,
    /// Xruns in 60 seconds before the header goes red.
    pub xrun_alert: u32,
    /// Meter the default input device. Opens a capture stream, so this process
    /// then appears in the mic-in-use audit it exists to report.
    pub meter_input: bool,
    /// Display refresh, in frames per second.
    pub refresh_hz: u64,
}

impl Default for Config {
    /// The spec's numbers, exactly. A fresh install must look and behave like
    /// the version that had no settings menu.
    fn default() -> Self {
        // Taken from the constants rather than restated, so there is one place
        // each of these numbers lives and the tests below cannot drift from the
        // code they are pinning.
        Self {
            theme: Theme::Spec,
            ascii: false,
            meter_floor: crate::meter::FLOOR_DBFS,
            spectrum_floor: crate::dsp::FLOOR_DBFS,
            fft_size: crate::dsp::FFT_SIZE,
            spectrum_decay: crate::spectrum::DECAY_DB_PER_SEC,
            peak_hold: crate::spectrum::PEAK_HOLD_SECS,
            xrun_alert: crate::model::XRUN_ALERT_THRESHOLD,
            meter_input: false,
            refresh_hz: crate::app::SAMPLE_HZ,
        }
    }
}

impl Config {
    /// `$XDG_CONFIG_HOME/soundwatch/config.toml`, or `~/.config/...`.
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("soundwatch").join("config.toml"))
    }

    /// Load from disk, falling back to defaults for anything missing, unknown
    /// or out of range. Never fails: a broken config file must not stop a
    /// diagnostic tool from starting.
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| Self::parse(&s))
            .unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let mut c = Self::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once('=') else { continue };
            let (key, value) = (key.trim(), value.trim().trim_matches('"'));
            match key {
                "theme" => {
                    if let Some(t) = Theme::parse(value) {
                        c.theme = t;
                    }
                }
                "ascii" => c.ascii = value == "true",
                "meter_input" => c.meter_input = value == "true",
                "meter_floor" => set_f32(&mut c.meter_floor, value, -120.0, -20.0),
                "spectrum_floor" => set_f32(&mut c.spectrum_floor, value, -140.0, -40.0),
                "spectrum_decay" => set_f32(&mut c.spectrum_decay, value, 1.0, 200.0),
                "peak_hold" => set_f32(&mut c.peak_hold, value, 0.0, 30.0),
                "fft_size" => {
                    if let Ok(v) = value.parse::<usize>()
                        && FFT_SIZES.contains(&v)
                    {
                        c.fft_size = v;
                    }
                }
                "refresh_hz" => {
                    if let Ok(v) = value.parse::<u64>()
                        && REFRESH_RATES.contains(&v)
                    {
                        c.refresh_hz = v;
                    }
                }
                "xrun_alert" => {
                    if let Ok(v) = value.parse::<u32>()
                        && (1..=100).contains(&v)
                    {
                        c.xrun_alert = v;
                    }
                }
                _ => {}
            }
        }
        c
    }

    pub fn serialise(&self) -> String {
        let mut s = String::new();
        s.push_str("# soundwatch-lite settings. Written by the `,` menu.\n");
        s.push_str("# Delete this file to go back to the defaults.\n\n");
        let _ = writeln!(s, "theme = \"{}\"", self.theme.name());
        let _ = writeln!(s, "ascii = {}", self.ascii);
        let _ = writeln!(s, "meter_input = {}", self.meter_input);
        let _ = writeln!(s, "meter_floor = {}", self.meter_floor);
        let _ = writeln!(s, "spectrum_floor = {}", self.spectrum_floor);
        let _ = writeln!(s, "fft_size = {}", self.fft_size);
        let _ = writeln!(s, "spectrum_decay = {}", self.spectrum_decay);
        let _ = writeln!(s, "peak_hold = {}", self.peak_hold);
        let _ = writeln!(s, "xrun_alert = {}", self.xrun_alert);
        let _ = writeln!(s, "refresh_hz = {}", self.refresh_hz);
        s
    }

    /// Write to disk, creating the directory. Returns a reason on failure so
    /// the menu can say what went wrong rather than pretending it saved.
    pub fn save(&self) -> Result<PathBuf, String> {
        let path = Self::path().ok_or("no home directory to save into")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, self.serialise()).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }
}

/// A path for showing on one line: `$HOME` becomes `~`, and anything still too
/// long keeps its **tail**.
///
/// Paths are identified by their end, not their beginning — every path on the
/// machine starts the same way. The grid contract truncates left-aligned text
/// from the right, which is correct for names and wrong for these, so this
/// trims before handing it over rather than fighting the contract.
pub fn short_path(p: &std::path::Path, max: usize) -> String {
    let full = p.display().to_string();
    let s = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && full.starts_with(&h) => format!("~{}", &full[h.len()..]),
        _ => full,
    };
    if s.chars().count() <= max {
        return s;
    }
    let tail: String =
        s.chars().skip(s.chars().count().saturating_sub(max.saturating_sub(1))).collect();
    format!("\u{2026}{tail}")
}

fn set_f32(slot: &mut f32, value: &str, lo: f32, hi: f32) {
    if let Ok(v) = value.parse::<f32>()
        && v.is_finite()
        && (lo..=hi).contains(&v)
    {
        *slot = v;
    }
}

// ── the menu's model ─────────────────────────────────────────────────────────

/// One row of the settings menu.
///
/// The menu is data-driven from this list rather than hand-drawn, so a new
/// setting is one variant and three match arms, and the tests can walk every
/// row without knowing what any of them are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    Theme,
    Ascii,
    MeterFloor,
    XrunAlert,
    RefreshHz,
    MeterInput,
    FftSize,
    SpectrumFloor,
    SpectrumDecay,
    PeakHold,
}

impl Setting {
    pub const ALL: [Setting; 10] = [
        Setting::Theme,
        Setting::Ascii,
        Setting::MeterFloor,
        Setting::XrunAlert,
        Setting::RefreshHz,
        Setting::MeterInput,
        Setting::FftSize,
        Setting::SpectrumFloor,
        Setting::SpectrumDecay,
        Setting::PeakHold,
    ];

    /// The heading this row sits under, or `None` if it continues the last one.
    pub fn section(self) -> Option<&'static str> {
        match self {
            Setting::Theme => Some("display"),
            Setting::MeterInput => Some("metering"),
            Setting::FftSize => Some("spectrum"),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Setting::Theme => "theme",
            Setting::Ascii => "glyphs",
            Setting::MeterFloor => "meter floor",
            Setting::XrunAlert => "xrun alert",
            Setting::RefreshHz => "refresh",
            Setting::MeterInput => "input meter",
            Setting::FftSize => "analysis size",
            Setting::SpectrumFloor => "spectrum floor",
            Setting::SpectrumDecay => "bar decay",
            Setting::PeakHold => "peak hold",
        }
    }

    /// One line saying what the setting costs you, not what it is called.
    pub fn note(self) -> &'static str {
        match self {
            Setting::Theme => "btop shades bars by height; spec colours them by meaning",
            Setting::Ascii => "ascii is safe on terminals with thin glyph coverage",
            Setting::MeterFloor => "lower shows the noise floor, and flattens music",
            Setting::XrunAlert => "xruns in 60s before the header goes red",
            Setting::RefreshHz => "higher is smoother and busier; the audio is unaffected",
            Setting::MeterInput => "opens a capture stream: you appear in your own mic audit",
            Setting::FftSize => "bigger is finer in frequency and blurrier in time",
            Setting::SpectrumFloor => "bins sit far below the broadband peak; -96 is 16-bit",
            Setting::SpectrumDecay => "how fast bars fall back after a transient",
            Setting::PeakHold => "how long the peak markers stay put; 0 turns them off",
        }
    }

    pub fn value(self, c: &Config) -> String {
        match self {
            Setting::Theme => c.theme.name().into(),
            Setting::Ascii => if c.ascii { "ascii" } else { "unicode" }.into(),
            Setting::MeterFloor => format!("{:.0} dBFS", c.meter_floor),
            Setting::XrunAlert => format!("{}", c.xrun_alert),
            Setting::RefreshHz => format!("{} fps", c.refresh_hz),
            Setting::MeterInput => if c.meter_input { "on" } else { "off" }.into(),
            Setting::FftSize => format!("{} pt", c.fft_size),
            Setting::SpectrumFloor => format!("{:.0} dBFS", c.spectrum_floor),
            Setting::SpectrumDecay => format!("{:.0} dB/s", c.spectrum_decay),
            Setting::PeakHold => {
                if c.peak_hold <= 0.0 {
                    "off".into()
                } else {
                    format!("{:.1} s", c.peak_hold)
                }
            }
        }
    }

    /// Step this setting one place. `delta` is `+1` or `-1`.
    pub fn adjust(self, c: &mut Config, delta: i32) {
        match self {
            Setting::Theme => {
                c.theme = if c.theme == Theme::Spec { Theme::Btop } else { Theme::Spec }
            }
            Setting::Ascii => c.ascii = !c.ascii,
            Setting::MeterInput => c.meter_input = !c.meter_input,
            Setting::MeterFloor => {
                c.meter_floor = step_list(&[-40.0, -60.0, -72.0, -90.0], c.meter_floor, delta)
            }
            Setting::SpectrumFloor => {
                c.spectrum_floor = step_list(&[-72.0, -96.0, -120.0], c.spectrum_floor, delta)
            }
            Setting::SpectrumDecay => {
                c.spectrum_decay = step_list(&[10.0, 20.0, 40.0, 80.0], c.spectrum_decay, delta)
            }
            Setting::PeakHold => c.peak_hold = step_list(&[0.0, 0.8, 1.5, 3.0], c.peak_hold, delta),
            Setting::XrunAlert => {
                c.xrun_alert = step_list(&[1.0, 3.0, 10.0, 25.0], c.xrun_alert as f32, delta) as u32
            }
            Setting::FftSize => {
                let v: Vec<f32> = FFT_SIZES.iter().map(|s| *s as f32).collect();
                c.fft_size = step_list(&v, c.fft_size as f32, delta) as usize;
            }
            Setting::RefreshHz => {
                let v: Vec<f32> = REFRESH_RATES.iter().map(|s| *s as f32).collect();
                c.refresh_hz = step_list(&v, c.refresh_hz as f32, delta) as u64;
            }
        }
    }

    /// Does changing this need the audio backend restarted?
    ///
    /// Only one does, and saying so on screen is the difference between a menu
    /// that works and one that appears not to.
    pub fn needs_restart(self) -> bool {
        self == Setting::MeterInput
    }
}

/// Move to the next value in `opts`, wrapping. Nearest match if the current
/// value is not in the list — which happens when it was hand-edited into the
/// file within the allowed range but off the menu's ladder.
fn step_list(opts: &[f32], current: f32, delta: i32) -> f32 {
    if opts.is_empty() {
        return current;
    }
    let nearest = opts
        .iter()
        .enumerate()
        .min_by(|a, b| {
            (a.1 - current)
                .abs()
                .partial_cmp(&(b.1 - current).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i as i32)
        .unwrap_or(0);
    let n = opts.len() as i32;
    opts[(((nearest + delta) % n + n) % n) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_specs_numbers() {
        let c = Config::default();
        assert_eq!(c.theme, Theme::Spec, "the spec look must survive a fresh install");
        assert!(!c.ascii);
        assert_eq!(c.meter_floor, -60.0);
        assert_eq!(c.spectrum_floor, crate::dsp::FLOOR_DBFS);
        assert_eq!(c.fft_size, crate::dsp::FFT_SIZE);
        assert_eq!(c.xrun_alert, crate::model::XRUN_ALERT_THRESHOLD);
        assert_eq!(c.spectrum_decay, crate::spectrum::DECAY_DB_PER_SEC);
        assert_eq!(c.peak_hold, crate::spectrum::PEAK_HOLD_SECS);
        assert_eq!(c.refresh_hz, crate::app::SAMPLE_HZ);
    }

    #[test]
    fn a_config_survives_a_round_trip() {
        let c = Config {
            theme: Theme::Btop,
            ascii: true,
            meter_floor: -90.0,
            fft_size: 8192,
            peak_hold: 0.0,
            xrun_alert: 25,
            refresh_hz: 60,
            ..Default::default()
        };
        assert_eq!(Config::parse(&c.serialise()), c);
    }

    /// A broken config file must not stop a diagnostic tool from starting.
    #[test]
    fn a_damaged_file_falls_back_field_by_field() {
        let c = Config::parse(
            "
            # a comment
            theme = \"btop\"
            ascii = yes please
            meter_floor = not-a-number
            fft_size = 4095
            refresh_hz = 999
            xrun_alert = 0
            nonsense_key = 4
            no equals sign here
            spectrum_decay = 40
            ",
        );
        let d = Config::default();
        assert_eq!(c.theme, Theme::Btop, "the one good line was dropped");
        assert_eq!(c.spectrum_decay, 40.0, "a later good line was dropped");
        // Everything malformed kept its default.
        assert_eq!(c.ascii, d.ascii);
        assert_eq!(c.meter_floor, d.meter_floor);
        assert_eq!(c.fft_size, d.fft_size, "an off-ladder size was accepted");
        assert_eq!(c.refresh_hz, d.refresh_hz);
        assert_eq!(c.xrun_alert, d.xrun_alert, "zero xruns would alert on nothing");
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(Config::parse(""), Config::default());
        assert_eq!(Config::parse("\n\n# only comments\n"), Config::default());
    }

    /// Every row has to cycle, and cycling all the way round has to land back
    /// where it started — otherwise a setting has a value you can reach and
    /// never leave.
    #[test]
    fn every_setting_cycles_and_returns() {
        for s in Setting::ALL {
            let start = Config::default();
            let mut c = start;
            let mut seen = vec![s.value(&c)];
            for _ in 0..12 {
                s.adjust(&mut c, 1);
                let v = s.value(&c);
                if v == seen[0] {
                    break;
                }
                seen.push(v);
            }
            assert!(seen.len() > 1, "{s:?} has only one value");
            assert_eq!(s.value(&c), seen[0], "{s:?} does not cycle back to its start");

            // And backwards is the inverse of forwards.
            let mut c = start;
            s.adjust(&mut c, 1);
            s.adjust(&mut c, -1);
            assert_eq!(c, start, "{s:?} is not reversible");
        }
    }

    #[test]
    fn every_setting_stays_inside_the_range_the_parser_accepts() {
        for s in Setting::ALL {
            let mut c = Config::default();
            for _ in 0..20 {
                s.adjust(&mut c, 1);
                // A value the menu can reach must survive being written and
                // read back, or saving would silently discard it.
                assert_eq!(Config::parse(&c.serialise()), c, "{s:?} produced an unsaveable value");
            }
        }
    }

    #[test]
    fn every_row_is_labelled_and_explained() {
        for s in Setting::ALL {
            assert!(!s.label().is_empty());
            assert!(!s.note().is_empty(), "{s:?} has no explanation");
            // The note has to fit the panel it is drawn in.
            assert!(s.note().len() <= 62, "{s:?} note is too long: {}", s.note());
        }
    }

    #[test]
    fn a_long_path_keeps_the_end_that_identifies_it() {
        let p = std::path::Path::new("/very/long/prefix/that/nobody/reads/soundwatch/config.toml");
        let short = short_path(p, 30);
        assert!(short.chars().count() <= 30, "{short}");
        assert!(short.ends_with("config.toml"), "the identifying end was cut: {short}");
        assert!(short.starts_with('\u{2026}'), "no elision marker: {short}");
        // A path that fits is left alone.
        let p = std::path::Path::new("/tmp/a.toml");
        assert_eq!(short_path(p, 30), "/tmp/a.toml");
    }

    #[test]
    fn only_the_input_meter_needs_a_restart() {
        let restarts: Vec<_> = Setting::ALL.into_iter().filter(|s| s.needs_restart()).collect();
        assert_eq!(restarts, vec![Setting::MeterInput]);
    }

    #[test]
    fn a_hand_edited_off_ladder_value_still_steps() {
        // Legal per the parser's range, but not one of the menu's stops.
        let mut c = Config { meter_floor: -66.0, ..Default::default() };
        Setting::MeterFloor.adjust(&mut c, 1);
        assert_eq!(c.meter_floor, -72.0, "stepping from an off-ladder value went nowhere");
    }
}
