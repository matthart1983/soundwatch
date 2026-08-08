//! SoundWatch Lite — a read-only audio diagnostics TUI.
//!
//! One screen, five keys, four colours. Read-only: no volume, mute, routing or
//! default changes, and nothing is ever written back to the audio system.
//!
//! Output metering uses a Core Audio process tap (macOS 14.2+), which observes
//! without altering what anyone hears — see [`backend::tap`] for why that
//! departs from the handoff's "never meter by tapping" rule. Input metering is
//! a real capture stream and is therefore opt-in; see [`backend::input`].
//!
//! Neither can read anything without audio-capture consent, and consent is not
//! something a command-line tool gets for free. [`tcc`] is the half of the
//! program that makes macOS willing to ask the question at all — without it the
//! tap runs perfectly and delivers silence forever.

mod app;
mod backend;
mod demo;
mod dsp;
mod fmt;
mod grid;
mod layout;
mod meter;
mod model;
mod spectrum;
mod tcc;
mod theme;
mod ui;

use std::io::{self, Write};
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{ExecutableCommand, cursor};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use app::App;
use backend::AudioBackend;
use backend::coreaudio::CoreAudio;

const USAGE: &str = "\
soundwatch-lite — read-only audio diagnostics, one screen

USAGE:
    soundwatch-lite [OPTIONS]

OPTIONS:
    --demo            drive the UI from the design fixtures instead of the live
                      backend. Touches no audio device at all — useful for
                      design review, screenshots and machines without consent.
    --meter-input     also meter the default input device. Off by default: it
                      opens a capture stream, so this tool then shows up in the
                      mic-in-use audit it is here to report.
    --ascii           use ASCII stand-ins for the glyphs that are not reliably
                      single-width in every terminal
    --once <STATE>    render one frame to stdout and exit. STATE is one of
                      main, paused, filter, detail, alert, help, spectrum
    --no-color        with --once, emit plain text
    --probe-tap       start the output tap, watch it for five seconds, and
                      report whether real samples arrive. Use this first when
                      the meters look flat.
    -h, --help        this message
    -V, --version     print version

KEYS:
    q quit    p pause    s spectrum    / filter    enter detail    ? help
    up/down (or j/k) move the selection    esc close detail or clear filter
";

struct Args {
    demo: bool,
    ascii: bool,
    once: Option<String>,
    color: bool,
    probe: bool,
    meter_input: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        demo: false,
        ascii: false,
        once: None,
        color: true,
        probe: false,
        meter_input: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--probe-tap" => a.probe = true,
            "--meter-input" => a.meter_input = true,
            "--demo" => a.demo = true,
            "--ascii" => a.ascii = true,
            "--no-color" => a.color = false,
            "--once" => {
                a.once = Some(it.next().ok_or("--once needs a state")?);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("{}", ui::VERSION);
                std::process::exit(0);
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(a)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("soundwatch-lite: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    // Metering needs consent, and consent needs us to answer for ourselves
    // rather than for whichever terminal launched us. On success this call
    // replaces the process image and never returns; on failure we carry on and
    // the backend reports metering as unavailable, with the reason on screen.
    //
    // --demo and --once never meter, so they never ask.
    let wants_metering = !args.demo && args.once.is_none();
    let mut identity_error = None;
    if wants_metering && !tcc::is_own_subject() {
        // Irrefutable: the success arm of adopt_own_identity() never returns.
        let Err(e) = tcc::adopt_own_identity();
        identity_error = Some(e);
    }

    if args.probe {
        probe_tap(identity_error.as_deref());
        return;
    }

    // --demo drives the fixtures and must not open, tap or even enumerate a
    // real device: it is the state you fall back to when consent is refused.
    if args.demo {
        let mut app = App::demo(args.ascii);
        if let Some(state) = args.once {
            return once(&mut app, &state, args.color);
        }
        if let Err(e) = run(&mut app) {
            eprintln!("soundwatch-lite: {e}");
            std::process::exit(1);
        }
        return;
    }

    let metering =
        backend::Metering { output: wants_metering, input: wants_metering && args.meter_input };

    // --once is a one-shot render, so it reads the backend synchronously: there
    // is no UI to keep responsive and no second frame to wait for.
    if let Some(state) = args.once {
        let mut be = CoreAudio::new(metering);
        let mut app = App::snapshot_only(be.snapshot(), args.ascii);
        return once(&mut app, &state, args.color);
    }

    // Live: the backend goes on its own thread and the UI never touches
    // CoreAudio. See backend::worker for what happens when it does.
    let be = CoreAudio::new(metering);
    let name = be.name();
    let host = backend::ffi::hostname();
    let mut app =
        App::live(backend::worker::BackendWorker::spawn(Box::new(be)), name, host, args.ascii);

    if let Err(e) = run(&mut app) {
        eprintln!("soundwatch-lite: {e}");
        std::process::exit(1);
    }
}

/// Start the output tap, watch it for five seconds, and report what arrived.
///
/// The failure this exists for is the quiet one: the tap is created, the device
/// starts, the IOProc fires at exactly the right rate, and every sample is
/// zero. Nothing in the CoreAudio return codes distinguishes that from a silent
/// machine, so the probe separates "no callbacks", "callbacks but digital
/// silence" and "working" by inspection.
fn once(app: &mut App, state: &str, color: bool) {
    match render_once(app, state, color) {
        Ok(s) => print!("{s}"),
        Err(e) => {
            eprintln!("soundwatch-lite: {e}");
            std::process::exit(2);
        }
    }
}

fn probe_tap(identity_error: Option<&str>) {
    use backend::tap::LevelTap;

    println!("soundwatch-lite tap probe\n");
    println!("signed as     : {}", tcc::signing_identifier().unwrap_or_else(|| "unsigned".into()));
    match identity_error {
        None if tcc::is_own_subject() => {
            println!("tcc subject   : this binary ({})", tcc::BUNDLE_ID)
        }
        None => println!("tcc subject   : this binary"),
        Some(e) => println!("tcc subject   : the parent terminal — {e}"),
    }
    if let Some(advice) = tcc::signing_advice() {
        println!("\n{advice}\n");
    }

    let t = match LevelTap::start() {
        Err(e) => {
            println!("tap           : did not start — {e}");
            return;
        }
        Ok(t) => t,
    };
    println!("tap           : started\n");
    println!("watching for 5s — play some audio now");

    let mut best = f32::NEG_INFINITY;
    for i in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(d) = t.peak_dbfs()
            && d > best
        {
            best = d;
        }
        if i % 10 == 9 {
            let (calls, frames) = t.stats();
            println!(
                "  {}s  io_proc calls={calls}  frames={frames}  peak={best:.1} dBFS",
                (i + 1) / 10
            );
        }
    }

    let (calls, frames) = t.stats();
    println!("\nio_proc calls : {calls}");
    println!("frames seen   : {frames}");
    println!("peak          : {best:.1} dBFS\n");

    if calls == 0 {
        println!("verdict: the IOProc never fired — the aggregate device is not running.");
    } else if best <= crate::meter::FLOOR_DBFS {
        println!(
            "verdict: the IOProc is running but every sample is zero.\n\
             \n\
             Either nothing was playing, or audio capture has not been allowed.\n\
             A tap without consent is not an error — it is silence, forever.\n\
             \n\
             Look in System Settings \u{203a} Privacy & Security \u{203a} Screen & System\n\
             Audio Recording for \"soundwatch-lite\" and switch it on. If it is not\n\
             listed, no consent dialog was ever shown: run this probe again from a\n\
             normal login session, and answer the prompt."
        );
    } else {
        println!("verdict: metering works.");
    }
}

fn run(app: &mut App) -> io::Result<()> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?.execute(cursor::Hide)?;

    // Leave the terminal usable even if something panics mid-frame.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        hook(info);
    }));

    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;

    let result = loop {
        // Adopt the terminal's size before drawing, so a resize takes effect on
        // the frame the user sees it and the selection is clamped against the
        // same list height that is about to be drawn.
        match term.size() {
            Ok(s) => app.set_viewport(s.width, s.height),
            Err(e) => break Err(e),
        }
        if let Err(e) = term.draw(|f| {
            let area = f.area();
            ui::render(app, f.buffer_mut(), area);
        }) {
            break Err(e);
        }
        // Poll for the sampling interval, so input stays responsive without
        // spinning. Repaint therefore tops out at 20 fps, comfortably inside the
        // family's 30 fps coalescing budget.
        match event::poll(app::SAMPLE_INTERVAL) {
            Ok(true) => match event::read() {
                Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => app.on_key(k),
                Ok(_) => {}
                Err(e) => break Err(e),
            },
            Ok(false) => {}
            Err(e) => break Err(e),
        }
        app.poll_backend();
        app.sample();
        app.tick();
        if app.should_quit {
            break Ok(());
        }
    };

    restore()?;
    result
}

fn restore() -> io::Result<()> {
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?.execute(cursor::Show)?;
    io::stdout().flush()
}

/// Render a single frame in a named state and return it as text. This is how the
/// layout is checked against the spec's row and column numbers without a
/// terminal, and it is what the grid tests assert on.
fn render_once(app: &mut App, state: &str, color: bool) -> Result<String, String> {
    render_once_at(app, state, color, grid::MIN_COLS, grid::MIN_ROWS)
}

/// The same, at an explicit size. The tests drive this at several sizes; the
/// layout being right at 80x24 says nothing about it being right at 200x60.
fn render_once_at(
    app: &mut App,
    state: &str,
    color: bool,
    w: u16,
    h: u16,
) -> Result<String, String> {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let press = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);

    match state {
        "main" => {}
        "paused" => app.on_key(press(KeyCode::Char('p'))),
        "detail" => app.on_key(press(KeyCode::Enter)),
        "help" => app.on_key(press(KeyCode::Char('?'))),
        "spectrum" => {
            app.on_key(press(KeyCode::Char('s')));
            if app.demo {
                app.seed_demo_spectrum();
            }
        }
        "filter" => {
            app.on_key(press(KeyCode::Char('/')));
            for ch in "zoom".chars() {
                app.on_key(press(KeyCode::Char(ch)));
            }
        }
        "alert" => {
            if !app.demo {
                return Err("--once alert requires --demo (a real xrun cannot be summoned)".into());
            }
            app.on_key(press(KeyCode::Char('x')));
        }
        other => return Err(format!("unknown state: {other}")),
    }

    let area = Rect::new(0, 0, w, h);
    app.set_viewport(w, h);
    let mut buf = Buffer::empty(area);
    ui::render(app, &mut buf, area);
    Ok(buffer_to_string(&buf, color))
}

fn buffer_to_string(buf: &Buffer, color: bool) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            let Some(cell) = buf.cell((x, y)) else {
                continue;
            };
            if color {
                line.push_str(&ansi(cell.fg, cell.bg, cell.modifier));
            }
            line.push_str(cell.symbol());
        }
        if color {
            line.push_str("\x1b[0m");
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn ansi(fg: Color, bg: Color, m: Modifier) -> String {
    let rgb = |c: Color, base: u8| match c {
        Color::Rgb(r, g, b) => format!("\x1b[{base};2;{r};{g};{b}m"),
        _ => String::new(),
    };
    let mut s = String::from("\x1b[0m");
    if m.contains(Modifier::BOLD) {
        s.push_str("\x1b[1m");
    }
    s.push_str(&rgb(fg, 38));
    s.push_str(&rgb(bg, 48));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(state: &str) -> Vec<String> {
        frame_at(state, grid::MIN_COLS, grid::MIN_ROWS)
    }

    fn frame_at(state: &str, w: u16, h: u16) -> Vec<String> {
        let mut app = App::demo(false);
        let s = render_once_at(&mut app, state, false, w, h).expect("render");
        s.lines().map(str::to_owned).collect()
    }

    /// Sizes worth checking: the spec's minimum, one column and one row above
    /// it (where the surplus-sharing arithmetic first does anything), an
    /// awkward prime-ish size, and a plausible full screen.
    const SIZES: &[(u16, u16)] =
        &[(80, 24), (81, 25), (97, 31), (120, 40), (160, 50), (203, 61), (300, 80)];

    /// The spec's layout, for the tests that assert the spec's row numbers.
    fn spec() -> crate::layout::Layout {
        crate::layout::Layout::new(grid::MIN_COLS, grid::MIN_ROWS)
    }

    /// Every rendered line must fit the grid. This is the regression test for
    /// the whole class of overflow defects found in the spec review.
    fn assert_within_grid(lines: &[String]) {
        assert_grid(lines, grid::MIN_COLS, grid::MIN_ROWS);
    }

    fn assert_grid(lines: &[String], w: u16, h: u16) {
        assert_eq!(lines.len(), h as usize, "wrong row count at {w}x{h}");
        for (i, l) in lines.iter().enumerate() {
            let got = grid::width(l);
            assert!(got <= w, "at {w}x{h}, row {i} is {got} columns wide: {l:?}");
        }
    }

    #[test]
    fn every_state_fits_the_grid() {
        for state in ["main", "paused", "filter", "detail", "alert", "help", "spectrum"] {
            assert_within_grid(&frame(state));
        }
    }

    /// The regression test for the whole class of overflow defects, now at
    /// every size rather than only the one the spec drew.
    #[test]
    fn every_state_fits_every_terminal_size() {
        for &(w, h) in SIZES {
            for state in ["main", "paused", "filter", "detail", "alert", "help", "spectrum"] {
                assert_grid(&frame_at(state, w, h), w, h);
            }
        }
    }

    /// A wider terminal has to actually be used, not letterboxed. The meters
    /// span the content field, so the axis rule is the honest witness: it is
    /// drawn edge to edge and nothing else on that row is elastic.
    #[test]
    fn a_bigger_terminal_is_filled_not_letterboxed() {
        for &(w, h) in SIZES {
            let f = frame_at("main", w, h);
            let axis = f
                .iter()
                .find(|l| l.contains("60s ago") && l.contains("now"))
                .unwrap_or_else(|| panic!("no time axis at {w}x{h}"));
            // Content is columns 1..=w-2, and the axis ends in a space that
            // trim_end takes, so a full-width rule measures w-2.
            assert_eq!(grid::width(axis), w - 2, "at {w}x{h} the axis stops short: {axis:?}");
            let footer = f.last().expect("a footer");
            assert!(footer.contains("q quit"), "at {w}x{h}: {footer:?}");
            assert!(
                footer.contains(env!("CARGO_PKG_VERSION")),
                "no version pinned to the right edge at {w}x{h}: {footer:?}"
            );
        }
    }

    /// A crowded list, which is the normal case on a desktop and the one the
    /// eight-row fixture never exercises.
    fn crowded_frame_at(w: u16, h: u16) -> Vec<String> {
        let mut app = App::demo(false);
        let template = app.snap.streams[0].clone();
        for i in 0..40 {
            let mut s = template.clone();
            s.key = format!("extra:{i}");
            s.app = format!("extra{i}");
            app.snap.streams.push(s);
        }
        let s = render_once_at(&mut app, "main", false, w, h).expect("render");
        s.lines().map(str::to_owned).collect()
    }

    /// Taller terminals must show more streams, not more blank rows.
    #[test]
    fn a_taller_terminal_shows_more_streams() {
        // Count the rows actually drawn between the table rule and the prompt.
        let drawn = |w: u16, h: u16| {
            let l = crate::layout::Layout::new(w, h);
            let f = crowded_frame_at(w, h);
            (l.row_table_rule as usize + 1..l.row_prompt as usize)
                .filter(|&i| !f[i].trim().is_empty())
                .count()
        };
        let short = drawn(80, 24);
        let tall = drawn(80, 60);
        assert_eq!(short, 8, "the spec's list is eight rows");
        assert!(
            tall > short,
            "a 60-row terminal drew {tall} stream rows, a 24-row one drew {short}"
        );
    }

    /// And the surplus reaches the meters too, not only the list.
    #[test]
    fn a_taller_terminal_grows_the_meters() {
        let head = |f: &[String]| {
            f.iter().position(|l| l.contains("APP") && l.contains("DEVICE")).expect("table head")
        };
        let short = head(&frame_at("main", 80, 24));
        let tall = head(&frame_at("main", 80, 60));
        assert!(tall > short, "the meters did not grow into a taller terminal");
        // The footer stays pinned to the last row at every height.
        for &(w, h) in SIZES {
            let f = frame_at("main", w, h);
            assert!(
                f[h as usize - 1].contains("q quit"),
                "footer is not on the last row at {w}x{h}"
            );
        }
    }

    #[test]
    fn the_spec_rows_hold_their_content() {
        let f = frame("main");
        assert!(f[spec().row_header as usize].starts_with(" soundwatch"));
        assert!(f[spec().row_out_label as usize].contains("dBFS out"));
        assert!(f[spec().row_in_label as usize].contains("dBFS in"));
        assert!(f[spec().row_axis as usize].contains("60s ago"));
        assert!(f[spec().row_axis as usize].contains("now"));
        assert!(f[spec().row_vitals as usize].contains("all nominal"));
        assert!(f[spec().row_table_head as usize].contains("APP"));
        assert!(f[spec().row_table_head as usize].contains("60s"));
        assert!(f[spec().row_footer as usize].contains("q quit"));
        // At 80 columns six keys leave no room for the full name, so the
        // version compresses to its number rather than a key being dropped.
        assert!(f[spec().row_footer as usize].contains(env!("CARGO_PKG_VERSION")));
        assert!(f[spec().row_footer as usize].contains("? help"), "a key was shed at 80 columns");
        // With room, the full string comes back.
        let wide = frame_at("main", 120, 24);
        assert!(wide[spec().row_footer as usize].contains("soundwatch-lite"));
    }

    #[test]
    fn table_columns_sit_where_the_family_says() {
        let f = frame("main");
        let head = &f[spec().row_table_head as usize];
        // Columns are 1-indexed in the spec, so subtract one for a byte offset.
        assert_eq!(head.find("APP"), Some(1));
        assert_eq!(head.find("DEVICE"), Some(17));
        // LEVEL, RATE and LAT are right-aligned in their fields.
        assert_eq!(head.find("LEVEL"), Some(45));
        assert_eq!(head.find("RATE"), Some(57));
        assert_eq!(head.find("LAT"), Some(66));
        assert_eq!(head.find("60s"), Some(70));
    }

    #[test]
    fn the_alert_state_never_collides_vitals_with_the_verdict() {
        let f = frame("alert");
        assert_within_grid(&f);
        let vitals = &f[spec().row_vitals as usize];
        assert!(vitals.contains("xruns 14"), "missing xrun count: {vitals:?}");
        assert!(vitals.contains("buffer too small"), "missing verdict: {vitals:?}");
        // There must be whitespace between the last vital and the verdict.
        let cut = vitals.find("14 xruns \u{b7}").expect("verdict present");
        assert!(vitals[..cut].ends_with("  "), "vitals and verdict are touching: {vitals:?}");
    }

    #[test]
    fn filter_shows_a_derived_count() {
        let f = frame("filter");
        let prompt = &f[spec().row_prompt as usize];
        // '/' at column 1, query at column 3 — the spec's own spacing.
        assert_eq!(prompt.find('/'), Some(1), "{prompt:?}");
        assert_eq!(prompt.find("zoom"), Some(3), "{prompt:?}");
        assert!(prompt.contains(" of 8 match"), "{prompt:?}");
        assert!(!prompt.contains("8 of 8"), "filter did not actually filter");
    }

    #[test]
    fn detail_opens_a_three_row_block_under_a_five_row_list() {
        let f = frame("detail");
        assert!(f[spec().row_detail() as usize].contains("pid "));
        assert!(f[spec().row_detail() as usize + 1].contains("Hz"));
        assert!(f[spec().row_detail() as usize + 2].contains("latency"));
        // Rows 19-21 are the detail block, so the list stops at 18. The block is
        // anchored by the tree corner at column 3.
        assert_eq!(f[19].find('\u{2514}'), Some(3), "corner glyph misplaced: {:?}", f[19]);
    }

    /// The spectrum screen has to render its graph, its axis and its verdict,
    /// at every size, and it has to name the faults in the fixture.
    #[test]
    fn the_spectrum_screen_draws_and_names_what_it_finds() {
        for &(w, h) in SIZES {
            let f = frame_at("spectrum", w, h);
            let joined = f.join("\n");
            assert!(joined.contains("hann"), "no analysis label at {w}x{h}");
            assert!(joined.contains("1k"), "no frequency axis at {w}x{h}");
            assert!(joined.contains("50 Hz hum"), "the fixture's hum went unreported at {w}x{h}");
            // The stream table belongs to the other screen.
            assert!(!joined.contains("APP  "), "the table leaked into spectrum at {w}x{h}");
            // And there are actual bars, not an empty frame.
            let bars = f.iter().filter(|l| l.contains('\u{2588}')).count();
            assert!(bars >= 4, "only {bars} rows of graph at {w}x{h}");
        }
    }

    #[test]
    fn paused_says_so() {
        let f = frame("paused");
        assert!(f[spec().row_header as usize].contains("PAUSED"));
    }

    #[test]
    fn help_lists_the_keys_the_spec_forgot() {
        let f = frame("help");
        let joined = f.join("\n");
        assert!(joined.contains("move the selection"));
        assert!(joined.contains("clear the filter"));
    }

    #[test]
    fn overflow_is_reported_rather_than_silently_dropped() {
        let f = frame("main");
        // The fixture has 8 streams in 8 rows, so no range indicator.
        assert!(!f[spec().row_prompt as usize].contains(" of 8"));
        // In detail the list shrinks to 5, which must be disclosed.
        let d = frame("detail");
        assert!(
            d[spec().row_prompt as usize].contains("of 8"),
            "{:?}",
            d[spec().row_prompt as usize]
        );
    }
}
