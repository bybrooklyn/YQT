// SPDX-License-Identifier: GPL-3.0-only
//! Rows of a page list: what a [`PageData`](crate::pagedata::PageData) turns into, how progressively loaded
//! rows are slotted into place, and the library "Liked music" de-duplication. Plain Rust, no Qt.

#![allow(dead_code)] // a few accessors are API for the page builders that is not used here yet

use super::Change;
use crate::pagedata::{Item, ItemKind, Section, SectionStyle};
use std::collections::BTreeMap;

/// Browse ids that all mean "the liked songs": our synthetic row, YouTube's playlist, and its `VL`-prefixed form.
pub const LIKED_ID: &str = "LIKED";
const LIKED_ALIASES: [&str; 3] = [LIKED_ID, "LM", "VLLM"];

pub fn is_liked_id(browse_id: &str) -> bool {
    LIKED_ALIASES.contains(&browse_id)
}

/// The id to open for a playlist: every spelling of "liked music" opens the working liked-tracks path.
pub fn canonical_playlist_id(id: &str) -> &str {
    if is_liked_id(id) {
        LIKED_ID
    } else {
        id.trim_start_matches("VL")
    }
}

/// One row (or card) of a list, with the section it belongs to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Row {
    pub item: Item,
    pub section_id: String,
    pub section: String,
    pub style: SectionStyle,
}

impl Row {
    /// Which placeholder art to show when there is no (or no loaded) cover.
    pub fn placeholder(&self) -> &'static str {
        match self.item.kind {
            ItemKind::Song => "song",
            ItemKind::Album => "album",
            ItemKind::Artist => "artist",
            ItemKind::Playlist if is_liked_id(&self.item.browse_id) => "liked",
            ItemKind::Playlist => "playlist",
        }
    }

    /// YouTube's own liked-music playlist (as opposed to our synthetic [`LIKED_ID`] row).
    fn is_youtube_liked(&self) -> bool {
        self.item.kind == ItemKind::Playlist && matches!(self.item.browse_id.as_str(), "LM" | "VLLM")
    }
    fn is_synthetic_liked(&self) -> bool {
        self.item.kind == ItemKind::Playlist && self.item.browse_id == LIKED_ID
    }
    pub fn is_song(&self) -> bool {
        self.item.kind == ItemKind::Song
    }
}

pub fn style_name(s: SectionStyle) -> &'static str {
    match s {
        SectionStyle::List => "list",
        SectionStyle::NumberedList => "numbered",
        SectionStyle::Cards => "cards",
        SectionStyle::Artists => "artists",
        SectionStyle::Grid => "grid",
    }
}

pub fn rows_from_sections(sections: &[Section]) -> Vec<Row> {
    sections
        .iter()
        .flat_map(|s| {
            s.items.iter().map(|item| Row { item: item.clone(), section_id: s.id.clone(), section: s.title.clone(), style: s.style })
        })
        .collect()
}

/// Make the Library's playlist list show exactly one "Liked music" row.
///
/// `existing` are the rows already shown, `new` the batch about to be added (it is edited in place). YouTube's own
/// liked playlist wins over our synthetic row (it has the real cover); a synthetic row is only kept while
/// YouTube's has not appeared. Returns `(index, row)` when a row already on screen must be replaced in place.
pub fn dedupe_liked(existing: &[Row], new: &mut Vec<Row>) -> Option<(usize, Row)> {
    let mut replace = None;
    if let Some(yt) = new.iter().position(Row::is_youtube_liked) {
        new.retain(|r| !r.is_synthetic_liked());
        if let Some(at) = existing.iter().position(Row::is_synthetic_liked) {
            let yt = new.iter().position(Row::is_youtube_liked).unwrap_or(yt.min(new.len().saturating_sub(1)));
            let mut row = new.remove(yt);
            row.section = existing[at].section.clone();
            replace = Some((at, row));
        }
    } else if existing.iter().any(|r| r.is_youtube_liked() || r.is_synthetic_liked()) {
        new.retain(|r| !r.is_synthetic_liked());
    }
    // a second copy of YouTube's playlist in the same list is never useful either
    if existing.iter().any(Row::is_youtube_liked) {
        new.retain(|r| !r.is_youtube_liked());
    }
    replace
}

/// Rows grouped by *slot*: a page that loads in independent parts (Home rows) shows each part at its slot's
/// position no matter in which order they finish.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Assembly {
    slots: BTreeMap<i32, Vec<Row>>,
}

impl Assembly {
    pub fn from_slots(slots: BTreeMap<i32, Vec<Row>>) -> Assembly {
        Assembly { slots }
    }
    pub fn slots(&self) -> &BTreeMap<i32, Vec<Row>> {
        &self.slots
    }
    pub fn flat(&self) -> Vec<Row> {
        self.slots.values().flatten().cloned().collect()
    }
    pub fn len(&self) -> usize {
        self.slots.values().map(Vec::len).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn clear(&mut self) -> Change<Row> {
        self.slots.clear();
        Change::Reset(Vec::new())
    }

    /// Put `rows` at `slot`. A new slot is inserted at its place; refilling an existing slot rebuilds the list.
    pub fn set_slot(&mut self, slot: i32, rows: Vec<Row>) -> Change<Row> {
        if let std::collections::btree_map::Entry::Occupied(mut e) = self.slots.entry(slot) {
            e.insert(rows);
            return Change::Reset(self.flat());
        }
        let at = self.slots.range(..slot).map(|(_, v)| v.len()).sum();
        self.slots.insert(slot, rows.clone());
        Change::Insert { at, rows }
    }

    /// Append to the end of the last slot (the next page of a long list).
    pub fn append(&mut self, rows: Vec<Row>) -> Change<Row> {
        let at = self.len();
        self.slots.entry(self.slots.keys().next_back().copied().unwrap_or(0)).or_default().extend(rows.iter().cloned());
        Change::Insert { at, rows }
    }

    /// Replace one row in place (flat index).
    pub fn update(&mut self, flat_index: usize, row: Row) -> Option<Change<Row>> {
        let mut left = flat_index;
        for v in self.slots.values_mut() {
            if left < v.len() {
                v[left] = row.clone();
                return Some(Change::Update { at: flat_index, row });
            }
            left -= v.len();
        }
        None
    }

    pub fn songs(&self) -> Vec<Item> {
        self.slots.values().flatten().filter(|r| r.is_song()).map(|r| r.item.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::apply;
    use super::*;

    fn song(id: &str) -> Row {
        Row { item: Item { kind: ItemKind::Song, video_id: id.into(), title: id.into(), ..Default::default() }, ..Default::default() }
    }
    fn playlist(id: &str) -> Row {
        Row { item: Item { kind: ItemKind::Playlist, browse_id: id.into(), title: id.into(), ..Default::default() }, ..Default::default() }
    }
    fn ids(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| r.item.browse_id.clone() + &r.item.video_id).collect()
    }

    #[test]
    fn sections_flatten_into_rows_that_remember_their_section() {
        let sections = vec![
            Section {
                id: "a".into(),
                title: "A".into(),
                style: SectionStyle::Cards,
                items: vec![song("1"), song("2")].into_iter().map(|r| r.item).collect(),
                ..Default::default()
            },
            Section { id: "b".into(), title: "B".into(), items: vec![song("3").item], ..Default::default() },
        ];
        let rows = rows_from_sections(&sections);
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[1].section.as_str(), rows[1].style, rows[2].section_id.as_str()), ("A", SectionStyle::Cards, "b"));
    }

    #[test]
    fn placeholders_follow_the_kind_and_liked_ids() {
        assert_eq!(song("x").placeholder(), "song");
        assert_eq!(playlist("PL1").placeholder(), "playlist");
        for id in ["LIKED", "LM", "VLLM"] {
            assert_eq!(playlist(id).placeholder(), "liked", "{id}");
        }
        let artist = Row { item: Item { kind: ItemKind::Artist, ..Default::default() }, ..Default::default() };
        assert_eq!(artist.placeholder(), "artist");
    }

    #[test]
    fn every_spelling_of_liked_music_opens_the_working_path() {
        for id in ["LIKED", "LM", "VLLM"] {
            assert_eq!(canonical_playlist_id(id), "LIKED");
        }
        assert_eq!(canonical_playlist_id("VLPLabc"), "PLabc");
        assert_eq!(canonical_playlist_id("PLabc"), "PLabc");
    }

    #[test]
    fn youtubes_liked_playlist_replaces_the_synthetic_one_in_the_first_page() {
        let mut page = vec![playlist("LIKED"), playlist("PL1"), playlist("LM"), playlist("PL2")];
        assert!(dedupe_liked(&[], &mut page).is_none());
        assert_eq!(ids(&page), ["PL1", "LM", "PL2"]);
    }

    #[test]
    fn the_synthetic_row_stays_when_youtube_has_none() {
        let mut page = vec![playlist("LIKED"), playlist("PL1")];
        dedupe_liked(&[], &mut page);
        assert_eq!(ids(&page), ["LIKED", "PL1"]);
    }

    #[test]
    fn a_later_page_with_youtubes_liked_swaps_it_in_for_the_synthetic_row() {
        let shown = vec![playlist("LIKED"), playlist("PL1")];
        let mut next = vec![playlist("PL2"), playlist("VLLM")];
        let swap = dedupe_liked(&shown, &mut next).expect("swap");
        assert_eq!((swap.0, swap.1.item.browse_id.as_str()), (0, "VLLM"));
        assert_eq!(ids(&next), ["PL2"]);
    }

    #[test]
    fn liked_music_never_appears_twice() {
        let shown = vec![playlist("LM")];
        let mut next = vec![playlist("LIKED"), playlist("LM"), playlist("PL3")];
        assert!(dedupe_liked(&shown, &mut next).is_none());
        assert_eq!(ids(&next), ["PL3"]);
    }

    #[test]
    fn slots_land_in_order_whatever_order_they_finish() {
        let mut a = Assembly::default();
        let mut mirror: Vec<Row> = Vec::new();
        for (slot, rows) in [(2, vec![song("c")]), (0, vec![song("a1"), song("a2")]), (3, vec![song("d")]), (1, vec![song("b")])] {
            let change = a.set_slot(slot, rows);
            apply(&mut mirror, &change);
            assert_eq!(mirror, a.flat());
        }
        assert_eq!(ids(&mirror), ["a1", "a2", "b", "c", "d"]);
    }

    #[test]
    fn refilling_a_slot_rebuilds_instead_of_duplicating() {
        let mut a = Assembly::default();
        a.set_slot(0, vec![song("a")]);
        let c = a.set_slot(0, vec![song("z")]);
        assert!(matches!(c, Change::Reset(ref v) if v.len() == 1));
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn appending_extends_the_last_slot_and_reports_the_insert_position() {
        let mut a = Assembly::default();
        a.set_slot(0, vec![song("a"), song("b")]);
        assert_eq!(a.append(vec![song("c")]), Change::Insert { at: 2, rows: vec![song("c")] });
        assert_eq!(a.len(), 3);
        let mut mirror = a.flat();
        let c = a.update(2, song("c2")).unwrap();
        apply(&mut mirror, &c);
        assert_eq!(mirror, a.flat());
        assert!(a.update(9, song("x")).is_none());
    }

    #[test]
    fn songs_skips_albums_and_artists() {
        let mut a = Assembly::default();
        a.set_slot(0, vec![song("a"), playlist("PL"), song("b")]);
        assert_eq!(a.songs().len(), 2);
    }
}
