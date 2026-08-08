//! One trait over the audio stacks. Only CoreAudio is implemented today; the
//! PipeWire / PulseAudio / ALSA members of the family slot in behind the same
//! interface, and `Caps` is what lets the UI stay identical across all of them.

use crate::model::{Caps, Snapshot};

pub mod coreaudio;
pub mod ffi;
pub mod input;
pub mod ioproc;
pub mod tap;
pub mod worker;

/// A single level sample, taken at the sampler's rate (~20 Hz) rather than the
/// state tick's rate (1 Hz).
#[derive(Clone, Copy, Debug, Default)]
pub struct Levels {
    pub out_dbfs: Option<f32>,
    pub in_dbfs: Option<f32>,
}

/// Which meters a backend should actually attempt to open.
///
/// Metering is the one thing this tool does that is not pure observation, so it
/// is a decision the caller makes explicitly rather than a side effect of
/// constructing a backend. `--demo` and `--once` ask for neither, which is what
/// keeps them from creating system audio objects — including in the test suite,
/// where several backends exist at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Metering {
    /// Observe the system output mix through a process tap. Read-only, changes
    /// nothing about what is heard, but needs audio-capture consent.
    pub output: bool,
    /// Open a capture stream on the default input device. Off unless asked for:
    /// it puts this process into the mic-in-use audit it exists to report.
    pub input: bool,
}

impl Metering {
    /// Neither meter: enumerate and read properties only.
    pub const OBSERVE_ONLY: Self = Self { output: false, input: false };

    pub fn any(self) -> bool {
        self.output || self.input
    }
}

/// A meter that may still be waiting on a consent dialog.
///
/// Opening a tap blocks inside CoreAudio for as long as the permission sheet is
/// on screen — which on a first run is however long the user takes to read it.
/// Doing that on the main thread meant the tool showed a blank terminal and
/// looked hung at exactly the moment it was asking for something. So the meter
/// starts on its own thread and the UI paints immediately, degraded, with
/// "waiting for consent" as its note; when the answer arrives the meter comes
/// live mid-session with no restart.
pub enum Pending<T> {
    /// Never asked for.
    Off,
    /// Being opened on a worker thread.
    Starting(std::sync::mpsc::Receiver<Result<T, String>>),
    Live(T),
    Failed(String),
}

impl<T: Send + 'static> Pending<T> {
    /// Begin opening a meter in the background.
    pub fn spawn(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("soundwatch-meter-start".into())
            .spawn(move || {
                // A send failure just means the app quit first; nothing to do.
                let _ = tx.send(f());
            })
            .map(|_| Self::Starting(rx))
            .unwrap_or_else(|e| Self::Failed(format!("could not start a meter thread: {e}")))
    }

    /// Promote `Starting` to `Live` or `Failed` once the worker has answered.
    /// Cheap enough to call every tick.
    pub fn poll(&mut self) {
        let Self::Starting(rx) = self else { return };
        *self = match rx.try_recv() {
            Ok(Ok(v)) => Self::Live(v),
            Ok(Err(e)) => Self::Failed(e),
            // Disconnected without a value means the worker panicked.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Self::Failed("the meter thread stopped unexpectedly".into())
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
        };
    }

    pub fn live(&self) -> Option<&T> {
        match self {
            Self::Live(v) => Some(v),
            _ => None,
        }
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(e) => Some(e),
            _ => None,
        }
    }

    pub fn is_starting(&self) -> bool {
        matches!(self, Self::Starting(_))
    }
}

pub trait AudioBackend: Send {
    /// Backend name for the header: `coreaudio`, `pipewire`, `pulse`, `alsa`.
    fn name(&self) -> &'static str;

    /// What this backend can actually report.
    fn caps(&self) -> Caps;

    /// Full state read. Called at the 1 Hz tick.
    fn snapshot(&mut self) -> Snapshot;

    /// Level sample. Called at the meter rate. Returns `None` for any meter
    /// that is not live — not yet open, unavailable, or never asked for.
    fn levels(&mut self) -> Levels;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle<T: Send + 'static>(p: &mut Pending<T>) {
        // The worker is a real thread; give it a bounded chance to answer
        // rather than sleeping a fixed amount and hoping.
        for _ in 0..500 {
            p.poll();
            if !p.is_starting() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("meter never finished starting");
    }

    #[test]
    fn a_meter_that_opens_goes_live_without_a_restart() {
        let mut p = Pending::spawn(|| Ok(7u32));
        settle(&mut p);
        assert_eq!(p.live(), Some(&7));
        assert!(p.error().is_none());
    }

    #[test]
    fn a_meter_that_is_refused_keeps_its_reason() {
        let mut p: Pending<u32> = Pending::spawn(|| Err("consent denied".into()));
        settle(&mut p);
        assert!(p.live().is_none());
        assert_eq!(p.error(), Some("consent denied"));
    }

    #[test]
    fn a_meter_thread_that_dies_does_not_hang_the_ui() {
        // A panic in the worker drops the sender. Without the Disconnected arm
        // this would sit in Starting for the life of the process, and the note
        // would promise a consent prompt that is never coming.
        let mut p: Pending<u32> = Pending::spawn(|| panic!("boom"));
        settle(&mut p);
        assert!(p.error().is_some(), "a dead worker must surface as a failure");
    }

    #[test]
    fn polling_before_the_answer_leaves_it_starting() {
        let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();
        let mut p = Pending::Starting(rx);
        p.poll();
        assert!(p.is_starting(), "no answer yet is not a failure");
        assert!(p.live().is_none() && p.error().is_none());
        tx.send(Ok(1)).unwrap();
        p.poll();
        assert_eq!(p.live(), Some(&1));
    }
}
