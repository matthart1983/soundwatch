# SoundWatch

**Read-only audio diagnostics for macOS and Linux, in ten tabs.** One question:
*why does my audio sound wrong?*

![SoundWatch](docs/media/demo.gif)

Fourth member of the family after [NetWatch](https://github.com/matthart1983/netwatch),
[SysWatch](https://github.com/matthart1983/syswatch) and
[DiskWatch](https://github.com/matthart1983/diskwatch).

---

## What it answers

| # | tab | what it answers |
|---|---|---|
| 1 | **Overview** | is anything wrong right now |
| 2 | **Devices** | what exists, how it is attached, what it is running at |
| 3 | **Streams** | which app has the microphone, and what is making that noise |
| 4 | **Meters** | peak *and* RMS, and the crest factor between them |
| 5 | **Spectrum** | what is *in* the signal — hum, band limits, DC offset |
| 6 | **Latency** | where the milliseconds actually went |
| 7 | **Xruns** | when it dropped out, on which device |
| 8 | **Routing** | what your "device" is really built out of |
| 9 | **Timeline** | what changed while you were not looking |
| 0 | **Insights** | all of the above, in plain English |

The last one is the point. Everything else asks you to read a number and know
what it means; Insights reads them for you and says which tab to open:

```
▌ WARN   a device changed format mid-session
▌ AirPods Pro: 48k/24 → 16k/16. A rate change under a running stream is heard as a glitch.
▌ [9] Timeline

▌ NOTE   AirPods Pro is on bluetooth
▌ Bluetooth resamples and compresses. If it also has a microphone open, macOS
▌ drops the output to headset quality.
▌ [2] Devices
```

## Install

**From source** — works today, and signs the binary correctly on your machine:

```sh
git clone https://github.com/matthart1983/soundwatch
cd soundwatch
make install          # builds, signs, installs to /usr/local/bin
soundwatch
```

**From a release** — prebuilt, and signed on macOS:

```sh
tar xzf soundwatch-macos-aarch64.tar.gz      # or soundwatch-linux-x86_64.tar.gz
install -m 755 soundwatch-macos-aarch64 /usr/local/bin/soundwatch
```

**On macOS, build with `make`, not `cargo build`.** Cargo alone produces a binary
whose meters can never read anything — see [Consent](#consent-and-why-there-is-a-makefile),
which is the most interesting thing in this repository. On Linux there is nothing
to sign and `cargo build --release` is equivalent; `make` still works and the
signing step is a no-op.

```sh
make                  # release build, signed, ready to meter
make run              # debug build, signed, run against the live audio stack
make probe            # is metering actually receiving samples?
make dist             # per-arch macOS tarballs, signed and verified
make dist-linux       # one tarball for the host architecture
make check            # fmt, clippy -D warnings, tests
make hooks            # refuse commits that fail any of the above

soundwatch --demo     # the design fixtures — touches no audio device at all
```

macOS 14.2+ for metering. Older versions run and report everything else: the
process-tap symbols are resolved at runtime rather than linked, so the binary
starts and explains itself instead of dying in the loader.

Each release is signed with a different code hash, and macOS keys audio consent
to the signature — so upgrading asks for permission again. That is expected, not
a bug.

On Linux, everything except the meters needs nothing installed at all. The meters
need libpulse, which is opened at runtime for the same reason — a machine without
a sound server still runs the tool and says which part is missing rather than
failing to start.

## Keys

`1`–`0` select a tab · `Tab`/`⇧Tab` cycle · `,` settings · `/` filter ·
`↵` detail · `?` help · `p` pause · `q` quit · `↑↓` (or `j`/`k`) move the
selection · `esc` closes whatever is open.

Seven keys and the version string do not fit an 80-column footer, so two things
give way in order: the version drops its name to a bare number, and then keys
are shed — from the *second* to last, never the last, because the last one is
`?` and it is the key that explains all the others.

## Flags

| flag | effect |
|---|---|
| `--demo` | drive the UI from deterministic fixtures; opens nothing |
| `--lite` | the original single screen, at the handoff's exact rows |
| `--meter-input` | also meter the default input device (see [Input](#input-is-opt-in)) |
| `--palette <name>` | `terminal` (default) or `spec` — see [Palettes](#palettes) |
| `--theme <name>` | `spec` (default) or `btop` — see [Themes](#themes) |
| `--ascii` | ASCII stand-ins for glyphs that are not reliably single-width |
| `--once <state>` | render one frame to stdout and exit — any tab by name, or `main`, `paused`, `filter`, `detail`, `alert`, `help`, `settings` |
| `--no-color` | with `--once`, emit plain text |
| `--probe-tap` | start the tap, watch it for five seconds, report what arrived |
| `--defaults` | ignore the saved settings for this run |

## On the tab list

There is no `design_handoff_soundwatch` for the full product — only the Lite
one. The chrome here is the family's, taken from
`design_handoff_syswatch/source/syswatch/sw-chrome.jsx` down to the active tab's
`┘ └` inset. The ten tabs are **derived** from the family taxonomy (Overview
first, Insights last, Timeline beside it) applied to the audio domain. They are
not specified, and this file says so rather than implying otherwise.

## Settings

`,` opens a menu for everything the tool can be told to do differently. Values
are shown rather than hidden behind submenus — half the point of a settings
screen in a diagnostic tool is answering *what is this thing currently doing?*
— and every row carries a line saying what the setting **costs**, because each
one is a trade and none has a right answer.

```
┌─ settings ───────────────────────────────────────────────────┐
│ display                                                      │
│ › theme                                                 spec │
│   glyphs                                             unicode │
│   meter floor                                       -60 dBFS │
│   xrun alert                                               3 │
│   refresh                                             20 fps │
│ metering                                                     │
│   input meter                                            off │
│ spectrum                                                     │
│   analysis size                                      4096 pt │
│   spectrum floor                                    -96 dBFS │
│   bar decay                                          20 dB/s │
│   peak hold                                            1.5 s │
│                                                              │
│ btop shades bars by height; spec colours them by meaning     │
│ ↑↓ choose   ←→ change   w write   r reset   esc close        │
└──────────────────────────────────────────────────────────────┘
```

Everything applies **immediately**, including the input meter — a menu whose
switches only take effect at the next launch is a configuration file with extra
steps. Changing the input meter opens or closes a real capture stream, so macOS
may ask for consent at that moment.

`w` writes to `$XDG_CONFIG_HOME/soundwatch/config.toml` (or `~/.config/…`).
Saving is explicit rather than automatic: a menu that rewrites a dotfile as you
scroll through it is a menu you cannot experiment in. `r` resets to defaults
without saving. Command-line flags override the saved file for one run without
editing it, and `--defaults` ignores it entirely.

The file is `key = value` lines, parsed by hand — taking `serde` and `toml` for
ten scalars would roughly double the dependency tree of a program that
hand-rolls its CoreAudio bindings and its FFT to avoid exactly that. The parser
is deliberately forgiving: an unknown key, a malformed value or one out of range
is skipped and the default kept, and the rest of the file still loads. A
diagnostic tool that refuses to start because its config has a typo has failed
at the one moment you needed it.

**`Config::default()` is the spec's numbers, taken from the constants
themselves.** A fresh install behaves identically to the version that had no
settings menu; a setting is somewhere to depart from the design, not a
substitute for having one.

## Palettes

**SoundWatch defers to your terminal's palette by default.** It pins no colours
of its own: every slot resolves to an ANSI entry and foreground/background use
`Reset`, so a terminal profile, pywal, matugen or a system-wide rice carries
straight through and SoundWatch sits beside your other tools instead of
fighting them.

`--palette spec` is the design handoff's fixed hexes, if you want the look the
screenshots were taken in regardless of the terminal.

Two compromises the 16-colour palette forces. There is no ANSI orange, so the
btop gradient's warm stop takes bright red — it still climbs in the right
direction, but as a step rather than a blend. And with no intermediate shades
to blend through, that gradient becomes four discrete bands rather than a ramp;
it degrades visibly rather than collapsing to one flat colour, which would look
identical to the gradient being switched off.

Palette and theme are independent axes — *which colours* versus *how charts are
drawn* — and they compose: `--palette spec --theme btop` is the original btop
look.

## Themes

`--theme btop` colours every bar as a vertical gradient — the direction colour
at the floor, through yellow and orange to red at the top — and draws charts in
braille, which packs two bars into each cell for twice the horizontal
resolution.

```
spec                                    btop
 ▁▂▃▄▅▆▇█  one colour per column         ⣀⣠⣤⣶⣿  two bars per cell,
           meaning: green until                shaded by height
           yellow at -6 dBFS, red
           at 0 dBFS
```

It is **opt-in, and it costs something.** The palette's central rule is that
red means the audio is wrong right now and nothing decorative is ever red; a
height gradient paints red for height alone, so under this theme a red-tipped
bar no longer *means* anything, it is just tall. In exchange, level becomes
legible as colour at a glance across a wide screen, which a single flat colour
is not. The help overlay's legend changes with the theme, because a legend that
explained the spec's colours under the btop theme would be a lie on the one
screen whose job is to explain the screen. Text, headers and verdicts keep the
reserved colours either way, so the things that are genuinely wrong still
announce themselves.

Braille is not simply higher resolution — it trades **half the vertical steps
for twice the horizontal**: 4 sub-rows per cell against the block elements' 8,
and 2 bars per cell against 1. That is a good trade for a spectrum, where
horizontal is frequency resolution, and a poor one for a three-row meter, which
is one more reason it is not the default. `--ascii` forces blocks whatever the
theme asks for: that flag exists for terminals whose glyph coverage cannot be
trusted, and braille is a far riskier bet than `▁▂▃`.

## Read-only, by construction

No volume, mute, routing or default changes. Nothing is ever written back to the
audio system. Levels are observed, never recorded: no sample leaves the IOProc,
nothing is written to disk, and the only state crossing out of the real-time
thread is an atomic peak and, while the spectrum screen is open, a ring of
recent samples that never leaves the process.

## Consent, and why there is a Makefile

Output metering uses `AudioHardwareCreateProcessTap` (macOS 14.2+), the API Apple
added so a program can observe the system mix without joining the graph. It does
not alter routing, does not change what anyone hears, and does not run inside
anyone else's callback. It is also gated on the `kTCCServiceAudioCapture`
permission, and getting that permission is most of the work.

**The failure mode has no error code.** Without consent the tap is created
successfully, the aggregate device starts, the IOProc fires at exactly the right
rate — and every sample is `0.0`, forever. That is indistinguishable from a quiet
Mac unless you know to look. It is why the meters in this tool used to be flat.

Two things are required, and each is useless without the other:

1. **An identity.** A command-line tool has no bundle, so `build.rs` embeds
   `Info.plist` into the binary's `__TEXT,__info_plist` section, giving it a
   bundle identifier and the `NSAudioCaptureUsageDescription` string that TCC
   insists on before it will display a dialog.

2. **Responsibility.** TCC does not evaluate the process that made the call; it
   evaluates the *responsible* process, which for anything launched from a shell
   is the terminal emulator. Terminals do not ship an audio-capture usage
   description, so `tccd` refuses even to prompt:

   ```
   Refusing authorization request for service kTCCServiceAudioCapture and subject
   Sub:{com.mitchellh.ghostty} ... without NSAudioCaptureUsageDescription key
   ```

   So on startup the process re-executes itself with responsibility disclaimed
   (`responsibility_spawnattrs_setdisclaim`, resolved by `dlsym` so a macOS that
   drops it degrades instead of failing to launch). It then answers for its own
   permissions, the dialog appears, and the grant is recorded against this
   binary. See `src/tcc.rs`.

And one build step: **cargo's output is linker-signed, and a linker signature
does not bind the plist.** `codesign -dvv` reports `Info.plist=not bound` with an
identifier derived from the filename, so TCC has nothing to prompt with. The
Makefile re-signs after linking; that is the whole reason it exists. A binary
built with plain `cargo build` reports the problem itself rather than metering
silence:

```
$ cargo run -- --probe-tap
signed as     : soundwatch_lite-54bd2326cce3536f
this build is not signed with its Info.plist, so macOS cannot ask for audio
consent — run `make` (or `codesign -f -s -` on the binary)
```

Ad-hoc signatures identify a binary by its code hash, so **every rebuild is a new
identity and macOS asks again**. For daily use, sign with a stable certificate:

```sh
make SIGN_ID="Developer ID Application: Your Name (TEAMID)"
```

If the meters are flat, `make probe` separates the three cases: the IOProc never
fired, it fired and delivered digital silence, or metering works. The running
tool makes the same call on its own — a tap that has never seen a non-zero sample
while apps are demonstrably playing to the default output says so on row 11
rather than drawing a confident flat line.

## Input is opt-in

Output has a tap API whose entire purpose is observation without participation.
Input has no equivalent: reading a microphone level means *being* a microphone
client. The indicator comes on, the device may be started if nothing else is
using it, and this process appears in the very mic-in-use audit the stream table
exists to show.

It reports itself there, deliberately. With only the output tap running,
SoundWatch hides itself — looking like an audio client to the HAL is an artefact
of measuring, not a fact about your machine. With `--meter-input` it is really
holding the microphone, and a tool whose job is answering *what has my mic*
does not get to exempt itself from the answer.

That is a decision for the user, so it lives behind `--meter-input` and the input
meter reads `--` until you ask for it. Microphone consent fails the same quiet
way audio capture does, and is detected the same way: a capture stream that reads
*exactly* zero for eight seconds is not a quiet room.

## Platform support

Two backends behind an `AudioBackend` trait: CoreAudio on macOS, ALSA plus an
optional libpulse meter on Linux. A `Caps` struct declares what the active
backend can actually report; the UI renders `--` plus one explanatory line for
anything unavailable, and never shifts the layout to hide a gap. The trait is
sized for the rest of the family — JACK and a native PipeWire backend fit the
same shape.

### macOS

**What works:** output levels, input levels with `--meter-input`, device names,
sample rate, bit depth, channel count, buffer frame size, computed latency
(device + stream latency + safety offset), running state, per-app streams with
the mic-in-use marker, and xrun counts via a `kAudioDeviceProcessorOverload`
listener.

**What does not: per-app levels.** The HAL reports which processes are running
audio, but no level for any of them, and the LEVEL column stays `--`. A process
tap can be scoped to specific processes, so this is reachable — it needs one tap
per process and an aggregate device rebuilt whenever the set changes, which is
more churn in the system audio graph than a diagnostic tool should cause without
being asked. Left for the full tool.

Per-app streams need macOS 14 or newer (`kAudioHardwarePropertyProcessObjectList`).
CoreAudio bindings are hand-rolled in `src/backend/ffi.rs` rather than pulled from
`coreaudio-sys`, which would drag bindgen and libclang into the build and whose
generated bindings predate the process-object API. Every selector was read out of
the SDK headers.

### Linux

Devices, formats, latency and xruns are read from `/proc/asound` — no library, no
daemon, no permissions beyond being able to read `/proc`. Levels are separate:
they open the default monitor source through `libpulse-simple`, which is loaded
with `dlopen` at first use rather than linked. That is the whole reason the tool
still starts on a headless box with no sound server, and it is checked in CI —
if `libpulse` ever becomes a load-time dependency, the build fails.

**What works:** output levels, input levels with `--meter-input`, card and PCM
names, driver-derived transport (HDA, USB, HDMI, Bluetooth…), sample rate, format
and bit depth, channel count, buffer and period size, latency computed from them,
running state, xrun counts, and which process is holding each device.

**What does not: per-application streams.** `/proc/asound` names the process
holding the card, and under PipeWire or PulseAudio that is the sound server for
every application on the machine — so the Streams tab shows one entry for
`pipewire`, not one per app, and says so in the caps note. Per-app streams need
to talk to the sound server itself, which is a native PipeWire or PulseAudio
backend rather than an addition to this one.

Levels come from the default monitor, so they are what the machine is playing in
total, not per device. A card that is not the default sink meters as silence,
which is correct and indistinguishable from a card playing nothing —
`soundwatch --probe-tap` reports which of those you are looking at:

```
soundwatch level probe

output monitor: peak -1.9 dBFS · working
input         : peak -120.0 dBFS · every sample is zero — is anything playing to it?
```

Verified against real hardware (Intel HDA, HDMI, PipeWire) rather than only
against fixtures; the `/proc` parser tests use captures taken verbatim from that
machine.

## Where this departs from the spec

The handoff is marked "high-fidelity / spec-accurate — every glyph, column
position, colour and label is final". These are the places that did not survive
contact with real data, and what was done instead. Each is commented at the site.

**"Never meter by tapping."** Said three times, and written against the old
meaning of the word: instantiating an AudioUnit on the device, which puts work in
the client's callback path and can perturb the very glitch you are observing. A
process tap is not that. Without it both headline meters are dead on macOS, which
makes the tool not worth opening.

**Row 10 collides with itself.** `rate 48k · buffer 128fr · latency 11.8ms ·
xruns 14` occupies columns 1–51 and the alert verdict `14 xruns · buffer too
small` occupies 52–78 — zero gutter, in the mockup's own values. `rate 44.1k`,
`xruns 147` or `latency 112.4ms` overlap outright. The vitals row now sheds its
least load-bearing pair first (`rate`, which is already in the header).

**Nothing outside the table was clipped.** The reference renderer's `drawText` is
unbounded and its right-aligned `padL` keeps the *head* of an over-long string,
so `44.1k→48k/24` renders as `44.1k→48k/`. Every write now goes through a
`Field`, left-aligned text truncates from the right, and right-aligned *values*
are re-formatted to fit rather than sliced — a truncated number is a wrong
number. The header hostname and both meter-label device names have truncation
rules they previously lacked; real device descriptions run past 40 characters.

**60 samples cannot fill 78 columns.** `LITE.md` specifies a "60-sample-per-minute
ring" for a 78-column chart labelled `60s ago → now`. The window is now 78 columns
of 769 ms each, so it really is 60 seconds. The 60→9 sparkline reduction, which
the spec never defines, is **max** — averaging a peak meter hides the transients
the widget exists to show.

**Silence was drawn two ways.** The spec's meter algorithm yields a blank column
at the −60 dBFS floor while its sparkline algorithm yields `▁` for the same input.
Both now draw the baseline; a genuinely blank meter means "no data".

**One xrun is not an emergency.** `LITE.md` triggers the alert on "xruns in the
last 60s" — literally one, which would paint the header red almost permanently and
contradicts its own rule that healthy audio looks calm. The threshold is 3, and
sustained clipping means 5 clipped columns, not one loud snare.

**Red is not actually reserved.** The palette says red means "clipping at 0 dBFS,
or xruns/dropouts. Nothing else", then paints `buffer`, `latency` and a stopped
device red in the alert state. Resolved toward the broader rule the alert state
needs: red means the audio is wrong right now.

**Bit-depth conversion was invisible.** RATE encodes conversion as a rate-to-rate
arrow only, so a 24→16 truncation at the same rate rendered as unremarkable dim
text — despite the full tool calling it out with `insight_bit_depth_downgrade`.
The column now shows `24→16` for that case.

**More keys than the spec allows.** The handoff specifies five; the full
product needs `1`-`0` for the tabs, `,` for settings and `s` for the spectrum in
Lite mode. Same trade as `?` and `↑↓` before them: the alternative is a screen
that cannot be reached. What they overflow is the footer, and §Keys above
describes what gives way.

**80×24 is a floor, not a frame.** The handoff fixes the grid and says nothing
about other sizes; the first build letterboxed, which wasted most of a
full-screen terminal and kept the meters small exactly where there was room to
make them large. See [Layout](#layout).

**The spec forgot some screens.** `?` is in the footer and in the five keys, and
the README promises "an overlay that returns to the same screen", but no document
says what it contains. There was also no key to move the selection — with eight
rows, a fixed selection and `↵ detail`, streams past the eighth were unreachable —
and no overflow story for a list that a normal desktop overruns. Added: the help
panel, arrow/`j`/`k` selection with scrolling, and a `1-5 of 8` range indicator on
the otherwise-blank row 22.

**Filter had no exit and an ambiguous `↵`.** `/` now filters live with no
selection tint (as specified); `↵` commits the query and returns to a selectable
list; `esc` cancels. That resolves the collision with `↵ detail` on the same key.

**`Instant` cannot be serialised.** `TECHNICAL.md` types every timestamp as
`std::time::Instant`, including inside the `Tick` that snapshot/diff and
`--record` write to disk. Lite does not record, but it shares the family's types,
so the clock is wall-clock milliseconds from the start.

**Glyphs.** The handoff fixes the direction arrows as `⯈`/`⯇` (U+2BC8/U+2BC7)
and calls them "the least universal characters in the spec". That was an
understatement — they are absent from JetBrains Mono and from every other common
terminal font checked, and rendered as an empty box, which is how the first take
of the demo recording came out. They are `▸`/`◂` (U+25B8/U+25C2) here: same
shape, same weight, and they exist. `●` (U+25CF) is East-Asian-Ambiguous and
renders double-width under some terminal configurations, which would shift the
DEVICE column on every input row; `--ascii` swaps the risky glyphs, and widths
are computed as display widths throughout.

Two spec details were also **wrong in the prose and right in the renderer**: the
detail-state selection index (`LITE.md` says 1, `so-lite.jsx` uses 2 — neither is
hardcoded here, the selection simply stays visible), and the input DEVICE field,
which is 20 columns not 22 because the mic dot lives inside it.

## Layout

Rows and columns are the spec's, verbatim.

```
row  0  header        row  9  time axis      rows 12-13  table head + rule
rows 2-5  output      row 10  vitals         rows 14-21  streams
rows 6-8  input       row 11  degradation    row 22  filter / range
                              note           row 23  footer
```

Table columns, identical across all four Lites: `1 / 17 / 40 / 51 / 62 / 70`.

80×24 is the **minimum**, not the size. The layout is computed from the
terminal: a third of surplus height goes to the meters in the spec's 3:2 ratio
and the rest to the stream list, and surplus width goes to DEVICE and APP (the
two fields real device names actually overflow) and to the sparkline. Every
number above is reproduced exactly at 80×24, which the tests assert — a
generalisation that does not pass through the spec's own values is a different
design wearing the same name. Smaller terminals get an explicit message rather
than a mangled layout.

## Modules

| module | responsibility |
|---|---|
| `grid.rs` | the clipping contract: `Field`, display-width truncation, glyph sets |
| `fmt.rs` | width-aware value formatting; never truncates a number |
| `meter.rs` | dB-space normalisation, block meters, sparklines, the 60s history ring |
| `model.rs` | data model, `Caps`, and the alert rules |
| `tcc.rs` | becoming our own TCC subject, so consent can be asked for at all |
| `layout.rs` | every row and column, for whatever size the terminal is |
| `dsp.rs` | the FFT, the Hann window, and the normalisation that is usually wrong |
| `spectrum.rs` | log frequency mapping, analyser ballistics, and the fault detectors |
| `chart.rs` | bar charts in blocks or braille, shared by the meters and the spectrum |
| `backend/ioproc.rs` | the real-time callback, and the seqlock the RMS pair rides on |
| `config.rs` | every setting, the `,` menu's model, and the file it is saved in |
| `tabs/` | the ten tabs and the chrome around them |
| `backend/worker.rs` | the backend on its own thread, so a stalled HAL cannot freeze the UI |
| `backend/ioproc.rs` | the real-time peak callback both meters attach through |
| `backend/tap.rs` | the output process tap and its private aggregate device |
| `backend/input.rs` | the opt-in capture stream behind the input meter |
| `backend/ffi.rs` | hand-rolled CoreAudio HAL bindings |
| `backend/coreaudio.rs` | the macOS backend, and what it will admit it cannot do |
| `ui.rs` | the 80×24 screen and all six states |
| `app.rs` | state machine, sampling, key handling |
| `demo.rs` | the handoff's deterministic fixtures, behind the backend trait |

## Licence

MIT. See [LICENSE](LICENSE).
