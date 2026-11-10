// SPDX-License-Identifier: GPL-3.0-only
//! Page loading: the one entry point the page controller uses (`pages::load`, `pages::more`, `pages::home_loaders`).
//!
//! Album, playlist, artist, Home and the library tabs have real builders (`album.rs`, `playlist.rs`, `artist.rs`,
//! `home.rs`, `library.rs`: a hero plus typed sections). Search, Explore and Liked music still start from the legacy JSON
//! loaders in `backend.rs` and are adapted (and restyled) here. The public signatures never change.
#![allow(dead_code)] // consumed by the page controller once it lands

pub mod album;
pub mod artist;
pub mod common;
pub mod home;
pub mod library;
pub mod playlist;

use crate::auth::AUTH_MSG;
use crate::errors::{AppError, ErrorKind};
use crate::pagedata::{Hero, Item, PageData, Section, SectionStyle};
use crate::state::st;
use rustypipe::model::paginator::Paginator;
use rustypipe::model::TrackItem;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// What to load: `target` is `home`, `explore`, `album:ID`, `playlist:ID`, `artist:ID`, `library:<tab>`;
/// or, for a search, `query` plus a `filter` (`""`, `songs`, `albums`, `artists`, `playlists`).
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub target: String,
    pub query: String,
    pub filter: String,
}

/// An error string from the legacy loaders (`"headline"` / `"headline\n\ndetail"`) as an [`AppError`].
pub fn error_from_ui_string(s: &str) -> AppError {
    let (headline, detail) = s.split_once("\n\n").unwrap_or((s, ""));
    if s == AUTH_MSG {
        AppError::new(ErrorKind::Auth, AUTH_MSG, "")
    } else {
        AppError::new(ErrorKind::Internal, headline, detail)
    }
}

fn slug(s: &str) -> String {
    let lowered: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    lowered.split('-').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-")
}

/// Run a RustyPipe request, and once more after a short pause if it failed in a transient way.
async fn retry<T, F, Fut>(mut f: F) -> Result<T, AppError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, rustypipe::error::Error>>,
{
    let result = match f().await {
        Err(e) if crate::errors::worth_retrying(&e) => {
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            f().await
        }
        r => r,
    };
    result.map_err(|e| AppError::from_rustypipe_request(&e, false, false))
}

// ---- continuation of track lists (playlists) ------------------------------------------------------------------

/// Pager ids of this module start here, far above the ids the legacy loaders hand out.
const PAGER_BASE: u32 = 1 << 30;
/// How many partially loaded lists are remembered at once.
const MAX_CONTS: usize = 16;

struct TrackCont {
    pager: Paginator<TrackItem>,
    cover: String,
    shown: usize,
}

static CONTS: Mutex<Option<HashMap<u32, TrackCont>>> = Mutex::new(None);
static NEXT_CONT: AtomicU32 = AtomicU32::new(PAGER_BASE);

fn conts<R>(f: impl FnOnce(&mut HashMap<u32, TrackCont>) -> R) -> R {
    f(CONTS.lock().unwrap().get_or_insert_default())
}

/// Remember a partially loaded track list; the returned id goes in `PageData::pager`.
pub(crate) fn register_tracks(pager: Paginator<TrackItem>, cover: String, shown: usize) -> u32 {
    let id = NEXT_CONT.fetch_add(1, Ordering::Relaxed) + 1;
    conts(|m| {
        m.insert(id, TrackCont { pager, cover, shown });
        drop_oldest(m, MAX_CONTS);
    });
    id
}

/// Lists nobody pages to the end (or forgets) must not pile up. Ids only grow, so the smallest is the oldest.
fn drop_oldest<T>(m: &mut HashMap<u32, T>, max: usize) {
    while m.len() > max {
        let Some(&oldest) = m.keys().min() else { break };
        m.remove(&oldest);
    }
}

/// Forget a partially loaded list whose page is not going to ask for more.
pub fn forget(pager: u32) {
    if pager > PAGER_BASE {
        conts(|m| m.remove(&pager));
    } else {
        crate::backend::drop_pager(pager as i32); // a list of the legacy loaders (Liked music, search)
    }
}

async fn more_tracks(id: u32) -> Result<PageData, AppError> {
    let mut cont = conts(|m| m.remove(&id)).ok_or_else(|| AppError::new(ErrorKind::NotFound, "No more results", ""))?;
    if let Err(e) = cont.pager.extend(&st().rp.query()).await {
        conts(|m| m.insert(id, cont)); // allow a retry
        return Err(AppError::from_rustypipe_request(&e, false, false));
    }
    let new = common::numbered_tracks(cont.pager.items.get(cont.shown..).unwrap_or_default(), &cont.cover, cont.shown);
    cont.shown = cont.pager.items.len();
    let has_more = cont.pager.ctoken.is_some();
    if has_more {
        conts(|m| m.insert(id, cont));
    }
    Ok(PageData {
        hero: None,
        sections: vec![common::section("tracks", "", SectionStyle::NumberedList, new)],
        pager: has_more.then_some(id),
        has_more,
    }
    .cleaned())
}

// ---- legacy adapters ------------------------------------------------------------------------------------------

/// Group legacy items into sections: consecutive items with the same `section` name form one section.
pub fn sections_from_items(items: &[Value]) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    for v in items {
        let Some(item) = Item::from_value(v) else { continue };
        let title = v["section"].as_str().unwrap_or("").to_owned();
        match sections.last_mut() {
            Some(s) if s.title == title => s.items.push(item),
            _ => sections.push(Section { id: slug(&title), title, style: SectionStyle::List, items: vec![item], ..Default::default() }),
        }
    }
    sections
}

/// Adapt one legacy page object (`{title, subtitle, thumbnail, items, pager?, more?}`) into [`PageData`].
pub fn page_from_value(v: &Value) -> PageData {
    let title = v["title"].as_str().unwrap_or("");
    let hero = (!title.is_empty()).then(|| Hero {
        title: title.to_owned(),
        subtitle: v["subtitle"].as_str().unwrap_or("").to_owned(),
        cover: v["thumbnail"].as_str().unwrap_or("").to_owned(),
        ..Default::default()
    });
    PageData {
        hero,
        sections: sections_from_items(v["items"].as_array().map(Vec::as_slice).unwrap_or(&[])),
        pager: v["pager"].as_u64().map(|p| p as u32),
        has_more: v["more"].as_bool().unwrap_or(false),
    }
    .cleaned()
}

async fn legacy_browse(target: String) -> Result<PageData, AppError> {
    crate::backend::browse(target).await.map(|v| page_from_value(&v)).map_err(|e| error_from_ui_string(&e))
}

/// Explore: shelves of new releases and videos.
fn restyle_explore(mut page: PageData) -> PageData {
    page.sections = home::restyle_all(page.sections);
    page
}

/// Load a page.
pub async fn load(req: &Request) -> Result<PageData, AppError> {
    if !req.query.is_empty() {
        let value = crate::backend::search(req.query.clone(), req.filter.clone()).await;
        return value.map(|v| page_from_value(&v)).map_err(|e| error_from_ui_string(&e));
    }
    let (kind, id) = req.target.split_once(':').unwrap_or((req.target.as_str(), ""));
    match (kind, id) {
        ("home", _) => home::load().await,
        ("album", id) if !id.is_empty() => album::load(id).await,
        // YouTube's own "Liked Music" playlist is the same thing as our synthetic one; it has its own fallbacks.
        ("playlist", "LIKED" | "LM" | "VLLM") => legacy_browse("playlist:LIKED".into()).await.map(playlist::decorate_legacy),
        ("playlist", id) if !id.is_empty() => playlist::load(id).await,
        ("artist", id) if !id.is_empty() => artist::load(id).await,
        ("library", tab) => library::load(tab).await,
        ("explore", _) => legacy_browse(req.target.clone()).await.map(restyle_explore),
        _ => legacy_browse(req.target.clone()).await,
    }
}

/// The next page of a list whose answer carried a `pager`. The returned sections hold only the *new* items.
pub async fn more(pager: u32) -> Result<PageData, AppError> {
    if pager > PAGER_BASE {
        return more_tracks(pager).await;
    }
    crate::backend::more(pager as i32).await.map(|v| page_from_value(&v)).map_err(|e| error_from_ui_string(&e))
}

/// One independently loadable row of Home.
pub struct HomeRow {
    /// Position on the page; rows may finish in any order.
    pub slot: i32,
    pub load: Pin<Box<dyn Future<Output = Result<Vec<Section>, AppError>> + Send>>,
}

/// The rows of Home, to be run concurrently and shown as each completes. Signed in that is one row: the account's
/// own YouTube Music feed (or the built rows if there is none). Signed-out users get charts and new releases.
pub fn home_loaders() -> Vec<HomeRow> {
    use crate::backend::{home_charts, home_new_albums, home_new_videos, home_signed_in, Row};
    fn adapt(f: impl Future<Output = Row> + Send + 'static) -> Pin<Box<dyn Future<Output = Result<Vec<Section>, AppError>> + Send>> {
        Box::pin(async move { f.await.map(|items| home::restyle_all(sections_from_items(&items))).map_err(|e| error_from_ui_string(&e)) })
    }
    if crate::backend::is_logged_in() {
        return vec![HomeRow { slot: 0, load: adapt(home_signed_in()) }];
    }
    vec![
        HomeRow { slot: 1, load: adapt(home_charts()) },
        HomeRow { slot: 2, load: adapt(home_new_albums()) },
        HomeRow { slot: 3, load: adapt(home_new_videos()) },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn song(title: &str, vid: &str, section: &str) -> Value {
        json!({"kind": "song", "title": title, "videoId": vid, "section": section, "browseId": "", "thumbnail": "", "duration": "", "subtitle": "", "artist": "", "artistId": "", "albumId": ""})
    }

    #[test]
    fn consecutive_items_with_the_same_section_form_one_section() {
        let items = vec![song("a", "1", "Songs"), song("b", "2", "Songs"), song("c", "3", "Albums"), song("d", "4", "Songs")];
        let s = sections_from_items(&items);
        let shape: Vec<(&str, usize)> = s.iter().map(|x| (x.title.as_str(), x.items.len())).collect();
        assert_eq!(shape, [("Songs", 2), ("Albums", 1), ("Songs", 1)]);
        assert_eq!(s[0].id, "songs");
    }

    #[test]
    fn a_collection_page_becomes_a_hero_plus_sections_and_keeps_paging_info() {
        let v = json!({"title": "Mix", "subtitle": "50 songs", "thumbnail": "u", "pager": 4, "more": true,
                       "items": [song("a", "1", ""), song("dead", "", "")]});
        let p = page_from_value(&v);
        assert_eq!(p.hero.as_ref().map(|h| h.title.as_str()), Some("Mix"));
        assert_eq!((p.pager, p.has_more, p.item_count()), (Some(4), true, 1), "the song without an id is dropped");
    }

    #[test]
    fn a_plain_list_has_no_hero() {
        assert!(page_from_value(&json!({"title": "", "items": [song("a", "1", "X")]})).hero.is_none());
    }

    #[test]
    fn legacy_error_strings_become_app_errors() {
        let e = error_from_ui_string(AUTH_MSG);
        assert_eq!((e.kind, e.detail.as_str()), (ErrorKind::Auth, ""));
        let e = error_from_ui_string("Can't reach YouTube.\n\ndns failure");
        assert_eq!((e.headline.as_str(), e.detail.as_str()), ("Can't reach YouTube.", "dns failure"));
    }

    #[test]
    fn slugs_are_stable_ids() {
        assert_eq!(slug("New albums & singles"), "new-albums-singles");
        assert_eq!(slug("  Top  songs "), "top-songs");
        assert_eq!(slug(""), "");
    }

    #[test]
    fn our_pager_ids_never_collide_with_the_legacy_ones() {
        let id = register_tracks(Paginator::default(), String::new(), 0);
        assert!(id > PAGER_BASE);
        assert!(conts(|m| m.remove(&id)).is_some());
    }

    #[test]
    fn remembered_lists_are_bounded_and_can_be_forgotten() {
        let first = register_tracks(Paginator::default(), String::new(), 0);
        forget(first);
        assert!(conts(|m| !m.contains_key(&first)));
        // (on a map of its own: the shared one is used by the other tests at the same time)
        let mut m: HashMap<u32, ()> = (1..=20).map(|id| (id, ())).collect();
        drop_oldest(&mut m, 16);
        let mut left: Vec<u32> = m.into_keys().collect();
        left.sort_unstable();
        assert_eq!(left, (5..=20).collect::<Vec<u32>>(), "the oldest go first");
    }

    #[test]
    fn an_unknown_pager_is_a_clean_error_not_a_panic() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let err = rt.block_on(more_tracks(PAGER_BASE + 999_999)).unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotFound);
    }
}
