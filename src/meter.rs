//! Level history, dB-space normalisation, and the block-glyph meters.
//!
//! Two things here differ from LITE.md, both deliberately.
//!
//! **Window arithmetic.** The spec asks for "a 60-sample-per-minute ring" driving
//! a chart that is 78 columns wide and labelled `60s ago -> now`; 60 samples
//! cannot fill 78 columns. (The reference renderer generates 78.) Resolved by
//! keeping 78 columns of 60/78 = 769ms each, so the window really is 60 seconds
//! and one column really is one column.
//!
//! **The floor glyph.** The spec's meter algorithm yields a blank column at the
//! -60 dBFS floor while its sparkline algorithm yields `▁` for the same input —
//! the same silence drawn two ways on one screen. Resolved toward the baseline:
//! a signal at the floor draws `▁`, and truly blank means "no data", which is
//! visually distinct and matches the `--` degradation rule.

use std::collections::VecDeque;

/// The bottom of the scale. Right for speech and music; the full tool serves
/// noise-floor work.
pub const FLOOR_DBFS: f32 = -60.0;
/// One column per chart cell, content columns 1..=78.
pub const HISTORY_COLS: usize = 78;
/// The window the time axis advertises.
pub const WINDOW_SECS: f32 = 60.0;
/// Seconds of signal aggregated into one column.
pub const BUCKET_SECS: f32 = WINDOW_SECS / HISTORY_COLS as f32;

/// dB-space normalisation to 0..=1. dBFS is logarithmic and the chart is not;
/// normalising in dB space before the glyph conversion is what keeps the meter
/// honest. Plotting raw amplitude makes every meter a liar.
pub fn norm(dbfs: f32) -> f32 {
    if !dbfs.is_finite() {
        return 0.0;
    }
    ((dbfs - FLOOR_DBFS) / -FLOOR_DBFS).clamp(0.0, 1.0)
}

/// One meter column, bottom row first. `None` is an unpainted cell.
pub fn column(dbfs: f32, height: u16, blocks: &[char; 8]) -> Vec<Option<char>> {
    let h = height as usize;
    let subs = (norm(dbfs) * h as f32 * 8.0).round() as usize;
    let full = subs / 8;
    let rem = subs % 8;
    let mut out = vec![None; h];
    for (i, cell) in out.iter_mut().enumerate() {
        if i < full {
            *cell = Some(blocks[7]);
        } else if i == full && rem > 0 {
            *cell = Some(blocks[rem - 1]);
        }
    }
    // A signal sitting at the floor is still a signal: draw the baseline.
    if subs == 0 && h > 0 {
        out[0] = Some(blocks[0]);
    }
    out
}

/// One sparkline cell.
pub fn spark(dbfs: f32, blocks: &[char; 8]) -> char {
    let idx = (norm(dbfs) * 7.0).round().clamp(0.0, 7.0) as usize;
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
        return vec![FLOOR_DBFS; n];
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
            cols: VecDeque::with_capacity(HISTORY_COLS),
            bucket: f32::NEG_INFINITY,
            bucket_has_sample: false,
            has_data: false,
        }
    }

    /// Seed a full window from a pre-built series (used by `--demo`).
    pub fn from_series(series: &[f32]) -> Self {
        let mut h = Self::new();
        for &v in series.iter().rev().take(HISTORY_COLS).rev() {
            h.cols.push_back(v);
        }
        h.has_data = !h.cols.is_empty();
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
        let v = if self.bucket_has_sample { self.bucket } else { FLOOR_DBFS };
        if self.cols.len() == HISTORY_COLS {
            self.cols.pop_front();
        }
        self.cols.push_back(v);
        self.bucket = f32::NEG_INFINITY;
        self.bucket_has_sample = false;
    }

    pub fn has_data(&self) -> bool {
        self.has_data
    }

    /// The window, oldest first, left-padded to [`HISTORY_COLS`] with the floor
    /// so a freshly started process doesn't draw a misleading full-width chart.
    pub fn series(&self) -> Vec<f32> {
        let mut out = vec![FLOOR_DBFS; HISTORY_COLS.saturating_sub(self.cols.len())];
        out.extend(self.cols.iter().copied());
        out
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
    const B: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

    #[test]
    fn normalisation_is_in_db_space() {
        assert_eq!(norm(-60.0), 0.0);
        assert_eq!(norm(0.0), 1.0);
        assert_eq!(norm(-30.0), 0.5);
        // Below the floor and above full scale both clamp.
        assert_eq!(norm(-90.0), 0.0);
        assert_eq!(norm(6.0), 1.0);
    }

    #[test]
    fn full_scale_fills_every_row() {
        let c = column(0.0, 3, &B);
        assert_eq!(c, vec![Some('█'), Some('█'), Some('█')]);
    }

    #[test]
    fn floor_draws_a_baseline_not_a_void() {
        // The spec's algorithm yields all-None here; the sparkline yields '▁'.
        // Both now agree on the baseline.
        assert_eq!(column(-60.0, 3, &B), vec![Some('▁'), None, None]);
        assert_eq!(spark(-60.0, &B), '▁');
    }

    #[test]
    fn partial_levels_use_the_right_sub_block() {
        // -30 dBFS on a 3-row meter: norm 0.5 -> 12 subs -> 1 full + 4 rem.
        assert_eq!(column(-30.0, 3, &B), vec![Some('█'), Some('▄'), None]);
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
        let cols = HISTORY_COLS as f32;
        assert!((cols * BUCKET_SECS - WINDOW_SECS).abs() < 1e-4);
    }

    #[test]
    fn history_pads_left_and_evicts_oldest() {
        let mut h = History::new();
        h.push_sample(-10.0);
        h.commit();
        let s = h.series();
        assert_eq!(s.len(), HISTORY_COLS);
        assert_eq!(*s.last().unwrap(), -10.0);
        assert_eq!(s[0], FLOOR_DBFS);

        for i in 0..HISTORY_COLS + 20 {
            h.push_sample(-(i as f32));
            h.commit();
        }
        assert_eq!(h.series().len(), HISTORY_COLS);
    }

    #[test]
    fn empty_bucket_decays_to_floor() {
        let mut h = History::new();
        h.push_sample(-6.0);
        h.commit();
        h.commit(); // a bucket with no samples
        assert_eq!(*h.series().last().unwrap(), FLOOR_DBFS);
        // Peak-hold still remembers the window.
        assert_eq!(h.peak(), Some(-6.0));
    }
}
