// SPDX-License-Identifier: GPL-3.0-only
//! A smooth playback clock.
//!
//! Players report their position in coarse steps (often ~1 Hz or on buffer boundaries), which makes a progress bar
//! and synced lyrics visibly tick. `ClockState` extrapolates between reports using wall-clock time, and corrects
//! itself gently when the next report arrives, so the position advances smoothly every frame. The rule is pure
//! and unit-tested; `PlaybackClock` is a thin Qt wrapper QML calls once per frame.
//! The audio engine (when present) reports sample-accurate positions through the very same rule.
use core::pin::Pin;
use cxx_qt::CxxQtType;

/// A report further than this (ms) from the prediction is a seek/glitch, not jitter: jump straight to it.
pub const RESYNC_MS: f64 = 250.0;
/// Fraction of a small prediction error corrected per report (the rest is absorbed gradually).
const SLEW: f64 = 0.2;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ClockState {
    base_ms: f64,
    base_at_ms: f64,
    playing: bool,
    valid: bool,
}

impl ClockState {
    /// Where playback is at wall-clock time `now_ms` (ms since any fixed epoch, same one for all calls).
    pub fn position_at(&self, now_ms: f64) -> f64 {
        if self.playing && self.valid {
            self.base_ms + (now_ms - self.base_at_ms).max(0.0)
        } else {
            self.base_ms
        }
    }

    /// Jump straight to `ms` (an explicit seek, or the first report).
    pub fn snap(&mut self, ms: f64, now_ms: f64) {
        self.base_ms = ms.max(0.0);
        self.base_at_ms = now_ms;
        self.valid = true;
    }

    /// The player reported `reported_ms` at `now_ms`.
    pub fn report(&mut self, reported_ms: f64, now_ms: f64, playing: bool) {
        let was_running = self.valid && self.playing;
        if !was_running || !playing {
            // First report, resuming from a pause, or pausing: trust the player and (re)start from it.
            self.snap(reported_ms, now_ms);
            self.playing = playing;
            return;
        }
        let predicted = self.position_at(now_ms);
        let error = reported_ms - predicted;
        if error.abs() > RESYNC_MS {
            self.snap(reported_ms, now_ms); // seek or glitch
        } else {
            // Jitter: nudge toward the report, but never step backwards (the bar must not twitch).
            self.base_ms = (predicted + error * SLEW).max(predicted);
            self.base_at_ms = now_ms;
        }
        self.playing = playing;
    }
}

#[cxx_qt::bridge]
pub mod qobject {
    #[auto_cxx_name] // snake_case Rust names become camelCase in QML (positionAt)
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        type PlaybackClock = super::PlaybackClockRust;

        /// The player reported `reported_ms` at `now_ms` (e.g. `Date.now()`).
        #[qinvokable]
        fn report(self: Pin<&mut PlaybackClock>, reported_ms: f64, now_ms: f64, playing: bool);
        /// Jump to `ms` at once (an explicit seek).
        #[qinvokable]
        fn snap(self: Pin<&mut PlaybackClock>, ms: f64, now_ms: f64);
        /// The smoothed position (ms) at `now_ms`.
        #[qinvokable]
        fn position_at(self: &PlaybackClock, now_ms: f64) -> f64;
    }
}

#[derive(Default)]
pub struct PlaybackClockRust {
    state: ClockState,
}

impl qobject::PlaybackClock {
    fn report(self: Pin<&mut Self>, reported_ms: f64, now_ms: f64, playing: bool) {
        // SAFETY: only mutates the plain `state` field in place; the object is never moved out of its pin.
        let this = unsafe { self.rust_mut().get_unchecked_mut() };
        this.state.report(reported_ms, now_ms, playing);
    }

    fn snap(self: Pin<&mut Self>, ms: f64, now_ms: f64) {
        // SAFETY: as in `report`.
        let this = unsafe { self.rust_mut().get_unchecked_mut() };
        this.state.snap(ms, now_ms);
    }

    fn position_at(&self, now_ms: f64) -> f64 {
        self.rust().state.position_at(now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate a 60 fps UI: every frame (≈16.7 ms) ask for the position; the "player" reports once a second.
    fn run(report_every_ms: f64, jitter: impl Fn(u32) -> f64, seconds: f64) -> (Vec<f64>, ClockState) {
        let mut c = ClockState::default();
        let mut frames = Vec::new();
        let mut next_report = 0.0;
        let mut now = 1_000_000.0; // arbitrary epoch
        let start = now;
        let mut n = 0;
        while now - start < seconds * 1000.0 {
            if now - start >= next_report {
                let truth = now - start; // real playback position
                c.report(truth + jitter(n), now, true);
                next_report += report_every_ms;
                n += 1;
            }
            frames.push(c.position_at(now));
            now += 1000.0 / 60.0;
        }
        (frames, c)
    }

    #[test]
    fn advances_smoothly_between_coarse_reports() {
        // The player only reports once a second; the clock must still move every single frame.
        let (frames, _) = run(1000.0, |_| 0.0, 5.0);
        let mut max_step = 0.0f64;
        for w in frames.windows(2) {
            assert!(w[1] > w[0], "position must advance every frame, got {} then {}", w[0], w[1]);
            max_step = max_step.max(w[1] - w[0]);
        }
        assert!(max_step < 20.0, "largest per-frame step {max_step} ms is not smooth (frame = 16.7 ms)");
    }

    #[test]
    fn never_goes_backwards_under_report_jitter() {
        // reports wobble ±60 ms around the truth
        let (frames, _) = run(500.0, |n| if n % 2 == 0 { 60.0 } else { -60.0 }, 10.0);
        for w in frames.windows(2) {
            assert!(w[1] >= w[0], "went backwards: {} -> {}", w[0], w[1]);
        }
    }

    #[test]
    fn stays_close_to_the_truth_despite_jitter() {
        let (frames, _) = run(500.0, |n| if n % 2 == 0 { 40.0 } else { -40.0 }, 10.0);
        let truth_at_end = (frames.len() as f64 - 1.0) * 1000.0 / 60.0;
        let drift = (frames.last().unwrap() - truth_at_end).abs();
        assert!(drift < 120.0, "drifted {drift} ms from the truth");
    }

    #[test]
    fn a_big_error_is_a_seek_and_snaps_immediately() {
        let mut c = ClockState::default();
        c.report(10_000.0, 0.0, true);
        assert_eq!(c.position_at(1000.0), 11_000.0);
        // the player jumps to 60 s (user seeked)
        c.report(60_000.0, 1000.0, true);
        assert_eq!(c.position_at(1000.0), 60_000.0);
        assert_eq!(c.position_at(1500.0), 60_500.0);
    }

    #[test]
    fn small_error_does_not_snap() {
        let mut c = ClockState::default();
        c.report(1000.0, 0.0, true);
        // at t=1000 the clock predicts 2000 but the player says 1130: a -870 ms error is a seek, so it snaps
        c.report(1130.0, 1000.0, true);
        assert_eq!(c.position_at(1000.0), 1130.0);
        // a genuinely small error: at t=2000 the clock predicts 2130 and the player says 2200 (+70 ms)
        c.report(2200.0, 2000.0, true);
        let p = c.position_at(2000.0);
        assert!(p > 2130.0 && p < 2200.0, "should move toward the report without jumping to it: {p}");
    }

    #[test]
    fn pausing_freezes_the_clock_and_resuming_restarts_from_the_report() {
        let mut c = ClockState::default();
        c.report(5000.0, 0.0, true);
        c.report(6000.0, 1000.0, false);
        assert_eq!(c.position_at(1000.0), 6000.0);
        assert_eq!(c.position_at(9000.0), 6000.0, "paused: must not advance");
        c.report(6000.0, 9000.0, true);
        assert_eq!(c.position_at(9500.0), 6500.0);
    }

    #[test]
    fn explicit_snap_wins_and_negative_input_is_clamped() {
        let mut c = ClockState::default();
        c.report(5000.0, 0.0, true);
        c.snap(42_000.0, 100.0);
        assert_eq!(c.position_at(100.0), 42_000.0);
        c.snap(-5.0, 200.0);
        assert_eq!(c.position_at(200.0), 0.0);
    }

    #[test]
    fn unstarted_clock_reads_zero_and_time_going_backwards_is_harmless() {
        let c = ClockState::default();
        assert_eq!(c.position_at(123.0), 0.0);
        let mut c = ClockState::default();
        c.report(1000.0, 5000.0, true);
        assert_eq!(c.position_at(4000.0), 1000.0, "a clock reading earlier than the base must not go negative");
    }
}
