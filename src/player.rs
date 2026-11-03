// SPDX-License-Identifier: GPL-3.0-only
use crate::items::track;
use crate::models::queue::{pick_other, QueueState};
use crate::models::Change;
use crate::queuemodel::{self, QueueEditor};
use crate::session::Session;
use crate::state::st;
use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use rand::Rng;
use rustypipe::param::StreamFilter;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(i32, queue)] // revision: bumps on every queue edit (the rows themselves are in `QueueModel`)
        #[qproperty(i32, index)]
        #[qproperty(bool, has_track)]
        #[qproperty(QString, title)]
        #[qproperty(QString, artist)]
        #[qproperty(QString, thumbnail)]
        #[qproperty(QString, cover)] // same image, requested large for the Now Playing view
        #[qproperty(QString, video_id)]
        #[qproperty(QString, artist_id)] // browse ids of the current track, "" if unknown
        #[qproperty(QString, album_id)]
        #[qproperty(QString, duration)] // "m:ss" from the item
        #[qproperty(QString, source)] // resolved stream URL for the QML MediaPlayer
        #[qproperty(QString, stream_status)] // which stream route is in use / last failure
        #[qproperty(bool, autoplay)] // false while a restored session is loaded but should stay paused
        #[qproperty(i64, resume_pos)] // ms to seek to once the restored track has loaded
        #[qproperty(bool, loading)]
        #[qproperty(QString, error)]
        #[qproperty(bool, shuffle)]
        #[qproperty(i32, repeat)] // 0 off, 1 all, 2 one
        #[qproperty(f64, volume)]
        type Player = super::PlayerRust;

        #[qinvokable]
        fn play_song(self: Pin<&mut Player>, song_json: &QString);
        #[qinvokable]
        fn play_list(self: Pin<&mut Player>, songs_json: &QString, start: i32);
        #[qinvokable]
        fn play_index(self: Pin<&mut Player>, i: i32);
        #[qinvokable]
        fn enqueue(self: Pin<&mut Player>, song_json: &QString);
        #[qinvokable]
        fn play_next(self: Pin<&mut Player>, song_json: &QString);
        #[qinvokable]
        fn remove_at(self: Pin<&mut Player>, i: i32);
        #[qinvokable]
        fn next(self: Pin<&mut Player>);
        #[qinvokable]
        fn previous(self: Pin<&mut Player>);
        /// Called by QML when the media backend fails: re-resolves the stream once, then reports.
        #[qinvokable]
        fn media_failed(self: Pin<&mut Player>, message: &QString);
        #[qinvokable]
        fn clear_queue(self: Pin<&mut Player>);
        /// Move a queue entry (keeps the current track playing).
        #[qinvokable]
        fn move_item(self: Pin<&mut Player>, from: i32, to: i32);
        /// Health check: resolve a known track and make sure the stream is not capped.
        #[qinvokable]
        fn test_stream(self: Pin<&mut Player>);
        /// Start the MPRIS D-Bus service; its commands arrive via `command`.
        #[qinvokable]
        fn start_mpris(self: Pin<&mut Player>);
        /// QML pushes playback state (ms) so D-Bus clients see it.
        #[qinvokable]
        fn sync_mpris(self: Pin<&mut Player>, playing: bool, position: i64, duration: i64);
        /// A media-key / D-Bus request: play, pause, playpause, stop, next, previous, seek, setposition, shuffle, repeat, volume, raise, quit.
        #[qsignal]
        fn command(self: Pin<&mut Player>, cmd: QString, arg: i64);
        /// Persist queue, current track and playback position to disk.
        #[qinvokable]
        fn save_session(self: Pin<&mut Player>, position: i64);
        /// Reload the last saved session, paused.
        #[qinvokable]
        fn restore_session(self: Pin<&mut Player>);
    }

    impl cxx_qt::Threading for Player {}
    impl cxx_qt::Initialize for Player {}
}

pub struct PlayerRust {
    queue: i32,
    index: i32,
    has_track: bool,
    title: QString,
    artist: QString,
    thumbnail: QString,
    cover: QString,
    video_id: QString,
    artist_id: QString,
    album_id: QString,
    duration: QString,
    source: QString,
    stream_status: QString,
    autoplay: bool,
    resume_pos: i64,
    loading: bool,
    error: QString,
    shuffle: bool,
    repeat: i32,
    volume: f64,
    state: QueueState<Value>,
    generation: u64,
    radio_busy: bool,
    retried: u64, // generation that already had one automatic retry
}

impl Default for PlayerRust {
    fn default() -> Self {
        Self {
            queue: 0,
            index: -1,
            has_track: false,
            title: QString::default(),
            artist: QString::default(),
            thumbnail: QString::default(),
            cover: QString::default(),
            video_id: QString::default(),
            artist_id: QString::default(),
            album_id: QString::default(),
            duration: QString::default(),
            source: QString::default(),
            stream_status: QString::default(),
            autoplay: true,
            resume_pos: 0,
            loading: false,
            error: QString::default(),
            shuffle: false,
            repeat: 0,
            volume: 0.8,
            state: QueueState::default(),
            generation: 0,
            radio_busy: false,
            retried: u64::MAX,
        }
    }
}

type Cache = HashMap<String, (String, Instant)>;
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
static HEALTH: Mutex<String> = Mutex::new(String::new());

fn set_health(s: impl Into<String>) {
    *HEALTH.lock().unwrap() = s.into();
}

fn health() -> QString {
    QString::from(&*HEALTH.lock().unwrap())
}

fn forget(id: &str) {
    CACHE.lock().unwrap().get_or_insert_default().remove(id);
}

/// Developer harness only: never look streams up (no network, no audio).
#[cfg(feature = "harness")]
static NO_STREAMS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "harness")]
pub fn disable_streams() {
    NO_STREAMS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Resolve a playable audio stream URL for a video id (cached for 30 min).
async fn resolve(id: &str) -> Result<String, String> {
    #[cfg(feature = "harness")]
    if NO_STREAMS.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("Streams are switched off in the harness".to_owned());
    }
    if let Some((url, at)) = CACHE.lock().unwrap().get_or_insert_default().get(id) {
        if at.elapsed() < Duration::from_secs(crate::config::get().playback.stream_cache_minutes * 60) {
            return Ok(url.clone());
        }
    }
    let (real, ua) = match crate::vision::audio_url(id).await {
        Ok((u, _mime)) => {
            set_health("visionOS client: OK");
            (u, crate::vision::user_agent().to_owned())
        }
        Err(e) => {
            eprintln!("visionOS route failed ({e}); falling back to RustyPipe");
            let p = match st().rp.query().player(id).await {
                Ok(p) => p,
                Err(e2) => {
                    set_health(format!("Both stream routes failed. visionOS: {e}"));
                    return Err(e2.to_string());
                }
            };
            set_health(format!("Fallback route (RustyPipe iOS) – streams may cut off after ~1 min. visionOS: {e}"));
            let real = p.select_audio_stream(&StreamFilter::default()).map(|s| s.url.clone()).ok_or("No audio stream available")?;
            (real, st().rp.query().user_agent(p.client_type).into_owned())
        }
    };
    let url = crate::proxy::local_url(id, &real, &ua);
    CACHE.lock().unwrap().get_or_insert_default().insert(id.to_owned(), (url.clone(), Instant::now()));
    Ok(url)
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_owned()
}

impl qobject::Player {
    fn parse(json: &QString) -> Option<Value> {
        serde_json::from_str(&json.to_string()).ok()
    }

    /// Mutable access to the non-property state. Only ever used on the Qt thread.
    fn st_mut(self: Pin<&mut Self>) -> &mut PlayerRust {
        unsafe { self.rust_mut().get_unchecked_mut() }
    }

    /// Tell the `QueueModel` (and Main.qml's session saver, via the `queue` revision) about an edit.
    fn publish(mut self: Pin<&mut Self>, change: Change<Value>) {
        let items = self.rust().state.items().to_vec();
        queuemodel::publish(change, move || items);
        let rev = self.queue.wrapping_add(1);
        self.as_mut().set_queue(rev);
    }

    /// Keep the `index` property equal to the queue's playing row.
    fn sync_index(mut self: Pin<&mut Self>) {
        let idx = self.rust().state.current().map_or(-1, |c| c as i32);
        if *self.index() != idx {
            self.as_mut().set_index(idx);
        }
    }

    fn stop_playing(mut self: Pin<&mut Self>) {
        self.as_mut().st_mut().generation += 1;
        self.as_mut().st_mut().state.set_current(None);
        self.as_mut().set_has_track(false);
        self.as_mut().set_index(-1);
        self.as_mut().set_source(QString::default());
        self.as_mut().set_title(QString::default());
    }

    fn load(mut self: Pin<&mut Self>, i: usize) {
        let Some(item) = self.rust().state.get(i).cloned() else { return };
        let generation = {
            let r = self.as_mut().st_mut();
            r.generation += 1;
            r.state.set_current(Some(i));
            r.generation
        };
        let vid = s(&item, "videoId");
        self.as_mut().set_index(i as i32);
        self.as_mut().set_has_track(true);
        self.as_mut().set_title(QString::from(&s(&item, "title")));
        self.as_mut().set_artist(QString::from(&s(&item, "artist")));
        self.as_mut().set_thumbnail(QString::from(&s(&item, "thumbnail")));
        self.as_mut().set_cover(QString::from(&crate::items::upscale(&s(&item, "thumbnail"), crate::items::COVER_BIG_PX)));
        self.as_mut().set_video_id(QString::from(&vid));
        self.as_mut().set_artist_id(QString::from(&s(&item, "artistId")));
        self.as_mut().set_album_id(QString::from(&s(&item, "albumId")));
        self.as_mut().set_duration(QString::from(&s(&item, "duration")));
        self.as_mut().set_source(QString::default());
        self.as_mut().set_error(QString::default());
        self.as_mut().set_autoplay(true);
        self.as_mut().set_loading(true);

        let next_vid = self.rust().state.get(i + 1).map(|n| s(n, "videoId")).filter(|_| crate::config::get().playback.prefetch_next);
        let th = self.qt_thread();
        st().rt.spawn(async move {
            let r = resolve(&vid).await;
            let _ = th.queue(move |mut p| {
                if p.rust().generation != generation {
                    return;
                }
                match r {
                    Ok(url) => p.as_mut().set_source(QString::from(&url)),
                    Err(e) => p.as_mut().set_error(QString::from(&e)),
                }
                p.as_mut().set_loading(false);
                p.as_mut().set_stream_status(health());
            });
            if let Some(n) = next_vid {
                let _ = resolve(&n).await; // warm the cache so skips are instant
            }
        });
    }

    fn fetch_radio(mut self: Pin<&mut Self>, advance: bool) {
        if self.rust().radio_busy {
            return;
        }
        let Some(seed) = self.rust().state.items().last().map(|l| s(l, "videoId")) else { return };
        let generation = self.rust().generation;
        self.as_mut().st_mut().radio_busy = true;
        let th = self.qt_thread();
        st().rt.spawn(async move {
            let items: Vec<Value> = match st().rp.query().music_radio_track(&seed).await {
                Ok(p) => p.items.iter().map(|t| track(t, "", "")).collect(),
                Err(_) => Vec::new(),
            };
            let _ = th.queue(move |mut p| {
                p.as_mut().st_mut().radio_busy = false;
                let have: Vec<String> = p.rust().state.items().iter().map(|i| s(i, "videoId")).collect();
                let new: Vec<Value> = items.into_iter().filter(|i| !have.contains(&s(i, "videoId"))).collect();
                if !new.is_empty() {
                    let change = p.as_mut().st_mut().state.extend(new);
                    p.as_mut().publish(change);
                }
                // Only advance if the user hasn't moved on (or emptied the queue) meanwhile.
                if advance && p.rust().generation == generation {
                    let idx = (*p.index()).max(0) as usize;
                    if idx + 1 < p.rust().state.len() {
                        p.as_mut().load(idx + 1);
                    }
                }
            });
        });
    }

    // ---- queue operations on typed values (the JSON invokables and the page controller both end up here) ----

    pub(crate) fn play_one(mut self: Pin<&mut Self>, song: Value) {
        let change = self.as_mut().st_mut().state.replace(vec![song]);
        self.as_mut().publish(change);
        self.as_mut().load(0);
        self.fetch_radio(false);
    }

    pub(crate) fn play_many(mut self: Pin<&mut Self>, songs: Vec<Value>, start: usize) {
        if songs.is_empty() {
            return;
        }
        let start = start.min(songs.len() - 1);
        let change = self.as_mut().st_mut().state.replace(songs);
        self.as_mut().publish(change);
        self.load(start);
    }

    pub(crate) fn enqueue_one(self: Pin<&mut Self>, song: Value) {
        self.enqueue_many(vec![song]);
    }

    pub(crate) fn enqueue_many(mut self: Pin<&mut Self>, songs: Vec<Value>) {
        if songs.is_empty() {
            return;
        }
        let change = self.as_mut().st_mut().state.extend(songs);
        self.as_mut().publish(change);
        if !*self.has_track() {
            let last = self.rust().state.len() - 1;
            self.load(last);
        }
    }

    pub(crate) fn play_next_one(mut self: Pin<&mut Self>, song: Value) {
        let change = self.as_mut().st_mut().state.insert_next(song);
        self.as_mut().publish(change);
        self.as_mut().sync_index();
        if !*self.has_track() {
            self.load(0);
        }
    }

    /// Move an existing queue row to just after the playing one (the "Play next" of a queue row).
    pub(crate) fn move_to_next(mut self: Pin<&mut Self>, from: usize) {
        let cur = self.rust().state.current();
        let to = match cur {
            Some(c) if from > c => c + 1,
            Some(c) => c, // from <= c: after the removal the playing row sits at c-1, so "after it" is index c
            None => 0,
        };
        if from == to {
            return;
        }
        if let Some(change) = self.as_mut().st_mut().state.move_item(from, to) {
            self.as_mut().publish(change);
            self.sync_index();
        }
    }

    pub(crate) fn set_shuffle_on(mut self: Pin<&mut Self>) {
        self.as_mut().set_shuffle(true);
    }

    fn play_song(self: Pin<&mut Self>, song_json: &QString) {
        let Some(song) = Self::parse(song_json) else { return };
        self.play_one(song);
    }

    fn play_list(self: Pin<&mut Self>, songs_json: &QString, start: i32) {
        let Some(Value::Array(songs)) = Self::parse(songs_json) else { return };
        self.play_many(songs, start.max(0) as usize);
    }

    fn play_index(self: Pin<&mut Self>, i: i32) {
        if i >= 0 && (i as usize) < self.rust().state.len() {
            self.load(i as usize);
        }
    }

    fn enqueue(self: Pin<&mut Self>, song_json: &QString) {
        let Some(song) = Self::parse(song_json) else { return };
        self.enqueue_one(song);
    }

    fn play_next(self: Pin<&mut Self>, song_json: &QString) {
        let Some(song) = Self::parse(song_json) else { return };
        self.play_next_one(song);
    }

    fn remove_at(mut self: Pin<&mut Self>, i: i32) {
        let Ok(i) = usize::try_from(i) else { return };
        let Some(removal) = self.as_mut().st_mut().state.remove(i) else { return };
        self.as_mut().publish(removal.change);
        if !removal.was_current {
            return self.sync_index();
        }
        match removal.replacement {
            Some(k) => self.load(k),
            None => self.stop_playing(),
        }
    }

    fn next(self: Pin<&mut Self>) {
        let len = self.rust().state.len();
        if len == 0 {
            return;
        }
        let cur = (*self.index()).max(0) as usize;
        if *self.shuffle() && len > 1 {
            let pick = pick_other(len, cur, rand::rng().random_range(0..len - 1)).unwrap_or(0);
            let mut this = self;
            this.as_mut().st_mut().state.push_history(cur);
            this.load(pick);
        } else if cur + 1 < len {
            self.load(cur + 1);
        } else if *self.repeat() == 1 {
            self.load(0);
        } else {
            if crate::config::get().playback.autoplay_radio {
                self.fetch_radio(true);
            }
        }
    }

    fn media_failed(mut self: Pin<&mut Self>, message: &QString) {
        let cur = *self.index();
        let generation = self.rust().generation;
        let Some(item) = (cur >= 0).then(|| self.rust().state.get(cur as usize).cloned()).flatten() else { return };
        if self.rust().retried != generation {
            // Stream URLs can expire or get cut off: fetch a fresh one once.
            let autoplay = *self.autoplay();
            self.as_mut().st_mut().retried = generation;
            forget(&s(&item, "videoId"));
            self.as_mut().load(cur as usize);
            self.as_mut().set_autoplay(autoplay);
            // load() bumped the generation; remember that this one was already a retry.
            let g = self.rust().generation;
            self.as_mut().st_mut().retried = g;
        } else {
            self.as_mut().set_loading(false);
            self.set_error(QString::from(&format!("Playback failed: {message}")));
        }
    }

    fn clear_queue(mut self: Pin<&mut Self>) {
        let r = self.as_mut().st_mut();
        let change = r.state.clear();
        r.generation += 1;
        self.as_mut().publish(change);
        self.as_mut().set_has_track(false);
        self.as_mut().set_index(-1);
        self.as_mut().set_source(QString::default());
        self.as_mut().set_title(QString::default());
        self.as_mut().set_loading(false);
        self.set_error(QString::default());
    }

    fn move_item(mut self: Pin<&mut Self>, from: i32, to: i32) {
        let (Ok(from), Ok(to)) = (usize::try_from(from), usize::try_from(to)) else { return };
        if let Some(change) = self.as_mut().st_mut().state.move_item(from, to) {
            self.as_mut().publish(change);
            self.sync_index();
        }
    }

    fn test_stream(self: Pin<&mut Self>) {
        let th = self.qt_thread();
        st().rt.spawn(async move {
            let pb = &crate::config::get().playback;
            let id = pb.check_video_id.as_str();
            let msg = match crate::vision::audio_url(id).await {
                Err(e) => format!("visionOS route failed: {e}"),
                Ok((url, _)) => {
                    // The iOS-style cutoff happens ~768 KB in, so ask for bytes well past that.
                    let r = crate::state::http()
                        .get(&url)
                        .header("User-Agent", crate::vision::user_agent())
                        .header("Range", format!("bytes={0}-{1}", pb.check_offset_bytes, pb.check_offset_bytes + 127))
                        .send()
                        .await;
                    match r {
                        Ok(r) if r.status().as_u16() == 206 => "visionOS client: OK (stream not capped)".to_owned(),
                        Ok(r) => format!("visionOS stream is capped/blocked (HTTP {})", r.status().as_u16()),
                        Err(e) => format!("Stream check failed: {e}"),
                    }
                }
            };
            set_health(msg);
            let _ = th.queue(|mut p| p.as_mut().set_stream_status(health()));
        });
    }

    fn start_mpris(self: Pin<&mut Self>) {
        if !crate::config::get().integration.mpris {
            return;
        }
        let th = self.qt_thread();
        crate::mpris::start(move |cmd, arg| {
            let cmd = QString::from(cmd);
            let _ = th.queue(move |mut p| p.as_mut().command(cmd, arg));
        });
    }

    fn sync_mpris(self: Pin<&mut Self>, playing: bool, position: i64, duration: i64) {
        let snap = crate::mpris::Snap {
            has_track: *self.has_track(),
            playing,
            title: self.title().to_string(),
            artist: self.artist().to_string(),
            art: self.thumbnail().to_string(),
            video_id: self.video_id().to_string(),
            length_us: duration.max(0) * 1000,
            volume: *self.volume(),
            shuffle: *self.shuffle(),
            repeat: *self.repeat(),
        };
        crate::mpris::update(snap, position.max(0) * 1000);
    }

    fn save_session(self: Pin<&mut Self>, position: i64) {
        let idx = *self.index();
        if idx < 0 || self.rust().state.is_empty() {
            Session::clear();
            return;
        }
        Session { items: self.rust().state.items().to_vec(), index: idx as usize, position }.save();
    }

    fn restore_session(mut self: Pin<&mut Self>) {
        let Some(session) = Session::load() else { return };
        let change = self.as_mut().st_mut().state.replace(session.items);
        self.as_mut().publish(change);
        self.as_mut().set_resume_pos(session.position);
        self.as_mut().load(session.index);
        self.set_autoplay(false);
    }

    /// QML restarts the current track itself when it's past 3s.
    fn previous(mut self: Pin<&mut Self>) {
        if let Some(i) = self.as_mut().st_mut().state.pop_history() {
            return self.load(i);
        }
        let cur = *self.index();
        if cur > 0 {
            self.load((cur - 1) as usize);
        }
    }
}

// ---- reaching the Player from other Rust QObjects ----------------------------------------------------------------

static HANDLE: Mutex<Option<cxx_qt::CxxQtThread<qobject::Player>>> = Mutex::new(None);

/// Run `f` on the Qt thread with the `Player` (a no-op until QML has created it). This is how the page controller
/// starts playback and how the queue model's drag-and-drop edits reach the queue.
pub fn with_player(f: impl FnOnce(Pin<&mut qobject::Player>) + Send + 'static) {
    if let Some(th) = HANDLE.lock().unwrap().as_ref() {
        let _ = th.queue(f);
    }
}

impl cxx_qt::Initialize for qobject::Player {
    fn initialize(self: Pin<&mut Self>) {
        *HANDLE.lock().unwrap() = Some(self.qt_thread());
        queuemodel::install_editor(|edit| with_player(move |p| edit(&mut PlayerEditor(p))));
    }
}

/// What the `QueueModel` may ask of the queue's owner.
struct PlayerEditor<'a>(Pin<&'a mut qobject::Player>);

impl QueueEditor for PlayerEditor<'_> {
    fn move_row(&mut self, from: usize, to: usize) {
        self.0.as_mut().move_item(from as i32, to as i32);
    }
    fn remove_row(&mut self, at: usize) {
        self.0.as_mut().remove_at(at as i32);
    }
    fn play_row(&mut self, at: usize) {
        self.0.as_mut().play_index(at as i32);
    }
    fn play_row_next(&mut self, at: usize) {
        self.0.as_mut().move_to_next(at);
    }
}
