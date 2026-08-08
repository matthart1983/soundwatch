//! Deterministic fixtures, ported from the handoff's reference renderer.
//!
//! `--demo` drives the real UI from the same seeded series the HTML mockups
//! use, so the layout and the meter maths are reviewable anywhere: on a machine
//! where audio-capture consent is unavailable, in CI, and in the tests. It
//! opens no device and creates no audio object. It is a design-review and test
//! aid, never a substitute for live data — the header says `demo` where the
//! backend name goes.

use crate::model::{Caps, Device, Direction, Format, Snapshot, Stream, Timestamp};

/// Width the handoff's fixtures were authored at: the content columns of an
/// 80-column terminal. The history ring is a different, larger resolution, and
/// `History::from_series` stretches these across it — keeping the demo screens
/// pixel-identical to the mockups while the ring underneath is free to change.
pub const FIXTURE_COLS: usize = 78;

/// The mockup's LCG series, reproduced so screenshots line up with the spec.
pub fn levels(seed: u32, n: usize, avg_db: f32, spread: f32, clip_at: f32) -> Vec<f32> {
    let mut s = seed as u64;
    (0..n)
        .map(|i| {
            s = (s * 9301 + 49297) % 233280;
            let r = s as f32 / 233280.0;
            let env = (i as f32 / 6.0).sin() * 0.5 + (i as f32 / 17.0).sin() * 0.5;
            let mut d = avg_db + env * spread * 0.5 + (r - 0.5) * spread;
            let fi = i as f32;
            let fn_ = n as f32;
            if clip_at > -50.0 && fi > fn_ * 0.62 && fi < fn_ * 0.68 {
                d = clip_at;
            }
            d.clamp(crate::meter::FLOOR_DBFS, 0.0)
        })
        .collect()
}

struct Fixture {
    app: &'static str,
    dev: &'static str,
    input: bool,
    level: f32,
    rate: u32,
    bits: u8,
    requested: Option<(u32, u8)>,
    lat: f32,
    seed: u32,
    pid: i32,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        app: "zoom.us",
        dev: "MacBook Mic",
        input: true,
        level: -31.6,
        rate: 48000,
        bits: 24,
        requested: None,
        lat: 11.8,
        seed: 11,
        pid: 4218,
    },
    Fixture {
        app: "obs",
        dev: "Scarlett 2i2 In 1",
        input: true,
        level: -18.4,
        rate: 48000,
        bits: 24,
        requested: None,
        lat: 5.3,
        seed: 22,
        pid: 5120,
    },
    Fixture {
        app: "Spotify",
        dev: "MacBook Speakers",
        input: false,
        level: -8.2,
        rate: 48000,
        bits: 24,
        requested: Some((44100, 24)),
        lat: 11.8,
        seed: 33,
        pid: 3301,
    },
    Fixture {
        app: "zoom.us",
        dev: "MacBook Speakers",
        input: false,
        level: -14.2,
        rate: 48000,
        bits: 24,
        requested: None,
        lat: 11.8,
        seed: 44,
        pid: 4218,
    },
    Fixture {
        app: "Firefox",
        dev: "MacBook Speakers",
        input: false,
        level: -22.8,
        rate: 48000,
        bits: 24,
        requested: None,
        lat: 11.8,
        seed: 55,
        pid: 2044,
    },
    Fixture {
        app: "Ardour",
        dev: "Scarlett 2i2 Out",
        input: false,
        level: -3.1,
        rate: 48000,
        bits: 32,
        requested: None,
        lat: 2.7,
        seed: 66,
        pid: 7781,
    },
    // Requested 24-bit into a sink running 16-bit: a silent truncation, and the
    // case LITE.md's rate-only RATE column would render as unremarkable text.
    Fixture {
        app: "Slack",
        dev: "MacBook Speakers",
        input: false,
        level: -38.4,
        rate: 48000,
        bits: 16,
        requested: Some((48000, 24)),
        lat: 11.8,
        seed: 77,
        pid: 1180,
    },
    Fixture {
        app: "coreaudiod",
        dev: "BlackHole 2ch",
        input: false,
        level: -46.2,
        rate: 48000,
        bits: 24,
        requested: None,
        lat: 1.3,
        seed: 88,
        pid: 311,
    },
];

pub fn stream_series(index: usize) -> Vec<f32> {
    let f = &FIXTURES[index % FIXTURES.len()];
    levels(f.seed, FIXTURE_COLS, f.level, 10.0, -70.0)
}

pub fn out_series(alert: bool) -> Vec<f32> {
    levels(
        7,
        FIXTURE_COLS,
        if alert { -6.0 } else { -16.0 },
        16.0,
        if alert { -0.05 } else { -70.0 },
    )
}

pub fn in_series() -> Vec<f32> {
    levels(23, FIXTURE_COLS, -30.0, 12.0, -70.0)
}

fn device(id: u32, name: &str, direction: Direction) -> Device {
    Device {
        id,
        name: name.into(),
        direction,
        format: Format { rate: 48000, bits: 24, channels: 2 },
        buffer_frames: 128,
        latency_frames: 155,
        is_default: true,
        is_running: true,
    }
}

pub fn snapshot(xruns: u32) -> Snapshot {
    let now = Timestamp::now();
    let streams = FIXTURES
        .iter()
        .enumerate()
        .map(|(i, f)| Stream {
            key: format!("demo:{i}"),
            app: f.app.into(),
            pid: Some(f.pid),
            device: f.dev.into(),
            device_id: if f.input { 2 } else { 1 },
            direction: if f.input { Direction::Input } else { Direction::Output },
            format: Format { rate: f.rate, bits: f.bits, channels: 2 },
            requested: f.requested.map(|(rate, bits)| Format { rate, bits, channels: 2 }),
            level_dbfs: Some(f.level),
            latency_ms: Some(f.lat),
            node_id: Some(62 + i as u32),
            first_seen: Timestamp(now.0.saturating_sub(2_538_000)),
        })
        .collect();

    Snapshot {
        backend: "demo",
        host: "jules-mbp".into(),
        default_out: Some(device(1, "MacBook Speakers", Direction::Output)),
        default_in: Some(device(2, "MacBook Mic", Direction::Input)),
        streams,
        xruns_60s: xruns,
        caps: Caps {
            device_levels: true,
            input_levels: true,
            stream_levels: true,
            per_app_streams: true,
            requested_format: true,
            xruns: true,
            hold_is_since_launch: false,
            note: None,
        },
        at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_are_deterministic_and_in_range() {
        let a = out_series(false);
        let b = out_series(false);
        assert_eq!(a, b);
        assert_eq!(a.len(), FIXTURE_COLS);
        assert!(a.iter().all(|v| (crate::meter::FLOOR_DBFS..=0.0).contains(v)));
    }

    #[test]
    fn the_alert_series_actually_clips() {
        let a = out_series(true);
        assert!(a.iter().any(|v| *v >= -0.1), "alert fixture never reaches clip");
    }

    #[test]
    fn fixtures_cover_both_conversion_kinds() {
        let s = snapshot(0);
        let rate_conv =
            s.streams.iter().any(|x| x.requested.is_some_and(|r| r.rate != x.format.rate));
        // Loss is requested-depth > actual-depth: 24-bit source, 16-bit sink.
        let bits_conv = s.streams.iter().any(|x| {
            x.requested.is_some_and(|r| r.rate == x.format.rate && r.bits > x.format.bits)
        });
        assert!(rate_conv, "no rate conversion fixture");
        assert!(bits_conv, "no bit-depth conversion fixture");
    }
}
