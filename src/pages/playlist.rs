// SPDX-License-Identifier: GPL-3.0-only
//! Playlist page: hero (cover, owner, song count, description), numbered tracks that keep loading as you scroll,
//! and related playlists. Liked music (`LIKED`) keeps going through the legacy loader (it has its own fallbacks)
//! but gets the same hero actions.

use super::common::*;
use crate::errors::AppError;
use crate::items::{thumb, tidy};
use crate::pagedata::{Action, Hero, ItemKind, PageData, SectionStyle, Stat};
use crate::state::st;
use rustypipe::model::MusicPlaylist;

pub async fn load(id: &str) -> Result<PageData, AppError> {
    let q = st().rp.query();
    let playlist = super::retry(|| q.music_playlist(id.trim_start_matches("VL"))).await?;
    let mut page = build(&playlist);
    if page.has_more {
        let cover = thumb(&playlist.thumbnail);
        let shown = playlist.tracks.items.len();
        page.pager = Some(super::register_tracks(playlist.tracks, cover, shown));
    }
    Ok(page)
}

pub fn actions() -> Vec<Action> {
    vec![Action::Play, Action::Shuffle, Action::Download, Action::AddToQueue]
}

/// Everything on the page except the network and the pager id (the caller registers the continuation).
pub fn build(p: &MusicPlaylist) -> PageData {
    let cover = thumb(&p.thumbnail);
    let has_more = p.tracks.ctoken.is_some();
    let shown = p.tracks.items.len() as u64;
    let count = p.track_count.or(p.tracks.count).unwrap_or(shown).max(shown);

    let mut stats = vec![Stat { text: "Playlist".into(), icon: String::new() }];
    if count > 0 {
        stats.push(Stat { text: songs_text(count), icon: String::new() });
    }
    // Only a complete list has a meaningful total.
    if !has_more {
        stats.extend(duration_text(total_secs(&p.tracks.items)).map(|text| Stat { text, icon: String::new() }));
    }
    let hero = Hero {
        kind: Some(ItemKind::Playlist),
        title: tidy(&p.name),
        subtitle: p.channel.as_ref().map(|c| tidy(&c.name)).unwrap_or_default(),
        cover: cover.clone(),
        description: description_of(p.description.as_ref()),
        wikipedia_url: String::new(),
        stats,
        actions: actions(),
    };
    let sections = vec![
        section("tracks", "", SectionStyle::NumberedList, numbered_tracks(&p.tracks.items, &cover, 0)),
        section("related", "Related playlists", SectionStyle::Cards, p.related_playlists.items.iter().filter_map(playlist_item).collect()),
    ];
    PageData { hero: Some(hero), sections, pager: None, has_more }.cleaned()
}

/// Give a legacy-loaded collection (Liked music) the playlist hero actions and numbered rows.
pub fn decorate_legacy(mut page: PageData) -> PageData {
    if let Some(h) = &mut page.hero {
        h.kind = Some(ItemKind::Playlist);
        h.actions = actions();
    }
    let mut n = 0;
    for s in &mut page.sections {
        s.style = SectionStyle::NumberedList;
        for i in &mut s.items {
            n += 1;
            i.number = n;
        }
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn section_of(id: &str, items: Vec<crate::pagedata::Item>) -> crate::pagedata::Section {
        section(id, "", SectionStyle::List, items)
    }

    fn track(id: &str, dur: Option<u32>) -> Value {
        json!({"id": id, "name": format!("Song {id}"), "duration": dur,
               "cover": [{"url": "https://lh3.googleusercontent.com/c=w120-h120-l90-rj", "width": 120, "height": 120}],
               "artists": [{"id": "UC1", "name": "Band"}], "artist_id": "UC1", "album": null, "view_count": null,
               "track_type": "track", "track_nr": null, "by_va": false})
    }

    fn related(id: &str) -> Value {
        json!({"id": id, "name": "Related", "thumbnail": [], "channel": null, "track_count": 10, "from_ytm": true, "is_podcast": false})
    }

    fn playlist(extra: Value) -> MusicPlaylist {
        let mut v = json!({"id": "PL1", "name": "Road trip", "thumbnail": [], "channel": {"id": "UC9", "name": "Me"},
            "description": null, "track_count": 2, "from_ytm": false,
            "tracks": {"count": 2, "items": [track("a", Some(120)), track("b", Some(181))], "ctoken": null, "endpoint": "music_browse"},
            "related_playlists": {"count": null, "items": [], "ctoken": null, "endpoint": "music_browse"}});
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        serde_json::from_value(v).expect("fixture matches RustyPipe's MusicPlaylist")
    }

    #[test]
    fn hero_has_owner_count_and_duration() {
        let p = build(&playlist(json!({})));
        let h = p.hero.unwrap();
        assert_eq!((h.title.as_str(), h.subtitle.as_str()), ("Road trip", "Me"));
        assert_eq!(h.stats.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["Playlist", "2 songs", "5 min"]);
        assert_eq!(h.actions, actions());
        assert!(!p.has_more);
    }

    #[test]
    fn rows_are_numbered_and_never_art_less_when_the_playlist_has_a_cover() {
        let p = build(&playlist(json!({"thumbnail": [{"url": "https://lh3.googleusercontent.com/p=w60-h60", "width": 60, "height": 60}]})));
        let tracks = &p.sections[0];
        assert_eq!(tracks.id, "tracks");
        assert_eq!(tracks.items.iter().map(|i| i.number).collect::<Vec<_>>(), [1, 2]);
        assert!(tracks.items.iter().all(|i| !i.thumbnail.is_empty()));
    }

    #[test]
    fn a_partial_list_has_more_and_no_total_time() {
        let mut v = serde_json::to_value(playlist(json!({"track_count": 120}))).unwrap();
        v["tracks"]["ctoken"] = json!("TOKEN");
        let p = build(&serde_json::from_value(v).unwrap());
        assert!(p.has_more);
        let h = p.hero.unwrap();
        assert_eq!(h.stats.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["Playlist", "120 songs"]);
    }

    #[test]
    fn related_playlists_form_a_card_shelf_after_the_tracks() {
        let mut v = serde_json::to_value(playlist(json!({}))).unwrap();
        v["related_playlists"]["items"] = json!([related("RP1"), related("")]);
        let p = build(&serde_json::from_value(v).unwrap());
        assert_eq!(p.sections.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["tracks", "related"]);
        assert_eq!(p.sections[1].items.len(), 1, "a playlist without an id is dropped");
        assert_eq!(p.sections[1].style, SectionStyle::Cards);
    }

    #[test]
    fn missing_everything_still_builds() {
        let p = build(&playlist(json!({"channel": null, "track_count": null, "thumbnail": [],
            "tracks": {"count": null, "items": [track("a", None)], "ctoken": null, "endpoint": "music_browse"}})));
        let h = p.hero.unwrap();
        assert_eq!(h.subtitle, "");
        assert_eq!(h.stats.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["Playlist", "1 song"]);
    }

    #[test]
    fn legacy_collections_get_actions_and_continuous_numbers() {
        let song = |id: &str, section: &str| section_of(section, vec![crate::pagedata::Item { video_id: id.into(), ..Default::default() }]);
        let page = PageData {
            hero: Some(Hero { title: "Liked music".into(), ..Default::default() }),
            sections: vec![song("a", "x"), song("b", "y")],
            ..Default::default()
        };
        let d = decorate_legacy(page);
        assert_eq!(d.hero.unwrap().actions, actions());
        assert_eq!(d.sections.iter().flat_map(|s| s.items.iter().map(|i| i.number)).collect::<Vec<_>>(), [1, 2]);
    }
}
