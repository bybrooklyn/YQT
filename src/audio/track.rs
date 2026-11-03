// SPDX-License-Identifier: GPL-3.0-only
//! One track as a stream of stereo `f32` frames at the output rate: demux + decode + channel mapping + resample.
use super::decode::{CodecKind, TrackDecoder};
use super::resample::StreamResampler;
use super::source;
use symphonia::core::io::MediaSource;

/// Decoded audio that is ready to be played, in frames (one frame = left + right sample).
pub struct TrackStream {
    dec: TrackDecoder,
    rs: StreamResampler,
    out_rate: u32,
    /// Ready stereo samples; `head` is the read offset (in samples).
    buf: Vec<f32>,
    head: usize,
    /// The decoder is done and the resampler is flushed.
    eos: bool,
    /// An error to report once the buffered audio is exhausted.
    pending_error: Option<String>,
    start_ms: u64,
    taken_frames: u64,
    scratch: Vec<f32>,
    block: Vec<f32>,
    pub codec: CodecKind,
    pub duration_ms: Option<u64>,
}

/// Map `ch` interleaved channels to interleaved stereo (mono is duplicated; extra channels are dropped).
pub fn to_stereo(input: &[f32], ch: usize, out: &mut Vec<f32>) {
    match ch {
        1 => out.extend(input.iter().flat_map(|&s| [s, s])),
        2 => out.extend_from_slice(input),
        n => {
            for f in input.chunks_exact(n) {
                out.push(f[0]);
                out.push(f[1]);
            }
        }
    }
}

impl TrackStream {
    pub fn open_url(url: &str, out_rate: u32) -> Result<Self, String> {
        Self::open(source::open(url)?, out_rate)
    }

    pub fn open(src: Box<dyn MediaSource>, out_rate: u32) -> Result<Self, String> {
        let dec = TrackDecoder::open(src)?;
        let rs = StreamResampler::new(dec.info.sample_rate, out_rate, 2)?;
        let (codec, duration_ms) = (dec.info.codec, dec.info.duration_ms);
        Ok(Self {
            dec,
            rs,
            out_rate,
            buf: Vec::new(),
            head: 0,
            eos: false,
            pending_error: None,
            start_ms: 0,
            taken_frames: 0,
            scratch: Vec::new(),
            block: Vec::new(),
            codec,
            duration_ms,
        })
    }

    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    /// Frames decoded and waiting.
    pub fn buffered(&self) -> usize {
        (self.buf.len() - self.head) / 2
    }

    /// No more audio will ever come out (everything has been taken).
    pub fn finished(&self) -> bool {
        self.eos && self.buffered() == 0
    }

    /// The decoder has reached its end (buffered audio may remain).
    pub fn at_end(&self) -> bool {
        self.eos
    }

    pub fn take_error(&mut self) -> Option<String> {
        if self.eos && self.buffered() == 0 {
            self.pending_error.take()
        } else {
            None
        }
    }

    /// Position (ms) of the next frame `take` will return.
    pub fn position_ms(&self) -> f64 {
        self.start_ms as f64 + self.taken_frames as f64 * 1000.0 / f64::from(self.out_rate)
    }

    /// Decode until at least `frames` are buffered or the stream ends.
    pub fn fill(&mut self, frames: usize) {
        while self.buffered() < frames && !self.eos {
            match self.dec.next_block(&mut self.block) {
                Ok(true) => {
                    self.scratch.clear();
                    to_stereo(&self.block, self.dec.info.channels, &mut self.scratch);
                    let mut out = std::mem::take(&mut self.buf);
                    if self.head > 32_768 && self.head * 2 > out.len() {
                        out.drain(..self.head);
                        self.head = 0;
                    }
                    if let Err(e) = self.rs.push(&self.scratch, &mut out) {
                        self.pending_error = Some(e);
                        self.eos = true;
                    }
                    self.buf = out;
                }
                Ok(false) => {
                    let mut out = std::mem::take(&mut self.buf);
                    if let Err(e) = self.rs.finish(&mut out) {
                        self.pending_error = Some(e);
                    }
                    self.buf = out;
                    self.eos = true;
                }
                Err(e) => {
                    self.pending_error = Some(e);
                    self.eos = true;
                }
            }
        }
    }

    /// Move up to `frames` ready frames (scaled by `gain`) onto the end of `out`. Returns how many were moved.
    pub fn take(&mut self, frames: usize, gain: f32, out: &mut Vec<f32>) -> usize {
        let n = frames.min(self.buffered());
        let src = &self.buf[self.head..self.head + n * 2];
        if (gain - 1.0).abs() < f32::EPSILON {
            out.extend_from_slice(src);
        } else {
            out.extend(src.iter().map(|s| s * gain));
        }
        self.head += n * 2;
        self.taken_frames += n as u64;
        n
    }

    /// Jump to `ms`; the buffer is dropped and refilled from the new position.
    pub fn seek(&mut self, ms: u64) -> Result<(), String> {
        let at = self.dec.seek_ms(ms)?;
        self.rs = StreamResampler::new(self.dec.info.sample_rate, self.out_rate, 2)?;
        self.buf.clear();
        self.head = 0;
        self.eos = false;
        self.pending_error = None;
        self.start_ms = at;
        self.taken_frames = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::codec_tests::fixture;

    #[test]
    fn stereo_mapping() {
        let mut o = Vec::new();
        to_stereo(&[1.0, 2.0], 1, &mut o);
        assert_eq!(o, [1.0, 1.0, 2.0, 2.0]);
        o.clear();
        to_stereo(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3, &mut o);
        assert_eq!(o, [1.0, 2.0, 4.0, 5.0]);
    }

    #[test]
    fn plays_a_whole_file_at_the_requested_rate() {
        for (name, rate, approx) in
            [("sine440_opus.webm", 44_100u32, 88_200usize), ("sine440_aac.m4a", 48_000, 96_000), ("chirp_opus.webm", 48_000, 288_000)]
        {
            let mut t = TrackStream::open_url(&fixture(name), rate).unwrap();
            let mut out = Vec::new();
            while !t.finished() {
                t.fill(4096);
                t.take(4096, 1.0, &mut out);
            }
            assert!(t.take_error().is_none());
            let frames = out.len() / 2;
            assert!((frames as i64 - approx as i64).abs() < 4500, "{name}: {frames} frames vs ~{approx}");
        }
    }

    #[test]
    fn seek_repositions_the_position_counter() {
        let mut t = TrackStream::open_url(&fixture("chirp_opus.webm"), 48_000).unwrap();
        t.seek(3000).unwrap();
        assert!((t.position_ms() - 3000.0).abs() < 1.0);
        t.fill(4800);
        let mut out = Vec::new();
        t.take(4800, 1.0, &mut out);
        assert!((t.position_ms() - 3100.0).abs() < 1.0, "{}", t.position_ms());
    }
}
