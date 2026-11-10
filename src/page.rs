// SPDX-License-Identifier: GPL-3.0-only
//! `PageController`: one object per list page. It loads a [`PageData`] through the `pages` seam, owns the paging
//! rule, the page cache, the retry policy and the "play / feed the queue" actions, and is itself the
//! `QAbstractListModel` the page's `ListView` shows (rows carry their section so the view can group them).
//! QML does no JSON parsing, no paging arithmetic and no caching.
//!
//! Everything with a rule lives in `models::{items,pager}` and `errors` (plain Rust, tested); this file is the
//! thin Qt skin around them plus the async glue.

use crate::errors::{self, AppError, ErrorKind};
use crate::models::items::{self, Assembly, Row};
use crate::models::pager::{self, CachedPage, Lookup, PageCache, PagerState};
use crate::models::Change;
use crate::pagedata::{Hero, Item, ItemKind, PageData, Section};
use crate::pages::{self, HomeRow, Request};
use crate::state::st;
use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{
    QByteArray, QHash, QHashPair_i32_QByteArray, QJsonArray, QJsonObject, QJsonValue, QModelIndex, QString, QStringList, QVariant,
};
use serde_json::Value;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

pub type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Where pages come from. The app uses [`pages`]; the developer harness swaps in canned data.
pub trait PageSource: Send + Sync {
    fn load(&self, req: Request) -> BoxFut<Result<PageData, AppError>>;
    fn more(&self, pager: u32) -> BoxFut<Result<PageData, AppError>>;
    fn home_rows(&self) -> Vec<HomeRow>;
    fn signed_in(&self) -> bool {
        crate::backend::is_logged_in()
    }
}

struct Live;
impl PageSource for Live {
    fn load(&self, req: Request) -> BoxFut<Result<PageData, AppError>> {
        Box::pin(async move { pages::load(&req).await })
    }
    fn more(&self, pager: u32) -> BoxFut<Result<PageData, AppError>> {
        Box::pin(async move { pages::more(pager).await })
    }
    fn home_rows(&self) -> Vec<HomeRow> {
        pages::home_loaders()
    }
}

static SOURCE: RwLock<Option<Arc<dyn PageSource>>> = RwLock::new(None);

#[cfg_attr(not(feature = "harness"), allow(dead_code))] // the harness swaps in canned pages
pub fn set_source(s: Arc<dyn PageSource>) {
    *SOURCE.write().unwrap() = Some(s);
}

fn source() -> Arc<dyn PageSource> {
    SOURCE.read().unwrap().clone().unwrap_or_else(|| Arc::new(Live))
}

static CACHE: Mutex<Option<PageCache>> = Mutex::new(None);

fn with_cache<R>(f: impl FnOnce(&mut PageCache) -> R) -> R {
    f(CACHE.lock().unwrap().get_or_insert_default())
}

fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn cache_ttl_ms() -> u64 {
    crate::config::current().library.page_cache_secs.saturating_mul(1000) // read per use: follows Settings
}

// ---- roles -------------------------------------------------------------------------------------------------

const ROLE_BASE: i32 = 0x100; // Qt::UserRole
const ROLE_NAMES: [&str; 16] = [
    "title",
    "subtitle",
    "artist",
    "kind",
    "videoId",
    "browseId",
    "thumbnail",
    "duration",
    "artistId",
    "albumId",
    "number",
    "section",
    "sectionId",
    "sectionStyle",
    "placeholder",
    "playable",
];
// what a row replaced in place may change (the Liked-music swap): title, subtitle, browseId, thumbnail, placeholder
const UPDATE_ROLES: [i32; 5] = [ROLE_BASE, ROLE_BASE + 1, ROLE_BASE + 5, ROLE_BASE + 6, ROLE_BASE + 14];

fn kind_name(k: ItemKind) -> &'static str {
    match k {
        ItemKind::Song => "song",
        ItemKind::Album => "album",
        ItemKind::Artist => "artist",
        ItemKind::Playlist => "playlist",
    }
}

fn qlist<I: IntoIterator<Item = String>>(it: I) -> QStringList {
    it.into_iter().map(|s| QString::from(&s)).collect()
}

/// A song as the player stores it (the JSON the queue and the saved session use).
fn song_value(item: &Item) -> Value {
    serde_json::to_value(item).unwrap_or(Value::Null)
}

/// The page as the rich view (`RichPage.qml`) wants it: `{hero, sections: [{id, title, subtitle, style, items}], hasMore}`.
/// Consecutive rows of one section form one section again, so a page that grew by paging is still one list.
fn page_value(hero: Option<&Hero>, rows: &[Row], has_more: bool) -> Value {
    let mut sections: Vec<Section> = Vec::new();
    for r in rows {
        match sections.last_mut() {
            // (the titles too: ids are slugs, and two differently titled neighbours can share one - e.g. "")
            Some(s) if s.id == r.section_id && s.style == r.style && s.title == r.section && s.subtitle == r.section_subtitle => {
                s.items.push(r.item.clone())
            }
            _ => sections.push(Section {
                id: r.section_id.clone(),
                title: r.section.clone(),
                subtitle: r.section_subtitle.clone(),
                style: r.style,
                items: vec![r.item.clone()],
            }),
        }
    }
    serde_json::json!({ "hero": hero, "sections": sections, "hasMore": has_more })
}

/// A JSON value as a `QJsonValue`, which QML sees as a plain JS object / array (no text parsing on either side).
fn to_qjson(v: &Value) -> QJsonValue {
    match v {
        Value::Null => QJsonValue::default(),
        Value::Bool(b) => QJsonValue::from(*b),
        Value::Number(n) => n.as_i64().map_or_else(|| QJsonValue::from(n.as_f64().unwrap_or(0.0)), QJsonValue::from),
        Value::String(s) => QJsonValue::from(&QString::from(s)),
        Value::Array(a) => {
            let mut arr = QJsonArray::default();
            for x in a {
                arr.append(&to_qjson(x));
            }
            QJsonValue::from(&arr)
        }
        Value::Object(o) => {
            let mut obj = QJsonObject::default();
            for (k, x) in o {
                obj.insert(&QString::from(k), &to_qjson(x));
            }
            QJsonValue::from(&obj)
        }
    }
}

/// The songs of a loaded page (the reserved `hero-*` sections carry header data, never songs).
fn page_songs(page: &PageData) -> Vec<Item> {
    page.sections
        .iter()
        .filter(|s| !s.id.starts_with("hero-"))
        .flat_map(|s| s.items.iter())
        .filter(|i| i.kind == ItemKind::Song && i.is_actionable())
        .cloned()
        .collect()
}

/// Which queue a feed appends the rest of a long list to: the queue's epoch (`Player::queue_epoch`) as it is right
/// after the "play" that started the feed. `None` before that is known, and again once that queue has been replaced
/// (so a feed never appends to a queue that is not the one it was started for).
type FedQueue = Arc<Mutex<Option<i64>>>;

/// Note which queue there is once everything already asked of the player has happened (requests reach the player in
/// order, so this comes after the `play_many` that precedes it and before any `append_fed` that follows).
fn mark_queue() -> FedQueue {
    let queue = FedQueue::default();
    let slot = queue.clone();
    crate::player::with_player(move |p| *slot.lock().unwrap() = Some(p.queue_epoch()));
    queue
}

/// Append `songs` to the marked queue if it is still the queue; if not, the mark is cleared. Answers through `done`.
fn append_fed(queue: &FedQueue, songs: Vec<Value>, done: Option<tokio::sync::oneshot::Sender<bool>>) {
    let queue = queue.clone();
    crate::player::with_player(move |p| {
        let mut slot = queue.lock().unwrap();
        let alive = slot.is_some_and(|epoch| p.enqueue_fed(epoch, songs));
        if !alive {
            *slot = None;
        }
        if let Some(done) = done {
            let _ = done.send(alive);
        }
    });
}

// ---- the QObject ---------------------------------------------------------------------------------------------

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        // declared once, in queuemodel.rs: a second declaration would define its up/down-cast symbols twice
        type QAbstractListModel = crate::queuemodel::qobject::QAbstractListModel;

        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qjsonvalue.h");
        type QJsonValue = cxx_qt_lib::QJsonValue;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qvector.h");
        type QVector_i32 = cxx_qt_lib::QVector<i32>;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        #[qml_element]
        // "loading" | "ready" | "error" | "offline"
        #[qproperty(QString, state)]
        // the page being shown
        #[qproperty(QString, target)]
        #[qproperty(QString, query)]
        #[qproperty(QString, filter)]
        #[qproperty(QStringList, chip_labels)]
        #[qproperty(QStringList, chip_values)]
        #[qproperty(bool, collection)]
        #[qproperty(bool, searchable)]
        #[qproperty(bool, needs_login)]
        #[qproperty(i32, count)]
        #[qproperty(i32, song_count)]
        #[qproperty(bool, has_more)]
        #[qproperty(bool, loading_more)]
        // true while parts of a progressive page are still on their way
        #[qproperty(bool, refreshing)]
        // the error (when state is "error" / "offline")
        #[qproperty(QString, error_kind)]
        #[qproperty(QString, error_headline)]
        #[qproperty(QString, error_hint)]
        #[qproperty(QString, error_detail)]
        #[qproperty(QString, error_id)]
        #[qproperty(QString, error_retry)] // "never" | "manual" | "auto"
        #[qproperty(i32, error_retry_ms)] // > 0: retry by itself after this long
        #[qproperty(QString, error_diagnostics)]
        // the header of album / artist / playlist pages
        #[qproperty(bool, has_hero)]
        #[qproperty(QString, hero_title)]
        #[qproperty(QString, hero_subtitle)]
        #[qproperty(QString, hero_cover)]
        #[qproperty(QString, hero_description)]
        #[qproperty(QString, hero_wikipedia_url)]
        #[qproperty(QStringList, hero_stats)]
        #[qproperty(QStringList, hero_actions)]
        #[qproperty(QStringList, suggestions)]
        // the whole page for the rich view (hero + sections + hasMore); see `page_value`
        #[qproperty(QJsonValue, page_data)]
        type PageController = super::PageControllerRust;

        /// Load a page. `target` is `home`, `explore`, `album:ID`, `playlist:ID`, `artist:ID` or `library`;
        /// a non-empty `query` searches instead. `filter` is a chip value. `force` bypasses the page cache.
        #[qinvokable]
        fn load(self: Pin<&mut PageController>, target: &QString, query: &QString, filter: &QString, force: bool);
        /// Load the signed-in user's playlists for the "Add to playlist" picker (Liked music left out).
        #[qinvokable]
        fn load_picker(self: Pin<&mut PageController>);
        /// Choose a chip (search type / library tab): reloads.
        #[qinvokable]
        fn select_filter(self: Pin<&mut PageController>, value: &QString);
        /// The user pressed Retry (or reload): forget the automatic-retry count and load again, bypassing the cache.
        #[qinvokable]
        fn retry(self: Pin<&mut PageController>);
        /// The countdown of an automatic retry ran out.
        #[qinvokable]
        fn auto_retry(self: Pin<&mut PageController>);
        /// Call when the signed-in state changed: pages of that kind reload.
        #[qinvokable]
        fn login_changed(self: Pin<&mut PageController>);
        /// The list was scrolled / resized: `bottom_index` is the row at the bottom edge (-1 if none).
        /// Starts the next page when it is time; returns whether it did.
        #[qinvokable]
        fn near_end(self: Pin<&mut PageController>, bottom_index: i32, count: i32) -> bool;
        /// Click on a song row: plays it (on a collection page, the whole list from there).
        #[qinvokable]
        fn play_item(self: Pin<&mut PageController>, i: i32);
        /// Like `play_item`, for the rich view, which knows songs by video id rather than row.
        #[qinvokable]
        fn play_song(self: Pin<&mut PageController>, video_id: &QString);
        #[qinvokable]
        fn play_next_song(self: Pin<&mut PageController>, video_id: &QString);
        #[qinvokable]
        fn enqueue_song(self: Pin<&mut PageController>, video_id: &QString);
        /// Load the album / playlist (`kind` + `browse_id`) and play it (hover button of a card, the artist radio).
        #[qinvokable]
        fn play_collection(self: Pin<&mut PageController>, kind: &QString, browse_id: &QString, shuffle: bool);
        /// The page's songs as a JSON array, ready to hand to `Downloads.downloadAll`.
        #[qinvokable]
        fn songs_json(self: &PageController) -> QString;
        #[qinvokable]
        fn play_next(self: Pin<&mut PageController>, i: i32);
        #[qinvokable]
        fn enqueue(self: Pin<&mut PageController>, i: i32);
        /// Play every song of the page (shuffled from a random one if asked); keeps loading further pages into the queue.
        #[qinvokable]
        fn play_all(self: Pin<&mut PageController>, shuffle: bool);
        #[qinvokable]
        fn add_all_to_queue(self: Pin<&mut PageController>);
        /// Keep fetching pages and appending their songs to the queue.
        #[qinvokable]
        fn start_feed_queue(self: Pin<&mut PageController>);
        /// Ask for search suggestions; they arrive in `suggestions`.
        #[qinvokable]
        fn suggest(self: Pin<&mut PageController>, text: &QString);

        #[inherit]
        #[cxx_name = "beginInsertRows"]
        unsafe fn begin_insert_rows(self: Pin<&mut PageController>, parent: &QModelIndex, first: i32, last: i32);
        #[inherit]
        #[cxx_name = "endInsertRows"]
        unsafe fn end_insert_rows(self: Pin<&mut PageController>);
        #[inherit]
        #[cxx_name = "beginRemoveRows"]
        unsafe fn begin_remove_rows(self: Pin<&mut PageController>, parent: &QModelIndex, first: i32, last: i32);
        #[inherit]
        #[cxx_name = "endRemoveRows"]
        unsafe fn end_remove_rows(self: Pin<&mut PageController>);
        #[inherit]
        #[cxx_name = "beginMoveRows"]
        unsafe fn begin_move_rows(
            self: Pin<&mut PageController>,
            source_parent: &QModelIndex,
            source_first: i32,
            source_last: i32,
            dest_parent: &QModelIndex,
            dest_child: i32,
        ) -> bool;
        #[inherit]
        #[cxx_name = "endMoveRows"]
        unsafe fn end_move_rows(self: Pin<&mut PageController>);
        #[inherit]
        #[cxx_name = "beginResetModel"]
        unsafe fn begin_reset_model(self: Pin<&mut PageController>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        unsafe fn end_reset_model(self: Pin<&mut PageController>);
        #[inherit]
        #[cxx_name = "dataChanged"]
        #[qsignal]
        fn data_changed(self: Pin<&mut PageController>, top_left: &QModelIndex, bottom_right: &QModelIndex, roles: &QVector_i32);
        #[inherit]
        fn index(self: &PageController, row: i32, column: i32, parent: &QModelIndex) -> QModelIndex;

        #[qinvokable]
        #[cxx_override]
        fn data(self: &PageController, index: &QModelIndex, role: i32) -> QVariant;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &PageController) -> QHash_i32_QByteArray;
        #[qinvokable]
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &PageController, parent: &QModelIndex) -> i32;
    }

    impl cxx_qt::Threading for PageController {}
}

#[derive(Default)]
pub struct PageControllerRust {
    state: QString,
    target: QString,
    query: QString,
    filter: QString,
    chip_labels: QStringList,
    chip_values: QStringList,
    collection: bool,
    searchable: bool,
    needs_login: bool,
    count: i32,
    song_count: i32,
    has_more: bool,
    loading_more: bool,
    refreshing: bool,
    error_kind: QString,
    error_headline: QString,
    error_hint: QString,
    error_detail: QString,
    error_id: QString,
    error_retry: QString,
    error_retry_ms: i32,
    error_diagnostics: QString,
    has_hero: bool,
    hero_title: QString,
    hero_subtitle: QString,
    hero_cover: QString,
    hero_description: QString,
    hero_wikipedia_url: QString,
    hero_stats: QStringList,
    hero_actions: QStringList,
    suggestions: QStringList,
    page_data: QJsonValue,

    // not exposed
    rows: Vec<Row>,
    assembly: Assembly,
    hero: Option<Hero>,
    pager: PagerState,
    /// Bumped by every load; answers from an older one are dropped.
    generation: u64,
    picker: bool,
    auto_attempts: u32,
    last_error: Option<AppError>,
    home_pending: usize,
    home_any: bool,
    home_error: Option<AppError>,
    suggest_gen: u64,
    /// The queue the feed of this page appends to (see `FedQueue`).
    fed_queue: FedQueue,
}

impl qobject::PageController {
    fn st_mut(self: Pin<&mut Self>) -> &mut PageControllerRust {
        self.rust_mut().get_mut()
    }

    fn apply(mut self: Pin<&mut Self>, change: Change<Row>) {
        crate::apply_to_model!(self, change, &mut self.as_mut().rust_mut().get_mut().rows, UPDATE_ROLES);
        self.sync_counts();
    }

    fn sync_counts(mut self: Pin<&mut Self>) {
        let n = self.rows.len() as i32;
        let songs = self.assembly.songs().len() as i32;
        let (more, loading_more) = (self.pager.has_more(), self.pager.loading_more());
        self.as_mut().set_count(n);
        self.as_mut().set_song_count(songs);
        self.as_mut().set_has_more(more);
        self.as_mut().set_loading_more(loading_more);
        let page = to_qjson(&page_value(self.hero.as_ref(), &self.rows, more));
        self.as_mut().set_page_data(page);
    }

    fn set_state_name(mut self: Pin<&mut Self>, name: &str) {
        if self.state().to_string() != name {
            self.as_mut().set_state(QString::from(name));
        }
    }

    fn set_hero_data(mut self: Pin<&mut Self>, hero: Option<Hero>) {
        let h = hero.clone().unwrap_or_default();
        self.as_mut().st_mut().hero = hero.clone();
        self.as_mut().set_has_hero(hero.is_some());
        self.as_mut().set_hero_title(QString::from(&h.title));
        self.as_mut().set_hero_subtitle(QString::from(&h.subtitle));
        self.as_mut().set_hero_cover(QString::from(&h.cover));
        self.as_mut().set_hero_description(QString::from(&h.description));
        let wiki = if crate::security::is_safe_external_link(&h.wikipedia_url) { h.wikipedia_url.clone() } else { String::new() };
        self.as_mut().set_hero_wikipedia_url(QString::from(&wiki));
        self.as_mut().set_hero_stats(qlist(h.stats.iter().map(|s| s.text.clone())));
        let actions = h.actions.iter().filter_map(|a| serde_json::to_value(a).ok()).filter_map(|v| v.as_str().map(str::to_owned));
        self.as_mut().set_hero_actions(qlist(actions));
    }

    fn clear_error(mut self: Pin<&mut Self>) {
        self.as_mut().st_mut().last_error = None;
        self.as_mut().set_error_kind(QString::default());
        self.as_mut().set_error_headline(QString::default());
        self.as_mut().set_error_hint(QString::default());
        self.as_mut().set_error_detail(QString::default());
        self.as_mut().set_error_id(QString::default());
        self.as_mut().set_error_retry(QString::default());
        self.as_mut().set_error_retry_ms(0);
        self.as_mut().set_error_diagnostics(QString::default());
    }

    /// Show a failure in place of the (empty) page.
    fn show_error(mut self: Pin<&mut Self>, e: AppError) {
        let e = e.for_connectivity(errors::is_online());
        let attempts = self.auto_attempts;
        let delay = e.retry.next_auto_delay_ms(attempts);
        self.as_mut().set_error_kind(QString::from(e.kind.name()));
        self.as_mut().set_error_headline(QString::from(&e.headline));
        self.as_mut().set_error_hint(QString::from(&e.hint));
        self.as_mut().set_error_detail(QString::from(&e.detail));
        self.as_mut().set_error_id(QString::from(&e.id));
        // an exhausted automatic policy looks like a manual one: just the button
        let retry = if e.retry.offers_button() && delay.is_none() { "manual" } else { e.retry.name() };
        self.as_mut().set_error_retry(QString::from(retry));
        self.as_mut().set_error_retry_ms(delay.map_or(0, |ms| ms.min(i32::MAX as u64) as i32));
        self.as_mut().set_error_diagnostics(QString::from(&e.diagnostics_redacted()));
        let name = if e.kind == ErrorKind::Offline { "offline" } else { "error" };
        self.as_mut().st_mut().last_error = Some(e);
        self.as_mut().st_mut().pager.fail();
        self.as_mut().set_state_name(name);
        self.as_mut().set_refreshing(false);
        self.sync_counts();
    }

    fn start(mut self: Pin<&mut Self>, force: bool) {
        let (target, query, filter) = (self.target().to_string(), self.query().to_string(), self.filter().to_string());
        let eff = pager::effective_target(&target, &filter);
        let r = self.as_mut().st_mut();
        r.generation += 1;
        let generation = r.generation;
        r.pager.start();
        r.home_pending = 0;
        r.home_any = false;
        r.home_error = None;
        self.as_mut().clear_error();
        self.as_mut().set_refreshing(false);

        // pages that need an account don't even try without one
        if pager::needs_login(&target, &query) && !source().signed_in() {
            let e = AppError::new(ErrorKind::Auth, crate::auth::AUTH_MSG, "");
            let clear = self.as_mut().st_mut().assembly.clear();
            self.as_mut().apply(clear);
            return self.show_error(e);
        }

        // cache: fresh = done, stale = show now and refresh quietly behind it
        let key = PageCache::key(&eff, &query, "");
        let lookup = key
            .as_deref()
            .map_or(Lookup::Miss, |k| with_cache(|c| c.get(k, now_ms(), cache_ttl_ms(), crate::backend::login_epoch(), force)));
        let mut quiet = false;
        match lookup {
            Lookup::Fresh(page) => {
                self.as_mut().show_cached(page);
                self.as_mut().st_mut().pager.ready(None, false);
                return;
            }
            Lookup::Stale(page) => {
                quiet = true;
                self.as_mut().show_cached(page);
            }
            Lookup::Miss => {
                let clear = self.as_mut().st_mut().assembly.clear();
                self.as_mut().set_hero_data(None);
                self.as_mut().apply(clear);
            }
        }
        if !quiet {
            self.as_mut().set_state_name("loading");
        }

        let th = self.qt_thread();
        if pager::is_progressive(&target, &query) {
            let rows = source().home_rows();
            self.as_mut().st_mut().home_pending = rows.len();
            self.as_mut().set_refreshing(true);
            if rows.is_empty() {
                self.finish_home(generation);
                return;
            }
            for HomeRow { slot, load } in rows {
                let th = th.clone();
                st().rt.spawn(async move {
                    let r = load.await;
                    let _ = th.queue(move |mut c| c.as_mut().on_slot(generation, slot, r));
                });
            }
        } else {
            let req = Request { target: eff, query, filter };
            let fut = source().load(req);
            st().rt.spawn(async move {
                let r = fut.await;
                let _ = th.queue(move |mut c| c.as_mut().on_loaded(generation, r));
            });
        }
    }

    fn show_cached(mut self: Pin<&mut Self>, page: CachedPage) {
        self.as_mut().set_hero_data(page.hero);
        self.as_mut().st_mut().assembly = Assembly::from_slots(page.slots);
        let flat = self.assembly.flat();
        self.as_mut().apply(Change::Reset(flat));
        self.as_mut().set_state_name("ready");
    }

    /// Rows for a batch of sections, with the page-specific clean-up (Liked music de-duplication, picker filter).
    fn rows_for(&self, sections: &[Section], existing: &[Row]) -> (Vec<Row>, Option<(usize, Row)>) {
        let mut rows = items::rows_from_sections(sections);
        let mut swap = None;
        if self.picker {
            rows.retain(|r| !items::is_liked_id(&r.item.browse_id));
        } else if pager::effective_target(&self.target.to_string(), &self.filter.to_string()) == "library:playlists" {
            swap = items::dedupe_liked(existing, &mut rows);
        }
        (rows, swap)
    }

    fn on_loaded(mut self: Pin<&mut Self>, generation: u64, result: Result<PageData, AppError>) {
        if self.generation != generation {
            return;
        }
        match result {
            Err(e) => {
                // keep what is on screen (a cached copy); only replace the page with the error when it is empty
                if self.rows.is_empty() {
                    self.show_error(e);
                } else {
                    errors::toast(&e);
                    self.as_mut().st_mut().pager.ready(None, false);
                    self.set_state_name("ready");
                }
            }
            Ok(page) => {
                let (rows, _) = self.rows_for(&page.sections, &[]);
                let mut slots = std::collections::BTreeMap::new();
                slots.insert(0, rows);
                self.as_mut().st_mut().assembly = Assembly::from_slots(slots);
                let flat = self.assembly.flat();
                self.as_mut().set_hero_data(page.hero.clone());
                self.as_mut().st_mut().pager.ready(page.pager, page.has_more);
                self.as_mut().apply(Change::Reset(flat));
                self.as_mut().set_state_name("ready");
                self.as_mut().store_in_cache(page.has_more);
            }
        }
    }

    fn store_in_cache(&self, has_more: bool) {
        if self.picker {
            return; // the picker's list has Liked music filtered out: it is not the Library page
        }
        let (target, query, filter) = (self.target.to_string(), self.query.to_string(), self.filter.to_string());
        let eff = pager::effective_target(&target, &filter);
        if let Some(key) = PageCache::key(&eff, &query, "") {
            let page = CachedPage { hero: self.hero.clone(), slots: self.assembly.slots().clone() };
            with_cache(|c| c.put(&key, page, has_more, now_ms(), cache_ttl_ms(), crate::backend::login_epoch()));
        }
    }

    fn on_slot(mut self: Pin<&mut Self>, generation: u64, slot: i32, result: Result<Vec<Section>, AppError>) {
        if self.generation != generation {
            return;
        }
        let r = self.as_mut().st_mut();
        r.home_pending = r.home_pending.saturating_sub(1);
        match result {
            Ok(sections) if !sections.is_empty() => {
                let rows = items::rows_from_sections(&sections);
                self.as_mut().st_mut().home_any = true;
                let change = self.as_mut().st_mut().assembly.set_slot(slot, rows);
                self.as_mut().apply(change);
                self.as_mut().st_mut().pager.ready(None, false);
                self.as_mut().set_state_name("ready");
            }
            Ok(_) => {}
            Err(e) => {
                self.as_mut().st_mut().home_error.get_or_insert(e);
            }
        }
        if self.home_pending == 0 {
            self.finish_home(generation);
        }
    }

    fn finish_home(mut self: Pin<&mut Self>, _generation: u64) {
        self.as_mut().set_refreshing(false);
        if self.home_any {
            self.as_mut().st_mut().pager.ready(None, false);
            self.as_mut().set_state_name("ready");
            self.as_mut().store_in_cache(false);
        } else if let Some(e) = self.as_mut().st_mut().home_error.take() {
            if self.rows.is_empty() {
                self.show_error(e);
            }
        } else {
            self.as_mut().st_mut().pager.ready(None, false);
            self.set_state_name("ready"); // nothing to show is an empty page, not an error
        }
    }

    fn fetch_more(mut self: Pin<&mut Self>, pager_id: u32) {
        let generation = self.generation;
        self.as_mut().sync_counts();
        let th = self.qt_thread();
        let fut = source().more(pager_id);
        st().rt.spawn(async move {
            let r = fut.await;
            let _ = th.queue(move |mut c| c.as_mut().on_more(generation, r));
        });
    }

    fn on_more(mut self: Pin<&mut Self>, generation: u64, result: Result<PageData, AppError>) {
        if self.generation != generation {
            return;
        }
        match result {
            Err(e) => {
                self.as_mut().st_mut().pager.more_failed();
                self.as_mut().sync_counts();
                errors::toast(&e); // keep what we have, tell the user once
            }
            Ok(page) => {
                let existing = self.rows.clone();
                let (rows, swap) = self.rows_for(&page.sections, &existing);
                if let Some((at, row)) = swap {
                    if let Some(change) = self.as_mut().st_mut().assembly.update(at, row) {
                        self.as_mut().apply(change);
                    }
                }
                // The queue this page was feeding has been replaced (or there is no player): the feed is over. (The mark
                // is always set by now: `mark_queue` was asked of the player before this page was even requested.)
                if self.pager.feed_queue() && self.fed_queue.lock().unwrap().is_none() {
                    self.as_mut().st_mut().pager.set_feed_queue(false);
                }
                let feed: Vec<Value> = if self.pager.feed_queue() {
                    rows.iter().filter(|r| r.is_song()).map(|r| song_value(&r.item)).collect()
                } else {
                    Vec::new()
                };
                if !rows.is_empty() {
                    let change = self.as_mut().st_mut().assembly.append(rows);
                    self.as_mut().apply(change);
                }
                self.as_mut().st_mut().pager.more_done(page.pager, page.has_more);
                self.as_mut().sync_counts();
                if !feed.is_empty() {
                    // only into the queue the feed was started for
                    append_fed(&self.fed_queue, feed, None);
                }
                // feeding the queue ignores the scroll position, but still one request at a time
                self.as_mut().continue_feed();
            }
        }
    }

    fn continue_feed(mut self: Pin<&mut Self>) {
        let count = self.rows.len();
        if let Some(id) = self.as_mut().st_mut().pager.begin_more(0, count) {
            self.fetch_more(id);
        }
    }

    fn song_by_id(&self, video_id: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.is_song() && r.item.video_id == video_id)
    }

    fn song_at(&self, i: i32) -> Option<&Row> {
        usize::try_from(i).ok().and_then(|i| self.rows.get(i)).filter(|r| r.is_song())
    }
}

// ---- invokables ---------------------------------------------------------------------------------------------

impl qobject::PageController {
    fn load(mut self: Pin<&mut Self>, target: &QString, query: &QString, filter: &QString, force: bool) {
        let (t, q) = (target.to_string(), query.to_string());
        let f = if filter.is_empty() { pager::default_filter(&t, &q).to_owned() } else { filter.to_string() };
        self.as_mut().st_mut().auto_attempts = 0;
        self.as_mut().set_target(target.clone());
        self.as_mut().set_query(query.clone());
        self.as_mut().set_filter(QString::from(&f));
        let chips = pager::chips(&t, &q);
        self.as_mut().set_chip_labels(qlist(chips.iter().map(|c| c.label.to_owned())));
        self.as_mut().set_chip_values(qlist(chips.iter().map(|c| c.value.to_owned())));
        self.as_mut().set_collection(pager::is_collection(&t, &q));
        self.as_mut().set_searchable(pager::is_searchable(&t, &q));
        self.as_mut().set_needs_login(pager::needs_login(&t, &q));
        self.start(force);
    }

    fn load_picker(mut self: Pin<&mut Self>) {
        self.as_mut().st_mut().picker = true;
        let (t, q, f) = (QString::from("library"), QString::default(), QString::from("playlists"));
        self.load(&t, &q, &f, true);
    }

    fn select_filter(mut self: Pin<&mut Self>, value: &QString) {
        if self.filter() == value {
            return;
        }
        self.as_mut().set_filter(value.clone());
        self.as_mut().st_mut().auto_attempts = 0;
        self.start(false);
    }

    fn retry(mut self: Pin<&mut Self>) {
        self.as_mut().st_mut().auto_attempts = 0;
        self.start(true);
    }

    fn auto_retry(mut self: Pin<&mut Self>) {
        self.as_mut().st_mut().auto_attempts += 1;
        self.start(true);
    }

    fn login_changed(mut self: Pin<&mut Self>) {
        let (t, q) = (self.target().to_string(), self.query().to_string());
        if pager::reloads_on_login(&t, &q) {
            self.as_mut().st_mut().auto_attempts = 0;
            self.start(true);
        }
    }

    fn near_end(mut self: Pin<&mut Self>, bottom_index: i32, count: i32) -> bool {
        let count = usize::try_from(count).unwrap_or(0);
        match self.as_mut().st_mut().pager.begin_more(bottom_index, count) {
            Some(id) => {
                self.fetch_more(id);
                true
            }
            None => false,
        }
    }

    fn play_item(self: Pin<&mut Self>, i: i32) {
        let Some(row) = self.song_at(i) else { return };
        // the clicked row itself, so the second copy of a song that is in the list twice starts from there
        let nth = self.rows.iter().take(i.max(0) as usize).filter(|r| r.is_song()).count();
        self.play_row_item(&row.item, Some(nth));
    }

    /// Play a song of this page: on a collection page the whole list from there, otherwise just that song.
    /// `nth` is its position among the page's songs when the caller knows it.
    fn play_row_item(&self, item: &Item, nth: Option<usize>) {
        if self.collection {
            let songs = self.assembly.songs();
            let start = nth
                .filter(|n| songs.get(*n).is_some_and(|s| s.video_id == item.video_id))
                .or_else(|| songs.iter().position(|s| s.video_id == item.video_id))
                .unwrap_or(0);
            let values: Vec<Value> = songs.iter().map(song_value).collect();
            crate::player::with_player(move |p| p.play_many(values, start));
        } else {
            let v = song_value(item);
            crate::player::with_player(move |p| p.play_one(v));
        }
    }

    fn play_song(self: Pin<&mut Self>, video_id: &QString) {
        if let Some(row) = self.song_by_id(&video_id.to_string()) {
            self.play_row_item(&row.item, None);
        }
    }

    fn play_next_song(self: Pin<&mut Self>, video_id: &QString) {
        if let Some(row) = self.song_by_id(&video_id.to_string()) {
            let v = song_value(&row.item);
            crate::player::with_player(move |p| p.play_next_one(v));
        }
    }

    fn enqueue_song(self: Pin<&mut Self>, video_id: &QString) {
        if let Some(row) = self.song_by_id(&video_id.to_string()) {
            let v = song_value(&row.item);
            crate::player::with_player(move |p| p.enqueue_one(v));
        }
    }

    fn play_collection(self: Pin<&mut Self>, kind: &QString, browse_id: &QString, shuffle: bool) {
        let (kind, id) = (kind.to_string(), browse_id.to_string());
        if !matches!(kind.as_str(), "album" | "playlist") || id.is_empty() {
            return;
        }
        let src = source();
        let fut = src.load(Request { target: format!("{kind}:{id}"), ..Default::default() });
        st().rt.spawn(async move {
            match fut.await {
                Ok(page) => {
                    let mut pager = page.pager.filter(|_| page.has_more);
                    let songs = page_songs(&page);
                    if songs.is_empty() {
                        if let Some(id) = pager {
                            pages::forget(id);
                        }
                        return;
                    }
                    let start = if shuffle { rand::random_range(0..songs.len()) } else { 0 };
                    let values: Vec<Value> = songs.iter().map(song_value).collect();
                    crate::player::with_player(move |mut p| {
                        if shuffle {
                            p.as_mut().set_shuffle_on();
                        }
                        p.play_many(values, start);
                    });
                    // The rest of a long list follows into that queue, page by page, until the list ends or the queue
                    // is replaced; either way nothing is left behind for this list.
                    let queue = mark_queue();
                    while let Some(id) = pager.take() {
                        let Ok(next) = src.more(id).await else {
                            pages::forget(id);
                            break;
                        };
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        append_fed(&queue, page_songs(&next).iter().map(song_value).collect(), Some(tx));
                        pager = next.pager.filter(|_| next.has_more);
                        if !rx.await.unwrap_or(false) {
                            if let Some(id) = pager.take() {
                                pages::forget(id);
                            }
                        }
                    }
                }
                Err(e) => errors::toast(&e),
            }
        });
    }

    fn songs_json(&self) -> QString {
        let values: Vec<Value> = self.assembly.songs().iter().map(song_value).collect();
        QString::from(&Value::Array(values).to_string())
    }

    fn play_next(self: Pin<&mut Self>, i: i32) {
        if let Some(row) = self.song_at(i) {
            let v = song_value(&row.item);
            crate::player::with_player(move |p| p.play_next_one(v));
        }
    }

    fn enqueue(self: Pin<&mut Self>, i: i32) {
        if let Some(row) = self.song_at(i) {
            let v = song_value(&row.item);
            crate::player::with_player(move |p| p.enqueue_one(v));
        }
    }

    fn play_all(self: Pin<&mut Self>, shuffle: bool) {
        let songs = self.assembly.songs();
        if songs.is_empty() {
            return;
        }
        let start = if shuffle { rand::random_range(0..songs.len()) } else { 0 };
        let values: Vec<Value> = songs.iter().map(song_value).collect();
        crate::player::with_player(move |mut p| {
            if shuffle {
                p.as_mut().set_shuffle_on();
            }
            p.play_many(values, start);
        });
        self.start_feed_queue();
    }

    fn add_all_to_queue(self: Pin<&mut Self>) {
        let values: Vec<Value> = self.assembly.songs().iter().map(song_value).collect();
        crate::player::with_player(move |p| p.enqueue_many(values));
    }

    fn start_feed_queue(mut self: Pin<&mut Self>) {
        // the queue as it is after the play request that came just before this (see `mark_queue`)
        self.as_mut().st_mut().fed_queue = mark_queue();
        self.as_mut().st_mut().pager.set_feed_queue(true);
        self.continue_feed();
    }

    fn suggest(mut self: Pin<&mut Self>, text: &QString) {
        let text = text.to_string().trim().to_owned();
        if text.chars().count() < 2 {
            self.as_mut().set_suggestions(QStringList::default());
            return;
        }
        let r = self.as_mut().st_mut();
        r.suggest_gen += 1;
        let generation = r.suggest_gen;
        let th = self.qt_thread();
        st().rt.spawn(async move {
            let Ok(v) = crate::backend::suggest(text).await else { return };
            let list: Vec<String> =
                v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            let _ = th.queue(move |mut c| {
                if c.suggest_gen == generation {
                    c.as_mut().set_suggestions(qlist(list));
                }
            });
        });
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(row) = usize::try_from(index.row()).ok().and_then(|i| self.rows.get(i)) else { return QVariant::default() };
        let it = &row.item;
        let text = |s: &str| QVariant::from(&QString::from(s));
        match role - ROLE_BASE {
            0 => text(&it.title),
            1 => text(&it.subtitle),
            2 => text(&it.artist),
            3 => text(kind_name(it.kind)),
            4 => text(&it.video_id),
            5 => text(&it.browse_id),
            6 => text(&it.thumbnail),
            7 => text(&it.duration),
            8 => text(&it.artist_id),
            9 => text(&it.album_id),
            10 => QVariant::from(&(it.number as i32)),
            11 => text(&row.section),
            12 => text(&row.section_id),
            13 => text(items::style_name(row.style)),
            14 => text(row.placeholder()),
            15 => QVariant::from(&it.is_actionable()),
            _ => QVariant::default(),
        }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        for (i, name) in ROLE_NAMES.iter().enumerate() {
            roles.insert(ROLE_BASE + i as i32, QByteArray::from(*name));
        }
        roles
    }

    pub fn row_count(&self, _parent: &QModelIndex) -> i32 {
        self.rows.len() as i32
    }
}

// ---- developer harness: canned pages ------------------------------------------------------------------------------

#[cfg(feature = "harness")]
mod demo;

#[cfg(feature = "harness")]
pub fn install_demo_source() {
    set_source(Arc::new(demo::Demo));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pagedata::SectionStyle;

    #[test]
    fn songs_reach_the_player_in_the_json_shape_it_stores() {
        let item = Item { kind: ItemKind::Song, title: "T".into(), video_id: "v".into(), artist_id: "UC".into(), ..Default::default() };
        let v = song_value(&item);
        assert_eq!((v["videoId"].as_str(), v["artistId"].as_str(), v["kind"].as_str()), (Some("v"), Some("UC"), Some("song")));
    }

    fn row(section: &str, style: SectionStyle, title: &str) -> Row {
        Row {
            item: Item { kind: ItemKind::Song, title: title.into(), video_id: title.into(), ..Default::default() },
            section_id: section.into(),
            section: section.to_uppercase(),
            section_subtitle: "kicker".into(),
            style,
        }
    }

    #[test]
    fn rows_regroup_into_the_sections_the_rich_view_draws() {
        let rows = [
            row("a", SectionStyle::List, "1"),
            row("a", SectionStyle::List, "2"),
            row("b", SectionStyle::Cards, "3"),
            row("a", SectionStyle::List, "4"),
        ];
        let v = page_value(None, &rows, true);
        let secs = v["sections"].as_array().unwrap();
        assert_eq!(secs.len(), 3, "only consecutive rows of a section merge");
        assert_eq!((secs[0]["id"].as_str(), secs[0]["items"].as_array().map(Vec::len)), (Some("a"), Some(2)));
        assert_eq!(
            (secs[0]["title"].as_str(), secs[0]["subtitle"].as_str(), secs[1]["style"].as_str()),
            (Some("A"), Some("kicker"), Some("cards"))
        );
        assert_eq!((v["hasMore"].as_bool(), v["hero"].is_null()), (Some(true), true));
        assert_eq!(secs[0]["items"][1]["videoId"], "2", "items keep their camelCase fields");
    }

    #[test]
    fn neighbouring_sections_that_share_an_id_but_not_a_title_stay_apart() {
        // slugs of non-ASCII titles are all "": the id alone cannot tell such sections apart
        let mut a = row("", SectionStyle::Cards, "1");
        a.section = "日本語".into();
        let mut b = row("", SectionStyle::Cards, "2");
        b.section = "中文".into();
        let v = page_value(None, &[a, b], false);
        assert_eq!(v["sections"].as_array().unwrap().len(), 2);
        assert_eq!(
            UPDATE_ROLES.map(|r| ROLE_NAMES[(r - ROLE_BASE) as usize]),
            ["title", "subtitle", "browseId", "thumbnail", "placeholder"]
        );
    }

    #[test]
    fn a_page_that_grew_by_paging_is_still_one_section() {
        let first = [row("tracks", SectionStyle::NumberedList, "1")];
        let more = [row("tracks", SectionStyle::NumberedList, "2")];
        let all: Vec<Row> = first.iter().chain(more.iter()).cloned().collect();
        assert_eq!(page_value(None, &all, false)["sections"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn hero_songs_ignore_the_reserved_sections() {
        let song = |id: &str| Item { kind: ItemKind::Song, video_id: id.into(), ..Default::default() };
        let page = PageData {
            sections: vec![
                Section { id: "hero-radio".into(), items: vec![song("r")], ..Default::default() },
                Section { id: "tracks".into(), items: vec![song("a"), song("b")], ..Default::default() },
            ],
            ..Default::default()
        };
        assert_eq!(page_songs(&page).iter().map(|s| s.video_id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn json_converts_to_qt_json_without_panicking() {
        let v = serde_json::json!({"a": [1, 2.5, "x", null, true], "b": {"c": "d"}});
        let q = to_qjson(&v);
        let obj = q.to_object();
        assert_eq!(obj.len(), 2);
    }

    #[test]
    fn role_names_and_data_columns_line_up() {
        assert_eq!(ROLE_NAMES.len(), 16);
        assert_eq!(ROLE_NAMES[14], "placeholder");
        assert_eq!(kind_name(ItemKind::Playlist), "playlist");
    }
}
