//! The Linux backend.
//!
//! The `AudioBackend` trait has claimed since the first commit that PipeWire,
//! PulseAudio and ALSA slot in behind it. This is that claim being cashed, and
//! `Caps` is what makes it honest: the UI asks what the backend can report and
//! draws `--` plus one explanatory line for the rest, so a platform that knows
//! less produces a smaller screen rather than a wrong one.
//!
//! Hardware comes from `/proc/asound` (see [`proc`]) — no library, no FFI, no
//! subprocess per tick, and it reports the same devices whether the machine
//! runs PipeWire, PulseAudio or nothing.
//!
//! Levels come from libpulse, opened with `dlopen` rather than linked (see
//! [`pulse`]). PipeWire ships a PulseAudio-compatible library, so one small
//! interface covers both of the servers anyone actually runs, and a machine
//! with neither degrades to `--` instead of failing to launch — the same
//! lesson the macOS side learned when a hard link to a 14.2-only symbol made
//! the binary die in the loader on Ventura.
//!
//! **Per-app streams are not reported.** Under PipeWire every application is a
//! client of a PipeWire node, so ALSA sees `pipewire` holding the card and
//! nothing about who is behind it. Getting the real list means speaking to the
//! sound server's object model, which is a different and much larger piece of
//! work than this; the Streams tab says so rather than showing an empty table.

pub mod proc;
pub mod pulse;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use super::{Audio, AudioBackend, Levels, Metering, Pending};
use crate::model::{
    Caps, Device, Direction, EventKind, Format, SessionEvent, Snapshot, Stream, Timestamp,
    Transport, XrunEvent,
};

const XRUN_LOG_MAX: usize = 256;
const EVENT_LOG_MAX: usize = 512;

pub struct Alsa {
    root: PathBuf,
    host: String,
    metering: Metering,
    out_meter: Pending<pulse::Monitor>,
    in_meter: Pending<pulse::Monitor>,
    window_len: usize,
    /// Xrun totals per device, so the deltas can be turned into events. ALSA
    /// counts since the substream opened, which is a lifetime counter and
    /// therefore noise; what matters is what just happened.
    last_xruns: HashMap<String, u64>,
    xrun_window: VecDeque<(Timestamp, u32)>,
    xrun_log: VecDeque<XrunEvent>,
    events: VecDeque<SessionEvent>,
    prev: Vec<(String, Format, bool)>,
    first_seen: HashMap<String, Timestamp>,
    prev_streams: std::collections::HashSet<String>,
    /// Whatever was already open when the tool attached is the initial state,
    /// not something that happened a second after launch.
    started_streams: bool,
    started: bool,
}

impl Alsa {
    pub fn new(metering: Metering) -> Self {
        Self::with_root(metering, PathBuf::from("/proc/asound"))
    }

    /// The same, rooted anywhere — which is how the tests drive it against a
    /// captured `/proc/asound` instead of the machine's own.
    pub fn with_root(metering: Metering, root: PathBuf) -> Self {
        let out_meter = match metering.output {
            true => Pending::spawn(|| pulse::Monitor::open(pulse::Which::Output)),
            false => Pending::Off,
        };
        let in_meter = match metering.input {
            true => Pending::spawn(|| pulse::Monitor::open(pulse::Which::Input)),
            false => Pending::Off,
        };
        Self {
            root,
            host: crate::backend::linux::hostname(),
            metering,
            out_meter,
            in_meter,
            window_len: crate::dsp::FFT_SIZE,
            last_xruns: HashMap::new(),
            xrun_window: VecDeque::new(),
            xrun_log: VecDeque::new(),
            events: VecDeque::new(),
            prev: Vec::new(),
            first_seen: HashMap::new(),
            prev_streams: std::collections::HashSet::new(),
            started_streams: false,
            started: false,
        }
    }

    fn log(&mut self, at: Timestamp, kind: EventKind, what: String) {
        if self.events.len() >= EVENT_LOG_MAX {
            self.events.pop_front();
        }
        self.events.push_back(SessionEvent { at, kind, what });
    }

    /// Turn ALSA's lifetime xrun counters into a trailing-60-second count and a
    /// log of what changed, the same shape the CoreAudio backend produces.
    fn roll_xruns(&mut self, now: Timestamp, pcms: &[proc::Pcm]) -> u32 {
        let mut fresh: Vec<(String, u32)> = Vec::new();
        for p in pcms {
            let id = p.id();
            let prev = self.last_xruns.get(&id).copied();
            self.last_xruns.insert(id.clone(), p.xruns);
            // A device that just opened starts its counter at zero; the first
            // reading is a baseline, not a burst.
            if let Some(prev) = prev
                && p.xruns > prev
            {
                fresh.push((format!("{} ({})", p.card_name, p.id()), (p.xruns - prev) as u32));
            }
        }
        for (device, count) in fresh {
            self.xrun_window.push_back((now, count));
            if self.xrun_log.len() >= XRUN_LOG_MAX {
                self.xrun_log.pop_front();
            }
            self.xrun_log.push_back(XrunEvent { at: now, device: device.clone(), count });
            self.log(now, EventKind::Xrun, format!("{count} on {device}"));
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

    fn record_changes(&mut self, now: Timestamp, devices: &[Device], streams: &[Stream]) {
        if !self.started {
            self.started = true;
            self.log(now, EventKind::Started, format!("{} devices", devices.len()));
        } else {
            let prev: HashMap<&str, Format> =
                self.prev.iter().map(|(n, f, _)| (n.as_str(), *f)).collect();
            let mut changes = Vec::new();
            for d in devices {
                match prev.get(d.name.as_str()) {
                    None => changes.push((
                        EventKind::DeviceAdded,
                        format!("{} ({})", d.name, d.transport.name()),
                    )),
                    // Only when both are real: a device that just opened moves
                    // from "no negotiated format" to one, which is it starting,
                    // not changing.
                    Some(f) if *f != d.format && f.rate != 0 && d.format.rate != 0 => {
                        changes.push((
                            EventKind::FormatChanged,
                            format!(
                                "{}: {} \u{2192} {}",
                                d.name,
                                crate::fmt::rate_bits(f.rate, f.bits),
                                crate::fmt::rate_bits(d.format.rate, d.format.bits)
                            ),
                        ))
                    }
                    _ => {}
                }
            }
            let live: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
            let gone: Vec<String> = self
                .prev
                .iter()
                .map(|(n, _, _)| n.clone())
                .filter(|n| !live.contains(&n.as_str()))
                .collect();
            for n in gone {
                changes.push((EventKind::DeviceRemoved, n));
            }
            for (kind, what) in changes {
                self.log(now, kind, what);
            }
        }
        // Devices opening and closing is the Linux equivalent of a stream
        // starting: ALSA has no per-application view, but "something started
        // using the card" is exactly what the Timeline is for.
        let live: std::collections::HashSet<String> =
            streams.iter().map(|s| s.key.clone()).collect();
        let opened: Vec<String> = streams
            .iter()
            .filter(|s| !self.prev_streams.contains(&s.key))
            .map(|s| format!("{} on {}", s.app, s.device))
            .collect();
        let closed: Vec<String> = self.prev_streams.difference(&live).cloned().collect();
        if self.started_streams {
            for what in opened {
                self.log(now, EventKind::StreamOpened, what);
            }
            for key in closed {
                self.log(now, EventKind::StreamClosed, key);
            }
        }
        self.started_streams = true;
        self.prev_streams = live;

        self.prev = devices.iter().map(|d| (d.name.clone(), d.format, d.is_running)).collect();
    }
}

/// The process holding a device, named rather than numbered.
///
/// Under PipeWire this is almost always `pipewire` itself, which is exactly the
/// point worth making on the Streams tab: the sound server is the only ALSA
/// client, and the applications behind it are invisible from here. A row
/// saying so is more use than an empty table.
fn owner_name(pid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("pid {pid}"))
}

/// `/proc/asound` describes a PCM, not a routing graph, so "transport" is
/// inferred from the driver ALSA names. It is the same question the macOS
/// column answers — is this thing on USB, on HDMI, or built in — and the driver
/// is where Linux keeps that answer.
fn transport_of(driver: &str, name: &str) -> Transport {
    let d = driver.to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    if d.contains("usb") {
        Transport::Usb
    } else if n.contains("hdmi") || n.contains("displayport") {
        Transport::Hdmi
    } else if d.contains("bluetooth") || n.contains("bluez") {
        Transport::Bluetooth
    } else if d.contains("hda") || d.contains("intel") || d.contains("ac97") {
        Transport::BuiltIn
    } else if d.contains("loopback") || d.contains("dummy") || d.contains("virmidi") {
        Transport::Virtual
    } else if d.contains("firewire") {
        Transport::FireWire
    } else {
        Transport::Unknown
    }
}

fn to_device(p: &proc::Pcm) -> Device {
    let direction = if p.playback { Direction::Output } else { Direction::Input };
    let name = if p.pcm_name.is_empty() || p.pcm_name == p.card_name {
        p.card_name.clone()
    } else {
        format!("{} {}", p.card_name, p.pcm_name)
    };
    Device {
        // /proc has no object ids, so the card/device pair is the identity —
        // and unlike a name it is unique, which the macOS side learned the
        // hard way when two devices shared one.
        id: p.card * 256 + p.device * 2 + u32::from(!p.playback),
        name,
        direction,
        format: Format { rate: p.params.rate, bits: p.params.bits, channels: p.params.channels },
        buffer_frames: p.params.buffer_frames,
        // ALSA reports the buffer and the period; the hardware's own latency
        // and safety offset have no equivalent here, and are left at zero
        // rather than guessed. The Latency tab shows what it was given.
        latency_frames: 0,
        is_default: false,
        is_running: p.params.open,
        uid: Some(p.id()),
        manufacturer: (!p.driver.is_empty()).then(|| p.driver.clone()),
        transport: transport_of(&p.driver, &p.pcm_name),
        device_latency: 0,
        stream_latency: 0,
        safety_offset: 0,
        buffer_range: None,
        clock_domain: p.card,
        is_alive: true,
        sub_device_uids: Vec::new(),
    }
}

pub(crate) fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().split('.').next().unwrap_or("localhost").to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

impl AudioBackend for Alsa {
    fn name(&self) -> &'static str {
        "alsa"
    }

    fn caps(&self) -> Caps {
        let note = if let Some(e) = self.out_meter.error() {
            Some(e.to_string())
        } else if let Some(e) = self.in_meter.error() {
            Some(format!("input meter unavailable: {e}"))
        } else if self.out_meter.is_starting() || self.in_meter.is_starting() {
            Some("opening the meters\u{2026}".into())
        } else if self
            .out_meter
            .live()
            .is_some_and(|m| m.running_for(8) && !m.has_ever_heard_signal())
        {
            // The same quiet failure the macOS tap has: a monitor that opens
            // and delivers nothing but zeros looks exactly like a quiet machine.
            Some("output meter reads silence \u{2014} is anything playing?".into())
        } else if !self.metering.any() {
            Some("levels not metered in this mode".into())
        } else {
            Some("streams are ALSA device holders; per-application needs the sound server".into())
        };
        Caps {
            device_levels: self.out_meter.live().is_some(),
            input_levels: self.in_meter.live().is_some(),
            stream_levels: false,
            // ALSA sees `pipewire` holding the card, not the applications
            // behind it. Saying so is better than an empty table.
            // Device holders, yes; the applications behind the sound server,
            // no. The Streams tab shows the former and the note explains.
            per_app_streams: true,
            requested_format: false,
            xruns: true,
            hold_is_since_launch: true,
            note,
        }
    }

    fn snapshot(&mut self) -> Snapshot {
        self.out_meter.poll();
        self.in_meter.poll();
        let now = Timestamp::now();

        let pcms = proc::devices(&self.root);
        let mut devices: Vec<Device> = pcms.iter().map(to_device).collect();
        devices.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(b.direction.is_input().cmp(&a.direction.is_input()))
        });

        // "Default" here means the one carrying audio: ALSA has no notion of a
        // system default, and the running device is the honest answer to the
        // question the header is asking.
        let pick = |input: bool| -> Option<Device> {
            devices
                .iter()
                .filter(|d| d.direction.is_input() == input)
                .find(|d| d.is_running)
                .or_else(|| devices.iter().find(|d| d.direction.is_input() == input))
                .cloned()
        };
        let mut default_out = pick(false);
        let mut default_in = pick(true);
        for d in [&mut default_out, &mut default_in].into_iter().flatten() {
            d.is_default = true;
        }
        for d in devices.iter_mut() {
            d.is_default = default_out.as_ref().is_some_and(|x| x.id == d.id)
                || default_in.as_ref().is_some_and(|x| x.id == d.id);
        }

        // One stream per open device, attributed to the process ALSA says is
        // holding it. Not per-application — see the module note — but real.
        let streams: Vec<Stream> = pcms
            .iter()
            .filter(|p| p.params.open)
            .map(|p| {
                let pid = p.owner_pid;
                let app = pid.map(owner_name).unwrap_or_else(|| "unknown".into());
                let dev = devices.iter().find(|d| d.uid.as_deref() == Some(&p.id())).cloned();
                Stream {
                    key: format!("{}:{}", pid.unwrap_or(-1), p.id()),
                    app,
                    pid,
                    device: dev.as_ref().map(|d| d.name.clone()).unwrap_or_else(|| p.id()),
                    device_id: dev.as_ref().map(|d| d.id).unwrap_or(0),
                    direction: if p.playback { Direction::Output } else { Direction::Input },
                    format: Format {
                        rate: p.params.rate,
                        bits: p.params.bits,
                        channels: p.params.channels,
                    },
                    requested: None,
                    level_dbfs: None,
                    latency_ms: (p.params.rate > 0)
                        .then(|| p.params.buffer_frames as f32 * 1000.0 / p.params.rate as f32),
                    node_id: None,
                    first_seen: *self.first_seen.entry(p.id()).or_insert(now),
                }
            })
            .collect();
        let live: Vec<String> = streams.iter().map(|s| s.key.clone()).collect();
        self.first_seen.retain(|k, _| live.iter().any(|l| l.ends_with(k.as_str())));

        let xruns_60s = self.roll_xruns(now, &pcms);
        self.record_changes(now, &devices, &streams);

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

    fn levels(&mut self) -> Levels {
        self.out_meter.poll();
        self.in_meter.poll();
        Levels {
            out_dbfs: self.out_meter.live().and_then(|m| m.peak_dbfs()),
            in_dbfs: self.in_meter.live().and_then(|m| m.peak_dbfs()),
            out_rms_dbfs: self.out_meter.live().and_then(|m| m.rms_dbfs()),
            in_rms_dbfs: self.in_meter.live().and_then(|m| m.rms_dbfs()),
        }
    }

    fn set_input_metering(&mut self, on: bool) {
        if on && self.in_meter.live().is_some() {
            return;
        }
        self.metering.input = on;
        self.in_meter = if on {
            Pending::spawn(|| pulse::Monitor::open(pulse::Which::Input))
        } else {
            Pending::Off
        };
    }

    fn input_metering(&self) -> bool {
        self.metering.input
    }

    fn set_window_len(&mut self, n: usize) {
        self.window_len = n;
    }

    fn audio(&mut self, want: crate::spectrum::Source) -> Option<Audio> {
        use crate::spectrum::Source;
        let rate = 48_000;
        let (samples, overruns) = match want {
            Source::Off => return None,
            Source::Output => {
                let m = self.out_meter.live()?;
                (m.recent(self.window_len)?, m.overruns())
            }
            Source::Input => {
                let m = self.in_meter.live()?;
                (m.recent(self.window_len)?, m.overruns())
            }
        };
        Some(Audio { source: want, samples, rate, overruns })
    }
}
