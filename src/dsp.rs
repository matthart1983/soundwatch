//! The transform behind the spectrum screen.
//!
//! Hand-rolled rather than `rustfft`, for the reason `ffi.rs` is hand-rolled
//! rather than `coreaudio-sys`: a radix-2 FFT is textbook, and this keeps the
//! dependency tree at four crates. It is explicitly **not** a performance
//! argument — `rustfft` is faster and better tested, and at twenty-three
//! transforms a second neither is measurable. If this ever fails its accuracy
//! tests twice, take the dependency and stop.
//!
//! A full complex transform of `N` reals, rather than the `N/2` real-input
//! packing trick. It costs twice as much and a 4096-point transform still runs
//! in tens of microseconds; the packing is where the bugs live.
//!
//! ## The part that is usually wrong
//!
//! Hann has a coherent gain of 0.5, so a spectrum that does not divide it back
//! out reads **6.02 dB low, everywhere**, and looks entirely plausible while
//! doing so. See [`Analysis::bin_dbfs`] and the test that pins a full-scale
//! sine to 0.0 dBFS.

use std::f32::consts::PI;

/// Transform size. 11.7 Hz bins and an 85 ms window at 48 kHz.
///
/// Doubling this would halve the region where the log axis is finer than the
/// transform (see `spectrum::COLUMN_LIMITED_BELOW_HZ`), at the cost of a 170 ms
/// window — long enough to smear music into mush. The phenomena this screen
/// hunts are steady-state, but the display still has to feel alive.
pub const FFT_SIZE: usize = 4096;
// There is no explicit hop: the analyser takes the most recent FFT_SIZE
// samples from the ring every frame. At 20 fps and 48 kHz that is 2400 new
// samples against a 4096-sample window — about 41% overlap, arrived at by the
// display rate rather than imposed on it, and no scheduling to keep in step.

/// The bottom of the spectrum scale.
///
/// Not [`crate::meter::FLOOR_DBFS`]. Broadband energy divides across bins, so a
/// signal peaking at -6 dBFS sits far lower per bin — pink noise at -6 dBFS
/// broadband reads around -45 per bin at this resolution. Reusing the meters'
/// -60 dBFS floor would slam most real content into the bottom two rows.
/// -96 dBFS is the 16-bit noise floor, which is where an audio person expects a
/// spectrum to bottom out.
pub const FLOOR_DBFS: f32 = -96.0;

/// Hann window and its coherent gain, computed once.
pub struct Window {
    w: Vec<f32>,
    /// `mean(w)`; 0.5 for Hann, but derived rather than assumed.
    coherent_gain: f32,
}

impl Default for Window {
    fn default() -> Self {
        Self::hann(FFT_SIZE)
    }
}

impl Window {
    pub fn hann(n: usize) -> Self {
        let w: Vec<f32> =
            (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos()).collect();
        let coherent_gain = w.iter().sum::<f32>() / n as f32;
        Self { w, coherent_gain }
    }
}

/// In-place iterative radix-2 Cooley-Tukey. `re`/`im` must be a power of two.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two());
    debug_assert_eq!(im.len(), n);
    if n < 2 {
        return;
    }

    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    // Butterflies, stage by stage.
    let mut len = 2usize;
    while len <= n {
        let ang = -2.0 * PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        let mut i = 0;
        while i < n {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (ar, ai) = (re[i + k], im[i + k]);
                let (br, bi) = (re[i + k + len / 2], im[i + k + len / 2]);
                let (tr, ti) = (br * cr - bi * ci, br * ci + bi * cr);
                re[i + k] = ar + tr;
                im[i + k] = ai + ti;
                re[i + k + len / 2] = ar - tr;
                im[i + k + len / 2] = ai - ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
            i += len;
        }
        len <<= 1;
    }
}

/// One transform's worth of magnitudes, in dBFS.
pub struct Analysis {
    /// `size/2 + 1` bins, DC through Nyquist.
    pub bins: Vec<f32>,
    pub rate: u32,
    /// Transform size this came from. Configurable, so never assume the const.
    pub size: usize,
    /// Bottom of the scale these bins were clamped to.
    pub floor: f32,
}

impl Analysis {
    pub fn bin_hz(&self, k: usize) -> f32 {
        k as f32 * self.rate as f32 / self.size as f32
    }

    pub fn nyquist(&self) -> f32 {
        self.rate as f32 / 2.0
    }

    /// The loudest bin, with its frequency refined by parabolic interpolation.
    ///
    /// This matters more than it looks. Hann scalloping costs up to 1.42 dB for
    /// a tone between bin centres, and at 11.7 Hz bins the 50 Hz and 60 Hz hum
    /// cases land in *adjacent* bins — too coarse to tell apart by index, which
    /// is exactly the distinction the hum detector has to make. Interpolating
    /// on the log magnitudes resolves a strong tone to within about a hertz.
    pub fn peak(&self) -> Option<(f32, f32)> {
        // Skip DC: it is not a tone and it is reported separately.
        let (k, &db) = self
            .bins
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))?;
        if db <= self.floor {
            return None;
        }
        let delta = if k > 0 && k + 1 < self.bins.len() {
            let (a, b, c) = (self.bins[k - 1], db, self.bins[k + 1]);
            let denom = a - 2.0 * b + c;
            if denom.abs() < 1e-9 { 0.0 } else { (0.5 * (a - c) / denom).clamp(-0.5, 0.5) }
        } else {
            0.0
        };
        Some(((k as f32 + delta) * self.rate as f32 / self.size as f32, db))
    }
}

/// Windowing, transform and normalisation, with the scratch buffers kept.
pub struct Analyzer {
    window: Window,
    re: Vec<f32>,
    im: Vec<f32>,
    size: usize,
    floor: f32,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new(FFT_SIZE, FLOOR_DBFS)
    }
}

impl Analyzer {
    /// `size` must be a power of two; anything else falls back to the default,
    /// because a radix-2 transform of a non-power-of-two is not a transform.
    pub fn new(size: usize, floor: f32) -> Self {
        let size = if size.is_power_of_two() && size >= 64 { size } else { FFT_SIZE };
        Self {
            window: Window::hann(size),
            re: vec![0.0; size],
            im: vec![0.0; size],
            size,
            floor: if floor < 0.0 { floor } else { FLOOR_DBFS },
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn floor(&self) -> f32 {
        self.floor
    }

    /// Analyse exactly [`Analyzer::size`] samples. Shorter input is
    /// zero-padded, longer input uses the most recent window.
    pub fn analyze(&mut self, samples: &[f32], rate: u32) -> Analysis {
        let n = self.size;
        let src = if samples.len() > n { &samples[samples.len() - n..] } else { samples };
        self.re[..src.len()].copy_from_slice(src);
        self.re[src.len()..].fill(0.0);
        self.im.fill(0.0);
        for (v, w) in self.re.iter_mut().zip(self.window.w.iter()) {
            *v *= w;
        }

        fft(&mut self.re, &mut self.im);

        let nf = n as f32;
        let cg = self.window.coherent_gain;
        let floor = self.floor;
        let bins = (0..=n / 2)
            .map(|k| {
                let mag = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt();
                // Single-sided amplitude. DC and Nyquist are not doubled: they
                // have no negative-frequency twin to fold in.
                let amp =
                    if k == 0 || k == n / 2 { mag / (nf * cg) } else { 2.0 * mag / (nf * cg) };
                if amp <= 0.0 { floor } else { (20.0 * amp.log10()).max(floor) }
            })
            .collect();
        Analysis { bins, rate, size: n, floor }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn sine(hz: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * PI * hz * i as f32 / RATE as f32).sin()).collect()
    }

    /// Bin `k` sits at exactly `k * rate / N`, so this tone has no scalloping
    /// and no leakage beyond the window's own skirts.
    fn bin_centred(k: usize) -> f32 {
        k as f32 * RATE as f32 / FFT_SIZE as f32
    }

    /// The test that catches the missing coherent-gain division: without it
    /// every reading is 6.02 dB low and nothing else looks wrong.
    #[test]
    fn a_full_scale_sine_reads_zero_dbfs() {
        let mut a = Analyzer::default();
        let k = 85;
        let out = a.analyze(&sine(bin_centred(k), 1.0, FFT_SIZE), RATE);
        assert!(
            (out.bins[k] - 0.0).abs() < 0.1,
            "full-scale sine read {} dBFS, not 0.0",
            out.bins[k]
        );
    }

    #[test]
    fn a_one_kilohertz_tone_lands_in_bin_eighty_five() {
        let mut a = Analyzer::default();
        let out = a.analyze(&sine(1000.0, 1.0, FFT_SIZE), RATE);
        let (k, _) = out
            .bins
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|x, y| x.1.partial_cmp(y.1).unwrap())
            .unwrap();
        // 1000 / (48000/4096) = 85.3
        assert_eq!(k, 85, "1 kHz landed in bin {k}");
    }

    #[test]
    fn halving_the_amplitude_costs_six_decibels() {
        let mut a = Analyzer::default();
        let k = 85;
        let full = a.analyze(&sine(bin_centred(k), 1.0, FFT_SIZE), RATE).bins[k];
        let half = a.analyze(&sine(bin_centred(k), 0.5, FFT_SIZE), RATE).bins[k];
        assert!((full - half - 6.02).abs() < 0.05, "{full} vs {half}");
    }

    #[test]
    fn dc_lands_in_bin_zero_and_is_not_doubled() {
        let mut a = Analyzer::default();
        let out = a.analyze(&vec![1.0f32; FFT_SIZE], RATE);
        // A DC level of 1.0 is full scale, and bin 0 must not be doubled.
        assert!((out.bins[0] - 0.0).abs() < 0.1, "DC read {} dBFS", out.bins[0]);

        // Bin 1 is *also* at full scale, and that is correct rather than
        // leakage: Hann's main lobe is three bins wide, and its kernel is
        // (-0.25, 0.5, -0.25). Bin 1 is half the amplitude of bin 0 before
        // single-sided doubling and equal to it after. The first bin that
        // should be down in the sidelobes is bin 2.
        assert!(out.bins[2] < -30.0, "DC spread past its main lobe: bin 2 is {} dBFS", out.bins[2]);
    }

    #[test]
    fn nyquist_is_not_doubled_either() {
        let mut a = Analyzer::default();
        // Alternating +/-1 is exactly Nyquist at full scale.
        let s: Vec<f32> = (0..FFT_SIZE).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        let out = a.analyze(&s, RATE);
        let last = out.bins.len() - 1;
        assert!((out.bins[last] - 0.0).abs() < 0.1, "Nyquist read {} dBFS", out.bins[last]);
    }

    #[test]
    fn silence_reads_the_floor_everywhere_and_produces_no_nans() {
        let mut a = Analyzer::default();
        let out = a.analyze(&vec![0.0f32; FFT_SIZE], RATE);
        assert!(out.bins.iter().all(|v| v.is_finite()), "a non-finite bin escaped");
        assert!(out.bins.iter().all(|v| *v <= FLOOR_DBFS + 1e-3), "silence is not at the floor");
        assert!(out.peak().is_none(), "silence reported a peak");
    }

    /// Parseval: the energy in the bins equals the energy in the windowed
    /// samples. This is the check that catches a wrong scale factor that the
    /// single-tone tests happen to agree with.
    #[test]
    fn energy_is_conserved() {
        let mut a = Analyzer::default();
        // Two tones and a DC term, so no single special case dominates.
        let s: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                0.4 * (2.0 * PI * 997.0 * t).sin() + 0.2 * (2.0 * PI * 5000.0 * t).sin()
            })
            .collect();
        let w = Window::default();
        let time: f32 = s.iter().zip(w.w.iter()).map(|(v, ww)| (v * ww).powi(2)).sum();

        let out = a.analyze(&s, RATE);
        // Undo the normalisation to recover |X[k]|^2, then apply Parseval.
        let n = FFT_SIZE as f32;
        let freq: f32 = out
            .bins
            .iter()
            .enumerate()
            .map(|(k, db)| {
                let amp = 10f32.powf(db / 20.0);
                let mag = if k == 0 || k == FFT_SIZE / 2 {
                    amp * n * w.coherent_gain
                } else {
                    amp * n * w.coherent_gain / 2.0
                };
                let p = mag * mag / n;
                if k == 0 || k == FFT_SIZE / 2 { p } else { 2.0 * p }
            })
            .sum();
        let ratio = freq / time;
        assert!((ratio - 1.0).abs() < 0.02, "energy ratio {ratio}, expected 1.0");
    }

    /// The distinction the hum detector depends on: 50 Hz and 60 Hz are less
    /// than one bin apart at this resolution, so the bin index cannot tell them
    /// apart and the interpolated frequency must.
    #[test]
    fn peak_interpolation_separates_fifty_from_sixty_hertz() {
        let mut a = Analyzer::default();
        for hz in [50.0f32, 60.0] {
            let out = a.analyze(&sine(hz, 0.5, FFT_SIZE), RATE);
            let (f, _) = out.peak().expect("a tone has a peak");
            assert!((f - hz).abs() < 1.5, "{hz} Hz was located at {f} Hz");
        }
        // And confirm the bin index alone genuinely could not have done it.
        let bin_of = |hz: f32| (hz * FFT_SIZE as f32 / RATE as f32).round() as usize;
        assert!(
            bin_of(60.0) - bin_of(50.0) <= 1,
            "the bins are far enough apart that this test proves nothing"
        );
    }

    #[test]
    fn peak_interpolation_recovers_an_off_centre_tone() {
        let mut a = Analyzer::default();
        let out = a.analyze(&sine(1002.5, 0.7, FFT_SIZE), RATE);
        let (f, db) = out.peak().expect("a tone has a peak");
        assert!((f - 1002.5).abs() < 1.0, "peak located at {f} Hz");
        // 0.7 amplitude is -3.1 dBFS; scalloping may cost up to 1.42 dB.
        assert!((-3.1 - db).abs() < 1.5, "peak read {db} dBFS");
    }

    #[test]
    fn the_transform_inverts_a_known_impulse() {
        // An impulse at t=0 has a flat magnitude spectrum. Cheap, and it fails
        // loudly if the bit-reversal permutation is wrong.
        let mut re = vec![0.0f32; 16];
        let mut im = vec![0.0f32; 16];
        re[0] = 1.0;
        fft(&mut re, &mut im);
        for k in 0..16 {
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt();
            assert!((mag - 1.0).abs() < 1e-5, "bin {k} of an impulse is {mag}");
        }
    }

    /// Every offered size has to transform correctly, not just the default.
    #[test]
    fn every_configurable_size_reads_a_full_scale_sine_correctly() {
        for size in crate::config::FFT_SIZES {
            let mut a = Analyzer::new(size, FLOOR_DBFS);
            assert_eq!(a.size(), size);
            let k = size / 16;
            let hz = k as f32 * RATE as f32 / size as f32;
            let s: Vec<f32> =
                (0..size).map(|i| (2.0 * PI * hz * i as f32 / RATE as f32).sin()).collect();
            let out = a.analyze(&s, RATE);
            assert_eq!(out.bins.len(), size / 2 + 1);
            assert!((out.bins[k]).abs() < 0.15, "size {size}: read {} dBFS", out.bins[k]);
            let (f, _) = out.peak().expect("a tone has a peak");
            assert!((f - hz).abs() < 2.0, "size {size}: peak at {f}, expected {hz}");
        }
    }

    #[test]
    fn a_nonsense_size_falls_back_rather_than_panicking() {
        // A radix-2 transform of 4095 points is not a transform.
        assert_eq!(Analyzer::new(4095, FLOOR_DBFS).size(), FFT_SIZE);
        assert_eq!(Analyzer::new(0, FLOOR_DBFS).size(), FFT_SIZE);
        assert_eq!(Analyzer::new(FFT_SIZE, 12.0).floor(), FLOOR_DBFS);
    }

    #[test]
    fn a_lower_floor_shows_more_of_the_noise() {
        let quiet: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 1e-5 * (2.0 * PI * 1000.0 * i as f32 / RATE as f32).sin())
            .collect();
        let shallow = Analyzer::new(FFT_SIZE, -60.0).analyze(&quiet, RATE);
        let deep = Analyzer::new(FFT_SIZE, -120.0).analyze(&quiet, RATE);
        assert_eq!(shallow.bins[85], -60.0, "a -100 dBFS tone should be clamped at a -60 floor");
        assert!(deep.bins[85] < -80.0, "a lower floor did not reveal the tone: {}", deep.bins[85]);
    }

    #[test]
    fn the_window_is_hann_with_the_expected_gain() {
        let w = Window::hann(1024);
        assert!((w.coherent_gain - 0.5).abs() < 1e-3, "coherent gain {}", w.coherent_gain);
        assert_eq!(w.w.len(), 1024);
        assert!(w.w[0].abs() < 1e-6, "Hann does not start at zero");
        assert!((w.w[512] - 1.0).abs() < 1e-3, "Hann does not peak at 1.0");
    }
}
