// SPDX-License-Identifier: GPL-3.0-only
//! Artist page: header image, name, subscribers, YouTube's own bio with its Wikipedia link (exactly what YouTube
//! Music shows, no extra lookup), Play / Shuffle / Radio, top songs, then shelves of albums, singles, playlists
//! and similar artists.

use super::common::*;
use crate::errors::AppError;
use crate::items::{subscribers, thumb, tidy, upscale, COVER_BIG_PX};
use crate::pagedata::{Action, Hero, Item, ItemKind, PageData, SectionStyle};
use crate::state::st;
use rustypipe::model::{AlbumType, MusicArtist};

pub async fn load(id: &str) -> Result<PageData, AppError> {
    let q = st().rp.query();
    let artist = super::retry(|| q.music_artist(id, true)).await?;
    Ok(build(&artist))
}

pub fn build(a: &MusicArtist) -> PageData {
    let mut actions = vec![Action::Play, Action::Shuffle];
    let radio = a.radio_id.as_deref().filter(|r| !r.is_empty());
    if radio.is_some() {
        actions.push(Action::Radio);
    }
    let hero = Hero {
        kind: Some(ItemKind::Artist),
        title: tidy(&a.name),
        subtitle: a.subscriber_count.map(subscribers).unwrap_or_default(),
        cover: upscale(&thumb(&a.header_image), COVER_BIG_PX),
        description: a.description.as_deref().map(clean_description).unwrap_or_default(),
        wikipedia_url: https_only(a.wikipedia_url.as_deref()),
        stats: Vec::new(),
        actions,
    };

    let is_single = |t: AlbumType| matches!(t, AlbumType::Single | AlbumType::Ep);
    let albums = |single: bool| a.albums.iter().filter(|x| is_single(x.album_type) == single).filter_map(album_item).collect::<Vec<_>>();
    let radio_item = radio.map(|r| Item {
        kind: ItemKind::Playlist,
        title: format!("{} Radio", tidy(&a.name)),
        browse_id: r.to_owned(),
        ..Default::default()
    });

    let sections = vec![
        section(HERO_RADIO, "", SectionStyle::Cards, radio_item.into_iter().collect()),
        section("top-songs", "Top songs", SectionStyle::List, a.tracks.iter().filter_map(|t| track_item(t, "", 0)).collect()),
        section("albums", "Albums", SectionStyle::Cards, albums(false)),
        section("singles", "Singles & EPs", SectionStyle::Cards, albums(true)),
        section("playlists", "Playlists", SectionStyle::Cards, a.playlists.iter().filter_map(playlist_item).collect()),
        section("similar", "Fans also like", SectionStyle::Artists, a.similar_artists.iter().filter_map(artist_item).collect()),
    ];
    PageData { hero: Some(hero), sections, pager: None, has_more: false }.cleaned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn thumbs(url: &str) -> Value {
        json!([{"url": url, "width": 120, "height": 120}])
    }
    fn track(id: &str) -> Value {
        json!({"id": id, "name": format!("Song {id}"), "duration": null, "cover": thumbs("https://lh3.googleusercontent.com/c=w120-h120"),
               "artists": [{"id": "UC1", "name": "Band"}], "artist_id": "UC1", "album": null, "view_count": null,
               "track_type": "track", "track_nr": null, "by_va": false})
    }
    fn album(id: &str, ty: &str) -> Value {
        json!({"id": id, "name": format!("A {id}"), "cover": thumbs("https://lh3.googleusercontent.com/a=w120-h120"),
               "artists": [], "artist_id": null, "album_type": ty, "year": 2020, "by_va": false})
    }
    fn similar(id: &str) -> Value {
        json!({"id": id, "name": format!("Similar {id}"), "avatar": thumbs("https://lh3.googleusercontent.com/s=w120-h120"), "subscriber_count": 1500})
    }
    fn playlist(id: &str) -> Value {
        json!({"id": id, "name": "PL", "thumbnail": [], "channel": null, "track_count": 5, "from_ytm": true, "is_podcast": false})
    }

    fn artist(extra: Value) -> MusicArtist {
        let mut v = json!({"id": "UC1", "name": "Band", "header_image": thumbs("https://lh3.googleusercontent.com/h=w120-h120"),
            "description": "A band from somewhere.\n\nFormed in 1999.", "wikipedia_url": "https://en.wikipedia.org/wiki/Band",
            "subscriber_count": 1_250_000, "tracks": [track("t1"), track("t2")],
            "albums": [album("MPREb_a", "album"), album("MPREb_s", "single"), album("MPREb_e", "ep"), album("MPREb_b", "album")],
            "playlists": [playlist("PL1")], "similar_artists": [similar("UC2")], "tracks_playlist_id": null,
            "videos_playlist_id": null, "radio_id": "RDEM1"});
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        serde_json::from_value(v).expect("fixture matches RustyPipe's MusicArtist")
    }

    fn ids(p: &PageData) -> Vec<&str> {
        p.sections.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn full_page_has_bio_link_actions_and_shelves_in_order() {
        let p = build(&artist(json!({})));
        let h = p.hero.as_ref().unwrap();
        assert_eq!((h.title.as_str(), h.subtitle.as_str()), ("Band", "1.2M subscribers"));
        assert_eq!(h.description, "A band from somewhere.\n\nFormed in 1999.");
        assert_eq!(h.wikipedia_url, "https://en.wikipedia.org/wiki/Band");
        assert_eq!(h.actions, [Action::Play, Action::Shuffle, Action::Radio]);
        assert!(h.cover.contains("w1200"), "header is requested large: {}", h.cover);
        assert_eq!(ids(&p), [HERO_RADIO, "top-songs", "albums", "singles", "playlists", "similar"]);
        let radio = &p.sections[0].items[0];
        assert_eq!((radio.kind, radio.browse_id.as_str()), (ItemKind::Playlist, "RDEM1"));
    }

    #[test]
    fn albums_and_singles_are_split() {
        let p = build(&artist(json!({})));
        let by = |id: &str| p.sections.iter().find(|s| s.id == id).unwrap().items.iter().map(|i| i.browse_id.clone()).collect::<Vec<_>>();
        assert_eq!(by("albums"), ["MPREb_a", "MPREb_b"]);
        assert_eq!(by("singles"), ["MPREb_s", "MPREb_e"]);
        assert_eq!(p.sections.iter().find(|s| s.id == "similar").unwrap().style, SectionStyle::Artists);
    }

    #[test]
    fn non_https_wikipedia_urls_are_dropped() {
        for bad in ["http://en.wikipedia.org/wiki/Band", "javascript:alert(1)", "//en.wikipedia.org/x", "ftp://x.org/a", ""] {
            let h = build(&artist(json!({ "wikipedia_url": bad }))).hero.unwrap();
            assert_eq!(h.wikipedia_url, "", "{bad:?} must not reach the UI");
        }
        assert_eq!(build(&artist(json!({"wikipedia_url": null}))).hero.unwrap().wikipedia_url, "");
    }

    #[test]
    fn a_huge_description_is_capped_on_a_word_boundary() {
        let desc = "lorem ipsum ".repeat(2000);
        let h = build(&artist(json!({ "description": desc }))).hero.unwrap();
        assert!(h.description.chars().count() <= MAX_DESCRIPTION + 1);
        assert!(h.description.ends_with("ipsum…") || h.description.ends_with("lorem…"), "{}", &h.description[h.description.len() - 20..]);
    }

    #[test]
    fn sparse_artists_still_build() {
        let p = build(&artist(json!({"description": null, "wikipedia_url": null, "subscriber_count": null, "header_image": [],
            "tracks": [], "albums": [], "playlists": [], "similar_artists": [], "radio_id": null})));
        let h = p.hero.unwrap();
        assert_eq!((h.subtitle.as_str(), h.description.as_str(), h.cover.as_str()), ("", "", ""));
        assert_eq!(h.actions, [Action::Play, Action::Shuffle]);
        assert!(p.sections.is_empty());
    }

    #[test]
    fn every_card_has_art() {
        let p = build(&artist(json!({})));
        for s in &p.sections {
            // (the radio item and the cover-less fixture playlist are the exceptions; QML draws a placeholder for those)
            if s.id != HERO_RADIO && s.id != "playlists" {
                assert!(s.items.iter().all(|i| !i.thumbnail.is_empty()), "{}", s.id);
            }
        }
    }
}
