// SPDX-License-Identifier: GPL-3.0-only
//! Album page: hero (cover, artists, year • songs • minutes, description), numbered tracks, other versions and
//! more by the same artist.

use super::common::*;
use crate::errors::AppError;
use crate::items::{thumb, tidy};
use crate::pagedata::{Action, Hero, ItemKind, PageData, SectionStyle, Stat};
use crate::state::st;
use rustypipe::model::{AlbumItem, MusicAlbum};

const MORE_BY_LIMIT: usize = 12;

pub async fn load(id: &str) -> Result<PageData, AppError> {
    let q = st().rp.query();
    let album = super::retry(|| q.music_album(id)).await?;
    // The artist's other releases are a bonus: any failure just leaves the shelf out.
    let more_by = match album.artist_id.as_deref().filter(|a| !a.is_empty()) {
        Some(artist) => q.music_artist(artist, false).await.map(|a| a.albums).unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(build(&album, &more_by))
}

/// Everything on the page except the network. `more_by` is the artist's discography (the album itself is filtered out).
pub fn build(a: &MusicAlbum, more_by: &[AlbumItem]) -> PageData {
    let cover = thumb(&a.cover);
    let by = artist_names(&a.artists);
    let count = if a.tracks.is_empty() { u64::from(a.track_count) } else { a.tracks.len() as u64 };

    let mut stats = vec![Stat { text: album_type_label(a.album_type).to_owned(), icon: String::new() }];
    stats.extend(a.year.map(|y| Stat { text: y.to_string(), icon: String::new() }));
    if count > 0 {
        stats.push(Stat { text: songs_text(count), icon: String::new() });
    }
    stats.extend(duration_text(total_secs(&a.tracks)).map(|text| Stat { text, icon: String::new() }));

    let hero = Hero {
        kind: Some(ItemKind::Album),
        title: tidy(&a.name),
        subtitle: by,
        cover: cover.clone(),
        description: description_of(a.description.as_ref()),
        wikipedia_url: String::new(),
        stats,
        actions: vec![Action::Play, Action::Shuffle, Action::Download, Action::AddToQueue],
    };

    let mut sections = vec![
        section(HERO_ARTISTS, "", SectionStyle::Artists, artist_links(&a.artists)),
        section("tracks", "", SectionStyle::NumberedList, numbered_tracks(&a.tracks, &cover, 0)),
        section("variants", "Other versions", SectionStyle::Cards, a.variants.iter().filter_map(album_item).collect()),
    ];
    let artist_name = a.artists.first().map(|x| x.name.trim()).filter(|n| !n.is_empty());
    sections.push(section(
        "more-by",
        &artist_name.map(|n| format!("More by {n}")).unwrap_or_else(|| "More by this artist".into()),
        SectionStyle::Cards,
        more_by
            .iter()
            .filter(|x| x.id != a.id && !a.variants.iter().any(|v| v.id == x.id))
            .filter_map(album_item)
            .take(MORE_BY_LIMIT)
            .collect(),
    ));
    PageData { hero: Some(hero), sections, pager: None, has_more: false }.cleaned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn thumbs() -> Value {
        json!([{"url": "https://lh3.googleusercontent.com/c=w120-h120-l90-rj", "width": 120, "height": 120}])
    }

    fn track(id: &str, dur: Option<u32>) -> Value {
        json!({"id": id, "name": format!("Song {id}"), "duration": dur, "cover": [], "artists": [{"id": "UC1", "name": "Band"}],
               "artist_id": "UC1", "album": null, "view_count": null, "track_type": "track", "track_nr": null, "by_va": false})
    }

    fn album_item(id: &str) -> Value {
        json!({"id": id, "name": format!("Album {id}"), "cover": thumbs(), "artists": [{"id": "UC1", "name": "Band"}],
               "artist_id": "UC1", "album_type": "album", "year": 2018, "by_va": false})
    }

    fn album(extra: Value) -> MusicAlbum {
        let mut v = json!({"id": "MPREb_1", "playlist_id": "OLAK5", "name": "The Album", "cover": thumbs(),
            "artists": [{"id": "UC1", "name": "Band"}, {"id": null, "name": "Guest"}], "artist_id": "UC1",
            "description": null, "album_type": "album", "year": 2019, "by_va": false, "track_count": 3,
            "tracks": [track("a", Some(200)), track("b", Some(300)), track("c", Some(2200))], "variants": []});
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        serde_json::from_value(v).unwrap()
    }

    fn items_of(id: &str, p: &PageData) -> usize {
        p.sections.iter().find(|s| s.id == id).map_or(0, |s| s.items.len())
    }

    #[test]
    fn hero_summarises_year_songs_and_total_time() {
        let p = build(&album(json!({})), &[]);
        let h = p.hero.unwrap();
        let stats: Vec<&str> = h.stats.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(stats, ["Album", "2019", "3 songs", "45 min"]);
        assert_eq!((h.title.as_str(), h.subtitle.as_str()), ("The Album", "Band, Guest"));
        assert!(h.cover.contains("w544-h544"), "cover is upscaled: {}", h.cover);
        assert_eq!(h.actions, [Action::Play, Action::Shuffle, Action::Download, Action::AddToQueue]);
    }

    #[test]
    fn tracks_are_numbered_from_one_and_get_the_album_cover() {
        let p = build(&album(json!({})), &[]);
        let tracks = &p.sections.iter().find(|s| s.id == "tracks").unwrap();
        assert_eq!(tracks.style, SectionStyle::NumberedList);
        assert_eq!(tracks.items.iter().map(|i| i.number).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(tracks.items.iter().all(|i| !i.thumbnail.is_empty()), "every row has art");
        assert_eq!(tracks.items[0].duration, "3:20");
    }

    #[test]
    fn missing_year_description_cover_and_durations_are_fine() {
        let p = build(
            &album(json!({"year": null, "cover": [], "tracks": [track("a", None)], "track_count": 0, "artists": [], "artist_id": null})),
            &[],
        );
        let h = p.hero.clone().unwrap();
        let stats: Vec<&str> = h.stats.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(stats, ["Album", "1 song"], "no year, no minutes");
        assert_eq!((h.cover.as_str(), h.description.as_str(), h.subtitle.as_str()), ("", "", ""));
        assert_eq!(items_of("tracks", &p), 1);
        assert_eq!(items_of(HERO_ARTISTS, &p), 0);
    }

    #[test]
    fn track_count_is_used_when_the_track_list_is_empty() {
        let p = build(&album(json!({"tracks": [], "track_count": 9})), &[]);
        assert!(p.hero.unwrap().stats.iter().any(|s| s.text == "9 songs"));
    }

    #[test]
    fn artist_links_skip_artists_without_a_channel() {
        let p = build(&album(json!({})), &[]);
        let links = p.sections.iter().find(|s| s.id == HERO_ARTISTS).unwrap();
        assert_eq!(links.items.iter().map(|i| i.browse_id.as_str()).collect::<Vec<_>>(), ["UC1"]);
    }

    #[test]
    fn more_by_excludes_this_album_and_variants_and_follows_the_tracks() {
        let rows: Vec<AlbumItem> =
            ["MPREb_1", "MPREb_2", "MPREb_v", "MPREb_3"].iter().map(|id| serde_json::from_value(album_item(id)).unwrap()).collect();
        let p = build(&album(json!({"variants": [album_item("MPREb_v")]})), &rows);
        let ids: Vec<&str> = p.sections.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, [HERO_ARTISTS, "tracks", "variants", "more-by"]);
        let more = p.sections.last().unwrap();
        assert_eq!(more.title, "More by Band");
        assert_eq!(more.items.iter().map(|i| i.browse_id.as_str()).collect::<Vec<_>>(), ["MPREb_2", "MPREb_3"]);
    }

    #[test]
    fn empty_shelves_do_not_appear() {
        let p = build(&album(json!({})), &[]);
        assert_eq!(p.sections.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), [HERO_ARTISTS, "tracks"]);
    }

    #[test]
    fn rich_text_description_becomes_plain_text() {
        let desc = json!([{"Text": {"text": "Released in 2019.\r\n\r\n\r\nSecond paragraph.", "style": {}}}]);
        let mut v = serde_json::to_value(album(json!({}))).unwrap();
        v["description"] = desc;
        let a: MusicAlbum = serde_json::from_value(v).expect("RichText fixture matches RustyPipe's serde shape");
        let h = build(&a, &[]).hero.unwrap();
        assert_eq!(h.description, "Released in 2019.\n\nSecond paragraph.");
    }
}
