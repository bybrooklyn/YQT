// SPDX-License-Identifier: GPL-3.0-only
//! Small signal-analysis helpers used by the tests and by the EQ sweep report (no runtime use).
use std::f64::consts::PI;

/// Least-squares fit of `a*cos(wt) + b*sin(wt)` to `x` (mono). Returns `(amplitude, snr_db)` where the SNR compares
/// the fitted tone with everything else in the signal. A clean tone has a high SNR; wrong or noisy audio does not.
pub fn tone_fit(x: &[f32], rate: f64, freq: f64) -> (f64, f64) {
    let w = 2.0 * PI * freq / rate;
    let (mut cc, mut ss, mut cs, mut xc, mut xs, mut xx) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for (i, &v) in x.iter().enumerate() {
        let v = f64::from(v);
        let (s, c) = (w * i as f64).sin_cos();
        cc += c * c;
        ss += s * s;
        cs += c * s;
        xc += v * c;
        xs += v * s;
        xx += v * v;
    }
    let det = cc * ss - cs * cs;
    if det.abs() < 1e-12 {
        return (0.0, f64::NEG_INFINITY);
    }
    let a = (xc * ss - xs * cs) / det;
    let b = (xs * cc - xc * cs) / det;
    let fitted = a * xc + b * xs; // energy explained by the tone
    let resid = (xx - fitted).max(1e-30);
    ((a * a + b * b).sqrt(), 10.0 * (fitted.max(1e-30) / resid).log10())
}

/// Goertzel power (amplitude^2 / 4 scaling not applied) at `freq`, normalised by the window length.
pub fn goertzel_amp(x: &[f32], rate: f64, freq: f64) -> f64 {
    let w = 2.0 * PI * freq / rate;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s0 = f64::from(v) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    2.0 * power.max(0.0).sqrt() / x.len().max(1) as f64
}

pub fn rms(x: &[f32]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

/// Left channel of an interleaved stereo (or the only channel of a mono) buffer.
pub fn channel(x: &[f32], channels: usize, ch: usize) -> Vec<f32> {
    x.chunks_exact(channels).map(|f| f[ch]).collect()
}

pub fn db(ratio: f64) -> f64 {
    20.0 * ratio.max(1e-12).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f64, rate: f64, n: usize, amp: f64) -> Vec<f32> {
        (0..n).map(|i| (amp * (2.0 * PI * f * i as f64 / rate).sin()) as f32).collect()
    }

    #[test]
    fn fit_finds_amplitude_and_high_snr_for_a_pure_tone() {
        let x = sine(440.0, 48_000.0, 48_000, 0.25);
        let (a, snr) = tone_fit(&x, 48_000.0, 440.0);
        assert!((a - 0.25).abs() < 1e-4 && snr > 80.0, "{a} {snr}");
        let (_, wrong) = tone_fit(&x, 48_000.0, 523.0);
        assert!(wrong < 10.0, "a wrong frequency must not fit: {wrong}");
    }

    #[test]
    fn goertzel_matches_amplitude() {
        let x = sine(1000.0, 48_000.0, 48_000, 0.5);
        assert!((goertzel_amp(&x, 48_000.0, 1000.0) - 0.5).abs() < 1e-3);
        assert!(goertzel_amp(&x, 48_000.0, 3000.0) < 1e-3);
    }
}
