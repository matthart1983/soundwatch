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
use std::sync::atomic::Ordering;

use super::level::MeterState;
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
    if client.is_null() || input.is_null() {
        return 0;
    }
    // SAFETY: `client` is the `Arc<MeterState>` pointer given to
    // AudioDeviceCreateIOProcID, kept alive by the IoProcMeter that owns it
    // until after AudioDeviceStop has returned.
    let state = unsafe { &*(client as *const MeterState) };

    unsafe {
        let list = &*input;
        let buffers =
            std::slice::from_raw_parts(list.buffers.as_ptr(), list.number_buffers as usize);
        for b in buffers {
            if b.data.is_null() {
                continue;
            }
            let n = b.data_byte_size as usize / size_of::<f32>();
            // One pass per buffer through the shared fold, so both platforms
            // measure the same thing from the same code.
            state.fold(std::slice::from_raw_parts(b.data as *const f32, n));
        }
    }
    0
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
        self.state.stats()
    }

    /// Has any sample, ever, been non-zero?
    ///
    /// This is the consent signal. A tap that was refused delivers a perfect
    /// stream of zeros with no error anywhere, so the absence of this — over a
    /// long enough window, while something is playing — is the only evidence
    /// available that the meter is not really listening.
    pub fn has_ever_heard_signal(&self) -> bool {
        self.state.has_ever_heard_signal()
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
        self.state.take_peak_dbfs()
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
    use crate::backend::level::RING_FRAMES;

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
