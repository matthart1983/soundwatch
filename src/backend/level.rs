//! The level state both platforms share.
//!
//! The two backends get their samples from very different places — a CoreAudio
//! IOProc on a thread the OS owns, and a blocking `pa_simple_read` on one of
//! ours — but what they do with them is identical: fold a peak, accumulate a
//! sum of squares, and keep a ring of recent audio for the spectrum. That is
//! all here, so neither platform can drift into measuring a different thing.
//!
//! Every field is an atomic and the whole type is `Sync`, which is what lets
//! the macOS side hand a pointer to it into a real-time callback and the Linux
//! side share it with a reader thread.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Everything a level meter accumulates, on either platform.
///
/// Fields are `pub(crate)` rather than private so the tests can drive them
/// directly: several of these invariants — the ring's lap detection, the RMS
/// seqlock — are only testable by putting the state into a position the public
/// API cannot reach.
///
/// One allocation per meter, reached through the IOProc's client pointer. It
/// was tempting to make these `static` — there is only ever one tap — but a
/// global silently couples any two meters that do coexist, which is exactly
/// what the test suite does, and a metering bug that only shows up under
/// `cargo test` is the worst kind.
#[derive(Debug)]
pub struct MeterState {
    /// Peak magnitude since the last read, as `f32` bits. Positive `f32` bit
    /// patterns are monotonic when compared as `u32`, so `fetch_max` gives a
    /// correct running peak without a lock — and reading swaps in zero, so no
    /// transient is counted twice or missed between 20 Hz samples.
    pub peak: AtomicU32,
    /// IOProc invocations and total samples seen. Diagnostic: they separate
    /// "the callback never fires" from "the callback fires and delivers zeros",
    /// which is how a tap without consent behaves — silence, not an error.
    pub calls: AtomicU64,
    pub frames: AtomicU64,
    /// Sum of squares and sample count since the last read, for RMS.
    ///
    /// Peak tells you whether you are clipping; RMS tells you how loud it
    /// actually is, and the gap between them is the crest factor — which is how
    /// you tell a compressed master from a live take at the same peak level.
    ///
    /// Both are **cumulative** and are published together under `rms_seq`, a
    /// seqlock. Reading them as two independent atomics cannot be made
    /// correct in either order: whichever is taken first, a callback landing
    /// between the two reads pairs a sum with a count that does not describe
    /// it. In the worst interleaving the reader takes a sum of zero against a
    /// non-zero count and reports -120 dBFS — a silent frame, from a signal
    /// that was not silent, with a crest factor of a hundred decibels beside
    /// it. The writer never spins (two stores either side of the update); the
    /// reader retries a bounded number of times and gives up rather than
    /// blocking the audio thread's progress.
    pub(crate) sum_sq: AtomicU64,
    pub(crate) sum_n: AtomicU64,
    /// Even between updates, odd during one.
    pub(crate) rms_seq: AtomicU64,
    /// What the reader has already accounted for. Only the reader writes
    /// these, so they need no synchronisation of their own.
    pub(crate) read_sq: AtomicU64,
    pub(crate) read_n: AtomicU64,
    /// Latched the first time any sample is non-zero. Never cleared: it is the
    /// evidence that consent was granted, and one sample is enough.
    pub ever_signal: AtomicBool,

    /// A mono mixdown of recent audio, for the spectrum screen.
    ///
    /// `AtomicU32` holding `f32` bits rather than an `UnsafeCell<[f32]>`: on
    /// arm64 a relaxed store compiles to a plain store, so five hundred of them
    /// per callback costs nothing measurable, and it keeps the real-time path
    /// free of hand-written aliasing arguments.
    pub(crate) ring: Box<[AtomicU32]>,
    /// Total samples ever written. Monotonic, never wrapped, so a reader can
    /// tell whether it was lapped mid-copy.
    pub(crate) written: AtomicU64,
    /// Frames the reader lost to the writer lapping it. Observable rather than
    /// silent, on the same principle as `ever_signal`.
    pub overruns: AtomicU64,
}

/// Samples kept for the spectrum: 341 ms at 48 kHz, four analysis windows.
pub const RING_FRAMES: usize = 16_384;

impl Default for MeterState {
    fn default() -> Self {
        Self {
            peak: AtomicU32::new(0),
            calls: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            ever_signal: AtomicBool::new(false),
            sum_sq: AtomicU64::new(0),
            sum_n: AtomicU64::new(0),
            rms_seq: AtomicU64::new(0),
            read_sq: AtomicU64::new(0),
            read_n: AtomicU64::new(0),
            ring: (0..RING_FRAMES).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
        }
    }
}

impl MeterState {
    /// RMS since the last call, in dBFS, clearing as it reads.
    ///
    /// Lives here rather than on the meter so it can be exercised
    /// concurrently: `IoProcMeter` holds raw CoreAudio handles and is
    /// deliberately not `Sync`, but this is nothing but atomics.
    pub fn take_rms_dbfs(&self) -> Option<f32> {
        // Seqlock read: a pair taken while the writer was mid-update is
        // discarded rather than used. Bounded, because the caller is a 20 Hz
        // sampler and a missed frame is a missed frame, not a stall.
        // The mirror of the writer's barrier: an *acquire load* on the second
        // read orders what comes after it, not the data loads that came
        // before, so those could be satisfied after the validation and defeat
        // the check. The fence goes between the data and the re-read.
        let mut pair = None;
        for _ in 0..8 {
            let before = self.rms_seq.load(Ordering::Acquire);
            if !before.is_multiple_of(2) {
                continue;
            }
            let sum = f64::from_bits(self.sum_sq.load(Ordering::Relaxed));
            let n = self.sum_n.load(Ordering::Relaxed);
            std::sync::atomic::fence(Ordering::Acquire);
            if self.rms_seq.load(Ordering::Relaxed) == before {
                pair = Some((sum, n));
                break;
            }
        }
        let (total_sq, total_n) = pair?;

        // Cumulative totals, so the reader subtracts what it has already had
        // rather than resetting shared state the writer is still adding to.
        let seen_sq = f64::from_bits(self.read_sq.load(Ordering::Relaxed));
        let seen_n = self.read_n.load(Ordering::Relaxed);
        self.read_sq.store(total_sq.to_bits(), Ordering::Relaxed);
        self.read_n.store(total_n, Ordering::Relaxed);

        let n = total_n.wrapping_sub(seen_n);
        let sum = total_sq - seen_sq;
        if n == 0 {
            return None;
        }
        let rms = (sum.max(0.0) / n as f64).sqrt() as f32;
        if !rms.is_finite() || rms <= 0.0 {
            return Some(crate::meter::CAPTURE_FLOOR_DBFS);
        }
        Some((20.0 * rms.log10()).clamp(crate::meter::CAPTURE_FLOOR_DBFS, 0.0))
    }

    /// The most recent `n` samples, oldest first.
    ///
    /// Lock-free and wait-free on both sides: the writer never coordinates with
    /// anyone, and this validates afterwards. If the writer lapped us during
    /// the copy the frame is discarded rather than stitched together out of two
    /// different moments — a torn window is a spectrum of a signal that never
    /// existed.
    pub fn recent(&self, n: usize) -> Option<Vec<f32>> {
        let n = n.min(RING_FRAMES);
        let end = self.written.load(Ordering::Acquire);
        if (end as usize) < n {
            return None;
        }
        let start = end - n as u64;
        let out: Vec<f32> = (0..n)
            .map(|i| {
                let idx = ((start + i as u64) as usize) % RING_FRAMES;
                f32::from_bits(self.ring[idx].load(Ordering::Relaxed))
            })
            .collect();
        // Did the writer get more than a whole buffer ahead while we copied?
        let now = self.written.load(Ordering::Acquire);
        if now - start > RING_FRAMES as u64 {
            self.overruns.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        Some(out)
    }
}
impl MeterState {
    /// Fold one block of samples: peak, sum of squares, and the spectrum ring.
    ///
    /// Written once and called from both platforms. On macOS this runs inside
    /// a real-time callback, so it allocates nothing, takes no lock, and makes
    /// no syscall — constraints the Linux caller does not have but does not
    /// mind either.
    pub fn fold(&self, samples: &[f32]) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if samples.is_empty() {
            return;
        }
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        let mut write = self.written.load(Ordering::Relaxed);
        for s in samples {
            let a = s.abs();
            if a > peak {
                peak = a;
            }
            // A non-finite sample would otherwise poison the accumulator: NaN
            // survives the sqrt and `clamp` does not clamp it.
            if s.is_finite() {
                sum_sq += (*s as f64) * (*s as f64);
            }
            let idx = (write as usize) % RING_FRAMES;
            self.ring[idx].store(s.to_bits(), Ordering::Relaxed);
            write += 1;
        }
        self.frames.fetch_add(samples.len() as u64, Ordering::Relaxed);
        self.written.store(write, Ordering::Release);

        if peak > 0.0 {
            self.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
            self.ever_signal.store(true, Ordering::Relaxed);
        }
        self.add_squares(sum_sq, samples.len() as u64);
    }

    /// Publish a block's sum of squares and its count together. See the
    /// seqlock note on the fields.
    pub fn add_squares(&self, sum_sq: f64, n: u64) {
        if n == 0 {
            return;
        }
        let seq = self.rms_seq.load(Ordering::Relaxed);
        self.rms_seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        let prev = f64::from_bits(self.sum_sq.load(Ordering::Relaxed));
        self.sum_sq.store((prev + sum_sq).to_bits(), Ordering::Relaxed);
        self.sum_n.store(self.sum_n.load(Ordering::Relaxed).wrapping_add(n), Ordering::Relaxed);
        self.rms_seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    /// Peak since the last call, in dBFS. `None` until the first block.
    pub fn take_peak_dbfs(&self) -> Option<f32> {
        if self.calls.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let bits = self.peak.swap(0, Ordering::Relaxed);
        if bits == 0 {
            // Silence is a real reading, not a missing one.
            return Some(crate::meter::CAPTURE_FLOOR_DBFS);
        }
        let linear = f32::from_bits(bits);
        Some((20.0 * linear.log10()).clamp(crate::meter::CAPTURE_FLOOR_DBFS, 0.0))
    }

    pub fn has_ever_heard_signal(&self) -> bool {
        self.ever_signal.load(Ordering::Relaxed)
    }

    /// `(callbacks, samples)` since start. A diagnostic for `--probe-tap`,
    /// which only one platform has.
    #[allow(dead_code)]
    pub fn stats(&self) -> (u64, u64) {
        (self.calls.load(Ordering::Relaxed), self.frames.load(Ordering::Relaxed))
    }
}
