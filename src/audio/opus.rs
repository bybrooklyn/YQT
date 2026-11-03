// SPDX-License-Identifier: GPL-3.0-only
//! Opus packets -> PCM with `opus-rs` (a pure-Rust port of libopus; no C, no FFmpeg).
//!
//! symphonia demuxes WebM/Matroska but has no native Opus decoder, so the packets it hands us go straight here.
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Opus always decodes at 48 kHz for us (the device resampler handles the rest).
pub const RATE: u32 = 48_000;
/// The longest packet Opus can carry: 120 ms at 48 kHz.
const MAX_FRAME: usize = 5760;

/// The fields of the `OpusHead` identification header that matter for playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusHead {
    pub channels: usize,
    /// Samples (at 48 kHz) to drop from the start of the stream.
    pub pre_skip: usize,
    /// Output gain in Q7.8 dB.
    pub gain_q8: i16,
}

/// Parse an `OpusHead` (RFC 7845 §5.1). Only channel mapping family 0 (mono/stereo) is supported.
pub fn parse_head(extra: &[u8]) -> Result<OpusHead, String> {
    if extra.len() < 19 || &extra[..8] != b"OpusHead" {
        return Err("missing OpusHead".into());
    }
    let channels = extra[9] as usize;
    let pre_skip = u16::from_le_bytes([extra[10], extra[11]]) as usize;
    let gain_q8 = i16::from_le_bytes([extra[16], extra[17]]);
    if extra[18] != 0 || !(1..=2).contains(&channels) {
        return Err(format!("unsupported Opus layout ({channels} channels, mapping family {})", extra[18]));
    }
    Ok(OpusHead { channels, pre_skip, gain_q8 })
}

pub struct OpusPacketDecoder {
    dec: opus_rs::OpusDecoder,
    pub head: OpusHead,
    gain: f32,
    pcm: Vec<f32>,
}

impl OpusPacketDecoder {
    pub fn new(head: OpusHead) -> Result<Self, String> {
        let dec = opus_rs::OpusDecoder::new(RATE as i32, head.channels).map_err(|e| format!("opus init: {e}"))?;
        let gain = 10f32.powf(f32::from(head.gain_q8) / 256.0 / 20.0);
        Ok(Self { dec, head, gain, pcm: vec![0.0; MAX_FRAME * head.channels] })
    }

    /// Decode one packet, appending interleaved f32 frames to `out`. Returns the number of frames.
    /// A corrupt packet is an `Err` (the caller skips it); it never panics.
    pub fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, String> {
        let ch = self.head.channels;
        let dec = &mut self.dec;
        let pcm = &mut self.pcm;
        let r = catch_unwind(AssertUnwindSafe(|| dec.decode(packet, MAX_FRAME, pcm)));
        match r {
            Ok(Ok(n)) => {
                let n = n.min(MAX_FRAME);
                if (self.gain - 1.0).abs() > f32::EPSILON {
                    out.extend(self.pcm[..n * ch].iter().map(|s| s * self.gain));
                } else {
                    out.extend_from_slice(&self.pcm[..n * ch]);
                }
                Ok(n)
            }
            Ok(Err(e)) => Err(format!("opus decode: {e}")),
            Err(_) => {
                // Internal panic on a malformed packet: rebuild the decoder so state is sane next time.
                if let Ok(d) = opus_rs::OpusDecoder::new(RATE as i32, ch) {
                    self.dec = d;
                }
                Err("opus decode panicked on a malformed packet".into())
            }
        }
    }

    /// Forget history (after a seek).
    pub fn reset(&mut self) {
        if let Ok(d) = opus_rs::OpusDecoder::new(RATE as i32, self.head.channels) {
            self.dec = d;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head_bytes(ch: u8, pre: u16, gain: i16, family: u8) -> Vec<u8> {
        let mut v = b"OpusHead".to_vec();
        v.extend([1, ch]);
        v.extend(pre.to_le_bytes());
        v.extend(48_000u32.to_le_bytes());
        v.extend(gain.to_le_bytes());
        v.push(family);
        v
    }

    #[test]
    fn parses_opus_head() {
        let h = parse_head(&head_bytes(2, 312, 0, 0)).unwrap();
        assert_eq!(h, OpusHead { channels: 2, pre_skip: 312, gain_q8: 0 });
        assert!(parse_head(&head_bytes(6, 312, 0, 1)).is_err(), "surround is not supported");
        assert!(parse_head(b"nope").is_err());
        assert!(parse_head(&head_bytes(0, 0, 0, 0)).is_err());
    }

    #[test]
    fn garbage_packets_are_errors_or_silence_never_panics() {
        let mut d = OpusPacketDecoder::new(OpusHead { channels: 2, pre_skip: 0, gain_q8: 0 }).unwrap();
        let mut out = Vec::new();
        let mut seed = 0x1234_5678u32;
        for len in [0usize, 1, 2, 3, 7, 40, 200, 1275] {
            for _ in 0..50 {
                let pkt: Vec<u8> = (0..len)
                    .map(|_| {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        (seed >> 24) as u8
                    })
                    .collect();
                let _ = d.decode(&pkt, &mut out); // Ok or Err, but no panic / abort
                out.clear();
            }
        }
    }
}
