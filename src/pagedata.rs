// SPDX-License-Identifier: GPL-3.0-only
//! The contract between page *builders* (`src/pages/*`, which turn YouTube data into pages) and the page
//! *controller / list models* (`src/page.rs`, `src/models/*`, which load, page, cache and show them).
//!
//! Builders return a [`PageData`]; the controller never needs to know where it came from. The JSON form
//! (camelCase) is what QML sees for items, matching the fields the QML rows already use
//! (`title, subtitle, artist, kind, videoId, browseId, thumbnail, duration, section, artistId, albumId`).
#![allow(dead_code)] // consumed by the workstreams that implement the builders and the models

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What an item *is*; decides what clicking it does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    #[default]
    Song,
    Album,
    Artist,
    Playlist,
}

/// One row/card in a list or shelf.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Item {
    pub kind: ItemKind,
    pub title: String,
    pub subtitle: String,
    /// First artist's name (songs).
    pub artist: String,
    /// Songs: the video id to play.
    pub video_id: String,
    /// Albums, artists, playlists: the id to open.
    pub browse_id: String,
    pub thumbnail: String,
    /// `m:ss` for songs, empty otherwise.
    pub duration: String,
    pub artist_id: String,
    pub album_id: String,
    /// 1-based position in a numbered list (album tracks); 0 = not numbered.
    pub number: u32,
}

impl Item {
    /// Parse the loose JSON items produced by `items.rs` today.
    pub fn from_value(v: &Value) -> Option<Item> {
        let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
        let kind = match v["kind"].as_str()? {
            "song" => ItemKind::Song,
            "album" => ItemKind::Album,
            "artist" => ItemKind::Artist,
            "playlist" => ItemKind::Playlist,
            _ => return None,
        };
        Some(Item {
            kind,
            title: s("title"),
            subtitle: s("subtitle"),
            artist: s("artist"),
            video_id: s("videoId"),
            browse_id: s("browseId"),
            thumbnail: s("thumbnail"),
            duration: s("duration"),
            artist_id: s("artistId"),
            album_id: s("albumId"),
            number: v["number"].as_u64().unwrap_or(0) as u32,
        })
    }

    /// Items the UI can act on: songs need a video id, everything else a browse id.
    pub fn is_actionable(&self) -> bool {
        match self.kind {
            ItemKind::Song => !self.video_id.is_empty(),
            _ => !self.browse_id.is_empty(),
        }
    }
}

/// How a section is laid out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionStyle {
    /// Dense rows (songs).
    #[default]
    List,
    /// Rows with a leading track number (album tracks).
    NumberedList,
    /// Horizontal shelf of square cards (albums, singles, playlists).
    Cards,
    /// Horizontal shelf of round avatars (artists).
    Artists,
    /// Wrapping grid of cards (e.g. "Listen again").
    Grid,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Section {
    /// Stable id (used to slot progressively loaded rows and to cache).
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub style: SectionStyle,
    pub items: Vec<Item>,
}

/// Buttons a hero offers; the page decides which make sense.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Play,
    Shuffle,
    Radio,
    Download,
    AddToQueue,
    SaveToLibrary,
}

/// A small fact shown as a chip under the title ("2019", "12 songs", "46 min").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Stat {
    pub text: String,
    /// Optional icon name for the chip.
    pub icon: String,
}

/// The big header of an album / artist / playlist page.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Hero {
    pub kind: Option<ItemKind>,
    pub title: String,
    pub subtitle: String,
    /// Cover (albums/playlists) or header image (artists).
    pub cover: String,
    /// Artist bio / album description, plain text.
    pub description: String,
    /// Link shown next to a bio ("Wikipedia"); only `https` URLs may be set here.
    pub wikipedia_url: String,
    pub stats: Vec<Stat>,
    pub actions: Vec<Action>,
}

/// Everything a page needs to render.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PageData {
    pub hero: Option<Hero>,
    pub sections: Vec<Section>,
    /// Present when the last section continues: pass it to `Backend.more` for the next page.
    pub pager: Option<u32>,
    pub has_more: bool,
}

impl PageData {
    /// Remove items that can't be acted on and sections left empty (cards without ids are dead cards).
    pub fn cleaned(mut self) -> PageData {
        for s in &mut self.sections {
            s.items.retain(Item::is_actionable);
        }
        self.sections.retain(|s| !s.items.is_empty());
        self
    }

    pub fn item_count(&self) -> usize {
        self.sections.iter().map(|s| s.items.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn items_parse_from_the_existing_loose_json() {
        let v = json!({"kind": "song", "title": "T", "subtitle": "S", "artist": "A", "videoId": "v1", "browseId": "",
                       "thumbnail": "u", "duration": "3:21", "section": "x", "artistId": "UC1", "albumId": "MPREb_1"});
        let i = Item::from_value(&v).unwrap();
        assert_eq!((i.kind, i.video_id.as_str(), i.artist_id.as_str(), i.album_id.as_str()), (ItemKind::Song, "v1", "UC1", "MPREb_1"));
        assert!(i.is_actionable());
        assert!(Item::from_value(&json!({"kind": "podcast"})).is_none());
        assert!(Item::from_value(&json!({"title": "no kind"})).is_none());
    }

    #[test]
    fn json_form_is_camel_case_for_qml() {
        let i = Item { kind: ItemKind::Album, title: "A".into(), browse_id: "MPREb_x".into(), ..Default::default() };
        let v = serde_json::to_value(&i).unwrap();
        assert_eq!(v["kind"], "album");
        assert_eq!(v["browseId"], "MPREb_x");
        assert!(v.get("browse_id").is_none());
    }

    #[test]
    fn cleaning_drops_dead_items_and_empty_sections() {
        let song = |id: &str| Item { kind: ItemKind::Song, video_id: id.into(), ..Default::default() };
        let page = PageData {
            sections: vec![
                Section { id: "a".into(), items: vec![song("v1"), song("")], ..Default::default() },
                Section { id: "b".into(), items: vec![song("")], ..Default::default() },
            ],
            ..Default::default()
        }
        .cleaned();
        assert_eq!(page.sections.len(), 1);
        assert_eq!(page.item_count(), 1);
    }

    #[test]
    fn page_data_round_trips_through_json() {
        let page = PageData {
            hero: Some(Hero { title: "Hero".into(), actions: vec![Action::Play, Action::Download], ..Default::default() }),
            sections: vec![Section { id: "s".into(), style: SectionStyle::Cards, ..Default::default() }],
            pager: Some(7),
            has_more: true,
        };
        let back: PageData = serde_json::from_value(serde_json::to_value(&page).unwrap()).unwrap();
        assert_eq!(back, page);
    }

    #[test]
    fn missing_fields_default_instead_of_failing() {
        let page: PageData = serde_json::from_value(json!({"sections": [{"title": "Only a title"}]})).unwrap();
        assert_eq!(page.sections[0].style, SectionStyle::List);
        assert!(page.hero.is_none() && !page.has_more);
    }
}
