// SPDX-License-Identifier: GPL-3.0-only
//! The engine: a `Core` that turns tracks into a gapless / cross-faded stream of chunks for the output ring, a thread
//! that drives it from commands (never the Qt thread), and an offline renderer used by the tests.
use super::eq::{EqParams, EqShared};
use super::output::{self, Backend, Controls, Event, OutputDsp, Ring, SinkHandle, TrackMeta};
use super::source::{self, Interrupt};
use super::track::TrackStream;
use std::f64::consts::FRAC_PI_2;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

/// Frames produced per chunk.
const CHUNK: usize = 2048;
/// How far ahead of the speakers the feeder keeps the ring filled.
const AHEAD_MS: usize = 500;
/// Pre-decode this much of the next track so the switch cannot stall.
const NEXT_PREDECODE_MS: usize = 500;
/// Normalization is bounded: never cut more than 15 dB or boost more than 3 dB.
pub const NORMALIZE_MIN_DB: f64 = -15.0;
pub const NORMALIZE_MAX_DB: f64 = 3.0;

/// Live-adjustable behaviour, shared with the feeder.
pub struct Settings {
    crossfade_ms: AtomicU32,
    gapless: AtomicBool,
    normalize: AtomicBool,
}

impl Settings {
    pub fn new(crossfade_ms: u32, gapless: bool, normalize: bool) -> Self {
        Self {
            crossfade_ms: AtomicU32::new(crossfade_ms.min(12_000)),
            gapless: AtomicBool::new(gapless),
            normalize: AtomicBool::new(normalize),
        }
    }
    pub fn set_crossfade_ms(&self, v: u32) {
        self.crossfade_ms.store(v.min(12_000), Ordering::Relaxed);
    }
    pub fn set_gapless(&self, v: bool) {
        self.gapless.store(v, Ordering::Relaxed);
    }
    pub fn set_normalize(&self, v: bool) {
        self.normalize.store(v, Ordering::Relaxed);
    }
    pub fn crossfade_ms(&self) -> u32 {
        self.crossfade_ms.load(Ordering::Relaxed)
    }
    pub fn gapless(&self) -> bool {
        self.gapless.load(Ordering::Relaxed)
    }
    pub fn normalize(&self) -> bool {
        self.normalize.load(Ordering::Relaxed)
    }
}

/// Gains of the outgoing and incoming track at step `i` of an `n`-step crossfade. Raised-cosine law:
/// the two always sum to exactly 1, so a correlated overlap never gets louder or quieter.
pub fn fade_gains(i: usize, n: usize) -> (f32, f32) {
    let t = if n <= 1 { 1.0 } else { (i as f64 + 0.5) / n as f64 };
    let incoming = (FRAC_PI_2 * t).sin().powi(2);
    ((1.0 - incoming) as f32, incoming as f32)
}

/// The video id in a proxy URL `http://127.0.0.1:<port>/t/<token>/<id>` (or legacy `/<id>`).
pub fn video_id_from_url(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next()?;
    let id = path.rsplit('/').next()?;
    (1..=64).contains(&id.len()).then_some(id).filter(|i| i.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
}

/// Gain (dB) that evens out a track whose YouTube loudness is `loudness_db`: bring it to the reference level.
pub fn normalization_gain_db(loudness_db: Option<f64>) -> f64 {
    match loudness_db {
        Some(l) if l.is_finite() => (-l).clamp(NORMALIZE_MIN_DB, NORMALIZE_MAX_DB),
        _ => 0.0,
    }
}

struct Slot {
    stream: TrackStream,
    meta: Arc<TrackMeta>,
    gain: f32,
}

struct Fade {
    total: usize,
    done: usize,
}

/// Turns the current (and next) track into ring chunks.
pub struct Core {
    ring: Arc<Ring>,
    settings: Arc<Settings>,
    events: Sender<Event>,
    cur: Option<Slot>,
    next: Option<Slot>,
    fade: Option<Fade>,
    serial: u64,
    loudness: fn(&str) -> Option<f64>,
    /// The last track that was current, kept after it ended so a `seek` can restart it (repeat-one).
    last_url: Option<String>,
    /// A "next" track that arrived while a crossfade was running: it follows the incoming track, so it is
    /// installed the moment the fade is over.
    after_fade: Option<(TrackStream, String)>,
}

impl Core {
    pub fn new(ring: Arc<Ring>, settings: Arc<Settings>, events: Sender<Event>) -> Self {
        Self {
            ring,
            settings,
            events,
            cur: None,
            next: None,
            fade: None,
            serial: 0,
            loudness: crate::vision::loudness,
            last_url: None,
            after_fade: None,
        }
    }

    fn rate(&self) -> u32 {
        self.ring.rate()
    }

    fn slot(&mut self, stream: TrackStream, url: &str) -> Slot {
        self.serial += 1;
        let gain_db = if self.settings.normalize() { normalization_gain_db(video_id_from_url(url).and_then(self.loudness)) } else { 0.0 };
        let meta = Arc::new(TrackMeta { serial: self.serial, url: url.to_owned(), duration_ms: stream.duration_ms, gain_db });
        Slot { stream, meta, gain: 10f64.powf(gain_db / 20.0) as f32 }
    }

    /// Replace everything with `stream` (a `load`). Audio that was queued stops at once.
    pub fn load_now(&mut self, stream: TrackStream, url: &str) {
        self.ring.flush(0.0, true);
        let slot = self.slot(stream, url);
        let _ = self.events.send(Event::Loaded(slot.meta.clone()));
        self.last_url = Some(slot.meta.url.clone());
        self.cur = Some(slot);
        self.next = None;
        self.fade = None;
        self.after_fade = None;
    }

    /// The track that was playing most recently, even if it has finished.
    pub fn last_url(&self) -> Option<&str> {
        self.last_url.as_deref()
    }

    fn note_current(&mut self) {
        if let Some(c) = &self.cur {
            self.last_url = Some(c.meta.url.clone());
        }
    }

    /// Queue the track to follow the current one (pre-decoding its first moments).
    pub fn set_next_now(&mut self, stream: Option<(TrackStream, String)>) {
        if self.fade.is_some() {
            // already mixing into one: the request is about the track after the incoming one, so it waits for the fade
            self.after_fade = stream;
            return;
        }
        self.next = stream.map(|(mut s, url)| {
            s.fill(self.ring.rate() as usize * NEXT_PREDECODE_MS / 1000);
            self.slot(s, &url)
        });
    }

    /// The crossfade is over (or was cut short) and the incoming track is the current one: queue what was waiting.
    fn install_after_fade(&mut self) {
        if let Some(waiting) = self.after_fade.take() {
            self.set_next_now(Some(waiting));
        }
    }

    /// A following track is queued (or waiting for a crossfade to finish).
    pub fn has_next(&self) -> bool {
        self.next.is_some() || self.after_fade.is_some()
    }

    pub fn stop(&mut self) {
        self.ring.flush(0.0, true);
        self.cur = None;
        self.next = None;
        self.fade = None;
        self.after_fade = None;
    }

    /// The audible track when it is no longer the one being decoded: the feeder runs up to `AHEAD_MS` ahead of the
    /// speakers, so near a gapless boundary `cur` is already the following track while the previous one is still
    /// heard (and its `Ended` has not been reported). A seek is meant for what is heard.
    pub fn audible_behind_url(&self) -> Option<String> {
        let playing = self.ring.playing()?;
        let cur = self.cur.as_ref()?;
        let is_incoming = self.fade.is_some() && self.next.as_ref().is_some_and(|n| n.meta.serial == playing.serial);
        (playing.serial != cur.meta.serial && !is_incoming).then(|| playing.url.clone())
    }

    /// Nothing left to produce (the ring may still be playing out).
    pub fn is_idle(&self) -> bool {
        self.cur.is_none()
    }

    pub fn current_url(&self) -> Option<&str> {
        self.cur.as_ref().map(|s| s.meta.url.as_str())
    }

    /// Length of the track being decoded (`None` = there is none, `Some(None)` = it has no known length).
    pub fn current_duration_ms(&self) -> Option<Option<u64>> {
        self.cur.as_ref().map(|s| s.meta.duration_ms)
    }

    /// True when `url` is the track that is playing right now (so `load(url)` needs to do nothing): it happens
    /// after a gapless switch, when the UI catches up with the engine.
    pub fn is_playing_url(&self, url: &str) -> bool {
        // During a crossfade the incoming track is already what is heard while `cur` is still the outgoing one: the UI
        // asking for the incoming track must not cut the fade and restart it.
        if self.fade.is_some() && self.next.as_ref().is_some_and(|n| n.meta.url == url) {
            return true;
        }
        let playing = self.ring.playing();
        match (&self.cur, playing) {
            (Some(c), Some(p)) => c.meta.serial == p.serial && p.url == url,
            _ => false,
        }
    }

    /// Seek within the current track. `Err` = the stream could not be repositioned (caller may reopen it).
    pub fn seek(&mut self, ms: u64) -> Result<(), String> {
        if self.fade.take().is_some() {
            let audible = self.ring.playing().map(|p| p.serial);
            if audible.is_some() && audible == self.cur.as_ref().map(|c| c.meta.serial) {
                // The fade has been produced but none of it has been heard yet (and the outgoing track's `Ended` has
                // not been reported): the seek is meant for the outgoing track, which simply goes on. The incoming
                // one is rewound to wait its turn again; whatever was to follow it is not known to the UI yet.
                self.after_fade = None;
                if self.next.as_mut().is_some_and(|n| n.stream.seek(0).is_err()) {
                    self.next = None; // the caller queues it again
                }
            } else {
                // the incoming track is the one being heard; it becomes the current one
                self.cur = self.next.take().or(self.cur.take());
                self.note_current();
                self.install_after_fade();
            }
        }
        let Some(c) = self.cur.as_mut() else { return Ok(()) };
        c.stream.seek(ms)?;
        let pos = c.stream.position_ms();
        self.ring.flush(pos, false);
        Ok(())
    }

    /// Replace the current stream (after a failed seek forced a reopen) keeping its identity.
    pub fn replace_stream(&mut self, stream: TrackStream) {
        if let Some(c) = self.cur.as_mut() {
            c.stream = stream;
        }
    }

    pub fn position_ms(&self) -> f64 {
        self.ring.position_ms()
    }

    /// Produce chunks until the ring holds `ahead_frames` or nothing more can be made. Returns frames produced.
    pub fn pump(&mut self, ahead_frames: usize) -> usize {
        let mut total = 0;
        while self.ring.frames() < ahead_frames {
            let n = self.produce();
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    }

    fn crossfade_frames(&self) -> usize {
        self.settings.crossfade_ms() as usize * self.rate() as usize / 1000
    }

    /// One step; returns frames pushed (a state change without audio counts as 1 so callers keep going).
    fn produce(&mut self) -> usize {
        let xf_frames = if self.next.is_some() { self.crossfade_frames() } else { 0 };
        let Some(cur) = self.cur.as_mut() else { return 0 };
        // The fade needs the whole tail decoded in advance; do that only when the end is near (always if the length is unknown).
        let fade_ms = xf_frames as f64 * 1000.0 / f64::from(self.ring.rate());
        let far_from_end = cur.stream.duration_ms.is_some_and(|d| (d as f64 - cur.stream.position_ms()) > fade_ms + 8_000.0);
        cur.stream.fill(CHUNK + if far_from_end { 0 } else { xf_frames });

        // start a crossfade once the outgoing track's tail is no longer than the fade
        if self.fade.is_none() && xf_frames > 0 && cur.stream.at_end() && cur.stream.buffered() <= xf_frames && cur.stream.buffered() > 0 {
            let total = cur.stream.buffered();
            if let Some(n) = self.next.as_mut() {
                n.stream.fill(total + CHUNK);
            }
            // tell the world the outgoing track is over: an empty end marker
            self.ring.push(Vec::new(), cur.meta.clone(), cur.stream.position_ms(), true);
            self.fade = Some(Fade { total, done: 0 });
        }

        if let Some(f) = self.fade.as_mut() {
            let (Some(cur), Some(next)) = (self.cur.as_mut(), self.next.as_mut()) else {
                self.fade = None;
                return 1;
            };
            let n = CHUNK.min(f.total - f.done).min(cur.stream.buffered().max(1));
            next.stream.fill(n);
            if next.stream.starved(n) {
                return 0; // its read was interrupted by a newer command: wait, do not mix silence in its place
            }
            let (mut a, mut b) = (Vec::with_capacity(n * 2), Vec::with_capacity(n * 2));
            cur.stream.take(n, cur.gain, &mut a);
            let start = next.stream.position_ms();
            next.stream.take(n, next.gain, &mut b);
            // a track that ran short mixes in as silence
            a.resize(n * 2, 0.0);
            b.resize(n * 2, 0.0);
            let mut mix = Vec::with_capacity(n * 2);
            for i in 0..n {
                let (ga, gb) = fade_gains(f.done + i, f.total);
                mix.push(a[i * 2] * ga + b[i * 2] * gb);
                mix.push(a[i * 2 + 1] * ga + b[i * 2 + 1] * gb);
            }
            f.done += n;
            let finished = f.done >= f.total || cur.stream.buffered() == 0;
            self.ring.push(mix, next.meta.clone(), start, false);
            if finished {
                self.fade = None;
                self.cur = self.next.take();
                self.note_current();
                self.install_after_fade();
            }
            return n;
        }

        let gain = cur.gain;
        let start = cur.stream.position_ms();
        let mut data = Vec::with_capacity(CHUNK * 2);
        let n = cur.stream.take(CHUNK, gain, &mut data);
        if n > 0 {
            self.ring.push(data, cur.meta.clone(), start, false);
            return n;
        }
        if cur.stream.finished() {
            // the track is over (or broke): close it and, if gapless, continue with the next one in the same breath
            if let Some(err) = cur.stream.take_error() {
                let _ = self.events.send(Event::Failed(err));
                self.cur = None;
                self.next = None;
                return 0;
            }
            self.ring.push(Vec::new(), cur.meta.clone(), start, true);
            self.cur = if self.settings.gapless() { self.next.take() } else { None };
            self.next = None;
            self.note_current();
            return 1;
        }
        0
    }
}

// ---------------------------------------------------------------------------------------------
// the threaded engine
// ---------------------------------------------------------------------------------------------

enum Cmd {
    Load {
        url: String,
        start_ms: u64,
    },
    Opened {
        token: u64,
        url: String,
        result: Result<TrackStream, String>,
    },
    SetNext(Option<String>),
    NextOpened {
        token: u64,
        url: String,
        result: Result<TrackStream, String>,
    },
    Seek(u64),
    /// `play()` was asked for: if nothing is left to play, the last track starts over.
    Play,
    Stop,
    Shutdown,
}

/// Flags the Qt side reads without locking.
#[derive(Default)]
pub struct Shared {
    pub loading: AtomicBool,
    /// No track is being produced any more.
    pub idle: AtomicBool,
    duration_ms: AtomicU64,
}

impl Shared {
    pub fn duration_ms(&self) -> Option<u64> {
        Some(self.duration_ms.load(Ordering::Relaxed)).filter(|&d| d != u64::MAX)
    }
    pub fn set_duration_ms(&self, d: Option<u64>) {
        self.duration_ms.store(d.unwrap_or(u64::MAX), Ordering::Relaxed);
    }
}

pub struct Engine {
    tx: Sender<Cmd>,
    pub ctl: Arc<Controls>,
    pub shared: Arc<Shared>,
    pub settings: Arc<Settings>,
    ring: Arc<Ring>,
    pub underruns: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
    sink: Option<SinkHandle>,
    /// Breaks a network read the feeder thread is blocked in, so `stop` / `load` / `seek` never wait for one.
    interrupt: Arc<Interrupt>,
}

impl Engine {
    /// Open the output and start the feeder thread. Events (loaded / started / ended / failed) arrive on the receiver.
    pub fn start(backend: &Backend, settings: Arc<Settings>) -> Result<(Engine, Receiver<Event>), String> {
        Self::start_with_eq(backend, settings, super::eq::shared().clone())
    }

    pub fn start_with_eq(backend: &Backend, settings: Arc<Settings>, eq: Arc<EqShared>) -> Result<(Engine, Receiver<Event>), String> {
        let rate = output::probe_rate(backend)?;
        let (ev_tx, ev_rx) = channel();
        let ring = Ring::new(rate, ev_tx.clone());
        let ctl = Arc::new(Controls::default());
        let dsp = OutputDsp::with_eq(ring.clone(), ctl.clone(), eq);
        let underruns = dsp.underruns.clone();
        let sink = output::start(backend, dsp)?;
        let shared = Arc::new(Shared::default());
        shared.idle.store(true, Ordering::Relaxed);
        shared.set_duration_ms(None);
        let (tx, rx) = channel();
        let core = Core::new(ring.clone(), settings.clone(), ev_tx);
        let (tx2, shared2) = (tx.clone(), shared.clone());
        let interrupt = Arc::new(Interrupt::default());
        let interrupt2 = interrupt.clone();
        let thread = std::thread::Builder::new()
            .name("ytm-audio-feed".into())
            .spawn(move || feeder(core, rx, tx2, shared2, interrupt2))
            .map_err(|e| e.to_string())?;
        Ok((Engine { tx, ctl, shared, settings, ring, underruns, thread: Some(thread), sink: Some(sink), interrupt }, ev_rx))
    }

    pub fn rate(&self) -> u32 {
        self.ring.rate()
    }

    pub fn load(&self, url: &str, start_ms: u64) {
        self.interrupt.raise();
        self.shared.loading.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Load { url: url.to_owned(), start_ms });
    }
    pub fn set_next(&self, url: Option<&str>) {
        let _ = self.tx.send(Cmd::SetNext(url.map(str::to_owned)));
    }
    pub fn seek(&self, ms: u64) {
        self.interrupt.raise();
        let _ = self.tx.send(Cmd::Seek(ms));
    }
    pub fn stop(&self) {
        self.interrupt.raise();
        self.ctl.playing.store(false, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Stop);
    }
    pub fn play(&self) {
        self.ctl.playing.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Play);
    }
    pub fn pause(&self) {
        self.ctl.playing.store(false, Ordering::Relaxed);
    }
    pub fn set_volume(&self, v: f32) {
        self.ctl.set_volume(v);
    }
    pub fn position_ms(&self) -> f64 {
        self.ring.position_ms()
    }

    /// A cheap, cloneable view for a status-polling thread (does not keep the engine alive).
    pub fn status(&self) -> Status {
        Status { ring: self.ring.clone(), ctl: self.ctl.clone(), shared: self.shared.clone() }
    }
}

/// Read-only view of the engine's state.
#[derive(Clone)]
pub struct Status {
    ring: Arc<Ring>,
    ctl: Arc<Controls>,
    shared: Arc<Shared>,
}

impl Status {
    pub fn position_ms(&self) -> i64 {
        self.ring.position_ms().round() as i64
    }
    /// Length of the track that is heard; before anything is heard (or after the last track), of the one that was loaded.
    pub fn duration_ms(&self) -> i64 {
        self.ring.audible_duration_ms().unwrap_or_else(|| self.shared.duration_ms()).map_or(0, |d| d as i64)
    }
    pub fn playing(&self) -> bool {
        self.ctl.playing.load(Ordering::Relaxed)
    }
    pub fn loading(&self) -> bool {
        self.shared.loading.load(Ordering::Relaxed)
    }
    /// Nothing is left to play (end of the queue of one).
    pub fn idle(&self) -> bool {
        self.shared.idle.load(Ordering::Relaxed)
    }
    pub fn set_playing(&self, v: bool) {
        self.ctl.playing.store(v, Ordering::Relaxed);
    }
    /// Hand on the track events the output has recorded (see `Ring::deliver_events`); for a status poller.
    pub fn deliver_events(&self) {
        self.ring.deliver_events();
    }
    /// Metadata of the track whose audio is coming out right now.
    pub fn playing_meta(&self) -> Option<Arc<TrackMeta>> {
        self.ring.playing()
    }
}

impl Engine {
    /// Close the output device now (quick), without waiting for the feeder thread. The engine is of no use afterwards.
    pub fn close_output(&mut self) {
        self.ctl.playing.store(false, Ordering::Relaxed);
        self.sink = None;
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.interrupt.close(); // a blocked read gives up at once, so the join below is prompt
        let _ = self.tx.send(Cmd::Shutdown);
        // The device first: the feeder may be in the middle of a network read and take a while to notice.
        self.sink = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn spawn_open(tx: Sender<Cmd>, url: String, rate: u32, token: u64, next: bool) {
    let _ = std::thread::Builder::new().name("ytm-audio-open".into()).spawn(move || {
        let result = TrackStream::open_url(&url, rate);
        let _ = tx.send(if next { Cmd::NextOpened { token, url, result } } else { Cmd::Opened { token, url, result } });
    });
}

fn feeder(mut core: Core, rx: Receiver<Cmd>, tx: Sender<Cmd>, shared: Arc<Shared>, interrupt: Arc<Interrupt>) {
    // Network reads on this thread give up as soon as a stop / load / seek is on its way (see `Interrupt`).
    source::install_interrupt(Some(interrupt.clone()));
    let rate = core.rate();
    let ahead = rate as usize * AHEAD_MS / 1000;
    let (mut load_token, mut next_token) = (0u64, 0u64);
    let mut pending_seek: Option<u64> = None;
    let mut next_url: Option<String> = None;
    loop {
        let cmd = rx.recv_timeout(Duration::from_millis(if core.ring.frames() < ahead && !core.is_idle() { 0 } else { 15 }));
        let mut cmds = Vec::new();
        match cmd {
            Ok(c) => cmds.push(c),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            Err(_) => {}
        }
        cmds.extend(rx.try_iter());
        for c in cmds {
            // A track to start over at a position because its stream is gone: (url, ms, keep the queued next track).
            let mut restart: Option<(String, u64, bool)> = None;
            if matches!(c, Cmd::Stop | Cmd::Load { .. } | Cmd::Seek(_)) {
                // taken up: from here on reads on this thread may wait again (unless another such command is queued)
                interrupt.ack();
            }
            match c {
                Cmd::Shutdown => return,
                Cmd::Stop => {
                    load_token += 1;
                    next_token += 1;
                    pending_seek = None;
                    next_url = None;
                    core.stop();
                    core.last_url = None;
                    shared.set_duration_ms(None);
                    shared.loading.store(false, Ordering::Relaxed);
                    shared.idle.store(true, Ordering::Relaxed);
                }
                Cmd::Load { url, start_ms } => {
                    if core.is_playing_url(&url) {
                        // the engine already moved on to this track by itself (gapless): just acknowledge
                        shared.loading.store(false, Ordering::Relaxed);
                        continue;
                    }
                    load_token += 1;
                    next_token += 1;
                    pending_seek = (start_ms > 0).then_some(start_ms);
                    next_url = None;
                    core.stop();
                    // the previous track is gone for good: if this one fails to open, `play`/`seek` must not bring it back
                    core.last_url = None;
                    shared.idle.store(false, Ordering::Relaxed);
                    spawn_open(tx.clone(), url, rate, load_token, false);
                }
                Cmd::Opened { token, url, result } => {
                    if token != load_token {
                        continue; // superseded
                    }
                    shared.loading.store(false, Ordering::Relaxed);
                    match result {
                        Ok(stream) => {
                            shared.set_duration_ms(stream.duration_ms);
                            core.load_now(stream, &url);
                            if let Some(ms) = pending_seek.take() {
                                seek_with_reopen(&mut core, ms, rate);
                            }
                            if let Some(n) = next_url.clone() {
                                next_token += 1;
                                spawn_open(tx.clone(), n, rate, next_token, true);
                            }
                        }
                        Err(e) => {
                            shared.idle.store(true, Ordering::Relaxed);
                            let _ = core.events.send(Event::Failed(e));
                        }
                    }
                }
                Cmd::SetNext(url) => {
                    next_token += 1;
                    next_url = url.clone();
                    match url {
                        Some(u) if !core.is_idle() => spawn_open(tx.clone(), u, rate, next_token, true),
                        Some(_) => {} // applied once the current track has loaded
                        None => core.set_next_now(None),
                    }
                }
                Cmd::NextOpened { token, url, result } => {
                    if token != next_token {
                        continue;
                    }
                    // a track that cannot be pre-opened is not an error now; it fails properly when it is loaded
                    if let Ok(stream) = result {
                        core.set_next_now(Some((stream, url)));
                    }
                }
                Cmd::Seek(ms) => {
                    if let Some(url) = core.audible_behind_url() {
                        // The feeder has already moved on to the following track, but the one that is heard (and
                        // whose `Ended` has not been reported) is still the previous one: the seek is meant for that.
                        // Its stream is gone, so it starts over at the position; the following track is queued again.
                        restart = Some((url, ms, true));
                    } else if core.current_url().is_some() {
                        let had_next = core.has_next();
                        seek_with_reopen(&mut core, ms, rate);
                        if let Some(n) = next_url.clone().filter(|_| had_next && !core.has_next()) {
                            // a seek back into the outgoing track of a crossfade could not rewind the incoming one
                            next_token += 1;
                            spawn_open(tx.clone(), n, rate, next_token, true);
                        }
                    } else if shared.loading.load(Ordering::Relaxed) {
                        pending_seek = Some(ms);
                    } else if let Some(url) = core.last_url().map(str::to_owned) {
                        // the track has finished: seeking in it (repeat-one, "play again") starts it over from there
                        restart = Some((url, ms, false));
                    }
                }
                Cmd::Play => {
                    // Nothing left to play (the last track ran out, or broke) and nothing on its way: "play" means
                    // play the last track again, never a silent engine that claims to be playing.
                    let nothing = core.is_idle() && core.ring.is_drained() && !shared.loading.load(Ordering::Relaxed);
                    if let Some(url) = core.last_url().map(str::to_owned).filter(|_| nothing) {
                        restart = Some((url, 0, false));
                    }
                }
            }
            if let Some((url, ms, keep_next)) = restart {
                load_token += 1;
                next_token += 1;
                pending_seek = (ms > 0).then_some(ms);
                if !keep_next {
                    next_url = None;
                }
                core.stop();
                core.ring.set_position_ms(ms as f64); // show the target at once, not 0 until the stream is open
                shared.loading.store(true, Ordering::Relaxed);
                shared.idle.store(false, Ordering::Relaxed);
                spawn_open(tx.clone(), url, rate, load_token, false);
            }
        }
        core.pump(ahead);
        if let Some(d) = core.current_duration_ms() {
            // the track being decoded; the audible one (which may still be the previous track) is reported by the ring
            shared.set_duration_ms(d);
        }
        if core.is_idle() && core.ring.frames() == 0 && !shared.loading.load(Ordering::Relaxed) {
            shared.idle.store(true, Ordering::Relaxed);
        }
    }
}

/// Seek; if the demuxer refuses (e.g. after it hit the end of the stream) reopen the URL and seek in the fresh stream.
fn seek_with_reopen(core: &mut Core, ms: u64, rate: u32) {
    if core.seek(ms).is_ok() {
        return;
    }
    // Asked only now: a seek during a crossfade makes the incoming track the current one, and it is that one's
    // stream that has to be reopened, not the outgoing track's.
    let Some(url) = core.current_url().map(str::to_owned) else { return };
    match TrackStream::open_url(&url, rate) {
        Ok(s) => {
            core.replace_stream(s);
            if let Err(e) = core.seek(ms) {
                let _ = core.events.send(Event::Failed(e));
            }
        }
        Err(e) => {
            let _ = core.events.send(Event::Failed(e));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// offline rendering (tests, tools)
// ---------------------------------------------------------------------------------------------

pub struct RenderOpts {
    pub rate: u32,
    pub volume: f32,
    pub eq: EqParams,
    pub crossfade_ms: u32,
    pub gapless: bool,
    pub normalize: bool,
    /// Loudness lookup for normalization (video id -> dB); `None` uses the app's cache.
    pub loudness: Option<fn(&str) -> Option<f64>>,
    /// Stop after this many seconds of output.
    pub max_seconds: f64,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            rate: 48_000,
            volume: 1.0,
            eq: EqParams::default(),
            crossfade_ms: 0,
            gapless: true,
            normalize: false,
            loudness: None,
            max_seconds: 120.0,
        }
    }
}

pub struct Rendered {
    pub rate: u32,
    /// Interleaved stereo.
    pub samples: Vec<f32>,
    pub events: Vec<Event>,
    /// Output-frame index at which each `Started` event happened (track boundaries).
    pub boundaries: Vec<usize>,
}

/// Play `urls` in order through the whole pipeline (decode, resample, gapless / crossfade, EQ, limiter, volume) into
/// memory instead of a device. The first is loaded, the second queued as "next" (like the app does).
pub fn render(urls: &[&str], o: &RenderOpts) -> Result<Rendered, String> {
    let (ev_tx, ev_rx) = channel();
    let ring = Ring::new(o.rate, ev_tx.clone());
    let ctl = Arc::new(Controls::default());
    ctl.set_volume(o.volume);
    let eq = Arc::new(EqShared::new(o.eq.clone()));
    let mut dsp = OutputDsp::with_eq(ring.clone(), ctl.clone(), eq);
    let settings = Arc::new(Settings::new(o.crossfade_ms, o.gapless, o.normalize));
    let mut core = Core::new(ring.clone(), settings, ev_tx);
    if let Some(l) = o.loudness {
        core.loudness = l;
    }
    let first = urls.first().ok_or("nothing to render")?;
    core.load_now(TrackStream::open_url(first, o.rate)?, first);
    if let Some(second) = urls.get(1) {
        core.set_next_now(Some((TrackStream::open_url(second, o.rate)?, (*second).to_owned())));
    }
    ctl.playing.store(true, Ordering::Relaxed);
    let mut samples = Vec::new();
    let mut boundaries = Vec::new();
    let mut events = Vec::new();
    let mut block = vec![0f32; 2 * 1024];
    let limit = (f64::from(o.rate) * o.max_seconds) as usize;
    loop {
        core.pump(o.rate as usize / 4);
        dsp.render(&mut block);
        ring.deliver_events(); // the output only records track events; this hands them on
        let before = samples.len() / 2;
        samples.extend_from_slice(&block);
        for e in ev_rx.try_iter() {
            if matches!(e, Event::Started(_)) {
                boundaries.push(before);
            }
            events.push(e);
        }
        if (core.is_idle() && ring.frames() == 0) || samples.len() / 2 >= limit {
            break;
        }
    }
    // drop trailing digital silence added by the last, partially filled block
    while samples.len() >= 2 && samples[samples.len() - 1] == 0.0 && samples[samples.len() - 2] == 0.0 {
        samples.truncate(samples.len() - 2);
    }
    if let Some(Event::Failed(msg)) = events.iter().find(|e| matches!(e, Event::Failed(_))) {
        return Err(msg.clone());
    }
    Ok(Rendered { rate: o.rate, samples, events, boundaries })
}

/// `render` into a 32-bit-float WAV file.
pub fn render_to_wav(urls: &[&str], o: &RenderOpts, path: &std::path::Path) -> Result<Rendered, String> {
    let r = render(urls, o)?;
    output::write_wav(path, r.rate, &r.samples).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::analysis::{channel, tone_fit};
    use crate::audio::codec_tests::fixture;

    #[test]
    fn crossfade_gains_sum_to_one_and_are_monotonic() {
        for n in [1usize, 2, 17, 480, 48_000, 576_000] {
            let mut last = (1.1f32, -0.1f32);
            for i in (0..n).step_by((n / 500).max(1)) {
                let (a, b) = fade_gains(i, n);
                assert!((a + b - 1.0).abs() < 1e-6, "n={n} i={i}: {a}+{b}");
                assert!(a >= 0.0 && b >= 0.0 && a <= 1.0 && b <= 1.0);
                assert!(a <= last.0 + 1e-6 && b >= last.1 - 1e-6, "monotonic");
                last = (a, b);
            }
        }
        let (a0, b0) = fade_gains(0, 1000);
        let (a1, b1) = fade_gains(999, 1000);
        assert!(a0 > 0.999 && b0 < 0.001 && a1 < 0.001 && b1 > 0.999);
    }

    #[test]
    fn video_ids_come_from_proxy_urls() {
        assert_eq!(video_id_from_url("http://127.0.0.1:41234/t/abcdef0123/dQw4w9WgXcQ"), Some("dQw4w9WgXcQ"));
        assert_eq!(video_id_from_url("http://127.0.0.1:1/legacyId"), Some("legacyId"));
        assert_eq!(video_id_from_url("http://h/t/tok/id?x=1"), Some("id"));
        assert_eq!(video_id_from_url("http://h/t/tok/"), None);
        assert_eq!(video_id_from_url("/tmp/some file.webm"), None);
    }

    #[test]
    fn normalization_gain_is_bounded() {
        assert_eq!(normalization_gain_db(None), 0.0);
        assert_eq!(normalization_gain_db(Some(f64::NAN)), 0.0);
        assert_eq!(normalization_gain_db(Some(5.0)), -5.0, "loud track is turned down");
        assert_eq!(normalization_gain_db(Some(-2.0)), 2.0, "quiet track is turned up a little");
        assert_eq!(normalization_gain_db(Some(40.0)), NORMALIZE_MIN_DB);
        assert_eq!(normalization_gain_db(Some(-40.0)), NORMALIZE_MAX_DB);
    }

    fn opts() -> RenderOpts {
        RenderOpts::default()
    }

    #[test]
    fn single_track_render_is_the_decoded_audio_through_a_transparent_chain() {
        let r = render(&[&fixture("sine440_opus.webm")], &opts()).unwrap();
        let l = channel(&r.samples, 2, 0);
        let (a, snr) = tone_fit(&l[9600..l.len() - 9600], 48_000.0, 440.0);
        assert!((a - 0.0884).abs() < 0.003 && snr > 40.0, "amp {a} snr {snr}");
        assert!((r.samples.len() as i64 / 2 - 96_648).abs() < 2000);
        assert!(matches!(r.events.first(), Some(Event::Loaded(_))));
        assert!(r.events.iter().any(|e| matches!(e, Event::Ended(_))));
    }

    /// Largest jump between neighbouring samples in a window.
    fn max_step(x: &[f32]) -> f32 {
        x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn gapless_boundary_has_no_gap_and_no_click() {
        // two different tones back to back: 440 Hz then 880 Hz, both 2 s
        let (a, b) = (fixture("sine440_opus.webm"), fixture("sine880_opus.webm"));
        let r = render(&[&a, &b], &opts()).unwrap();
        let l = channel(&r.samples, 2, 0);
        assert_eq!(r.boundaries.len(), 2, "two Started events: {:?}", r.boundaries);
        let boundary = r.boundaries[1];
        // the first track is ~96.6k frames; the boundary lies right after it (+ ring latency <= one block)
        assert!((boundary as i64 - 96_648).abs() < 3000, "boundary at {boundary}");
        // The engine plays track A's last frame and then B's first frame with nothing in between: find the longest
        // run of exact zeros around the seam (the Opus codec's own start-up silence is < 5 ms)
        let seam = &l[boundary.saturating_sub(2000)..(boundary + 2000).min(l.len())];
        let mut longest = 0;
        let mut run = 0;
        for s in seam {
            if s.abs() < 1e-7 {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        assert!(longest < 480, "a gap of {longest} zero samples at the seam");
        // no click: neighbouring samples never jump more than the loudest natural step of the 880 Hz tone
        let natural = 2.0 * PI32 * 880.0 / 48_000.0 * 0.0884;
        let step = max_step(seam);
        assert!(step < natural * 1.5, "click at the seam: step {step} vs natural {natural}");
        // both tones are intact on their side of the boundary
        let (a440, s1) = tone_fit(&l[9600..boundary - 9600], 48_000.0, 440.0);
        let (a880, s2) = tone_fit(&l[boundary + 9600..l.len() - 9600], 48_000.0, 880.0);
        assert!(a440 > 0.08 && a880 > 0.08 && s1 > 35.0 && s2 > 35.0, "{a440} {s1} {a880} {s2}");
    }

    const PI32: f32 = std::f32::consts::PI;

    #[test]
    fn gapless_across_codecs_and_sample_rates() {
        // Opus (48 kHz) into AAC (44.1 kHz) rendered at 44.1 kHz: both get resampled to the device rate
        let (a, b) = (fixture("sine440_opus.webm"), fixture("sine440_aac.m4a"));
        let mut o = opts();
        o.rate = 44_100;
        let r = render(&[&a, &b], &o).unwrap();
        let l = channel(&r.samples, 2, 0);
        let boundary = r.boundaries[1];
        let (amp, snr) = tone_fit(&l[boundary + 4410..l.len() - 4410], 44_100.0, 440.0);
        assert!((amp - 0.0884).abs() < 0.005 && snr > 30.0, "{amp} {snr}");
        let seam = &l[boundary - 500..boundary + 500];
        assert!(max_step(seam) < 0.1, "step {}", max_step(seam));
    }

    /// The UI asks for the next track as soon as the previous one ends, which in a crossfade is while the fade is
    /// still running. The engine must recognise the incoming track as the one that is playing, not reload it.
    #[test]
    fn asking_for_the_incoming_track_during_a_crossfade_does_not_restart_it() {
        let (a, b) = (fixture("sine440_opus.webm"), fixture("sine880_opus.webm"));
        let rate = 48_000u32;
        let (ev_tx, _ev_rx) = std::sync::mpsc::channel();
        let ring = Ring::new(rate, ev_tx.clone());
        let o = RenderOpts::default();
        let ctl = Arc::new(Controls::default());
        ctl.set_volume(o.volume);
        let mut dsp = OutputDsp::with_eq(ring.clone(), ctl.clone(), Arc::new(EqShared::new(o.eq.clone())));
        let mut core = Core::new(ring, Arc::new(Settings::new(1000, true, false)), ev_tx);
        core.load_now(TrackStream::open_url(&a, rate).unwrap(), &a);
        core.set_next_now(Some((TrackStream::open_url(&b, rate).unwrap(), b.clone())));
        ctl.playing.store(true, Ordering::Relaxed);
        // run (consuming the output, as a sound card would) until the crossfade has started
        let mut block = vec![0f32; 2 * 1024];
        for _ in 0..2000 {
            if core.fade.is_some() {
                break;
            }
            core.pump(rate as usize / 4);
            dsp.render(&mut block);
        }
        assert!(core.fade.is_some(), "the fade never started");
        assert!(core.is_playing_url(&b), "the incoming track is already being heard: loading it must be a no-op");
        assert!(!core.is_playing_url("https://example.invalid/some-other-track"), "an unrelated url is still a real load");
    }

    /// A core with its output, fed from fixture files: `(core, dsp, ring, events)`.
    fn rig(crossfade_ms: u32) -> (Core, OutputDsp, Arc<Ring>, Receiver<Event>) {
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        let ring = Ring::new(48_000, ev_tx.clone());
        let ctl = Arc::new(Controls::default());
        ctl.playing.store(true, Ordering::Relaxed);
        let dsp = OutputDsp::with_eq(ring.clone(), ctl, Arc::new(EqShared::new(EqParams::default())));
        (Core::new(ring.clone(), Arc::new(Settings::new(crossfade_ms, true, false)), ev_tx), dsp, ring, ev_rx)
    }

    /// The UI hands over the track after the incoming one while the fade into the incoming one is still running.
    /// It used to be thrown away, so every second transition had no crossfade (and a gap).
    #[test]
    fn a_next_track_that_arrives_during_a_crossfade_is_queued_when_the_fade_is_over() {
        let (a, b, c) = (fixture("sine440_opus.webm"), fixture("sine880_opus.webm"), fixture("chirp_opus.webm"));
        let (mut core, mut dsp, ring, ev_rx) = rig(1000);
        core.load_now(TrackStream::open_url(&a, 48_000).unwrap(), &a);
        core.set_next_now(Some((TrackStream::open_url(&b, 48_000).unwrap(), b.clone())));
        let mut block = vec![0f32; 2 * 1024];
        for _ in 0..2000 {
            if core.fade.is_some() {
                break;
            }
            core.pump(12_000);
            dsp.render(&mut block);
        }
        assert!(core.fade.is_some(), "the fade never started");
        core.set_next_now(Some((TrackStream::open_url(&c, 48_000).unwrap(), c.clone())));
        assert!(core.has_next());
        for _ in 0..2000 {
            if core.fade.is_none() {
                break;
            }
            core.pump(12_000);
            dsp.render(&mut block);
        }
        assert_eq!(core.current_url(), Some(b.as_str()), "the incoming track is the current one after the fade");
        assert_eq!(core.next.as_ref().map(|n| n.meta.url.as_str()), Some(c.as_str()), "the track after it is queued");
        // and it is really played: three tracks start, the last one is C
        for _ in 0..4000 {
            if core.is_idle() {
                break;
            }
            core.pump(12_000);
            dsp.render(&mut block);
        }
        ring.deliver_events();
        let started: Vec<String> =
            ev_rx.try_iter().filter_map(|e| if let Event::Started(m) = e { Some(m.url.clone()) } else { None }).collect();
        assert_eq!(started, [a, b, c]);
    }

    /// The feeder decodes ahead of the speakers: near a gapless boundary it is already in the next track while the
    /// previous one is still heard. A seek then is meant for the previous track, and its `Ended` must not get lost.
    #[test]
    fn near_a_gapless_boundary_the_audible_track_is_the_seek_target() {
        let (a, b) = (fixture("sine440_opus.webm"), fixture("sine880_opus.webm"));
        let (mut core, mut dsp, ring, ev_rx) = rig(0);
        core.load_now(TrackStream::open_url(&a, 48_000).unwrap(), &a);
        core.set_next_now(Some((TrackStream::open_url(&b, 48_000).unwrap(), b.clone())));
        let mut block = vec![0f32; 2 * 1024];
        dsp.render(&mut block); // nothing queued yet
        assert_eq!(core.audible_behind_url(), None, "nothing is audible yet");
        // decode far ahead without playing much: the core moves on to B while A is what comes out
        core.pump(48_000);
        dsp.render(&mut block);
        assert_eq!(core.audible_behind_url(), None, "A is audible and A is being decoded");
        core.pump(48_000 * 3);
        assert_eq!(core.current_url(), Some(b.as_str()));
        assert_eq!(core.audible_behind_url().as_deref(), Some(a.as_str()), "A is still the audible track");
        // play A out: the moment its Ended is reported it stops being the audible track, and B takes over
        let mut ended_a = false;
        for _ in 0..200 {
            dsp.render(&mut block);
            ring.deliver_events();
            for e in ev_rx.try_iter() {
                if matches!(&e, Event::Ended(m) if m.url == a) {
                    ended_a = true;
                    assert!(ring.playing().is_none_or(|p| p.url == b), "an ended track is never reported as audible");
                }
            }
            if ended_a {
                break;
            }
            assert_eq!(core.audible_behind_url().as_deref(), Some(a.as_str()), "until A's Ended, a seek is for A");
        }
        assert!(ended_a);
        dsp.render(&mut block);
        assert_eq!(core.audible_behind_url(), None, "B is audible and current: a seek is for B");
        assert!(core.is_playing_url(&b));
    }

    /// Same boundary with a crossfade: the fade has been produced but not heard, so a seek goes on in the outgoing
    /// track and the incoming one waits its turn again (from its start).
    #[test]
    fn a_seek_before_the_crossfade_is_heard_stays_in_the_outgoing_track() {
        let (a, b) = (fixture("chirp_opus.webm"), fixture("sine880_opus.webm"));
        let (mut core, mut dsp, ring, ev_rx) = rig(1000);
        core.load_now(TrackStream::open_url(&a, 48_000).unwrap(), &a);
        core.set_next_now(Some((TrackStream::open_url(&b, 48_000).unwrap(), b.clone())));
        let mut block = vec![0f32; 2 * 1024];
        core.pump(4096);
        dsp.render(&mut block); // A is audible
        for _ in 0..2000 {
            if core.fade.is_some() {
                break;
            }
            core.produce(); // far ahead of the output, nothing consumed: the fade starts unheard
        }
        assert!(core.fade.is_some());
        seek_with_reopen(&mut core, 1000, 48_000); // as the feeder does: a stream that hit its end is reopened
        assert!(core.fade.is_none());
        assert_eq!(core.current_url(), Some(a.as_str()), "the outgoing track goes on");
        assert!((core.position_ms() - 1000.0).abs() < 1.0);
        assert_eq!(core.next.as_ref().map(|n| n.stream.position_ms()), Some(0.0), "the incoming track starts over later");
        ring.deliver_events();
        assert!(!ev_rx.try_iter().any(|e| matches!(e, Event::Ended(_))), "A has not ended");
        // once the fade is heard, the same seek belongs to the incoming track
        for _ in 0..4000 {
            core.pump(12_000);
            dsp.render(&mut block);
            if core.fade.is_some() && ring.playing().is_some_and(|p| p.url == b) {
                break;
            }
        }
        assert!(core.fade.is_some() && ring.playing().is_some_and(|p| p.url == b), "the fade is being heard");
        core.seek(500).unwrap();
        assert_eq!(core.current_url(), Some(b.as_str()));
        assert!((core.position_ms() - 500.0).abs() < 1.0);
    }

    #[test]
    fn crossfade_overlaps_the_tracks_and_stays_smooth() {
        let (a, b) = (fixture("sine440_opus.webm"), fixture("sine880_opus.webm"));
        let plain = render(&[&a, &b], &opts()).unwrap();
        let mut o = opts();
        o.crossfade_ms = 1000;
        let r = render(&[&a, &b], &o).unwrap();
        // 1 s of overlap makes the whole thing ~1 s shorter
        let diff = plain.samples.len() as i64 / 2 - r.samples.len() as i64 / 2;
        assert!((diff - 48_000).abs() < 4000, "overlap shortened the output by {diff} frames");
        let l = channel(&r.samples, 2, 0);
        // before the fade only 440 Hz, after it only 880 Hz, and in the middle both
        let (a440, _) = tone_fit(&l[10_000..40_000], 48_000.0, 440.0);
        assert!(a440 > 0.08);
        let mid = r.boundaries[1] + 24_000;
        let (m440, _) = tone_fit(&l[mid..mid + 4800], 48_000.0, 440.0);
        let (m880, _) = tone_fit(&l[mid..mid + 4800], 48_000.0, 880.0);
        assert!(m440 > 0.02 && m880 > 0.02 && m440 < 0.07 && m880 < 0.07, "mid-fade: 440 -> {m440}, 880 -> {m880}");
        assert!(max_step(&l) < 0.2, "no step anywhere: {}", max_step(&l));
        assert!(l.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn normalization_applies_the_per_track_gain() {
        fn quiet(_: &str) -> Option<f64> {
            Some(6.0) // 6 dB louder than the reference -> turned down 6 dB
        }
        let f = fixture("sine440_opus.webm");
        let mut o = opts();
        let flat = render(&[&f], &o).unwrap();
        o.normalize = true;
        o.loudness = Some(quiet);
        // the file path has no video id in the proxy shape, so use a path whose last segment is a valid id
        let dir = std::env::temp_dir().join(format!("ytm-norm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let alias = dir.join("trackid_x");
        std::fs::copy(&f, &alias).unwrap();
        let norm = render(&[alias.to_str().unwrap()], &o).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let (a1, _) = tone_fit(&channel(&flat.samples, 2, 0)[9600..60_000], 48_000.0, 440.0);
        let (a2, _) = tone_fit(&channel(&norm.samples, 2, 0)[9600..60_000], 48_000.0, 440.0);
        let db = 20.0 * (a2 / a1).log10();
        assert!((db + 6.0).abs() < 0.2, "normalised by {db} dB");
    }

    #[test]
    fn volume_and_eq_apply_in_the_chain() {
        let f = fixture("sine440_opus.webm");
        let base = render(&[&f], &opts()).unwrap();
        let mut o = opts();
        o.volume = 0.5;
        let half = render(&[&f], &o).unwrap();
        let (a1, _) = tone_fit(&channel(&base.samples, 2, 0)[9600..60_000], 48_000.0, 440.0);
        let (a2, _) = tone_fit(&channel(&half.samples, 2, 0)[9600..60_000], 48_000.0, 440.0);
        assert!((a2 / a1 - 0.5).abs() < 0.01);
        let mut o = opts();
        o.volume = 0.0;
        assert!(render(&[&f], &o).unwrap().samples.iter().all(|&s| s == 0.0) || render(&[&f], &o).unwrap().samples.is_empty());
        let mut o = opts();
        o.eq = EqParams {
            enabled: true,
            bands_db: {
                let mut b = [0.0; 10];
                b[5] = 12.0;
                b[4] = 6.0;
                b[6] = 6.0;
                b
            },
            ..Default::default()
        };
        let boosted = render(&[&f], &o).unwrap();
        let (a3, _) = tone_fit(&channel(&boosted.samples, 2, 0)[9600..60_000], 48_000.0, 440.0);
        assert!(a3 > a1 * 1.5, "EQ boost around 440 Hz: {a1} -> {a3}");
    }

    #[test]
    fn nothing_to_render_and_missing_files_are_errors() {
        assert!(render(&[], &opts()).is_err());
        assert!(render(&["/no/such/file.webm"], &opts()).is_err());
    }
}
