// SPDX-License-Identifier: GPL-3.0-only
use crate::auth::{self, Input, AUTH_MSG};
use crate::items::*;
use crate::state::st;
use crate::ytapi;
use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use rustypipe::client::RustyPipeQuery;
use rustypipe::error::Error as RpError;
use rustypipe::model::paginator::Paginator;
use rustypipe::model::{AlbumItem, ArtistItem, MusicPlaylistItem, TrackItem};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::Mutex;

pub(crate) type Res = Result<Value, String>;

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
        #[qproperty(bool, logged_in)]
        #[qproperty(i32, liked_rev)] // bumps whenever the set of liked songs changes
        type Backend = super::BackendRust;

        /// Every request returns an id; the answer arrives via `done(id, error, json)`.
        #[qinvokable]
        fn search(self: Pin<&mut Backend>, query: &QString, filter: &QString) -> i32;
        #[qinvokable]
        fn suggest(self: Pin<&mut Backend>, query: &QString) -> i32;
        #[qinvokable]
        fn browse(self: Pin<&mut Backend>, target: &QString) -> i32;
        /// Home, loaded progressively: each row arrives as `partial(id, slot, json)` the moment it is ready
        /// (so a slow row never holds up the rest); `done` fires once all rows have finished.
        #[qinvokable]
        fn browse_home(self: Pin<&mut Backend>) -> i32;
        /// Next page for a list whose first answer carried a `pager` id.
        #[qinvokable]
        fn more(self: Pin<&mut Backend>, pager: i32) -> i32;
        /// A page shows this list: its pager is kept for as long as the page holds it (until `dropPager`).
        #[qinvokable]
        fn hold_pager(self: &Backend, pager: i32);
        /// Forget a list's pager (the page showing it went away), freeing the items it holds. A feed that is still
        /// walking the list finishes first.
        #[qinvokable]
        fn drop_pager(self: &Backend, pager: i32);
        /// Keep fetching a list's pages and appending their songs to the queue ("Play" on a long list), whether or not
        /// the page is still open. Every page also arrives through `done` under the returned id (`feeding` says
        /// whether more will follow). `epoch` is `Player.queueEpoch()` taken right after the queue was built from the
        /// pages shown so far: the feed stops when that queue is replaced. A page already on its way is part of the feed.
        #[qinvokable]
        fn feed_queue(self: Pin<&mut Backend>, pager: i32, epoch: i64) -> i32;
        /// The same feed, for "Download" on a long list: the songs of the pages not fetched yet are queued for download.
        /// Joins a feed already running on that pager (and returns its id) instead of starting a second one.
        #[qinvokable]
        fn feed_downloads(self: Pin<&mut Backend>, pager: i32) -> i32;
        /// Hand memory freed by closed pages back to the system.
        #[qinvokable]
        fn trim_memory(self: &Backend);
        /// Lyrics for a track: `{synced: [[ms, line], ...], plain, source}`; "Lyrics unavailable" when nobody has them.
        #[qinvokable]
        fn lyrics(self: Pin<&mut Backend>, video_id: &QString, title: &QString, artist: &QString, seconds: i32) -> i32;
        /// JSON list of importable browsers: [{id, name, isDefault}], default browser first.
        #[qinvokable]
        fn browsers(self: &Backend) -> QString;
        #[qinvokable]
        fn login_import(self: Pin<&mut Backend>, browser: &QString) -> i32;
        #[qinvokable]
        fn login_cookie(self: Pin<&mut Backend>, text: &QString) -> i32;
        #[qinvokable]
        fn logout(self: Pin<&mut Backend>);
        #[qinvokable]
        fn refresh_login(self: Pin<&mut Backend>);
        /// An error the user should hear about. True when it went out as a desktop notification (the window is not
        /// being looked at); false when the window should show it itself.
        #[qinvokable]
        fn notify_error(self: &Backend, summary: &QString, body: &QString, window_active: bool) -> bool;
        #[qinvokable]
        fn is_liked(self: &Backend, video_id: &QString) -> bool;
        /// Ask YouTube whether this song (the one that started playing) is liked and correct its heart: the liked
        /// list is only read so far down (`liked_limit`), and a like made in another app is not in it at all.
        #[qinvokable]
        fn confirm_liked(self: Pin<&mut Backend>, video_id: &QString);
        #[qinvokable]
        fn set_liked(self: Pin<&mut Backend>, video_id: &QString, liked: bool) -> i32;
        #[qinvokable]
        fn my_playlists(self: Pin<&mut Backend>) -> i32;
        #[qinvokable]
        fn add_to_playlist(self: Pin<&mut Backend>, playlist_id: &QString, video_id: &QString) -> i32;

        #[qsignal]
        fn done(self: Pin<&mut Backend>, id: i32, error: QString, json: QString);
        /// One finished row of a progressively loaded page; `slot` fixes its position on the page.
        #[qsignal]
        fn partial(self: Pin<&mut Backend>, id: i32, slot: i32, json: QString);
        /// At start a sign-in was stored, but YouTube no longer accepts it: the user is now signed out and should
        /// be told, since Home and the Library quietly show less.
        #[qsignal]
        fn session_expired(self: Pin<&mut Backend>);
        /// The stored sign-in had gone stale and was replaced by the browser's current one. `loggedIn` did not change,
        /// but pages fetched (or refused) with the old cookies should be loaded again.
        #[qsignal]
        fn session_renewed(self: Pin<&mut Backend>);
        /// A like/unlike YouTube refused: the heart has already been put back, show `message` to the user.
        #[qsignal]
        fn like_failed(self: Pin<&mut Backend>, video_id: QString, message: QString);
    }

    impl cxx_qt::Threading for Backend {}
}

#[derive(Default)]
pub struct BackendRust {
    logged_in: bool,
    liked_rev: i32,
    next_id: i32,
}

static LOGGED: AtomicBool = AtomicBool::new(false);
/// Counts sign-in / sign-out changes; cached pages from an earlier epoch belong to another account.
static LOGIN_EPOCH: AtomicU32 = AtomicU32::new(0);

fn set_logged(v: bool) {
    if LOGGED.swap(v, Ordering::Relaxed) != v {
        LOGIN_EPOCH.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn is_logged_in() -> bool {
    LOGGED.load(Ordering::Relaxed)
}

pub(crate) fn login_epoch() -> u32 {
    LOGIN_EPOCH.load(Ordering::Relaxed)
}
static LIKED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static LIKED_DIRTY: AtomicBool = AtomicBool::new(false);

fn liked_set<R>(f: impl FnOnce(&mut HashSet<String>) -> R) -> R {
    f(LIKED.lock().unwrap().get_or_insert_default())
}

/// What the user chose with the heart in this run, per song: newer than any list read from YouTube, which may have
/// been on its way when they clicked.
static LIKE_CHOICES: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);
/// Likes reach YouTube one at a time, in the order they were clicked: two clicks on one heart must not overtake
/// each other.
static LIKE_ORDER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn like_choices<R>(f: impl FnOnce(&mut HashMap<String, bool>) -> R) -> R {
    f(LIKE_CHOICES.lock().unwrap().get_or_insert_default())
}

/// Put the user's own clicks back on top of a set that was just filled from YouTube.
fn apply_like_choices(set: &mut HashSet<String>, choices: &HashMap<String, bool>) {
    for (id, liked) in choices {
        if *liked {
            set.insert(id.clone());
        } else {
            set.remove(id);
        }
    }
}

/// A like YouTube refused: is the heart still showing that click, so that it has to be put back? Not when the user
/// has clicked again since (`choice` is then the newer click, which has its own request).
fn like_needs_undo(choice: Option<bool>, refused: bool) -> bool {
    choice == Some(refused)
}

/// The list id of the real Liked-music page while one is open: every page read from it holds liked songs, however
/// far down the list (`liked_limit` only bounds what is read in the background).
static LIKED_PAGER: AtomicI32 = AtomicI32::new(0);

/// Songs YouTube just listed as liked: remember them for the hearts, with the user's own clicks kept on top.
fn remember_liked(ids: impl IntoIterator<Item = String>) {
    liked_set(|s| {
        s.extend(ids);
        like_choices(|c| apply_like_choices(s, c));
    });
    LIKED_DIRTY.store(true, Ordering::Relaxed);
}

/// YouTube said whether one song is liked. Take its word unless the user clicked that heart in this run (`choice`).
/// True when the heart has to be redrawn.
fn settle_like(set: &mut HashSet<String>, choice: Option<bool>, video_id: &str, liked: bool) -> bool {
    if choice.is_some() {
        return false;
    }
    if liked {
        set.insert(video_id.to_owned())
    } else {
        set.remove(video_id)
    }
}

fn next_id(this: Pin<&mut qobject::Backend>) -> i32 {
    let r = this.rust_mut();
    // SAFETY: only bumps a plain counter in place; the QObject is never moved out of its pin.
    let r = unsafe { r.get_unchecked_mut() };
    r.next_id += 1;
    r.next_id
}

/// Turn a failed request into the `done` signal, flipping the signed-in state if the session is gone.
fn finish(mut b: Pin<&mut qobject::Backend>, id: i32, r: Res) {
    match r {
        Ok(v) => b.as_mut().done(id, QString::default(), QString::from(&v.to_string())),
        Err(e) => {
            // An unreadable page on a signed-in request is nearly always the same thing as a refusal: YouTube answers
            // a session it no longer takes with a "sign in" page instead of an error.
            let suspect = e.starts_with(crate::errors::SESSION_SUSPECT_MSG);
            if suspect && auth::stored_cookie().is_some() && verify_session(b.qt_thread(), false) {
                REFUSED_WAITING.store(true, Ordering::Relaxed);
                return b.as_mut().done(id, QString::from(REFRESHING_MSG), QString::default());
            }
            if e == AUTH_MSG {
                if auth::stored_cookie().is_some() {
                    // Not this request's call to make: it may have been sent with cookies that have been renewed
                    // since (a browser's session, copied, lasts minutes to hours). Ask YouTube about the session as
                    // it is now. That renews it from the browser and has the page loaded again, or signs out if it
                    // really is gone; until then the page says what is going on instead of "sign in again".
                    if verify_session(b.qt_thread(), false) {
                        REFUSED_WAITING.store(true, Ordering::Relaxed);
                        return b.as_mut().done(id, QString::from(REFRESHING_MSG), QString::default());
                    }
                } else {
                    set_logged(false);
                    b.as_mut().set_logged_in(false);
                }
            }
            b.as_mut().done(id, QString::from(&e), QString::default())
        }
    }
}

/// A request slower than this is written to the log.
const SLOW_REQUEST: std::time::Duration = std::time::Duration::from_millis(2500);
/// The UI thread taking longer than this to get to something queued for it is written to the log.
const SLOW_UI: std::time::Duration = std::time::Duration::from_millis(150);

/// Ask the UI thread for a sign of life twice a second and log every time it takes long to answer: a frozen or
/// stuttering window is the UI thread being busy, and the log then says when and for how long. Started once.
fn watch_ui_thread(th: cxx_qt::CxxQtThread<qobject::Backend>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    st().rt.spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let asked = std::time::Instant::now();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let asking = th.queue(move |_| {
                let _ = tx.send(());
            });
            if asking.is_err() {
                return; // the app is closing
            }
            let _ = rx.await;
            let late = asked.elapsed();
            if late >= SLOW_UI {
                tracing::warn!("the UI thread was busy for {} ms", late.as_millis());
            }
        }
    });
}

fn spawn<F>(mut this: Pin<&mut qobject::Backend>, fut: F) -> i32
where
    F: Future<Output = Res> + Send + 'static,
{
    let id = next_id(this.as_mut());
    let th = this.qt_thread();
    st().rt.spawn(async move {
        let started = std::time::Instant::now();
        let r = fut.await;
        let took = started.elapsed();
        let _ = th.queue(move |mut b| {
            // "It lags" has two very different causes; the log tells them apart: YouTube (or a retry) was slow, or
            // the answer sat waiting because the UI thread was busy.
            let waited = started.elapsed().saturating_sub(took);
            if took >= SLOW_REQUEST || waited >= SLOW_UI {
                let what = std::any::type_name::<F>().trim_start_matches("yqt::").trim_end_matches("::{{closure}}");
                tracing::info!(
                    "slow: {what} took {} ms, then waited {} ms for the UI thread (ok: {})",
                    took.as_millis(),
                    waited.as_millis(),
                    r.is_ok()
                );
            }
            if LIKED_DIRTY.swap(false, Ordering::Relaxed) {
                let rev = *b.liked_rev() + 1;
                b.as_mut().set_liked_rev(rev);
            }
            finish(b, id, r);
        });
    });
    id
}

/// RustyPipe error → message for the UI ("headline" or "headline\n\ndetail", see `errors.rs`), for a call made
/// **without** the user's session (`st().rp.query()`: search, charts, an album, a public playlist). A 401/403 here is
/// YouTube refusing the address; it says nothing about the session, so it never becomes `AUTH_MSG` and never signs
/// the user out.
fn err(e: RpError) -> String {
    err_for(false, e)
}

/// The same for a call made through [`authed`]: there a 401/403 means the cookies are no longer valid.
fn err_authed(e: RpError) -> String {
    err_for(true, e)
}

/// The mapper for a call whose query was (`authenticated`) or was not made with the session. Pick it from the query
/// the failing call actually used, one call at a time.
fn err_for(authenticated: bool, e: RpError) -> String {
    crate::errors::friendly_request(&e, LOGGED.load(Ordering::Relaxed), authenticated)
}

fn authed() -> RustyPipeQuery {
    st().rp.query().authenticated()
}

/// Run a request, and once more after a short pause if it failed in a way that is usually transient
/// (an unreadable page – e.g. YouTube briefly answering "sign in" to a session it is rotating – or a network blip).
async fn with_retry<T, F, Fut>(mut f: F) -> Result<T, RpError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, RpError>>,
{
    match f().await {
        Err(e) if crate::errors::worth_retrying(&e) => {
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            f().await
        }
        r => r,
    }
}

/// The signed-in user's liked songs. Falls back to the Library "Songs" list when the dedicated
/// Liked-music page can't be read (YouTube serves it in more than one layout).
///
/// The flag says whether the list really is the liked songs: the fallback also holds songs that are merely in the
/// library (from saved albums), so it is fine to show but must not be taken as "these are liked" for the hearts.
async fn liked_tracks(aq: &RustyPipeQuery) -> Result<(Paginator<TrackItem>, bool), RpError> {
    match with_retry(|| aq.music_liked_tracks()).await {
        Ok(p) => Ok((p.tracks, true)),
        Err(first) => aq.music_saved_tracks().await.map(|p| (p, false)).map_err(|_| first),
    }
}

// ---------------------------------------------------------------------------------------------
// Pagination: the first answer carries a `pager` id; `more(pager)` returns the next page.

enum Pager {
    Tracks(Paginator<TrackItem>, String, String), // section, fallback cover
    Albums(Paginator<AlbumItem>, String),
    Artists(Paginator<ArtistItem>, String),
    Playlists(Paginator<MusicPlaylistItem>, String),
}

impl Pager {
    fn len(&self) -> usize {
        match self {
            Pager::Tracks(p, ..) => p.items.len(),
            Pager::Albums(p, ..) => p.items.len(),
            Pager::Artists(p, ..) => p.items.len(),
            Pager::Playlists(p, ..) => p.items.len(),
        }
    }
    fn has_more(&self) -> bool {
        match self {
            Pager::Tracks(p, ..) => p.ctoken.is_some(),
            Pager::Albums(p, ..) => p.ctoken.is_some(),
            Pager::Artists(p, ..) => p.ctoken.is_some(),
            Pager::Playlists(p, ..) => p.ctoken.is_some(),
        }
    }
    fn items(&self, from: usize) -> Vec<Value> {
        match self {
            Pager::Tracks(p, s, c) => p.items.iter().skip(from).map(|x| track(x, s, c)).collect(),
            Pager::Albums(p, s) => p.items.iter().skip(from).map(|x| album(x, s)).collect(),
            Pager::Artists(p, s) => p.items.iter().skip(from).map(|x| artist(x, s)).collect(),
            Pager::Playlists(p, s) => {
                let rows = p.items.iter().skip(from).map(|x| playlist(x, s)).collect();
                // YouTube's own "Liked Music" (`LM`) is shown once, as the first row of the library (see `library`).
                if from == 0 {
                    rows
                } else {
                    without_youtube_liked(rows)
                }
            }
        }
    }
    async fn extend(&mut self, q: &RustyPipeQuery) -> Result<(), RpError> {
        match self {
            Pager::Tracks(p, ..) => p.extend(q).await.map(|_| ()),
            Pager::Albums(p, ..) => p.extend(q).await.map(|_| ()),
            Pager::Artists(p, ..) => p.extend(q).await.map(|_| ()),
            Pager::Playlists(p, ..) => p.extend(q).await.map(|_| ()),
        }
    }
}

fn is_youtube_liked(row: &Value) -> bool {
    matches!(row["browseId"].as_str(), Some("LM" | "VLLM"))
}

fn without_youtube_liked(rows: Vec<Value>) -> Vec<Value> {
    rows.into_iter().filter(|r| !is_youtube_liked(r)).collect()
}

/// Exactly one "Liked music" row, always first. YouTube's own playlist wins when it is on this page (it has the real
/// cover); otherwise ours stands in. Both open through the same working path (`LIKED`).
fn liked_row_first(items: &mut Vec<Value>) {
    match items.iter().position(is_youtube_liked) {
        Some(at) => {
            let mut row = items.remove(at);
            row["browseId"] = json!("LIKED");
            items.insert(0, row);
        }
        None => items.insert(
            0,
            json!({"title": "Liked music", "subtitle": "Playlist", "artist": "", "kind": "playlist",
            "videoId": "", "browseId": "LIKED", "thumbnail": "", "duration": "", "section": "Playlists",
            "artistId": "", "albumId": ""}),
        ),
    }
}

static NEXT_PAGER: AtomicI32 = AtomicI32::new(0);
/// Lists that no page ever claimed (an answer nobody was waiting for any more) are the only ones that can pile up;
/// this bounds them. A list a page holds, or one that is being fetched from, is never dropped for this.
const MAX_LOOSE_PAGERS: usize = 16;

/// What a list's remaining pages are fetched for besides being shown: the queue / download feed.
#[derive(Debug, Clone, PartialEq)]
struct Feed {
    /// The `done` id its pages are delivered under.
    id: i32,
    /// Append the songs to the queue with this epoch (`Player.queueEpoch`), for as long as that is still the queue.
    queue: Option<i64>,
    /// Queue the songs for download.
    download: bool,
}

/// What to do with a page that has just been fetched (`Pagers::deliver`).
#[derive(Debug, Clone, PartialEq)]
struct Delivery {
    /// The `done` id QML gets it under.
    id: i32,
    queue: Option<i64>,
    download: bool,
    /// What QML is told about the feed: `Some(true)` = more pages will follow by themselves; `None` = no feed.
    feeding: Option<bool>,
}

/// Everything remembered about paged lists, under one lock (never held across an await).
///
/// The rules of the feed live here as plain functions. The ones that decide anything (`feed`, `deliver`, `settle`,
/// `release`) are only ever called on the Qt thread, which is also where QML builds a queue and where a page reaches
/// QML; that is what makes "which pages does this queue already contain" exact (see `deliver`).
struct Pagers<L> {
    /// Lists that can continue, by pager id. A list is absent while a page of it is being fetched.
    lists: HashMap<i32, L>,
    /// Pagers a page on screen has claimed (`Backend.holdPager`), until it lets go (`Backend.dropPager`).
    held: HashSet<i32>,
    /// Pagers a task is fetching pages of right now: one task per pager, so nothing ever races for a list.
    running: HashSet<i32>,
    /// Running pagers whose page has let go: forgotten when the task ends.
    orphaned: HashSet<i32>,
    /// What the running task does with the pages it fetches.
    feeds: HashMap<i32, Feed>,
}

impl<L> Pagers<L> {
    fn new() -> Self {
        Pagers { lists: HashMap::new(), held: HashSet::new(), running: HashSet::new(), orphaned: HashSet::new(), feeds: HashMap::new() }
    }

    /// Remember a list (again). Only unclaimed, idle lists count towards the bound; the oldest of those go first
    /// (ids only grow, so the smallest is the oldest).
    fn keep(&mut self, key: i32, list: L) {
        self.lists.insert(key, list);
        let mut loose: Vec<i32> = self.lists.keys().copied().filter(|k| !self.held.contains(k) && !self.running.contains(k)).collect();
        loose.sort_unstable();
        for old in loose.iter().take(loose.len().saturating_sub(MAX_LOOSE_PAGERS)) {
            self.lists.remove(old);
        }
    }

    /// A page shows this list (nothing to hold once the list has ended and is gone).
    fn hold(&mut self, key: i32) {
        if self.lists.contains_key(&key) || self.running.contains(&key) {
            self.held.insert(key);
        }
    }

    /// The page that showed this list has closed or shows something else. A list that is being fetched from (a page
    /// on its way, a feed) is forgotten when that ends instead.
    fn release(&mut self, key: i32) {
        self.held.remove(&key);
        if self.running.contains(&key) {
            self.orphaned.insert(key);
        } else {
            self.lists.remove(&key);
        }
    }

    /// A feed request (`want` says for what). Returns the `done` id its pages arrive under and whether a task has
    /// to be started: not when one is already fetching this list, whose next page then already belongs to the feed.
    fn feed(&mut self, key: i32, new_id: i32, want: impl FnOnce(&mut Feed)) -> (i32, bool) {
        let feed = self.feeds.entry(key).or_insert(Feed { id: new_id, queue: None, download: false });
        want(feed);
        (feed.id, self.running.insert(key))
    }

    /// A page of `key` has been fetched and is being handed to QML *in this very step* (`more`: the list continues;
    /// `None`: the fetch failed). What is read here is therefore exactly what was asked for before QML had this page:
    /// a queue registered by now was built without it and must get it, a queue built after this will already contain it.
    fn deliver(&self, key: i32, request_id: i32, more: Option<bool>) -> Delivery {
        match self.feeds.get(&key) {
            Some(f) => Delivery {
                id: f.id,
                queue: f.queue.filter(|_| more.is_some()),
                download: f.download && more.is_some(),
                feeding: more.map(|m| m && (f.queue.is_some() || f.download)),
            },
            None => Delivery { id: request_id, queue: None, download: false, feeding: None },
        }
    }

    /// The page's songs have gone where `deliver` said. `gone` is the queue epoch that turned out to be replaced:
    /// only *that* queue stops being fed (a new play request may have joined since `deliver`). Returns whether the
    /// task fetches another page; if not, the task is over.
    fn settle(&mut self, key: i32, gone: Option<i64>, more: bool) -> bool {
        let go_on = self.feeds.get_mut(&key).is_some_and(|f| {
            if gone.is_some() && f.queue == gone {
                f.queue = None;
            }
            more && (f.queue.is_some() || f.download)
        });
        if !go_on {
            self.end(key);
        }
        go_on
    }

    /// The task fetching `key` is over: its feed too, and a list nobody shows any more is forgotten.
    fn end(&mut self, key: i32) {
        self.feeds.remove(&key);
        self.running.remove(&key);
        if self.orphaned.remove(&key) {
            self.lists.remove(&key);
        }
    }
}

static PAGERS: Mutex<Option<Pagers<(Pager, bool)>>> = Mutex::new(None);

fn pagers<R>(f: impl FnOnce(&mut Pagers<(Pager, bool)>) -> R) -> R {
    f(PAGERS.lock().unwrap().get_or_insert_with(Pagers::new))
}

fn keep_pager(key: i32, entry: (Pager, bool)) {
    pagers(|p| p.keep(key, entry));
}

pub(crate) fn drop_pager(key: i32) {
    pagers(|p| p.release(key));
}

/// A page whose list can continue: remembers the pager and tells QML its id.
fn paged(title: &str, subtitle: &str, thumbnail: &str, pager: Pager, auth: bool) -> Value {
    let mut v = page(title, subtitle, thumbnail, pager.items(0));
    let more = pager.has_more();
    if more {
        let id = NEXT_PAGER.fetch_add(1, Ordering::Relaxed) + 1;
        keep_pager(id, (pager, auth));
        v["pager"] = json!(id);
    }
    v["more"] = json!(more);
    v
}

pub(crate) async fn more(key: i32) -> Res {
    let (mut pager, auth) = pagers(|p| p.lists.remove(&key)).ok_or("No more results")?;
    let from = pager.len();
    let q = if auth { authed() } else { st().rp.query() };
    if let Err(e) = pager.extend(&q).await {
        keep_pager(key, (pager, auth)); // allow a retry
        return Err(err_for(auth, e)); // the pager knows whether its pages are fetched with the session
    }
    let items = clean_all(pager.items(from));
    if key == LIKED_PAGER.load(Ordering::Relaxed) {
        remember_liked(items.iter().filter_map(|i| i["videoId"].as_str()).filter(|id| !id.is_empty()).map(str::to_owned));
    }
    let more = pager.has_more();
    if more {
        keep_pager(key, (pager, auth));
    }
    Ok(json!({"items": items, "more": more, "pager": key}))
}

/// The songs of one fetched page (what a feed appends to the queue or downloads).
fn page_songs(page: &Res) -> Vec<Value> {
    let items = page.as_ref().ok().and_then(|v| v["items"].as_array());
    items.into_iter().flatten().filter(|i| i["kind"] == "song").cloned().collect()
}

/// Fetch pages of list `key` and hand each to QML through `done`: one page for a plain `Backend.more` (delivered under
/// `request_id`), and on to the end of the list for as long as a feed wants the rest (`Pagers::feed`).
///
/// Per page, in this order: (1) on the Qt thread the page reaches QML and, in the same step, `Pagers::deliver` reads
/// what its songs are for; (2) on the Qt thread again, the songs are appended to the queue named in (1) if that is
/// still the queue, and `Pagers::settle` decides whether to go on. QML presses "Play" on the Qt thread as well, so a
/// queue it builds either came before (1) - then it lacks this page and is the one (1) reads - or after it, and then
/// it already contains the page and step (2) leaves it alone, because the epoch read in (1) is no longer the queue's.
fn spawn_walk(th: cxx_qt::CxxQtThread<qobject::Backend>, key: i32, request_id: i32) {
    st().rt.spawn(async move {
        loop {
            let r = more(key).await;
            let has_more = r.as_ref().ok().map(|v| v["more"] == true);
            let songs = page_songs(&r);
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = th.queue(move |b| {
                let mut r = r;
                let d = pagers(|p| p.deliver(key, request_id, has_more));
                if let (Ok(v), Some(feeding)) = (&mut r, d.feeding) {
                    v["feeding"] = json!(feeding);
                }
                finish(b, d.id, r);
                // nothing to append: decide right here whether there is a next page
                let settled = d.queue.is_none().then(|| pagers(|p| p.settle(key, None, has_more == Some(true))));
                let _ = tx.send((d, settled));
            });
            let Ok((d, settled)) = rx.await else {
                pagers(|p| p.end(key));
                break;
            };
            if d.download {
                crate::downloads::manager::enqueue_all(songs.iter().filter_map(crate::downloads::manager::Item::from_json).collect());
            }
            let go_on = match (settled, d.queue) {
                (Some(go_on), _) => go_on,
                (None, Some(epoch)) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    crate::player::with_player(move |p| {
                        let gone = (!p.enqueue_fed(epoch, songs)).then_some(epoch);
                        let _ = tx.send(pagers(|p| p.settle(key, gone, has_more == Some(true))));
                    });
                    // (no player to ask: nothing can be fed)
                    rx.await.unwrap_or_else(|_| {
                        pagers(|p| p.end(key));
                        false
                    })
                }
                (None, None) => false,
            };
            if !go_on {
                if d.feeding == Some(true) {
                    // QML was told that more would follow: say that it will not, so its "loading" state cannot stick
                    let last = json!({"items": [], "more": has_more == Some(true), "pager": key, "feeding": false});
                    let _ = th.queue(move |b| finish(b, d.id, Ok(last)));
                }
                break;
            }
        }
    });
}

// ---------------------------------------------------------------------------------------------

pub(crate) async fn search(q: String, filter: String) -> Res {
    let lib = &crate::config::current().library;
    let query = st().rp.query();
    let sec = |s: &str| s.to_owned();
    Ok(match filter.as_str() {
        "songs" => {
            paged("", "", "", Pager::Tracks(query.music_search_tracks(&q).await.map_err(err)?.items, sec("Songs"), String::new()), false)
        }
        "albums" => paged("", "", "", Pager::Albums(query.music_search_albums(&q).await.map_err(err)?.items, sec("Albums")), false),
        "artists" => paged("", "", "", Pager::Artists(query.music_search_artists(&q).await.map_err(err)?.items, sec("Artists")), false),
        "playlists" => {
            paged("", "", "", Pager::Playlists(query.music_search_playlists(&q, false).await.map_err(err)?.items, sec("Playlists")), false)
        }
        _ => {
            let (t, a, ar, p) = tokio::join!(
                query.music_search_tracks(&q),
                query.music_search_albums(&q),
                query.music_search_artists(&q),
                query.music_search_playlists(&q, false)
            );
            let mut v: Vec<Value> = Vec::new();
            v.extend(t.map_err(err)?.items.items.iter().take(lib.search_songs).map(|x| track(x, "Songs", "")));
            if let Ok(ar) = ar {
                v.extend(ar.items.items.iter().take(lib.search_artists).map(|x| artist(x, "Artists")));
            }
            if let Ok(a) = a {
                v.extend(a.items.items.iter().take(lib.search_albums).map(|x| album(x, "Albums")));
            }
            if let Ok(p) = p {
                v.extend(p.items.items.iter().take(lib.search_playlists).map(|x| playlist(x, "Playlists")));
            }
            page("", "", "", v)
        }
    })
}

pub(crate) async fn suggest(q: String) -> Res {
    let s = st().rp.query().music_search_suggestion(&q).await.map_err(err)?;
    Ok(json!(s.terms.into_iter().take(crate::config::current().library.suggestions).collect::<Vec<_>>()))
}

// ---- Home, one independent loader per row -----------------------------------------------------------
// Each returns the items of its row. Slots fix the order on the page, so rows can arrive in any order.

pub(crate) type Row = Result<Vec<Value>, String>;
pub(crate) type RowFuture = std::pin::Pin<Box<dyn Future<Output = Row> + Send>>;

/// "Listen again" from the history, plus a radio seeded from the last song. Empty when signed out.
pub(crate) async fn home_personal() -> Row {
    if !LOGGED.load(Ordering::Relaxed) {
        return Ok(Vec::new());
    }
    let lib = &crate::config::current().library;
    let aq = authed();
    let hist = with_retry(|| aq.music_history()).await.map_err(err_authed)?;
    let mut seen = HashSet::new();
    let recent: Vec<TrackItem> =
        hist.items.into_iter().map(|h| h.item).filter(|t| seen.insert(t.id.clone())).take(lib.history_items).collect();
    let mut v: Vec<Value> = recent.iter().map(|t| track(t, "Listen again", "")).collect();
    if let Some(first) = recent.first() {
        // The radio is a bonus: if it fails the history row still shows.
        if let Ok(r) = st().rp.query().music_radio_track(&first.id).await {
            let section = format!("More like {}", first.name);
            v.extend(r.items.iter().filter(|t| !seen.contains(&t.id)).take(lib.radio_items).map(|t| track(t, &section, "")));
        }
    }
    Ok(v)
}

pub(crate) async fn home_charts() -> Row {
    let n = crate::config::current().library.home_items;
    let c = st().rp.query().music_charts(None).await.map_err(err)?;
    let mut v = Vec::new();
    v.extend(c.trending_tracks.iter().take(n).map(|t| track(t, "Trending", "")));
    v.extend(c.top_tracks.iter().take(n).map(|t| track(t, "Top songs", "")));
    v.extend(c.artists.iter().take(n).map(|a| artist(a, "Top artists")));
    v.extend(c.playlists.iter().take(n).map(|p| playlist(p, "Chart playlists")));
    Ok(v)
}

pub(crate) async fn home_new_albums() -> Row {
    let n = crate::config::current().library.home_items;
    Ok(st().rp.query().music_new_albums().await.map_err(err)?.iter().take(n).map(|a| album(a, "New albums & singles")).collect())
}

pub(crate) async fn home_new_videos() -> Row {
    let n = crate::config::current().library.home_items;
    Ok(st().rp.query().music_new_videos().await.map_err(err)?.iter().take(n).map(|t| track(t, "New music videos", "")).collect())
}

/// How many further pages of the home feed to load after the first (each adds a few shelves). The feed ends by
/// itself, after five more pages when this was written; with 3 here its last five shelves never showed. Each page
/// is shown as it arrives, so the later ones cost the first nothing.
const HOME_FEED_MORE_PAGES: usize = 8;

/// YouTube Music's own personalised home feed: the account's shelves (Quick picks, Listen again, mixes, …).
/// Empty when signed out. A later page failing keeps the shelves already read.
pub(crate) async fn home_feed() -> Row {
    let mut items = Vec::new();
    home_feed_pages(|page| items.extend(page)).await?;
    Ok(items)
}

/// The home feed page by page: `each` gets the shelves of a page as soon as that page is read, so Home can show the
/// first one without waiting for the rest. An error is only the first page's.
async fn home_feed_pages(mut each: impl FnMut(Vec<Value>)) -> Result<(), String> {
    if !LOGGED.load(Ordering::Relaxed) {
        return Ok(());
    }
    let first = crate::ytapi::home_feed(None).await?;
    if crate::homefeed::answered_signed_out(&first) {
        // The cookies were sent and YouTube no longer takes them: say so, rather than pass off the anonymous feed.
        return Err(AUTH_MSG.into());
    }
    let visitor = crate::homefeed::visitor(&first).to_owned();
    let (items, mut next) = crate::homefeed::parse(&first);
    each(items);
    for _ in 0..HOME_FEED_MORE_PAGES {
        let Some(token) = next.take().filter(|_| !visitor.is_empty()) else { break };
        let Ok(resp) = crate::ytapi::home_feed(Some((&token, &visitor))).await else { break };
        let (more, after) = crate::homefeed::parse(&resp);
        each(more);
        next = after;
    }
    Ok(())
}

/// Home for a signed-in user, a page at a time: the account's own feed as its pages arrive, or the built rows (as
/// one page) if YouTube gives no feed. `each` is never handed an empty page.
async fn home_signed_in_pages(mut each: impl FnMut(Vec<Value>)) -> Result<(), String> {
    let mut any = false;
    let feed = home_feed_pages(|page| {
        if !page.is_empty() {
            any = true;
            each(page);
        }
    })
    .await;
    match feed {
        _ if any => Ok(()),
        // a dead session is reported, so the user is signed out rather than shown someone else's charts
        Err(e) if e == AUTH_MSG => Err(e),
        _ => {
            let built = home_built().await?;
            if !built.is_empty() {
                each(built);
            }
            Ok(())
        }
    }
}

/// Whether the home feed's answer is what Home shows. It is when it has shelves, and when the session is gone
/// (so the user is signed out rather than shown someone else's charts); anything else falls back to the built rows.
fn feed_stands(feed: &Row) -> bool {
    match feed {
        Ok(items) => !items.is_empty(),
        Err(e) => e == AUTH_MSG,
    }
}

/// The rows the app builds itself (history, charts, new releases), as one row. An error only if all are missing.
async fn home_built() -> Row {
    let (a, b, c, d) = tokio::join!(home_personal(), home_charts(), home_new_albums(), home_new_videos());
    let mut v = Vec::new();
    let mut first_err = None;
    for row in [a, b, c, d] {
        match row {
            Ok(items) => v.extend(items),
            Err(e) => first_err = first_err.or(Some(e)),
        }
    }
    match first_err {
        Some(e) if v.is_empty() => Err(e),
        _ => Ok(v),
    }
}

/// Home for a signed-in user: the account's own feed, or the built rows if YouTube doesn't give one.
pub(crate) async fn home_signed_in() -> Row {
    let feed = home_feed().await;
    if feed_stands(&feed) {
        feed
    } else {
        home_built().await
    }
}

/// All rows of Home at once (used by the live tests and as a non-progressive fallback).
#[cfg_attr(not(test), allow(dead_code))]
async fn home() -> Res {
    let v = if LOGGED.load(Ordering::Relaxed) { home_signed_in().await? } else { home_built().await? };
    if v.is_empty() {
        return Err("Nothing to show".into());
    }
    Ok(page("", "", "", v))
}

/// Run the rows concurrently, emitting each as it completes, then `done` when the last has finished. Signed in that
/// is one row, the account's own feed; signed out it is the four built rows.
/// Errors are only reported if *every* row failed – a missing row just stays missing.
fn spawn_home(mut this: Pin<&mut qobject::Backend>) -> i32 {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    let id = next_id(this.as_mut());
    let th = this.qt_thread();
    if LOGGED.load(Ordering::Relaxed) {
        // The account's feed: each of its pages is a row of its own (slot 0, 1, …) and is shown the moment it is
        // read, instead of the whole feed after its last page.
        st().rt.spawn(async move {
            let mut slot = 0;
            let pages = home_signed_in_pages(|items| {
                let json = QString::from(&json!({"items": items}).to_string());
                let at = slot;
                slot += 1;
                let _ = th.queue(move |mut b| b.as_mut().partial(id, at, json));
            })
            .await;
            let r: Res = match pages {
                Ok(()) if slot > 0 => Ok(json!({"items": [], "progressive": true})),
                Ok(()) => Err("Nothing to show".into()),
                Err(e) => Err(e),
            };
            let _ = th.queue(move |b| finish(b, id, r));
        });
        return id;
    }
    let rows: Vec<(i32, RowFuture)> =
        vec![(1, Box::pin(home_charts())), (2, Box::pin(home_new_albums())), (3, Box::pin(home_new_videos()))];
    let remaining = Arc::new(AtomicUsize::new(rows.len()));
    let any_rows = Arc::new(AtomicBool::new(false));
    let first_err: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    for (slot, fut) in rows {
        let (th, remaining, any_rows, first_err) = (th.clone(), remaining.clone(), any_rows.clone(), first_err.clone());
        st().rt.spawn(async move {
            match fut.await {
                Ok(items) if !items.is_empty() => {
                    any_rows.store(true, Ordering::Relaxed);
                    let json = QString::from(&json!({"items": items}).to_string());
                    let _ = th.queue(move |mut b| b.as_mut().partial(id, slot, json));
                }
                Ok(_) => {}
                Err(e) => {
                    first_err.lock().unwrap().get_or_insert(e);
                }
            }
            if remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
                let r: Res = if any_rows.load(Ordering::Relaxed) {
                    Ok(json!({"items": [], "progressive": true}))
                } else {
                    Err(first_err.lock().unwrap().take().unwrap_or_else(|| "Nothing to show".into()))
                };
                let _ = th.queue(move |b| finish(b, id, r));
            }
        });
    }
    id
}

async fn library(kind: &str) -> Res {
    let aq = authed();
    Ok(match kind {
        "albums" => paged("", "", "", Pager::Albums(aq.music_saved_albums().await.map_err(err_authed)?, "Albums".into()), true),
        "artists" => paged("", "", "", Pager::Artists(aq.music_saved_artists().await.map_err(err_authed)?, "Artists".into()), true),
        "songs" => {
            paged("", "", "", Pager::Tracks(with_retry(|| aq.music_saved_tracks()).await.map_err(err_authed)?, "".into(), "".into()), true)
        }
        "history" => {
            let h = with_retry(|| aq.music_history()).await.map_err(err_authed)?;
            let v = h.items.iter().map(|h| track(&h.item, h.playback_date_txt.as_deref().unwrap_or("Recently played"), "")).collect();
            page("", "", "", v)
        }
        _ => {
            // First page only: the rest comes in as the list is scrolled. "Liked music" is a fixed first row.
            let p = with_retry(|| aq.music_saved_playlists()).await.map_err(err_authed)?;
            let mut v = paged("", "", "", Pager::Playlists(p, "Playlists".into()), true);
            if let Some(items) = v["items"].as_array_mut() {
                liked_row_first(items);
            }
            v
        }
    })
}

/// Add the rich parts of a page (the hero with its description/bio, stats and Wikipedia link, and horizontal shelves of
/// cards) to the list JSON the page already gets. The page builders in `pages/` are pure and tested; the list itself is
/// unchanged, so a page without these fields simply shows the plain list.
fn with_page_data(mut v: Value, pd: &crate::pagedata::PageData) -> Value {
    use crate::pagedata::SectionStyle;
    if let Some(hero) = &pd.hero {
        v["hero"] = serde_json::to_value(hero).unwrap_or(Value::Null);
    }
    let shelves: Vec<Value> = pd
        .sections
        .iter()
        .filter(|s| matches!(s.style, SectionStyle::Cards | SectionStyle::Artists) && !s.items.is_empty() && !s.id.starts_with("hero-"))
        .filter_map(|s| serde_json::to_value(s).ok())
        .collect();
    if !shelves.is_empty() {
        v["shelves"] = Value::Array(shelves);
    }
    // an artist's radio (the hero's Radio button plays it)
    if let Some(radio) = pd.sections.iter().find(|s| s.id == crate::pages::common::HERO_RADIO).and_then(|s| s.items.first()) {
        v["radio"] = serde_json::to_value(radio).unwrap_or(Value::Null);
    }
    v
}

pub(crate) async fn browse(target: String) -> Res {
    let q = st().rp.query();
    let (kind, id) = target.split_once(':').unwrap_or((target.as_str(), ""));
    match kind {
        "home" => home().await,
        "explore" => {
            let (albums, videos) = tokio::join!(q.music_new_albums(), q.music_new_videos());
            let mut v = Vec::new();
            v.extend(albums.map_err(err)?.iter().map(|a| album(a, "New albums & singles")));
            v.extend(videos.map_err(err)?.iter().map(|t| track(t, "New music videos", "")));
            Ok(page("", "", "", v))
        }
        "album" => {
            let a = q.music_album(id).await.map_err(err)?;
            let cover = thumb(&a.cover);
            let by = a.artists.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", ");
            let sub = format!("{:?} • {} • {}", a.album_type, by, a.year.map(|y| y.to_string()).unwrap_or_default());
            let v = a.tracks.iter().map(|t| track(t, "", &cover)).collect();
            Ok(with_page_data(page(&a.name, &sub, &cover, v), &crate::pages::album::build(&a, &[])))
        }
        "playlist" => {
            // every spelling of "liked music" (our synthetic row, YouTube's `LM` / `VLLM`) takes the path that works
            let id = crate::models::items::canonical_playlist_id(id);
            if id == crate::models::items::LIKED_ID {
                // First page only; the rest loads on scroll. What the real Liked page shows is liked, so remember it for
                // the hearts (the library fallback also lists songs that are not).
                let (tracks, really_liked) = liked_tracks(&authed()).await.map_err(err_authed)?;
                if really_liked {
                    remember_liked(tracks.items.iter().map(|t| t.id.clone()));
                }
                let v = paged("Liked music", "", "", Pager::Tracks(tracks, "".into(), "".into()), true);
                if really_liked {
                    // the pages that follow (on scroll, or fed into the queue) are liked songs as well
                    LIKED_PAGER.store(v["pager"].as_i64().unwrap_or(0) as i32, Ordering::Relaxed);
                }
                return Ok(v);
            }
            let id = id.trim_start_matches("VL");
            let p = match with_retry(|| q.music_playlist(id)).await {
                Ok(p) => p,
                // The account's own mixes on the home feed ("My Supermix" …) have no playlist page to read: they are
                // radios, and only the account they were mixed for gets them.
                Err(e) if LOGGED.load(Ordering::Relaxed) && id.starts_with("RD") => {
                    let mix = authed().music_radio_playlist(id).await.map_err(|_| err(e))?;
                    // no title of its own: the page keeps the name of the card it was opened from
                    return Ok(paged("", "", "", Pager::Tracks(mix, "".into(), "".into()), true));
                }
                Err(e) => return Err(err(e)),
            };
            let cover = thumb(&p.thumbnail);
            let by = p.channel.as_ref().map(|c| c.name.as_str()).unwrap_or("");
            let sub = match p.track_count {
                Some(n) => format!("{by} • {n} songs"),
                None => by.to_owned(),
            };
            let pd = crate::pages::playlist::build(&p);
            let name = p.name.clone();
            Ok(with_page_data(
                paged(&name, sub.trim_start_matches(" • "), &cover, Pager::Tracks(p.tracks, "".into(), cover.clone()), false),
                &pd,
            ))
        }
        "artist" => {
            let a = q.music_artist(id, true).await.map_err(err)?;
            // The list holds the top songs; albums, singles, playlists and similar artists are shelves of cards.
            let v = a.tracks.iter().map(|t| track(t, "Top songs", "")).collect();
            let sub = a.subscriber_count.map(crate::items::subscribers).unwrap_or_default();
            Ok(with_page_data(page(&a.name, &sub, &thumb(&a.header_image), v), &crate::pages::artist::build(&a)))
        }
        "library" => library(id).await,
        _ => Err(format!("unknown target {target}")),
    }
}

async fn set_cookie(input: Input) -> Result<(), String> {
    let rp = &st().rp;
    match input {
        Input::Header(h) => rp.user_auth_set_cookie(h).await,
        Input::Netscape(t) => rp.user_auth_set_cookie_txt(&t).await,
    }
    .map_err(|e| match e {
        RpError::Auth(_) => "Those cookies aren't signed in. Log in at music.youtube.com first.".to_owned(),
        e => e.to_string(),
    })
}

async fn login_import(id: String) -> Res {
    let from = id.clone();
    let (browser, header) = tokio::task::spawn_blocking(move || auth::import_from_browser(&id)).await.map_err(|e| e.to_string())??;
    set_cookie(Input::Header(header)).await?;
    auth::remember_browser(Some(&from)); // an expired session is renewed from here (see `refresh_login`)
    Ok(json!({"browser": browser}))
}

async fn login_cookie(text: String) -> Res {
    set_cookie(auth::parse_input(&text)?).await?;
    auth::remember_browser(None); // pasted cookies can't be renewed
    Ok(json!({"browser": "pasted cookies"}))
}

/// Take the browser's current cookies again, if the sign-in came from a browser. For a caller outside the request
/// plumbing (the player) whose signed-in call was refused; the signed-in state itself is left to `verify_session`.
pub(crate) async fn renew_session() -> bool {
    match auth::remembered_browser() {
        Some(browser) => match login_import(browser.clone()).await {
            Ok(_) => {
                tracing::info!("renewed the sign-in from {browser} for playback");
                true
            }
            Err(e) => {
                tracing::warn!("the sign-in could not be renewed from {browser}: {e}");
                false
            }
        },
        None => false,
    }
}

/// What YouTube said about the stored sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    Accepted,
    /// YouTube answered and does not take the cookies any more.
    Refused,
    /// No answer to go by (offline, a server error).
    Unknown,
}

/// What a look at the stored sign-in ends in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    /// Signed in, with the cookies that were stored.
    Good,
    /// The stored cookies were stale and have been replaced by the browser's current ones: whatever was fetched
    /// (or refused) with the old ones has to be fetched again.
    Renewed,
    /// Signed out: nothing stored, or refused and not renewable.
    Dead,
}

/// `renewed` is only looked at (and the browser only read) when the session was refused.
fn session_after(check: Check, has_cookie: bool, renewed: impl FnOnce() -> bool) -> Session {
    match check {
        Check::Accepted => Session::Good,
        // offline: trust the stored cookie, a request will say otherwise soon enough
        Check::Unknown if has_cookie => Session::Good,
        Check::Unknown => Session::Dead,
        Check::Refused if renewed() => Session::Renewed,
        Check::Refused => Session::Dead,
    }
}

/// What a page shows while its refused request waits for the session check.
const REFRESHING_MSG: &str = "Refreshing your sign-in…";
/// A refused request was told `REFRESHING_MSG`: whatever the check finds, its page must hear about it.
static REFUSED_WAITING: AtomicBool = AtomicBool::new(false);
static VERIFYING: AtomicBool = AtomicBool::new(false);
static LAST_VERIFY: Mutex<Option<std::time::Instant>> = Mutex::new(None);
/// A request refused for want of a session asks for a check; a burst of them (a page's rows) must not ask a dozen times.
const VERIFY_EVERY: std::time::Duration = std::time::Duration::from_secs(20);

/// May a check start now? Never two at once; and, unless `forced` (the start of the app, the Settings page), not
/// again within `VERIFY_EVERY` of the last one.
fn may_verify(last: Option<std::time::Instant>, now: std::time::Instant, forced: bool) -> bool {
    forced || last.is_none_or(|at| now.duration_since(at) >= VERIFY_EVERY)
}

/// Ask YouTube whether the stored sign-in still stands and publish the answer. Browsers keep rotating these
/// cookies, so a stored copy goes stale (typically overnight); if the sign-in came from a browser its current cookies
/// are taken again instead of asking the user to. Only when that fails too is the user signed out.
///
/// A request that came back "session expired" calls this as well, instead of signing the user out on its own word:
/// right after the start such requests were sent with the stale cookies while the renewal was still running, and
/// used to leave the app signed out although the renewal had succeeded.
///
/// Returns whether an answer is on its way (a check was started, or one is running): the caller's pages are then
/// loaded again when it arrives (`session_renewed`), or the user is signed out.
fn verify_session(th: cxx_qt::CxxQtThread<qobject::Backend>, forced: bool) -> bool {
    {
        let mut last = LAST_VERIFY.lock().unwrap();
        let now = std::time::Instant::now();
        if VERIFYING.load(Ordering::Relaxed) {
            return true;
        }
        if !may_verify(*last, now, forced) {
            return false;
        }
        VERIFYING.store(true, Ordering::Relaxed); // (under the lock above, so two callers cannot both start one)
        *last = Some(now);
    }
    st().rt.spawn(async move {
        let had_cookie = auth::stored_cookie().is_some();
        let check = match st().rp.user_auth_check_cookie().await {
            Ok(()) => Check::Accepted,
            Err(RpError::Auth(_)) => Check::Refused,
            Err(e) => {
                tracing::info!("sign-in check got no answer ({}); keeping the stored session", err(e));
                Check::Unknown
            }
        };
        let mut renewed = false;
        if check == Check::Refused && had_cookie {
            match auth::remembered_browser() {
                Some(browser) => match login_import(browser.clone()).await {
                    Ok(_) => {
                        tracing::info!("the stored sign-in had expired; renewed it from {browser}");
                        renewed = true;
                    }
                    // (the reason names the browser and what went wrong, never a cookie)
                    Err(e) => tracing::warn!("the stored sign-in expired and could not be renewed from {browser}: {e}"),
                },
                None => tracing::warn!("the stored sign-in expired and there is no browser to renew it from (pasted cookies)"),
            }
        }
        let outcome = session_after(check, had_cookie, || renewed);
        let ok = outcome != Session::Dead;
        if outcome == Session::Renewed {
            // pages cached while the old cookies were refused must not be shown as this session's
            LIKED_DIRTY.store(true, Ordering::Relaxed);
            LOGIN_EPOCH.fetch_add(1, Ordering::Relaxed);
        }
        set_logged(ok);
        VERIFYING.store(false, Ordering::Relaxed);
        // Publish the sign-in state FIRST; the liked-songs preload below is slow and must never gate it.
        let th_liked = th.clone();
        let expired = !ok && had_cookie;
        let _ = th.queue(move |mut b| {
            b.as_mut().set_logged_in(ok);
            // (read here, on the Qt thread, where `finish` sets it: a request refused a moment ago is not missed)
            let waiting = REFUSED_WAITING.swap(false, Ordering::Relaxed);
            match outcome {
                Session::Renewed => b.as_mut().session_renewed(),
                // the session stands, but a request was refused and told "refreshing your sign-in": it is worth
                // one more go (and only one: the next check is `VERIFY_EVERY` away, so this cannot loop)
                Session::Good if waiting => b.as_mut().session_renewed(),
                Session::Dead if expired => b.as_mut().session_expired(),
                _ => {}
            }
        });
        if ok && (forced || outcome == Session::Renewed) {
            load_liked().await;
            let _ = th_liked.queue(|mut b| {
                if LIKED_DIRTY.swap(false, Ordering::Relaxed) {
                    let rev = *b.liked_rev() + 1;
                    b.as_mut().set_liked_rev(rev);
                }
            });
        }
    });
    true
}

/// Fill the "liked" set (for the hearts) in the background. It waits a few seconds so it never competes with
/// what the user is actually looking at, and only reads as far as `liked_limit` allows.
async fn load_liked() {
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let aq = authed();
    // Only the real Liked page: the library fallback would mark (and smart-download) songs that are not liked.
    let Ok((mut tracks, true)) = liked_tracks(&aq).await else { return };
    let _ = tracks.extend_limit(&aq, crate::config::current().library.liked_limit).await;
    // Merge instead of replacing: pages the user already browsed may hold songs beyond the limit.
    // (and a heart clicked while this list was being read stays as the user left it)
    remember_liked(tracks.items.iter().map(|t| t.id.clone()));
    // The smart-download rule "keep liked songs" gets its input here (it does nothing unless smart downloads are on).
    if crate::config::current().downloads.smart_enabled {
        let items = tracks
            .items
            .iter()
            .map(|t| crate::downloads::manager::Item {
                id: t.id.clone(),
                title: t.name.clone(),
                artist: t.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
                album: t.album.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
                cover: t.cover.last().map(|c| c.url.clone()).unwrap_or_default(),
                duration_ms: t.duration.map_or(0, |d| i64::from(d) * 1000),
            })
            .collect();
        crate::downloads::manager::set_inputs("liked", items);
    }
}

/// The user's playlists for the "Add to playlist" dialog: first page now, the rest via `more`.
async fn my_playlists() -> Res {
    let aq = authed();
    let p = with_retry(|| aq.music_saved_playlists()).await.map_err(err_authed)?;
    Ok(paged("", "", "", Pager::Playlists(p, "Playlists".into()), true))
}

/// Lyrics from every enabled provider at once (LRCLIB, NetEase, YouTube); synced lyrics always win over plain ones.
async fn lyrics(video_id: String, title: String, artist: String, seconds: u32) -> Res {
    use crate::lyrics::{rank::Query, LyricsResult};
    let cfg = crate::config::current();
    let q = Query { video_id, title, artist, duration_s: seconds };
    let dir = crate::lyrics::cache::default_dir();
    match crate::lyrics::resolve(crate::state::http(), &cfg.lyrics, cfg.security.allow_insecure_urls, &q, dir.as_deref()).await {
        LyricsResult::Synced { lines, source } => {
            // `[ms, text, end, gap, fillMs]` rows: the karaoke fill in NowPlayingPage.qml reads the fill time
            let lines = crate::lyrics::layout::values(&lines, i64::from(seconds) * 1000, cfg.lyrics.gap_dots);
            Ok(json!({"synced": lines, "plain": "", "source": source}))
        }
        LyricsResult::Plain { text, source } => Ok(json!({"synced": [], "plain": text, "source": source})),
        LyricsResult::Unavailable => Err("Lyrics unavailable".to_owned()),
        LyricsResult::Failed(e) => Err(e.ui_string()),
    }
}

impl qobject::Backend {
    fn lyrics(self: Pin<&mut Self>, video_id: &QString, title: &QString, artist: &QString, seconds: i32) -> i32 {
        spawn(self, lyrics(video_id.to_string(), title.to_string(), artist.to_string(), seconds.max(0) as u32))
    }
    fn search(self: Pin<&mut Self>, query: &QString, filter: &QString) -> i32 {
        spawn(self, search(query.to_string(), filter.to_string()))
    }
    fn suggest(self: Pin<&mut Self>, query: &QString) -> i32 {
        spawn(self, suggest(query.to_string()))
    }
    fn browse(self: Pin<&mut Self>, target: &QString) -> i32 {
        spawn(self, browse(target.to_string()))
    }
    fn browse_home(self: Pin<&mut Self>) -> i32 {
        spawn_home(self)
    }
    fn more(mut self: Pin<&mut Self>, pager: i32) -> i32 {
        // one task per list: a second request while a page is on its way has nothing to fetch from
        if !pagers(|p| p.running.insert(pager)) {
            return spawn(self, async { Err("No more results".to_owned()) });
        }
        let id = next_id(self.as_mut());
        spawn_walk(self.qt_thread(), pager, id);
        id
    }
    fn browsers(&self) -> QString {
        let list: Vec<Value> =
            auth::detect().iter().map(|b| json!({"id": b.id, "name": b.name, "isDefault": b.is_default, "note": b.note})).collect();
        QString::from(&Value::Array(list).to_string())
    }
    fn login_import(self: Pin<&mut Self>, browser: &QString) -> i32 {
        spawn(self, login_import(browser.to_string()))
    }
    fn login_cookie(self: Pin<&mut Self>, text: &QString) -> i32 {
        spawn(self, login_cookie(text.to_string()))
    }
    fn logout(self: Pin<&mut Self>) {
        let th = self.qt_thread();
        set_logged(false);
        liked_set(|s| s.clear());
        auth::remember_browser(None); // signed out on purpose: don't sign back in at the next start
        st().rt.spawn(async move {
            let _ = st().rp.user_auth_remove_cookie().await;
            let _ = st().rp.user_auth_logout().await; // also drops any OAuth token left by older versions
            let _ = th.queue(|mut b| {
                b.as_mut().set_logged_in(false);
                let rev = *b.liked_rev() + 1;
                b.as_mut().set_liked_rev(rev);
            });
        });
    }
    fn refresh_login(mut self: Pin<&mut Self>) {
        // Optimistic: a stored cookie means "signed in" right now, so the Library and Home don't wait on a network
        // round trip. The check corrects this if YouTube no longer accepts the session.
        if auth::stored_cookie().is_some() {
            set_logged(true);
            self.as_mut().set_logged_in(true);
        }
        verify_session(self.qt_thread(), true);
        watch_ui_thread(self.qt_thread());
    }
    fn notify_error(&self, summary: &QString, body: &QString, window_active: bool) -> bool {
        let out = crate::notify::goes_to_desktop(window_active, crate::config::current().integration.notifications);
        if out {
            crate::notify::error(&summary.to_string(), &body.to_string());
        }
        out
    }
    fn is_liked(&self, video_id: &QString) -> bool {
        liked_set(|s| s.contains(&video_id.to_string()))
    }
    fn confirm_liked(self: Pin<&mut Self>, video_id: &QString) {
        let vid = video_id.to_string();
        if vid.is_empty() || !is_logged_in() {
            return;
        }
        let th = self.qt_thread();
        st().rt.spawn(async move {
            // (no answer, or a song YouTube states no heart for: what is shown stays)
            let Ok(Some(liked)) = ytapi::like_status(&vid).await else { return };
            let redraw = liked_set(|s| {
                let choice = like_choices(|c| c.get(&vid).copied());
                settle_like(s, choice, &vid, liked)
            });
            if redraw {
                let _ = th.queue(|mut b| {
                    let rev = *b.liked_rev() + 1;
                    b.as_mut().set_liked_rev(rev);
                });
            }
        });
    }
    fn drop_pager(&self, pager: i32) {
        drop_pager(pager);
    }
    fn hold_pager(&self, pager: i32) {
        pagers(|p| p.hold(pager));
    }
    fn feed_queue(self: Pin<&mut Self>, pager: i32, epoch: i64) -> i32 {
        self.start_feed(pager, move |f| f.queue = Some(epoch))
    }
    fn feed_downloads(self: Pin<&mut Self>, pager: i32) -> i32 {
        self.start_feed(pager, |f| f.download = true)
    }
    /// Register the request with the list's feed and make sure a task is walking the list (see `spawn_walk`).
    fn start_feed(mut self: Pin<&mut Self>, pager: i32, want: impl FnOnce(&mut Feed)) -> i32 {
        let new_id = next_id(self.as_mut());
        let (id, start) = pagers(|p| p.feed(pager, new_id, want));
        if start {
            spawn_walk(self.qt_thread(), pager, id);
        }
        id
    }
    fn trim_memory(&self) {
        crate::state::trim_memory();
    }
    fn set_liked(mut self: Pin<&mut Self>, video_id: &QString, liked: bool) -> i32 {
        let vid = video_id.to_string();
        let th = self.qt_thread();
        // Optimistic: the heart flips immediately; if YouTube refuses, it flips back and `like_failed` says why.
        liked_set(|s| if liked { s.insert(vid.clone()) } else { s.remove(&vid) });
        like_choices(|c| c.insert(vid.clone(), liked));
        let rev = *self.liked_rev() + 1;
        self.as_mut().set_liked_rev(rev);
        spawn(self, async move {
            let _turn = LIKE_ORDER.lock().await;
            let mut sent = ytapi::rate(&vid, liked).await;
            // A session YouTube stopped accepting is renewed and the like sent again, instead of the heart quietly
            // flipping back.
            if matches!(&sent, Err(e) if e == AUTH_MSG) && renew_session().await {
                sent = ytapi::rate(&vid, liked).await;
            }
            // The tick in the bar means "YouTube has it": an "ok" alone is not taken for that, the song is asked
            // about again. (No answer to that question leaves the "ok" standing.)
            if sent.is_ok() {
                if let Ok(Some(now)) = ytapi::like_status(&vid).await {
                    if now != liked {
                        sent = Err("YouTube answered, but the song's like did not change. Try again in a moment.".to_owned());
                    }
                }
            }
            match sent {
                Ok(()) => {
                    tracing::info!("YouTube took the {} of {vid}", if liked { "like" } else { "unlike" });
                    Ok(json!({"liked": liked}))
                }
                Err(message) => {
                    tracing::warn!("YouTube did not take the {} of {vid}: {message}", if liked { "like" } else { "unlike" });
                    if !like_choices(|c| like_needs_undo(c.get(&vid).copied(), liked)) {
                        return Err(message); // clicked again since: that click decides what the heart shows
                    }
                    like_choices(|c| c.remove(&vid));
                    liked_set(|s| if liked { s.remove(&vid) } else { s.insert(vid.clone()) });
                    LIKED_DIRTY.store(true, Ordering::Relaxed); // makes `spawn` bump the revision, so the heart is redrawn
                    let (v, m) = (QString::from(&vid), QString::from(&message));
                    let _ = th.queue(move |mut b| b.as_mut().like_failed(v, m));
                    Err(message)
                }
            }
        })
    }
    fn my_playlists(self: Pin<&mut Self>) -> i32 {
        spawn(self, my_playlists())
    }
    fn add_to_playlist(self: Pin<&mut Self>, playlist_id: &QString, video_id: &QString) -> i32 {
        let (p, v) = (playlist_id.to_string(), video_id.to_string());
        spawn(self, async move {
            ytapi::add_to_playlist(&p, &v).await?;
            Ok(json!({"added": true}))
        })
    }
}

/// The feed's rules, driven step by step in the order things happen on the Qt thread (`spawn_walk`).
#[cfg(test)]
mod feed_tests {
    use super::*;

    fn book() -> Pagers<()> {
        let mut p = Pagers::new();
        p.keep(7, ());
        p.hold(7);
        p
    }
    /// `Backend.feedQueue(key, epoch)`.
    fn play(p: &mut Pagers<()>, key: i32, id: i32, epoch: i64) -> (i32, bool) {
        p.feed(key, id, |f| f.queue = Some(epoch))
    }
    /// Step 2 of a page for a queue that is still there / that has been replaced.
    fn appended(p: &mut Pagers<()>, d: &Delivery, queue_now: i64, more: bool) -> bool {
        let gone = d.queue.filter(|e| *e != queue_now);
        p.settle(7, gone, more)
    }

    #[test]
    fn a_feed_walks_to_the_end_of_the_list_and_tells_qml_when_it_is_over() {
        let mut p = book();
        assert_eq!(play(&mut p, 7, 100, 1), (100, true), "nothing is fetching this list yet: a task starts");
        let d = p.deliver(7, 100, Some(true));
        assert_eq!(d, Delivery { id: 100, queue: Some(1), download: false, feeding: Some(true) });
        assert!(appended(&mut p, &d, 1, true), "the queue is still there: next page");
        let d = p.deliver(7, 100, Some(false));
        assert_eq!((d.queue, d.feeding), (Some(1), Some(false)), "the last page is still queued; QML learns the feed is over");
        assert!(!appended(&mut p, &d, 1, false));
        assert!(p.feeds.is_empty() && p.running.is_empty());
    }

    #[test]
    fn a_plain_page_request_is_one_page_under_its_own_id() {
        let mut p = book();
        p.running.insert(7);
        let d = p.deliver(7, 55, Some(true));
        assert_eq!(d, Delivery { id: 55, queue: None, download: false, feeding: None });
        assert!(!p.settle(7, None, true), "nobody wants the rest");
        assert!(p.running.is_empty());
    }

    // 1a: "Play" again on the same list while its feed is running, at each point of a page's journey.

    #[test]
    fn play_again_before_the_page_reaches_qml_gets_that_page_appended_exactly_once() {
        let mut p = book();
        play(&mut p, 7, 100, 1);
        // the page is fetched; before it is delivered, QML replaces the queue (built WITHOUT this page) and asks again
        assert_eq!(play(&mut p, 7, 101, 2), (100, false), "joins the running feed, same id, no second task");
        let d = p.deliver(7, 100, Some(true));
        assert_eq!(d.queue, Some(2), "the page goes to the new queue, which lacks it");
        assert!(appended(&mut p, &d, 2, true));
    }

    #[test]
    fn play_again_between_delivery_and_appending_does_not_get_the_page_twice_and_keeps_being_fed() {
        let mut p = book();
        play(&mut p, 7, 100, 1);
        let d = p.deliver(7, 100, Some(true)); // QML has the page now ...
        assert_eq!(d.queue, Some(1));
        play(&mut p, 7, 101, 2); // ... so the queue it builds next already contains it
                                 // step 2 finds queue 1 replaced: nothing is appended, and only queue 1 stops being fed
        assert!(appended(&mut p, &d, 2, true), "the feed goes on, for the new queue");
        assert_eq!(p.feeds[&7].queue, Some(2), "the new request is not wiped out by the old queue's failure");
        assert_eq!(p.deliver(7, 100, Some(true)).queue, Some(2));
    }

    #[test]
    fn play_again_after_appending_simply_continues_with_the_new_queue() {
        let mut p = book();
        play(&mut p, 7, 100, 1);
        let d = p.deliver(7, 100, Some(true));
        assert!(appended(&mut p, &d, 1, true));
        play(&mut p, 7, 101, 2);
        let d = p.deliver(7, 100, Some(true));
        assert_eq!(d.queue, Some(2));
        assert!(appended(&mut p, &d, 2, true));
    }

    #[test]
    fn a_replaced_queue_ends_the_feed_and_qml_is_told_even_without_a_page() {
        let mut p = book();
        play(&mut p, 7, 100, 1);
        let d = p.deliver(7, 100, Some(true));
        assert_eq!(d.feeding, Some(true), "QML was told more would follow ...");
        assert!(!appended(&mut p, &d, 9, true), "... but the queue was replaced by something else: the walk is over");
        // (`spawn_walk` sends the final `feeding: false` notice exactly in this case: told true, not going on)
        assert!(p.feeds.is_empty() && p.running.is_empty());
        assert!(p.lists.contains_key(&7), "the page that still shows the list keeps its pager");
        // and the page can ask again: that is a new walk
        assert_eq!(play(&mut p, 7, 102, 9), (102, true));
    }

    #[test]
    fn a_download_feed_survives_the_queue_being_replaced_and_both_share_one_walk() {
        let mut p = book();
        play(&mut p, 7, 100, 1);
        assert_eq!(p.feed(7, 101, |f| f.download = true), (100, false));
        let d = p.deliver(7, 100, Some(true));
        assert_eq!((d.queue, d.download), (Some(1), true));
        assert!(appended(&mut p, &d, 9, true), "the downloads still want the rest");
        let d = p.deliver(7, 100, Some(true));
        assert_eq!((d.queue, d.download, d.feeding), (None, true, Some(true)));
    }

    // 1b: "Play" while a page asked for by scrolling is on its way.

    #[test]
    fn a_page_already_on_its_way_becomes_the_first_page_of_the_feed() {
        let mut p = book();
        p.running.insert(7); // Backend.more(7), request id 55
        assert_eq!(play(&mut p, 7, 100, 1), (100, false), "no second task: the one fetching continues as the feed");
        let d = p.deliver(7, 55, Some(true));
        assert_eq!(d, Delivery { id: 100, queue: Some(1), download: false, feeding: Some(true) }, "under the feed's id, into the queue");
        assert!(appended(&mut p, &d, 1, true), "and on to the following pages, whether or not the page is still open");
    }

    // 1c: pager lifetime.

    #[test]
    fn a_list_whose_page_closed_is_dropped_when_its_walk_ends_not_before() {
        for end in ["done", "replaced", "error"] {
            let mut p = book();
            play(&mut p, 7, 100, 1);
            p.release(7); // the page closes while the feed runs
            assert!(p.lists.contains_key(&7) && p.running.contains(&7), "{end}: the walk keeps its list");
            match end {
                "done" => {
                    let d = p.deliver(7, 100, Some(true));
                    assert!(appended(&mut p, &d, 1, true));
                    assert!(p.lists.contains_key(&7));
                    let d = p.deliver(7, 100, Some(false));
                    assert!(!appended(&mut p, &d, 1, false));
                }
                "replaced" => {
                    let d = p.deliver(7, 100, Some(true));
                    assert!(!appended(&mut p, &d, 9, true));
                }
                _ => {
                    let d = p.deliver(7, 100, None);
                    assert_eq!((d.id, d.queue, d.feeding), (100, None, None), "an error reaches QML under the feed's id");
                    assert!(!p.settle(7, None, false));
                }
            }
            assert!(!p.lists.contains_key(&7), "{end}: nobody is left to use the list");
            assert!(p.running.is_empty() && p.orphaned.is_empty() && p.feeds.is_empty() && p.held.is_empty(), "{end}");
        }
    }

    #[test]
    fn a_page_in_flight_when_its_page_closes_does_not_bring_the_list_back() {
        let mut p = book();
        p.running.insert(7);
        p.lists.remove(&7); // `more` took it out to fetch
        p.release(7);
        p.keep(7, ()); // `more` puts it back: the list continues
        let d = p.deliver(7, 55, Some(true));
        assert!(!p.settle(7, d.queue, true));
        assert!(p.lists.is_empty());
    }

    #[test]
    fn only_lists_nobody_uses_are_ever_dropped_for_the_bound() {
        let mut p: Pagers<()> = Pagers::new();
        p.keep(1, ());
        p.hold(1); // a page shows it
        p.keep(2, ());
        play(&mut p, 2, 100, 1); // a feed walks it
        p.keep(3, ()); // nobody claimed these
        for key in 4..(4 + MAX_LOOSE_PAGERS as i32 + 5) {
            p.keep(key, ());
        }
        assert!(p.lists.contains_key(&1) && p.lists.contains_key(&2), "in use: kept whatever their age");
        assert!(!p.lists.contains_key(&3), "the oldest unclaimed list went");
        assert_eq!(p.lists.len(), MAX_LOOSE_PAGERS + 2);
        // letting go of an idle list forgets it at once; a list that ended cannot be held
        p.release(1);
        assert!(!p.lists.contains_key(&1));
        p.hold(999);
        assert!(p.held.is_empty());
    }

    #[test]
    fn the_songs_of_a_page_are_what_a_feed_appends() {
        let page: Res = Ok(json!({"items": [{"kind": "song", "videoId": "a"}, {"kind": "album", "browseId": "b"}], "more": true}));
        let songs = page_songs(&page);
        assert_eq!(songs.len(), 1);
        assert!(songs[0].get("feed").is_none(), "nothing about the feed is written into the songs (or the saved session)");
        assert!(page_songs(&Err("x".into())).is_empty());
    }
}

#[cfg(test)]
mod like_tests {
    use super::*;

    #[test]
    fn a_list_read_from_youtube_does_not_undo_a_click() {
        // the list was on its way when the user unliked `a` and liked `c`
        let mut set: HashSet<String> = ["a", "b"].into_iter().map(String::from).collect();
        let choices: HashMap<String, bool> = [("a".to_owned(), false), ("c".to_owned(), true)].into_iter().collect();
        apply_like_choices(&mut set, &choices);
        assert!(!set.contains("a") && set.contains("b") && set.contains("c"));
    }

    #[test]
    fn a_refused_like_is_undone_only_while_it_is_the_latest_click() {
        assert!(like_needs_undo(Some(true), true));
        assert!(!like_needs_undo(Some(false), true), "unliked again since: the newer click stands");
        assert!(!like_needs_undo(None, true));
    }

    #[test]
    fn youtubes_word_on_one_song_corrects_its_heart_but_never_a_click() {
        let mut set: HashSet<String> = ["a"].into_iter().map(String::from).collect();
        assert!(settle_like(&mut set, None, "b", true), "liked far down the list, or in another app: the heart fills");
        assert!(set.contains("b"));
        assert!(!settle_like(&mut set, None, "b", true), "already shown: nothing to redraw");
        assert!(settle_like(&mut set, None, "a", false), "unliked elsewhere: the heart empties");
        assert!(!set.contains("a"));
        assert!(!settle_like(&mut set, Some(false), "b", false), "clicked in this run: the click's own request decides");
        assert!(set.contains("b"));
    }
}

#[cfg(test)]
mod session_check_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_refused_session_is_renewed_from_the_browser_before_anyone_is_signed_out() {
        let never = || -> bool { panic!("the browser is only read when the session was refused") };
        assert_eq!(session_after(Check::Accepted, true, never), Session::Good);
        assert_eq!(session_after(Check::Unknown, true, never), Session::Good, "offline: the stored cookie is trusted");
        assert_eq!(session_after(Check::Unknown, false, never), Session::Dead);
        assert_eq!(session_after(Check::Refused, true, || true), Session::Renewed);
        assert_eq!(session_after(Check::Refused, true, || false), Session::Dead);
    }

    #[test]
    fn a_burst_of_refused_requests_asks_once() {
        let t0 = Instant::now();
        assert!(may_verify(None, t0, false));
        assert!(!may_verify(Some(t0), t0 + Duration::from_secs(1), false));
        assert!(may_verify(Some(t0), t0 + VERIFY_EVERY, false));
        assert!(may_verify(Some(t0), t0 + Duration::from_secs(1), true), "the start of the app and Settings always ask");
    }
}

#[cfg(test)]
mod page_data_tests {
    use super::*;
    use crate::pagedata::{Hero, Item, ItemKind, PageData, Section, SectionStyle};

    fn card(title: &str) -> Item {
        Item { kind: ItemKind::Album, title: title.into(), browse_id: "MPREb_x".into(), ..Default::default() }
    }
    fn section(id: &str, style: SectionStyle, items: Vec<Item>) -> Section {
        Section { id: id.into(), title: id.into(), style, items, ..Default::default() }
    }

    #[test]
    fn hero_and_card_shelves_are_added_and_lists_are_not() {
        let pd = PageData {
            hero: Some(Hero {
                title: "Band".into(),
                description: "bio".into(),
                wikipedia_url: "https://en.wikipedia.org/wiki/Band".into(),
                ..Default::default()
            }),
            sections: vec![
                section("top-songs", SectionStyle::List, vec![card("song")]),
                section("albums", SectionStyle::Cards, vec![card("A")]),
                section("similar", SectionStyle::Artists, vec![card("B")]),
                section("empty", SectionStyle::Cards, vec![]),
            ],
            ..Default::default()
        };
        let v = with_page_data(json!({"title": "Band", "items": []}), &pd);
        assert_eq!(v["hero"]["description"], "bio");
        assert_eq!(v["hero"]["wikipediaUrl"], "https://en.wikipedia.org/wiki/Band");
        let titles: Vec<_> = v["shelves"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(titles, ["albums", "similar"], "only non-empty card/artist shelves; songs stay in the list");
        assert!(v.get("radio").is_none());
    }

    #[test]
    fn an_artists_radio_is_passed_on_and_not_shown_as_a_shelf() {
        let radio = Item { kind: ItemKind::Playlist, title: "Band Radio".into(), browse_id: "RDEM1".into(), ..Default::default() };
        let pd =
            PageData { sections: vec![section(crate::pages::common::HERO_RADIO, SectionStyle::Cards, vec![radio])], ..Default::default() };
        let v = with_page_data(json!({}), &pd);
        assert_eq!(v["radio"]["browseId"], "RDEM1");
        assert!(v.get("shelves").is_none());
    }

    #[test]
    fn a_page_without_rich_data_is_left_alone() {
        let v = with_page_data(json!({"title": "x"}), &PageData::default());
        assert_eq!(v, json!({"title": "x"}));
    }
}

#[cfg(test)]
mod session_error_tests {
    use super::*;

    #[tokio::test]
    async fn a_403_only_means_the_session_when_the_failing_call_used_it() {
        let refused = |code| RpError::HttpStatus(code, "x".into());
        for code in [401, 403] {
            // a call made with the session (library, history, liked): the session is gone
            assert_eq!(err_authed(refused(code)), AUTH_MSG, "{code}");
            // an anonymous call (search, charts, an album): refused, but the user stays signed in...
            assert_ne!(err(refused(code)), AUTH_MSG, "{code}");
            // ...also in a task that made an authenticated call before it and makes one after it (Home's personal
            // row: history with the session, then a radio without): each call is judged by its own query
            assert_eq!(err_authed(refused(code)), AUTH_MSG);
            assert_ne!(err(refused(code)), AUTH_MSG, "{code}: an earlier authed call must not colour this one");
            // `more()` maps with the pager's own flag: a search or public-playlist pager is anonymous
            assert_ne!(err_for(false, refused(code)), AUTH_MSG, "{code}");
            assert_eq!(err_for(true, refused(code)), AUTH_MSG, "{code}");
        }
        // only the exact session message makes `finish` flip the signed-in state
        assert!(err(refused(403)).starts_with("YouTube refused the request."));
        // other failures read the same either way
        assert_eq!(err(RpError::HttpStatus(429, "".into())), err_authed(RpError::HttpStatus(429, "".into())));
    }
}

#[cfg(test)]
mod liked_tests {
    use super::*;

    fn row(title: &str, id: &str, thumb: &str) -> Value {
        json!({"title": title, "browseId": id, "thumbnail": thumb})
    }

    fn liked_count(rows: &[Value]) -> usize {
        rows.iter().filter(|r| r["browseId"] == "LIKED").count()
    }

    #[test]
    fn a_synthetic_row_stands_in_when_youtubes_is_not_on_the_page() {
        let mut rows = vec![row("Mix", "PL1", "m.jpg")];
        liked_row_first(&mut rows);
        assert_eq!(rows[0]["title"], "Liked music");
        assert_eq!(liked_count(&rows), 1);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn youtubes_liked_playlist_replaces_ours_and_keeps_its_cover() {
        for lm in ["LM", "VLLM"] {
            let mut rows = vec![row("Mix", "PL1", "m.jpg"), row("Liked Music", lm, "purple.jpg")];
            liked_row_first(&mut rows);
            assert_eq!(liked_count(&rows), 1, "{lm}");
            assert_eq!(rows[0]["thumbnail"], "purple.jpg", "{lm}: the real cover is kept");
            assert_eq!(rows.len(), 2);
            assert!(rows.iter().all(|r| !is_youtube_liked(r)));
        }
    }

    #[test]
    fn later_pages_never_repeat_it() {
        let rows = vec![row("A", "PL2", ""), row("Liked Music", "LM", "p.jpg"), row("B", "PL3", "")];
        let kept = without_youtube_liked(rows);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|r| !is_youtube_liked(r)));
    }
}

#[cfg(test)]
mod home_feed_rule {
    use super::*;

    #[test]
    fn the_feed_is_shown_when_it_has_shelves_and_the_built_rows_otherwise() {
        assert!(feed_stands(&Ok(vec![json!({"kind": "song"})])));
        assert!(!feed_stands(&Ok(Vec::new())), "no shelves: show the built rows");
        assert!(!feed_stands(&Err("YouTube returned HTTP 500".into())), "a failed feed must not empty Home");
        assert!(feed_stands(&Err(AUTH_MSG.into())), "a dead session is reported, so the user gets signed out");
    }
}

/// Live checks against YouTube/LRCLIB: `cargo test -- --ignored --nocapture live_`
#[cfg(test)]
mod live {
    use super::*;

    /// Signed in: a mix from the home feed opens (as a radio; it has no playlist page). Skips if the feed has none.
    #[tokio::test]
    #[ignore]
    async fn live_home_feed_mix_opens() {
        set_logged(crate::auth::stored_cookie().is_some());
        let items = home_feed().await.expect("home feed");
        let Some(id) = items.iter().filter_map(|i| i["browseId"].as_str()).find(|id| id.starts_with("RDTM")) else {
            println!("no personal mix in this feed");
            return;
        };
        let mix = browse(format!("playlist:{id}")).await.expect("the mix opens");
        println!("mix: {} songs", count(&mix));
        assert!(count(&mix) > 5);
    }

    /// Signed in: the account's own home feed. Prints the shelves; says nothing about what is in them.
    #[tokio::test]
    #[ignore]
    async fn live_home_feed() {
        set_logged(crate::auth::stored_cookie().is_some());
        let items = home_feed().await.expect("home feed (needs a session YouTube still accepts)");
        let mut shelves: Vec<(String, usize)> = Vec::new();
        for i in &items {
            let section = i["section"].as_str().unwrap_or("");
            match shelves.last_mut() {
                Some((name, n)) if name == section => *n += 1,
                _ => shelves.push((section.to_owned(), 1)),
            }
        }
        let kinds: HashSet<&str> = items.iter().filter_map(|i| i["kind"].as_str()).collect();
        println!("home feed: {} items in {} shelves {:?}, kinds {:?}", items.len(), shelves.len(), shelves, kinds);
        assert!(shelves.len() >= 2, "expected at least two shelves");
        assert_eq!(crate::items::clean_all(items.clone()).len(), items.len(), "every item is playable or openable");
    }

    fn count(v: &Value) -> usize {
        v["items"].as_array().map(|a| a.len()).unwrap_or(0)
    }

    #[tokio::test]
    #[ignore]
    async fn live_home_search_pagination() {
        let h = browse("home".into()).await.expect("home");
        let sections: HashSet<&str> = h["items"].as_array().unwrap().iter().filter_map(|i| i["section"].as_str()).collect();
        println!("home: {} items, sections {:?}", count(&h), sections);
        assert!(count(&h) > 10);

        let s = search("daft punk".into(), "songs".into()).await.expect("search");
        let (n1, key) = (count(&s), s["pager"].as_i64().unwrap_or(0) as i32);
        println!("songs page 1: {n1}, more={}, pager={key}", s["more"]);
        assert!(s["more"].as_bool().unwrap() && key > 0);
        let m = more(key).await.expect("more");
        println!("songs page 2: +{}, more={}", count(&m), m["more"]);
        assert!(count(&m) > 0);

        let sg = suggest("daft p".into()).await.expect("suggest");
        println!("suggest: {sg}");
        assert!(!sg.as_array().unwrap().is_empty());

        let p = browse("playlist:PLMC9KNkIncKtPzgY-5rmhvj7fax8fdxoj".into()).await;
        println!("playlist: {:?}", p.as_ref().map(|v| (v["title"].clone(), v["subtitle"].clone(), count(v), v["more"].clone())));
    }

    #[tokio::test]
    #[ignore]
    async fn live_library_requires_login() {
        let e = browse("library:playlists".into()).await.unwrap_err();
        println!("library while signed out → {e}");
        assert_eq!(e, AUTH_MSG);
    }
}

#[cfg(test)]
mod signed_in_probe {
    use super::*;

    /// `cargo test -- --ignored --nocapture probe_signed_in`: with the stored session, which library calls work?
    /// Prints only counts / error text, never track data.
    #[tokio::test]
    #[ignore]
    async fn probe_signed_in() {
        println!("stored cookie present: {}", auth::stored_cookie().is_some());
        println!("check_cookie: {:?}", st().rp.user_auth_check_cookie().await.map_err(|e| e.to_string()));
        for target in ["library:playlists", "library:albums", "library:artists", "library:songs", "library:history", "playlist:LIKED"] {
            let t = std::time::Instant::now();
            let r = browse(target.to_owned()).await;
            let ms = t.elapsed().as_millis();
            match r {
                Ok(v) => println!("{target:<20} ok  {} items, more={}  [{ms} ms]", v["items"].as_array().map_or(0, |a| a.len()), v["more"]),
                Err(e) => println!("{target:<20} ERR {}  [{ms} ms]", e.lines().next().unwrap_or("")),
            }
        }
        // Home: each row separately, to see which one is slow
        for (name, fut) in [
            ("home:personal", Box::pin(home_personal()) as RowFuture),
            ("home:charts", Box::pin(home_charts())),
            ("home:new albums", Box::pin(home_new_albums())),
            ("home:new videos", Box::pin(home_new_videos())),
        ] {
            let t = std::time::Instant::now();
            let r = fut.await;
            println!(
                "{name:<20} {} [{} ms]",
                r.as_ref().map_or_else(|e| format!("ERR {}", e.lines().next().unwrap_or("")), |v| format!("ok  {} items", v.len())),
                t.elapsed().as_millis()
            );
        }
    }
}

#[cfg(test)]
mod thumbs_probe {
    use super::*;

    /// `cargo test -- --ignored --nocapture probe_thumbnails`: what cover URLs does YouTube give us?
    #[tokio::test]
    #[ignore]
    async fn probe_thumbnails() {
        let s = search("daft punk".into(), "".into()).await.unwrap();
        let mut seen = std::collections::HashSet::new();
        for i in s["items"].as_array().unwrap() {
            if seen.insert(i["kind"].as_str().unwrap().to_owned()) {
                println!("{:<9} {}", i["kind"].as_str().unwrap(), i["thumbnail"].as_str().unwrap());
            }
        }
        let h = browse("home".into()).await.unwrap();
        for i in h["items"].as_array().unwrap().iter().take(40) {
            if seen.insert(format!("home-{}", i["section"])) {
                println!("home {:<22} {}", i["section"].as_str().unwrap(), i["thumbnail"].as_str().unwrap());
            }
        }
    }
}
