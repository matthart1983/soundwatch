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
#[derive(Debug, Default)]
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
    /// Latched the first time any sample is non-zero. Never cleared: it is the
    /// evidence that consent was granted, and one sample is enough.
    pub ever_signal: AtomicBool,
}

/// The IOProc. Real-time thread: no allocation, no locks, no syscalls.
extern "C" fn peak_io_proc(
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
    unsafe {
        let list = &*input;
        let buffers =
            std::slice::from_raw_parts(list.buffers.as_ptr(), list.number_buffers as usize);
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
            }
        }
    }
    if peak > 0.0 {
        state.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        state.ever_signal.store(true, Ordering::Relaxed);
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

    /// Peak since the last call, in dBFS. `None` until the first block arrives.
    pub fn peak_dbfs(&self) -> Option<f32> {
        // No callback yet means no reading — distinct from a reading of silence.
        if self.state.calls.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let bits = self.state.peak.swap(0, Ordering::Relaxed);
        if bits == 0 {
            // Silence is a real reading, not a missing one.
            return Some(crate::meter::FLOOR_DBFS);
        }
        let linear = f32::from_bits(bits);
        Some((20.0 * linear.log10()).clamp(crate::meter::FLOOR_DBFS, 0.0))
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
        assert_eq!(m.peak_dbfs(), Some(crate::meter::FLOOR_DBFS));
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
        assert_eq!(b.peak_dbfs(), Some(crate::meter::FLOOR_DBFS), "b read a's signal");
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

    #[test]
    fn peak_bits_compare_monotonically_as_integers() {
        // The lock-free fetch_max depends on this.
        let vals = [0.0f32, 1e-6, 0.01, 0.5, 0.999, 1.0];
        for w in vals.windows(2) {
            assert!(w[0].to_bits() < w[1].to_bits(), "{} vs {}", w[0], w[1]);
        }
    }
}
