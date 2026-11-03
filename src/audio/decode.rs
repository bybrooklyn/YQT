// SPDX-License-Identifier: GPL-3.0-only
//! Container demux (symphonia: WebM/Matroska and MP4) + codec decode (Opus via `opus-rs`, AAC via symphonia).
//!
//! A `TrackDecoder` turns one stream into blocks of interleaved `f32` at the codec's native rate, can seek
//! (a new ranged request through the demuxer's seek index) and never panics on bad data: every problem is an `Err(String)`.
use super::opus::{self, OpusPacketDecoder};
use super::source::{sniff, Container};
use std::io::{Read, Seek, SeekFrom};
use symphonia::core::codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_OPUS};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::TimeBase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecKind {
    Opus,
    Aac,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackInfo {
    pub codec: CodecKind,
    pub container: Container,
    /// Native decoded rate (Hz) and channel count, as of the first decoded block.
    pub sample_rate: u32,
    pub channels: usize,
    pub duration_ms: Option<u64>,
}

enum Dec {
    Opus(Box<OpusPacketDecoder>),
    Sym(Box<dyn AudioDecoder>),
}

/// Decoder warm-up before a seek target so the first audible sample is converged (Opus needs 80 ms).
const PREROLL_MS: u64 = 100;
/// Give up on a stream after this many undecodable packets in a row.
const MAX_BAD_PACKETS: u32 = 50;

pub struct TrackDecoder {
    fmt: Box<dyn FormatReader>,
    track_id: u32,
    tb: Option<TimeBase>,
    dec: Dec,
    pub info: TrackInfo,
    /// Native frame index of the next frame to be decoded (only meaningful when resynced from a packet).
    pos_frames: u64,
    /// Frames to throw away from the upcoming output (Opus pre-skip, or the run-up to a seek target).
    skip: u64,
    /// The next packet's timestamp defines `pos_frames` (after a seek).
    resync: bool,
    seek_target: Option<u64>,
    /// The first decoded block, decoded during `open` so `info` is exact.
    pending: Vec<f32>,
    scratch: Vec<f32>,
    eos: bool,
}

fn ticks_to_frames(ts: i64, tb: Option<TimeBase>, rate: u32) -> u64 {
    let Some(tb) = tb else { return 0 };
    let secs = ts.max(0) as f64 * f64::from(tb.numer.get()) / f64::from(tb.denom.get());
    (secs * f64::from(rate)).round() as u64
}

impl TrackDecoder {
    /// Probe the container, pick the audio track, build the decoder and decode the first block.
    pub fn open(mut src: Box<dyn MediaSource>) -> Result<Self, String> {
        let mut head = [0u8; 12];
        src.read_exact(&mut head).map_err(|_| "stream is too short or empty".to_owned())?;
        src.seek(SeekFrom::Start(0)).map_err(|e| format!("cannot rewind stream: {e}"))?;
        let container = sniff(&head);
        let mut hint = Hint::new();
        match container {
            Container::WebM => hint.with_extension("webm"),
            Container::Mp4 => hint.with_extension("m4a"),
            Container::Unknown => return Err("unsupported container (expected WebM or MP4 audio)".into()),
        };
        let mss = MediaSourceStream::new(src, Default::default());
        let fmt = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .map_err(|e| format!("cannot read {container:?} stream: {e}"))?;
        let track = fmt.default_track(TrackType::Audio).ok_or("no audio track")?;
        let track_id = track.id;
        let tb = track.time_base;
        let params = match &track.codec_params {
            Some(CodecParameters::Audio(p)) => p.clone(),
            _ => return Err("audio track has no codec parameters".into()),
        };
        let (dec, codec, mut channels, mut rate, skip) = if params.codec == CODEC_ID_OPUS {
            let head = opus::parse_head(params.extra_data.as_deref().unwrap_or(&[]))?;
            let d = OpusPacketDecoder::new(head)?;
            (Dec::Opus(Box::new(d)), CodecKind::Opus, head.channels, opus::RATE, head.pre_skip as u64)
        } else if params.codec == CODEC_ID_AAC {
            let d = symphonia::default::get_codecs()
                .make_audio_decoder(&params, &AudioDecoderOptions::default())
                .map_err(|e| format!("AAC decoder: {e}"))?;
            let ch = params.channels.as_ref().map_or(2, |c| c.count());
            (Dec::Sym(d), CodecKind::Aac, ch, params.sample_rate.unwrap_or(44_100), 0)
        } else {
            return Err("unsupported audio codec (expected Opus or AAC)".into());
        };
        let ticks_ms = |d: u64, tb: TimeBase| d as f64 * 1000.0 * f64::from(tb.numer.get()) / f64::from(tb.denom.get());
        let media = fmt.media_info();
        let duration_ms = track
            .duration
            .zip(tb)
            .map(|(d, tb)| ticks_ms(d.get(), tb))
            .or_else(|| media.duration.zip(media.time_base).map(|(d, tb)| ticks_ms(d.get(), tb)))
            .or_else(|| track.num_frames.map(|n| n as f64 * 1000.0 / f64::from(rate)))
            .map(|ms| ms.round() as u64)
            .filter(|&ms| ms > 0); // fragmented MP4 announces 0 in its (empty) moov
        channels = channels.max(1);
        rate = rate.max(1);
        let mut me = Self {
            fmt,
            track_id,
            tb,
            dec,
            info: TrackInfo { codec, container, sample_rate: rate, channels, duration_ms },
            pos_frames: 0,
            skip,
            resync: false,
            seek_target: None,
            pending: Vec::new(),
            scratch: Vec::new(),
            eos: false,
        };
        // Decode the first block now: gives the exact output format and surfaces broken streams immediately.
        let mut first = Vec::new();
        if !me.read_block(&mut first)? {
            return Err("stream contains no audio".into());
        }
        me.pending = first;
        Ok(me)
    }

    /// The next block of interleaved `f32` at `info.sample_rate`/`info.channels`; `Ok(false)` at the end of the stream.
    pub fn next_block(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        out.clear();
        if !self.pending.is_empty() {
            std::mem::swap(out, &mut self.pending);
            return Ok(true);
        }
        self.read_block(out)
    }

    fn read_block(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        let mut bad = 0u32;
        loop {
            if self.eos {
                return Ok(false);
            }
            let packet = match self.fmt.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.eos = true;
                    return Ok(false);
                }
                Err(SymError::IoError(e)) => {
                    // An early EOF in the middle of a stream is a truncated download, not a clean end.
                    self.eos = true;
                    let near_end = self.info.duration_ms.is_none_or(|d| self.position_ms() + 2_000 >= d);
                    return if e.kind() == std::io::ErrorKind::UnexpectedEof && near_end {
                        Ok(false)
                    } else if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        Err("the stream ended unexpectedly (truncated)".into())
                    } else {
                        Err(format!("read error: {e}"))
                    };
                }
                Err(SymError::ResetRequired) => {
                    self.eos = true;
                    return Ok(false);
                }
                Err(e) => {
                    bad += 1;
                    if bad > MAX_BAD_PACKETS {
                        self.eos = true;
                        return Err(format!("corrupt stream: {e}"));
                    }
                    continue;
                }
            };
            if packet.track_id != self.track_id {
                continue;
            }
            if self.resync {
                self.pos_frames = ticks_to_frames(packet.pts.get(), self.tb, self.info.sample_rate);
                self.skip = self.seek_target.take().map_or(0, |t| t.saturating_sub(self.pos_frames));
                self.resync = false;
            }
            self.scratch.clear();
            let rate_and_ch = match &mut self.dec {
                Dec::Opus(d) => match d.decode(&packet.data, &mut self.scratch) {
                    Ok(_) => Some((opus::RATE, d.head.channels)),
                    Err(_) => None,
                },
                Dec::Sym(d) => match d.decode(&packet) {
                    Ok(buf) => {
                        let spec = buf.spec().clone();
                        buf.copy_to_vec_interleaved::<f32>(&mut self.scratch);
                        Some((spec.rate(), spec.channels().count()))
                    }
                    Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => None,
                    Err(e) => {
                        self.eos = true;
                        return Err(format!("AAC decode failed: {e}"));
                    }
                },
            };
            let Some((rate, ch)) = rate_and_ch else {
                bad += 1;
                if bad > MAX_BAD_PACKETS {
                    self.eos = true;
                    return Err("too many undecodable packets".into());
                }
                continue;
            };
            bad = 0;
            if rate != self.info.sample_rate || ch != self.info.channels {
                self.info.sample_rate = rate;
                self.info.channels = ch.max(1);
            }
            let ch = self.info.channels;
            let frames = self.scratch.len() / ch;
            if frames == 0 {
                continue;
            }
            let drop = (self.skip as usize).min(frames);
            self.skip -= drop as u64;
            self.pos_frames += frames as u64;
            if drop == frames {
                continue;
            }
            out.extend_from_slice(&self.scratch[drop * ch..frames * ch]);
            return Ok(true);
        }
    }

    fn position_ms(&self) -> u64 {
        self.pos_frames * 1000 / u64::from(self.info.sample_rate.max(1))
    }

    /// Move to `ms` (within one packet of exact; the run-up is decoded and discarded). Returns the position reached.
    pub fn seek_ms(&mut self, ms: u64) -> Result<u64, String> {
        let rate = u64::from(self.info.sample_rate);
        let target_ms = self.info.duration_ms.map_or(ms, |d| ms.min(d));
        let mut from_ms = target_ms.saturating_sub(PREROLL_MS);
        // The Matroska seek index only reaches as far as the last cue point; a target beyond it is reported as out of
        // range. Fall back to earlier points (up to 30 s) and decode forward from there.
        let mut back = 0;
        loop {
            let time = symphonia::core::units::Time::from_millis(from_ms as i64);
            match self.fmt.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(self.track_id) }) {
                Ok(_) => break,
                Err(SymError::SeekError(symphonia::core::errors::SeekErrorKind::OutOfRange)) if from_ms > 0 && back < 30 => {
                    back += 1;
                    from_ms = from_ms.saturating_sub(1000);
                }
                Err(e) => return Err(format!("seek failed: {e}")),
            }
        }
        match &mut self.dec {
            Dec::Opus(d) => d.reset(),
            Dec::Sym(d) => d.reset(),
        }
        self.pending.clear();
        self.eos = false;
        self.resync = true;
        self.seek_target = Some(target_ms * rate / 1000);
        Ok(target_ms)
    }
}

/// Decode a whole in-memory/streamed source to interleaved f32 (tests, offline tools).
pub fn decode_all(src: Box<dyn MediaSource>) -> Result<(TrackInfo, Vec<f32>), String> {
    let mut d = TrackDecoder::open(src)?;
    let mut all = Vec::new();
    let mut blk = Vec::new();
    while d.next_block(&mut blk)? {
        all.extend_from_slice(&blk);
    }
    Ok((d.info, all))
}
