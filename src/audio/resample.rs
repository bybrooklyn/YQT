// SPDX-License-Identifier: GPL-3.0-only
//! Streaming sample-rate conversion to the device rate (rubato's FFT resampler), interleaved `f32` in and out.
//!
//! `push` takes any amount of input and appends whatever output is ready; `finish` flushes the tail so the total
//! output length is exactly `ceil(input_frames * out_rate / in_rate)` and the start delay is trimmed away.
use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};

const CHUNK: usize = 1024;

pub struct StreamResampler {
    inner: Option<Fft<f32>>, // None = same rate, pass through
    channels: usize,
    in_rate: u32,
    out_rate: u32,
    pending: Vec<f32>,
    out_buf: Vec<f32>,
    /// Output frames still to discard (the filter's start delay).
    trim: usize,
    in_frames: u64,
    out_frames: u64,
}

impl StreamResampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Result<Self, String> {
        let inner = if in_rate == out_rate {
            None
        } else {
            Some(
                Fft::<f32>::new(in_rate as usize, out_rate as usize, CHUNK, channels, FixedSync::Input)
                    .map_err(|e| format!("resampler: {e}"))?,
            )
        };
        let (trim, out_buf) = match &inner {
            Some(r) => (r.output_delay(), vec![0.0; r.output_frames_max() * channels]),
            None => (0, Vec::new()),
        };
        Ok(Self { inner, channels, in_rate, out_rate, pending: Vec::new(), out_buf, trim, in_frames: 0, out_frames: 0 })
    }

    pub fn is_passthrough(&self) -> bool {
        self.inner.is_none()
    }

    /// Output frames expected for `in_frames` input frames.
    pub fn expected_out(in_frames: u64, in_rate: u32, out_rate: u32) -> u64 {
        (in_frames * u64::from(out_rate)).div_ceil(u64::from(in_rate))
    }

    fn emit(&mut self, frames: usize, out: &mut Vec<f32>) {
        let skip = self.trim.min(frames);
        self.trim -= skip;
        let ch = self.channels;
        out.extend_from_slice(&self.out_buf[skip * ch..frames * ch]);
        self.out_frames += (frames - skip) as u64;
    }

    /// Feed interleaved input; appends ready output to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), String> {
        let Some(_) = &self.inner else {
            out.extend_from_slice(input);
            return Ok(());
        };
        self.in_frames += (input.len() / self.channels) as u64;
        self.pending.extend_from_slice(input);
        let ch = self.channels;
        loop {
            let need = self.inner.as_ref().map_or(0, |r| r.input_frames_next());
            if self.pending.len() < need * ch {
                return Ok(());
            }
            let (frames_in, frames_out) = self.run(need, None)?;
            self.pending.drain(..frames_in * ch);
            self.emit(frames_out, out);
        }
    }

    fn run(&mut self, need: usize, partial: Option<usize>) -> Result<(usize, usize), String> {
        let ch = self.channels;
        let r = self.inner.as_mut().expect("resampler");
        let in_cap = need;
        let out_cap = self.out_buf.len() / ch;
        let mut zero_pad;
        let input: &[f32] = if partial.is_some() {
            zero_pad = self.pending.clone();
            zero_pad.resize(in_cap * ch, 0.0);
            &zero_pad
        } else {
            &self.pending[..in_cap * ch]
        };
        let ia = InterleavedSlice::new(input, ch, in_cap).map_err(|e| format!("resampler buffer: {e}"))?;
        let mut oa = InterleavedSlice::new_mut(&mut self.out_buf, ch, out_cap).map_err(|e| format!("resampler buffer: {e}"))?;
        let idx = Indexing { input_offset: 0, output_offset: 0, partial_len: partial, active_channels_mask: None };
        r.process_into_buffer(&ia, &mut oa, Some(&idx)).map_err(|e| format!("resample: {e}"))
    }

    /// Flush the tail (call once at the end of a stream).
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), String> {
        if self.inner.is_none() {
            return Ok(());
        }
        let ch = self.channels;
        let want = Self::expected_out(self.in_frames, self.in_rate, self.out_rate);
        // remaining input (a partial chunk), then silence until the delayed tail has come out
        let mut guard = 0;
        let mut first = true;
        while self.out_frames < want && guard < 64 {
            guard += 1;
            let need = self.inner.as_ref().map_or(0, |r| r.input_frames_next());
            let have = self.pending.len() / ch;
            let partial = if first { have.min(need) } else { 0 };
            first = false;
            let (_, frames_out) = self.run(need, Some(partial))?;
            self.pending.clear();
            self.emit(frames_out, out);
            // never exceed the exact expected length
            let over = self.out_frames.saturating_sub(want) as usize;
            if over > 0 {
                out.truncate(out.len() - over * ch);
                self.out_frames = want;
            }
        }
        Ok(())
    }

    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::analysis::tone_fit;
    use std::f64::consts::PI;

    fn sine_stereo(f: f64, rate: f64, n: usize) -> Vec<f32> {
        (0..n)
            .flat_map(|i| {
                let v = (0.5 * (2.0 * PI * f * i as f64 / rate).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    fn run(in_rate: u32, out_rate: u32, frames: usize, block: usize) -> Vec<f32> {
        let x = sine_stereo(1000.0, f64::from(in_rate), frames);
        let mut r = StreamResampler::new(in_rate, out_rate, 2).unwrap();
        let mut out = Vec::new();
        for c in x.chunks(block * 2) {
            r.push(c, &mut out).unwrap();
        }
        r.finish(&mut out).unwrap();
        out
    }

    #[test]
    fn length_is_exact_for_common_rate_pairs() {
        for (a, b) in [(44_100u32, 48_000u32), (48_000, 44_100), (22_050, 48_000), (48_000, 96_000), (44_100, 96_000)] {
            for (frames, block) in [(44_100usize, 1000usize), (12_345, 4096), (500, 333), (1, 1)] {
                let out = run(a, b, frames, block);
                let want = StreamResampler::expected_out(frames as u64, a, b);
                assert_eq!(out.len() as u64 / 2, want, "{a}->{b}, {frames} frames in blocks of {block}");
            }
        }
    }

    #[test]
    fn tone_keeps_its_frequency_and_level() {
        let out = run(44_100, 48_000, 44_100, 700);
        let l: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
        let (a, snr) = tone_fit(&l[2000..l.len() - 2000], 48_000.0, 1000.0);
        assert!((a - 0.5).abs() < 0.01, "amplitude {a}");
        assert!(snr > 50.0, "snr {snr}");
    }

    #[test]
    fn same_rate_is_a_bit_exact_passthrough() {
        let x = sine_stereo(440.0, 48_000.0, 5000);
        let mut r = StreamResampler::new(48_000, 48_000, 2).unwrap();
        assert!(r.is_passthrough());
        let mut out = Vec::new();
        r.push(&x, &mut out).unwrap();
        r.finish(&mut out).unwrap();
        assert_eq!(out, x);
    }

    #[test]
    fn start_delay_is_trimmed_so_the_signal_is_aligned() {
        // a click at frame 1000 must stay near frame 1000 * 48/44.1
        let mut x = vec![0.0f32; 2 * 4000];
        x[2 * 1000] = 1.0;
        x[2 * 1000 + 1] = 1.0;
        let mut r = StreamResampler::new(44_100, 48_000, 2).unwrap();
        let mut out = Vec::new();
        r.push(&x, &mut out).unwrap();
        r.finish(&mut out).unwrap();
        let peak = out.chunks(2).enumerate().max_by(|a, b| a.1[0].abs().total_cmp(&b.1[0].abs())).unwrap().0;
        let want = (1000.0 * 48_000.0 / 44_100.0) as i64;
        assert!((peak as i64 - want).abs() <= 3, "click at {peak}, expected {want}");
    }
}
