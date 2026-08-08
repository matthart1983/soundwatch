//! The spectrum screen's model: bins to columns, ballistics, and the reads.
//!
//! The graph is the easy half. A spectrum display that does not *name* what it
//! is showing is a screensaver, and this tool's whole thesis is that a
//! diagnostic says what is wrong in plain words. So the detectors in
//! [`Spectrum::verdict`] are as much the deliverable as the bars are:
//!
//! * mains hum — a ground loop, the single most common "why does this buzz"
//! * a band-limit cliff — a lossy source or a Bluetooth codec, invisible on
//!   every other screen in the program because the *level* is perfectly fine
//! * DC offset — quietly eating headroom
//!
//! Each requires several seconds of agreement before it will say anything, for
//! the same reason `XRUN_ALERT_THRESHOLD` exists: a row that flickers
//! accusations is a row people learn to ignore.

use crate::config::Config;
use crate::dsp::{Analysis, Analyzer, FLOOR_DBFS};

/// Lowest frequency the axis shows. Below this is subsonic for every purpose
/// this screen serves, and it would spend columns on nothing.
pub const MIN_HZ: f32 = 20.0;

/// Default fall, in dB per second. Conventional analyser ballistics: a
/// transient must appear on the frame it happens, and the fall has to be slow
/// enough to read. Configurable; see [`crate::config::Config`].
pub const DECAY_DB_PER_SEC: f32 = 20.0;
/// Default hold before a peak marker starts falling. Configurable.
pub const PEAK_HOLD_SECS: f32 = 1.5;
/// And how fast it falls once it lets go.
pub const PEAK_FALL_DB_PER_SEC: f32 = 12.0;

/// How long a condition must persist before the verdict line names it.
pub const SUSTAIN_SECS: f32 = 3.0;

/// Which signal the screen is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    Off,
    Output,
    Input,
}

impl Source {
    /// `s` cycles off → output → input → off.
    pub fn next(self) -> Self {
        match self {
            Source::Off => Source::Output,
            Source::Output => Source::Input,
            Source::Input => Source::Off,
        }
    }

    pub fn is_on(self) -> bool {
        self != Source::Off
    }

    pub fn label(self) -> &'static str {
        match self {
            Source::Off => "off",
            Source::Output => "output",
            Source::Input => "input",
        }
    }
}

/// Map a display column to a frequency, and back.
///
/// The axis is logarithmic because pitch is, and because every reason to open
/// this screen is. A linear axis spends half its width on 12–24 kHz, where
/// nothing diagnostic ever happens, and crushes the whole bass range into two
/// columns.
#[derive(Clone, Copy, Debug)]
pub struct Axis {
    pub width: u16,
    pub nyquist: f32,
    decades: f32,
}

impl Axis {
    pub fn new(width: u16, nyquist: f32) -> Self {
        let nyquist = nyquist.max(MIN_HZ * 2.0);
        Self { width: width.max(1), nyquist, decades: (nyquist / MIN_HZ).log10() }
    }

    /// Frequency at the left edge of column `i` (0-based).
    pub fn hz_at(&self, i: u16) -> f32 {
        let span = (self.width.max(2) - 1) as f32;
        MIN_HZ * 10f32.powf(i as f32 / span * self.decades)
    }

    /// The column a frequency belongs to, or `None` if it is off the axis.
    pub fn column_of(&self, hz: f32) -> Option<u16> {
        if hz < MIN_HZ || hz > self.nyquist {
            return None;
        }
        let span = (self.width.max(2) - 1) as f32;
        let c = (hz / MIN_HZ).log10() / self.decades * span;
        Some(c.round().clamp(0.0, span) as u16)
    }
}

/// dB-space normalisation to 0..=1 on the spectrum's floor.
///
/// The meters have their own at [`crate::meter::norm`] with a -60 dBFS floor.
/// Sharing one would put most real content in the bottom two rows here; see
/// [`FLOOR_DBFS`].
pub fn norm(dbfs: f32, floor: f32) -> f32 {
    crate::meter::norm(dbfs, floor)
}

/// One column of the graph.
#[derive(Clone, Copy, Debug, Default)]
pub struct Column {
    /// The live value after ballistics, in dBFS.
    pub level: f32,
    /// The peak-hold marker, in dBFS.
    pub hold: f32,
}

/// The spectrum screen's whole state.
pub struct Spectrum {
    pub source: Source,
    analyzer: Analyzer,
    /// Latest raw analysis, kept for the readout and the detectors.
    latest: Option<Analysis>,
    columns: Vec<Column>,
    hold_age: Vec<f32>,
    /// Seconds each detector has agreed with itself, for the sustain rule.
    hum_for: f32,
    band_for: f32,
    dc_for: f32,
    /// The frequency the band-limit detector last measured.
    band_edge: f32,
    /// The hum fundamental the detector last measured.
    hum_hz: f32,
    hum_excess: f32,
    /// Windows lost to the real-time callback lapping the reader.
    overruns: u64,
    /// Ballistics and scale, from the settings menu.
    decay: f32,
    hold: f32,
    floor: f32,
}

impl Default for Spectrum {
    fn default() -> Self {
        Self::new()
    }
}

impl Spectrum {
    pub fn new() -> Self {
        Self {
            source: Source::Off,
            analyzer: Analyzer::default(),
            latest: None,
            columns: Vec::new(),
            hold_age: Vec::new(),
            hum_for: 0.0,
            band_for: 0.0,
            dc_for: 0.0,
            band_edge: 0.0,
            hum_hz: 0.0,
            hum_excess: 0.0,
            overruns: 0,
            decay: DECAY_DB_PER_SEC,
            hold: PEAK_HOLD_SECS,
            floor: FLOOR_DBFS,
        }
    }

    /// Adopt the settings menu's values.
    ///
    /// Rebuilds the transform only when its shape actually changed: `configure`
    /// runs on every frame, and reallocating a 8192-point window twenty times a
    /// second to arrive at the same numbers would be a waste with a visible
    /// stutter attached.
    pub fn configure(&mut self, cfg: &Config) {
        self.decay = cfg.spectrum_decay;
        self.hold = cfg.peak_hold;
        if self.analyzer.size() != cfg.fft_size || self.analyzer.floor() != cfg.spectrum_floor {
            self.analyzer = Analyzer::new(cfg.fft_size, cfg.spectrum_floor);
            self.floor = self.analyzer.floor();
            // The old bins were measured on a different scale; keeping them
            // would draw one signal against another's floor for a frame.
            self.latest = None;
            self.columns.clear();
            self.hold_age.clear();
        }
    }

    /// How many samples a window needs, so the backend can fetch the right
    /// number rather than assuming the default size.
    pub fn window_len(&self) -> usize {
        self.analyzer.size()
    }

    pub fn floor(&self) -> f32 {
        self.floor
    }

    /// Forget everything measured. Called when the source changes, so one
    /// signal's history never bleeds into another's.
    pub fn reset(&mut self) {
        self.latest = None;
        self.columns.clear();
        self.hold_age.clear();
        self.hum_for = 0.0;
        self.band_for = 0.0;
        self.dc_for = 0.0;
        self.overruns = 0;
    }

    pub fn cycle_source(&mut self) {
        self.source = self.source.next();
        self.reset();
    }

    pub fn has_data(&self) -> bool {
        self.latest.is_some()
    }

    pub fn analysis(&self) -> Option<&Analysis> {
        self.latest.as_ref()
    }

    /// Feed one window of samples and advance the ballistics by `dt` seconds.
    pub fn push_with_overruns(
        &mut self,
        samples: &[f32],
        rate: u32,
        width: u16,
        dt: f32,
        overruns: u64,
    ) {
        self.overruns = overruns;
        self.push(samples, rate, width, dt);
    }

    /// Feed one window of samples and advance the ballistics by `dt` seconds.
    pub fn push(&mut self, samples: &[f32], rate: u32, width: u16, dt: f32) {
        let a = self.analyzer.analyze(samples, rate);
        let axis = Axis::new(width, a.rate as f32 / 2.0);
        let raw = fold_to_columns(&a, &axis);
        self.apply_ballistics(&raw, dt);
        self.update_detectors(&a, dt);
        self.latest = Some(a);
    }

    /// Advance the ballistics with no new audio: the meters fall rather than
    /// freezing when the signal stops arriving.
    pub fn idle(&mut self, dt: f32) {
        let raw: Vec<f32> = vec![self.floor; self.columns.len()];
        self.apply_ballistics(&raw, dt);
    }

    fn apply_ballistics(&mut self, raw: &[f32], dt: f32) {
        if self.columns.len() != raw.len() {
            self.columns = raw.iter().map(|&v| Column { level: v, hold: v }).collect();
            self.hold_age = vec![0.0; raw.len()];
            return;
        }
        let fall = self.decay * dt;
        let hold_fall = PEAK_FALL_DB_PER_SEC * dt;
        for (i, &v) in raw.iter().enumerate() {
            let c = &mut self.columns[i];
            // Attack is instantaneous; only the fall is slewed.
            c.level = if v >= c.level { v } else { (c.level - fall).max(v) };
            if c.level >= c.hold {
                c.hold = c.level;
                self.hold_age[i] = 0.0;
            } else {
                self.hold_age[i] += dt;
                if self.hold_age[i] > self.hold {
                    c.hold = (c.hold - hold_fall).max(c.level);
                }
            }
        }
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The loudest tone, interpolated. `None` when there is nothing to report.
    pub fn peak(&self) -> Option<(f32, f32)> {
        self.latest.as_ref()?.peak()
    }

    fn update_detectors(&mut self, a: &Analysis, dt: f32) {
        // Everything below is meaningless on a silent signal, and a silent
        // signal is the normal state of an idle machine.
        if a.bins.iter().all(|v| *v <= a.floor + 1.0) {
            self.hum_for = 0.0;
            self.band_for = 0.0;
            self.dc_for = 0.0;
            return;
        }

        match detect_hum(a) {
            Some((hz, excess)) => {
                self.hum_hz = hz;
                self.hum_excess = excess;
                self.hum_for += dt;
            }
            None => self.hum_for = 0.0,
        }
        match detect_band_limit(a) {
            Some(edge) => {
                self.band_edge = edge;
                self.band_for += dt;
            }
            None => self.band_for = 0.0,
        }
        if a.bins[0] >= DC_ALERT_DBFS {
            self.dc_for += dt;
        } else {
            self.dc_for = 0.0;
        }
    }

    /// What is wrong with this signal, in plain words. `None` when nothing is.
    pub fn verdict(&self) -> Option<String> {
        // First, because it means the picture itself is not to be trusted.
        if self.overruns > 0 {
            return Some(format!(
                "the analyser is dropping audio ({} windows lost) \u{2014} the display is behind",
                self.overruns
            ));
        }
        if self.hum_for >= SUSTAIN_SECS {
            let hz = self.hum_hz.round();
            return Some(format!(
                "{hz:.0} Hz hum, {:.0} dB above the noise floor \u{b7} check grounding and cable runs",
                self.hum_excess
            ));
        }
        if self.band_for >= SUSTAIN_SECS {
            return Some(format!(
                "content stops at {:.1} kHz \u{b7} lossy source, or a Bluetooth codec",
                self.band_edge / 1000.0
            ));
        }
        if self.dc_for >= SUSTAIN_SECS {
            return Some("DC offset on this path \u{b7} it is eating headroom".into());
        }
        None
    }
}

/// Bin 0 above this is a DC offset worth mentioning.
pub const DC_ALERT_DBFS: f32 = -60.0;
/// How far above its neighbours a bin must sit to be called hum.
pub const HUM_EXCESS_DB: f32 = 20.0;
/// Mains frequencies worth naming, and the tolerance for matching one.
pub const HUM_TOLERANCE_HZ: f32 = 2.0;

/// Reduce the linear bins to a logarithmic axis.
///
/// Below [`column_limited_below_hz`] several columns land in one bin and the
/// result stair-steps. That is deliberate and it is **not** interpolated:
/// smoothing there would invent resolution the transform never had, which is
/// the same sin as a truncated number rendered as a whole one. Above it, each
/// column aggregates several bins and takes the **maximum** — averaging hides
/// exactly the narrow peaks this screen exists to find.
pub fn fold_to_columns(a: &Analysis, axis: &Axis) -> Vec<f32> {
    let n = axis.width as usize;
    let mut out = vec![a.floor; n];
    let bin_hz = a.bin_hz(1);
    for (i, slot) in out.iter_mut().enumerate() {
        let lo = axis.hz_at(i as u16);
        let hi = if i + 1 < n { axis.hz_at(i as u16 + 1) } else { axis.nyquist };
        // At least one bin per column, always: the nearest, when the column is
        // narrower than a bin.
        let k0 = ((lo / bin_hz).floor() as usize).min(a.bins.len() - 1);
        // The last column runs to the end: `ceil(nyquist / bin_hz)` is the
        // Nyquist bin's *index*, so an exclusive range would drop it.
        let k1 = if i + 1 < n {
            ((hi / bin_hz).ceil() as usize).clamp(k0 + 1, a.bins.len())
        } else {
            a.bins.len()
        };
        *slot = a.bins[k0..k1].iter().copied().fold(a.floor, f32::max);
    }
    out
}

/// The frequency below which the log axis is finer than the transform.
///
/// A column spans `(10^(decades/span) - 1) * f`, so it equals one bin width at
/// this frequency. At 48 kHz over 78 columns that is about 122 Hz — columns 1
/// to 20 are resolution-limited, and drawn honestly as stair-steps rather than
/// smoothed into detail the transform never had.
///
/// Test-only: this exists to pin down the arithmetic behind that decision, not
/// to be drawn. A marker on the axis for it would be clutter on a screen whose
/// whole premise is that one line of plain words beats a legend.
#[cfg(test)]
pub fn column_limited_below_hz(axis: &Axis, rate: u32) -> f32 {
    let span = (axis.width.max(2) - 1) as f32;
    let ratio = 10f32.powf(axis.decades / span) - 1.0;
    let bin_hz = rate as f32 / crate::dsp::FFT_SIZE as f32;
    if ratio <= 0.0 { MIN_HZ } else { bin_hz / ratio }
}

/// Mains hum: a strong narrow peak at 50 or 60 Hz, well above its neighbours.
///
/// The peak frequency is the *interpolated* one, never a bin index — at 11.7 Hz
/// bins, 50 and 60 Hz are adjacent bins and the index cannot tell them apart.
fn detect_hum(a: &Analysis) -> Option<(f32, f32)> {
    let (hz, db) = a.peak()?;
    let mains =
        [50.0f32, 60.0, 100.0, 120.0].into_iter().find(|m| (hz - m).abs() <= HUM_TOLERANCE_HZ)?;

    // The floor to compare against: the median of the low band, excluding the
    // peak's own neighbourhood so a loud tone cannot raise its own baseline.
    let bin_hz = a.bin_hz(1);
    let mut neighbours: Vec<f32> = a
        .bins
        .iter()
        .enumerate()
        .filter(|(k, _)| {
            let f = *k as f32 * bin_hz;
            (30.0..=200.0).contains(&f) && (f - hz).abs() > 3.0 * bin_hz
        })
        .map(|(_, v)| *v)
        .collect();
    if neighbours.len() < 4 {
        return None;
    }
    neighbours.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let median = neighbours[neighbours.len() / 2];

    let excess = db - median;
    (excess >= HUM_EXCESS_DB).then_some((mains, excess))
}

/// A brick wall: content stops dead well below Nyquist.
///
/// Both halves are required. `edge < 0.9 * nyquist` alone would fire on any
/// quiet or genuinely dull material; the cliff test — a 40 dB fall inside a
/// kilohertz — is what distinguishes a codec's brick wall from a gentle
/// roll-off.
fn detect_band_limit(a: &Analysis) -> Option<f32> {
    let bin_hz = a.bin_hz(1);
    let threshold = a.floor + 12.0;
    let top = a.bins.iter().rposition(|v| *v >= threshold)?;
    let edge = top as f32 * bin_hz;
    if edge >= 0.9 * a.nyquist() || edge < 2000.0 {
        return None;
    }

    // Compare the kilohertz below the edge with the kilohertz above it.
    let span = (1000.0 / bin_hz).ceil() as usize;
    let below_lo = top.saturating_sub(span);
    let below: f32 = a.bins[below_lo..=top].iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let above_hi = (top + span).min(a.bins.len() - 1);
    if above_hi <= top + 1 {
        return None;
    }
    let above: f32 = a.bins[top + 1..=above_hi].iter().copied().fold(f32::NEG_INFINITY, f32::max);

    (below - above >= 40.0).then_some(edge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp;
    use std::f32::consts::PI;

    const RATE: u32 = 48_000;

    fn analyze(s: &[f32]) -> Analysis {
        Analyzer::default().analyze(s, RATE)
    }

    fn sine(hz: f32, amp: f32) -> Vec<f32> {
        (0..dsp::FFT_SIZE).map(|i| amp * (2.0 * PI * hz * i as f32 / RATE as f32).sin()).collect()
    }

    /// Deterministic pseudo-noise, band-limited by summing tones up to `top`.
    fn band_limited_noise(top: f32) -> Vec<f32> {
        let mut out = vec![0.0f32; dsp::FFT_SIZE];
        let mut seed = 12345u64;
        let mut f = 40.0f32;
        while f < top {
            seed = (seed.wrapping_mul(6364136223846793005)).wrapping_add(1442695040888963407);
            let phase = (seed >> 33) as f32 / (1u64 << 31) as f32 * 2.0 * PI;
            for (i, v) in out.iter_mut().enumerate() {
                *v += 0.02 * (2.0 * PI * f * i as f32 / RATE as f32 + phase).sin();
            }
            f *= 1.05;
        }
        out
    }

    // ── the axis ────────────────────────────────────────────────────────────

    /// The spec's numbers: at 48 kHz over 78 columns, 100 Hz / 1 kHz / 10 kHz
    /// land in columns 18, 43 and 68 (1-based content columns).
    #[test]
    fn the_axis_puts_the_decade_marks_where_the_spec_says() {
        let axis = Axis::new(78, 24_000.0);
        assert_eq!(axis.column_of(100.0).unwrap() + 1, 18);
        assert_eq!(axis.column_of(1000.0).unwrap() + 1, 43);
        assert_eq!(axis.column_of(10_000.0).unwrap() + 1, 68);
    }

    #[test]
    fn the_axis_spans_twenty_hertz_to_nyquist() {
        let axis = Axis::new(78, 24_000.0);
        assert!((axis.hz_at(0) - MIN_HZ).abs() < 0.01);
        assert!((axis.hz_at(77) - 24_000.0).abs() < 1.0);
        assert!(axis.column_of(10.0).is_none(), "below the axis is off the axis");
        assert!(axis.column_of(30_000.0).is_none());
    }

    #[test]
    fn the_axis_is_monotonic_at_every_width() {
        for w in [40u16, 78, 128, 200, 400] {
            let axis = Axis::new(w, 24_000.0);
            let mut prev = 0.0;
            for i in 0..w {
                let f = axis.hz_at(i);
                assert!(f > prev, "width {w}: column {i} is {f} Hz, not above {prev}");
                prev = f;
            }
        }
    }

    /// Every bin from 20 Hz up has to be claimed by some column, or content
    /// vanishes between the linear and logarithmic domains.
    #[test]
    fn no_bin_is_dropped_between_the_domains() {
        let a = analyze(&sine(1000.0, 1.0));
        let axis = Axis::new(78, 24_000.0);
        let bin_hz = RATE as f32 / dsp::FFT_SIZE as f32;
        let mut covered = vec![false; a.bins.len()];
        for i in 0..axis.width {
            let lo = axis.hz_at(i);
            let hi = if i + 1 < axis.width { axis.hz_at(i + 1) } else { axis.nyquist };
            let k0 = ((lo / bin_hz).floor() as usize).min(a.bins.len() - 1);
            let k1 = if i + 1 < axis.width {
                ((hi / bin_hz).ceil() as usize).clamp(k0 + 1, a.bins.len())
            } else {
                a.bins.len()
            };
            for c in covered.iter_mut().take(k1).skip(k0) {
                *c = true;
            }
        }
        let first = (MIN_HZ / bin_hz).floor() as usize;
        for (k, c) in covered.iter().enumerate().skip(first) {
            assert!(*c, "bin {k} ({} Hz) is not on the axis", k as f32 * bin_hz);
        }
    }

    #[test]
    fn a_tone_folds_into_the_column_it_belongs_to() {
        // Bin-centred, so the reading is exact: an off-centre tone loses up to
        // 1.42 dB to Hann scalloping and would blur what this test is checking.
        let hz = 85.0 * RATE as f32 / dsp::FFT_SIZE as f32;
        let a = analyze(&sine(hz, 1.0));
        let axis = Axis::new(78, 24_000.0);
        let cols = fold_to_columns(&a, &axis);
        let loudest = cols
            .iter()
            .enumerate()
            .max_by(|x, y| x.1.partial_cmp(y.1).unwrap())
            .map(|(i, _)| i as u16)
            .unwrap();
        let expected = axis.column_of(hz).unwrap();
        assert!(
            loudest.abs_diff(expected) <= 1,
            "{hz} Hz folded into column {loudest}, expected about {expected}"
        );
        assert!(
            (cols[loudest as usize] - 0.0).abs() < 0.2,
            "amplitude lost in the fold: {} dBFS",
            cols[loudest as usize]
        );
    }

    #[test]
    fn the_resolution_limit_is_where_the_arithmetic_says() {
        let axis = Axis::new(78, 24_000.0);
        let f = column_limited_below_hz(&axis, RATE);
        assert!((f - 122.0).abs() < 6.0, "resolution crossover at {f} Hz, expected about 122");
    }

    // ── ballistics ──────────────────────────────────────────────────────────

    #[test]
    fn attack_is_instant_and_decay_takes_the_specified_time() {
        let mut s = Spectrum::new();
        s.apply_ballistics(&[FLOOR_DBFS; 4], 0.05);
        s.apply_ballistics(&[0.0; 4], 0.05);
        assert_eq!(s.columns[0].level, 0.0, "attack was slewed");

        // One second of silence at 20 fps must fall exactly DECAY_DB_PER_SEC.
        for _ in 0..20 {
            s.apply_ballistics(&[FLOOR_DBFS; 4], 0.05);
        }
        assert!(
            (s.columns[0].level + DECAY_DB_PER_SEC).abs() < 0.01,
            "after a second the column is at {}",
            s.columns[0].level
        );
    }

    #[test]
    fn a_peak_holds_then_falls() {
        let mut s = Spectrum::new();
        s.apply_ballistics(&[FLOOR_DBFS; 2], 0.05);
        s.apply_ballistics(&[-10.0; 2], 0.05);
        assert_eq!(s.columns[0].hold, -10.0);

        // Just under the hold time: the marker has not moved.
        for _ in 0..29 {
            s.apply_ballistics(&[FLOOR_DBFS; 2], 0.05);
        }
        assert_eq!(s.columns[0].hold, -10.0, "the peak let go early");

        // Well past it: the marker is falling.
        for _ in 0..20 {
            s.apply_ballistics(&[FLOOR_DBFS; 2], 0.05);
        }
        assert!(s.columns[0].hold < -10.0, "the peak never fell");
        assert!(s.columns[0].hold >= s.columns[0].level, "the peak fell below the signal");
    }

    // ── the detectors ───────────────────────────────────────────────────────

    #[test]
    fn hum_is_detected_and_named_correctly() {
        for mains in [50.0f32, 60.0] {
            let mut s = band_limited_noise(15_000.0);
            for (i, v) in s.iter_mut().enumerate() {
                *v = *v * 0.02 + 0.5 * (2.0 * PI * mains * i as f32 / RATE as f32).sin();
            }
            let a = analyze(&s);
            let (hz, excess) =
                detect_hum(&a).unwrap_or_else(|| panic!("{mains} Hz hum not detected"));
            assert_eq!(hz, mains, "hum reported as {hz} Hz, not {mains}");
            assert!(excess >= HUM_EXCESS_DB, "excess only {excess} dB");
        }
    }

    #[test]
    fn music_like_noise_is_not_called_hum() {
        let a = analyze(&band_limited_noise(18_000.0));
        assert!(detect_hum(&a).is_none(), "broadband noise was reported as hum");
    }

    #[test]
    fn a_brick_wall_is_detected_near_the_right_frequency() {
        let a = analyze(&band_limited_noise(15_000.0));
        let edge = detect_band_limit(&a).expect("15 kHz brick wall not detected");
        assert!((edge - 15_000.0).abs() < 1500.0, "band edge reported at {edge} Hz");
    }

    #[test]
    fn full_band_content_is_not_called_band_limited() {
        let a = analyze(&band_limited_noise(23_000.0));
        assert!(detect_band_limit(&a).is_none(), "full-band noise was called lossy");
    }

    #[test]
    fn nothing_fires_on_silence() {
        let mut s = Spectrum::new();
        for _ in 0..200 {
            s.push(&vec![0.0f32; dsp::FFT_SIZE], RATE, 78, 0.05);
        }
        assert!(s.verdict().is_none(), "silence produced {:?}", s.verdict());
    }

    /// The sustain rule: one frame of anything is not a fault.
    #[test]
    fn a_detector_waits_before_it_accuses() {
        let mut sig = band_limited_noise(15_000.0);
        for (i, v) in sig.iter_mut().enumerate() {
            *v = *v * 0.02 + 0.5 * (2.0 * PI * 50.0 * i as f32 / RATE as f32).sin();
        }
        let mut s = Spectrum::new();
        s.push(&sig, RATE, 78, 0.05);
        assert!(s.verdict().is_none(), "accused after a single frame");

        // Three seconds at 20 fps.
        for _ in 0..(SUSTAIN_SECS / 0.05) as usize {
            s.push(&sig, RATE, 78, 0.05);
        }
        let v = s.verdict().expect("sustained hum went unreported");
        assert!(v.contains("50 Hz hum"), "{v}");
        assert!(v.contains("grounding"), "the verdict must say what to do: {v}");
    }

    #[test]
    fn a_dc_offset_is_reported() {
        // Full-band content plus an offset. A band-limited fixture would trip
        // the band-limit detector first — correctly — and prove nothing here.
        let mut s = Spectrum::new();
        let mut sig = band_limited_noise(23_000.0);
        for v in sig.iter_mut() {
            *v += 0.3;
        }
        for _ in 0..80 {
            s.push(&sig, RATE, 78, 0.05);
        }
        let v = s.verdict().expect("a DC offset went unreported");
        assert!(v.contains("DC offset"), "{v}");
    }

    #[test]
    fn changing_source_forgets_the_previous_signal() {
        let mut s = Spectrum::new();
        s.push(&sine(1000.0, 1.0), RATE, 78, 0.05);
        assert!(s.has_data());
        s.cycle_source();
        assert!(!s.has_data(), "the new source inherited the old one's spectrum");
        assert!(s.columns().is_empty());
    }

    #[test]
    fn the_source_cycles_through_all_three_and_back() {
        assert_eq!(Source::Off.next(), Source::Output);
        assert_eq!(Source::Output.next(), Source::Input);
        assert_eq!(Source::Input.next(), Source::Off);
        assert!(!Source::Off.is_on());
        assert!(Source::Output.is_on() && Source::Input.is_on());
    }
}
