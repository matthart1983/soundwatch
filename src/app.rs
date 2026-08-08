//! App state, sampling rings, and key handling.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::backend::worker::{BackendWorker, Update};
use crate::config::{Config, Setting};
use crate::grid::{ASCII, Glyphs, UNICODE};
use crate::layout::{Chrome, Layout};
use crate::meter::{self, History};
use crate::model::{Snapshot, Stream, Verdict, verdict};
use crate::spectrum::{Source, Spectrum};
use crate::tabs::Tab;
use crate::theme::Theme;

/// Meter sampling rate. The full tool meters at 60 Hz; Lite's chart advances one
/// column every 769ms, so 20 Hz is four times the resolution the display can
/// show and keeps the process invisible in its own stream list.
pub const SAMPLE_HZ: u64 = 20;
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000 / SAMPLE_HZ);
/// Full state read. Devices and stream lists do not need 20 Hz.
pub const TICK_INTERVAL: Duration = Duration::from_secs(1);
/// How long the backend may stay silent before the screen stops saying
/// "reading" and starts saying "something is wrong".
pub const STALL_AFTER: Duration = Duration::from_secs(2);
// How many rows the stream table gets is no longer a constant: it depends on
// how tall the terminal is. See `crate::layout::Layout`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Main,
    /// `/` is open and the query is being edited.
    Filter,
    /// A stream is expanded in place.
    Detail,
    /// The `?` overlay. LITE.md lists the key and never specifies the screen.
    Help,
    /// The `,` settings menu.
    Settings,
}

pub struct App {
    /// The live backend, on its own thread. `None` in `--demo` and in `--once`,
    /// which are driven synchronously and never poll.
    ///
    /// Nothing on the UI thread ever calls CoreAudio: a single blocking
    /// property read is enough to freeze the whole program, and one of them
    /// blocks whenever a consent decision is pending. See
    /// [`crate::backend::worker`].
    worker: Option<BackendWorker>,
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
    /// RMS history, for the Meters tab's crest factor. Peak alone cannot say
    /// how loud something is, only whether it is about to clip.
    pub out_rms: History,
    pub in_rms: History,
    pub stream_hist: HashMap<String, History>,
    pub glyphs: Glyphs,
    /// Everything the settings menu can change.
    pub cfg: Config,
    /// Which row of the settings menu is selected.
    pub setting: usize,
    /// What the last save attempt said, shown in the menu's footer.
    pub save_status: Option<String>,
    /// Where everything goes at the current terminal size. Owned by the App
    /// rather than recomputed in the renderer because the selection has to be
    /// clamped against the list height, and that is not a rendering concern.
    pub layout: Layout,
    /// The spectrum screen. Off by default; `s` cycles it.
    pub spectrum: Spectrum,
    /// The active tab in the full product.
    pub tab: Tab,
    pub demo: bool,
    pub should_quit: bool,
    last_tick: Instant,
    last_commit: Instant,
    /// When the app started, and whether the backend has ever answered.
    started: Instant,
    heard_from_backend: bool,
    /// Whether audio reached the analyser since the last `sample()`.
    spectrum_fed: bool,
    demo_xruns: u32,
}

impl App {
    /// The fixtures, with no audio system behind them.
    pub fn demo(cfg: Config) -> Self {
        Self::new(crate::demo::snapshot(0), None, true, cfg)
    }

    /// A live backend, polled on its own thread. Starts on a placeholder
    /// snapshot and replaces it the moment the worker answers — which may be
    /// immediately, or may be after the user has dealt with a consent dialog.
    pub fn live(worker: BackendWorker, backend: &'static str, host: String, cfg: Config) -> Self {
        Self::new(Snapshot::starting(backend, host), Some(worker), false, cfg)
    }

    /// One synchronous snapshot and no worker, for `--once`.
    pub fn snapshot_only(snap: Snapshot, cfg: Config) -> Self {
        Self::new(snap, None, false, cfg)
    }

    /// Private, and takes the theme, so no entry point can forget to pass it.
    /// It was a settable field for one commit, and `--theme btop` silently did
    /// nothing in the live TUI because one of the three assignments did not
    /// survive a reformat. A flag that is accepted and ignored is worse than
    /// one that is rejected.
    fn new(snap: Snapshot, worker: Option<BackendWorker>, demo: bool, cfg: Config) -> Self {
        let mut app = Self {
            worker,
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
            out_rms: History::new(),
            in_rms: History::new(),
            stream_hist: HashMap::new(),
            glyphs: if cfg.ascii { ASCII } else { UNICODE },
            cfg,
            setting: 0,
            save_status: None,
            layout: if cfg.lite {
                Layout::lite(crate::grid::MIN_COLS, crate::grid::MIN_ROWS)
            } else {
                Layout::new(crate::grid::MIN_COLS, crate::grid::MIN_ROWS, Chrome::Tabs)
            },
            spectrum: Spectrum::default(),
            tab: Tab::default(),
            demo,
            should_quit: false,
            last_tick: Instant::now(),
            last_commit: Instant::now(),
            started: Instant::now(),
            heard_from_backend: false,
            spectrum_fed: false,
            demo_xruns: 0,
        };
        if demo {
            app.seed_demo();
        }
        // Without this the saved config reaches the menu but not the analyser:
        // fft_size, the floors and the ballistics only ever arrived through
        // `change_setting`, so a saved spectrum setup silently reverted on
        // every restart — which defeats the point of saving it.
        app.apply_config();
        app.recompute_verdict();
        app
    }

    /// Drive the spectrum from a synthetic signal, so `--demo` exercises the
    /// transform, the log axis, the ballistics *and* all three detectors on a
    /// machine with no audio consent — and so CI can render the screen.
    pub fn seed_demo_spectrum(&mut self) {
        let sig = crate::demo::spectrum_signal();
        let w = self.layout.content.width() * self.sub_columns() as u16;
        // Enough frames to satisfy the detectors' sustain rule.
        let frames = (crate::spectrum::SUSTAIN_SECS / SAMPLE_INTERVAL.as_secs_f32()) as usize + 2;
        for _ in 0..frames {
            self.spectrum.push(&sig, 48_000, w, SAMPLE_INTERVAL.as_secs_f32());
        }
        self.spectrum_fed = true;
    }

    fn seed_demo(&mut self) {
        let alert = self.demo_xruns > 0;
        self.out_hist = History::from_series(&crate::demo::out_series(alert));
        self.in_hist = History::from_series(&crate::demo::in_series());
        // RMS sits below peak by the crest factor; the fixtures model that.
        self.out_rms = History::from_series(
            &crate::demo::out_series(alert).iter().map(|v| v - 9.0).collect::<Vec<_>>(),
        );
        self.in_rms = History::from_series(
            &crate::demo::in_series().iter().map(|v| v - 12.0).collect::<Vec<_>>(),
        );
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
        self.verdict = verdict(&self.snap, self.clipped_columns(), self.cfg.xrun_alert);
    }

    /// Take everything the backend thread has produced since the last frame.
    /// Never blocks, however wedged the audio system is.
    pub fn poll_backend(&mut self) {
        let Some(worker) = &self.worker else { return };
        // Collected first so the borrow of `worker` ends before `self` is
        // mutated. The queue is bounded, so this is a small vector.
        let updates: Vec<Update> = worker.drain().collect();
        if updates.is_empty() {
            return;
        }
        // Paused means the screen is frozen, so the updates are read off the
        // queue (a full queue would stall the backend) and then discarded.
        if self.paused {
            return;
        }
        let mut snapped = false;
        for u in updates {
            match u {
                Update::Audio(a) => {
                    if self.spectrum.source == a.source {
                        let w = self.layout.content.width() * self.sub_columns() as u16;
                        self.spectrum.push_with_overruns(
                            &a.samples,
                            a.rate,
                            w,
                            SAMPLE_INTERVAL.as_secs_f32(),
                            a.overruns,
                        );
                        self.spectrum_fed = true;
                    }
                }
                Update::Levels(l) => {
                    if let Some(d) = l.out_dbfs {
                        self.out_hist.push_sample(d);
                    }
                    if let Some(d) = l.in_dbfs {
                        self.in_hist.push_sample(d);
                    }
                    if let Some(d) = l.out_rms_dbfs {
                        self.out_rms.push_sample(d);
                    }
                    if let Some(d) = l.in_rms_dbfs {
                        self.in_rms.push_sample(d);
                    }
                    for s in &self.snap.streams {
                        if let Some(d) = s.level_dbfs {
                            self.stream_hist.entry(s.key.clone()).or_default().push_sample(d);
                        }
                    }
                }
                Update::Snapshot(snap) => {
                    self.snap = *snap;
                    self.heard_from_backend = true;
                    snapped = true;
                }
            }
        }
        if snapped {
            let live: Vec<&str> = self.snap.streams.iter().map(|s| s.key.as_str()).collect();
            self.stream_hist.retain(|k, _| live.contains(&k.as_str()));
            self.clamp_selection();
        }
        self.recompute_verdict();
    }

    /// Close the current history bucket when one is due. Separate from the
    /// backend poll because the chart advances on wall-clock time whether or
    /// not the audio system has anything to say.
    pub fn sample(&mut self) {
        if self.paused {
            return;
        }
        // A source that stops delivering must fall away rather than freeze
        // mid-air. Only when nothing arrived this frame: `push` already
        // advanced the ballistics for the frames that did.
        if self.spectrum.source.is_on() && !self.spectrum_fed {
            self.spectrum.idle(SAMPLE_INTERVAL.as_secs_f32());
        }
        self.spectrum_fed = false;
        if self.last_commit.elapsed().as_secs_f32() >= meter::BUCKET_SECS {
            self.last_commit = Instant::now();
            if !self.demo {
                self.out_hist.commit();
                self.in_hist.commit();
                self.out_rms.commit();
                self.in_rms.commit();
                for h in self.stream_hist.values_mut() {
                    h.commit();
                }
            }
        }
    }

    /// Demo-only state advance, at [`TICK_INTERVAL`]. Live state arrives from
    /// the backend thread instead; see [`App::poll_backend`].
    pub fn tick(&mut self) {
        if self.paused || !self.demo || self.last_tick.elapsed() < TICK_INTERVAL {
            return;
        }
        self.last_tick = Instant::now();
        self.snap = crate::demo::snapshot(self.demo_xruns);
        self.clamp_selection();
        self.recompute_verdict();
    }

    /// What to say when the backend has not answered yet.
    ///
    /// A backend that is merely slow is invisible — the first snapshot lands in
    /// milliseconds. A backend that is *stuck* is almost always stuck behind a
    /// permission dialog, which the user may not have noticed and which nothing
    /// on this screen would otherwise mention. So the placeholder escalates
    /// from "reading" to something the user can act on.
    pub fn backend_stall(&self) -> Option<&'static str> {
        if self.worker.is_none() || self.heard_from_backend {
            return None;
        }
        if self.started.elapsed() >= STALL_AFTER {
            Some("the audio system has not answered \u{2014} look for a permission prompt")
        } else {
            Some("reading the audio system\u{2026}")
        }
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

    /// Sub-columns per cell in charts: two under a braille theme, one
    /// otherwise. `--ascii` forces one — the whole point of that flag is
    /// terminals whose glyph coverage cannot be trusted, and braille is a far
    /// riskier bet than the block elements.
    pub fn sub_columns(&self) -> usize {
        crate::chart::sub_columns(self.cfg.theme.braille() && !self.cfg.ascii)
    }

    /// Apply the current settings to everything that caches a copy of them.
    /// Cheap and idempotent, so it can run every frame rather than needing a
    /// change-detection path that could miss one.
    pub fn apply_config(&mut self) {
        self.glyphs = if self.cfg.ascii { ASCII } else { UNICODE };
        // The chrome changes the row numbers under everything.
        self.layout = Layout::new(self.layout.w, self.layout.h, self.chrome());
        self.clamp_selection();
        self.spectrum.configure(&self.cfg);
        if let Some(w) = &self.worker {
            w.want_input_meter(self.cfg.meter_input);
            w.want_window_len(self.spectrum.window_len());
        }
        self.recompute_verdict();
    }

    /// The theme in force. Reads through to the config so nothing can hold a
    /// stale copy.
    pub fn theme(&self) -> Theme {
        self.cfg.theme
    }

    /// Adopt a new terminal size. Called once per frame before drawing, so a
    /// resize takes effect on the same frame the user sees it.
    pub fn set_viewport(&mut self, w: u16, h: u16) {
        let next = Layout::new(w, h, self.chrome());
        if next != self.layout {
            self.layout = next;
            // A shorter list can strand the selection off the bottom.
            self.clamp_selection();
        }
    }

    /// Which chrome the screen wears, from the settings.
    pub fn chrome(&self) -> Chrome {
        if self.cfg.lite { Chrome::Lite } else { Chrome::Tabs }
    }

    /// Rows the table on the current screen can show.
    ///
    /// Not the Lite table's height on the tabbed screens: they draw into the
    /// body, which is four rows taller than the Lite list at 80x24 and far
    /// taller on a real terminal. Sizing the selection window by the wrong one
    /// scrolled rows off the top of a table with blank space below it.
    pub fn list_rows(&self) -> usize {
        if self.cfg.lite {
            self.layout.list_rows_for(self.mode == Mode::Detail) as usize
        } else {
            // Every tabbed table reserves its header and rule.
            self.layout.body_rows.saturating_sub(2) as usize
        }
    }

    /// How many rows the selection can move over on the current screen.
    ///
    /// The tabs put very different lists under one selection index — twelve
    /// devices, three insights, eight streams — so the bound has to come from
    /// whichever one is being drawn, or Up/Down does nothing on most of them.
    pub fn selectable(&self) -> usize {
        if self.cfg.lite {
            return self.visible().len();
        }
        match self.tab {
            Tab::Devices => self.snap.devices.len(),
            Tab::Xruns => self.snap.xrun_log.len(),
            Tab::Timeline => self.snap.events.len(),
            _ => self.visible().len(),
        }
    }

    fn clamp_selection(&mut self) {
        let n = self.selectable();
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
        let n = self.selectable();
        if n == 0 {
            return;
        }
        let cur = self.sel as isize;
        self.sel = (cur + delta).clamp(0, n as isize - 1) as usize;
        self.clamp_selection();
    }

    /// Switch tabs, resetting the per-tab selection.
    ///
    /// The selection index is shared, and the tabs it means different things on
    /// have wildly different lengths — carrying row 30 of the device list into
    /// a three-row insight view would land on nothing.
    pub fn select_tab(&mut self, t: Tab) {
        if t == self.tab {
            return;
        }
        self.tab = t;
        self.sel = 0;
        self.scroll = 0;
        // The spectrum tab is the spectrum: opening it starts the audio, and
        // leaving it stops shipping windows nobody is looking at.
        let want = if t == Tab::Spectrum {
            if self.spectrum.source.is_on() { self.spectrum.source } else { Source::Output }
        } else {
            Source::Off
        };
        if want != self.spectrum.source {
            self.spectrum.source = want;
            self.spectrum.reset();
        }
        if let Some(w) = &self.worker {
            w.want_audio(want);
        }
        self.clamp_selection();
    }

    /// Keys inside the settings menu.
    fn on_settings_key(&mut self, k: KeyEvent) {
        let rows = Setting::ALL.len();
        match k.code {
            KeyCode::Esc | KeyCode::Char(',') | KeyCode::Char('q') => {
                self.mode = self.return_to;
                self.save_status = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.setting = (self.setting + rows - 1) % rows;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.setting = (self.setting + 1) % rows;
            }
            KeyCode::Left | KeyCode::Char('h') => self.change_setting(-1),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter | KeyCode::Char(' ') => {
                self.change_setting(1)
            }
            // Written explicitly rather than on every keystroke: a menu that
            // rewrites a dotfile as you scroll through it is a menu you cannot
            // experiment in.
            KeyCode::Char('w') => {
                self.save_status = Some(match self.cfg.save() {
                    Ok(p) => format!("saved to {}", crate::config::short_path(&p, 48)),
                    Err(e) => format!("could not save: {e}"),
                });
            }
            KeyCode::Char('r') => {
                self.cfg = Config::default();
                self.apply_config();
                self.save_status = Some("reset to defaults (not saved)".into());
            }
            _ => {}
        }
    }

    fn change_setting(&mut self, delta: i32) {
        let Some(s) = Setting::ALL.get(self.setting).copied() else { return };
        s.adjust(&mut self.cfg, delta);
        self.apply_config();
        self.save_status = if s.needs_restart() {
            Some("takes effect now; consent may be asked for".into())
        } else {
            None
        };
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

        if self.mode == Mode::Settings {
            self.on_settings_key(k);
            return;
        }

        // Digits pick a tab in the full product. Checked before the rest so a
        // tab key never collides with a screen-specific one.
        if !self.cfg.lite
            && let KeyCode::Char(ch) = k.code
            && let Some(t) = Tab::from_digit(ch)
        {
            self.select_tab(t);
            return;
        }

        match k.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab | KeyCode::Right if !self.cfg.lite => self.select_tab(self.tab.next(1)),
            KeyCode::BackTab | KeyCode::Left if !self.cfg.lite => {
                self.select_tab(self.tab.next(-1))
            }
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true
            }
            KeyCode::Char('p') => self.paused = !self.paused,
            KeyCode::Char(',') => {
                self.return_to = self.mode;
                self.mode = Mode::Settings;
                self.save_status = None;
            }
            KeyCode::Char('s') => {
                self.spectrum.cycle_source();
                // Tell the backend to start (or stop) collecting audio. It is
                // only shipped across the channel while this screen is open.
                if let Some(w) = &self.worker {
                    w.want_audio(self.spectrum.source);
                }
                self.clamp_selection();
            }
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
                if self.spectrum.source.is_on() {
                    self.spectrum.source = Source::Off;
                    self.spectrum.reset();
                    if let Some(w) = &self.worker {
                        w.want_audio(Source::Off);
                    }
                } else if self.mode == Mode::Detail {
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
                self.sel = self.selectable().saturating_sub(1);
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

    /// The Lite chrome, because these tests pin the Lite spec's row budget.
    fn app() -> App {
        App::demo(Config { lite: true, ..Default::default() })
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
        assert!(n > a.list_rows(), "fixture must overflow the list to test this");
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
        assert_eq!(a.list_rows(), 5, "detail must displace three of the eight rows");
        assert!(
            a.sel >= a.scroll && a.sel < a.scroll + a.list_rows(),
            "selected stream scrolled out of the truncated list"
        );
        assert!(a.selected().is_some());
    }

    /// The placeholder must escalate: a backend that never answers is almost
    /// always one waiting on a permission dialog the user has not seen.
    #[test]
    fn a_silent_backend_eventually_says_something_actionable() {
        // Demo has no worker, so it never claims the backend is stalled.
        let a = app();
        assert!(a.backend_stall().is_none(), "the fixtures are never stalled");

        let mut live = App::live(
            crate::backend::worker::BackendWorker::spawn(Box::new(Wedged)),
            "wedged",
            "test".into(),
            Config::default(),
        );
        live.poll_backend();
        assert_eq!(live.backend_stall(), Some("reading the audio system\u{2026}"));

        live.started = Instant::now() - STALL_AFTER;
        live.poll_backend();
        let stalled = live.backend_stall().expect("still stalled");
        assert!(stalled.contains("permission prompt"), "unhelpful stall message: {stalled:?}");
    }

    /// And it stops saying it the moment the backend does answer.
    #[test]
    fn a_backend_that_answers_clears_the_stall() {
        let mut live = App::live(
            crate::backend::worker::BackendWorker::spawn(Box::new(Prompt)),
            "stub",
            "test".into(),
            Config::default(),
        );
        for _ in 0..500 {
            live.poll_backend();
            if live.backend_stall().is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the stall notice never cleared");
    }

    struct Wedged;
    impl crate::backend::AudioBackend for Wedged {
        fn name(&self) -> &'static str {
            "wedged"
        }
        fn caps(&self) -> crate::model::Caps {
            crate::model::Caps::default()
        }
        fn snapshot(&mut self) -> Snapshot {
            std::thread::sleep(Duration::from_secs(3600));
            unreachable!()
        }
        fn levels(&mut self) -> crate::backend::Levels {
            crate::backend::Levels::default()
        }
    }

    struct Prompt;
    impl crate::backend::AudioBackend for Prompt {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn caps(&self) -> crate::model::Caps {
            crate::model::Caps::default()
        }
        fn snapshot(&mut self) -> Snapshot {
            crate::demo::snapshot(0)
        }
        fn levels(&mut self) -> crate::backend::Levels {
            crate::backend::Levels::default()
        }
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
