//! The backend, on its own thread.
//!
//! Every CoreAudio call this program makes is IPC to `coreaudiod`, and any of
//! them can block for as long as that daemon wants. The one that proves it is
//! consent: while a permission decision is pending for this process, an
//! ordinary property read stalls too. A sample of the frozen program, taken
//! while the microphone dialog was up:
//!
//! ```text
//! main  →  App::new  →  snapshot  →  read_device  →  channel_count
//!       →  AudioObjectGetPropertyData  →  HALC_ProxyObject::GetPropertyData
//! ```
//!
//! The whole UI was stuck there — no header, no meters, no explanation, not one
//! byte drawn — because a dialog was waiting for an answer. Opening the *meter*
//! on a worker thread was not enough: the HAL serialises per client, so the
//! main thread blocked behind the worker's request.
//!
//! So the renderer no longer calls CoreAudio at all. This thread does, and
//! posts results across a channel; the UI paints whatever it last received and
//! stays responsive whatever the audio system is doing. A stalled backend now
//! degrades to a screen that says so, which is the behaviour every other
//! failure in this program already has.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Instant;

use super::{Audio, AudioBackend, Levels};
use crate::model::Snapshot;
use crate::spectrum::Source;

/// One message from the backend thread.
pub enum Update {
    Snapshot(Box<Snapshot>),
    Levels(Levels),
    Audio(Box<Audio>),
}

/// What the UI currently wants collected. Read by the backend thread every
/// poll, so opening the spectrum takes effect on the next frame.
#[derive(Clone, Default)]
pub struct Wants(Arc<AtomicU8>);

impl Wants {
    pub fn set(&self, s: Source) {
        self.0.store(
            match s {
                Source::Off => 0,
                Source::Output => 1,
                Source::Input => 2,
            },
            Ordering::Relaxed,
        );
    }

    fn get(&self) -> Source {
        match self.0.load(Ordering::Relaxed) {
            1 => Source::Output,
            2 => Source::Input,
            _ => Source::Off,
        }
    }
}

/// Depth of the queue between the backend and the renderer.
///
/// Three seconds of level samples. If the renderer ever falls that far behind,
/// the right thing is to drop the oldest work rather than push back onto the
/// thread that is talking to the audio system — a blocked backend thread stops
/// the snapshots too, and then nothing on screen updates.
const QUEUE_DEPTH: usize = 64;

pub struct BackendWorker {
    rx: Receiver<Update>,
    stop: Arc<AtomicBool>,
    wants: Wants,
    /// Whether the settings menu currently wants the input meter open.
    input: Arc<AtomicBool>,
    /// Samples per spectrum window, from the settings menu.
    window_len: Arc<AtomicUsize>,
}

impl BackendWorker {
    /// Take ownership of a backend and start polling it.
    pub fn spawn(mut backend: Box<dyn AudioBackend>) -> Self {
        let (tx, rx) = sync_channel(QUEUE_DEPTH);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let wants = Wants::default();
        let asked = wants.clone();
        let input = Arc::new(AtomicBool::new(false));
        let want_input = input.clone();
        let window_len = Arc::new(AtomicUsize::new(crate::dsp::FFT_SIZE));
        let want_window = window_len.clone();

        let spawned = std::thread::Builder::new()
            .name("soundwatch-backend".into())
            .spawn(move || {
                // Send one immediately: the UI starts on a placeholder and
                // should replace it as soon as there is anything real.
                if !send(&tx, Update::Snapshot(Box::new(backend.snapshot())))
                    || flag.load(Ordering::Relaxed)
                {
                    return;
                }
                let mut last_tick = Instant::now();
                // Seeded from the backend rather than assumed: a session
                // launched with --meter-input starts with it already open, and
                // a worker that believes otherwise ignores the first switch-off.
                let mut input_on = backend.input_metering();
                let mut window = 0usize;
                loop {
                    std::thread::sleep(crate::app::SAMPLE_INTERVAL);
                    if flag.load(Ordering::Relaxed) {
                        return;
                    }
                    if !send(&tx, Update::Levels(backend.levels())) {
                        return;
                    }
                    // Opening a capture stream can block on a consent dialog,
                    // which is exactly why it happens on this thread.
                    let want_in = want_input.load(Ordering::Relaxed);
                    if want_in != input_on {
                        input_on = want_in;
                        backend.set_input_metering(want_in);
                    }
                    let want_len = want_window.load(Ordering::Relaxed);
                    if want_len != window {
                        window = want_len;
                        backend.set_window_len(want_len);
                    }
                    let want = asked.get();
                    if want.is_on()
                        && let Some(a) = backend.audio(want)
                        && !send(&tx, Update::Audio(Box::new(a)))
                    {
                        return;
                    }
                    if last_tick.elapsed() >= crate::app::TICK_INTERVAL {
                        last_tick = Instant::now();
                        if !send(&tx, Update::Snapshot(Box::new(backend.snapshot()))) {
                            return;
                        }
                    }
                }
            })
            .is_ok();

        if !spawned {
            stop.store(true, Ordering::Relaxed);
        }
        Self { rx, stop, wants, input, window_len }
    }

    /// Tell the backend how many samples the spectrum wants per window.
    pub fn want_window_len(&self, n: usize) {
        self.window_len.store(n, Ordering::Relaxed);
    }

    /// Ask the backend to open or close the input meter.
    pub fn want_input_meter(&self, on: bool) {
        self.input.store(on, Ordering::Relaxed);
    }

    /// Tell the backend which signal the spectrum screen wants, if any.
    pub fn want_audio(&self, s: Source) {
        self.wants.set(s);
    }

    /// Everything that has arrived since the last call. Never blocks.
    pub fn drain(&self) -> impl Iterator<Item = Update> + '_ {
        self.rx.try_iter()
    }
}

/// `false` when the receiver is gone and the thread should stop. A full queue
/// is not a reason to stop, and not a reason to wait either.
fn send(tx: &SyncSender<Update>, u: Update) -> bool {
    !matches!(tx.try_send(u), Err(TrySendError::Disconnected(_)))
}

impl Drop for BackendWorker {
    fn drop(&mut self) {
        // The thread checks this between polls. It is not joined: it may be
        // parked inside a CoreAudio call that will not return until a human
        // answers a dialog, and waiting for that is exactly the hang this
        // module exists to remove. The process is exiting; the OS will reap it.
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Caps;

    struct Stub;
    impl AudioBackend for Stub {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn caps(&self) -> Caps {
            Caps::default()
        }
        fn snapshot(&mut self) -> Snapshot {
            crate::demo::snapshot(0)
        }
        fn levels(&mut self) -> Levels {
            Levels { out_dbfs: Some(-12.0), ..Default::default() }
        }
    }

    /// A backend whose every call blocks forever, like one waiting on a consent
    /// dialog. The renderer must not care.
    struct Wedged;
    impl AudioBackend for Wedged {
        fn name(&self) -> &'static str {
            "wedged"
        }
        fn caps(&self) -> Caps {
            Caps::default()
        }
        fn snapshot(&mut self) -> Snapshot {
            std::thread::sleep(std::time::Duration::from_secs(3600));
            unreachable!()
        }
        fn levels(&mut self) -> Levels {
            Levels::default()
        }
    }

    #[test]
    fn a_snapshot_arrives_without_being_waited_for() {
        let w = BackendWorker::spawn(Box::new(Stub));
        for _ in 0..500 {
            if w.drain().any(|u| matches!(u, Update::Snapshot(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("no snapshot in one second");
    }

    /// The test that would have caught the freeze: draining a wedged backend
    /// has to return immediately, not when the backend feels like answering.
    #[test]
    fn a_wedged_backend_never_blocks_the_caller() {
        let w = BackendWorker::spawn(Box::new(Wedged));
        let t0 = Instant::now();
        for _ in 0..20 {
            assert_eq!(w.drain().count(), 0);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(t0.elapsed().as_millis() < 1000, "drain blocked on the backend");
        // And dropping it must not join the stuck thread.
        let t1 = Instant::now();
        drop(w);
        assert!(t1.elapsed().as_millis() < 100, "drop waited for a wedged thread");
    }

    #[test]
    fn a_full_queue_drops_work_rather_than_stalling_the_backend() {
        // Never drained; the worker must keep running rather than block on send.
        let w = BackendWorker::spawn(Box::new(Stub));
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Far more than QUEUE_DEPTH samples' worth of time has passed and the
        // thread is still alive to answer, which is all the caller can observe.
        assert!(w.drain().count() <= QUEUE_DEPTH + 1);
    }
}
