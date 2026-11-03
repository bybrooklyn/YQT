// SPDX-License-Identifier: GPL-3.0-only
//! A soft limiter that keeps boosted audio from clipping. Samples below the knee pass untouched (bit-transparent);
//! above it a tanh curve eases toward full scale and never reaches beyond it.

/// Start of the soft region (linear). -1.4 dBFS: ordinary music stays below it.
pub const KNEE: f32 = 0.85;

#[inline]
pub fn soft_limit(x: f32) -> f32 {
    if x.is_nan() {
        return 0.0;
    }
    let a = x.abs();
    if a <= KNEE {
        return x;
    }
    let room = 1.0 - KNEE;
    let y = (KNEE + room * ((a - KNEE) / room).tanh()).min(1.0);
    y.copysign(x)
}

/// Limit a buffer in place.
pub fn process(buf: &mut [f32]) {
    for s in buf {
        *s = soft_limit(*s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_exceeds_full_scale() {
        for i in -2000..=2000 {
            let x = i as f32 * 0.01; // -20 .. +20
            let y = soft_limit(x);
            assert!(y.abs() <= 1.0, "{x} -> {y}");
        }
        for x in [f32::INFINITY, f32::NEG_INFINITY, f32::MAX, f32::MIN, 1e30, -1e30] {
            assert!(soft_limit(x).abs() <= 1.0, "{x}");
        }
        assert_eq!(soft_limit(f32::NAN), 0.0);
    }

    #[test]
    fn transparent_below_the_knee_and_monotonic_above() {
        for i in -850..=850 {
            let x = i as f32 / 1000.0;
            assert_eq!(soft_limit(x), x);
        }
        let mut last = soft_limit(KNEE);
        for i in 0..400 {
            let y = soft_limit(KNEE + i as f32 * 0.01);
            assert!(y >= last, "not monotonic at {i}");
            last = y;
        }
        assert!(soft_limit(1.0) > KNEE && soft_limit(1.0) < 1.0);
        assert_eq!(soft_limit(-1.5), -soft_limit(1.5), "symmetric");
    }

    #[test]
    fn continuous_at_the_knee() {
        let a = soft_limit(KNEE);
        let b = soft_limit(KNEE + 1e-6);
        assert!((a - b).abs() < 2e-6);
    }

    #[test]
    fn boosted_sine_through_the_chain_stays_in_range() {
        let mut buf: Vec<f32> = (0..4800).map(|i| 3.0 * (i as f32 * 0.05).sin()).collect();
        process(&mut buf);
        assert!(buf.iter().all(|s| s.abs() <= 1.0));
    }
}
