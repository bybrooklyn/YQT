// SPDX-License-Identifier: GPL-3.0-only
//! Page loading: the one entry point the page controller uses (`pages::load`, `pages::more`, `pages::home_loaders`).
//!
//! **Today** these adapt the legacy JSON loaders in `backend.rs` into [`PageData`]. The rich-pages workstream
//! replaces the *internals* with real builders (`home.rs`, `album.rs`, `artist.rs`, `playlist.rs`, `library.rs`
//! producing heroes and shelves) behind exactly these signatures, so the controller never changes.
#![allow(dead_code)] // consumed by the page controller once it lands

use crate::auth::AUTH_MSG;
use crate::errors::{AppError, ErrorKind};
use crate::pagedata::{Hero, Item, PageData, Section, SectionStyle};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

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

/// Load a page.
pub async fn load(req: &Request) -> Result<PageData, AppError> {
    let value = if req.query.is_empty() {
        crate::backend::browse(req.target.clone()).await
    } else {
        crate::backend::search(req.query.clone(), req.filter.clone()).await
    };
    value.map(|v| page_from_value(&v)).map_err(|e| error_from_ui_string(&e))
}

/// The next page of a list whose answer carried a `pager`. The returned sections hold only the *new* items.
pub async fn more(pager: u32) -> Result<PageData, AppError> {
    crate::backend::more(pager as i32).await.map(|v| page_from_value(&v)).map_err(|e| error_from_ui_string(&e))
}

/// One independently loadable row of Home.
pub struct HomeRow {
    /// Position on the page; rows may finish in any order.
    pub slot: i32,
    pub load: Pin<Box<dyn Future<Output = Result<Vec<Section>, AppError>> + Send>>,
}

/// The rows of Home, to be run concurrently and shown as each completes. Signed-out users get no personal row.
pub fn home_loaders() -> Vec<HomeRow> {
    use crate::backend::{home_charts, home_new_albums, home_new_videos, home_personal, Row};
    fn adapt(f: impl Future<Output = Row> + Send + 'static) -> Pin<Box<dyn Future<Output = Result<Vec<Section>, AppError>> + Send>> {
        Box::pin(async move { f.await.map(|items| sections_from_items(&items)).map_err(|e| error_from_ui_string(&e)) })
    }
    vec![
        HomeRow { slot: 0, load: adapt(home_personal()) },
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
}
