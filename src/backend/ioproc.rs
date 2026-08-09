//! A peak meter attached to a CoreAudio device, and the real-time callback
//! behind it.
//!
//! Two very different things need this: the process tap (a private aggregate
//! device wrapping a tap of the system mix) and the input meter (a real capture
//! device). Both end up doing the same thing — attach an IOProc, fold the
//! absolute maximum of every block into an atomic, never allocate — so the
//! mechanism lives here once and the two callers differ only in what device
//! they hand over.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use super::ffi::{AudioObjectID, OSStatus};

#[repr(C)]
pub struct AudioTimeStamp {
    _opaque: [u8; 64],
}

#[repr(C)]
pub struct AudioBufferRaw {
    pub number_channels: u32,
    pub data_byte_size: u32,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct AudioBufferListRaw {
    pub number_buffers: u32,
    pub buffers: [AudioBufferRaw; 1],
}

pub type AudioDeviceIOProc = extern "C" fn(
    device: AudioObjectID,
    now: *const AudioTimeStamp,
    input: *const AudioBufferListRaw,
    input_time: *const AudioTimeStamp,
    output: *mut AudioBufferListRaw,
    output_time: *const AudioTimeStamp,
    client: *mut c_void,
) -> OSStatus;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioDeviceCreateIOProcID(
        device: AudioObjectID,
        proc_: AudioDeviceIOProc,
        client: *mut c_void,
        out_id: *mut *mut c_void,
    ) -> OSStatus;
    fn AudioDeviceDestroyIOProcID(device: AudioObjectID, id: *mut c_void) -> OSStatus;
    fn AudioDeviceStart(device: AudioObjectID, id: *mut c_void) -> OSStatus;
    fn AudioDeviceStop(device: AudioObjectID, id: *mut c_void) -> OSStatus;
}

/// Everything the real-time callback touches.
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
    sum_sq: AtomicU64,
    sum_n: AtomicU64,
    /// Even between updates, odd during one.
    rms_seq: AtomicU64,
    /// What the reader has already accounted for. Only the reader writes
    /// these, so they need no synchronisation of their own.
    read_sq: AtomicU64,
    read_n: AtomicU64,
    /// Latched the first time any sample is non-zero. Never cleared: it is the
    /// evidence that consent was granted, and one sample is enough.
    pub ever_signal: AtomicBool,

    /// A mono mixdown of recent audio, for the spectrum screen.
    ///
    /// `AtomicU32` holding `f32` bits rather than an `UnsafeCell<[f32]>`: on
    /// arm64 a relaxed store compiles to a plain store, so five hundred of them
    /// per callback costs nothing measurable, and it keeps the real-time path
    /// free of hand-written aliasing arguments.
    ring: Box<[AtomicU32]>,
    /// Total samples ever written. Monotonic, never wrapped, so a reader can
    /// tell whether it was lapped mid-copy.
    written: AtomicU64,
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

/// The IOProc. Real-time thread: no allocation, no locks, no syscalls.
///
/// A panic here does not unwind into CoreAudio — since Rust 1.81 an
/// `extern "C"` function aborts the process instead, and a diagnostic tool
/// that kills itself while you are diagnosing something is worse than useless.
/// The body below is written to have nothing that can panic (every slice comes
/// from a length derived from `data_byte_size`, every index is bounds-checked
/// or taken modulo, the one division is guarded), and `catch_unwind` is a
/// backstop for the edit that overlooks that. It costs nothing when nothing
/// panics.
///
/// It is also only a backstop if panics *unwind*, which is why the release
/// profile no longer sets `panic = "abort"` — see `Cargo.toml`.
extern "C" fn peak_io_proc(
    device: AudioObjectID,
    now: *const AudioTimeStamp,
    input: *const AudioBufferListRaw,
    input_time: *const AudioTimeStamp,
    output: *mut AudioBufferListRaw,
    output_time: *const AudioTimeStamp,
    client: *mut c_void,
) -> OSStatus {
    let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        peak_io_proc_inner(device, now, input, input_time, output, output_time, client)
    }));
    // A panic that got this far has already printed through the hook; there is
    // nothing safe to do on this thread but return quietly and let the level
    // freeze, which the UI reports as a meter that has stopped moving.
    guarded.unwrap_or(0)
}

fn peak_io_proc_inner(
    _device: AudioObjectID,
    _now: *const AudioTimeStamp,
    input: *const AudioBufferListRaw,
    _input_time: *const AudioTimeStamp,
    _output: *mut AudioBufferListRaw,
    _output_time: *const AudioTimeStamp,
    client: *mut c_void,
) -> OSStatus {
    if client.is_null() {
        return 0;
    }
    // SAFETY: `client` is the `Arc<MeterState>` pointer given to
    // AudioDeviceCreateIOProcID, kept alive by the IoProcMeter that owns it
    // until after AudioDeviceStop has returned.
    let state = unsafe { &*(client as *const MeterState) };

    state.calls.fetch_add(1, Ordering::Relaxed);
    if input.is_null() {
        return 0;
    }
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f64;
    let mut sample_count = 0u64;
    unsafe {
        let list = &*input;
        let buffers =
            std::slice::from_raw_parts(list.buffers.as_ptr(), list.number_buffers as usize);

        // Frames in this block, from the widest buffer. Buffers may be
        // deinterleaved (one per channel) or a single interleaved one.
        let mut write = state.written.load(Ordering::Relaxed);

        for b in buffers {
            if b.data.is_null() {
                continue;
            }
            let n = b.data_byte_size as usize / size_of::<f32>();
            state.frames.fetch_add(n as u64, Ordering::Relaxed);
            let samples = std::slice::from_raw_parts(b.data as *const f32, n);
            for s in samples {
                let a = s.abs();
                if a > peak {
                    peak = a;
                }
                // A non-finite sample from a misbehaving driver would
                // otherwise poison the whole accumulator: NaN propagates
                // through the sqrt and out through `clamp`, which does not
                // clamp NaN, and the meter prints `NaN dBFS`.
                if s.is_finite() {
                    sum_sq += (*s as f64) * (*s as f64);
                }
            }
            sample_count += n_samples_of(b);
        }

        // A mono mixdown for the spectrum: sum the buffers frame by frame and
        // scale by the channel count, so a centred signal does not read 6 dB
        // hot. Written after the peak fold so the two never disagree about
        // which block they are describing.
        let chans = buffers.iter().filter(|b| !b.data.is_null()).count().max(1);
        let frames = buffers
            .iter()
            .filter(|b| !b.data.is_null())
            .map(|b| {
                (b.data_byte_size as usize / size_of::<f32>()) / (b.number_channels as usize).max(1)
            })
            .min()
            .unwrap_or(0);
        for f in 0..frames {
            let mut sum = 0.0f32;
            let mut taken = 0usize;
            for b in buffers {
                if b.data.is_null() {
                    continue;
                }
                let ch = (b.number_channels as usize).max(1);
                let n = b.data_byte_size as usize / size_of::<f32>();
                let samples = std::slice::from_raw_parts(b.data as *const f32, n);
                // Interleaved within a buffer, deinterleaved across buffers.
                for c in 0..ch {
                    let idx = f * ch + c;
                    if idx < n {
                        sum += samples[idx];
                        taken += 1;
                    }
                }
            }
            let v = if taken > 0 { sum / taken as f32 } else { 0.0 };
            let idx = (write as usize) % RING_FRAMES;
            state.ring[idx].store(v.to_bits(), Ordering::Relaxed);
            write += 1;
            let _ = chans;
        }
        // Release: everything above must be visible before the count that
        // advertises it.
        state.written.store(write, Ordering::Release);
    }
    if peak > 0.0 {
        state.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        state.ever_signal.store(true, Ordering::Relaxed);
    }
    // Accumulate rather than replace: several callbacks may land between two
    // reads, and an RMS over the newest block alone would be a different
    // measurement each time depending on the buffer size.
    if sample_count > 0 {
        // Seqlock write: odd while the pair is inconsistent, even when it is
        // publishable again. No loop and no blocking, which is the only
        // acceptable shape on this thread.
        //
        // The fence is load-bearing and easy to get wrong. A *release store*
        // on the odd marker orders everything before it, and does nothing to
        // stop the data stores after it from becoming visible first — which on
        // arm64 they can. A reader would then see an even sequence either side
        // of a sum that already includes a block whose count is still missing:
        // the exact torn pair this whole mechanism exists to prevent, just far
        // rarer. The barrier has to sit *between* the marker and the data.
        let seq = state.rms_seq.load(Ordering::Relaxed);
        state.rms_seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        let prev = f64::from_bits(state.sum_sq.load(Ordering::Relaxed));
        state.sum_sq.store((prev + sum_sq).to_bits(), Ordering::Relaxed);
        state.sum_n.store(
            state.sum_n.load(Ordering::Relaxed).wrapping_add(sample_count),
            Ordering::Relaxed,
        );
        state.rms_seq.store(seq.wrapping_add(2), Ordering::Release);
    }
    0
}

/// Samples in one buffer, for the RMS denominator.
fn n_samples_of(b: &AudioBufferRaw) -> u64 {
    (b.data_byte_size as usize / size_of::<f32>()) as u64
}

/// A running peak meter on one device. Dropping it stops and detaches cleanly.
pub struct IoProcMeter {
    device: AudioObjectID,
    proc_id: *mut c_void,
    running: bool,
    state: Arc<MeterState>,
    started: Instant,
}

// The IOProc runs on a CoreAudio thread and communicates only through the
// atomics in `state`; the handles are touched only from the owning thread.
unsafe impl Send for IoProcMeter {}

impl IoProcMeter {
    /// Attach to `device` and start it.
    pub fn attach(device: AudioObjectID) -> Result<Self, String> {
        let mut me = Self {
            device,
            proc_id: std::ptr::null_mut(),
            running: false,
            state: Arc::new(MeterState::default()),
            started: Instant::now(),
        };

        // The IOProc reaches its counters through this pointer. The Arc lives
        // in `me`, whose Drop runs AudioDeviceStop before releasing it.
        let client = Arc::as_ptr(&me.state) as *mut c_void;
        let mut proc_id: *mut c_void = std::ptr::null_mut();
        let st = unsafe { AudioDeviceCreateIOProcID(device, peak_io_proc, client, &mut proc_id) };
        if st != 0 || proc_id.is_null() {
            return Err(format!("could not attach a meter (OSStatus {st})"));
        }
        me.proc_id = proc_id;

        let st = unsafe { AudioDeviceStart(device, me.proc_id) };
        if st != 0 {
            return Err(format!("could not start metering (OSStatus {st})"));
        }
        me.running = true;
        me.started = Instant::now();
        Ok(me)
    }

    /// `(io_proc calls, samples seen)` since start. Diagnostic.
    pub fn stats(&self) -> (u64, u64) {
        (self.state.calls.load(Ordering::Relaxed), self.state.frames.load(Ordering::Relaxed))
    }

    /// Has any sample, ever, been non-zero?
    ///
    /// This is the consent signal. A tap that was refused delivers a perfect
    /// stream of zeros with no error anywhere, so the absence of this — over a
    /// long enough window, while something is playing — is the only evidence
    /// available that the meter is not really listening.
    pub fn has_ever_heard_signal(&self) -> bool {
        self.state.ever_signal.load(Ordering::Relaxed)
    }

    /// Has the meter been running at least this long?
    pub fn has_run_for(&self, secs: u64) -> bool {
        self.started.elapsed().as_secs() >= secs
    }

    /// The most recent `n` samples of the mono mixdown, for the spectrum.
    pub fn recent(&self, n: usize) -> Option<Vec<f32>> {
        self.state.recent(n)
    }

    /// Frames the spectrum reader lost to the writer lapping it.
    pub fn overruns(&self) -> u64 {
        self.state.overruns.load(Ordering::Relaxed)
    }

    /// RMS since the last call, in dBFS, and the count it was taken over.
    /// Reading clears, so successive calls do not overlap.
    pub fn rms_dbfs(&self) -> Option<f32> {
        self.state.take_rms_dbfs()
    }

    /// Peak since the last call, in dBFS. `None` until the first block arrives.
    pub fn peak_dbfs(&self) -> Option<f32> {
        // No callback yet means no reading — distinct from a reading of silence.
        if self.state.calls.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let bits = self.state.peak.swap(0, Ordering::Relaxed);
        if bits == 0 {
            // Silence is a real reading, not a missing one. Recorded at the
            // capture floor rather than the display floor: what the meter draws
            // is configurable, what it heard is not.
            return Some(crate::meter::CAPTURE_FLOOR_DBFS);
        }
        let linear = f32::from_bits(bits);
        Some((20.0 * linear.log10()).clamp(crate::meter::CAPTURE_FLOOR_DBFS, 0.0))
    }

    /// A meter with no device behind it, for exercising the arithmetic.
    #[cfg(test)]
    pub fn detached() -> Self {
        Self {
            device: 0,
            proc_id: std::ptr::null_mut(),
            running: false,
            state: Arc::new(MeterState::default()),
            started: Instant::now(),
        }
    }
}

impl Drop for IoProcMeter {
    /// Order matters: the IOProc must be stopped and destroyed before `state`
    /// is released, or the real-time thread could dereference freed memory.
    /// Rust runs this body before dropping the fields, which is exactly that
    /// order — but it is the kind of thing a refactor breaks silently.
    fn drop(&mut self) {
        unsafe {
            if self.running && !self.proc_id.is_null() {
                AudioDeviceStop(self.device, self.proc_id);
            }
            if !self.proc_id.is_null() {
                AudioDeviceDestroyIOProcID(self.device, self.proc_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dbfs_conversion_is_sane() {
        let m = IoProcMeter::detached();
        // A meter that has never been called back reports no reading at all,
        // which is not the same as reporting silence.
        assert_eq!(m.peak_dbfs(), None, "no callback yet is not a level of -60");

        m.state.calls.store(1, Ordering::Relaxed);
        m.state.peak.store(1.0f32.to_bits(), Ordering::Relaxed);
        assert_eq!(m.peak_dbfs(), Some(0.0), "full scale must be 0 dBFS");

        m.state.peak.store(0.5f32.to_bits(), Ordering::Relaxed);
        let d = m.peak_dbfs().unwrap();
        assert!((d - -6.02).abs() < 0.05, "half amplitude should be about -6 dBFS, got {d}");

        // Reading clears, so silence follows.
        assert_eq!(m.peak_dbfs(), Some(crate::meter::CAPTURE_FLOOR_DBFS));
    }

    #[test]
    fn two_meters_do_not_share_a_reading() {
        // These counters used to be `static`, which made any second meter read
        // the first one's audio. Nothing in the API hinted at it.
        let a = IoProcMeter::detached();
        let b = IoProcMeter::detached();
        a.state.calls.store(1, Ordering::Relaxed);
        b.state.calls.store(1, Ordering::Relaxed);
        a.state.peak.store(1.0f32.to_bits(), Ordering::Relaxed);

        assert_eq!(a.peak_dbfs(), Some(0.0));
        assert_eq!(b.peak_dbfs(), Some(crate::meter::CAPTURE_FLOOR_DBFS), "b read a's signal");
    }

    #[test]
    fn silence_is_never_mistaken_for_consent() {
        let m = IoProcMeter::detached();
        assert!(!m.has_ever_heard_signal());
        // Zero-magnitude blocks must not latch the flag, however many arrive.
        m.state.calls.store(10_000, Ordering::Relaxed);
        assert!(!m.has_ever_heard_signal(), "digital silence is not evidence of consent");
        m.state.ever_signal.store(true, Ordering::Relaxed);
        assert!(m.has_ever_heard_signal());
    }

    /// The ring has to hand back exactly what the callback wrote, in order.
    #[test]
    fn the_ring_returns_the_most_recent_samples_in_order() {
        let m = IoProcMeter::detached();
        let st = &m.state;
        for i in 0..1000u32 {
            let idx = (i as usize) % RING_FRAMES;
            st.ring[idx].store((i as f32).to_bits(), Ordering::Relaxed);
        }
        st.written.store(1000, Ordering::Release);

        let got = st.recent(4).expect("1000 samples written, 4 requested");
        assert_eq!(got, vec![996.0, 997.0, 998.0, 999.0]);
    }

    #[test]
    fn the_ring_reports_nothing_until_it_has_a_full_window() {
        let m = IoProcMeter::detached();
        m.state.written.store(10, Ordering::Release);
        assert!(m.state.recent(64).is_none(), "returned a window it never recorded");
        assert!(m.state.recent(10).is_some());
    }

    /// A reader that gets lapped must discard the frame, not return a window
    /// stitched together from two different moments.
    #[test]
    fn being_lapped_discards_the_frame_and_is_counted() {
        let m = IoProcMeter::detached();
        let st = &m.state;
        // Pretend a great deal has been written since the window we want.
        st.written.store(RING_FRAMES as u64 * 4, Ordering::Release);
        let before = st.overruns.load(Ordering::Relaxed);
        // Ask for a window that starts further back than the ring holds.
        assert!(st.recent(RING_FRAMES).is_some(), "the newest full window is still valid");
        // Now simulate the writer running away mid-read by advancing after the
        // start index was taken: recent() re-reads and must notice.
        let start = st.written.load(Ordering::Acquire);
        st.written.store(start + RING_FRAMES as u64 + 1, Ordering::Release);
        assert_eq!(st.overruns.load(Ordering::Relaxed), before, "counted too eagerly");
    }

    #[test]
    fn rms_is_the_root_mean_square_and_clears_on_read() {
        let m = IoProcMeter::detached();
        // A full-scale sine has an RMS of 1/sqrt(2), which is -3.01 dBFS.
        let n = 4096usize;
        let mut sum = 0.0f64;
        for i in 0..n {
            let v = (std::f32::consts::TAU * 8.0 * i as f32 / n as f32).sin() as f64;
            sum += v * v;
        }
        m.state.sum_sq.store(sum.to_bits(), Ordering::Relaxed);
        m.state.sum_n.store(n as u64, Ordering::Relaxed);
        let db = m.rms_dbfs().expect("a reading");
        assert!((db + 3.01).abs() < 0.05, "full-scale sine RMS read {db} dBFS");
        // Reading clears, so there is nothing to double-count.
        assert_eq!(m.rms_dbfs(), None, "rms did not clear on read");
    }

    /// The guard on the callbacks is only a guard if panics unwind. Under
    /// `panic = "abort"` — which the release profile used to set — `catch_unwind`
    /// is never reached, so the shipped binary still died on a panic in a
    /// CoreAudio callback while the code claimed otherwise.
    #[test]
    fn a_panicking_callback_does_not_kill_the_process() {
        let caught = std::panic::catch_unwind(|| panic!("as a callback would"));
        assert!(caught.is_err(), "panics are not unwinding: catch_unwind is inert here");
    }

    /// The *writer's* NaN guard, which the reader-side test does not reach: a
    /// non-finite sample must never enter the accumulator in the first place.
    #[test]
    fn a_non_finite_sample_is_refused_by_the_callback() {
        let state = std::sync::Arc::new(MeterState::default());
        let mut samples = vec![0.5f32; 64];
        samples[7] = f32::NAN;
        samples[9] = f32::INFINITY;
        let mut buffers = AudioBufferListRaw {
            number_buffers: 1,
            buffers: [AudioBufferRaw {
                number_channels: 1,
                data_byte_size: (samples.len() * size_of::<f32>()) as u32,
                data: samples.as_mut_ptr() as *mut c_void,
            }],
        };
        peak_io_proc_inner(
            0,
            std::ptr::null(),
            &buffers,
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::sync::Arc::as_ptr(&state) as *mut c_void,
        );
        let _ = &mut buffers;
        let db = state.take_rms_dbfs().expect("a reading");
        assert!(db.is_finite(), "a NaN sample reached the accumulator: {db}");
        // 62 of 64 samples at half scale, two skipped: still about -6 dBFS.
        assert!((db + 6.1).abs() < 0.5, "read {db} dBFS");
    }

    #[test]
    fn a_non_finite_sample_cannot_poison_the_meter() {
        let m = IoProcMeter::detached();
        m.state.sum_sq.store(f64::NAN.to_bits(), Ordering::Relaxed);
        m.state.sum_n.store(64, Ordering::Relaxed);
        // NaN must come back as silence, not as `NaN dBFS` on the screen.
        assert_eq!(m.rms_dbfs(), Some(crate::meter::CAPTURE_FLOOR_DBFS));
    }

    /// The callback and the reader must not be able to pair a sum with the
    /// wrong count. Driven concurrently, every reading has to be a level that
    /// could actually have been measured.
    #[test]
    fn concurrent_accumulation_never_reports_an_impossible_level() {
        use std::sync::Arc;
        let m = Arc::new(MeterState::default());
        let writer = {
            let m = m.clone();
            std::thread::spawn(move || {
                for _ in 0..20_000 {
                    // Every sample is exactly half scale, so any honest
                    // reading is -6.02 dBFS whatever window it covers.
                    let block = 64.0f64 * 0.25;
                    let seq = m.rms_seq.load(Ordering::Relaxed);
                    m.rms_seq.store(seq.wrapping_add(1), Ordering::Release);
                    let prev = f64::from_bits(m.sum_sq.load(Ordering::Relaxed));
                    m.sum_sq.store((prev + block).to_bits(), Ordering::Relaxed);
                    m.sum_n.store(m.sum_n.load(Ordering::Relaxed) + 64, Ordering::Relaxed);
                    m.rms_seq.store(seq.wrapping_add(2), Ordering::Release);
                }
            })
        };
        for _ in 0..20_000 {
            if let Some(db) = m.take_rms_dbfs() {
                assert!(
                    (db + 6.02).abs() < 0.5,
                    "read {db} dBFS from a stream of half-scale samples"
                );
            }
        }
        writer.join().unwrap();
    }

    #[test]
    fn peak_bits_compare_monotonically_as_integers() {
        // The lock-free fetch_max depends on this.
        let vals = [0.0f32, 1e-6, 0.01, 0.5, 0.999, 1.0];
        for w in vals.windows(2) {
            assert!(w[0].to_bits() < w[1].to_bits(), "{} vs {}", w[0], w[1]);
        }
    }
}
