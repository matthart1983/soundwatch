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
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::ffi::{self, AudioObjectID, AudioObjectPropertyAddress, AudioStreamBasicDescription};
use super::{Audio, AudioBackend, Levels, Metering, Pending};
use crate::model::{
    Caps, Device, Direction, EventKind, Format, SessionEvent, Snapshot, Stream, Timestamp,
    Transport, XrunEvent,
};

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

/// Which device most recently overloaded. A bare count says the machine
/// dropped audio; the id says *which interface* to go and look at, which is the
/// only version of that fact you can act on.
static LAST_OVERLOAD: AtomicU32 = AtomicU32::new(0);

/// Called from a CoreAudio thread. As with the IOProc, a panic here aborts
/// rather than unwinding, so it is guarded — the body is two atomic stores and
/// cannot panic today, but this is an FFI boundary and the cost is nothing.
extern "C" fn overload_listener(
    id: AudioObjectID,
    _n: u32,
    _addrs: *const AudioObjectPropertyAddress,
    _data: *mut std::ffi::c_void,
) -> i32 {
    let _ = std::panic::catch_unwind(|| {
        XRUNS.fetch_add(1, Ordering::Relaxed);
        LAST_OVERLOAD.store(id, Ordering::Relaxed);
    });
    0
}

fn transport_of(id: AudioObjectID) -> Transport {
    match ffi::get::<u32>(id, &ffi::addr(ffi::DEV_TRANSPORT_TYPE, ffi::SCOPE_GLOBAL)) {
        Some(ffi::TRANSPORT_BUILTIN) => Transport::BuiltIn,
        Some(ffi::TRANSPORT_USB) => Transport::Usb,
        Some(ffi::TRANSPORT_BLUETOOTH) | Some(ffi::TRANSPORT_BLUETOOTH_LE) => Transport::Bluetooth,
        Some(ffi::TRANSPORT_HDMI) => Transport::Hdmi,
        Some(ffi::TRANSPORT_DISPLAYPORT) => Transport::DisplayPort,
        Some(ffi::TRANSPORT_AIRPLAY) => Transport::AirPlay,
        Some(ffi::TRANSPORT_VIRTUAL) => Transport::Virtual,
        Some(ffi::TRANSPORT_AGGREGATE) => Transport::Aggregate,
        Some(ffi::TRANSPORT_THUNDERBOLT) => Transport::Thunderbolt,
        Some(ffi::TRANSPORT_FIREWIRE) => Transport::FireWire,
        Some(ffi::TRANSPORT_PCI) => Transport::Pci,
        Some(ffi::TRANSPORT_CONTINUITY) => Transport::Continuity,
        _ => Transport::Unknown,
    }
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
    /// Device names from the last tick, so an overload callback that only
    /// carries an id can still be reported by name.
    last_names: HashMap<AudioObjectID, String>,
    /// Which device the last input-meter attempt was for. Stops a device that
    /// genuinely cannot be opened from being retried every single tick.
    last_input_attempt: Option<AudioObjectID>,
    /// Set at each tick when some process is playing to the default output.
    /// Corroborates a silent tap; see [`CoreAudio::output_meter_is_deaf`].
    output_is_busy: bool,
    /// The default devices as of the last snapshot, so `audio()` can label a
    /// window with the right sample rate without re-reading the HAL — every
    /// property read is IPC, and this one runs twenty times a second.
    last_out: Option<Device>,
    last_in: Option<Device>,
    /// Samples the spectrum wants per window. Configurable, so it cannot be
    /// read off a constant.
    window_len: usize,
    /// Recent dropouts, and what has changed this session. Both are bounded:
    /// this is a session log, not a database, and a tool that grows without
    /// limit while you leave it open is a tool you stop leaving open.
    xrun_log: VecDeque<XrunEvent>,
    events: VecDeque<SessionEvent>,
    /// Previous tick's world, for diffing into events.
    ///
    /// Keyed by `(id, direction)` and carrying the name only to display.
    ///
    /// `snapshot` emits one entry per (device, direction), so a duplex
    /// interface appears twice. Keyed by name alone the two halves overwrote
    /// each other and compared against each other's format every tick,
    /// reporting a change from a value to itself once a second. Keying by
    /// `(name, direction)` fixed the common case and left the same failure for
    /// two devices *sharing* a name — two identical USB interfaces, or an
    /// aggregate wrapping a device under its own name — where it also
    /// suppressed genuine add and remove events. The id is the identity.
    prev_devices: Vec<((AudioObjectID, bool), String, Format)>,
    prev_default_out: Option<String>,
    prev_default_in: Option<String>,
    prev_streams: HashSet<String>,
    started: bool,
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

/// Bounds on the session logs. Enough to scroll back through a working
/// session, not enough to be a memory leak with a nice name.
const XRUN_LOG_MAX: usize = 256;
const EVENT_LOG_MAX: usize = 512;

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
            last_names: HashMap::new(),
            last_input_attempt: None,
            output_is_busy: false,
            last_out: None,
            last_in: None,
            window_len: crate::dsp::FFT_SIZE,
            xrun_log: VecDeque::new(),
            events: VecDeque::new(),
            prev_devices: Vec::new(),
            prev_default_out: None,
            prev_default_in: None,
            prev_streams: HashSet::new(),
            started: false,
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

        let buffer_range = ffi::get::<ffi::AudioValueRange>(
            id,
            &ffi::addr(ffi::DEV_BUFFER_RANGE, ffi::SCOPE_GLOBAL),
        )
        .map(|r| (r.mMinimum.max(0.0) as u32, r.mMaximum.max(0.0) as u32));

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
            uid: ffi::get_string(id, &ffi::addr(ffi::DEV_UID, ffi::SCOPE_GLOBAL)),
            manufacturer: ffi::get_string(id, &ffi::addr(ffi::DEV_MANUFACTURER, ffi::SCOPE_GLOBAL)),
            transport: transport_of(id),
            device_latency,
            stream_latency,
            safety_offset: safety,
            buffer_range,
            clock_domain: ffi::get::<u32>(id, &ffi::addr(ffi::DEV_CLOCK_DOMAIN, ffi::SCOPE_GLOBAL))
                .unwrap_or(0),
            is_alive: ffi::get::<u32>(id, &ffi::addr(ffi::DEV_IS_ALIVE, ffi::SCOPE_GLOBAL))
                .unwrap_or(1)
                != 0,
            sub_device_uids: ffi::get_string_array(
                id,
                &ffi::addr(ffi::AGG_FULL_SUB_DEVICE_LIST, ffi::SCOPE_GLOBAL),
            ),
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

    /// Record an event, keeping the log bounded.
    fn log(&mut self, at: Timestamp, kind: EventKind, what: String) {
        if self.events.len() >= EVENT_LOG_MAX {
            self.events.pop_front();
        }
        self.events.push_back(SessionEvent { at, kind, what });
    }

    /// Diff this tick against the last and record what changed.
    ///
    /// The Timeline tab exists because the hard audio bugs are the ones where
    /// something changed and you did not see it: a device woke up and stole the
    /// default, a format shifted under a running stream, an interface dropped
    /// off the bus for a second. A snapshot cannot show any of that; only the
    /// difference between two of them can.
    fn record_changes(
        &mut self,
        now: Timestamp,
        devices: &[Device],
        default_out: Option<&Device>,
        default_in: Option<&Device>,
        streams: &[Stream],
    ) {
        if !self.started {
            self.started = true;
            self.log(now, EventKind::Started, format!("{} devices", devices.len()));
            // What was already playing when we attached is the initial state,
            // not something that happened; logging it as StreamOpened a second
            // after launch is a lie about when it started.
            self.prev_streams = streams.iter().map(|s| s.key.clone()).collect();
        } else {
            // Collected before logging: `log` takes &mut self and the diff
            // borrows the previous state immutably.
            let prev: HashMap<(AudioObjectID, bool), (&str, Format)> =
                self.prev_devices.iter().map(|(k, name, f)| (*k, (name.as_str(), *f))).collect();
            let known: HashSet<AudioObjectID> = prev.keys().map(|(id, _)| *id).collect();

            let mut changes: Vec<(EventKind, String)> = Vec::new();
            let mut announced: HashSet<AudioObjectID> = HashSet::new();
            for d in devices {
                match prev.get(&(d.id, d.direction.is_input())) {
                    // A new device announces itself once, not once per scope.
                    None if !known.contains(&d.id) && announced.insert(d.id) => {
                        changes.push((
                            EventKind::DeviceAdded,
                            format!("{} ({})", d.name, d.transport.name()),
                        ));
                    }
                    // A scope appearing on a device we already knew about is
                    // not a new device and not a format change.
                    None => {}
                    Some((_, f)) if *f != d.format => changes.push((
                        EventKind::FormatChanged,
                        format!(
                            "{} {}: {} \u{2192} {}",
                            d.name,
                            if d.direction.is_input() { "in" } else { "out" },
                            crate::fmt::rate_bits(f.rate, f.bits),
                            crate::fmt::rate_bits(d.format.rate, d.format.bits)
                        ),
                    )),
                    _ => {}
                }
            }
            for (kind, what) in changes {
                self.log(now, kind, what);
            }
            // Removals are reported once per device, not once per direction:
            // unplugging one interface is one event however many scopes it had.
            let live: HashSet<AudioObjectID> = devices.iter().map(|d| d.id).collect();
            let mut gone: Vec<(AudioObjectID, String)> = self
                .prev_devices
                .iter()
                .filter(|((id, _), _, _)| !live.contains(id))
                .map(|((id, _), name, _)| (*id, name.clone()))
                .collect();
            gone.sort_unstable();
            gone.dedup_by_key(|(id, _)| *id);
            for (_, name) in gone {
                self.log(now, EventKind::DeviceRemoved, name);
            }

            let default_changes: Vec<String> = [
                ("output", self.prev_default_out.clone(), default_out.map(|d| d.name.clone())),
                ("input", self.prev_default_in.clone(), default_in.map(|d| d.name.clone())),
            ]
            .into_iter()
            .filter(|(_, was, now_name)| was.as_deref() != now_name.as_deref())
            .map(|(label, _, now_name)| {
                format!("{label} \u{2192} {}", now_name.unwrap_or_else(|| "none".into()))
            })
            .collect();
            for what in default_changes {
                self.log(now, EventKind::DefaultChanged, what);
            }

            let live_streams: HashSet<String> = streams.iter().map(|s| s.key.clone()).collect();
            let opened: Vec<String> = streams
                .iter()
                .filter(|s| !self.prev_streams.contains(&s.key))
                .map(|s| format!("{} on {}", s.app, s.device))
                .collect();
            for what in opened {
                self.log(now, EventKind::StreamOpened, what);
            }
            let closed: Vec<String> =
                self.prev_streams.difference(&live_streams).cloned().collect();
            for key in closed {
                // The key is pid:direction:device; the app name is gone with it.
                self.log(now, EventKind::StreamClosed, key);
            }
            self.prev_streams = live_streams;
        }

        self.prev_devices = devices
            .iter()
            .map(|d| ((d.id, d.direction.is_input()), d.name.clone(), d.format))
            .collect();
        self.prev_default_out = default_out.map(|d| d.name.clone());
        self.prev_default_in = default_in.map(|d| d.name.clone());
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
            let id = LAST_OVERLOAD.load(Ordering::Relaxed);
            let device =
                self.last_names.get(&id).cloned().unwrap_or_else(|| {
                    if id == 0 { "unknown".into() } else { format!("device {id}") }
                });
            if self.xrun_log.len() >= XRUN_LOG_MAX {
                self.xrun_log.pop_front();
            }
            self.xrun_log.push_back(XrunEvent {
                at: now,
                device: device.clone(),
                count: delta as u32,
            });
            if self.events.len() >= EVENT_LOG_MAX {
                self.events.pop_front();
            }
            self.events.push_back(SessionEvent {
                at: now,
                kind: EventKind::Xrun,
                what: format!("{delta} on {device}"),
            });
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
            // Not truncated here: the renderer clips row 11 to whatever the
            // terminal is, and a note that needed truncating at 78 columns
            // would still be wrong on a wide screen. The requirement is that
            // these fit the *smallest* supported screen, which is a test.
            note,
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
        self.last_names = names.clone();

        // Every device, both directions. A device with channels in both shows
        // up twice — which is the truth: they are two independent paths with
        // their own formats, buffers and latencies.
        let mut devices: Vec<Device> = Vec::new();
        for id in Self::all_devices() {
            for dir in [Direction::Output, Direction::Input] {
                if let Some(d) = Self::read_device(id, dir, false) {
                    devices.push(d);
                }
            }
        }
        devices.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(b.direction.is_input().cmp(&a.direction.is_input()))
        });

        let default_out = Self::default_device(true)
            .and_then(|id| Self::read_device(id, Direction::Output, true));
        let default_in = Self::default_device(false)
            .and_then(|id| Self::read_device(id, Direction::Input, true));

        let streams = self.read_streams(&names, default_out.as_ref(), default_in.as_ref(), now);
        self.forget_stale(&streams);
        let xruns_60s = self.xruns_60s(now);

        self.last_out = default_out.clone();
        self.last_in = default_in.clone();

        // Corroboration for the silent-tap check: someone is actually playing.
        self.output_is_busy = default_out.as_ref().is_some_and(|d| {
            d.is_running && streams.iter().any(|s| !s.direction.is_input() && s.device_id == d.id)
        });

        // Mark the defaults inside the full list, so the Devices tab can show
        // them without a second lookup.
        for d in &mut devices {
            let is_out = !d.direction.is_input();
            let def = if is_out { default_out.as_ref() } else { default_in.as_ref() };
            d.is_default = def.is_some_and(|x| x.id == d.id);
        }

        self.record_changes(now, &devices, default_out.as_ref(), default_in.as_ref(), &streams);

        Snapshot {
            backend: self.name(),
            host: self.host.clone(),
            default_out,
            default_in,
            devices,
            streams,
            xruns_60s,
            xrun_log: self.xrun_log.iter().cloned().collect(),
            events: self.events.iter().cloned().collect(),
            caps: self.caps(),
            at: now,
        }
    }

    fn set_input_metering(&mut self, on: bool) {
        // No early return on "no change": the caller's idea of the current
        // state is not authoritative. The worker starts believing metering is
        // off, so a session launched with --meter-input and then switched off
        // in the menu used to be a no-op — leaving the capture stream open,
        // and this process in the mic-in-use audit, while the menu said off.
        if on && self.input.live().is_some() {
            return;
        }
        self.metering.input = on;
        self.last_input_attempt = None;
        self.input = if on {
            Pending::spawn(super::input::InputMeter::start)
        } else {
            // Dropping the meter stops and detaches the IOProc, which also
            // releases the device if we were its only client — and takes this
            // process back out of the mic-in-use audit.
            Pending::Off
        };
    }

    fn set_window_len(&mut self, n: usize) {
        self.window_len = n;
    }

    fn input_metering(&self) -> bool {
        self.metering.input
    }

    fn audio(&mut self, want: crate::spectrum::Source) -> Option<Audio> {
        use crate::spectrum::Source;
        let rate = |d: &Option<Device>| d.as_ref().map(|d| d.format.rate).unwrap_or(48_000);
        let (samples, rate, overruns) = match want {
            Source::Off => return None,
            Source::Output => {
                let t = self.tap.live()?;
                (t.recent(self.window_len)?, rate(&self.last_out), t.overruns())
            }
            Source::Input => {
                let m = self.input.live()?;
                (m.recent(self.window_len)?, rate(&self.last_in), m.overruns())
            }
        };
        Some(Audio { source: want, samples, rate, overruns })
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
            out_rms_dbfs: self.tap.live().and_then(|t| t.rms_dbfs()),
            in_rms_dbfs: self.input.live().and_then(|m| m.rms_dbfs()),
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

    fn dev(id: AudioObjectID, name: &str, dir: Direction, rate: u32, ch: u8) -> Device {
        Device {
            id,
            name: name.into(),
            direction: dir,
            format: Format { rate, bits: 24, channels: ch },
            buffer_frames: 512,
            latency_frames: 0,
            is_default: false,
            is_running: true,
            uid: None,
            manufacturer: None,
            transport: Transport::Usb,
            device_latency: 0,
            stream_latency: 0,
            safety_offset: 0,
            buffer_range: None,
            clock_domain: 0,
            is_alive: true,
            sub_device_uids: Vec::new(),
        }
    }

    /// A duplex interface is two entries under one name with different channel
    /// counts. Keyed by name alone the two halves compared against each other
    /// every tick and reported a format change from a value to itself — once a
    /// second, until the event log had evicted everything real and Insights
    /// showed a permanent warning.
    #[test]
    fn a_duplex_device_does_not_report_a_format_change_every_tick() {
        let mut be = observer();
        let devices = vec![
            dev(9, "Scarlett 2i2", Direction::Output, 48_000, 2),
            dev(9, "Scarlett 2i2", Direction::Input, 48_000, 1),
        ];
        be.record_changes(Timestamp(0), &devices, None, None, &[]);
        for t in 1..10u64 {
            be.record_changes(Timestamp(t * 1000), &devices, None, None, &[]);
        }
        let spam = be.events.iter().filter(|e| e.kind == EventKind::FormatChanged).count();
        assert_eq!(spam, 0, "a steady duplex device reported {spam} format changes");
    }

    /// Plugging one in is one event, not one per scope.
    #[test]
    fn plugging_in_a_duplex_device_is_a_single_event() {
        let mut be = observer();
        be.record_changes(Timestamp(0), &[], None, None, &[]);
        let devices = vec![
            dev(9, "Scarlett 2i2", Direction::Output, 48_000, 2),
            dev(9, "Scarlett 2i2", Direction::Input, 48_000, 1),
        ];
        be.record_changes(Timestamp(1000), &devices, None, None, &[]);
        let added = be.events.iter().filter(|e| e.kind == EventKind::DeviceAdded).count();
        assert_eq!(added, 1, "one interface produced {added} add events");

        be.record_changes(Timestamp(2000), &[], None, None, &[]);
        let removed = be.events.iter().filter(|e| e.kind == EventKind::DeviceRemoved).count();
        assert_eq!(removed, 1, "one interface produced {removed} removals");
    }

    /// Two devices can share a name — two identical USB interfaces, or an
    /// aggregate wrapping a device under it. Keyed by name they collapse to one
    /// entry, reproduce the flood, and swallow genuine add and remove events.
    #[test]
    fn two_devices_with_the_same_name_are_told_apart() {
        let both = vec![
            dev(10, "USB Audio CODEC", Direction::Output, 44_100, 2),
            dev(11, "USB Audio CODEC", Direction::Output, 48_000, 2),
        ];
        let mut be = observer();
        be.record_changes(Timestamp(0), &both, None, None, &[]);
        for t in 1..10u64 {
            be.record_changes(Timestamp(t * 1000), &both, None, None, &[]);
        }
        let spam = be.events.iter().filter(|e| e.kind == EventKind::FormatChanged).count();
        assert_eq!(spam, 0, "two same-named devices reported {spam} format changes");

        let mut be = observer();
        be.record_changes(Timestamp(0), &both[..1], None, None, &[]);
        be.record_changes(Timestamp(1000), &both, None, None, &[]);
        assert_eq!(
            be.events.iter().filter(|e| e.kind == EventKind::DeviceAdded).count(),
            1,
            "the second same-named device was not reported as added"
        );
        be.record_changes(Timestamp(2000), &both[..1], None, None, &[]);
        assert_eq!(
            be.events.iter().filter(|e| e.kind == EventKind::DeviceRemoved).count(),
            1,
            "the second same-named device was not reported as removed"
        );
    }

    /// A real format change still has to be reported.
    #[test]
    fn a_real_format_change_is_still_caught() {
        let mut be = observer();
        let before = vec![dev(3, "AirPods", Direction::Output, 48_000, 2)];
        let after = vec![dev(3, "AirPods", Direction::Output, 16_000, 2)];
        be.record_changes(Timestamp(0), &before, None, None, &[]);
        be.record_changes(Timestamp(1000), &after, None, None, &[]);
        let e = be
            .events
            .iter()
            .find(|e| e.kind == EventKind::FormatChanged)
            .expect("a real rate change went unreported");
        assert!(e.what.contains("48k") && e.what.contains("16k"), "{}", e.what);
    }

    /// What was already playing when the tool attached is the initial state,
    /// not an event that happened a second after launch.
    #[test]
    fn streams_present_at_launch_are_not_logged_as_opening() {
        let mut be = observer();
        let s = Stream {
            key: "1:out:2".into(),
            app: "Spotify".into(),
            pid: Some(1),
            device: "Speakers".into(),
            device_id: 2,
            direction: Direction::Output,
            format: Format::default(),
            requested: None,
            level_dbfs: None,
            latency_ms: None,
            node_id: None,
            first_seen: Timestamp(0),
        };
        be.record_changes(Timestamp(0), &[], None, None, std::slice::from_ref(&s));
        be.record_changes(Timestamp(1000), &[], None, None, std::slice::from_ref(&s));
        let opened = be.events.iter().filter(|e| e.kind == EventKind::StreamOpened).count();
        assert_eq!(opened, 0, "a stream already playing was logged as opening");
    }

    /// A session launched with --meter-input has to be able to switch it off.
    /// The worker used to seed its idea of the state to false, so the first
    /// switch-off was computed as a no-op and the capture stream stayed open
    /// while the menu said it was closed.
    #[test]
    fn input_metering_can_be_switched_off_from_on() {
        let mut be = CoreAudio::new(Metering { output: false, input: false });
        assert!(!be.input_metering());
        be.metering.input = true;
        be.set_input_metering(false);
        assert!(!be.input_metering(), "switching off from on was ignored");
        assert!(be.input.live().is_none());
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
            crate::grid::width(&note) <= crate::grid::content(crate::grid::MIN_COLS).width(),
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
                crate::grid::width(&note) <= crate::grid::content(crate::grid::MIN_COLS).width(),
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
