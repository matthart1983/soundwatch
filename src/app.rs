//! App state, sampling rings, and key handling.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::backend::AudioBackend;
use crate::grid::{ASCII, Glyphs, UNICODE};
use crate::meter::{self, History};
use crate::model::{Snapshot, Stream, Verdict, verdict};

/// Meter sampling rate. The full tool meters at 60 Hz; Lite's chart advances one
/// column every 769ms, so 20 Hz is four times the resolution the display can
/// show and keeps the process invisible in its own stream list.
pub const SAMPLE_HZ: u64 = 20;
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000 / SAMPLE_HZ);
/// Full state read. Devices and stream lists do not need 20 Hz.
pub const TICK_INTERVAL: Duration = Duration::from_secs(1);
/// Rows available to the stream table in the main state.
pub const LIST_ROWS: usize = 8;
/// Rows available to the stream table when the detail block is open.
pub const DETAIL_LIST_ROWS: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Main,
    /// `/` is open and the query is being edited.
    Filter,
    /// A stream is expanded in place.
    Detail,
    /// The `?` overlay. LITE.md lists the key and never specifies the screen.
    Help,
}

pub struct App {
    backend: Box<dyn AudioBackend>,
    pub snap: Snapshot,
    pub verdict: Verdict,
    pub mode: Mode,
    /// Where `?` should return to.
    return_to: Mode,
    pub paused: bool,
    /// Live query while [`Mode::Filter`] is open.
    pub query: String,
    /// Query still applied after leaving filter mode.
    pub applied: Option<String>,
    pub sel: usize,
    pub scroll: usize,
    pub out_hist: History,
    pub in_hist: History,
    pub stream_hist: HashMap<String, History>,
    pub glyphs: Glyphs,
    pub demo: bool,
    pub should_quit: bool,
    last_tick: Instant,
    last_commit: Instant,
    demo_xruns: u32,
}

impl App {
    pub fn new(mut backend: Box<dyn AudioBackend>, demo: bool, ascii: bool) -> Self {
        let snap = if demo { crate::demo::snapshot(0) } else { backend.snapshot() };
        let mut app = Self {
            backend,
            verdict: Verdict::Nominal,
            snap,
            mode: Mode::Main,
            return_to: Mode::Main,
            paused: false,
            query: String::new(),
            applied: None,
            sel: 0,
            scroll: 0,
            out_hist: History::new(),
            in_hist: History::new(),
            stream_hist: HashMap::new(),
            glyphs: if ascii { ASCII } else { UNICODE },
            demo,
            should_quit: false,
            last_tick: Instant::now(),
            last_commit: Instant::now(),
            demo_xruns: 0,
        };
        if demo {
            app.seed_demo();
        }
        app.recompute_verdict();
        app
    }

    fn seed_demo(&mut self) {
        let alert = self.demo_xruns > 0;
        self.out_hist = History::from_series(&crate::demo::out_series(alert));
        self.in_hist = History::from_series(&crate::demo::in_series());
        for (i, s) in self.snap.streams.iter().enumerate() {
            self.stream_hist
                .insert(s.key.clone(), History::from_series(&crate::demo::stream_series(i)));
        }
    }

    /// Columns in the output window that reached clipping. Drives the
    /// "sustained clipping" alert, as distinct from a single loud transient.
    pub fn clipped_columns(&self) -> usize {
        self.out_hist.series().iter().filter(|v| **v >= -0.1).count()
    }

    fn recompute_verdict(&mut self) {
        self.verdict = verdict(&self.snap, self.clipped_columns());
    }

    /// One meter sample. Cheap, and skipped entirely while paused.
    pub fn sample(&mut self) {
        if self.paused {
            return;
        }
        if !self.demo {
            let l = self.backend.levels();
            if let Some(d) = l.out_dbfs {
                self.out_hist.push_sample(d);
            }
            if let Some(d) = l.in_dbfs {
                self.in_hist.push_sample(d);
            }
            for s in &self.snap.streams {
                if let Some(d) = s.level_dbfs {
                    self.stream_hist.entry(s.key.clone()).or_default().push_sample(d);
                }
            }
        }
        if self.last_commit.elapsed().as_secs_f32() >= meter::BUCKET_SECS {
            self.last_commit = Instant::now();
            if !self.demo {
                self.out_hist.commit();
                self.in_hist.commit();
                for h in self.stream_hist.values_mut() {
                    h.commit();
                }
            }
        }
    }

    /// Full state read, at [`TICK_INTERVAL`].
    pub fn tick(&mut self) {
        if self.paused || self.last_tick.elapsed() < TICK_INTERVAL {
            return;
        }
        self.last_tick = Instant::now();
        if self.demo {
            self.snap = crate::demo::snapshot(self.demo_xruns);
        } else {
            self.snap = self.backend.snapshot();
            let live: Vec<&str> = self.snap.streams.iter().map(|s| s.key.as_str()).collect();
            self.stream_hist.retain(|k, _| live.contains(&k.as_str()));
        }
        self.clamp_selection();
        self.recompute_verdict();
    }

    /// Streams after the active filter, in display order.
    pub fn visible(&self) -> Vec<&Stream> {
        let q = match self.mode {
            Mode::Filter => Some(self.query.as_str()),
            _ => self.applied.as_deref(),
        }
        .filter(|q| !q.is_empty())
        .map(str::to_lowercase);

        self.snap
            .streams
            .iter()
            .filter(|s| match &q {
                None => true,
                Some(q) => s.app.to_lowercase().contains(q) || s.device.to_lowercase().contains(q),
            })
            .collect()
    }

    /// Rows the table can show in the current mode.
    pub fn list_rows(&self) -> usize {
        if self.mode == Mode::Detail { DETAIL_LIST_ROWS } else { LIST_ROWS }
    }

    fn clamp_selection(&mut self) {
        let n = self.visible().len();
        if n == 0 {
            self.sel = 0;
            self.scroll = 0;
            return;
        }
        self.sel = self.sel.min(n - 1);
        let rows = self.list_rows();
        // Keep the selection inside the window — this is what makes `↵ detail`
        // reachable for streams past the eighth, which the spec's five-key
        // scheme has no way to express.
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + rows {
            self.scroll = self.sel + 1 - rows;
        }
        let max_scroll = n.saturating_sub(rows);
        self.scroll = self.scroll.min(max_scroll);
    }

    fn move_sel(&mut self, delta: isize) {
        let n = self.visible().len();
        if n == 0 {
            return;
        }
        let cur = self.sel as isize;
        self.sel = (cur + delta).clamp(0, n as isize - 1) as usize;
        self.clamp_selection();
    }

    pub fn selected(&self) -> Option<Stream> {
        self.visible().get(self.sel).map(|s| (*s).clone())
    }

    pub fn history_for(&self, key: &str) -> Option<&History> {
        self.stream_hist.get(key)
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        // Filter mode swallows text input, so it is handled first.
        if self.mode == Mode::Filter {
            match k.code {
                KeyCode::Esc => {
                    self.query.clear();
                    self.applied = None;
                    self.mode = Mode::Main;
                }
                // Commit the query and return to a selectable list. This is what
                // resolves the spec's conflict between "no selection tint" in
                // filter mode and `↵ detail` on the same key.
                KeyCode::Enter => {
                    self.applied = (!self.query.is_empty()).then(|| self.query.clone());
                    self.mode = Mode::Main;
                    self.sel = 0;
                    self.scroll = 0;
                }
                KeyCode::Backspace => {
                    self.query.pop();
                }
                KeyCode::Char(c) => self.query.push(c),
                _ => {}
            }
            self.clamp_selection();
            return;
        }

        if self.mode == Mode::Help {
            // Any key dismisses the overlay and returns to the same screen.
            self.mode = self.return_to;
            return;
        }

        match k.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true
            }
            KeyCode::Char('p') => self.paused = !self.paused,
            KeyCode::Char('/') => {
                self.query = self.applied.clone().unwrap_or_default();
                self.mode = Mode::Filter;
            }
            KeyCode::Char('?') => {
                self.return_to = self.mode;
                self.mode = Mode::Help;
            }
            KeyCode::Enter => {
                self.mode = if self.mode == Mode::Detail { Mode::Main } else { Mode::Detail };
                self.clamp_selection();
            }
            KeyCode::Esc => {
                if self.mode == Mode::Detail {
                    self.mode = Mode::Main;
                } else if self.applied.is_some() {
                    self.applied = None;
                    self.query.clear();
                }
                self.clamp_selection();
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Home => {
                self.sel = 0;
                self.clamp_selection();
            }
            KeyCode::End => {
                self.sel = self.visible().len().saturating_sub(1);
                self.clamp_selection();
            }
            // Demo-only: exercise the alert state without waiting for a glitch.
            KeyCode::Char('x') if self.demo => {
                self.demo_xruns = if self.demo_xruns == 0 { 14 } else { 0 };
                self.snap = crate::demo::snapshot(self.demo_xruns);
                self.seed_demo();
                self.recompute_verdict();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{AudioBackend, Levels};
    use crate::model::Caps;

    struct DemoBackend;
    impl AudioBackend for DemoBackend {
        fn name(&self) -> &'static str {
            "demo"
        }
        fn caps(&self) -> Caps {
            Caps::default()
        }
        fn snapshot(&mut self) -> Snapshot {
            crate::demo::snapshot(0)
        }
        fn levels(&mut self) -> Levels {
            Levels::default()
        }
    }

    fn app() -> App {
        App::new(Box::new(DemoBackend), true, false)
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    /// The design fixture holds exactly eight streams, which is exactly the row
    /// budget — so the mockup never shows what happens on a real desktop, where
    /// Chrome alone can hold several. Overflow it deliberately.
    fn crowded_app() -> App {
        let mut a = app();
        let template = a.snap.streams[0].clone();
        for i in 0..6 {
            let mut s = template.clone();
            s.key = format!("extra:{i}");
            s.app = format!("extra{i}");
            a.snap.streams.push(s);
        }
        a
    }

    #[test]
    fn selection_can_reach_every_stream_past_the_window() {
        let mut a = crowded_app();
        let n = a.visible().len();
        assert!(n > LIST_ROWS, "fixture must overflow the list to test this");
        for _ in 0..n {
            a.on_key(key(KeyCode::Down));
        }
        assert_eq!(a.sel, n - 1, "could not reach the last stream");
        // The window must have scrolled to keep the selection visible — without
        // this, `↵ detail` can never open anything past the eighth row.
        assert!(a.sel >= a.scroll && a.sel < a.scroll + a.list_rows());
        assert!(a.selected().is_some());

        // And back up again.
        for _ in 0..n {
            a.on_key(key(KeyCode::Up));
        }
        assert_eq!(a.sel, 0);
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn filter_commits_on_enter_and_cancels_on_esc() {
        let mut a = app();
        let all = a.visible().len();

        a.on_key(key(KeyCode::Char('/')));
        assert_eq!(a.mode, Mode::Filter);
        for c in "zoom".chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
        let matched = a.visible().len();
        assert!(matched > 0 && matched < all, "filter matched {matched} of {all}");

        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Main);
        assert_eq!(a.applied.as_deref(), Some("zoom"));
        assert_eq!(a.visible().len(), matched, "committed filter must persist");

        a.on_key(key(KeyCode::Esc));
        assert_eq!(a.visible().len(), all, "esc must clear the applied filter");
    }

    #[test]
    fn filter_matches_device_as_well_as_app() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('/')));
        for c in "scarlett".chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
        assert!(a.visible().iter().all(|s| s.device.to_lowercase().contains("scarlett")));
        assert!(!a.visible().is_empty());
    }

    #[test]
    fn help_returns_to_the_screen_it_was_opened_from() {
        let mut a = app();
        a.on_key(key(KeyCode::Enter)); // into detail
        assert_eq!(a.mode, Mode::Detail);
        a.on_key(key(KeyCode::Char('?')));
        assert_eq!(a.mode, Mode::Help);
        a.on_key(key(KeyCode::Char('z'))); // any key dismisses
        assert_eq!(a.mode, Mode::Detail);
    }

    #[test]
    fn detail_narrows_the_list_and_keeps_the_selection_visible() {
        let mut a = app();
        for _ in 0..6 {
            a.on_key(key(KeyCode::Down));
        }
        a.on_key(key(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Detail);
        assert_eq!(a.list_rows(), DETAIL_LIST_ROWS);
        assert!(
            a.sel >= a.scroll && a.sel < a.scroll + DETAIL_LIST_ROWS,
            "selected stream scrolled out of the truncated list"
        );
        assert!(a.selected().is_some());
    }

    #[test]
    fn pause_freezes_sampling() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('p')));
        assert!(a.paused);
        let before = a.out_hist.series();
        a.sample();
        assert_eq!(a.out_hist.series(), before);
    }

    #[test]
    fn the_alert_state_is_reachable_and_names_a_cause() {
        let mut a = app();
        assert_eq!(a.verdict, Verdict::Nominal);
        a.on_key(key(KeyCode::Char('x')));
        match &a.verdict {
            Verdict::Alert { headline, reason } => {
                assert!(headline.contains("xrun"));
                assert!(reason.contains("buffer too small"));
            }
            v => panic!("expected an alert, got {v:?}"),
        }
    }

    #[test]
    fn empty_filter_results_do_not_panic() {
        let mut a = app();
        a.on_key(key(KeyCode::Char('/')));
        for c in "zzzznomatch".chars() {
            a.on_key(key(KeyCode::Char(c)));
        }
        assert!(a.visible().is_empty());
        assert_eq!(a.sel, 0);
        assert!(a.selected().is_none());
    }
}
