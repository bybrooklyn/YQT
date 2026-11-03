// SPDX-License-Identifier: GPL-3.0-only
//! Small pure helpers shared by the page builders: RustyPipe items → [`Item`], text clean-up, stat strings.

use crate::items;
use crate::pagedata::{Item, Section, SectionStyle};
use crate::security::is_safe_external_link;
use rustypipe::model::richtext::{RichText, ToPlaintext};
use rustypipe::model::{AlbumItem, AlbumType, ArtistId, ArtistItem, MusicPlaylistItem, TrackItem};
use serde_json::Value;

/// Reserved section id: the artists of an album/playlist as links for the hero (QML shows them as chips under the
/// title, not as a shelf). Items are `Artist` kind with a browse id.
pub const HERO_ARTISTS: &str = "hero-artists";
/// Reserved section id: one `Playlist` item whose `browseId` is the artist radio's playlist id (the "Radio" action).
pub const HERO_RADIO: &str = "hero-radio";

/// Longest description we pass on (characters); the UI collapses it much earlier.
pub const MAX_DESCRIPTION: usize = 4000;

fn from_json(v: Value) -> Option<Item> {
    items::clean(v).and_then(|v| Item::from_value(&v))
}

/// A track as an [`Item`]; `fallback_cover` is used when the track brings none (album tracks), `number` is the
/// 1-based list position (0 = unnumbered).
pub fn track_item(t: &TrackItem, fallback_cover: &str, number: u32) -> Option<Item> {
    let mut item = from_json(items::track(t, "", fallback_cover))?;
    item.number = number;
    Some(item)
}

pub fn album_item(a: &AlbumItem) -> Option<Item> {
    from_json(items::album(a, ""))
}

pub fn artist_item(a: &ArtistItem) -> Option<Item> {
    from_json(items::artist(a, ""))
}

pub fn playlist_item(p: &MusicPlaylistItem) -> Option<Item> {
    from_json(items::playlist(p, ""))
}

pub fn section(id: &str, title: &str, style: SectionStyle, items: Vec<Item>) -> Section {
    Section { id: id.to_owned(), title: title.to_owned(), subtitle: String::new(), style, items }
}

/// The numbered track rows of an album/playlist; numbering continues from `already` rows.
pub fn numbered_tracks(tracks: &[TrackItem], fallback_cover: &str, already: usize) -> Vec<Item> {
    tracks.iter().enumerate().filter_map(|(i, t)| track_item(t, fallback_cover, (already + i + 1) as u32)).collect()
}

/// Links to the artists that have a channel id, as `Artist` items for the hero.
pub fn artist_links(artists: &[ArtistId]) -> Vec<Item> {
    artists
        .iter()
        .filter_map(|a| {
            let id = a.id.as_deref().filter(|id| !id.is_empty())?;
            Some(Item {
                kind: crate::pagedata::ItemKind::Artist,
                title: items::tidy(&a.name),
                browse_id: id.to_owned(),
                ..Default::default()
            })
        })
        .collect()
}

pub fn artist_names(artists: &[ArtistId]) -> String {
    artists.iter().map(|a| a.name.trim()).filter(|n| !n.is_empty()).collect::<Vec<_>>().join(", ")
}

pub fn album_type_label(t: AlbumType) -> &'static str {
    match t {
        AlbumType::Ep => "EP",
        AlbumType::Single => "Single",
        AlbumType::Audiobook => "Audiobook",
        AlbumType::Show => "Show",
        _ => "Album",
    }
}

/// "1 song" / "12 songs".
pub fn songs_text(n: u64) -> String {
    format!("{n} {}", if n == 1 { "song" } else { "songs" })
}

/// Total play time as "46 min" / "1 hr 5 min" / "2 hr"; `None` when nothing is known.
pub fn duration_text(total_secs: u64) -> Option<String> {
    if total_secs == 0 {
        return None;
    }
    let mins = ((total_secs + 30) / 60).max(1);
    Some(match (mins / 60, mins % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} hr"),
        (h, m) => format!("{h} hr {m} min"),
    })
}

/// Sum of the track durations that are known.
pub fn total_secs(tracks: &[TrackItem]) -> u64 {
    tracks.iter().filter_map(|t| t.duration).map(u64::from).sum()
}

/// Description text for display: line breaks kept (at most one blank line in a row), other control characters
/// dropped, trailing blanks trimmed, and absurd lengths cut on a word boundary.
pub fn clean_description(raw: &str) -> String {
    let normalised = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalised.len().min(MAX_DESCRIPTION + 8));
    let mut newlines = 0;
    for line in normalised.split('\n') {
        let line: String = line.chars().filter(|c| !c.is_control()).collect();
        let line = line.trim();
        if line.is_empty() {
            newlines += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if newlines > 0 { "\n\n" } else { "\n" });
        }
        newlines = 0;
        out.push_str(line);
    }
    excerpt(&out, MAX_DESCRIPTION)
}

pub fn description_of(text: Option<&RichText>) -> String {
    text.map(|t| clean_description(&t.to_plaintext())).unwrap_or_default()
}

/// At most `max` characters, cut at a word boundary and marked with an ellipsis; shorter text is returned as is.
pub fn excerpt(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max).collect();
    // Back up to the last whitespace unless that would throw away most of the text (one huge word).
    let keep = match cut.rfind(char::is_whitespace) {
        Some(i) if cut[..i].chars().count() * 2 >= max => &cut[..i],
        _ => cut.as_str(),
    };
    format!("{}…", keep.trim_end_matches(|c: char| c.is_whitespace() || ",.;:-–—".contains(c)))
}

/// A URL we are willing to put in front of the UI: `https` only (and well-formed), else empty.
pub fn https_only(url: Option<&str>) -> String {
    url.map(str::trim).filter(|u| is_safe_external_link(u)).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration_text(0), None);
        assert_eq!(duration_text(20), Some("1 min".into()));
        assert_eq!(duration_text(46 * 60 + 10), Some("46 min".into()));
        assert_eq!(duration_text(3600), Some("1 hr".into()));
        assert_eq!(duration_text(3600 + 5 * 60), Some("1 hr 5 min".into()));
    }

    #[test]
    fn song_counts_pluralise() {
        assert_eq!(songs_text(1), "1 song");
        assert_eq!(songs_text(12), "12 songs");
    }

    #[test]
    fn excerpts_break_on_words() {
        assert_eq!(excerpt("short", 10), "short");
        assert_eq!(excerpt("one two three four", 12), "one two…");
        // a single endless word is cut hard instead of vanishing
        assert_eq!(excerpt("abcdefghijklmnop", 8), "abcdefgh…");
        // multibyte text is cut on characters
        assert_eq!(excerpt("ééééé ééééé", 7).chars().count(), 6);
    }

    #[test]
    fn descriptions_are_tidied() {
        assert_eq!(clean_description("  A\r\nB\n\n\n\nC \u{0} \n"), "A\nB\n\nC");
        assert_eq!(clean_description(""), "");
        let huge = "word ".repeat(5000);
        let d = clean_description(&huge);
        assert!(d.chars().count() <= MAX_DESCRIPTION + 1 && d.ends_with('…'));
        assert!(!d.contains("wor…"), "cut on a word boundary");
    }

    #[test]
    fn only_https_links_pass() {
        assert_eq!(https_only(Some("https://en.wikipedia.org/wiki/X")), "https://en.wikipedia.org/wiki/X");
        assert_eq!(https_only(Some("http://en.wikipedia.org/wiki/X")), "");
        assert_eq!(https_only(Some("javascript:alert(1)")), "");
        assert_eq!(https_only(Some("file:///etc/passwd")), "");
        assert_eq!(https_only(Some("https://user:pw@evil.example/")), "");
        assert_eq!(https_only(None), "");
    }

    #[test]
    fn artist_links_skip_artists_without_channel() {
        let artists: Vec<ArtistId> =
            serde_json::from_value(json!([{"id": null, "name": "Various"}, {"id": "UC1", "name": "Real"}])).unwrap();
        let links = artist_links(&artists);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].browse_id, "UC1");
        assert_eq!(artist_names(&artists), "Various, Real");
    }
}
