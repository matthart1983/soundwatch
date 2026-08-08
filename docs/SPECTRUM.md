# SPECTRUM.md — the spectrum analyser mode

Status: **implemented.** Written as a specification against `4081a6a` and built
from it; this section records where the build departed from the plan, and why.
Everything else below is what shipped.

| planned | shipped |
|---|---|
| rows 3–18, a fixed 16-row graph | the graph fills whatever height the terminal has — the layout stopped being fixed while this was being built |
| row 22 carries a band readout under a cursor | **not built.** There is no cursor, and adding one means a seventh key and a mode. The peak readout on row 2 answers the same question for the case that matters |
| an explicit 50% hop between transforms | no hop. The analyser takes the most recent window from the ring each frame — about 41% overlap at 20 fps and 48 kHz, arrived at by the display rate rather than scheduled against it |
| `Analysis` exposes the resolution limit | test-only. It pins the arithmetic behind "do not interpolate"; drawing a marker for it would be clutter |

Two things the spec did not anticipate and the build added:

* **Overrun reporting outranks every other verdict.** If the callback laps the
  reader, the picture is of a signal that never existed, so the verdict line
  says so before it says anything about hum or bandwidth.
* **The audio only crosses the channel while the screen is open.** `s` tells the
  backend thread what to collect; with the spectrum closed, nothing is sent.
  A hundred kilobytes a second for a screen nobody is looking at is waste.

The rest of this document is the specification as written, and is still the
reference for the DSP constants and the detector thresholds.

---

## 1. What it is for

The tool answers one question — *why does my audio sound wrong?* The meters
answer it in the amplitude domain: too loud, too quiet, clipping, dropping out.
A whole class of "sounds wrong" is invisible there, because the level is fine
and the **content** is not:

| symptom | what the meters show | what a spectrum shows |
|---|---|---|
| ground-loop hum | normal level | a spike at 50/60 Hz and its harmonics |
| Bluetooth SBC / low-bitrate codec | normal level | everything above ~14–16 kHz gone, with a cliff edge |
| a DC-offset interface eating headroom | slightly high peaks | energy in bin 0 |
| a resonant room or a bad driver | normal level | a persistent narrow peak |

Those are the reads this mode exists to deliver. **A spectrum display that does
not name them is a screensaver.** Section 9 specifies the detectors, and they
are as much the deliverable as the graph is.

### What it is explicitly not

Not a measurement instrument. No calibrated SPL, no weighting curves, no
THD+N figure, no waterfall, no per-app spectrum. Anyone who needs those has
Room EQ Wizard and a measurement mic. This is the same promise as the rest of
the tool: enough to find the fault, in one screen, in a terminal.

---

## 2. The mode

`s` cycles the main screen through **off → output → input → off**.

This is a sixth key, and the handoff specifies five. Taking the deviation
deliberately, and noting it in the README's deviations section, for the same
reason `?` and `↑↓` were added: the alternative is a feature that cannot be
reached. `s` was chosen because it is unbound, mnemonic, and not adjacent to
`q` on a QWERTY keyboard.

* **Output** reads the process tap. Green, per the palette's playback path.
* **Input** reads the capture stream. Cyan. If `--meter-input` was not passed
  or the meter is not live, cycling to input shows the degraded state and the
  existing `Caps` note explains why, exactly as the input meter does now. It
  does **not** silently skip to off — a key that appears to do nothing is worse
  than a key that explains itself.

`esc` also leaves the mode. `p`, `/`, `↵` and `?` behave as they do now; the
filter and the stream table are not visible in this mode, so `/` and `↵` return
to the main screen first and then act. Interaction with `Mode::Detail` and
`Mode::Filter`: spectrum is a property of the main screen, not a `Mode` — add a
`spectrum: SpectrumSource` field to `App` rather than a variant to `Mode`, or
the six-way state machine becomes a matrix.

`--once spectrum` renders one frame from the demo fixtures, for review and for
the grid tests.

---

## 3. Layout

Spectrum replaces the meters, the time axis and the stream table. It does not
replace the header, the vitals row, the degradation note or the footer — those
are how you know the machine is still healthy while you stare at the graph.

The stream table goes because the question being asked has changed. In spectrum
mode you are asking *what is in this signal*, not *which app is making it*.

```
row  0   header                     (unchanged)
row  1   blank
row  2   source · window · resolution · peak readout
rows 3-18   the spectrum            (16 rows)
row 19   frequency axis
row 20   blank
row 21   diagnostic verdict
row 22   band readout under the cursor, or hints
row 23   footer                     (unchanged, with `s spectrum`)
```

Content stays in columns 1–78, as everywhere else. Row 10's vitals and row 11's
note are **not** shown in this mode — rows 2–19 are the graph — but the header
keeps its alert colour, so an xrun during a spectrum session is still visible.

### Row 2

```
 ⯈ output   4096pt hann   11.7 Hz/bin   peak 1002 Hz  -6.1 dBFS
```

Left: the direction glyph and source, in the source colour. Right: the
interpolated peak (§6.4). Both through `Field`s; the peak readout is the
elastic element and is dropped entirely before anything truncates, per the
grid contract — a truncated frequency is a wrong frequency.

### Row 19, the frequency axis

Ticks at 100 Hz, 1 kHz, 10 kHz, drawn on the rule and labelled centred on their
true column. At 48 kHz those columns are **18, 43 and 68** (§6.2), which is a
directly assertable test.

```
  20    100        1k         10k      24k
 ─────────┴──────────┴──────────┴─────────
```

---

## 4. Signal path

```
CoreAudio IOProc  →  SPSC ring  →  sampler thread  →  FFT  →  bins
   (real-time)       (lock-free)     (20 Hz)                    ↓
                                                          log columns
                                                               ↓
                                                          ballistics → glyphs
```

### 4.1 The real-time half

`ioproc.rs` currently folds a peak into an atomic and returns. That is one
scalar; a spectrum needs the samples. The IOProc gains one job: copy the block
into a preallocated ring. It must keep every property it has now — **no
allocation, no locks, no syscalls, no logging, and no unbounded work.**

* Ring is `Box<[f32]>`, allocated once at `IoProcMeter::attach`, capacity
  16384 frames (power of two, 64 KiB). That is 341 ms at 48 kHz — four analysis
  windows of slack.
* Two `AtomicUsize` indices, `write` and `read`, both `Relaxed` for the
  counters with a single `Release`/`Acquire` pair publishing the write. Single
  producer, single consumer; no CAS loop.
* The IOProc **sums to mono** as it copies: `0.5 * (L + R)`. One multiply-add
  per frame, and it quarters both the ring and the reader's work.
* **Overrun drops the oldest.** The writer never blocks and never skips
  writing. A spectrum wants the most recent audio; stale audio is worthless.
  Advance `read` past the overwritten region and increment an
  `overruns: AtomicU64` so the condition is observable rather than silent —
  the same principle as `has_ever_heard_signal`.

The mono sum is the one decision here that forecloses something: it makes the
L/R diagnostics of §14 impossible without a second ring. That is accepted, and
§14 prices the reversal.

### 4.2 The non-real-time half

The existing 20 Hz sampler drains the ring. It must tolerate finding zero, one
or several complete hops, and must never block on the ring.

---

## 5. Analysis

| parameter | value | why |
|---|---|---|
| FFT size `N` | 4096 | 11.7 Hz bins and an 85 ms window at 48 kHz |
| hop | 2048 (50% overlap) | 23.4 analyses/sec, comfortably above the 20 Hz display |
| window | Hann | the right default: −31 dB sidelobes, and the scalloping is tolerable and correctable (§6.4) |
| display floor | **−96 dBFS** | not `meter::FLOOR_DBFS` — see below |

**The floor is not −60.** Broadband energy divides across bins, so a signal
whose *peak* is −6 dBFS sits far lower per bin: pink noise at −6 dBFS broadband
reads around −45 per bin at this resolution. Reusing the meters' −60 dBFS floor
would slam most real content into the bottom two rows. −96 dBFS is the 16-bit
noise floor and is what an audio person expects a spectrum to bottom out at.
16 rows × 8 sub-blocks = 128 steps over 96 dB = **0.75 dB per step**.

### 5.1 Normalisation — the part that is usually wrong

Hann's coherent gain is `CG = mean(w) = 0.5`. Single-sided amplitude for bin
`k`, for `0 < k < N/2`:

```
A[k] = 2·|X[k]| / (N·CG)  =  4·|X[k]| / N
```

Bins `0` (DC) and `N/2` (Nyquist) are **not** doubled: `A = |X[k]| / (N·CG)`.
Then `dBFS = 20·log10(A)`.

Check the arithmetic, because this is the bug that ships: a full-scale sine at
exactly a bin centre gives `|X[k]| = N·CG·A/2 = 4096·0.5·0.5 = 1024`, so
`A = 4·1024/4096 = 1.0` → **0.0 dBFS**. That is §12's first test.

Skipping the `1/CG` compensation costs exactly 6.02 dB and makes every reading
quietly wrong.

### 5.2 Which FFT

Recommend **hand-rolled**: iterative radix-2 with a precomputed twiddle table,
plus the standard real-input trick (pack `N` reals into an `N/2` complex FFT
and untangle). Around 120 lines, textbook, and it holds the dependency tree at
four crates — the same reasoning that keeps `ffi.rs` hand-rolled instead of
pulling in `coreaudio-sys`.

This is *not* a performance argument. `rustfft` is faster and better tested,
and at 23 transforms a second neither is measurable. If the hand-rolled version
fails §12's accuracy tests twice, take `rustfft` and stop.

---

## 6. Frequency → columns

### 6.1 The axis is logarithmic

Pitch is logarithmic and so is every reason you would open this screen. A
linear axis spends half its width on 12–24 kHz, where nothing diagnostic ever
happens, and crushes the entire bass range into two columns.

Range: **20 Hz to Nyquist**, over content columns 1–78.

```
f(c) = 20 · 10^( (c-1)/77 · log10(Nyquist/20) )
c(f) = 1 + 77 · log10(f/20) / log10(Nyquist/20)
```

### 6.2 Checkable consequences at 48 kHz

`log10(24000/20) = 3.07918`, so 0.03999 decades per column.

| f | column |
|---|---|
| 100 Hz | 18 |
| 1 kHz | 43 |
| 10 kHz | 68 |

### 6.3 Resolution, told honestly

A column spans `0.0964 · f`, and a bin is 11.72 Hz, so column width equals bin
width at **121.6 Hz — column 21**. Below that, several columns land in one bin.

**Do not interpolate them.** Draw the bin's true value and let the low end
stair-step. Interpolation would invent resolution that is not there, which is
the same sin as a truncated number rendered as a whole one. Above column 21,
each column aggregates several bins: take the **maximum**, for the reason
`downsample_max` already gives — averaging hides the narrow peaks the widget
exists to find.

Doubling to `N = 8192` moves the stair-stepped region to column 13, at the cost
of a 170 ms window. Not worth it: the phenomena being hunted are steady-state,
but 170 ms of smear makes the display feel broken on music.

### 6.4 The peak readout

Report the peak's frequency by **parabolic interpolation on the log-magnitudes**
of the three bins around the maximum:

```
δ = 0.5·(y[k-1] − y[k+1]) / (y[k-1] − 2·y[k] + y[k+1])      (δ ∈ [−0.5, 0.5])
f = (k + δ) · sample_rate / N
```

This matters more than it looks. Hann scalloping costs up to 1.42 dB for a tone
between bin centres, and raw bin indices put 50 Hz and 60 Hz in adjacent bins —
**too coarse to tell apart, which is exactly the distinction §9.1 needs to
make.** Interpolation resolves a strong tone to within about 1 Hz, so the hum
detector reads the interpolated frequency, never the bin index.

---

## 7. Ballistics

Raw per-frame bins flicker too much to read. Per bin:

* **Attack: instantaneous.** A transient must appear on the frame it happens.
* **Decay: −20 dB/s**, applied as `−20/fps` dB per frame.
* **Peak hold: 1.5 s**, then falls at −12 dB/s. Drawn as the topmost cell of
  the column in a dimmed source colour, so held peaks read as a distinct outline
  above the live bars.

These are conventional analyser ballistics and are chosen to be recognisable,
not novel. All three live in one `Ballistics` struct with the rates as
constants, testable by stepping a known number of frames.

---

## 8. Colour

Reuse `theme::level(dbfs, base)` unchanged, with `base` the source colour. A
column above −6 dBFS goes yellow, at or above −0.1 goes red. This keeps the one
documented palette deviation — SoundWatch being the only Lite whose chart
colour varies within a chart — rather than adding a second one.

Peak-hold caps use `theme::FAINT` mixed toward the source colour: present, not
competing.

---

## 9. The diagnostics

Each produces one line on row 21, in the same voice as `Verdict`: what is wrong,
then what to do. Each requires **3 seconds sustained** before it fires, for the
reason `XRUN_ALERT_THRESHOLD` exists — one frame of anything is not a fault, and
a row that flickers accusations is a row people learn to ignore. All are
suppressed when the source is silent (every bin at the floor).

### 9.1 Mains hum

A peak whose interpolated frequency (§6.4) is within 2 Hz of 50 or 60 Hz, at
least **20 dB** above the median of bins between 30 and 200 Hz.

```
50 Hz hum, 22 dB above the noise floor · check grounding and cable runs
```

Also test 100/120 Hz: hum with a strong second harmonic and a weak fundamental
is the signature of rectified supply ripple rather than a ground loop, and is
worth distinguishing if the fundamental is also present.

### 9.2 Band limit

Let `F` be the highest frequency with a level at least 12 dB above the floor.
Fire when `F < 0.9 · Nyquist` **and** the level falls by ≥ 40 dB across less
than 1 kHz around `F` — a cliff, not a roll-off. The cliff test is what keeps
quiet or genuinely dull material from tripping it.

```
content stops at 15.7 kHz · lossy source, or a Bluetooth codec
```

This is the highest-value read here. It catches SBC and low-bitrate AAC over
Bluetooth, and low-bitrate files, neither of which shows on any other screen.

### 9.3 DC offset

Bin 0 at or above −60 dBFS.

```
DC offset on the capture path · it is eating headroom
```

### 9.4 Deliberately absent

No aliasing detector (needs a known stimulus), no clipping detector (the
existing sustained-clipping rule already owns that verdict and two sources
disagreeing on one screen is worse than one), no room-mode analysis (needs a
calibrated mic and a sweep).

---

## 10. Degraded states

The mode must render at every stage of not-working, and never draw a confident
flat line. It reuses the existing machinery rather than inventing a second one.

| condition | display |
|---|---|
| meter not live (`Caps` says so) | flat faint baseline, `--` in the readout, existing note on row 21 |
| meter opening (consent dialog up) | same, with the existing "answer the audio permission prompt" note |
| live but never heard a signal, while apps are playing | the existing deaf-meter note — this is the consent failure that has no error code |
| live and genuinely silent | every column at the floor, `peak --`, row 21 blank |
| paused | last frame held, everything in `FAINT`, `◆ PAUSED` in the header |
| ring overruns > 0 | `spectrum is dropping blocks` on row 21 |

The distinction between the last two rows of that table and a working analyser
is the whole reason `has_ever_heard_signal` exists. Do not regress it.

---

## 11. Demo fixtures

`--demo` must drive this mode with no audio device, as it does everything else —
it is the fallback when consent is unavailable, and it is what CI can run.

Add to `demo.rs` a deterministic synthetic spectrum: pink-noise-shaped tilt,
a 1 kHz tone at −6 dBFS, a 50 Hz spike 24 dB up, and a brick wall at 15.7 kHz.
One fixture then exercises the renderer, the log axis, the ballistics **and all
three detectors**, and `--once spectrum` is a review artefact and a test.

`x` in demo mode should toggle the faults on and off, as it toggles xruns now.

---

## 12. Tests

Non-negotiable, because every one of these is a silent failure if wrong:

**Correctness of the transform**
1. Full-scale 1 kHz sine at a bin centre reads `0.0 dBFS ± 0.1` — catches the
   missing `1/CG`.
2. It peaks in bin `round(1000/11.719) = 85`.
3. DC-only input puts its energy in bin 0 and nothing above the window's
   sidelobe floor in bin 1.
4. Nyquist-frequency input is not doubled.
5. Parseval: summed bin power equals windowed time-domain power, ±0.1 dB.
6. Silence reads the floor in every bin, with no `NaN` and no `-inf` escaping
   into the renderer.

**Mapping**
7. 100 Hz, 1 kHz, 10 kHz land in columns 18, 43, 68 at 48 kHz.
8. Column → frequency is strictly monotonic, `f(1) = 20`, `f(78) = Nyquist`.
9. Every bin from 20 Hz to Nyquist is claimed by at least one column — no bin is
   silently dropped between the linear and log domains.
10. Parabolic interpolation recovers a 1002.5 Hz tone to within 1 Hz.

**Ballistics**
11. A bin driven to 0 dBFS then to silence decays 20 dB in exactly one second
    of frames.
12. A peak hold survives 1.5 s and then falls.

**Ring**
13. Writer overrun drops the oldest, never blocks, and the reader still gets
    the most recent `N` samples contiguously.
14. `overruns` increments when and only when data was actually lost.

**Detectors**
15. Hum fires on the synthetic 50 Hz fixture, reports 50 not 60, and does not
    fire on full-band pink noise.
16. Band limit fires on the 15.7 kHz fixture and not on full-band noise.
17. Nothing fires on silence.
18. Nothing fires before 3 seconds sustained.

**Layout**
19. Every spectrum state fits the grid — reuse `assert_within_grid`, which is
    what already catches this whole class.

---

## 13. Performance

A 4096-point real FFT is roughly 50k flops: 20–40 µs. At 23 per second that is
under 0.1% of one core. The copy in the IOProc is 512 multiply-adds per block,
far below the existing peak fold's cost. Nothing here justifies a fast path, and
nothing here belongs on the real-time thread beyond the copy.

If profiling ever says otherwise, the first move is to drop the analysis rate,
not to optimise the transform.

---

## 14. Not in v1, and what it would cost

**Per-channel spectra, and the mono/phase reads.** *Is my stereo actually mono?
Is the right channel dead? Is something out of phase?* These are top-tier
questions that no other screen in the tool can answer, and the meters cannot
either. They need the IOProc to keep L and R separate: a second ring (+64 KiB),
a second FFT (still under 0.2% CPU), and a decision about how to draw two
spectra in 16 rows — probably one mirrored above the other, or an L−R
difference trace over a summed spectrum.

Deferred only because §4.1's mono sum is the cheaper v1, and the reader's
interface is designed so that adding a second ring does not change it. If this
mode gets built and used, this is the most likely first follow-up.

**Also out:** waterfall/spectrogram (needs the history rows the stream table
occupies), per-app spectra (needs the per-process taps the README already
prices), 1/3-octave banding, and any form of calibration.

---

## 15. Decisions for the author

1. **Six keys.** `s` breaks the five-key promise. Accept, or hide the mode
   behind a flag and keep the promise?
2. **Losing the stream table.** Argued for in §3, but it means you cannot see
   *which app* is producing the spectrum you are looking at. Is a compact
   8-row spectrum that keeps the table worth having instead — and is 8 rows
   (64 steps, 1.5 dB each) enough to read?
3. **Default source.** Output, per §2. But the loudest use case for a spectrum
   is diagnosing a *microphone*, which needs `--meter-input`. Should `s` imply
   input metering and prompt for consent, or stay passive?
4. **`--once spectrum`.** Worth the fixture work in §11? It is what makes the
   mode reviewable without a terminal and testable in CI, so this is really a
   question about how much CI coverage this mode deserves.
