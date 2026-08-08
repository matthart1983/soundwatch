//! Level history, dB-space normalisation, and the block-glyph meters.
//!
//! Two things here differ from LITE.md, both deliberately.
//!
//! **Window arithmetic.** The spec asks for "a 60-sample-per-minute ring"
//! driving a chart that is 78 columns wide and labelled `60s ago -> now`; 60
//! samples cannot fill 78 columns. (The reference renderer generates 78.)
//! Resolved by decoupling the two entirely: the ring records at the sampler's
//! rate and the chart reduces it to whatever width it has. The window really is
//! 60 seconds at every terminal size, which is what the axis claims.
//!
//! **The floor glyph.** The spec's meter algorithm yields a blank column at the
//! -60 dBFS floor while its sparkline algorithm yields `▁` for the same input —
//! the same silence drawn two ways on one screen. Resolved toward the baseline:
//! a signal at the floor draws `▁`, and truly blank means "no data", which is
//! visually distinct and matches the `--` degradation rule.

use std::collections::VecDeque;

/// The default bottom of the *display* scale. Right for speech and music; the
/// settings menu lowers it for noise-floor work. See [`crate::config::Config`].
pub const FLOOR_DBFS: f32 = -60.0;

/// The lowest level the meters ever *record*, as distinct from the lowest they
/// draw.
///
/// These were one number until the floor became configurable, and conflating
/// them is a subtle bug: if silence is stored as -60 and the user then lowers
/// the display floor to -90, every silent bucket in the history redraws at a
/// third of full height. The ring keeps real dB and the renderer decides where
/// the bottom of the picture is.
pub const CAPTURE_FLOOR_DBFS: f32 = -120.0;
/// The window the time axis advertises.
pub const WINDOW_SECS: f32 = 60.0;

/// Buckets held in the history ring, independent of how wide the chart is.
///
/// This used to be 78 — the content width of an 80-column terminal — which
/// tied the ring to the screen. On a wider terminal that leaves two bad
/// options: stretch 78 buckets across 200 columns (inventing detail that was
/// never recorded) or resize the ring on every resize (throwing away the
/// history you were looking at).
///
/// So the ring keeps one bucket per sample at the sampler's rate, and the
/// renderer reduces it to whatever width the chart happens to be with
/// [`downsample_max`] — the same reduction the 9-column sparkline already used.
/// One resolution, recorded once, drawn at any size.
pub const HISTORY_BUCKETS: usize = 1200;
/// Seconds of signal aggregated into one bucket: 50 ms, the sampler's period.
pub const BUCKET_SECS: f32 = WINDOW_SECS / HISTORY_BUCKETS as f32;

/// dB-space normalisation to 0..=1. dBFS is logarithmic and the chart is not;
/// normalising in dB space before the glyph conversion is what keeps the meter
/// honest. Plotting raw amplitude makes every meter a liar.
pub fn norm(dbfs: f32, floor: f32) -> f32 {
    if !dbfs.is_finite() || floor >= 0.0 {
        return 0.0;
    }
    ((dbfs - floor) / -floor).clamp(0.0, 1.0)
}

/// One sparkline cell.
pub fn spark(dbfs: f32, blocks: &[char; 8], floor: f32) -> char {
    let idx = (norm(dbfs, floor) * 7.0).round().clamp(0.0, 7.0) as usize;
    blocks[idx]
}

/// Reduce a series to `n` cells, keeping the **peak** of each bucket.
///
/// The spec says the 9-column sparkline shows "that stream's 60s level history"
/// but never says how 60 seconds becomes 9 cells. It has to be max: averaging a
/// peak meter hides exactly the transients the widget exists to show.
pub fn downsample_max(src: &[f32], n: usize) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    if src.is_empty() {
        return vec![CAPTURE_FLOOR_DBFS; n];
    }
    (0..n)
        .map(|i| {
            let lo = i * src.len() / n;
            let hi = (((i + 1) * src.len()).div_ceil(n)).max(lo + 1).min(src.len());
            src[lo..hi].iter().copied().fold(f32::NEG_INFINITY, f32::max)
        })
        .collect()
}

/// A rolling 60-second peak history, filled by a ~20 Hz sampler and committed
/// one column at a time.
#[derive(Debug, Clone)]
pub struct History {
    cols: VecDeque<f32>,
    bucket: f32,
    bucket_has_sample: bool,
    has_data: bool,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    pub fn new() -> Self {
        Self {
            cols: VecDeque::with_capacity(HISTORY_BUCKETS),
            bucket: f32::NEG_INFINITY,
            bucket_has_sample: false,
            has_data: false,
        }
    }

    /// Seed a full window from a pre-built series (used by `--demo`).
    ///
    /// The fixtures are authored at chart resolution, not ring resolution, so
    /// they are stretched across the ring by nearest neighbour. Reducing that
    /// back down with [`downsample_max`] returns the original shape exactly —
    /// a max over repeated values is the value — which is what keeps the demo
    /// screenshots identical to the handoff's while the ring underneath grew.
    pub fn from_series(series: &[f32]) -> Self {
        let mut h = Self::new();
        if series.is_empty() {
            return h;
        }
        for i in 0..HISTORY_BUCKETS {
            let src = i * series.len() / HISTORY_BUCKETS;
            h.cols.push_back(series[src.min(series.len() - 1)]);
        }
        h.has_data = true;
        h
    }

    /// Feed one sample from the sampler thread.
    pub fn push_sample(&mut self, dbfs: f32) {
        self.bucket = self.bucket.max(dbfs);
        self.bucket_has_sample = true;
        self.has_data = true;
    }

    /// Close the current column and start a new one. Called every
    /// [`BUCKET_SECS`]. A bucket with no samples holds the floor rather than
    /// carrying the previous peak forward, so a stopped stream visibly decays.
    pub fn commit(&mut self) {
        let v = if self.bucket_has_sample { self.bucket } else { CAPTURE_FLOOR_DBFS };
        if self.cols.len() == HISTORY_BUCKETS {
            self.cols.pop_front();
        }
        self.cols.push_back(v);
        self.bucket = f32::NEG_INFINITY;
        self.bucket_has_sample = false;
    }

    pub fn has_data(&self) -> bool {
        self.has_data
    }

    /// The window, oldest first, left-padded to [`HISTORY_BUCKETS`] with the
    /// floor so a freshly started process doesn't draw a misleading full-width
    /// chart. Reduce to the chart's width with [`downsample_max`].
    pub fn series(&self) -> Vec<f32> {
        let mut out = vec![CAPTURE_FLOOR_DBFS; HISTORY_BUCKETS.saturating_sub(self.cols.len())];
        out.extend(self.cols.iter().copied());
        out
    }

    /// The window reduced to exactly `width` columns, peak-preserving.
    pub fn columns(&self, width: usize) -> Vec<f32> {
        downsample_max(&self.series(), width)
    }

    /// Peak-hold across the window.
    pub fn peak(&self) -> Option<f32> {
        let live = self.bucket_has_sample.then_some(self.bucket);
        self.cols
            .iter()
            .copied()
            .chain(live)
            .filter(|v| v.is_finite())
            .fold(None::<f32>, |acc, v| Some(acc.map_or(v, |a| a.max(v))))
    }

    /// Most recent committed or in-flight value.
    pub fn current(&self) -> Option<f32> {
        if self.bucket_has_sample { Some(self.bucket) } else { self.cols.back().copied() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation_is_in_db_space() {
        assert_eq!(norm(-60.0, -60.0), 0.0);
        assert_eq!(norm(0.0, -60.0), 1.0);
        assert_eq!(norm(-30.0, -60.0), 0.5);
        // Below the floor and above full scale both clamp.
        assert_eq!(norm(-90.0, -60.0), 0.0);
        assert_eq!(norm(6.0, -60.0), 1.0);
        // And a lowered floor rescales rather than clipping.
        assert_eq!(norm(-45.0, -90.0), 0.5);
        assert_eq!(norm(-90.0, -90.0), 0.0);
    }

    #[test]
    fn downsample_keeps_peaks() {
        let src = vec![-60.0, -60.0, -3.0, -60.0, -60.0, -60.0];
        let out = downsample_max(&src, 3);
        assert_eq!(out.len(), 3);
        // The -3.0 transient must survive; a mean would bury it.
        assert!(out.contains(&-3.0), "peak lost: {out:?}");
    }

    #[test]
    fn downsample_covers_every_input_sample() {
        let src: Vec<f32> = (0..78).map(|i| i as f32 - 60.0).collect();
        let out = downsample_max(&src, 9);
        assert_eq!(out.len(), 9);
        // The global maximum must appear in the output.
        assert_eq!(out.iter().copied().fold(f32::MIN, f32::max), 17.0);
    }

    #[test]
    fn history_window_is_exactly_sixty_seconds() {
        let buckets = HISTORY_BUCKETS as f32;
        assert!((buckets * BUCKET_SECS - WINDOW_SECS).abs() < 1e-4);
        // And one bucket is one sample at the sampler's rate.
        let sample_secs = 1.0 / crate::app::SAMPLE_HZ as f32;
        assert!((BUCKET_SECS - sample_secs).abs() < 1e-4, "bucket is {BUCKET_SECS}s");
    }

    #[test]
    fn history_pads_left_and_evicts_oldest() {
        let mut h = History::new();
        h.push_sample(-10.0);
        h.commit();
        let s = h.series();
        assert_eq!(s.len(), HISTORY_BUCKETS);
        assert_eq!(*s.last().unwrap(), -10.0);
        assert_eq!(s[0], CAPTURE_FLOOR_DBFS);

        for i in 0..HISTORY_BUCKETS + 20 {
            h.push_sample(-(i as f32));
            h.commit();
        }
        assert_eq!(h.series().len(), HISTORY_BUCKETS);
    }

    /// The ring is one resolution; the chart is whatever the terminal is. The
    /// reduction has to work at every width, including wider than the ring.
    #[test]
    fn the_chart_reduces_to_any_width() {
        let mut h = History::new();
        for i in 0..HISTORY_BUCKETS {
            h.push_sample(if i == 600 { -3.0 } else { -50.0 });
            h.commit();
        }
        for width in [9usize, 78, 200, 400, 1200, 2000] {
            let c = h.columns(width);
            assert_eq!(c.len(), width, "width {width}");
            // The transient survives every reduction — that is the whole point
            // of reducing by max rather than by mean.
            let top = c.iter().copied().fold(f32::MIN, f32::max);
            assert_eq!(top, -3.0, "peak lost at width {width}");
        }
    }

    /// A demo fixture authored at 78 columns must come back out at 78 columns
    /// unchanged, however big the ring underneath it is.
    #[test]
    fn fixtures_survive_the_round_trip_through_the_ring() {
        let src: Vec<f32> = (0..78).map(|i| -60.0 + i as f32 * 0.5).collect();
        let h = History::from_series(&src);
        assert_eq!(h.columns(78), src, "fixture changed shape in the ring");
    }

    /// A silent bucket must record real silence, not the display floor, or
    /// lowering the floor redraws history that never happened.
    #[test]
    fn silence_is_recorded_below_every_display_floor() {
        let mut h = History::new();
        h.commit();
        let quietest = *h.series().last().unwrap();
        for floor in [-40.0, -60.0, -90.0, -120.0] {
            assert_eq!(norm(quietest, floor), 0.0, "silence is not at the bottom at {floor}");
        }
    }

    #[test]
    fn empty_bucket_decays_to_floor() {
        let mut h = History::new();
        h.push_sample(-6.0);
        h.commit();
        h.commit(); // a bucket with no samples
        assert_eq!(*h.series().last().unwrap(), CAPTURE_FLOOR_DBFS);
        // Peak-hold still remembers the window.
        assert_eq!(h.peak(), Some(-6.0));
    }
}
