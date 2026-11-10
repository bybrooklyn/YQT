// SPDX-License-Identifier: GPL-3.0-only
//! The play-out side: a ring of decoded chunks, the output DSP (EQ, limiter, volume, pause ramp) and the sinks
//! that pull from it (cpal for real devices, a paced null sink for real-time tests, plain calls for offline renders).
use super::eq::{self, EqProcessor};
use super::limiter;
use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
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
    meta: Arc<TrackMeta>,
    /// Position in the track of `data[0]`.
    start_ms: f64,
    end_of_track: bool,
    /// The flush generation it was queued in: chunks of an older one are skipped unheard.
    epoch: u64,
}

/// Chunks the queue holds before the feed side keeps the rest back (the feeder stays far below this).
const SLOTS: usize = 512;
/// Track events (started / ended) waiting to be handed on; the output stops consuming rather than lose one.
const EVENT_SLOTS: usize = 64;
/// Tracks whose metadata is remembered for events and `playing()` (audible, decoding, queued, a few past ones).
const KNOWN_TRACKS: usize = 16;
/// The low 48 bits of a tagged word are the value, the high 16 the flush generation it belongs to.
const VALUE_MASK: u64 = (1 << 48) - 1;
const EV_ENDED: u64 = 1;

fn pack(epoch: u64, value: u64) -> u64 {
    (epoch << 48) | (value & VALUE_MASK)
}
fn tag_of(word: u64) -> u64 {
    word >> 48
}
fn value_of(word: u64) -> u64 {
    word & VALUE_MASK
}
fn ms_to_ns(ms: f64) -> u64 {
    ((ms.max(0.0) * 1e6).round() as u64).min(VALUE_MASK)
}

/// What only the feed side (feeder thread, status readers, tests) touches, behind a lock the output never takes.
struct FeedSide {
    /// Slots below this index have been emptied (their chunks freed).
    freed: usize,
    /// Chunks that did not fit into the queue yet.
    backlog: VecDeque<Chunk>,
    known: VecDeque<Arc<TrackMeta>>,
    out: Sender<Event>,
}

/// Decoded audio waiting to be played, shared between the feeder thread and the output callback.
///
/// The output callback (`pop`) is wait-free: it takes no lock, and it neither allocates nor frees. The hand-off is a
/// single-producer / single-consumer queue of chunks:
/// * the feed side writes slot `tail` and then publishes it by advancing `tail`;
/// * the output reads slot `head` in place and advances `head` when it is done with it;
/// * the feed side later empties the slots the output has left behind (`freed..head`), so chunks are always freed
///   on the feed side;
/// * a flush cannot take chunks away from the output, so it starts a new generation (`epoch`) instead: the output
///   skips older chunks without playing them. Position and audible track carry the generation in their top bits and
///   the output changes them by compare-and-swap, so a late write of the output can never undo a flush;
/// * track events travel as plain numbers through a second fixed queue; the feed side turns them into `Event`s.
pub struct Ring {
    rate: u32,
    slots: Box<[UnsafeCell<Option<Chunk>>]>,
    /// Chunks ever queued (feed side writes) / ever consumed (output writes); slot = index % SLOTS.
    tail: AtomicUsize,
    head: AtomicUsize,
    /// Read offset (samples) into the chunk at `head`; only the output uses it.
    off: AtomicUsize,
    /// Guards the single-consumer rule the unsafe code relies on.
    consuming: AtomicBool,
    epoch: AtomicU64,
    /// Generation | position in ns of the next frame to be played.
    position: AtomicU64,
    /// Generation | serial of the audible track (0 = none).
    playing: AtomicU64,
    /// Frames ever pushed / ever taken out of the queue (played, skipped or dropped) / pushed before the last flush.
    pushed_frames: AtomicU64,
    consumed_frames: AtomicU64,
    flushed_frames: AtomicU64,
    /// `tail` at the last flush: chunks below it are void.
    flushed_chunks: AtomicUsize,
    /// serial << 1 | ended, written by the output, read by the feed side.
    events: [AtomicU64; EVENT_SLOTS],
    ev_head: AtomicUsize,
    ev_tail: AtomicUsize,
    feed: Mutex<FeedSide>,
}

// SAFETY: the only non-`Sync` part is `slots`. A slot is accessed by exactly one side at a time: the feed side
// (serialised by the `feed` mutex) owns the slots outside `head..tail`, i.e. it writes slot `tail` before publishing
// it and empties slots below `head`; the output (a single consumer, enforced by `consuming`) only reads slot `head`
// while `head < tail`. The indices are handed over with Release/Acquire.
unsafe impl Sync for Ring {}

/// Holds the consumer side for the length of one `pop`.
struct Consumer<'a>(&'a AtomicBool);

impl Drop for Consumer<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Ring {
    pub fn new(rate: u32, events: Sender<Event>) -> Arc<Self> {
        Arc::new(Self {
            rate,
            slots: (0..SLOTS).map(|_| UnsafeCell::new(None)).collect(),
            tail: AtomicUsize::new(0),
            head: AtomicUsize::new(0),
            off: AtomicUsize::new(0),
            consuming: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            position: AtomicU64::new(0),
            playing: AtomicU64::new(0),
            pushed_frames: AtomicU64::new(0),
            consumed_frames: AtomicU64::new(0),
            flushed_frames: AtomicU64::new(0),
            flushed_chunks: AtomicUsize::new(0),
            events: std::array::from_fn(|_| AtomicU64::new(0)),
            ev_head: AtomicUsize::new(0),
            ev_tail: AtomicUsize::new(0),
            feed: Mutex::new(FeedSide { freed: 0, backlog: VecDeque::new(), known: VecDeque::with_capacity(KNOWN_TRACKS), out: events }),
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    fn feed(&self) -> std::sync::MutexGuard<'_, FeedSide> {
        self.feed.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The feed side's housekeeping: free what the output has played, queue what was held back, hand on its events.
    fn service(&self, f: &mut FeedSide) {
        let head = self.head.load(Ordering::Acquire);
        while f.freed < head {
            // SAFETY: the output has moved past this slot (`freed < head`) and never goes back; see `Sync`.
            drop(unsafe { (*self.slots[f.freed % SLOTS].get()).take() });
            f.freed += 1;
        }
        let mut tail = self.tail.load(Ordering::Relaxed);
        while tail - f.freed < SLOTS {
            let Some(c) = f.backlog.pop_front() else { break };
            // SAFETY: slot `tail` is not published yet (the output stops at `tail`) and it is empty: fewer than
            // SLOTS slots lie between `freed` and `tail`, so this one was emptied above or never used.
            unsafe { *self.slots[tail % SLOTS].get() = Some(c) };
            tail += 1;
            self.tail.store(tail, Ordering::Release);
        }
        let ev_tail = self.ev_tail.load(Ordering::Acquire);
        let mut ev = self.ev_head.load(Ordering::Relaxed);
        while ev < ev_tail {
            let word = self.events[ev % EVENT_SLOTS].load(Ordering::Relaxed);
            ev += 1;
            self.ev_head.store(ev, Ordering::Release);
            if let Some(meta) = f.known.iter().rev().find(|m| m.serial & VALUE_MASK == word >> 1) {
                let _ = f.out.send(if word & 1 == EV_ENDED { Event::Ended(meta.clone()) } else { Event::Started(meta.clone()) });
            }
        }
    }

    /// Hand the track events the output has recorded on to the event channel. The feeder does this by itself every
    /// few milliseconds; a status poller calls it so events also arrive while the feeder is busy, and code that
    /// drives the output by hand (offline rendering, tests) calls it before it reads the channel.
    pub fn deliver_events(&self) {
        self.service(&mut self.feed());
    }

    /// Frames queued for playing.
    pub fn frames(&self) -> usize {
        let mut f = self.feed();
        self.service(&mut f);
        let gone = self.consumed_frames.load(Ordering::Acquire).max(self.flushed_frames.load(Ordering::Relaxed));
        (self.pushed_frames.load(Ordering::Relaxed) - gone) as usize
    }

    /// Nothing at all is queued, not even a track's (frameless) end marker that would still report `Ended`.
    pub fn is_drained(&self) -> bool {
        let f = self.feed();
        let done = self.head.load(Ordering::Acquire).max(self.flushed_chunks.load(Ordering::Relaxed));
        f.backlog.is_empty() && self.tail.load(Ordering::Relaxed) == done
    }

    /// Position (ms) in the playing track of the next frame to be played; sample accurate.
    pub fn position_ms(&self) -> f64 {
        value_of(self.position.load(Ordering::Relaxed)) as f64 / 1e6
    }

    /// Show `ms` (feed side; meant for when nothing is queued, e.g. while a track is being reopened at a position).
    pub fn set_position_ms(&self, ms: f64) {
        let _f = self.feed();
        self.position.store(pack(self.epoch.load(Ordering::Relaxed), ms_to_ns(ms)), Ordering::Release);
    }

    /// The track whose audio is currently coming out (`None` once its last frame has been played).
    pub fn playing(&self) -> Option<Arc<TrackMeta>> {
        let serial = value_of(self.playing.load(Ordering::Acquire));
        if serial == 0 {
            return None;
        }
        self.feed().known.iter().rev().find(|m| m.serial & VALUE_MASK == serial).cloned()
    }

    /// Duration of the audible track: `None` = nothing is audible, `Some(None)` = it has no known length.
    pub fn audible_duration_ms(&self) -> Option<Option<u64>> {
        self.playing().map(|m| m.duration_ms)
    }

    pub fn push(&self, data: Vec<f32>, meta: Arc<TrackMeta>, start_ms: f64, end_of_track: bool) {
        let mut f = self.feed();
        if !f.known.iter().any(|m| m.serial == meta.serial) {
            if f.known.len() >= KNOWN_TRACKS {
                f.known.pop_front();
            }
            f.known.push_back(meta.clone());
        }
        self.pushed_frames.fetch_add((data.len() / 2) as u64, Ordering::Relaxed);
        let epoch = self.epoch.load(Ordering::Relaxed);
        f.backlog.push_back(Chunk { data, meta, start_ms, end_of_track, epoch });
        self.service(&mut f);
    }

    /// Drop everything queued (seek / load / stop) and show `pos_ms` at once.
    pub fn flush(&self, pos_ms: f64, forget_playing: bool) {
        let mut f = self.feed();
        let epoch = self.epoch.load(Ordering::Relaxed) + 1;
        // In this order: once the audible track carries the new generation, an output call that is still running on
        // the old one can neither report a track event nor move the position any more; only then does the new
        // generation open, in which the chunks queued so far are skipped.
        let mut cur = self.playing.load(Ordering::Acquire);
        while let Err(now) = self.playing.compare_exchange_weak(
            cur,
            pack(epoch, if forget_playing { 0 } else { value_of(cur) }),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            cur = now;
        }
        self.position.store(pack(epoch, ms_to_ns(pos_ms)), Ordering::Release);
        self.epoch.store(epoch, Ordering::Release);
        let held_back: usize = f.backlog.iter().map(|c| c.data.len() / 2).sum();
        f.backlog.clear();
        self.consumed_frames.fetch_add(held_back as u64, Ordering::AcqRel);
        self.flushed_frames.store(self.pushed_frames.load(Ordering::Relaxed), Ordering::Relaxed);
        self.flushed_chunks.store(self.tail.load(Ordering::Relaxed), Ordering::Relaxed);
        self.service(&mut f);
    }

    /// Record a track event for the feed side (output only). The caller has made sure there is room.
    fn emit(&self, serial: u64, ended: bool) {
        let t = self.ev_tail.load(Ordering::Relaxed);
        self.events[t % EVENT_SLOTS].store((serial << 1) | if ended { EV_ENDED } else { 0 }, Ordering::Relaxed);
        self.ev_tail.store(t + 1, Ordering::Release);
    }

    /// Copy up to `out.len() / 2` frames into `out`; returns the frames written. Updates the position and records
    /// track events. This is the output callback's side: no lock, no allocation, nothing freed. With an empty `out`
    /// it only skips what a flush has voided.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        if self.consuming.swap(true, Ordering::Acquire) {
            return 0; // a second consumer: never, but then it gets nothing rather than a data race
        }
        let _consumer = Consumer(&self.consuming);
        let want = out.len() / 2;
        let mut got = 0;
        let epoch = self.epoch.load(Ordering::Acquire);
        loop {
            let head = self.head.load(Ordering::Relaxed);
            if head == self.tail.load(Ordering::Acquire) {
                break;
            }
            // SAFETY: `head < tail`, so the feed side has published this slot and will not touch it until `head`
            // has moved past it, which only this (single) consumer does, after its last use of `c`; see `Sync`.
            let Some(c) = (unsafe { &*self.slots[head % SLOTS].get() }).as_ref() else { break };
            let off = self.off.load(Ordering::Relaxed);
            if c.epoch != epoch {
                if c.epoch > epoch {
                    break; // flushed and refilled while this call was running: the next call plays it
                }
                // voided by a flush: skipped unheard, and without track events
                self.consumed_frames.fetch_add(((c.data.len() - off) / 2) as u64, Ordering::AcqRel);
                self.off.store(0, Ordering::Relaxed);
                self.head.store(head + 1, Ordering::Release);
                continue;
            }
            if got >= want {
                break;
            }
            if EVENT_SLOTS - (self.ev_tail.load(Ordering::Relaxed) - self.ev_head.load(Ordering::Acquire)) < 2 {
                break; // nobody is collecting the events: wait rather than lose a track's start or end
            }
            let serial = c.meta.serial & VALUE_MASK;
            let playing = self.playing.load(Ordering::Acquire);
            if tag_of(playing) != tag_of(pack(epoch, 0)) {
                break; // a flush is under way
            }
            if value_of(playing) != serial {
                if self.playing.compare_exchange(playing, pack(epoch, serial), Ordering::AcqRel, Ordering::Acquire).is_err() {
                    break;
                }
                self.emit(serial, false);
            }
            let n = ((c.data.len() - off) / 2).min(want - got);
            out[got * 2..(got + n) * 2].copy_from_slice(&c.data[off..off + n * 2]);
            let off = off + n * 2;
            got += n;
            self.consumed_frames.fetch_add(n as u64, Ordering::AcqRel);
            let pos = pack(epoch, ms_to_ns(c.start_ms + (off / 2) as f64 * 1000.0 / f64::from(self.rate)));
            // only within its own generation: after a flush the position is the flush's
            let mut cur = self.position.load(Ordering::Acquire);
            while tag_of(cur) == tag_of(pos) {
                match self.position.compare_exchange_weak(cur, pos, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => break,
                    Err(now) => cur = now,
                }
            }
            if off < c.data.len() {
                self.off.store(off, Ordering::Relaxed);
                continue;
            }
            if c.end_of_track {
                // Its last frame is out, so it is no longer the audible track: "audible" and "Ended was reported"
                // can never disagree (a seek near a track boundary relies on that). Not if a flush got in between.
                let was = pack(epoch, serial);
                if self.playing.compare_exchange(was, pack(epoch, 0), Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    self.emit(serial, true);
                }
            }
            self.off.store(0, Ordering::Relaxed);
            self.head.store(head + 1, Ordering::Release);
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
            // Never waits: if the settings are being written this very moment, the current ones stay in use and the
            // next call picks the new ones up (the version is only taken over together with the settings).
            if let Some(p) = self.eq_src.try_get() {
                self.eq.set_params(&p);
                self.limiter_on = p.limiter;
                self.eq_version = v;
            }
        }
        let target_play = if self.ctl.playing.load(Ordering::Relaxed) { 1.0 } else { 0.0 };
        if self.play == 0.0 && target_play == 0.0 {
            self.ring.pop(&mut []); // paused: play nothing, but let go of what a seek / load has voided
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
    /// Frames rendered per step of the callback.
    const SCRATCH_FRAMES: usize = 8192;
    let mut stereo = vec![0f32; SCRATCH_FRAMES * 2];
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                // In pieces of the scratch buffer: however large a buffer the device asks for, nothing is allocated here.
                for data in data.chunks_mut(SCRATCH_FRAMES * channels) {
                    let frames = data.len() / channels;
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
                }
            },
            |e| tracing::warn!("audio output error: {e}"),
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
        ring.deliver_events();
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
    fn audible_track_and_its_duration_follow_what_is_played() {
        let (tx, _rx) = channel();
        let ring = Ring::new(1000, tx);
        let with = |serial, d| Arc::new(TrackMeta { serial, url: format!("t{serial}"), duration_ms: d, gain_db: 0.0 });
        assert_eq!(ring.audible_duration_ms(), None);
        ring.push(vec![0.5; 2 * 100], with(1, Some(100)), 0.0, false);
        ring.push(Vec::new(), with(1, Some(100)), 100.0, true);
        ring.push(vec![0.5; 2 * 100], with(2, None), 0.0, false);
        ring.push(Vec::new(), with(2, None), 100.0, true);
        let mut out = vec![0f32; 2 * 60];
        ring.pop(&mut out);
        assert_eq!((ring.playing().map(|p| p.serial), ring.audible_duration_ms()), (Some(1), Some(Some(100))));
        ring.pop(&mut out); // crosses the boundary: track 1 ended, track 2 (no known length) is audible
        assert_eq!((ring.playing().map(|p| p.serial), ring.audible_duration_ms()), (Some(2), Some(None)));
        let mut out = vec![0f32; 2 * 200];
        assert_eq!(ring.pop(&mut out), 80);
        assert_eq!((ring.playing().map(|p| p.serial), ring.audible_duration_ms()), (None, None), "ended = no longer audible");
        // played-out chunks stay where they are until the feed side comes by: `pop` frees nothing
        let slot = |i: usize| unsafe { (*ring.slots[i].get()).is_some() };
        assert!((0..4).all(slot), "the output left all four chunks in place");
        ring.push(vec![0.0; 2], with(3, None), 0.0, false);
        assert!(!(0..4).any(slot) && slot(4), "the feed side freed them and queued the new one");
    }

    /// A flush cannot take chunks away from the output; it voids them. They must be skipped unheard and without
    /// track events, and a late write of the output must not undo the flush's position.
    #[test]
    fn a_flush_voids_what_is_queued_without_events_or_stale_positions() {
        let (tx, rx) = channel();
        let ring = Ring::new(1000, tx);
        ring.push(vec![0.5; 2 * 100], meta(1), 0.0, false);
        let mut out = vec![0f32; 2 * 10];
        assert_eq!(ring.pop(&mut out), 10);
        ring.push(Vec::new(), meta(1), 100.0, true);
        ring.push(vec![0.25; 2 * 50], meta(2), 0.0, false);
        // a seek in track 1: everything queued is void, track 1 stays the audible one
        ring.flush(40.0, false);
        assert_eq!((ring.frames(), ring.position_ms(), ring.is_drained()), (0, 40.0, true));
        assert_eq!(ring.playing().map(|p| p.serial), Some(1));
        assert_eq!(ring.pop(&mut out), 0, "nothing of the voided audio is played");
        assert_eq!(ring.position_ms(), 40.0);
        ring.push(vec![0.75; 2 * 20], meta(1), 40.0, false);
        assert_eq!(ring.frames(), 20);
        assert_eq!(ring.pop(&mut out), 10);
        assert_eq!((out[0], ring.position_ms(), ring.frames()), (0.75, 50.0, 10));
        // a load: the audible track is forgotten too
        ring.flush(0.0, true);
        assert!(ring.playing().is_none() && ring.audible_duration_ms().is_none());
        ring.deliver_events();
        let evs: Vec<Event> = rx.try_iter().collect();
        assert!(matches!(&evs[..], [Event::Started(a)] if a.serial == 1), "no Ended for 1, nothing for 2: {evs:?}");
    }

    #[test]
    fn more_chunks_than_the_queue_holds_are_kept_back_and_played_in_order() {
        let (tx, _rx) = channel();
        let ring = Ring::new(1000, tx);
        let n = SLOTS * 2 + 7;
        for i in 0..n {
            ring.push(vec![i as f32; 2], meta(1), i as f64, false);
        }
        assert_eq!(ring.frames(), n);
        let mut out = vec![0f32; 2 * 100];
        let mut next = 0;
        while next < n {
            let got = ring.pop(&mut out);
            assert!(got > 0, "stuck at {next}");
            for f in 0..got {
                assert_eq!(out[f * 2], (next + f) as f32);
            }
            next += got;
            assert_eq!(ring.frames(), n - next); // the feed side also moves the kept-back chunks into the queue
        }
        assert!(ring.is_drained());
    }

    /// The real thing: one thread plays, another feeds and flushes, nothing shared but the ring. Every frame that
    /// comes out must be the next one of the newest generation it has seen (never a voided one after a newer one,
    /// never out of order), and the track events must stay paired.
    #[test]
    fn output_and_feed_side_run_concurrently_without_a_lock() {
        let (tx, rx) = channel();
        let ring = Ring::new(48_000, tx);
        let stop = Arc::new(AtomicBool::new(false));
        let (ring2, stop2) = (ring.clone(), stop.clone());
        let player = std::thread::spawn(move || {
            let mut out = vec![0f32; 2 * 64];
            let (mut generation, mut expect, mut frames) = (0f32, 0f32, 0u64);
            while !stop2.load(Ordering::Relaxed) {
                let got = ring2.pop(&mut out);
                for f in out[..got * 2].as_chunks::<2>().0 {
                    // left = generation, right = running frame number within it
                    if f[0] != generation {
                        assert!(f[0] > generation, "audio of a voided generation {} after {generation}", f[0]);
                        (generation, expect) = (f[0], f[1]);
                    }
                    assert_eq!(f[1], expect, "generation {generation}: frames out of order");
                    expect += 1.0;
                }
                frames += got as u64;
                std::hint::spin_loop();
            }
            frames
        });
        let mut serial = 0;
        for generation in 1..=300u32 {
            serial += 1;
            let m = meta(serial);
            let mut n = 0f32;
            for _ in 0..(generation % 7 + 1) {
                let data: Vec<f32> = (0..32)
                    .flat_map(|_| {
                        n += 1.0;
                        [generation as f32, n - 1.0]
                    })
                    .collect();
                ring.push(data, m.clone(), 0.0, false);
            }
            ring.push(Vec::new(), m, 0.0, true);
            if generation % 3 == 0 {
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
            ring.flush(0.0, generation % 2 == 0);
            assert_eq!(ring.frames(), 0);
        }
        stop.store(true, Ordering::Relaxed);
        let played = player.join().expect("the output thread saw only valid audio");
        ring.deliver_events();
        let mut open = None;
        for e in rx.try_iter() {
            match e {
                Event::Started(m) => open = Some(m.serial),
                Event::Ended(m) => assert_eq!(open.take(), Some(m.serial), "Ended without its Started"),
                _ => {}
            }
        }
        println!("{played} frames played across 300 flushes");
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
