//! Level metering through libpulse's simple API.
//!
//! ## Why libpulse and not PipeWire directly
//!
//! PipeWire ships `libpulse` compatibility, and PulseAudio *is* libpulse, so
//! one small interface covers both of the servers anyone actually runs. The
//! native PipeWire API would mean a main loop, a registry, proxies and SPA POD
//! parsing — several hundred lines of FFI to reach the same numbers.
//!
//! ## Why `dlopen` and not a link
//!
//! A linked `libpulse-simple.so.0` is an undefined symbol the loader must
//! satisfy at launch, so a machine without PulseAudio installed could not start
//! the binary *at all* — no devices, no xruns, no explanation. That is exactly
//! how the macOS build used to die on Ventura, where a hard link to a 14.2-only
//! symbol killed the process before `main` ran. Opened at runtime, a missing
//! library is a `--` on two rows and one line saying why.
//!
//! ## Output levels
//!
//! Recording *output* means recording the sink's monitor source, which both
//! servers expose. Passing a null device name gets the default source (a
//! microphone); the monitor has to be asked for by name, and its name is the
//! default sink's with `.monitor` appended — which is what `default_monitor`
//! works out.

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::backend::level::MeterState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Output,
    Input,
}

// ── the slice of libpulse this needs ─────────────────────────────────────────

#[repr(C)]
struct SampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

/// `PA_SAMPLE_FLOAT32LE`, so the peak fold needs no conversion.
const PA_SAMPLE_FLOAT32LE: c_int = 5;
const PA_STREAM_RECORD: c_int = 2;
const RTLD_LAZY: c_int = 1;

type SimpleNew = unsafe extern "C" fn(
    *const c_char, // server
    *const c_char, // name
    c_int,         // direction
    *const c_char, // device
    *const c_char, // stream name
    *const SampleSpec,
    *const c_void, // channel map
    *const c_void, // buffer attr
    *mut c_int,    // error
) -> *mut c_void;
type SimpleRead = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_int) -> c_int;
type SimpleFree = unsafe extern "C" fn(*mut c_void);

struct Lib {
    new: SimpleNew,
    read: SimpleRead,
    free: SimpleFree,
}

unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

fn load() -> Result<&'static Lib, String> {
    use std::sync::OnceLock;
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();

    let lib = LIB.get_or_init(|| unsafe {
        // Distributions disagree about the soname suffix; try the versioned
        // one first, since the unsuffixed name only exists with -dev installed.
        let handle = [c"libpulse-simple.so.0", c"libpulse-simple.so"]
            .into_iter()
            .map(|n| dlopen(n.as_ptr(), RTLD_LAZY))
            .find(|h| !h.is_null())?;
        let sym = |n: &std::ffi::CStr| dlsym(handle, n.as_ptr());
        let (new, read, free) =
            (sym(c"pa_simple_new"), sym(c"pa_simple_read"), sym(c"pa_simple_free"));
        if new.is_null() || read.is_null() || free.is_null() {
            return None;
        }
        Some(Lib {
            new: std::mem::transmute::<*mut c_void, SimpleNew>(new),
            read: std::mem::transmute::<*mut c_void, SimpleRead>(read),
            free: std::mem::transmute::<*mut c_void, SimpleFree>(free),
        })
    });
    lib.as_ref().ok_or_else(|| {
        "levels need libpulse-simple (PipeWire's pulse shim or PulseAudio); it is not installed"
            .to_string()
    })
}

/// The default sink's monitor source, read from the PulseAudio/PipeWire server
/// state that both write to disk, so this needs no second API.
///
/// Falls back to `@DEFAULT_MONITOR@`, which both servers understand and which
/// is what a human would type.
fn default_monitor() -> CString {
    CString::new("@DEFAULT_MONITOR@").expect("no NUL in a literal")
}

/// A running level meter on one source.
pub struct Monitor {
    handle: *mut c_void,
    lib: &'static Lib,
    state: Arc<MeterState>,
    stop: Arc<AtomicBool>,
    /// Set by the reader as it leaves, so `Drop` knows when the handle is no
    /// longer in use. See the note there.
    finished: Arc<AtomicBool>,
    opened: std::time::Instant,
}

// The reader thread touches only the atomics in `state`; `handle` is owned by
// this struct and freed in Drop after the thread has stopped.
unsafe impl Send for Monitor {}

impl Monitor {
    pub fn open(which: Which) -> Result<Self, String> {
        let lib = load()?;
        let spec = SampleSpec { format: PA_SAMPLE_FLOAT32LE, rate: 48_000, channels: 2 };
        let app = CString::new("soundwatch").expect("no NUL");
        let stream = CString::new(match which {
            Which::Output => "output level",
            Which::Input => "input level",
        })
        .expect("no NUL");
        let device = match which {
            // Null means "the default source", which is a microphone. The
            // output has to be asked for by name.
            Which::Input => None,
            Which::Output => Some(default_monitor()),
        };

        let mut err: c_int = 0;
        let handle = unsafe {
            (lib.new)(
                std::ptr::null(),
                app.as_ptr(),
                PA_STREAM_RECORD,
                device.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
                stream.as_ptr(),
                &spec,
                std::ptr::null(),
                std::ptr::null(),
                &mut err,
            )
        };
        if handle.is_null() {
            return Err(match which {
                Which::Output => format!("could not open the output monitor (pulse error {err})"),
                Which::Input => {
                    format!("could not open the default input (pulse error {err})")
                }
            });
        }

        let state = Arc::new(MeterState::default());
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));

        // pa_simple_read blocks, so it gets its own thread — the same shape the
        // CoreAudio IOProc has, minus the real-time constraints.
        {
            let state = state.clone();
            let stop = stop.clone();
            let finished = finished.clone();
            let handle = handle as usize;
            std::thread::Builder::new()
                .name("soundwatch-pulse".into())
                .spawn(move || {
                    let mut buf = vec![0f32; 1024];
                    while !stop.load(Ordering::Relaxed) {
                        let bytes = std::mem::size_of_val(&buf[..]);
                        let mut err: c_int = 0;
                        let rc = unsafe {
                            (lib.read)(
                                handle as *mut c_void,
                                buf.as_mut_ptr() as *mut c_void,
                                bytes,
                                &mut err,
                            )
                        };
                        if rc < 0 {
                            break;
                        }
                        state.fold(&buf);
                    }
                    finished.store(true, Ordering::Release);
                })
                .map_err(|e| format!("could not start the meter thread: {e}"))?;
        }

        Ok(Self { handle, lib, state, stop, finished, opened: std::time::Instant::now() })
    }

    pub fn peak_dbfs(&self) -> Option<f32> {
        self.state.take_peak_dbfs()
    }

    pub fn rms_dbfs(&self) -> Option<f32> {
        self.state.take_rms_dbfs()
    }

    pub fn recent(&self, n: usize) -> Option<Vec<f32>> {
        self.state.recent(n)
    }

    pub fn overruns(&self) -> u64 {
        self.state.overruns.load(Ordering::Relaxed)
    }

    /// Has any sample, ever, been non-zero? A monitor that opens and delivers
    /// nothing but zeros is indistinguishable from a quiet machine unless the
    /// screen says which it is looking at.
    pub fn has_ever_heard_signal(&self) -> bool {
        self.state.has_ever_heard_signal()
    }

    pub fn running_for(&self, secs: u64) -> bool {
        self.opened.elapsed().as_secs() >= secs
    }
}

impl Drop for Monitor {
    /// Wait for the reader to leave before freeing the handle it is reading.
    ///
    /// Freeing underneath a blocked `pa_simple_read` is a use-after-free that
    /// libpulse catches with an assertion and an `abort()`:
    ///
    /// ```text
    /// Assertion 'pa_atomic_load(&(c)->_ref) >= 1' failed at
    /// ../src/pulse/context.c:1088, function pa_context_get_state(). Aborting.
    /// ```
    ///
    /// A monitor delivers a buffer every ~21 ms, so the reader notices `stop`
    /// almost immediately. If it does not — a suspended sink can leave a read
    /// blocked indefinitely — the handle is **deliberately leaked**. One
    /// abandoned context on a path that only runs when a meter is being closed
    /// or the process is exiting is a far better outcome than aborting the
    /// tool, which is the one thing a diagnostic must not do.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !self.finished.load(Ordering::Acquire) {
            if std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        unsafe { (self.lib.free)(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The library is optional by design. On a machine without it this must be
    /// a message, not a crash and not a failure to start.
    #[test]
    fn a_missing_libpulse_is_a_message() {
        match load() {
            Ok(_) => {}
            Err(e) => {
                assert!(e.contains("libpulse"), "unhelpful error: {e}");
                assert!(Monitor::open(Which::Output).is_err());
            }
        }
    }

    #[test]
    fn the_monitor_source_is_named_not_defaulted() {
        // A null device gets the default *source*, which is a microphone; the
        // output monitor has to be asked for by name or the output meter
        // silently shows the input.
        assert_eq!(default_monitor().to_str().unwrap(), "@DEFAULT_MONITOR@");
    }
}
