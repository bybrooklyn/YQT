// SPDX-License-Identifier: GPL-3.0-only
//! `AudioEngine`: the QML face of the Rust audio engine. Commands are cheap (atomics / a channel send); the device is
//! opened on a helper thread and a status thread feeds properties and signals back through `qt_thread()`, so the Qt
//! thread never waits on audio, the network or a decoder.
use crate::audio::engine::{Engine, Settings, Status};
use crate::audio::output::{self, Backend, Event};
use crate::config;
use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QStringList};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(i64, position)] // ms, from the engine's sample-accurate frame counter
        #[qproperty(i64, duration)] // ms, 0 = unknown
        #[qproperty(bool, playing)]
        #[qproperty(bool, loading)]
        #[qproperty(bool, stalled)] // asked to play but the position is not advancing (opening, buffering)
        #[qproperty(QString, error)]
        type AudioEngine = super::AudioEngineRust;

        /// Open `url` (the stream proxy URL). If it is the track the engine already moved on to by itself (gapless), nothing restarts.
        #[qinvokable]
        fn load(self: Pin<&mut AudioEngine>, url: &QString);
        #[qinvokable]
        fn play(self: Pin<&mut AudioEngine>);
        #[qinvokable]
        fn pause(self: Pin<&mut AudioEngine>);
        #[qinvokable]
        fn stop(self: Pin<&mut AudioEngine>);
        #[qinvokable]
        fn seek(self: Pin<&mut AudioEngine>, ms: i64);
        /// Linear 0..1.
        #[qinvokable]
        fn set_volume(self: Pin<&mut AudioEngine>, v: f64);
        /// The track to continue with seamlessly (empty = none). Its start is decoded ahead of time.
        #[qinvokable]
        fn set_next(self: Pin<&mut AudioEngine>, url: &QString);
        #[qinvokable]
        fn set_crossfade_ms(self: Pin<&mut AudioEngine>, ms: i32);
        #[qinvokable]
        fn set_normalize(self: Pin<&mut AudioEngine>, on: bool);
        #[qinvokable]
        fn set_gapless(self: Pin<&mut AudioEngine>, on: bool);
        /// Switch the output device ("" = default); playback resumes where it was.
        #[qinvokable]
        fn set_output_device(self: Pin<&mut AudioEngine>, name: &QString);
        /// Ids of the available output devices.
        #[qinvokable]
        fn output_devices(self: &AudioEngine) -> QStringList;

        #[qsignal]
        fn loaded(self: Pin<&mut AudioEngine>);
        #[qsignal]
        fn ended(self: Pin<&mut AudioEngine>);
        #[qsignal]
        fn failed(self: Pin<&mut AudioEngine>, message: QString);
    }

    impl cxx_qt::Threading for AudioEngine {}
}

pub struct AudioEngineRust {
    position: i64,
    duration: i64,
    playing: bool,
    loading: bool,
    stalled: bool,
    error: QString,
    engine: Option<Engine>,
    starting: bool,
    settings: Arc<Settings>,
    device: String,
    volume: f32,
    /// What the user asked for; applied when the engine comes up and re-applied after a device switch.
    want_url: Option<String>,
    want_next: Option<String>,
    want_seek: u64,
    want_playing: bool,
    stop_poller: Arc<AtomicBool>,
    /// Signals when a retired engine has let go of its device (a new one waits for that before opening its own).
    closing: Option<Receiver<()>>,
}

impl Default for AudioEngineRust {
    fn default() -> Self {
        let a = &config::get().audio;
        Self {
            position: 0,
            duration: 0,
            playing: false,
            loading: false,
            stalled: false,
            error: QString::default(),
            engine: None,
            starting: false,
            settings: Arc::new(Settings::new(u32::try_from(a.crossfade_ms).unwrap_or(0), a.gapless, a.normalize)),
            device: a.output_device.clone(),
            volume: 1.0,
            want_url: None,
            want_next: None,
            want_seek: 0,
            want_playing: false,
            stop_poller: Arc::new(AtomicBool::new(false)),
            closing: None,
        }
    }
}

impl Drop for AudioEngineRust {
    fn drop(&mut self) {
        self.stop_poller.store(true, Ordering::Relaxed);
        if let Some(mut e) = self.engine.take() {
            // The device is closed here (quick), so nothing touches the sound system while the app shuts down; only
            // the wait for the feeder thread, which may sit in a network read, is left to another thread.
            e.close_output();
            let _ = retire(e);
        }
    }
}

/// Shut an engine down without making the caller (the Qt thread) wait: dropping it joins its feeder thread, which
/// may be in the middle of a network read, and closes the device. It is silenced at once; the rest happens elsewhere.
/// The receiver yields (or disconnects) once the device has been closed, so a new engine can open it.
fn retire(mut engine: Engine) -> Receiver<()> {
    engine.pause();
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new().name("ytm-audio-retire".into()).spawn(move || {
        engine.close_output();
        let _ = tx.send(());
        drop(engine);
    });
    rx
}

/// With `playing` on, a position that has not moved for this long means the engine is waiting for audio.
const STALL_AFTER: Duration = Duration::from_millis(250);

/// The engine plays what the app's own stream proxy serves (loopback only), never arbitrary URLs or file paths.
fn is_local_stream(url: &str) -> bool {
    ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"].iter().any(|p| url.starts_with(p))
}

fn backend_for(device: &str) -> Backend {
    // Dev/test switch: run the whole pipeline without opening a sound device.
    if std::env::var_os("YTM_AUDIO_NULL").is_some() {
        Backend::Null { rate: 48_000 }
    } else {
        Backend::Device { name: device.to_owned() }
    }
}

impl qobject::AudioEngine {
    /// Bring the engine up on a helper thread (opening a device can take a while) unless it is running or starting.
    fn ensure_started(mut self: Pin<&mut Self>) {
        if self.rust().engine.is_some() || self.rust().starting {
            return;
        }
        let (device, settings) = (self.rust().device.clone(), self.rust().settings.clone());
        self.as_mut().rust_mut().get_mut().starting = true;
        let closing = self.as_mut().rust_mut().get_mut().closing.take();
        let th = self.qt_thread();
        std::thread::spawn(move || {
            if let Some(closed) = closing {
                // the engine being replaced still holds its device for a moment (some devices open only once)
                let _ = closed.recv_timeout(Duration::from_secs(5));
            }
            let r = Engine::start(&backend_for(&device), settings);
            let _ = th.queue(move |mut q| {
                q.as_mut().rust_mut().get_mut().starting = false;
                if q.rust().device != device {
                    // The output device was changed while this one was being opened: the newer choice wins.
                    if let Ok((engine, _)) = r {
                        let closing = retire(engine);
                        q.as_mut().rust_mut().get_mut().closing = Some(closing);
                    }
                    if q.rust().want_url.is_some() {
                        q.as_mut().ensure_started();
                    }
                    return;
                }
                match r {
                    Ok((engine, rx)) => q.as_mut().install(engine, rx),
                    Err(msg) => {
                        q.as_mut().set_loading(false);
                        q.as_mut().set_error(QString::from(&msg));
                        q.as_mut().failed(QString::from(&msg));
                    }
                }
            });
        });
    }

    fn install(mut self: Pin<&mut Self>, engine: Engine, rx: Receiver<Event>) {
        let (volume, want_playing) = (self.rust().volume, self.rust().want_playing);
        engine.set_volume(volume);
        if let Some(url) = self.rust().want_url.clone() {
            engine.load(&url, self.rust().want_seek);
            if let Some(n) = self.rust().want_next.clone() {
                engine.set_next(Some(&n));
            }
            if want_playing {
                engine.play();
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.as_mut().rust_mut().get_mut().stop_poller = stop.clone();
        spawn_poller(self.qt_thread(), engine.status(), rx, stop);
        self.as_mut().rust_mut().get_mut().engine = Some(engine);
    }

    fn load(mut self: Pin<&mut Self>, url: &QString) {
        let url = url.to_string();
        if url.is_empty() {
            return self.stop();
        }
        if !is_local_stream(&url) {
            let msg = QString::from("refusing to play a stream that is not served by the local proxy");
            self.as_mut().set_error(msg.clone());
            return self.failed(msg);
        }
        {
            let me = self.as_mut().rust_mut().get_mut();
            me.want_url = Some(url.clone());
            me.want_seek = 0;
            me.want_next = None;
        }
        self.as_mut().set_error(QString::default());
        self.as_mut().set_loading(true);
        if self.rust().engine.is_none() {
            return self.ensure_started();
        }
        if let Some(e) = &self.rust().engine {
            e.load(&url, 0);
        }
    }

    fn play(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().get_mut().want_playing = true;
        if let Some(e) = &self.rust().engine {
            e.play();
        }
        if self.rust().want_url.is_some() {
            self.as_mut().set_playing(true);
        }
    }

    fn pause(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().get_mut().want_playing = false;
        if let Some(e) = &self.rust().engine {
            e.pause();
        }
        self.as_mut().set_playing(false);
    }

    fn stop(mut self: Pin<&mut Self>) {
        {
            let me = self.as_mut().rust_mut().get_mut();
            me.want_playing = false;
            me.want_url = None;
            me.want_next = None;
            me.want_seek = 0;
        }
        if let Some(e) = &self.rust().engine {
            e.stop();
        }
        self.as_mut().set_playing(false);
        self.as_mut().set_loading(false);
        self.as_mut().set_stalled(false);
        self.as_mut().set_position(0);
        self.as_mut().set_duration(0);
    }

    fn seek(mut self: Pin<&mut Self>, ms: i64) {
        let ms = ms.max(0);
        self.as_mut().rust_mut().get_mut().want_seek = ms as u64;
        if let Some(e) = &self.rust().engine {
            e.seek(ms as u64);
        }
        self.as_mut().set_position(ms);
    }

    fn set_volume(mut self: Pin<&mut Self>, v: f64) {
        let v = if v.is_finite() { v.clamp(0.0, 1.0) as f32 } else { 0.0 };
        self.as_mut().rust_mut().get_mut().volume = v;
        if let Some(e) = &self.rust().engine {
            e.set_volume(v);
        }
    }

    fn set_next(mut self: Pin<&mut Self>, url: &QString) {
        let url = url.to_string();
        let next = (!url.is_empty() && is_local_stream(&url)).then_some(url);
        self.as_mut().rust_mut().get_mut().want_next = next.clone();
        if let Some(e) = &self.rust().engine {
            e.set_next(next.as_deref());
        }
    }

    fn set_crossfade_ms(self: Pin<&mut Self>, ms: i32) {
        self.rust().settings.set_crossfade_ms(u32::try_from(ms).unwrap_or(0));
    }

    fn set_normalize(self: Pin<&mut Self>, on: bool) {
        self.rust().settings.set_normalize(on);
    }

    fn set_gapless(self: Pin<&mut Self>, on: bool) {
        self.rust().settings.set_gapless(on);
    }

    fn set_output_device(mut self: Pin<&mut Self>, name: &QString) {
        let name = name.to_string();
        if name == self.rust().device {
            return;
        }
        let pos = self.rust().engine.as_ref().map_or(0, |e| e.position_ms() as u64);
        {
            let me = self.as_mut().rust_mut().get_mut();
            me.device = name;
            if me.engine.is_some() {
                me.want_seek = pos;
            }
            // retiring the engine stops its threads and closes the old device (not on this thread: that can take a while)
            me.stop_poller.store(true, Ordering::Relaxed);
            if let Some(e) = me.engine.take() {
                me.closing = Some(retire(e));
            }
        }
        if self.rust().want_url.is_some() {
            self.ensure_started();
        }
    }

    fn output_devices(&self) -> QStringList {
        output::output_devices().iter().map(|d| QString::from(d.as_str())).collect()
    }
}

/// Mirror the engine's state into the QObject ~30 times a second and turn its events into signals.
fn spawn_poller(th: cxx_qt::CxxQtThread<qobject::AudioEngine>, status: Status, rx: Receiver<Event>, stop: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new().name("ytm-audio-status".into()).spawn(move || {
        let (mut last_pos, mut last_dur, mut last_play, mut last_load) = (i64::MIN, i64::MIN, false, false);
        let mut pending_end = false;
        let mut last_pos_at = std::time::Instant::now();
        let (mut moved_pos, mut moved_at, mut last_stalled) = (i64::MIN, std::time::Instant::now(), false);
        while !stop.load(Ordering::Relaxed) {
            status.deliver_events(); // also while the feeder thread is busy (a slow network read)
            for ev in rx.try_iter() {
                match ev {
                    Event::Loaded(_) => {
                        if th.queue(|mut q| q.as_mut().loaded()).is_err() {
                            return;
                        }
                    }
                    Event::Started(meta) => {
                        let d = meta.duration_ms.map_or(0, |d| d as i64);
                        last_dur = d;
                        let _ = th.queue(move |mut q| q.as_mut().set_duration(d));
                    }
                    Event::Ended(_) => {
                        pending_end = true;
                        let _ = th.queue(|mut q| q.as_mut().ended());
                    }
                    Event::Failed(msg) => {
                        status.set_playing(false);
                        let _ = th.queue(move |mut q| {
                            let m = QString::from(&msg);
                            q.as_mut().set_error(m.clone());
                            q.as_mut().set_loading(false);
                            q.as_mut().failed(m);
                        });
                    }
                }
            }
            // after the last track has been played out the engine goes idle: that is "stopped"
            if pending_end && status.idle() {
                pending_end = false;
                status.set_playing(false);
            }
            let (pos, dur, play, load) = (status.position_ms(), status.duration_ms(), status.playing(), status.loading());
            // Told to play but the position stands still (a stream is being opened, the network fell behind, nothing
            // is left to play): say so, so the UI's clock does not run ahead of the audio.
            if pos != moved_pos || play != last_play {
                (moved_pos, moved_at) = (pos, std::time::Instant::now());
            }
            let stalled = play && moved_at.elapsed() >= STALL_AFTER;
            let pos_due = pos != last_pos && (last_pos_at.elapsed() >= Duration::from_millis(90) || !play || pos.abs_diff(last_pos) > 400);
            if pos_due || (dur != last_dur && dur != 0) || play != last_play || load != last_load || stalled != last_stalled {
                if pos_due {
                    (last_pos, last_pos_at) = (pos, std::time::Instant::now());
                }
                (last_dur, last_play, last_load, last_stalled) = (if dur != 0 { dur } else { last_dur }, play, load, stalled);
                let r = th.queue(move |mut q| {
                    q.as_mut().set_position(pos);
                    if dur != 0 {
                        q.as_mut().set_duration(dur);
                    }
                    q.as_mut().set_playing(play);
                    q.as_mut().set_loading(load);
                    q.as_mut().set_stalled(stalled);
                });
                if r.is_err() {
                    return; // the QObject is gone
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::is_local_stream;

    #[test]
    fn only_loopback_proxy_urls_are_playable() {
        assert!(is_local_stream("http://127.0.0.1:41234/t/abc/dQw4w9WgXcQ"));
        assert!(is_local_stream("http://localhost:1/x"));
        for bad in [
            "",
            "/etc/passwd",
            "file:///etc/passwd",
            "https://127.0.0.1/x",
            "http://example.com/a",
            "http://127.0.0.1.evil.com/a",
            "ftp://127.0.0.1:1/",
        ] {
            assert!(!is_local_stream(bad), "{bad}");
        }
    }
}
