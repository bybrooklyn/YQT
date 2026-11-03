// SPDX-License-Identifier: GPL-3.0-only
//! The rules of a page that loads, pages and caches: no Qt in here.
//!
//! * [`PagerState`] — idle / loading / ready / error, and when the next page may be fetched ("near the end",
//!   one request at a time, never chained).
//! * [`PageCache`] — pages seen recently, keyed by target + filter, with a time-to-live from the config.
//! * [`chips`] — the tab-like filters of a page (search types, library tabs).

#![allow(dead_code)] // a few accessors are API for the page builders that is not used here yet

use super::items::Row;
use crate::pagedata::Hero;
use std::collections::{BTreeMap, HashMap};

/// How many rows from the end the bottom of the viewport must be before the next page is requested.
pub const NEAR_END_ROWS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    Loading,
    Ready,
    Error,
}

#[derive(Debug, Clone, Default)]
pub struct PagerState {
    phase: Phase,
    pager: Option<u32>,
    has_more: bool,
    loading_more: bool,
    /// "Play" on a long list: keep fetching pages (regardless of scrolling) and append them to the queue.
    feed_queue: bool,
}

impl PagerState {
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn has_more(&self) -> bool {
        self.has_more
    }
    pub fn loading_more(&self) -> bool {
        self.loading_more
    }
    pub fn feed_queue(&self) -> bool {
        self.feed_queue
    }

    /// A fresh load starts: forget any paging of the previous one.
    pub fn start(&mut self) {
        *self = PagerState { phase: Phase::Loading, ..Default::default() };
    }

    /// The first answer arrived. `pager` is the id to pass to `more` when `has_more`.
    pub fn ready(&mut self, pager: Option<u32>, has_more: bool) {
        self.phase = Phase::Ready;
        self.pager = pager;
        self.has_more = has_more && pager.is_some();
        self.loading_more = false;
    }

    pub fn fail(&mut self) {
        self.phase = Phase::Error;
        self.has_more = false;
        self.loading_more = false;
        self.feed_queue = false;
    }

    /// Without a progressive page being "ready" the first rows can show while it is still loading.
    pub fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
    }

    pub fn set_feed_queue(&mut self, on: bool) {
        self.feed_queue = on && self.has_more;
    }

    /// Is the bottom of the viewport close enough to the end of the list? `bottom` is the index of the row at the
    /// bottom edge: `-1` means the edge is past the last row, anything below that means "the view could not tell yet"
    /// (rows not laid out). Judged by index, never by content height, which a `ListView` only estimates.
    pub fn near_end(bottom: i32, count: usize) -> bool {
        count > 0 && (bottom == -1 || (bottom >= 0 && bottom as usize + NEAR_END_ROWS >= count))
    }

    /// May a next page be requested now? Yes only when the list is ready, continues, nothing is in flight, and
    /// either the user is near the end or the queue is being fed.
    pub fn wants_more(&self, bottom: i32, count: usize) -> bool {
        self.phase == Phase::Ready
            && self.has_more
            && !self.loading_more
            && self.pager.is_some()
            && (self.feed_queue || Self::near_end(bottom, count))
    }

    /// Claim the next page: returns the pager id and marks a request in flight. A second call returns `None`
    /// until [`more_done`](Self::more_done) / [`more_failed`](Self::more_failed) — loads never chain by themselves.
    pub fn begin_more(&mut self, bottom: i32, count: usize) -> Option<u32> {
        if !self.wants_more(bottom, count) {
            return None;
        }
        self.loading_more = true;
        self.pager
    }

    pub fn more_done(&mut self, pager: Option<u32>, has_more: bool) {
        self.loading_more = false;
        self.pager = pager.or(self.pager);
        self.has_more = has_more;
        if !has_more {
            self.feed_queue = false;
        }
    }

    /// Keep what is on screen and stop asking (a failing endpoint is not hammered on every scroll event).
    pub fn more_failed(&mut self) {
        self.loading_more = false;
        self.has_more = false;
        self.feed_queue = false;
    }
}

// ---------------------------------------------------------------------------------------------------------

/// What the cache keeps of a page: enough to rebuild it without a request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CachedPage {
    pub hero: Option<Hero>,
    pub slots: BTreeMap<i32, Vec<Row>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    /// Inside the TTL: show it and do not ask again.
    Fresh(CachedPage),
    /// Older than the TTL: show it now, refresh quietly behind it.
    Stale(CachedPage),
    Miss,
}

struct Entry {
    at_ms: u64,
    epoch: u32,
    page: CachedPage,
}

/// Recently seen pages. Searches and long (paged) lists are never stored; a change of the signed-in account
/// (the `epoch`) makes everything stored before it invisible.
#[derive(Default)]
pub struct PageCache {
    entries: HashMap<String, Entry>,
}

impl PageCache {
    /// The cache key for a request, or `None` when it must not be cached (searches).
    pub fn key(target: &str, query: &str, filter: &str) -> Option<String> {
        (query.is_empty() && !target.is_empty()).then(|| format!("{target}|{filter}"))
    }

    /// `ttl_ms == 0` disables the cache. `force` bypasses it (e.g. "reload").
    pub fn get(&self, key: &str, now_ms: u64, ttl_ms: u64, epoch: u32, force: bool) -> Lookup {
        if ttl_ms == 0 || force {
            return Lookup::Miss;
        }
        match self.entries.get(key) {
            Some(e) if e.epoch == epoch => {
                if now_ms.saturating_sub(e.at_ms) < ttl_ms {
                    Lookup::Fresh(e.page.clone())
                } else {
                    Lookup::Stale(e.page.clone())
                }
            }
            _ => Lookup::Miss,
        }
    }

    /// Store a page. Returns whether it was stored (not for paged lists, not when the cache is off).
    pub fn put(&mut self, key: &str, page: CachedPage, has_more: bool, now_ms: u64, ttl_ms: u64, epoch: u32) -> bool {
        if ttl_ms == 0 || has_more {
            return false;
        }
        self.entries.insert(key.to_owned(), Entry { at_ms: now_ms, epoch, page });
        true
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

// ---------------------------------------------------------------------------------------------------------

/// A tab-like filter shown above a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub label: &'static str,
    pub value: &'static str,
}

const SEARCH_CHIPS: [Chip; 5] = [
    Chip { label: "All", value: "" },
    Chip { label: "Songs", value: "songs" },
    Chip { label: "Albums", value: "albums" },
    Chip { label: "Artists", value: "artists" },
    Chip { label: "Playlists", value: "playlists" },
];

const LIBRARY_CHIPS: [Chip; 5] = [
    Chip { label: "Playlists", value: "playlists" },
    Chip { label: "Albums", value: "albums" },
    Chip { label: "Artists", value: "artists" },
    Chip { label: "Songs", value: "songs" },
    Chip { label: "History", value: "history" },
];

/// The filters for a page: search types for a search, tabs for the library, none otherwise.
pub fn chips(target: &str, query: &str) -> &'static [Chip] {
    if !query.is_empty() {
        &SEARCH_CHIPS
    } else if target == "library" || target.starts_with("library:") {
        &LIBRARY_CHIPS
    } else {
        &[]
    }
}

/// The filter a page starts with (the library opens on Playlists).
pub fn default_filter(target: &str, query: &str) -> &'static str {
    chips(target, query).first().map_or("", |c| c.value)
}

/// The request target for a page: the library tab is part of the target.
pub fn effective_target(target: &str, filter: &str) -> String {
    if target == "library" && !filter.is_empty() {
        format!("library:{filter}")
    } else {
        target.to_owned()
    }
}

/// Album / playlist / artist pages: clicking a song plays the whole list.
pub fn is_collection(target: &str, query: &str) -> bool {
    query.is_empty() && ["album:", "playlist:", "artist:"].iter().any(|p| target.starts_with(p))
}

/// Pages that show the search field.
pub fn is_searchable(target: &str, query: &str) -> bool {
    !query.is_empty() || !(is_collection(target, query) || target.starts_with("library"))
}

/// Pages that need a signed-in account.
pub fn needs_login(target: &str, query: &str) -> bool {
    query.is_empty() && target.starts_with("library")
}

/// Pages that must be rebuilt when the account changes.
pub fn reloads_on_login(target: &str, query: &str) -> bool {
    needs_login(target, query) || (query.is_empty() && target == "home")
}

/// Home arrives as independent rows.
pub fn is_progressive(target: &str, query: &str) -> bool {
    query.is_empty() && target == "home"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(more: bool) -> PagerState {
        let mut p = PagerState::default();
        p.start();
        p.ready(Some(7), more);
        p
    }

    #[test]
    fn near_the_end_is_judged_by_the_bottom_row_index() {
        assert!(!PagerState::near_end(0, 0), "an empty list is never near its end");
        assert!(!PagerState::near_end(5, 100));
        assert!(PagerState::near_end(90, 100));
        assert!(PagerState::near_end(99, 100));
        assert!(PagerState::near_end(-1, 100), "-1: past the last row");
        assert!(!PagerState::near_end(-2, 100), "below -1: the view cannot tell yet, so do not guess");
        assert!(PagerState::near_end(0, 8), "a short list is always near its end");
    }

    #[test]
    fn a_page_loads_more_only_when_ready_continuing_and_near_the_end() {
        let mut p = PagerState::default();
        assert!(!p.wants_more(99, 100), "idle");
        p.start();
        assert!(!p.wants_more(99, 100), "still loading the first page");
        p.ready(Some(7), true);
        assert!(!p.wants_more(10, 100), "far from the end");
        assert!(p.wants_more(95, 100));
        assert!(!ready(false).wants_more(99, 100), "nothing more to load");
        let mut no_pager = PagerState::default();
        no_pager.start();
        no_pager.ready(None, true);
        assert!(!no_pager.has_more(), "a continuation without a pager id is no continuation");
    }

    #[test]
    fn loads_never_chain() {
        let mut p = ready(true);
        assert_eq!(p.begin_more(99, 100), Some(7));
        assert_eq!(p.begin_more(99, 100), None, "one request at a time");
        p.more_done(None, true);
        // the list grew: the viewport is now far from the end, so nothing starts by itself
        assert_eq!(p.begin_more(60, 200), None);
        assert!(p.has_more() && !p.loading_more());
        // only a new "near the end" (a scroll) starts the next one
        assert_eq!(p.begin_more(195, 200), Some(7));
    }

    #[test]
    fn a_failed_page_stops_further_attempts_but_keeps_the_list() {
        let mut p = ready(true);
        p.begin_more(99, 100);
        p.more_failed();
        assert_eq!(p.begin_more(99, 100), None);
        assert_eq!(p.phase(), Phase::Ready);
    }

    #[test]
    fn feeding_the_queue_ignores_the_scroll_position_until_the_end() {
        let mut p = ready(true);
        p.set_feed_queue(true);
        assert_eq!(p.begin_more(0, 100), Some(7));
        assert_eq!(p.begin_more(0, 100), None, "feeding still never has two requests in flight");
        p.more_done(Some(8), true);
        assert_eq!(p.begin_more(0, 200), Some(8), "a continuation may bring a new pager id");
        p.more_done(None, false);
        assert!(!p.feed_queue(), "feeding ends with the list");
        assert_eq!(p.begin_more(0, 300), None);
    }

    #[test]
    fn feeding_needs_something_to_feed_from() {
        let mut p = ready(false);
        p.set_feed_queue(true);
        assert!(!p.feed_queue());
    }

    #[test]
    fn starting_over_forgets_paging_and_errors_stop_it() {
        let mut p = ready(true);
        p.set_feed_queue(true);
        p.start();
        assert_eq!((p.phase(), p.has_more(), p.feed_queue()), (Phase::Loading, false, false));
        p.fail();
        assert_eq!(p.phase(), Phase::Error);
        assert!(!p.wants_more(99, 100));
    }

    fn page(tag: &str) -> CachedPage {
        let mut slots = BTreeMap::new();
        slots.insert(0, vec![Row { section: tag.into(), ..Default::default() }]);
        CachedPage { hero: None, slots }
    }

    #[test]
    fn cache_keys_exist_for_pages_but_not_searches() {
        assert_eq!(PageCache::key("library", "", "albums").as_deref(), Some("library|albums"));
        assert_eq!(PageCache::key("album:X", "", "").as_deref(), Some("album:X|"));
        assert_eq!(PageCache::key("", "daft punk", "songs"), None);
        assert_eq!(PageCache::key("home", "q", ""), None);
    }

    #[test]
    fn cached_pages_are_fresh_then_stale_by_ttl() {
        let mut c = PageCache::default();
        assert!(c.put("k", page("a"), false, 1_000, 60_000, 1));
        assert!(matches!(c.get("k", 30_000, 60_000, 1, false), Lookup::Fresh(_)));
        assert!(matches!(c.get("k", 61_000, 60_000, 1, false), Lookup::Stale(_)), "past the TTL it is shown while refreshing");
        assert_eq!(c.get("missing", 0, 60_000, 1, false), Lookup::Miss);
    }

    #[test]
    fn force_and_a_zero_ttl_bypass_the_cache() {
        let mut c = PageCache::default();
        c.put("k", page("a"), false, 0, 60_000, 1);
        assert_eq!(c.get("k", 10, 60_000, 1, true), Lookup::Miss);
        assert_eq!(c.get("k", 10, 0, 1, false), Lookup::Miss);
        assert!(!c.put("z", page("b"), false, 0, 0, 1), "nothing is stored while the cache is off");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn paged_lists_are_not_cached() {
        let mut c = PageCache::default();
        assert!(!c.put("k", page("a"), true, 0, 60_000, 1));
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn a_login_change_hides_everything_cached_before_it() {
        let mut c = PageCache::default();
        c.put("k", page("a"), false, 0, 60_000, 1);
        assert_eq!(c.get("k", 10, 60_000, 2, false), Lookup::Miss);
        c.clear();
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn chips_belong_to_the_kind_of_page() {
        let values = |t, q| chips(t, q).iter().map(|c| c.value).collect::<Vec<_>>();
        assert_eq!(values("", "x"), ["", "songs", "albums", "artists", "playlists"]);
        assert_eq!(values("library", ""), ["playlists", "albums", "artists", "songs", "history"]);
        assert!(values("home", "").is_empty() && values("album:1", "").is_empty());
        assert_eq!(default_filter("library", ""), "playlists");
        assert_eq!(default_filter("", "q"), "");
    }

    #[test]
    fn page_kinds_follow_the_target() {
        assert_eq!(effective_target("library", "albums"), "library:albums");
        assert_eq!(effective_target("home", "albums"), "home");
        assert!(is_collection("playlist:LIKED", "") && is_collection("artist:UC1", "") && !is_collection("home", ""));
        assert!(!is_collection("album:1", "query"), "a search is never a collection");
        assert!(is_searchable("home", "") && is_searchable("", "q") && !is_searchable("library", "") && !is_searchable("album:1", ""));
        assert!(needs_login("library", "") && !needs_login("home", ""));
        assert!(reloads_on_login("home", "") && reloads_on_login("library", "") && !reloads_on_login("album:1", ""));
        assert!(is_progressive("home", "") && !is_progressive("home", "q"));
    }
}
