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
mod fmt;
mod grid;
mod meter;
mod model;
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
                      main, paused, filter, detail, alert, help
    --no-color        with --once, emit plain text
    --probe-tap       start the output tap, watch it for five seconds, and
                      report whether real samples arrive. Use this first when
                      the meters look flat.
    -h, --help        this message
    -V, --version     print version

KEYS:
    q quit    p pause    / filter    enter detail    ? help
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
    let backend: Box<dyn AudioBackend> = if args.demo {
        Box::new(demo::DemoBackend::default())
    } else {
        Box::new(CoreAudio::new(backend::Metering {
            output: wants_metering,
            input: wants_metering && args.meter_input,
        }))
    };
    let mut app = App::new(backend, args.demo, args.ascii);

    if let Some(state) = args.once {
        match render_once(&mut app, &state, args.color) {
            Ok(s) => print!("{s}"),
            Err(e) => {
                eprintln!("soundwatch-lite: {e}");
                std::process::exit(2);
            }
        }
        return;
    }

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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let press = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);

    match state {
        "main" => {}
        "paused" => app.on_key(press(KeyCode::Char('p'))),
        "detail" => app.on_key(press(KeyCode::Enter)),
        "help" => app.on_key(press(KeyCode::Char('?'))),
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

    let area = Rect::new(0, 0, grid::COLS, grid::ROWS);
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
    use crate::backend::Levels;
    use crate::model::{Caps, Snapshot};

    struct Fake;
    impl AudioBackend for Fake {
        fn name(&self) -> &'static str {
            "demo"
        }
        fn caps(&self) -> Caps {
            Caps::default()
        }
        fn snapshot(&mut self) -> Snapshot {
            demo::snapshot(0)
        }
        fn levels(&mut self) -> Levels {
            Levels::default()
        }
    }

    fn frame(state: &str) -> Vec<String> {
        let mut app = App::new(Box::new(Fake), true, false);
        let s = render_once(&mut app, state, false).expect("render");
        s.lines().map(str::to_owned).collect()
    }

    /// Every rendered line must fit the grid. This is the regression test for
    /// the whole class of overflow defects found in the spec review.
    fn assert_within_grid(lines: &[String]) {
        assert_eq!(lines.len(), grid::ROWS as usize, "wrong row count");
        for (i, l) in lines.iter().enumerate() {
            let w = grid::width(l);
            assert!(w <= grid::COLS, "row {i} is {w} columns wide: {l:?}");
        }
    }

    #[test]
    fn every_state_fits_the_grid() {
        for state in ["main", "paused", "filter", "detail", "alert", "help"] {
            assert_within_grid(&frame(state));
        }
    }

    #[test]
    fn the_spec_rows_hold_their_content() {
        let f = frame("main");
        assert!(f[ui::ROW_HEADER as usize].starts_with(" soundwatch"));
        assert!(f[ui::ROW_OUT_LABEL as usize].contains("dBFS out"));
        assert!(f[ui::ROW_IN_LABEL as usize].contains("dBFS in"));
        assert!(f[ui::ROW_AXIS as usize].contains("60s ago"));
        assert!(f[ui::ROW_AXIS as usize].contains("now"));
        assert!(f[ui::ROW_VITALS as usize].contains("all nominal"));
        assert!(f[ui::ROW_TABLE_HEAD as usize].contains("APP"));
        assert!(f[ui::ROW_TABLE_HEAD as usize].contains("60s"));
        assert!(f[ui::ROW_FOOTER as usize].contains("q quit"));
        assert!(f[ui::ROW_FOOTER as usize].contains("soundwatch-lite"));
    }

    #[test]
    fn table_columns_sit_where_the_family_says() {
        let f = frame("main");
        let head = &f[ui::ROW_TABLE_HEAD as usize];
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
        let vitals = &f[ui::ROW_VITALS as usize];
        assert!(vitals.contains("xruns 14"), "missing xrun count: {vitals:?}");
        assert!(vitals.contains("buffer too small"), "missing verdict: {vitals:?}");
        // There must be whitespace between the last vital and the verdict.
        let cut = vitals.find("14 xruns \u{b7}").expect("verdict present");
        assert!(vitals[..cut].ends_with("  "), "vitals and verdict are touching: {vitals:?}");
    }

    #[test]
    fn filter_shows_a_derived_count() {
        let f = frame("filter");
        let prompt = &f[ui::ROW_PROMPT as usize];
        // '/' at column 1, query at column 3 — the spec's own spacing.
        assert_eq!(prompt.find('/'), Some(1), "{prompt:?}");
        assert_eq!(prompt.find("zoom"), Some(3), "{prompt:?}");
        assert!(prompt.contains(" of 8 match"), "{prompt:?}");
        assert!(!prompt.contains("8 of 8"), "filter did not actually filter");
    }

    #[test]
    fn detail_opens_a_three_row_block_under_a_five_row_list() {
        let f = frame("detail");
        assert!(f[ui::ROW_DETAIL as usize].contains("pid "));
        assert!(f[ui::ROW_DETAIL as usize + 1].contains("Hz"));
        assert!(f[ui::ROW_DETAIL as usize + 2].contains("latency"));
        // Rows 19-21 are the detail block, so the list stops at 18. The block is
        // anchored by the tree corner at column 3.
        assert_eq!(f[19].find('\u{2514}'), Some(3), "corner glyph misplaced: {:?}", f[19]);
    }

    #[test]
    fn paused_says_so() {
        let f = frame("paused");
        assert!(f[ui::ROW_HEADER as usize].contains("PAUSED"));
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
        assert!(!f[ui::ROW_PROMPT as usize].contains(" of 8"));
        // In detail the list shrinks to 5, which must be disclosed.
        let d = frame("detail");
        assert!(d[ui::ROW_PROMPT as usize].contains("of 8"), "{:?}", d[ui::ROW_PROMPT as usize]);
    }
}
