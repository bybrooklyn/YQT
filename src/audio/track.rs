// SPDX-License-Identifier: GPL-3.0-only
//! One track as a stream of stereo `f32` frames at the output rate: demux + decode + channel mapping + resample.
use super::decode::{CodecKind, Next, TrackDecoder};
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
    /// Where the stream came from, when it can be opened again (a demuxer that cannot be repositioned is replaced).
    url: Option<String>,
    /// The demuxer was just replaced and has not produced audio yet (so it is not replaced a second time).
    reopened: bool,
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
        let mut me = Self::open(source::open(url)?, out_rate)?;
        me.url = Some(url.to_owned());
        Ok(me)
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
            url: None,
            reopened: false,
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

    /// `frames` are not buffered yet although the stream has more: the decoder is waiting (its read was interrupted).
    pub fn starved(&self, frames: usize) -> bool {
        self.buffered() < frames && !self.eos
    }

    /// The demuxer could not be repositioned (a seek after it hit the end of the stream, or after an interrupted
    /// read left it in the middle of an element): open the stream again and let the new demuxer take over at the
    /// same frame. `None` = not possible here, the error stands.
    fn reopen(&mut self) -> Option<Result<(), String>> {
        let at = self.dec.resume_point()?;
        let url = self.url.as_deref().filter(|_| !self.reopened)?;
        Some(source::open(url).and_then(TrackDecoder::open).map(|mut dec| {
            dec.resume_from(at);
            self.dec = dec;
            self.reopened = true;
        }))
    }

    /// Decode until at least `frames` are buffered or the stream ends. It may return with less when a newer command
    /// interrupts the network read (`source::Interrupt`); the next call goes on from the very same sample.
    pub fn fill(&mut self, frames: usize) {
        while self.buffered() < frames && !self.eos {
            match self.dec.next(&mut self.block) {
                Ok(Next::Interrupted) => break,
                Err(e) => match self.reopen() {
                    Some(Ok(())) => {}
                    // interrupted while reopening: nothing changed, the next call tries again
                    Some(Err(_)) if source::interrupt_pending() => break,
                    _ => {
                        self.pending_error = Some(e);
                        self.eos = true;
                    }
                },
                Ok(Next::Block) => {
                    self.reopened = false;
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
                Ok(Next::End) => {
                    let mut out = std::mem::take(&mut self.buf);
                    if let Err(e) = self.rs.finish(&mut out) {
                        self.pending_error = Some(e);
                    }
                    self.buf = out;
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
        self.reopened = false;
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

    /// Seeking back to 0 (repeat-one, "previous") must give the very audio a fresh open gives: the codec's start-up
    /// samples (Opus pre-skip) are dropped again instead of being played and shifting everything.
    #[test]
    fn seek_to_the_start_matches_a_fresh_open() {
        for name in ["sine440_opus.webm", "chirp_opus.webm", "sine440_aac.m4a"] {
            let mut t = TrackStream::open_url(&fixture(name), 48_000).unwrap();
            let (mut fresh, mut again) = (Vec::new(), Vec::new());
            t.fill(24_000);
            t.take(24_000, 1.0, &mut fresh);
            t.seek(0).unwrap();
            t.fill(24_000);
            t.take(24_000, 1.0, &mut again);
            assert_eq!(fresh.len(), again.len(), "{name}");
            // compare past the decoder's convergence at the very start; a 312-sample shift of a tone is a large error
            let worst = fresh[9600..].iter().zip(&again[9600..]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(worst < 1e-3, "{name}: audio after seek(0) differs from a fresh open by {worst}");
        }
    }

    /// A file whose next read fails once `armed` is set, raising the thread's interrupt like a newer command would.
    struct Interruptible {
        file: std::fs::File,
        armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        interrupt: std::sync::Arc<source::Interrupt>,
    }
    impl std::io::Read for Interruptible {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                self.interrupt.raise();
                return Err(std::io::Error::other("interrupted"));
            }
            self.file.read(buf)
        }
    }
    impl std::io::Seek for Interruptible {
        fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
            self.file.seek(pos)
        }
    }
    impl MediaSource for Interruptible {
        fn is_seekable(&self) -> bool {
            true
        }
        fn byte_len(&self) -> Option<u64> {
            self.file.metadata().ok().map(|m| m.len())
        }
    }

    /// A read that is interrupted by a newer command (so the command thread is not kept waiting) must cost nothing:
    /// the stream goes on at the very same sample afterwards, with the same total length.
    #[test]
    fn an_interrupted_read_resumes_at_the_same_sample() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let path = fixture("chirp_opus.webm");
        let mut clean = Vec::new();
        let mut t = TrackStream::open_url(&path, 48_000).unwrap();
        while !t.finished() {
            t.fill(4096);
            t.take(4096, 1.0, &mut clean);
        }
        for interrupt_after in [4_800usize, 60_000, 150_000] {
            let (armed, interrupt) = (Arc::new(AtomicBool::new(false)), Arc::new(source::Interrupt::default()));
            source::install_interrupt(Some(interrupt.clone()));
            let src = Interruptible { file: std::fs::File::open(&path).unwrap(), armed: armed.clone(), interrupt: interrupt.clone() };
            let mut t = TrackStream::open(Box::new(src), 48_000).unwrap();
            let mut out = Vec::new();
            t.fill(interrupt_after);
            t.take(interrupt_after, 1.0, &mut out);
            armed.store(true, Ordering::SeqCst);
            // it reads on until the armed read fails; then it comes back with less, and neither ended nor failed
            t.fill(usize::MAX);
            assert!(interrupt.pending() && t.starved(usize::MAX) && !t.at_end(), "after {interrupt_after}: not interrupted");
            interrupt.ack();
            while !t.finished() {
                t.fill(4096);
                t.take(4096, 1.0, &mut out);
            }
            source::install_interrupt(None);
            assert!(t.take_error().is_none());
            assert_eq!(out.len(), clean.len(), "after {interrupt_after}: same length");
            let worst = out.iter().zip(&clean).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(worst < 2e-3, "after {interrupt_after}: the audio differs from an uninterrupted decode by {worst}");
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
