// SPDX-License-Identifier: GPL-3.0-only
//! The play-out side: a ring of decoded chunks, the output DSP (EQ, limiter, volume, pause ramp) and the sinks
//! that pull from it (cpal for real devices, a paced null sink for real-time tests, plain calls for offline renders).
use super::eq::{self, EqProcessor};
use super::limiter;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// What the engine tells the outside world about.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A track's first frame reached the output (a new track, or a gapless/crossfade boundary).
    Started(Arc<TrackMeta>),
    /// A track's last frame was played.
    Ended(Arc<TrackMeta>),
    /// The track requested by `load` is open and ready.
    Loaded(Arc<TrackMeta>),
    Failed(String),
}

#[derive(Debug, PartialEq)]
pub struct TrackMeta {
    pub serial: u64,
    pub url: String,
    pub duration_ms: Option<u64>,
    pub gain_db: f64,
}

struct Chunk {
    data: Vec<f32>,
    /// Read offset into `data`, in samples.
    off: usize,
    meta: Arc<TrackMeta>,
    /// Position in the track of `data[0]`.
    start_ms: f64,
    end_of_track: bool,
}

#[derive(Default)]
struct RingState {
    chunks: VecDeque<Chunk>,
    frames: usize,
    playing: Option<Arc<TrackMeta>>,
}

/// Decoded audio waiting to be played, shared between the feeder thread and the output callback.
pub struct Ring {
    state: Mutex<RingState>,
    rate: u32,
    position_bits: AtomicU64,
    events: Sender<Event>,
}

impl Ring {
    pub fn new(rate: u32, events: Sender<Event>) -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(RingState::default()), rate, position_bits: AtomicU64::new(0f64.to_bits()), events })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RingState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Frames queued for playing.
    pub fn frames(&self) -> usize {
        self.lock().frames
    }

    /// Position (ms) in the playing track of the next frame to be played; sample accurate.
    pub fn position_ms(&self) -> f64 {
        f64::from_bits(self.position_bits.load(Ordering::Relaxed))
    }

    pub fn set_position_ms(&self, ms: f64) {
        self.position_bits.store(ms.max(0.0).to_bits(), Ordering::Relaxed);
    }

    /// The track whose audio is currently coming out.
    pub fn playing(&self) -> Option<Arc<TrackMeta>> {
        self.lock().playing.clone()
    }

    pub fn push(&self, data: Vec<f32>, meta: Arc<TrackMeta>, start_ms: f64, end_of_track: bool) {
        let mut s = self.lock();
        s.frames += data.len() / 2;
        s.chunks.push_back(Chunk { data, off: 0, meta, start_ms, end_of_track });
    }

    /// Drop everything queued (seek / load / stop) and show `pos_ms` at once.
    pub fn flush(&self, pos_ms: f64, forget_playing: bool) {
        let mut s = self.lock();
        s.chunks.clear();
        s.frames = 0;
        if forget_playing {
            s.playing = None;
        }
        self.set_position_ms(pos_ms);
    }

    /// Copy up to `out.len() / 2` frames into `out`; returns the frames written. Updates the position and emits track events.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let want = out.len() / 2;
        let mut got = 0;
        let mut guard = self.lock();
        let s = &mut *guard;
        while got < want {
            let Some(c) = s.chunks.front_mut() else { break };
            if s.playing.as_ref().map(|p| p.serial) != Some(c.meta.serial) {
                s.playing = Some(c.meta.clone());
                let _ = self.events.send(Event::Started(c.meta.clone()));
            }
            let n = ((c.data.len() - c.off) / 2).min(want - got);
            out[got * 2..(got + n) * 2].copy_from_slice(&c.data[c.off..c.off + n * 2]);
            c.off += n * 2;
            got += n;
            let pos = c.start_ms + (c.off / 2) as f64 * 1000.0 / f64::from(self.rate);
            let done = c.off >= c.data.len();
            let (eot, meta) = (c.end_of_track, c.meta.clone());
            self.set_position_ms(pos);
            s.frames -= n;
            if done {
                s.chunks.pop_front();
                if eot {
                    let _ = self.events.send(Event::Ended(meta));
                }
            }
        }
        got
    }
}

/// Settings shared with the callback; plain atomics so the Qt thread never blocks.
pub struct Controls {
    pub playing: AtomicBool,
    volume_bits: AtomicU32,
}

impl Default for Controls {
    fn default() -> Self {
        Self { playing: AtomicBool::new(false), volume_bits: AtomicU32::new(1f32.to_bits()) }
    }
}

impl Controls {
    pub fn set_volume(&self, v: f32) {
        self.volume_bits.store(if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }.to_bits(), Ordering::Relaxed);
    }
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }
}

/// Everything between the ring and the device: EQ, limiter, volume and a click-free pause.
pub struct OutputDsp {
    ring: Arc<Ring>,
    ctl: Arc<Controls>,
    eq_src: Arc<eq::EqShared>,
    eq: EqProcessor,
    eq_version: u64,
    limiter_on: bool,
    vol: f32,
    play: f32,
    step: f32,
    /// Frames of underrun while playing (the feeder fell behind).
    pub underruns: Arc<AtomicU64>,
}

impl OutputDsp {
    /// Follows the process-wide EQ settings.
    pub fn new(ring: Arc<Ring>, ctl: Arc<Controls>) -> Self {
        Self::with_eq(ring, ctl, eq::shared().clone())
    }

    /// Follows the given EQ settings (tests use private ones).
    pub fn with_eq(ring: Arc<Ring>, ctl: Arc<Controls>, eq_src: Arc<eq::EqShared>) -> Self {
        let rate = ring.rate();
        let vol = ctl.volume();
        Self {
            ring,
            ctl,
            eq_src,
            eq: EqProcessor::new(f64::from(rate)),
            eq_version: 0,
            limiter_on: true,
            vol,
            play: 0.0,
            step: 1.0 / (rate as f32 * 0.012),
            underruns: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Fill `out` (interleaved stereo) with the next audio. Always writes the whole buffer.
    pub fn render(&mut self, out: &mut [f32]) {
        let v = self.eq_src.version();
        if v != self.eq_version {
            let p = self.eq_src.get();
            self.eq.set_params(&p);
            self.limiter_on = p.limiter;
            self.eq_version = v;
        }
        let target_play = if self.ctl.playing.load(Ordering::Relaxed) { 1.0 } else { 0.0 };
        if self.play == 0.0 && target_play == 0.0 {
            out.fill(0.0);
            return;
        }
        let got = self.ring.pop(out);
        out[got * 2..].fill(0.0);
        if got * 2 < out.len() && target_play > 0.0 {
            self.underruns.fetch_add(((out.len() - got * 2) / 2) as u64, Ordering::Relaxed);
        }
        self.eq.process(&mut out[..got * 2]);
        if self.limiter_on {
            limiter::process(&mut out[..got * 2]);
        }
        let vol_t = self.ctl.volume();
        for f in out.as_chunks_mut::<2>().0 {
            self.vol += (vol_t - self.vol).clamp(-self.step, self.step);
            self.play += (target_play - self.play).clamp(-self.step, self.step);
            let g = self.vol * self.play;
            f[0] *= g;
            f[1] *= g;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// sinks
// ---------------------------------------------------------------------------------------------

/// Where the audio goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Backend {
    /// Consume at real-time speed and discard (tests, headless runs).
    Null { rate: u32 },
    /// A real output device; empty name = the system default.
    Device { name: String },
}

/// Keeps a sink alive; dropping it stops the output thread.
pub struct SinkHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SinkHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The rate a backend will run at (opens nothing audible).
pub fn probe_rate(b: &Backend) -> Result<u32, String> {
    match b {
        Backend::Null { rate } => Ok(*rate),
        Backend::Device { name } => device_rate(name),
    }
}

/// Start pulling from `dsp`. `rate` must be what `probe_rate` returned.
pub fn start(b: &Backend, dsp: OutputDsp) -> Result<SinkHandle, String> {
    let stop = Arc::new(AtomicBool::new(false));
    match b {
        Backend::Null { rate } => {
            let (stop2, rate) = (stop.clone(), *rate);
            let mut dsp = dsp;
            let t = std::thread::Builder::new()
                .name("ytm-audio-null".into())
                .spawn(move || {
                    let mut buf = vec![0f32; (rate as usize / 100) * 2];
                    let mut next = std::time::Instant::now();
                    while !stop2.load(Ordering::Relaxed) {
                        dsp.render(&mut buf);
                        next += std::time::Duration::from_millis(10);
                        let now = std::time::Instant::now();
                        if next > now {
                            std::thread::sleep(next - now);
                        } else if now - next > std::time::Duration::from_millis(200) {
                            next = now;
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(SinkHandle { stop, thread: Some(t) })
        }
        Backend::Device { name } => start_device(name, dsp, stop),
    }
}

fn pick_device(name: &str) -> Result<cpal::Device, String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    if !name.is_empty() {
        if let Ok(devs) = host.output_devices() {
            for d in devs {
                let id = d.id().map(|i| i.to_string()).unwrap_or_default();
                let desc = d.description().map(|x| x.to_string()).unwrap_or_default();
                if id == name || desc == name {
                    return Ok(d);
                }
            }
        }
        return Err(format!("output device \"{name}\" not found"));
    }
    host.default_output_device().ok_or_else(|| "no audio output device found".to_owned())
}

/// Names (ids) of the output devices, for the settings UI. Enumerating opens nothing.
pub fn output_devices() -> Vec<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    host.output_devices().map(|it| it.filter_map(|d| d.id().ok().map(|i| i.to_string())).collect()).unwrap_or_default()
}

fn device_rate(name: &str) -> Result<u32, String> {
    use cpal::traits::DeviceTrait;
    let d = pick_device(name)?;
    let cfg = d.default_output_config().map_err(|e| format!("output device: {e}"))?;
    Ok(cfg.sample_rate())
}

fn start_device(name: &str, dsp: OutputDsp, stop: Arc<AtomicBool>) -> Result<SinkHandle, String> {
    // The stream is created and kept alive on its own thread (cpal streams are not Send on every platform).
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (name, stop2) = (name.to_owned(), stop.clone());
    let t = std::thread::Builder::new()
        .name("ytm-audio-out".into())
        .spawn(move || match build_stream(&name, dsp) {
            Ok(stream) => {
                let _ = tx.send(Ok(()));
                while !stop2.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                drop(stream);
            }
            Err(e) => {
                let _ = tx.send(Err(e));
            }
        })
        .map_err(|e| e.to_string())?;
    match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(SinkHandle { stop, thread: Some(t) }),
        Ok(Err(e)) => {
            let _ = t.join();
            Err(e)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            Err("the audio device did not respond".into())
        }
    }
}

fn build_stream(name: &str, dsp: OutputDsp) -> Result<cpal::Stream, String> {
    use cpal::traits::{DeviceTrait, StreamTrait};
    use cpal::SampleFormat as F;
    let device = pick_device(name)?;
    let supported = device.default_output_config().map_err(|e| format!("output device: {e}"))?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let stream = match format {
        F::F32 => build::<f32>(&device, config, dsp),
        F::I16 => build::<i16>(&device, config, dsp),
        F::U16 => build::<u16>(&device, config, dsp),
        F::I32 => build::<i32>(&device, config, dsp),
        other => Err(format!("unsupported sample format {other}")),
    }?;
    stream.play().map_err(|e| format!("cannot start output: {e}"))?;
    Ok(stream)
}

fn build<T>(device: &cpal::Device, config: cpal::StreamConfig, mut dsp: OutputDsp) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    use cpal::traits::DeviceTrait;
    let channels = usize::from(config.channels).max(1);
    let mut stereo = vec![0f32; 8192 * 2];
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let frames = data.len() / channels;
                if stereo.len() < frames * 2 {
                    stereo.resize(frames * 2, 0.0); // rare: a device asking for an unusually large buffer
                }
                dsp.render(&mut stereo[..frames * 2]);
                for (i, out) in data.chunks_exact_mut(channels).enumerate() {
                    let (l, r) = (stereo[i * 2], stereo[i * 2 + 1]);
                    match channels {
                        1 => out[0] = T::from_sample((l + r) * 0.5),
                        _ => {
                            out[0] = T::from_sample(l);
                            out[1] = T::from_sample(r);
                            for o in &mut out[2..] {
                                *o = T::from_sample(0.0);
                            }
                        }
                    }
                }
            },
            |e| eprintln!("audio output error: {e}"),
            None,
        )
        .map_err(|e| format!("cannot open the output device: {e}"))
}

// ---------------------------------------------------------------------------------------------
// WAV
// ---------------------------------------------------------------------------------------------

/// Write interleaved stereo `f32` as a 32-bit-float WAV.
pub fn write_wav(path: &std::path::Path, rate: u32, samples: &[f32]) -> std::io::Result<()> {
    use std::io::Write;
    let data_len = (samples.len() * 4) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&3u16.to_le_bytes())?; // IEEE float
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 8).to_le_bytes())?;
    f.write_all(&8u16.to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn meta(serial: u64) -> Arc<TrackMeta> {
        Arc::new(TrackMeta { serial, url: format!("t{serial}"), duration_ms: None, gain_db: 0.0 })
    }

    #[test]
    fn ring_tracks_position_and_events_sample_accurately() {
        let (tx, rx) = channel();
        let ring = Ring::new(1000, tx); // 1 kHz so frames == ms
        ring.push(vec![0.5; 2 * 100], meta(1), 40.0, true);
        ring.push(vec![0.25; 2 * 50], meta(2), 0.0, false);
        let mut out = vec![0f32; 2 * 30];
        assert_eq!(ring.pop(&mut out), 30);
        assert!((ring.position_ms() - 70.0).abs() < 1e-9);
        let mut out = vec![0f32; 2 * 100];
        assert_eq!(ring.pop(&mut out), 100);
        assert_eq!(out[2 * 69], 0.5);
        assert_eq!(out[2 * 70], 0.25, "gapless: the next track follows immediately");
        assert!((ring.position_ms() - 30.0).abs() < 1e-9);
        let evs: Vec<Event> = rx.try_iter().collect();
        assert!(
            matches!(&evs[..], [Event::Started(a), Event::Ended(b), Event::Started(c)] if a.serial == 1 && b.serial == 1 && c.serial == 2),
            "{evs:?}"
        );
        // underrun: only 20 frames left
        let mut out = vec![1f32; 2 * 50];
        assert_eq!(ring.pop(&mut out), 20);
        assert_eq!(ring.frames(), 0);
    }

    #[test]
    fn flush_empties_and_repositions() {
        let (tx, _rx) = channel();
        let ring = Ring::new(48_000, tx);
        ring.push(vec![0.0; 2000], meta(1), 0.0, false);
        ring.flush(12_345.0, true);
        assert_eq!((ring.frames(), ring.position_ms()), (0, 12_345.0));
        assert!(ring.playing().is_none());
    }

    #[test]
    fn dsp_pause_and_volume_are_ramped_and_zero_volume_is_silent() {
        let (tx, _rx) = channel();
        let ring = Ring::new(48_000, tx);
        let ctl = Arc::new(Controls::default());
        let mut dsp = OutputDsp::new(ring.clone(), ctl.clone());
        ring.push(vec![0.5; 2 * 48_000], meta(1), 0.0, false);
        ctl.playing.store(true, Ordering::Relaxed);
        let mut out = vec![0f32; 2 * 4800];
        dsp.render(&mut out); // fade in from silence
        assert!(out[0].abs() < 0.01 && (out[2 * 4799] - 0.5).abs() < 1e-6, "{} {}", out[0], out[2 * 4799]);
        let max_step = out.chunks(2).map(|f| f[0]).collect::<Vec<_>>().windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(max_step < 0.01, "start ramp is smooth: {max_step}");
        // pause: ramps to zero, then the ring stops being consumed
        ctl.playing.store(false, Ordering::Relaxed);
        dsp.render(&mut out);
        assert_eq!(out[2 * 4799], 0.0);
        let frames_before = ring.frames();
        dsp.render(&mut out);
        assert_eq!(ring.frames(), frames_before, "paused: nothing consumed");
        // volume 0 = exact silence
        ctl.playing.store(true, Ordering::Relaxed);
        ctl.set_volume(0.0);
        for _ in 0..4 {
            dsp.render(&mut out);
        }
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn wav_header_is_valid() {
        let p = std::env::temp_dir().join(format!("ytm-wav-{}.wav", std::process::id()));
        write_wav(&p, 48_000, &[0.0, 0.5, -0.5, 1.0]).unwrap();
        let b = std::fs::read(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(&b[..4], b"RIFF");
        assert_eq!(&b[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([b[20], b[21]]), 3);
        assert_eq!(u32::from_le_bytes([b[24], b[25], b[26], b[27]]), 48_000);
        assert_eq!(b.len(), 44 + 16);
        assert_eq!(f32::from_le_bytes([b[48], b[49], b[50], b[51]]), 0.5);
    }

    #[test]
    fn volume_is_clamped() {
        let c = Controls::default();
        c.set_volume(7.0);
        assert_eq!(c.volume(), 1.0);
        c.set_volume(f32::NAN);
        assert_eq!(c.volume(), 0.0);
    }
}
