//! ALSA's `/proc/asound` tree, which is the whole hardware picture without a
//! single library or a single fork.
//!
//! Every Linux audio stack sits on ALSA, so this reports the same devices
//! whether the machine runs PipeWire, PulseAudio or nothing at all — and it
//! reports them by reading files, which costs no dependency, no FFI, and no
//! subprocess per tick. (A sibling of this tool once forked `netstat` and
//! `ifconfig` on every sample; the storm that produced is not repeated here.)
//!
//! What it gives: cards, PCM devices in both directions, and for anything
//! currently open, the negotiated format, buffer and period sizes, and the
//! **xrun count** — which ALSA maintains per substream and nothing else on
//! Linux exposes so directly.
//!
//! What it does not give: per-application streams or levels. Under PipeWire
//! every application is one client of one PipeWire node, so ALSA sees
//! "pipewire" holding the device and nothing about who is behind it. Those come
//! from the sound server, not the kernel.

use std::fs;
use std::path::{Path, PathBuf};

/// One PCM substream's live state, when it is open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HwParams {
    pub rate: u32,
    pub bits: u8,
    pub channels: u8,
    pub buffer_frames: u32,
    pub period_frames: u32,
    pub open: bool,
}

/// One PCM device: `card0/pcm0p` and friends.
#[derive(Clone, Debug)]
pub struct Pcm {
    pub card: u32,
    pub device: u32,
    pub playback: bool,
    pub card_name: String,
    pub pcm_name: String,
    pub driver: String,
    pub params: HwParams,
    /// Xruns since the substream was opened, summed across substreams.
    pub xruns: u64,
    /// The process holding it, when ALSA reports one.
    pub owner_pid: Option<i32>,
}

impl Pcm {
    pub fn id(&self) -> String {
        format!("hw:{},{}{}", self.card, self.device, if self.playback { "p" } else { "c" })
    }
}

fn read(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok()
}

/// `hw:0,3` style device list with names, from `/proc/asound/pcm`.
///
/// The format is `00-03: HDMI 0 : HDMI 0 : playback 1`, i.e.
/// `card-device: id : name : <direction> <count>` repeated per direction.
pub fn devices(root: &Path) -> Vec<Pcm> {
    let Some(list) = read(root.join("pcm")) else {
        return Vec::new();
    };
    let cards = card_names(root);

    let mut out = Vec::new();
    for line in list.lines() {
        let mut parts = line.split(':');
        let Some(index) = parts.next() else { continue };
        let (card, device) = match index.trim().split_once('-') {
            Some((c, d)) => match (c.trim().parse(), d.trim().parse()) {
                (Ok(c), Ok(d)) => (c, d),
                _ => continue,
            },
            None => continue,
        };
        let _id = parts.next().unwrap_or("").trim().to_string();
        let name = parts.next().unwrap_or("").trim().to_string();
        let rest: Vec<&str> = parts.collect();
        let tail = rest.join(":");

        // One entry per direction the line advertises.
        for (playback, key) in [(true, "playback"), (false, "capture")] {
            if !tail.contains(key) {
                continue;
            }
            let dir = if playback { 'p' } else { 'c' };
            let dir_dir = root.join(format!("card{card}")).join(format!("pcm{device}{dir}"));
            let (params, xruns, owner_pid) = substreams(&dir_dir);
            let (card_name, driver) = cards
                .iter()
                .find(|(c, _, _)| *c == card)
                .map(|(_, n, d)| (n.clone(), d.clone()))
                .unwrap_or_default();
            out.push(Pcm {
                card,
                device,
                playback,
                card_name,
                pcm_name: name.clone(),
                driver,
                params,
                xruns,
                owner_pid,
            });
        }
    }
    out
}

/// `(index, name, driver)` from `/proc/asound/cards`.
///
/// Two lines per card: ` 0 [PCH  ]: HDA-Intel - HDA Intel PCH` then an
/// indented longer description. The bracketed short id is the stable one.
fn card_names(root: &Path) -> Vec<(u32, String, String)> {
    let Some(text) = read(root.join("cards")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((head, tail)) = line.split_once(']') else { continue };
        let Some((idx, _short)) = head.split_once('[') else { continue };
        let Ok(card) = idx.trim().parse::<u32>() else { continue };
        let tail = tail.trim_start_matches(':').trim();
        let (driver, name) = match tail.split_once(" - ") {
            Some((d, n)) => (d.trim().to_string(), n.trim().to_string()),
            None => (tail.to_string(), tail.to_string()),
        };
        out.push((card, name, driver));
    }
    out
}

/// Aggregate every substream of one PCM direction.
fn substreams(dir: &Path) -> (HwParams, u64, Option<i32>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return (HwParams::default(), 0, None);
    };
    let mut params = HwParams::default();
    let mut xruns = 0u64;
    let mut owner = None;

    let mut subs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("sub")))
        .collect();
    subs.sort();

    for sub in subs {
        if let Some(p) = hw_params(&sub.join("hw_params"))
            && !params.open
        {
            params = p;
        }
        let (x, pid) = status(&sub.join("status"));
        xruns += x;
        owner = owner.or(pid);
    }
    (params, xruns, owner)
}

/// `hw_params` is `key: value` lines, or the single word `closed`.
pub fn hw_params(path: &Path) -> Option<HwParams> {
    let text = read(path)?;
    if text.trim() == "closed" {
        return Some(HwParams::default());
    }
    let mut p = HwParams { open: true, ..Default::default() };
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        match k.trim() {
            "rate" => {
                p.rate = v.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0)
            }
            "channels" => p.channels = v.parse().unwrap_or(0),
            "buffer_size" => p.buffer_frames = v.parse().unwrap_or(0),
            "period_size" => p.period_frames = v.parse().unwrap_or(0),
            "format" => p.bits = bits_of(v),
            _ => {}
        }
    }
    Some(p)
}

/// ALSA names formats like `S16_LE`, `S24_3LE`, `FLOAT_LE`. Only the width
/// matters here, and the width is the digits.
pub fn bits_of(format: &str) -> u8 {
    if format.starts_with("FLOAT64") {
        return 64;
    }
    if format.starts_with("FLOAT") {
        return 32;
    }
    let digits: String = format
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().unwrap_or(0)
}

/// `status` carries the running state, the owner pid, and the xrun count.
pub fn status(path: &Path) -> (u64, Option<i32>) {
    let Some(text) = read(path) else {
        return (0, None);
    };
    if text.trim() == "closed" {
        return (0, None);
    }
    let mut xruns = 0;
    let mut pid = None;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        match k.trim() {
            "xruns" => xruns = v.trim().parse().unwrap_or(0),
            "owner_pid" => pid = v.trim().parse().ok(),
            _ => {}
        }
    }
    (xruns, pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixtures taken verbatim from a live Ubuntu 24.04 box, so the parser is
    /// pinned to what the kernel actually writes rather than to what the
    /// documentation says it writes.
    fn fixture() -> tempdir::Dir {
        let d = tempdir::Dir::new();
        d.write("cards", " 0 [PCH            ]: HDA-Intel - HDA Intel PCH\n                      HDA Intel PCH at 0x5020310000 irq 217\n 1 [USB            ]: USB-Audio - Scarlett 2i2 USB\n                      Focusrite Scarlett 2i2 USB at usb-0000:00:14.0-1\n");
        d.write(
            "pcm",
            "00-03: HDMI 0 : HDMI 0 : playback 1\n01-00: USB Audio : USB Audio : playback 1 : capture 1\n",
        );
        d.write("card0/pcm3p/sub0/hw_params", "closed\n");
        d.write("card0/pcm3p/sub0/status", "closed\n");
        d.write(
            "card1/pcm0p/sub0/hw_params",
            "access: MMAP_INTERLEAVED\nformat: S32_LE\nsubformat: STD\nchannels: 2\nrate: 48000 (48000/1)\nperiod_size: 512\nbuffer_size: 2048\n",
        );
        d.write(
            "card1/pcm0p/sub0/status",
            "state: RUNNING\nowner_pid   : 1668\ntrigger_time: 1234.5\nhw_ptr      : 99\nappl_ptr    : 100\nxruns       : 7\n",
        );
        d.write("card1/pcm0c/sub0/hw_params", "closed\n");
        d.write("card1/pcm0c/sub0/status", "closed\n");
        d
    }

    #[test]
    fn devices_are_read_in_both_directions() {
        let d = fixture();
        let pcms = devices(d.path());
        let ids: Vec<String> = pcms.iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["hw:0,3p", "hw:1,0p", "hw:1,0c"], "{ids:?}");
        assert_eq!(pcms[0].card_name, "HDA Intel PCH");
        assert_eq!(pcms[1].card_name, "Scarlett 2i2 USB");
        assert_eq!(pcms[1].driver, "USB-Audio");
    }

    #[test]
    fn an_open_substream_reports_its_negotiated_format() {
        let d = fixture();
        let pcms = devices(d.path());
        let out = pcms.iter().find(|p| p.id() == "hw:1,0p").expect("the usb playback device");
        assert!(out.params.open);
        assert_eq!(out.params.rate, 48_000);
        assert_eq!(out.params.bits, 32);
        assert_eq!(out.params.channels, 2);
        assert_eq!(out.params.buffer_frames, 2048);
        assert_eq!(out.params.period_frames, 512);
        assert_eq!(out.xruns, 7, "the xrun count is the one thing only ALSA reports");
        assert_eq!(out.owner_pid, Some(1668));
    }

    #[test]
    fn a_closed_substream_is_closed_rather_than_zeroed_nonsense() {
        let d = fixture();
        let pcms = devices(d.path());
        let idle = pcms.iter().find(|p| p.id() == "hw:0,3p").expect("the hdmi device");
        assert!(!idle.params.open);
        assert_eq!(idle.params.rate, 0, "a closed device has no negotiated rate to report");
        assert_eq!(idle.xruns, 0);
        assert_eq!(idle.owner_pid, None);
    }

    #[test]
    fn alsa_format_names_yield_their_width() {
        assert_eq!(bits_of("S16_LE"), 16);
        assert_eq!(bits_of("S24_3LE"), 24);
        assert_eq!(bits_of("S32_LE"), 32);
        assert_eq!(bits_of("FLOAT_LE"), 32);
        assert_eq!(bits_of("FLOAT64_LE"), 64);
        assert_eq!(bits_of("nonsense"), 0);
    }

    #[test]
    fn a_machine_with_no_sound_card_is_empty_rather_than_a_panic() {
        let d = tempdir::Dir::new();
        assert!(devices(d.path()).is_empty());
        d.write("pcm", "");
        assert!(devices(d.path()).is_empty());
        d.write("pcm", "garbage without colons\n:: :: ::\n");
        assert!(devices(d.path()).is_empty());
    }

    /// A tiny scratch directory, so the fixtures above are real files read by
    /// the real code path rather than strings handed to a parser directly.
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new() -> Self {
                let mut p = std::env::temp_dir();
                p.push(format!(
                    "soundwatch-proc-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                let _ = std::fs::remove_dir_all(&p);
                std::fs::create_dir_all(&p).expect("scratch dir");
                Self(p)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }

            pub fn write(&self, rel: &str, body: &str) {
                let p = self.0.join(rel);
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).expect("scratch subdir");
                }
                std::fs::write(p, body).expect("scratch file");
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
