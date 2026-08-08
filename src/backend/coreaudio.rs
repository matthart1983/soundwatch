//! The macOS backend.
//!
//! State is read through `AudioObjectGetPropertyData`: nothing is written back,
//! no volume, mute, routing or default is ever changed.
//!
//! **On levels.** The CoreAudio HAL exposes volume but no peak or RMS metering,
//! so a level has to come from somewhere else. There are two sources and they
//! are not equivalent:
//!
//! * Output goes through a process tap ([`super::tap`]), which macOS 14.2 added
//!   precisely so a program can observe the mix without joining the graph. It
//!   does not alter routing and does not run inside anyone else's callback.
//! * Input has no such API. A level means opening a capture stream
//!   ([`super::input`]), which is real participation — this process becomes a
//!   microphone client and appears in its own mic-in-use audit. That is why it
//!   is behind `--meter-input` rather than on by default.
//!
//! Both need consent, and both fail soft: [`Caps`] reports what is actually
//! live, the UI renders `--` for the rest, and the note says why.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use super::ffi::{self, AudioObjectID, AudioObjectPropertyAddress, AudioStreamBasicDescription};
use super::{AudioBackend, Levels, Metering, Pending};
use crate::model::{Caps, Device, Direction, Format, Snapshot, Stream, Timestamp};

/// Incremented from the HAL's `kAudioDeviceProcessorOverload` callback, which
/// runs on a CoreAudio-owned thread. An atomic keeps the callback allocation-
/// and lock-free; the 60-second window is reconstructed by sampling deltas.
static XRUNS: AtomicU64 = AtomicU64::new(0);

/// Devices this *process* has an overload listener on.
///
/// The HAL identifies a registration by `(object, address, proc, client data)`,
/// and all four are the same for every `CoreAudio` here — the counter above is
/// a static, so the callback has no per-instance state to carry. That makes the
/// registration process-wide whether we track it that way or not: a second
/// backend asking for the same listener is refused, and if each instance kept
/// its own set it would conclude it had failed and quietly stop counting xruns.
/// Tracking it where it actually lives is the honest version.
static LISTENING: Mutex<BTreeSet<AudioObjectID>> = Mutex::new(BTreeSet::new());

extern "C" fn overload_listener(
    _id: AudioObjectID,
    _n: u32,
    _addrs: *const AudioObjectPropertyAddress,
    _data: *mut std::ffi::c_void,
) -> i32 {
    XRUNS.fetch_add(1, Ordering::Relaxed);
    0
}

/// `(register these, forget these)` given what exists and what we already hold.
///
/// Pulled out of [`CoreAudio::install_listeners`] so the bookkeeping can be
/// tested against an imagined dock being plugged and unplugged, rather than
/// against whatever happens to be connected to the machine running the suite.
fn listener_delta(
    present: &BTreeSet<AudioObjectID>,
    registered: &BTreeSet<AudioObjectID>,
) -> (Vec<AudioObjectID>, Vec<AudioObjectID>) {
    (
        present.difference(registered).copied().collect(),
        registered.difference(present).copied().collect(),
    )
}

/// Reverse-DNS names truncated into a 15-column field are useless — Apple's
/// helper executables are literally called `com.apple.WebKit.GPU`, which renders
/// as `com.apple.WebKi`. Drop the reverse-DNS prefix and keep the part that
/// identifies the program. Names that are not reverse-DNS are left alone, so
/// `zoom.us` and `coreaudiod` survive intact.
fn friendly_name(raw: &str) -> String {
    const PREFIXES: [&str; 8] = ["com", "org", "net", "io", "co", "app", "dev", "me"];
    let parts: Vec<&str> = raw.split('.').collect();
    if parts.len() >= 3 && PREFIXES.contains(&parts[0]) {
        return parts[2..].join(".");
    }
    raw.to_string()
}

pub struct CoreAudio {
    host: String,
    /// `(observed_at, count)` pairs, pruned to the trailing 60 seconds.
    xrun_window: VecDeque<(Timestamp, u32)>,
    last_xrun_total: u64,
    /// When the counter was last read, so a stale delta is not attributed to
    /// the moment it was noticed. See [`CoreAudio::xruns_60s`].
    last_xrun_read: Option<Timestamp>,
    /// First time each stream key was seen, for the hold column.
    first_seen: HashMap<String, Timestamp>,
    has_process_api: bool,
    /// What the caller asked us to meter.
    metering: Metering,
    /// Output metering: off, opening, live, or failed with a reason.
    tap: Pending<super::tap::LevelTap>,
    /// Input metering, same four states.
    input: Pending<super::input::InputMeter>,
    /// Which device the last input-meter attempt was for. Stops a device that
    /// genuinely cannot be opened from being retried every single tick.
    last_input_attempt: Option<AudioObjectID>,
    /// Set at each tick when some process is playing to the default output.
    /// Corroborates a silent tap; see [`CoreAudio::output_meter_is_deaf`].
    output_is_busy: bool,
}

/// How long a tap may read pure silence before that becomes suspicious. Long
/// enough that a pause between tracks is never mistaken for a permission
/// problem.
const DEAF_AFTER_SECS: u64 = 8;

/// How stale a reading of the xrun counter may be and still be attributed to
/// the moment it was taken. The tick is 1 Hz, so this is generous enough to
/// absorb a slow frame and short enough that a pause, a sleep or a stopped
/// process can never masquerade as a burst of dropouts.
const XRUN_GAP_SECS: u64 = 5;

impl Default for CoreAudio {
    fn default() -> Self {
        Self::new(Metering::OBSERVE_ONLY)
    }
}

impl CoreAudio {
    /// Open the backend. `metering` decides whether any audio object is
    /// created at all — with [`Metering::OBSERVE_ONLY`] this is a pure reader.
    pub fn new(metering: Metering) -> Self {
        let has_process_api =
            ffi::has(ffi::SYSTEM_OBJECT, &ffi::addr(ffi::HW_PROCESS_LIST, ffi::SCOPE_GLOBAL));

        // Both of these can block on a consent dialog, so neither happens here.
        let tap = match metering.output {
            true => Pending::spawn(super::tap::LevelTap::start),
            false => Pending::Off,
        };
        let input = match metering.input {
            true => Pending::spawn(super::input::InputMeter::start),
            false => Pending::Off,
        };

        Self {
            host: ffi::hostname(),
            xrun_window: VecDeque::new(),
            last_xrun_total: 0,
            last_xrun_read: None,
            first_seen: HashMap::new(),
            has_process_api,
            metering,
            tap,
            input,
            last_input_attempt: None,
            output_is_busy: false,
        }
    }

    fn all_devices() -> Vec<AudioObjectID> {
        ffi::get_array(ffi::SYSTEM_OBJECT, &ffi::addr(ffi::HW_DEVICES, ffi::SCOPE_GLOBAL))
    }

    fn default_device(output: bool) -> Option<AudioObjectID> {
        let sel = if output { ffi::HW_DEFAULT_OUTPUT } else { ffi::HW_DEFAULT_INPUT };
        ffi::get::<AudioObjectID>(ffi::SYSTEM_OBJECT, &ffi::addr(sel, ffi::SCOPE_GLOBAL))
            .filter(|id| *id != 0)
    }

    fn device_name(id: AudioObjectID) -> String {
        ffi::get_string(id, &ffi::addr(ffi::OBJ_NAME, ffi::SCOPE_GLOBAL))
            .unwrap_or_else(|| format!("device {id}"))
    }

    /// Read a device in one direction. Returns `None` if it has no channels in
    /// that scope — that is how CoreAudio distinguishes sinks from sources.
    fn read_device(id: AudioObjectID, direction: Direction, is_default: bool) -> Option<Device> {
        let scope = if direction.is_input() { ffi::SCOPE_INPUT } else { ffi::SCOPE_OUTPUT };
        let channels = ffi::channel_count(id, scope);
        if channels == 0 {
            return None;
        }

        let rate = ffi::get::<f64>(id, &ffi::addr(ffi::DEV_NOMINAL_RATE, ffi::SCOPE_GLOBAL))
            .unwrap_or(0.0)
            .max(0.0) as u32;

        // Bit depth and per-stream latency come from the device's first stream.
        let streams: Vec<AudioObjectID> = ffi::get_array(id, &ffi::addr(ffi::DEV_STREAMS, scope));
        let (bits, stream_latency) = streams
            .first()
            .map(|&sid| {
                let bits = ffi::get::<AudioStreamBasicDescription>(
                    sid,
                    &ffi::addr(ffi::STREAM_PHYSICAL_FORMAT, ffi::SCOPE_GLOBAL),
                )
                .map(|f| f.mBitsPerChannel as u8)
                .unwrap_or(0);
                let lat = ffi::get::<u32>(sid, &ffi::addr(ffi::DEV_LATENCY, ffi::SCOPE_GLOBAL))
                    .unwrap_or(0);
                (bits, lat)
            })
            .unwrap_or((0, 0));

        let device_latency = ffi::get::<u32>(id, &ffi::addr(ffi::DEV_LATENCY, scope)).unwrap_or(0);
        let safety = ffi::get::<u32>(id, &ffi::addr(ffi::DEV_SAFETY_OFFSET, scope)).unwrap_or(0);

        Some(Device {
            id,
            name: Self::device_name(id),
            direction,
            format: Format { rate, bits, channels: channels.min(u8::MAX as u32) as u8 },
            buffer_frames: ffi::get::<u32>(
                id,
                &ffi::addr(ffi::DEV_BUFFER_FRAMES, ffi::SCOPE_GLOBAL),
            )
            .unwrap_or(0),
            latency_frames: device_latency + safety + stream_latency,
            is_default,
            is_running: ffi::get::<u32>(
                id,
                &ffi::addr(ffi::DEV_IS_RUNNING_SOMEWHERE, ffi::SCOPE_GLOBAL),
            )
            .unwrap_or(0)
                != 0,
        })
    }

    /// Keep an overload listener on exactly the devices that currently exist.
    ///
    /// Both halves matter. Interfaces are hot-plugged constantly in audio work,
    /// and a session that runs for a day sees the same dock connect and
    /// disconnect dozens of times. Only adding meant registrations accumulated
    /// in the HAL for devices that were long gone, and `listening` grew without
    /// bound; only removing on disconnect keeps both sides honest.
    fn install_listeners(&mut self) {
        let present: BTreeSet<AudioObjectID> = Self::all_devices().into_iter().collect();
        let a = ffi::addr(ffi::DEV_PROCESSOR_OVERLOAD, ffi::SCOPE_GLOBAL);

        // A poisoned lock would mean a panic while holding it, and the set is a
        // BTreeSet of integers — there is nothing here to be left inconsistent.
        let mut registered = LISTENING.lock().unwrap_or_else(|e| e.into_inner());
        let (to_add, to_drop) = listener_delta(&present, &registered);

        for id in to_add {
            if ffi::add_listener(id, &a, overload_listener, std::ptr::null_mut()) {
                registered.insert(id);
            }
        }
        // Unplugged. Removing is best-effort: the object may already be gone,
        // in which case the HAL has forgotten us anyway.
        for id in to_drop {
            ffi::remove_listener(id, &a, overload_listener, std::ptr::null_mut());
            registered.remove(&id);
        }
    }

    /// Follow the default input device when the user changes it.
    ///
    /// An IOProc is bound to one device for its whole life, so switching from
    /// the built-in microphone to an interface leaves the meter reading a
    /// device nobody is talking into — a flat line that looks exactly like a
    /// permission problem. Audio people swap interfaces constantly, so the
    /// meter has to follow. Dropping the old meter stops and detaches it, which
    /// also releases the device if we were the only client.
    ///
    /// The output tap needs no equivalent: it is a *global* tap of the system
    /// mix, so it follows the default output on its own.
    ///
    /// A failure has to be retried, not latched. Unplugging an interface is a
    /// two-step event as macOS sees it — the default input disappears for a
    /// moment before another device takes over — so a restart triggered by the
    /// first step lands on "no default input device" and fails. Without a
    /// retry, plugging anything back in would never bring the meter back, and
    /// the hot-plug this function exists for is exactly the case that broke it.
    fn follow_default_input(&mut self) {
        if !self.metering.input {
            return;
        }
        let current = Self::default_device(false);
        let restart = match &self.input {
            // Metering the wrong device.
            Pending::Live(live) => current != Some(live.device()),
            // Failed earlier; try again once there is a device to try.
            Pending::Failed(_) => current.is_some() && current != self.last_input_attempt,
            // Off, or still opening — leave it alone.
            _ => false,
        };
        if restart {
            self.last_input_attempt = current;
            self.input = Pending::spawn(super::input::InputMeter::start);
        }
    }

    /// Roll the atomic counter into a trailing-60-second count.
    ///
    /// A lifetime counter is noise — what matters is what just happened.
    ///
    /// The counter keeps rising whether or not anyone is reading it, and it is
    /// read from the 1 Hz tick, which stops while paused and does not run at
    /// all while the machine is asleep. So a delta is only evidence about
    /// *when* if it was taken recently: resume after an hour paused and the
    /// naive reading credits an hour of overloads to this instant, paints the
    /// header red and blames the buffer size, for events that may have
    /// happened while the lid was shut. Past [`XRUN_GAP_SECS`] the delta is
    /// discarded and the baseline just moves — the count is unknowable rather
    /// than large, and this row says so by staying quiet.
    fn xruns_60s(&mut self, now: Timestamp) -> u32 {
        let total = XRUNS.load(Ordering::Relaxed);
        let delta = total.saturating_sub(self.last_xrun_total);
        let gap = self.last_xrun_read.map(|t| now.secs_since(t));
        self.last_xrun_total = total;
        self.last_xrun_read = Some(now);

        let attributable = match gap {
            // First read of the session: the counter started with us.
            None => true,
            Some(g) => g <= XRUN_GAP_SECS,
        };
        if delta > 0 && attributable {
            self.xrun_window.push_back((now, delta as u32));
        }

        while let Some(&(at, _)) = self.xrun_window.front() {
            if now.secs_since(at) >= 60 {
                self.xrun_window.pop_front();
            } else {
                break;
            }
        }
        self.xrun_window.iter().map(|(_, c)| c).sum()
    }

    /// Per-app streams via the macOS 14 process-object API. This is the only way
    /// to answer "what has my microphone" without opening anything.
    fn read_streams(
        &mut self,
        names: &HashMap<AudioObjectID, String>,
        default_out: Option<&Device>,
        default_in: Option<&Device>,
        now: Timestamp,
    ) -> Vec<Stream> {
        if !self.has_process_api {
            return Vec::new();
        }
        let procs: Vec<AudioObjectID> =
            ffi::get_array(ffi::SYSTEM_OBJECT, &ffi::addr(ffi::HW_PROCESS_LIST, ffi::SCOPE_GLOBAL));

        let mut out = Vec::new();
        for p in procs {
            let running_in =
                ffi::get::<u32>(p, &ffi::addr(ffi::PROC_IS_RUNNING_INPUT, ffi::SCOPE_GLOBAL))
                    .unwrap_or(0)
                    != 0;
            let running_out =
                ffi::get::<u32>(p, &ffi::addr(ffi::PROC_IS_RUNNING_OUTPUT, ffi::SCOPE_GLOBAL))
                    .unwrap_or(0)
                    != 0;
            if !running_in && !running_out {
                continue;
            }

            let pid = ffi::get::<i32>(p, &ffi::addr(ffi::PROC_PID, ffi::SCOPE_GLOBAL));

            // Whether to report ourselves depends on why we are in this list.
            //
            // The output tap alone is pure observation: it makes the process
            // look like an audio client to the HAL without being one in any
            // sense the user cares about, and putting SoundWatch at the top of
            // its own mic-in-use audit would be an artefact of measuring rather
            // than a fact about the machine.
            //
            // `--meter-input` is different. That opens a real capture stream —
            // the indicator comes on and we are genuinely holding the
            // microphone. A tool whose job is answering "what has my mic"
            // must not exempt itself from the answer.
            if pid == Some(std::process::id() as i32) && self.input.live().is_none() {
                continue;
            }
            let app = pid
                .and_then(ffi::process_name)
                .or_else(|| ffi::get_string(p, &ffi::addr(ffi::PROC_BUNDLE_ID, ffi::SCOPE_GLOBAL)))
                .map(|n| friendly_name(&n))
                .unwrap_or_else(|| "unknown".into());

            let devs: Vec<AudioObjectID> =
                ffi::get_array(p, &ffi::addr(ffi::PROC_DEVICES, ffi::SCOPE_GLOBAL));

            for (active, direction) in
                [(running_in, Direction::Input), (running_out, Direction::Output)]
            {
                if !active {
                    continue;
                }
                let fallback = if direction.is_input() { default_in } else { default_out };
                // Prefer a device this process is actually attached to that has
                // channels in the right direction; otherwise the current default.
                let dev_id = devs
                    .iter()
                    .copied()
                    .find(|&d| {
                        let scope =
                            if direction.is_input() { ffi::SCOPE_INPUT } else { ffi::SCOPE_OUTPUT };
                        ffi::channel_count(d, scope) > 0
                    })
                    .or_else(|| fallback.map(|d| d.id));

                let (dev_name, format, latency_ms) = match dev_id {
                    Some(d) if Some(d) == fallback.map(|f| f.id) => {
                        let f = fallback.expect("matched above");
                        (f.name.clone(), f.format, f.latency_ms())
                    }
                    Some(d) => {
                        let dev = Self::read_device(d, direction, false);
                        (
                            names.get(&d).cloned().unwrap_or_else(|| Self::device_name(d)),
                            dev.as_ref().map(|x| x.format).unwrap_or_default(),
                            dev.as_ref().and_then(Device::latency_ms),
                        )
                    }
                    None => (crate::fmt::NA.to_string(), Format::default(), None),
                };

                let key = format!(
                    "{}:{}:{}",
                    pid.unwrap_or(-1),
                    if direction.is_input() { "in" } else { "out" },
                    dev_id.unwrap_or(0)
                );
                let first_seen = *self.first_seen.entry(key.clone()).or_insert(now);

                out.push(Stream {
                    key,
                    app: app.clone(),
                    pid,
                    device: dev_name,
                    device_id: dev_id.unwrap_or(0),
                    direction,
                    format,
                    // The HAL does not report what the client asked for, only
                    // what the device is running at, so conversions cannot be
                    // detected here. PipeWire does report this.
                    requested: None,
                    level_dbfs: None,
                    latency_ms,
                    node_id: Some(p),
                    first_seen,
                });
            }
        }

        // Inputs first — "what has my mic" is the sharper question — then output
        // by level descending. With no levels available, fall back to app name so
        // the order is at least stable between ticks.
        out.sort_by(|a, b| {
            b.direction
                .is_input()
                .cmp(&a.direction.is_input())
                .then_with(|| {
                    b.level_dbfs
                        .unwrap_or(f32::NEG_INFINITY)
                        .partial_cmp(&a.level_dbfs.unwrap_or(f32::NEG_INFINITY))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.app.to_lowercase().cmp(&b.app.to_lowercase()))
                .then_with(|| a.key.cmp(&b.key))
        });
        out
    }

    /// Is the output meter running, and reading nothing, while the machine is
    /// audibly busy?
    ///
    /// Refused audio-capture consent produces no error anywhere: the tap is
    /// created, the IOProc fires at the nominal rate, and every sample is 0.0
    /// for as long as the program runs. The only way to tell that apart from a
    /// quiet Mac is corroboration — some process has been playing to the
    /// default output for a while, and we have still never seen a non-zero
    /// sample. Both halves are required, so a genuinely silent machine with
    /// idle apps never trips it.
    fn output_meter_is_deaf(&self) -> bool {
        let Some(tap) = self.tap.live() else {
            return false;
        };
        tap.has_run_for(DEAF_AFTER_SECS) && !tap.has_ever_heard_signal() && self.output_is_busy
    }

    /// The same question for the input meter, minus the corroboration: a
    /// microphone with nothing in front of it still has a noise floor, so a
    /// capture stream that reads *exactly* zero for this long is not a quiet
    /// room. It is a stream that was opened and then muted by the system.
    fn input_meter_is_deaf(&self) -> bool {
        let Some(input) = self.input.live() else {
            return false;
        };
        input.has_run_for(DEAF_AFTER_SECS) && !input.has_ever_heard_signal()
    }

    /// Prune identities for streams that have gone away, so a process that stops
    /// and restarts gets a fresh hold time rather than an inherited one.
    fn forget_stale(&mut self, live: &[Stream]) {
        let keys: HashSet<&str> = live.iter().map(|s| s.key.as_str()).collect();
        self.first_seen.retain(|k, _| keys.contains(k.as_str()));
    }
}

impl AudioBackend for CoreAudio {
    fn name(&self) -> &'static str {
        "coreaudio"
    }

    fn caps(&self) -> Caps {
        // One line, 78 columns, and several things can be wrong at once — so
        // this is a priority order, most actionable first, not a list.
        // A build whose signature does not bind Info.plist can never obtain
        // consent, so say that first: every other explanation would send the
        // user to a Settings pane that will not list this binary.
        let note = if self.metering.any()
            && let Some(advice) = crate::tcc::signing_advice()
        {
            Some(advice)
        } else if let Some(e) = self.tap.error() {
            Some(e.to_string())
        } else if let Some(e) = self.input.error() {
            Some(format!("input meter unavailable: {e}"))
        } else if self.tap.is_starting() || self.input.is_starting() {
            // Almost instant once consent has been given; on a first run this
            // is the line that is on screen while the dialog is up.
            Some("opening the meters \u{2014} answer the audio permission prompt".into())
        } else if self.input_meter_is_deaf() {
            Some(
                "input meter reads digital silence \u{2014} \
                 allow microphone access under Privacy & Security"
                    .into(),
            )
        } else if self.output_meter_is_deaf() {
            // The failure mode that has no error code: consent was never given,
            // so the tap delivers digital silence and looks like a quiet Mac.
            Some(
                "output meter reads silence while apps are playing \u{2014} \
                 allow audio recording, or run --probe-tap"
                    .into(),
            )
        } else if !self.has_process_api {
            Some("per-app streams need macOS 14 or newer".into())
        } else if !self.metering.any() {
            Some("levels not metered in this mode".into())
        } else if self.input.live().is_none() {
            Some("per-app levels are not reported; input needs --meter-input".into())
        } else {
            Some("per-app levels are not reported by the CoreAudio HAL".into())
        };

        Caps {
            device_levels: self.tap.live().is_some(),
            input_levels: self.input.live().is_some(),
            stream_levels: false,
            per_app_streams: self.has_process_api,
            requested_format: false,
            xruns: true,
            hold_is_since_launch: true,
            // Notes must fit the 78-column content width.
            note: note.map(|n| crate::grid::truncate(&n, crate::grid::CONTENT.width()).to_string()),
        }
    }

    fn snapshot(&mut self) -> Snapshot {
        self.tap.poll();
        self.input.poll();
        self.follow_default_input();
        self.install_listeners();
        let now = Timestamp::now();

        let names: HashMap<AudioObjectID, String> =
            Self::all_devices().into_iter().map(|id| (id, Self::device_name(id))).collect();

        let default_out = Self::default_device(true)
            .and_then(|id| Self::read_device(id, Direction::Output, true));
        let default_in = Self::default_device(false)
            .and_then(|id| Self::read_device(id, Direction::Input, true));

        let streams = self.read_streams(&names, default_out.as_ref(), default_in.as_ref(), now);
        self.forget_stale(&streams);
        let xruns_60s = self.xruns_60s(now);

        // Corroboration for the silent-tap check: someone is actually playing.
        self.output_is_busy = default_out.as_ref().is_some_and(|d| {
            d.is_running && streams.iter().any(|s| !s.direction.is_input() && s.device_id == d.id)
        });

        Snapshot {
            backend: self.name(),
            host: self.host.clone(),
            default_out,
            default_in,
            streams,
            xruns_60s,
            caps: self.caps(),
            at: now,
        }
    }

    fn levels(&mut self) -> Levels {
        // Polled here as well as in snapshot(): levels runs at 20 Hz, so a
        // meter that finishes opening goes live within one frame rather than
        // waiting up to a second for the next full tick.
        self.tap.poll();
        self.input.poll();
        Levels {
            out_dbfs: self.tap.live().and_then(|t| t.peak_dbfs()),
            in_dbfs: self.input.live().and_then(|m| m.peak_dbfs()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backend that reads the machine but creates nothing on it.
    ///
    /// Every test here used to call `CoreAudio::new()`, which started a process
    /// tap — so a `cargo test` run built one private aggregate device per test,
    /// in parallel, and they contended. `levels_degrade_rather_than_tapping`
    /// then *passed* because its tap lost the race and metering came back
    /// unavailable, which was exactly what it asserted. Run alone, it failed.
    /// A green suite that depends on a resource conflict is worse than a red
    /// one, so the hardware tests are now explicitly observers.
    fn observer() -> CoreAudio {
        CoreAudio::new(Metering::OBSERVE_ONLY)
    }

    #[test]
    fn snapshot_reads_real_hardware() {
        let mut be = observer();
        let s = be.snapshot();
        assert_eq!(s.backend, "coreaudio");
        assert!(!s.host.is_empty());
        // Any Mac has a default output device.
        let out = s.default_out.expect("no default output device");
        assert!(!out.name.is_empty());
        assert!(out.format.rate >= 8000, "implausible rate {}", out.format.rate);
        assert!(out.format.channels >= 1);
        assert_eq!(out.direction, Direction::Output);
        assert!(out.is_default);
    }

    #[test]
    fn computed_latency_is_plausible() {
        let mut be = observer();
        let s = be.snapshot();
        if let Some(ms) = s.latency_ms() {
            assert!(ms > 0.0 && ms < 1000.0, "implausible latency {ms}ms");
        }
    }

    #[test]
    fn stream_identities_are_stable_across_ticks() {
        let mut be = observer();
        let a = be.snapshot();
        let b = be.snapshot();
        // Hold times must not reset between ticks for streams that persisted.
        for s in &a.streams {
            if let Some(t) = b.streams.iter().find(|x| x.key == s.key) {
                assert_eq!(t.first_seen, s.first_seen, "hold time reset for {}", s.key);
            }
        }
    }

    #[test]
    fn inputs_sort_before_outputs() {
        let mut be = observer();
        let s = be.snapshot();
        let first_output = s.streams.iter().position(|x| !x.direction.is_input());
        let last_input = s.streams.iter().rposition(|x| x.direction.is_input());
        if let (Some(fo), Some(li)) = (first_output, last_input) {
            assert!(li < fo, "an input sorted after an output");
        }
    }

    #[test]
    fn reverse_dns_executables_get_a_usable_name() {
        assert_eq!(friendly_name("com.apple.WebKit.GPU"), "WebKit.GPU");
        assert_eq!(friendly_name("com.google.Chrome"), "Chrome");
        // Not reverse-DNS: left exactly as the OS reports it.
        assert_eq!(friendly_name("zoom.us"), "zoom.us");
        assert_eq!(friendly_name("coreaudiod"), "coreaudiod");
        assert_eq!(friendly_name("Spotify"), "Spotify");
        // Whatever comes back must still fit the 15-column APP field after the
        // grid truncates it, and must never be empty.
        assert!(!friendly_name("com.apple").is_empty());
    }

    #[test]
    fn the_degradation_note_fits_the_content_width() {
        let be = observer();
        let note = be.caps().note.expect("a degraded field needs an explanation");
        assert!(
            crate::grid::width(&note) <= crate::grid::CONTENT.width(),
            "note is {} columns: {note:?}",
            crate::grid::width(&note)
        );
    }

    /// Every branch of the note has to fit row 11, not just the one this
    /// machine happens to produce. The consent messages are the long ones and
    /// they are exactly the ones a developer never sees on their own Mac.
    #[test]
    fn every_degradation_note_fits_the_content_width() {
        let mut be = observer();
        let cases: Vec<(&str, String)> = vec![
            ("tap refused", super::super::tap::LevelTap::explain(0x6E6F7065)),
            ("tap failed", super::super::tap::LevelTap::explain(-4242)),
            ("input failed", "no default input device".into()),
        ];
        for (what, err) in cases {
            be.tap = Pending::Failed(err);
            let note = be.caps().note.expect("a degraded field needs an explanation");
            assert!(
                crate::grid::width(&note) <= crate::grid::CONTENT.width(),
                "{what} note is {} columns: {note:?}",
                crate::grid::width(&note)
            );
        }
    }

    #[test]
    fn an_observer_backend_creates_nothing_and_says_so() {
        // OBSERVE_ONLY is what --demo, --once and this suite use. It must not
        // start a tap, must report no levels, and must still explain itself.
        let mut be = observer();
        let l = be.levels();
        assert!(l.out_dbfs.is_none() && l.in_dbfs.is_none());
        assert!(!be.caps().device_levels);
        assert!(!be.caps().input_levels);
        assert!(be.caps().note.is_some(), "a degraded field needs an explanation");
    }

    fn ids(v: &[AudioObjectID]) -> BTreeSet<AudioObjectID> {
        v.iter().copied().collect()
    }

    /// `XRUNS` is process-wide, so the tests that drive it have to take turns.
    static XRUN_TEST: Mutex<()> = Mutex::new(());

    /// Start from a known counter value, holding the lock for the test.
    fn xrun_fixture() -> (CoreAudio, std::sync::MutexGuard<'static, ()>) {
        let guard = XRUN_TEST.lock().unwrap_or_else(|e| e.into_inner());
        let mut be = observer();
        XRUNS.store(0, Ordering::Relaxed);
        be.last_xrun_total = 0;
        be.last_xrun_read = None;
        (be, guard)
    }

    #[test]
    fn a_steady_device_list_re_registers_nothing() {
        let devices = ids(&[1, 2, 3]);
        let (add, drop) = listener_delta(&devices, &devices);
        assert!(add.is_empty(), "re-registering on every tick");
        assert!(drop.is_empty());
    }

    #[test]
    fn plugging_an_interface_in_registers_only_the_new_one() {
        let (add, drop) = listener_delta(&ids(&[1, 2, 3]), &ids(&[1, 2]));
        assert_eq!(add, vec![3]);
        assert!(drop.is_empty());
    }

    /// The half that was missing: a disconnected interface used to keep its
    /// registration in the HAL forever, and the set grew for the life of the
    /// process. A day of dock cycles is a lot of dead registrations.
    #[test]
    fn unplugging_an_interface_forgets_it() {
        let (add, drop) = listener_delta(&ids(&[1, 2]), &ids(&[1, 2, 3]));
        assert!(add.is_empty());
        assert_eq!(drop, vec![3], "stale registration kept");
    }

    #[test]
    fn swapping_one_interface_for_another_does_both() {
        let (add, drop) = listener_delta(&ids(&[1, 4]), &ids(&[1, 3]));
        assert_eq!(add, vec![4]);
        assert_eq!(drop, vec![3]);
    }

    /// The counter keeps climbing while the tick is not running. Resuming must
    /// not credit all of it to the moment of resume — that paints the header
    /// red and blames the buffer size for events that may have happened while
    /// the machine was asleep.
    #[test]
    fn a_backlog_of_xruns_is_not_reported_as_a_burst() {
        let (mut be, _turn) = xrun_fixture();

        // A normal reading at t=0 establishes the baseline.
        assert_eq!(be.xruns_60s(Timestamp(0)), 0);

        // 30 overloads accumulate while paused for ten minutes.
        XRUNS.store(30, Ordering::Relaxed);
        let after_pause = be.xruns_60s(Timestamp(600_000));
        assert_eq!(after_pause, 0, "a ten-minute backlog was reported as happening now");

        // The baseline still moved, so the next real overload counts once.
        XRUNS.store(31, Ordering::Relaxed);
        assert_eq!(
            be.xruns_60s(Timestamp(601_000)),
            1,
            "discarding the backlog must not double-count afterwards"
        );
    }

    #[test]
    fn xruns_inside_the_window_are_still_counted() {
        let (mut be, _turn) = xrun_fixture();
        assert_eq!(be.xruns_60s(Timestamp(0)), 0);

        XRUNS.store(4, Ordering::Relaxed);
        assert_eq!(be.xruns_60s(Timestamp(1_000)), 4, "a live burst must be reported");
        // And it ages out of the trailing window.
        assert_eq!(be.xruns_60s(Timestamp(62_000)), 0);
    }

    #[test]
    fn real_devices_get_a_real_listener() {
        let mut be = observer();
        be.install_listeners();
        let registered = LISTENING.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!registered.is_empty(), "no device got an overload listener");
    }

    /// The silent-tap heuristic must not fire on a machine that is merely
    /// quiet, or the tool would cry permissions at every idle desktop.
    #[test]
    fn an_idle_machine_is_not_reported_as_a_permission_problem() {
        let be = observer();
        assert!(!be.output_meter_is_deaf(), "no tap at all cannot be deaf");
        assert!(!be.input_meter_is_deaf());
    }
}
