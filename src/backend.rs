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
        #[qinvokable]
        fn is_liked(self: &Backend, video_id: &QString) -> bool;
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
            if e == AUTH_MSG {
                set_logged(false);
                b.as_mut().set_logged_in(false);
            }
            b.as_mut().done(id, QString::from(&e), QString::default())
        }
    }
}

fn spawn<F>(mut this: Pin<&mut qobject::Backend>, fut: F) -> i32
where
    F: Future<Output = Res> + Send + 'static,
{
    let id = next_id(this.as_mut());
    let th = this.qt_thread();
    st().rt.spawn(async move {
        let r = fut.await;
        let _ = th.queue(move |mut b| {
            if LIKED_DIRTY.swap(false, Ordering::Relaxed) {
                let rev = *b.liked_rev() + 1;
                b.as_mut().set_liked_rev(rev);
            }
            finish(b, id, r);
        });
    });
    id
}

/// RustyPipe error → message for the UI ("headline" or "headline\n\ndetail", see `errors.rs`).
/// An auth failure means the cookies are no longer valid.
fn err(e: RpError) -> String {
    crate::errors::friendly(&e, LOGGED.load(Ordering::Relaxed))
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
async fn liked_tracks(aq: &RustyPipeQuery) -> Result<Paginator<TrackItem>, RpError> {
    match with_retry(|| aq.music_liked_tracks()).await {
        Ok(p) => Ok(p.tracks),
        Err(first) => aq.music_saved_tracks().await.map_err(|_| first),
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
            Pager::Playlists(p, s) => p.items.iter().skip(from).map(|x| playlist(x, s)).collect(),
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

static PAGERS: Mutex<Option<HashMap<i32, (Pager, bool)>>> = Mutex::new(None);
static NEXT_PAGER: AtomicI32 = AtomicI32::new(0);

/// A page whose list can continue: remembers the pager and tells QML its id.
fn paged(title: &str, subtitle: &str, thumbnail: &str, pager: Pager, auth: bool) -> Value {
    let mut v = page(title, subtitle, thumbnail, pager.items(0));
    let more = pager.has_more();
    if more {
        let id = NEXT_PAGER.fetch_add(1, Ordering::Relaxed) + 1;
        PAGERS.lock().unwrap().get_or_insert_default().insert(id, (pager, auth));
        v["pager"] = json!(id);
    }
    v["more"] = json!(more);
    v
}

pub(crate) async fn more(key: i32) -> Res {
    let (mut pager, auth) = PAGERS.lock().unwrap().get_or_insert_default().remove(&key).ok_or("No more results")?;
    let from = pager.len();
    let q = if auth { authed() } else { st().rp.query() };
    if let Err(e) = pager.extend(&q).await {
        PAGERS.lock().unwrap().get_or_insert_default().insert(key, (pager, auth)); // allow a retry
        return Err(err(e));
    }
    let items = clean_all(pager.items(from));
    let more = pager.has_more();
    if more {
        PAGERS.lock().unwrap().get_or_insert_default().insert(key, (pager, auth));
    }
    Ok(json!({"items": items, "more": more, "pager": key}))
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
    let hist = with_retry(|| aq.music_history()).await.map_err(err)?;
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

/// All rows of Home at once (used by the live tests and as a non-progressive fallback).
#[cfg_attr(not(test), allow(dead_code))]
async fn home() -> Res {
    let (a, b, c, d) = tokio::join!(home_personal(), home_charts(), home_new_albums(), home_new_videos());
    let mut v = Vec::new();
    let mut first_err = None;
    for row in [a, b, c, d] {
        match row {
            Ok(items) => v.extend(items),
            Err(e) => first_err = first_err.or(Some(e)),
        }
    }
    if v.is_empty() {
        return Err(first_err.unwrap_or_else(|| "Nothing to show".into()));
    }
    Ok(page("", "", "", v))
}

/// Run the four rows concurrently, emitting each as it completes, then `done` when the last has finished.
/// Errors are only reported if *every* row failed – a missing row just stays missing.
fn spawn_home(mut this: Pin<&mut qobject::Backend>) -> i32 {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    let id = next_id(this.as_mut());
    let th = this.qt_thread();
    let rows: Vec<(i32, RowFuture)> = vec![
        (0, Box::pin(home_personal())),
        (1, Box::pin(home_charts())),
        (2, Box::pin(home_new_albums())),
        (3, Box::pin(home_new_videos())),
    ];
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
        "albums" => paged("", "", "", Pager::Albums(aq.music_saved_albums().await.map_err(err)?, "Albums".into()), true),
        "artists" => paged("", "", "", Pager::Artists(aq.music_saved_artists().await.map_err(err)?, "Artists".into()), true),
        "songs" => paged("", "", "", Pager::Tracks(with_retry(|| aq.music_saved_tracks()).await.map_err(err)?, "".into(), "".into()), true),
        "history" => {
            let h = with_retry(|| aq.music_history()).await.map_err(err)?;
            let v = h.items.iter().map(|h| track(&h.item, h.playback_date_txt.as_deref().unwrap_or("Recently played"), "")).collect();
            page("", "", "", v)
        }
        _ => {
            // First page only: the rest comes in as the list is scrolled. "Liked music" is a fixed first row.
            let p = with_retry(|| aq.music_saved_playlists()).await.map_err(err)?;
            let mut v = paged("", "", "", Pager::Playlists(p, "Playlists".into()), true);
            if let Some(items) = v["items"].as_array_mut() {
                items.insert(
                    0,
                    json!({"title": "Liked music", "subtitle": "Playlist", "artist": "", "kind": "playlist",
                    "videoId": "", "browseId": "LIKED", "thumbnail": "", "duration": "", "section": "Playlists",
                    "artistId": "", "albumId": ""}),
                );
            }
            v
        }
    })
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
            Ok(page(&a.name, &sub, &cover, v))
        }
        "playlist" => {
            // every spelling of "liked music" (our synthetic row, YouTube's `LM` / `VLLM`) takes the path that works
            let id = crate::models::items::canonical_playlist_id(id);
            if id == crate::models::items::LIKED_ID {
                // First page only; the rest loads on scroll. Whatever we see here is liked, so remember it for the hearts.
                let tracks = liked_tracks(&authed()).await.map_err(err)?;
                liked_set(|s| s.extend(tracks.items.iter().map(|t| t.id.clone())));
                LIKED_DIRTY.store(true, Ordering::Relaxed);
                return Ok(paged("Liked music", "", "", Pager::Tracks(tracks, "".into(), "".into()), true));
            }
            let p = with_retry(|| q.music_playlist(id.trim_start_matches("VL"))).await.map_err(err)?;
            let cover = thumb(&p.thumbnail);
            let by = p.channel.as_ref().map(|c| c.name.as_str()).unwrap_or("");
            let sub = match p.track_count {
                Some(n) => format!("{by} • {n} songs"),
                None => by.to_owned(),
            };
            Ok(paged(&p.name, sub.trim_start_matches(" • "), &cover, Pager::Tracks(p.tracks, "".into(), cover.clone()), false))
        }
        "artist" => {
            let a = q.music_artist(id, true).await.map_err(err)?;
            let mut v = Vec::new();
            v.extend(a.tracks.iter().map(|t| track(t, "Top songs", "")));
            v.extend(a.albums.iter().map(|x| album(x, "Albums & singles")));
            v.extend(a.playlists.iter().map(|p| playlist(p, "Playlists")));
            v.extend(a.similar_artists.iter().map(|x| artist(x, "Similar artists")));
            let sub = a.subscriber_count.map(|n| format!("{n} subscribers")).unwrap_or_default();
            Ok(page(&a.name, &sub, &thumb(&a.header_image), v))
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
    let (browser, header) = tokio::task::spawn_blocking(move || auth::import_from_browser(&id)).await.map_err(|e| e.to_string())??;
    set_cookie(Input::Header(header)).await?;
    Ok(json!({"browser": browser}))
}

async fn login_cookie(text: String) -> Res {
    set_cookie(auth::parse_input(&text)?).await?;
    Ok(json!({"browser": "pasted cookies"}))
}

/// Fill the "liked" set (for the hearts) in the background. It waits a few seconds so it never competes with
/// what the user is actually looking at, and only reads as far as `liked_limit` allows.
async fn load_liked() {
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let aq = authed();
    let Ok(mut tracks) = liked_tracks(&aq).await else { return };
    let _ = tracks.extend_limit(&aq, crate::config::current().library.liked_limit).await;
    // Merge instead of replacing: pages the user already browsed may hold songs beyond the limit.
    liked_set(|s| s.extend(tracks.items.iter().map(|t| t.id.clone())));
    LIKED_DIRTY.store(true, Ordering::Relaxed);
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
    let p = with_retry(|| aq.music_saved_playlists()).await.map_err(err)?;
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
            let lines: Vec<Value> = lines.into_iter().map(|(t, l)| json!([t, l])).collect();
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
    fn more(self: Pin<&mut Self>, pager: i32) -> i32 {
        spawn(self, more(pager))
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
        let th = self.qt_thread();
        // Optimistic: a stored cookie means "signed in" right now, so the Library and Home don't wait on a network
        // round trip. The check below corrects this if YouTube no longer accepts the session.
        if auth::stored_cookie().is_some() {
            set_logged(true);
            self.as_mut().set_logged_in(true);
        }
        st().rt.spawn(async move {
            let ok = match st().rp.user_auth_check_cookie().await {
                Ok(()) => true,
                Err(RpError::Auth(_)) => false,
                Err(_) => auth::stored_cookie().is_some(), // offline: trust the stored cookie
            };
            set_logged(ok);
            // Publish the sign-in state FIRST; the liked-songs preload below is slow and must never gate it.
            let th_liked = th.clone();
            let _ = th.queue(move |mut b| b.as_mut().set_logged_in(ok));
            if ok {
                load_liked().await;
                let _ = th_liked.queue(|mut b| {
                    if LIKED_DIRTY.swap(false, Ordering::Relaxed) {
                        let rev = *b.liked_rev() + 1;
                        b.as_mut().set_liked_rev(rev);
                    }
                });
            }
        });
    }
    fn is_liked(&self, video_id: &QString) -> bool {
        liked_set(|s| s.contains(&video_id.to_string()))
    }
    fn set_liked(mut self: Pin<&mut Self>, video_id: &QString, liked: bool) -> i32 {
        let vid = video_id.to_string();
        let th = self.qt_thread();
        // Optimistic: the heart flips immediately; if YouTube refuses, it flips back and `like_failed` says why.
        liked_set(|s| if liked { s.insert(vid.clone()) } else { s.remove(&vid) });
        let rev = *self.liked_rev() + 1;
        self.as_mut().set_liked_rev(rev);
        spawn(self, async move {
            match ytapi::rate(&vid, liked).await {
                Ok(()) => Ok(json!({"liked": liked})),
                Err(message) => {
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

/// Live checks against YouTube/LRCLIB: `cargo test -- --ignored --nocapture live_`
#[cfg(test)]
mod live {
    use super::*;

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
