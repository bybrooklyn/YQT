// SPDX-License-Identifier: GPL-3.0-only
//! The 10-band graphic equalizer: RBJ peaking filters (via the `biquad` crate) at 31 Hz ... 16 kHz, a preamp,
//! click-free changes (the old and new chain are cross-faded) and a frequency-response function for the UI.
use biquad::{Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type};
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

pub const BANDS: usize = 10;
/// Centre frequencies (Hz) of the ten bands.
pub const FREQS: [f64; BANDS] = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
/// Bandwidth of every band (about one octave).
pub const Q: f64 = 1.41;
pub const MAX_DB: f64 = 12.0;
/// Seconds over which a change of the chain is cross-faded.
const FADE_SECS: f64 = 0.02;
/// A band closer than this to 0 dB is skipped entirely (keeps a flat EQ bit-transparent).
const EPS_DB: f64 = 1e-3;

#[derive(Debug, Clone, PartialEq)]
pub struct EqParams {
    pub enabled: bool,
    pub preamp_db: f64,
    pub bands_db: [f64; BANDS],
    /// Soft limiter after the EQ (guards against clipping from boosted bands).
    pub limiter: bool,
}

impl Default for EqParams {
    fn default() -> Self {
        Self { enabled: false, preamp_db: 0.0, bands_db: [0.0; BANDS], limiter: true }
    }
}

impl EqParams {
    /// Clamp everything into its legal range (NaN becomes 0).
    pub fn sanitized(mut self) -> Self {
        let c = |v: f64| if v.is_finite() { v.clamp(-MAX_DB, MAX_DB) } else { 0.0 };
        self.preamp_db = c(self.preamp_db);
        for b in &mut self.bands_db {
            *b = c(*b);
        }
        self
    }

    /// From a list of gains of any length (extra values ignored, missing ones flat).
    pub fn bands_from_slice(v: &[f64]) -> [f64; BANDS] {
        let mut out = [0.0; BANDS];
        for (o, i) in out.iter_mut().zip(v) {
            *o = if i.is_finite() { i.clamp(-MAX_DB, MAX_DB) } else { 0.0 };
        }
        out
    }

    /// True when processing would change nothing at all.
    pub fn is_identity(&self) -> bool {
        !self.enabled || (self.preamp_db.abs() < EPS_DB && self.bands_db.iter().all(|b| b.abs() < EPS_DB))
    }
}

/// RBJ cookbook peaking-EQ coefficients (normalised, a0 = 1).
pub fn peaking(fs: f64, f0: f64, gain_db: f64) -> Option<Coefficients<f64>> {
    Coefficients::<f64>::from_params(Type::PeakingEQ(gain_db), fs.hz(), f0.hz(), Q).ok()
}

/// |H(e^jw)| in dB of a biquad at `f` Hz.
pub fn magnitude_db(c: &Coefficients<f64>, f: f64, fs: f64) -> f64 {
    let w = 2.0 * PI * f / fs;
    let (c1, s1) = (w.cos(), w.sin());
    let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
    let num_re = c.b0 + c.b1 * c1 + c.b2 * c2;
    let num_im = -(c.b1 * s1 + c.b2 * s2);
    let den_re = 1.0 + c.a1 * c1 + c.a2 * c2;
    let den_im = -(c.a1 * s1 + c.a2 * s2);
    10.0 * ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).max(1e-30).log10()
}

/// The combined response (dB) of preamp + bands at each of `freqs`, for drawing the curve. A disabled EQ is flat.
pub fn response_db(p: &EqParams, freqs: &[f64], fs: f64) -> Vec<f64> {
    let coeffs: Vec<Option<Coefficients<f64>>> = (0..BANDS)
        .map(|i| if p.bands_db[i].abs() < EPS_DB || FREQS[i] >= fs * 0.45 { None } else { peaking(fs, FREQS[i], p.bands_db[i]) })
        .collect();
    freqs
        .iter()
        .map(|&f| {
            if !p.enabled {
                return 0.0;
            }
            p.preamp_db + coeffs.iter().flatten().map(|c| magnitude_db(c, f.clamp(1.0, fs * 0.499), fs)).sum::<f64>()
        })
        .collect()
}

type Filter = DirectForm2Transposed<f64>;

/// One configuration of the chain with its filter memories (stereo).
#[derive(Clone)]
struct Chain {
    gain: f64,
    bands: [Option<[Filter; 2]>; BANDS],
}

impl Chain {
    fn identity() -> Self {
        Self { gain: 1.0, bands: Default::default() }
    }

    fn is_identity(&self) -> bool {
        self.gain == 1.0 && self.bands.iter().all(Option::is_none)
    }

    /// Build the chain for `p`, inheriting filter memory from `prev` for bands that stay active (no discontinuity).
    fn build(p: &EqParams, fs: f64, prev: &Chain) -> Self {
        if p.is_identity() {
            return Self::identity();
        }
        let mut bands: [Option<[Filter; 2]>; BANDS] = Default::default();
        for i in 0..BANDS {
            if p.bands_db[i].abs() < EPS_DB || FREQS[i] >= fs * 0.45 {
                continue;
            }
            let Some(c) = peaking(fs, FREQS[i], p.bands_db[i]) else { continue };
            bands[i] = Some(match &prev.bands[i] {
                Some(old) => {
                    let mut f = *old;
                    f[0].update_coefficients(c);
                    f[1].update_coefficients(c);
                    f
                }
                None => [Filter::new(c), Filter::new(c)],
            });
        }
        Self { gain: 10f64.powf(p.preamp_db / 20.0), bands }
    }

    #[inline]
    fn frame(&mut self, l: f64, r: f64) -> (f64, f64) {
        let (mut l, mut r) = (l * self.gain, r * self.gain);
        for b in self.bands.iter_mut().flatten() {
            l = b[0].run(l);
            r = b[1].run(r);
        }
        (l, r)
    }
}

/// Real-time stereo equalizer. Call `set_params` whenever the settings change and `process` on interleaved stereo.
pub struct EqProcessor {
    fs: f64,
    params: EqParams,
    cur: Chain,
    /// The chain we are fading away from, and how many frames of the fade are left.
    old: Option<Chain>,
    fade_left: usize,
    fade_len: usize,
}

impl EqProcessor {
    pub fn new(fs: f64) -> Self {
        Self {
            fs,
            params: EqParams::default(),
            cur: Chain::identity(),
            old: None,
            fade_left: 0,
            fade_len: ((fs * FADE_SECS) as usize).max(1),
        }
    }

    pub fn params(&self) -> &EqParams {
        &self.params
    }

    pub fn set_params(&mut self, p: &EqParams) {
        if *p == self.params {
            return;
        }
        let next = Chain::build(p, self.fs, &self.cur);
        // Keep the previous sound alive for the cross-fade (unless both are the identity: nothing to hide).
        if !(self.cur.is_identity() && next.is_identity()) {
            let prev = std::mem::replace(&mut self.cur, next);
            // If a fade was running, the older chain is dropped: the newest transition wins.
            self.old = Some(prev);
            self.fade_left = self.fade_len;
        } else {
            self.cur = next;
        }
        self.params = p.clone();
    }

    pub fn is_settled_identity(&self) -> bool {
        self.old.is_none() && self.cur.is_identity()
    }

    /// Process interleaved stereo `f32` in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        if self.is_settled_identity() {
            return; // exact bit-transparency
        }
        for fr in buf.as_chunks_mut::<2>().0 {
            let (l, r) = (f64::from(fr[0]), f64::from(fr[1]));
            let (mut ol, mut or) = self.cur.frame(l, r);
            if let Some(old) = self.old.as_mut() {
                let w = 1.0 - self.fade_left as f64 / self.fade_len as f64; // 0 -> 1
                let (pl, pr) = old.frame(l, r);
                ol = ol * w + pl * (1.0 - w);
                or = or * w + pr * (1.0 - w);
                self.fade_left -= 1;
                if self.fade_left == 0 {
                    self.old = None;
                }
            }
            fr[0] = ol as f32;
            fr[1] = or as f32;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// shared live settings (the Equalizer QObject writes, the audio callback reads)
// ---------------------------------------------------------------------------------------------

pub struct EqShared {
    params: RwLock<EqParams>,
    version: AtomicU64,
}

impl EqShared {
    pub fn new(p: EqParams) -> Self {
        Self { params: RwLock::new(p.sanitized()), version: AtomicU64::new(1) }
    }
    pub fn set(&self, p: EqParams) {
        if let Ok(mut g) = self.params.write() {
            *g = p.sanitized();
        }
        self.version.fetch_add(1, Ordering::Release);
    }
    pub fn get(&self) -> EqParams {
        self.params.read().map(|g| g.clone()).unwrap_or_default()
    }
    /// For the output callback: the settings unless they are being written right now (`None`); never blocks.
    pub fn try_get(&self) -> Option<EqParams> {
        match self.params.try_read() {
            Ok(g) => Some(g.clone()),
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner().clone()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }
}

/// The process-wide settings the engine follows.
pub fn shared() -> &'static std::sync::Arc<EqShared> {
    static S: OnceLock<std::sync::Arc<EqShared>> = OnceLock::new();
    S.get_or_init(|| std::sync::Arc::new(EqShared::new(EqParams::default())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::analysis::goertzel_amp;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// Reference values computed independently with the RBJ cookbook formulas (awk, double precision):
    /// A = 10^(dB/40), w0 = 2*pi*f0/fs, alpha = sin(w0)/(2Q), normalised by a0 = 1 + alpha/A.
    #[test]
    fn coefficients_match_the_rbj_cookbook_reference_values() {
        let c = peaking(48_000.0, 1000.0, 6.0).unwrap();
        assert!(close(c.b0, 1.031577910617), "{}", c.b0);
        assert!(close(c.b1, -1.919976143598));
        assert!(close(c.b2, 0.904965631439));
        assert!(close(c.a1, -1.919976143598));
        assert!(close(c.a2, 0.936543542056));
        let c = peaking(44_100.0, 31.25, -9.0).unwrap();
        assert!(close(c.b0, 0.998294402721) && close(c.b1, -1.994693084772) && close(c.b2, 0.996418453217));
        assert!(close(c.a1, -1.994693084772) && close(c.a2, 0.994712855938));
        let c = peaking(48_000.0, 16_000.0, 12.0).unwrap();
        assert!(close(c.b0, 1.397630830331) && close(c.b1, 0.866614805141) && close(c.b2, 0.335598779951));
        assert!(close(c.a1, 0.866614805141) && close(c.a2, 0.733229610282));
    }

    #[test]
    fn centre_gain_equals_the_setting() {
        for (i, f) in FREQS.iter().enumerate() {
            for g in [-12.0, -6.0, 3.0, 12.0] {
                let c = peaking(48_000.0, *f, g).unwrap();
                assert!((magnitude_db(&c, *f, 48_000.0) - g).abs() < 1e-6, "band {i} at {g} dB");
            }
        }
    }

    fn stereo_sine(f: f64, fs: f64, n: usize, amp: f64) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                let v = (amp * (2.0 * PI * f * i as f64 / fs).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn flat_eq_is_bit_transparent() {
        let x = stereo_sine(440.0, 48_000.0, 4800, 0.7);
        for enabled in [false, true] {
            let mut eq = EqProcessor::new(48_000.0);
            eq.set_params(&EqParams { enabled, ..Default::default() });
            let mut y = x.clone();
            eq.process(&mut y);
            assert_eq!(x, y, "enabled={enabled}");
        }
        // every band at ~0 with the EQ on is the same thing
        let mut eq = EqProcessor::new(48_000.0);
        eq.set_params(&EqParams { enabled: true, bands_db: [0.0004; BANDS], ..Default::default() });
        let mut y = x.clone();
        eq.process(&mut y);
        assert_eq!(x, y);
    }

    /// Measure the gain (dB) the running processor applies to a sine at `f`.
    fn measured_db(p: &EqParams, f: f64, fs: f64) -> f64 {
        let mut eq = EqProcessor::new(fs);
        eq.set_params(p);
        let mut buf = stereo_sine(f, fs, 48_000, 0.1);
        let input = buf.clone();
        eq.process(&mut buf);
        let skip = 24_000 * 2; // past the fade-in
        let l_out: Vec<f32> = buf[skip..].chunks(2).map(|c| c[0]).collect();
        let l_in: Vec<f32> = input[skip..].chunks(2).map(|c| c[0]).collect();
        20.0 * (goertzel_amp(&l_out, fs, f) / goertzel_amp(&l_in, fs, f)).log10()
    }

    #[test]
    fn each_band_sweep_hits_its_setting_within_half_a_db() {
        for i in 0..BANDS {
            for g in [-12.0, 6.0, 12.0] {
                let mut bands = [0.0; BANDS];
                bands[i] = g;
                let p = EqParams { enabled: true, bands_db: bands, ..Default::default() };
                let m = measured_db(&p, FREQS[i], 48_000.0);
                assert!((m - g).abs() <= 0.5, "band {i} ({} Hz) set {g} dB, measured {m:.3} dB", FREQS[i]);
            }
        }
    }

    #[test]
    fn preamp_scales_everything() {
        let p = EqParams { enabled: true, preamp_db: -6.0, ..Default::default() };
        let m = measured_db(&p, 1000.0, 48_000.0);
        assert!((m + 6.0).abs() < 0.05, "{m}");
    }

    #[test]
    fn response_curve_matches_measurement() {
        let p = EqParams { enabled: true, preamp_db: -2.0, bands_db: [3.0, 0.0, -4.0, 0.0, 5.0, 0.0, 0.0, -6.0, 0.0, 8.0], limiter: true };
        let freqs = [40.0, 150.0, 500.0, 3000.0, 9000.0];
        let curve = response_db(&p, &freqs, 48_000.0);
        for (f, c) in freqs.iter().zip(&curve) {
            let m = measured_db(&p, *f, 48_000.0);
            assert!((m - c).abs() < 0.2, "{f} Hz: curve {c:.2} vs measured {m:.2}");
        }
        assert!(response_db(&EqParams::default(), &freqs, 48_000.0).iter().all(|&v| v == 0.0), "disabled = flat curve");
    }

    #[test]
    fn bypass_crossfade_has_no_discontinuity() {
        let fs = 48_000.0;
        let x = stereo_sine(1000.0, fs, 96_000, 0.5);
        let mut eq = EqProcessor::new(fs);
        let on = EqParams { enabled: true, bands_db: [6.0; BANDS], ..Default::default() };
        eq.set_params(&on);
        let mut y = x.clone();
        let (a, b) = y.split_at_mut(2 * 48_000);
        eq.process(a);
        eq.set_params(&EqParams { enabled: false, ..on.clone() }); // bypass in the middle of the signal
        eq.process(b);
        // largest sample-to-sample step on the left channel across the whole render vs. the natural step of the sine
        let l: Vec<f32> = y.chunks(2).map(|c| c[0]).collect();
        let max_step = l.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        let natural = (2.0 * PI * 1000.0 / fs * 0.5 * 2.0) as f32; // boosted +12 dB worst case would be 4x; we boost 6 dB (2x) in 6 bands
        assert!(max_step < natural * 2.5, "step {max_step} vs natural {natural}: a click at the switch");
        // and a hard (non-faded) switch really would have clicked, so the test can fail
        let hard: Vec<f32> = {
            let mut e = EqProcessor::new(fs);
            e.set_params(&on);
            let mut a = x[..2 * 48_000].to_vec();
            e.process(&mut a);
            let tail = x[2 * 48_000..].to_vec();
            a.extend(tail);
            a.chunks(2).map(|c| c[0]).collect()
        };
        let hard_step = hard.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(hard_step > max_step * 1.2, "a hard switch ({hard_step}) should be worse than the cross-fade ({max_step})");
    }

    #[test]
    fn changing_a_band_while_playing_does_not_click() {
        let fs = 48_000.0;
        let x = stereo_sine(500.0, fs, 48_000, 0.3);
        let mut eq = EqProcessor::new(fs);
        let mut p = EqParams { enabled: true, ..Default::default() };
        p.bands_db[4] = 3.0;
        eq.set_params(&p);
        let mut y = x.clone();
        let (a, b) = y.split_at_mut(48_000);
        eq.process(a);
        p.bands_db[4] = 12.0;
        eq.set_params(&p);
        eq.process(b);
        let l: Vec<f32> = y.chunks(2).map(|c| c[0]).collect();
        let max_step = l.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        let natural = (2.0 * PI * 500.0 / fs * 0.3 * 4.2) as f32; // +12 dB = x3.98
        assert!(max_step < natural * 1.3, "{max_step} vs {natural}");
    }

    #[test]
    fn sanitizing_clamps_and_cleans() {
        let p = EqParams {
            enabled: true,
            preamp_db: 99.0,
            bands_db: [f64::NAN, -40.0, 40.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            limiter: true,
        }
        .sanitized();
        assert_eq!((p.preamp_db, p.bands_db[0], p.bands_db[1], p.bands_db[2]), (12.0, 0.0, -12.0, 12.0));
        assert_eq!(EqParams::bands_from_slice(&[1.0, 2.0])[..3], [1.0, 2.0, 0.0]);
        assert_eq!(EqParams::bands_from_slice(&[1.0; 20]).len(), 10);
    }

    #[test]
    fn low_sample_rates_skip_bands_that_do_not_fit() {
        let mut eq = EqProcessor::new(16_000.0);
        eq.set_params(&EqParams { enabled: true, bands_db: [6.0; BANDS], ..Default::default() });
        let mut buf = stereo_sine(1000.0, 16_000.0, 2000, 0.3);
        eq.process(&mut buf);
        assert!(buf.iter().all(|v| v.is_finite()));
    }

    /// Dev report: renders a 20 Hz - 20 kHz log sweep through the EQ ("Rock" preset) into a WAV and prints the
    /// measured gain at every band centre next to the setting and the analytic curve.
    /// `YTM_SWEEP_OUT=<dir> cargo test -- --ignored --nocapture dev_eq_sweep_report`
    #[test]
    #[ignore]
    fn dev_eq_sweep_report() {
        let dir = std::path::PathBuf::from(std::env::var_os("YTM_SWEEP_OUT").expect("set YTM_SWEEP_OUT to a directory"));
        let fs = 48_000.0;
        let rock = crate::audio::presets::builtin("rock").unwrap();
        let p = EqParams { enabled: true, preamp_db: rock.preamp_db, bands_db: rock.bands_db, limiter: true };
        // the sweep
        let secs = 12.0;
        let (f0, f1) = (20.0f64, 20_000.0f64);
        let n = (fs * secs) as usize;
        let k = (f1 / f0).ln();
        let sweep: Vec<f32> = (0..n)
            .flat_map(|i| {
                let t = i as f64 / fs;
                let phase = 2.0 * PI * f0 * secs / k * ((t / secs * k).exp() - 1.0);
                let v = (0.25 * phase.sin()) as f32;
                [v, v]
            })
            .collect();
        let mut out = sweep.clone();
        let mut eq = EqProcessor::new(fs);
        eq.set_params(&p);
        eq.process(&mut out);
        crate::audio::output::write_wav(&dir.join("sweep_flat.wav"), 48_000, &sweep).unwrap();
        crate::audio::output::write_wav(&dir.join("sweep_rock.wav"), 48_000, &out).unwrap();
        println!("wrote {}/sweep_flat.wav and sweep_rock.wav (Rock: preamp {} dB, bands {:?})", dir.display(), p.preamp_db, p.bands_db);
        println!("band     set(dB)  measured(dB)  analytic(dB)   single-band +9 dB test (measured)");
        let analytic = response_db(&p, &FREQS, fs);
        for i in 0..BANDS {
            let m = measured_db(&p, FREQS[i], fs);
            let mut one = [0.0; BANDS];
            one[i] = 9.0;
            let single = measured_db(&EqParams { enabled: true, bands_db: one, ..Default::default() }, FREQS[i], fs);
            println!(
                "{:>6} Hz  {:+6.1}   {:+8.2}      {:+8.2}        {:+7.2}",
                FREQS[i],
                p.bands_db[i] + p.preamp_db,
                m,
                analytic[i],
                single
            );
        }
    }
}
