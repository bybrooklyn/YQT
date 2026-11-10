// SPDX-License-Identifier: GPL-3.0-only
//! Container demux (symphonia: WebM/Matroska and MP4) + codec decode (Opus via `opus-rs`, AAC via symphonia).
//!
//! A `TrackDecoder` turns one stream into blocks of interleaved `f32` at the codec's native rate, can seek
//! (a new ranged request through the demuxer's seek index) and never panics on bad data: every problem is an `Err(String)`.
use super::opus::{self, OpusPacketDecoder};
use super::source::{interrupt_pending, sniff, Container};
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
/// Picking up after an interrupted read is spliced onto audio that was already played, so there the decoder is run in
/// for much longer: the seam has to be inaudible, not just converged.
const RESUME_PREROLL_MS: u64 = 1000;
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
    /// The stream's own start-up samples (Opus pre-skip; 0 for AAC), dropped whenever decoding starts at the first packet.
    pre_skip: u64,
    /// The next packet's timestamp defines `pos_frames` (after a seek).
    resync: bool,
    seek_target: Option<u64>,
    /// A reposition still to be carried out before the next packet is read. Seeks are lazy (no I/O in `seek_ms`), and
    /// a read that was interrupted leaves one behind so decoding continues at the very same frame.
    goto: Option<Goto>,
    /// The reposition that has been carried out but has not reached its packet yet (interrupted again = once more).
    active: Option<Goto>,
    /// Picking up after an interruption: decode and discard everything up to and including this packet.
    discard_through: Option<Goto>,
    /// Timestamp of the last packet that went through the decoder.
    last_pts: Option<i64>,
    /// The first decoded block, decoded during `open` so `info` is exact.
    pending: Vec<f32>,
    scratch: Vec<f32>,
    eos: bool,
}

/// Where decoding is to continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goto {
    /// A seek asked for by the caller: output starts at this native frame (as exact as the packet timestamps are).
    Seek { frame: u64 },
    /// Pick up exactly where an interrupted read left off: right after the packet with timestamp `pts`, less `skip`
    /// frames that were still to be dropped. Packet timestamps are rounded (to 1 ms in WebM), so the packet itself is
    /// the anchor, not a frame count derived from them: that is what makes the seam sample-exact. `pos` is the
    /// decoder's frame counter at that point (it also says roughly where in the stream that is).
    After { pts: i64, skip: u64, pos: u64 },
}

/// What `TrackDecoder::next` produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// A block of audio is in the buffer.
    Block,
    /// The stream is over.
    End,
    /// A newer command interrupted the network read (`source::Interrupt`). Nothing is lost: call again later.
    Interrupted,
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
            pre_skip: skip,
            resync: false,
            seek_target: None,
            active: None,
            discard_through: None,
            last_pts: None,
            goto: None,
            pending: Vec::new(),
            scratch: Vec::new(),
            eos: false,
        };
        // Decode the first block now: gives the exact output format and surfaces broken streams immediately.
        let mut first = Vec::new();
        match me.read_block(&mut first)? {
            Next::Block => {}
            Next::End => return Err("stream contains no audio".into()),
            Next::Interrupted => return Err("interrupted while opening the stream".into()),
        }
        me.pending = first;
        Ok(me)
    }

    /// The next block of interleaved `f32` at `info.sample_rate`/`info.channels`; `Ok(false)` at the end of the stream.
    /// For callers that are never interrupted (tests, offline tools): an interruption is an error here.
    pub fn next_block(&mut self, out: &mut Vec<f32>) -> Result<bool, String> {
        match self.next(out)? {
            Next::Block => Ok(true),
            Next::End => Ok(false),
            Next::Interrupted => Err("interrupted".into()),
        }
    }

    /// The next block of interleaved `f32` at `info.sample_rate`/`info.channels`, the end of the stream, or the news
    /// that the read was interrupted (then the same audio comes from a later call).
    pub fn next(&mut self, out: &mut Vec<f32>) -> Result<Next, String> {
        out.clear();
        if !self.pending.is_empty() {
            std::mem::swap(out, &mut self.pending);
            return Ok(Next::Block);
        }
        self.read_block(out)
    }

    /// The reposition that is still owed (after an interruption, or a seek that could not be carried out): what a
    /// fresh decoder of the same stream needs to take over (`resume_from`).
    pub fn resume_point(&self) -> Option<Goto> {
        self.goto
    }

    /// Continue where another decoder of the same stream stopped.
    pub fn resume_from(&mut self, at: Goto) {
        self.pending.clear();
        self.eos = false;
        self.resync = false;
        self.active = None;
        self.discard_through = None;
        self.goto = Some(at);
    }

    /// Position the demuxer for `g` (network I/O; interruptible).
    fn reposition(&mut self, g: Goto) -> Result<(), String> {
        let rate = u64::from(self.info.sample_rate.max(1));
        let mut from_ms = match g {
            Goto::Seek { frame } => (frame * 1000 / rate).saturating_sub(PREROLL_MS),
            Goto::After { pos, .. } => (pos * 1000 / rate).saturating_sub(RESUME_PREROLL_MS),
        };
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
        self.active = Some(g);
        self.resync = false;
        self.discard_through = None;
        match g {
            Goto::Seek { frame } => {
                self.resync = true;
                self.seek_target = Some(frame);
            }
            Goto::After { .. } => self.discard_through = Some(g),
        }
        Ok(())
    }

    /// The read was interrupted: remember exactly where to continue.
    fn interrupted(&mut self) -> Next {
        if self.goto.is_none() {
            self.goto = Some(match (self.active, self.last_pts) {
                // repositioned, but not arrived yet: the same again
                (Some(g), _) => g,
                // mid-stream: right after the last packet that was decoded (minus what was still to be skipped)
                (None, Some(pts)) => Goto::After { pts, skip: self.skip, pos: self.pos_frames },
                (None, None) => Goto::Seek { frame: 0 },
            });
        }
        Next::Interrupted
    }

    fn read_block(&mut self, out: &mut Vec<f32>) -> Result<Next, String> {
        let mut bad = 0u32;
        loop {
            if self.eos {
                return Ok(Next::End);
            }
            if let Some(g) = self.goto {
                match self.reposition(g) {
                    Ok(()) => self.goto = None,
                    Err(_) if interrupt_pending() => return Ok(Next::Interrupted),
                    // kept in `goto`: the caller may carry it over to a freshly opened stream
                    Err(e) => return Err(e),
                }
            }
            let packet = match self.fmt.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.eos = true;
                    return Ok(Next::End);
                }
                // Whatever the demuxer made of it, a failure while a newer command is waiting for this thread is the
                // read being interrupted, not the stream being broken.
                Err(_) if interrupt_pending() => return Ok(self.interrupted()),
                Err(SymError::IoError(e)) => {
                    // An early EOF in the middle of a stream is a truncated download, not a clean end.
                    self.eos = true;
                    let near_end = self.info.duration_ms.is_none_or(|d| self.position_ms() + 2_000 >= d);
                    return if e.kind() == std::io::ErrorKind::UnexpectedEof && near_end {
                        Ok(Next::End)
                    } else if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        Err("the stream ended unexpectedly (truncated)".into())
                    } else {
                        Err(format!("read error: {e}"))
                    };
                }
                Err(SymError::ResetRequired) => {
                    self.eos = true;
                    return Ok(Next::End);
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
            let pts = packet.pts.get();
            if self.resync {
                self.pos_frames = ticks_to_frames(pts, self.tb, self.info.sample_rate);
                self.skip = self.seek_target.take().map_or(0, |t| t.saturating_sub(self.pos_frames));
                if self.pos_frames == 0 {
                    // back at the very first packet: the codec's start-up samples are dropped again, as on open
                    self.skip += self.pre_skip;
                }
                self.resync = false;
                self.active = None;
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
            self.last_pts = Some(pts);
            if let Some(Goto::After { pts: through, skip, pos }) = self.discard_through {
                if pts < through {
                    continue; // the run-in: decoded for the decoder's state only
                }
                // Arrived. From the next packet on (less `skip`) the audio is new; the counters are as they were.
                self.discard_through = None;
                self.active = None;
                (self.skip, self.pos_frames) = (skip, pos);
                if pts == through {
                    continue;
                }
                // (a packet past the anchor without having met it cannot happen; if it does, play on from here)
            }
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
            return Ok(Next::Block);
        }
    }

    fn position_ms(&self) -> u64 {
        self.pos_frames * 1000 / u64::from(self.info.sample_rate.max(1))
    }

    /// Move to `ms` (within one packet of exact; the run-up is decoded and discarded). Returns the position reached.
    ///
    /// No I/O happens here: the demuxer is repositioned by the next read. A seek therefore never waits for the network
    /// (the caller can show the new position at once), and a stream that cannot be repositioned reports it from
    /// `next`, where `TrackStream` answers with a fresh stream.
    pub fn seek_ms(&mut self, ms: u64) -> Result<u64, String> {
        let rate = u64::from(self.info.sample_rate);
        let target_ms = self.info.duration_ms.map_or(ms, |d| ms.min(d));
        self.resume_from(Goto::Seek { frame: target_ms * rate / 1000 });
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
