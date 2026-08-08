//! [10] Insights — what is wrong, in words, with the tab that shows it.
//!
//! The family's last tab, and the one that justifies the other nine: everything
//! else asks you to read a number and know what it means. This reads them for
//! you. Each card is a plain sentence, a severity, and the tab to open next.
//!
//! Read-only and non-destructive, per the family brief: an insight names a
//! problem and points at evidence. It never changes anything.

use crate::app::App;
use crate::grid::{Canvas, Field};
use crate::layout::Layout;
use crate::model::Transport;
use crate::theme;

use super::Tab;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Something is wrong with the audio right now.
    Fault,
    /// Working, but degraded or about to bite.
    Warn,
    /// Worth knowing.
    Note,
}

impl Severity {
    pub fn badge(self) -> &'static str {
        match self {
            Severity::Fault => "FAULT",
            Severity::Warn => " WARN",
            Severity::Note => " NOTE",
        }
    }

    pub fn colour(self) -> ratatui::style::Color {
        match self {
            Severity::Fault => theme::RED,
            Severity::Warn => theme::YELLOW,
            Severity::Note => theme::CYAN,
        }
    }
}

pub struct Insight {
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub goto: Tab,
}

/// Everything the tool currently believes is worth saying, worst first.
///
/// Each rule reads state that some other tab already displays, so an insight is
/// always checkable against the screen it points at. Nothing here invents a
/// measurement; it only joins ones that already exist.
pub fn collect(app: &App) -> Vec<Insight> {
    let mut out = Vec::new();
    let snap = &app.snap;

    // ── dropouts ────────────────────────────────────────────────────────────
    if snap.caps.xruns && snap.xruns_60s >= app.cfg.xrun_alert {
        let where_ = snap
            .xrun_log
            .last()
            .map(|e| e.device.clone())
            .unwrap_or_else(|| "an audio device".into());
        let tight = snap.default_out.as_ref().is_some_and(|d| {
            d.buffer_frames > 0 && d.buffer_frames <= super::latency::TIGHT_BUFFER_FRAMES
        });
        out.push(Insight {
            severity: Severity::Fault,
            title: format!("{} dropouts in the last minute", snap.xruns_60s),
            body: if tight {
                format!("{where_} is running a small buffer. Raise it in Latency and the dropouts should stop.")
            } else {
                format!("{where_} is missing its deadline. Something else on the machine is taking the CPU.")
            },
            goto: Tab::Xruns,
        });
    }

    // ── the spectrum's own reads ────────────────────────────────────────────
    if let Some(v) = app.spectrum.verdict() {
        // The spectrum states its own findings; lift them rather than
        // re-deriving them, so the two screens can never disagree.
        let (title, body) = match v.split_once(" \u{b7} ") {
            Some((t, b)) => (t.to_string(), b.to_string()),
            None => (v.clone(), String::new()),
        };
        out.push(Insight { severity: Severity::Warn, title, body, goto: Tab::Spectrum });
    }

    // ── a lossy path on the default device ──────────────────────────────────
    for (name, transport) in super::devices::lossy(app) {
        let what = match transport {
            Transport::Bluetooth => {
                "Bluetooth resamples and compresses. If it also has a microphone open, macOS drops the output to headset quality."
            }
            Transport::AirPlay => "AirPlay compresses and adds seconds of buffering.",
            _ => "This transport does not carry the signal unchanged.",
        };
        out.push(Insight {
            severity: Severity::Note,
            title: format!("{name} is on {}", transport.name()),
            body: what.into(),
            goto: Tab::Devices,
        });
    }

    // ── a device that woke up and took the default ──────────────────────────
    if let Some(e) =
        snap.events.iter().rev().find(|e| e.kind == crate::model::EventKind::DefaultChanged)
        && snap.at.secs_since(e.at) < 120
    {
        out.push(Insight {
            severity: Severity::Note,
            title: "the default device changed recently".into(),
            body: format!("{} \u{2014} if the sound moved, this is why.", e.what),
            goto: Tab::Timeline,
        });
    }

    // ── a format that shifted under a running stream ────────────────────────
    if let Some(e) =
        snap.events.iter().rev().find(|e| e.kind == crate::model::EventKind::FormatChanged)
        && snap.at.secs_since(e.at) < 300
    {
        out.push(Insight {
            severity: Severity::Warn,
            title: "a device changed format mid-session".into(),
            body: format!("{}. A rate change under a running stream is heard as a glitch.", e.what),
            goto: Tab::Timeline,
        });
    }

    // ── latency worth mentioning ────────────────────────────────────────────
    if let Some(ms) = snap.latency_ms()
        && ms > 40.0
        && !snap.latency_is_one_way()
    {
        out.push(Insight {
            severity: Severity::Warn,
            title: format!("{ms:.0}ms round trip"),
            body: "Over about 40ms is audible when monitoring yourself. Latency shows which term dominates.".into(),
            goto: Tab::Latency,
        });
    }

    // ── nothing is metered ──────────────────────────────────────────────────
    if !snap.caps.device_levels && !app.demo {
        out.push(Insight {
            severity: Severity::Warn,
            title: "output is not being metered".into(),
            body: snap.caps.note.clone().unwrap_or_else(|| "the tap did not start.".into()),
            goto: Tab::Meters,
        });
    }

    out.sort_by_key(|i| i.severity);
    out
}

pub fn count(app: &App) -> usize {
    collect(app).len()
}

/// Break a sentence across `max` columns at word boundaries.
///
/// The bodies here are sentences, not labels, and the grid contract truncates
/// from the right — which on a sentence means cutting it off mid-word and
/// losing the half that told you what to do.
fn wrap(text: &str, max: u16, lines: usize) -> Vec<String> {
    let max = max.max(8) as usize;
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur = word.to_string();
        } else if cur.chars().count() + 1 + word.chars().count() <= max {
            cur.push(' ');
            cur.push_str(word);
        } else {
            out.push(std::mem::take(&mut cur));
            cur = word.to_string();
            if out.len() == lines {
                break;
            }
        }
    }
    if out.len() < lines && !cur.is_empty() {
        out.push(cur);
    }
    // An elision marker on the last line, so a cut sentence says it was cut.
    if out.len() == lines {
        let joined: usize = out.iter().map(|l| l.chars().count() + 1).sum();
        if joined < text.chars().count()
            && let Some(last) = out.last_mut()
        {
            if last.chars().count() + 1 > max {
                last.truncate(last.char_indices().nth(max - 1).map(|(i, _)| i).unwrap_or(0));
            }
            last.push('\u{2026}');
        }
    }
    out
}

pub fn draw(c: &mut Canvas, l: &Layout, app: &App) {
    let f = l.content;
    let mut y = l.body_top;
    let bottom = l.body_top + l.body_rows;

    let items = collect(app);
    if items.is_empty() {
        super::section(c, f, y, "insights");
        c.left(f, y + 2, "nothing to report \u{2014} the audio path looks healthy", theme::GREEN);
        return;
    }

    let body_field = Field::new(f.x0 + 2, f.x1);
    for i in items {
        let body = wrap(&i.body, body_field.width(), 2);
        let card = 2 + body.len() as u16;
        if y + card > bottom {
            break;
        }
        // Severity stripe down the left edge, as the family's Insights does.
        for dy in 0..card {
            c.set(f.x0, y + dy, c.g.blocks[7], i.severity.colour());
        }
        let x = c.text(f, f.x0 + 2, y, i.severity.badge(), i.severity.colour(), true);
        c.text(f, x + 2, y, &i.title, theme::FG, true);
        for (n, line) in body.iter().enumerate() {
            c.left(body_field, y + 1 + n as u16, line, theme::DIM);
        }
        c.text(
            body_field,
            f.x0 + 2,
            y + card - 1,
            &format!("[{}] {}", i.goto.digit(), i.goto.name()),
            theme::CYAN,
            false,
        );
        y += card + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_breaks_at_words_and_never_exceeds_the_width() {
        let text = "Bluetooth resamples and compresses the signal on the way out";
        for max in [12u16, 20, 40, 78] {
            let lines = wrap(text, max, 2);
            assert!(!lines.is_empty());
            for l in &lines {
                assert!(
                    l.chars().count() <= max as usize,
                    "width {max}: {l:?} is {} chars",
                    l.chars().count()
                );
            }
        }
    }

    #[test]
    fn a_sentence_that_does_not_fit_says_it_was_cut() {
        let long = "one two three four five six seven eight nine ten eleven twelve";
        let lines = wrap(long, 12, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('\u{2026}'), "a cut sentence must show it: {:?}", lines[1]);
    }

    #[test]
    fn a_short_sentence_is_left_alone() {
        assert_eq!(wrap("all fine", 40, 2), vec!["all fine".to_string()]);
        assert!(wrap("", 40, 2).is_empty());
    }
}
