// SPDX-License-Identifier: GPL-3.0-only
//! The `Downloads` QML singleton: a thin shell over `downloads::manager`.
//! (It lives next to the other bridges in `src/` because cxx-qt-build wants all bridge files of one QML module
//! in a single directory.)
use crate::downloads::manager::{self, Event, Item};
use crate::downloads::{local_playback_url, rules};
use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;

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
        /// Bytes used by downloaded songs.
        #[qproperty(i64, used_bytes)]
        /// The storage budget (`[downloads] cap_gb`).
        #[qproperty(i64, cap_bytes)]
        /// Songs queued or downloading.
        #[qproperty(i32, active_count)]
        /// Downloads are held back (by the user, being offline, a metered connection or a low battery).
        #[qproperty(bool, paused)]
        /// Why: "", "user", "offline", "metered" or "low_battery".
        #[qproperty(QString, pause_reason)]
        /// No network connection (from `Connectivity`).
        #[qproperty(bool, offline)]
        /// Non-empty when metered-connection detection is unavailable on this system (show it in Settings).
        #[qproperty(QString, metered_note)]
        type Downloads = super::DownloadsRust;

        /// Download one song (`{videoId, title, artist|subtitle, cover|thumbnail, duration}`); invalid ids are ignored.
        #[qinvokable]
        fn download(self: Pin<&mut Downloads>, item_json: &QString);
        /// Download many songs (a JSON array of items), e.g. an album or playlist.
        #[qinvokable]
        fn download_all(self: Pin<&mut Downloads>, items_json: &QString);
        /// Delete a song's files and index row, or cancel its download.
        #[qinvokable]
        fn remove(self: Pin<&mut Downloads>, video_id: &QString);
        /// Retry a failed download.
        #[qinvokable]
        fn retry(self: Pin<&mut Downloads>, video_id: &QString);
        #[qinvokable]
        fn is_downloaded(self: &Downloads, video_id: &QString) -> bool;
        /// "none", "queued", "downloading", "downloaded" or "failed".
        #[qinvokable]
        fn state(self: &Downloads, video_id: &QString) -> QString;
        /// Download progress 0-100 (100 when downloaded).
        #[qinvokable]
        fn percent(self: &Downloads, video_id: &QString) -> i32;
        /// Pin one song: exempt from automatic removal.
        #[qinvokable]
        fn pin(self: Pin<&mut Downloads>, id: &QString, on: bool);
        /// "Keep offline" for an album/playlist: download its songs and exempt them from automatic removal.
        #[qinvokable]
        fn keep_offline(self: Pin<&mut Downloads>, collection_id: &QString, items_json: &QString, on: bool);
        #[qinvokable]
        fn is_kept_offline(self: &Downloads, collection_id: &QString) -> bool;
        /// Pause or resume everything ("Pause all"). (Not `setPaused`: that name belongs to the `paused` property.)
        #[qinvokable]
        fn pause_all(self: Pin<&mut Downloads>, on: bool);
        /// The Downloads page data as a JSON array (running jobs first, then the songs on disk).
        #[qinvokable]
        fn list(self: &Downloads) -> QString;
        /// Tell the eviction order that a song was played.
        #[qinvokable]
        fn played(self: Pin<&mut Downloads>, video_id: &QString);
        /// Feed the smart rules: `kind` is "liked", "recent" or "queue" (the next songs in play order); `items_json` an array of items.
        #[qinvokable]
        fn set_smart_inputs(self: Pin<&mut Downloads>, kind: &QString, items_json: &QString);
        /// `http://127.0.0.1:…/dl/<id>` for a downloaded song, "" otherwise.
        #[qinvokable]
        fn local_url(self: Pin<&mut Downloads>, video_id: &QString) -> QString;

        /// Something about this song changed (state, progress milestone, removal, pin).
        #[qsignal]
        fn changed(self: Pin<&mut Downloads>, video_id: QString);
        /// Live progress of a running download.
        #[qsignal]
        fn progress(self: Pin<&mut Downloads>, video_id: QString, percent: i32, speed_bps: i64);
    }

    impl cxx_qt::Threading for Downloads {}
    impl cxx_qt::Initialize for Downloads {}
}

pub struct DownloadsRust {
    used_bytes: i64,
    cap_bytes: i64,
    active_count: i32,
    paused: bool,
    pause_reason: QString,
    offline: bool,
    metered_note: QString,
}

impl Default for DownloadsRust {
    fn default() -> Self {
        Self {
            used_bytes: 0,
            cap_bytes: i64::try_from(rules::cap_bytes(crate::config::current().downloads.cap_gb)).unwrap_or(i64::MAX),
            active_count: 0,
            paused: false,
            pause_reason: QString::default(),
            offline: !crate::connectivity::is_online(),
            metered_note: QString::default(),
        }
    }
}

impl cxx_qt::Initialize for qobject::Downloads {
    fn initialize(self: Pin<&mut Self>) {
        let th = self.qt_thread();
        manager::set_sink(move |ev| {
            let _ = th.queue(move |mut d| {
                match ev {
                    Event::Changed(id) => d.as_mut().changed(QString::from(&id)),
                    Event::Progress { id, percent, bps } => d.as_mut().progress(QString::from(&id), percent, bps),
                    Event::Stats => {}
                }
                d.refresh_stats();
            });
        });
        let th = self.qt_thread();
        crate::connectivity::subscribe(move |online| {
            let _ = th.queue(move |mut d| d.as_mut().set_offline(!online));
        });
        // opening the index and reconciling with the disk must not stall the UI thread
        let th = self.qt_thread();
        std::thread::spawn(move || {
            manager::init();
            let _ = th.queue(|d| d.refresh_stats());
        });
    }
}

fn parse_items(json: &QString) -> Vec<Item> {
    match serde_json::from_str::<serde_json::Value>(&json.to_string()) {
        Ok(serde_json::Value::Array(a)) => a.iter().filter_map(Item::from_json).collect(),
        Ok(v) => Item::from_json(&v).into_iter().collect(),
        Err(_) => Vec::new(),
    }
}

impl qobject::Downloads {
    fn refresh_stats(mut self: Pin<&mut Self>) {
        self.as_mut().set_used_bytes(manager::used_bytes());
        self.as_mut().set_active_count(manager::active_count());
        let reason = manager::pause_reason();
        self.as_mut().set_paused(reason.is_some());
        self.as_mut().set_pause_reason(QString::from(reason.map_or("", |r| r.as_str())));
        self.as_mut().set_metered_note(QString::from(manager::metered_note().unwrap_or("")));
    }

    fn download(self: Pin<&mut Self>, item_json: &QString) {
        for it in parse_items(item_json).into_iter().take(1) {
            manager::enqueue(it);
        }
    }

    fn download_all(self: Pin<&mut Self>, items_json: &QString) {
        manager::enqueue_all(parse_items(items_json)); // one index write for the whole album / playlist
    }

    fn remove(self: Pin<&mut Self>, video_id: &QString) {
        manager::remove(&video_id.to_string());
    }

    fn retry(self: Pin<&mut Self>, video_id: &QString) {
        manager::retry(&video_id.to_string());
    }

    fn is_downloaded(&self, video_id: &QString) -> bool {
        manager::init();
        manager::is_downloaded(&video_id.to_string())
    }

    fn state(&self, video_id: &QString) -> QString {
        manager::init();
        QString::from(manager::state_of(&video_id.to_string()).0)
    }

    fn percent(&self, video_id: &QString) -> i32 {
        manager::init();
        manager::state_of(&video_id.to_string()).1
    }

    fn pin(self: Pin<&mut Self>, id: &QString, on: bool) {
        manager::pin(&id.to_string(), on);
    }

    fn keep_offline(self: Pin<&mut Self>, collection_id: &QString, items_json: &QString, on: bool) {
        manager::keep_offline(&collection_id.to_string(), &parse_items(items_json), on);
    }

    fn is_kept_offline(&self, collection_id: &QString) -> bool {
        manager::init();
        manager::is_kept_offline(&collection_id.to_string())
    }

    fn pause_all(self: Pin<&mut Self>, on: bool) {
        manager::set_paused(on);
    }

    fn list(&self) -> QString {
        manager::init();
        QString::from(&manager::list_json())
    }

    fn played(self: Pin<&mut Self>, video_id: &QString) {
        manager::played(&video_id.to_string());
    }

    fn set_smart_inputs(self: Pin<&mut Self>, kind: &QString, items_json: &QString) {
        manager::set_inputs(&kind.to_string(), parse_items(items_json));
    }

    fn local_url(self: Pin<&mut Self>, video_id: &QString) -> QString {
        QString::from(&local_playback_url(&video_id.to_string()).unwrap_or_default())
    }
}
