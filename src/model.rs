//! The data model, and the alert rules derived from it.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

impl Direction {
    pub fn is_input(self) -> bool {
        self == Direction::Input
    }
}

/// Wall-clock milliseconds since the Unix epoch.
///
/// `TECHNICAL.md` types every timestamp as `std::time::Instant`, including inside
/// the `Tick` that snapshot/diff and `--record`/`--replay` serialise. `Instant`
/// implements neither `Serialize` nor any stable cross-process meaning, so that
/// model cannot be written to disk as specified. Lite doesn't record, but it
/// shares the family's types, so the clock is serialisable from the start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub u64);

impl Timestamp {
    pub fn now() -> Self {
        Self(
            SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        )
    }

    pub fn secs_since(&self, earlier: Timestamp) -> u64 {
        self.0.saturating_sub(earlier.0) / 1000
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub bits: u8,
    pub channels: u8,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: u32,
    pub name: String,
    /// Part of the model contract shared with the other backends; Lite's single
    /// screen only ever renders the current defaults.
    #[allow(dead_code)]
    pub direction: Direction,
    pub format: Format,
    pub buffer_frames: u32,
    /// Device latency + stream latency + safety offset, in frames.
    pub latency_frames: u32,
    #[allow(dead_code)]
    pub is_default: bool,
    pub is_running: bool,
}

impl Device {
    /// One-way latency implied by the buffer and the driver's reported latency.
    pub fn latency_ms(&self) -> Option<f32> {
        if self.format.rate == 0 {
            return None;
        }
        let frames = self.buffer_frames + self.latency_frames;
        Some(frames as f32 * 1000.0 / self.format.rate as f32)
    }
}

#[derive(Clone, Debug)]
pub struct Stream {
    /// Stable identity across ticks, so level history follows the right stream.
    pub key: String,
    pub app: String,
    pub pid: Option<i32>,
    pub device: String,
    pub device_id: u32,
    pub direction: Direction,
    pub format: Format,
    /// What the app asked for, when the backend can tell us. A difference from
    /// `format` is a conversion happening silently in the chain.
    pub requested: Option<Format>,
    pub level_dbfs: Option<f32>,
    pub latency_ms: Option<f32>,
    pub node_id: Option<u32>,
    /// When this tool first observed the stream. Not when the app opened it —
    /// see [`Caps::hold_is_since_launch`].
    pub first_seen: Timestamp,
}

/// What the active backend can actually report.
///
/// Load-bearing: the UI reads this and renders `--` plus one explanatory line
/// rather than hiding fields or shifting the layout.
#[derive(Clone, Debug, Default)]
pub struct Caps {
    /// The output meter is live.
    pub device_levels: bool,
    /// The input meter is live. Separate from `device_levels` because on macOS
    /// the two come from different mechanisms with different consent, and one
    /// routinely works while the other does not.
    pub input_levels: bool,
    pub stream_levels: bool,
    pub per_app_streams: bool,
    pub requested_format: bool,
    pub xruns: bool,
    /// True when hold durations are measured from this process's launch rather
    /// than from when the app actually opened the stream.
    pub hold_is_since_launch: bool,
    /// One line, shown under the meters when something is unavailable.
    pub note: Option<String>,
}

/// The state of the world for one repaint.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub backend: &'static str,
    pub host: String,
    pub default_out: Option<Device>,
    pub default_in: Option<Device>,
    pub streams: Vec<Stream>,
    pub xruns_60s: u32,
    pub caps: Caps,
    pub at: Timestamp,
}

impl Snapshot {
    /// Computed round-trip latency: capture chain plus playback chain.
    ///
    /// With no input device there is no round trip, so this reports the output
    /// path alone. LITE.md labels the field "computed round-trip" without saying
    /// what it means when nothing is capturing — which is most of the time.
    pub fn latency_ms(&self) -> Option<f32> {
        let out = self.default_out.as_ref().and_then(Device::latency_ms);
        let inp = self.default_in.as_ref().and_then(Device::latency_ms);
        match (inp, out) {
            (Some(i), Some(o)) => Some(i + o),
            (None, Some(o)) => Some(o),
            (Some(i), None) => Some(i),
            (None, None) => None,
        }
    }

    /// True when the round-trip figure is really only one direction.
    pub fn latency_is_one_way(&self) -> bool {
        self.default_in.is_none() || self.default_out.is_none()
    }
}

/// A single xrun is routine and usually inaudible. LITE.md triggers the alert on
/// "xruns in the last 60s", which would paint the header red essentially always
/// and contradicts its own design rule that healthy audio looks calm. A small
/// threshold keeps red meaningful.
pub const XRUN_ALERT_THRESHOLD: u32 = 3;
/// Clipped columns within the window before "sustained clipping" is declared.
/// One clipped transient is a loud snare; five is a problem.
pub const CLIP_ALERT_COLUMNS: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Nominal,
    /// `headline` goes in the header, `reason` in the vitals row.
    Alert {
        headline: String,
        reason: String,
    },
}

impl Verdict {
    pub fn is_alert(&self) -> bool {
        matches!(self, Verdict::Alert { .. })
    }
}

/// Decide whether anything is wrong, and say what in plain words.
pub fn verdict(snap: &Snapshot, clipped_columns: usize) -> Verdict {
    if snap.caps.xruns && snap.xruns_60s >= XRUN_ALERT_THRESHOLD {
        let cause = if snap
            .default_out
            .as_ref()
            .is_some_and(|d| d.buffer_frames > 0 && d.buffer_frames <= 128)
        {
            "buffer too small"
        } else {
            "dropouts"
        };
        return Verdict::Alert {
            headline: format!("{} xruns in 60s", snap.xruns_60s),
            reason: format!("{} xruns \u{b7} {cause}", snap.xruns_60s),
        };
    }

    if clipped_columns >= CLIP_ALERT_COLUMNS {
        return Verdict::Alert {
            headline: "output clipping".into(),
            reason: "sustained clipping \u{b7} reduce level".into(),
        };
    }

    if let Some(d) = &snap.default_out
        && !d.is_running
        && snap.streams.iter().any(|s| s.device_id == d.id)
    {
        return Verdict::Alert {
            headline: "device stopped".into(),
            reason: "default sink stopped mid-stream".into(),
        };
    }

    Verdict::Nominal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: u32, dir: Direction, buffer: u32, running: bool) -> Device {
        Device {
            id,
            name: "Test Device".into(),
            direction: dir,
            format: Format { rate: 48000, bits: 24, channels: 2 },
            buffer_frames: buffer,
            latency_frames: 0,
            is_default: true,
            is_running: running,
        }
    }

    fn snap(xruns: u32, out: Option<Device>, inp: Option<Device>) -> Snapshot {
        Snapshot {
            backend: "coreaudio",
            host: "test".into(),
            default_out: out,
            default_in: inp,
            streams: Vec::new(),
            xruns_60s: xruns,
            caps: Caps { xruns: true, ..Default::default() },
            at: Timestamp(0),
        }
    }

    #[test]
    fn one_xrun_does_not_paint_the_screen_red() {
        let s = snap(1, Some(dev(1, Direction::Output, 128, true)), None);
        assert_eq!(verdict(&s, 0), Verdict::Nominal);
        let s = snap(2, Some(dev(1, Direction::Output, 128, true)), None);
        assert_eq!(verdict(&s, 0), Verdict::Nominal);
    }

    #[test]
    fn a_burst_of_xruns_does() {
        let s = snap(14, Some(dev(1, Direction::Output, 128, true)), None);
        match verdict(&s, 0) {
            Verdict::Alert { headline, reason } => {
                assert_eq!(headline, "14 xruns in 60s");
                assert_eq!(reason, "14 xruns \u{b7} buffer too small");
            }
            v => panic!("expected alert, got {v:?}"),
        }
    }

    #[test]
    fn a_large_buffer_is_not_blamed_for_the_xruns() {
        let s = snap(14, Some(dev(1, Direction::Output, 1024, true)), None);
        match verdict(&s, 0) {
            Verdict::Alert { reason, .. } => assert_eq!(reason, "14 xruns \u{b7} dropouts"),
            v => panic!("expected alert, got {v:?}"),
        }
    }

    #[test]
    fn sustained_clipping_alerts_but_a_transient_does_not() {
        let s = snap(0, Some(dev(1, Direction::Output, 128, true)), None);
        assert_eq!(verdict(&s, 1), Verdict::Nominal);
        assert!(verdict(&s, CLIP_ALERT_COLUMNS).is_alert());
    }

    #[test]
    fn latency_without_an_input_is_the_output_path_alone() {
        let out = dev(1, Direction::Output, 480, false);
        let s = snap(0, Some(out), None);
        assert_eq!(s.latency_ms(), Some(10.0));
        assert!(s.latency_is_one_way());

        let s = snap(
            0,
            Some(dev(1, Direction::Output, 480, false)),
            Some(dev(2, Direction::Input, 480, false)),
        );
        assert_eq!(s.latency_ms(), Some(20.0));
        assert!(!s.latency_is_one_way());
    }

    #[test]
    fn timestamps_are_serialisable_wall_clock() {
        let a = Timestamp(1_000);
        let b = Timestamp(2_500);
        assert_eq!(b.secs_since(a), 1);
        // Ordering survives, which Instant-in-a-file would not.
        assert!(b > a);
    }
}
