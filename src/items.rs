// SPDX-License-Identifier: GPL-3.0-only
//! Maps RustyPipe model types to the flat JSON items the QML views consume:
//! {title, subtitle, artist, kind, videoId, browseId, thumbnail, duration, section}
use rustypipe::model::{AlbumId, AlbumItem, ArtistId, ArtistItem, MusicPlaylistItem, Thumbnail, TrackItem};
use serde_json::{json, Value};

/// Size cover art is requested at for lists, headers and the player bar.
pub const COVER_PX: u32 = 544;
/// Size for the large Now Playing cover.
pub const COVER_BIG_PX: u32 = 1200;

/// Best offered thumbnail, upscaled to `COVER_PX` where the server allows it.
pub fn thumb(t: &[Thumbnail]) -> String {
    let best = t.iter().max_by_key(|t| t.width.min(600)).map(|t| t.url.as_str()).unwrap_or_default();
    upscale(best, COVER_PX)
}

/// YouTube Music covers come from Google image hosts as `…=w120-h120-l90-rj`; the size lives in the URL and
/// the server renders any size on request. Raise `w`/`h`/`s` size tokens to `px` (never lower them), so a
/// 120 px song cover becomes a sharp one. Other hosts (e.g. `i.ytimg.com` video stills) are left alone.
pub fn upscale(url: &str, px: u32) -> String {
    let google = url.contains("googleusercontent.com") || url.contains("ggpht.com");
    let Some((base, params)) = url.rsplit_once('=').filter(|_| google) else { return url.to_owned() };
    let raised: Vec<String> = params
        .split('-')
        .map(|tok| {
            let (kind, digits) = tok.split_at(tok.chars().next().map_or(0, char::len_utf8));
            match (kind, digits.parse::<u32>()) {
                ("w" | "h" | "s", Ok(n)) if !digits.is_empty() => format!("{kind}{}", n.max(px)),
                _ => tok.to_owned(),
            }
        })
        .collect();
    format!("{base}={}", raised.join("-"))
}

pub fn fmt_dur(s: u32) -> String {
    format!("{}:{:02}", s / 60, s % 60)
}

/// Inverse of [`fmt_dur`] for the `m:ss` strings in queue items; 0 when unknown. Mirrors `seconds()` in
/// NowPlayingPage.qml so a prefetch and the page produce the same lyrics cache key.
pub fn parse_dur(s: &str) -> u32 {
    match s.split_once(':') {
        Some((m, sec)) => match (m.trim().parse::<u32>(), sec.trim().parse::<u32>()) {
            (Ok(m), Ok(sec)) => m * 60 + sec,
            _ => 0,
        },
        None => 0,
    }
}

#[allow(clippy::too_many_arguments)] // flat JSON record: the fields are the schema the QML expects
fn item(
    title: &str,
    subtitle: String,
    artist: String,
    kind: &str,
    vid: &str,
    browse: &str,
    thumb: String,
    dur: String,
    section: &str,
) -> Value {
    json!({"title": title, "subtitle": subtitle, "artist": artist, "kind": kind, "videoId": vid,
           "browseId": browse, "thumbnail": thumb, "duration": dur, "section": section})
}

pub fn track(t: &TrackItem, section: &str, fallback_thumb: &str) -> Value {
    let artists = t.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>();
    let cover = if t.cover.is_empty() { fallback_thumb.to_owned() } else { thumb(&t.cover) };
    let (artist_id, album_id) = link_ids(&t.artists, t.album.as_ref());
    let mut v = item(
        &t.name,
        artists.join(", "),
        artists.first().copied().unwrap_or("").to_owned(),
        "song",
        &t.id,
        "",
        cover,
        t.duration.map(fmt_dur).unwrap_or_default(),
        section,
    );
    v["artistId"] = json!(artist_id);
    v["albumId"] = json!(album_id);
    v
}

/// Browse ids for "go to artist / album": the first artist that has a channel id, and the album id.
/// Empty strings when unknown, so QML can just test for truthiness.
pub fn link_ids(artists: &[ArtistId], album: Option<&AlbumId>) -> (String, String) {
    let artist = artists.iter().find_map(|a| a.id.clone()).unwrap_or_default();
    (artist, album.map(|a| a.id.clone()).unwrap_or_default())
}

pub fn album(a: &AlbumItem, section: &str) -> Value {
    let mut sub = vec![format!("{:?}", a.album_type)];
    sub.extend(a.artists.first().map(|x| x.name.clone()));
    sub.extend(a.year.map(|y| y.to_string()));
    item(&a.name, sub.join(" • "), String::new(), "album", "", &a.id, thumb(&a.cover), String::new(), section)
}

/// "1.2M subscribers", "35K subscribers", "812 subscribers".
pub fn subscribers(n: u64) -> String {
    match n {
        1_000_000.. => format!("{:.1}M subscribers", n as f64 / 1e6),
        1_000.. => format!("{}K subscribers", n / 1000),
        _ => format!("{n} subscribers"),
    }
}

pub fn artist(a: &ArtistItem, section: &str) -> Value {
    let sub = match a.subscriber_count {
        Some(n) => format!("Artist • {}", subscribers(n)),
        None => "Artist".into(),
    };
    item(&a.name, sub, String::new(), "artist", "", &a.id, thumb(&a.avatar), String::new(), section)
}

pub fn playlist(p: &MusicPlaylistItem, section: &str) -> Value {
    let mut sub = vec!["Playlist".to_owned()];
    sub.extend(p.channel.as_ref().map(|c| c.name.clone()));
    sub.extend(p.track_count.map(|n| format!("{n} songs")));
    item(&p.name, sub.join(" • "), String::new(), "playlist", "", &p.id, thumb(&p.thumbnail), String::new(), section)
}

const MAX_TEXT: usize = 300;

/// Tidy one text field: control characters and line breaks become spaces, runs of spaces collapse,
/// absurdly long values are cut. YouTube text occasionally carries stray newlines or invisible junk.
pub fn tidy(text: &str) -> String {
    let flat: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > MAX_TEXT {
        let cut: String = flat.chars().take(MAX_TEXT).collect();
        format!("{}…", cut.trim_end())
    } else {
        flat
    }
}

/// Make one list entry safe to show, or drop it. A song with no video id can't be played and any other entry with
/// no browse id can't be opened, so those are skipped rather than rendered as dead rows; a missing title becomes a
/// placeholder instead of a blank line; every field the UI reads is guaranteed to exist as a string.
pub fn clean(mut v: Value) -> Option<Value> {
    let kind = v["kind"].as_str().unwrap_or("").to_owned();
    let playable = if kind == "song" {
        v["videoId"].as_str().is_some_and(|s| !s.is_empty())
    } else {
        v["browseId"].as_str().is_some_and(|s| !s.is_empty())
    };
    if !playable {
        return None;
    }
    for key in ["title", "subtitle", "artist", "section", "duration"] {
        let t = tidy(v[key].as_str().unwrap_or(""));
        v[key] = json!(t);
    }
    if v["title"].as_str() == Some("") {
        v["title"] = json!(if kind == "song" { "Unknown title" } else { "Untitled" });
    }
    for key in ["videoId", "browseId", "thumbnail", "artistId", "albumId"] {
        if !v[key].is_string() {
            v[key] = json!("");
        }
    }
    Some(v)
}

pub fn clean_all(items: Vec<Value>) -> Vec<Value> {
    items.into_iter().filter_map(clean).collect()
}

pub fn page(title: &str, subtitle: &str, thumbnail: &str, items: Vec<Value>) -> Value {
    json!({"title": tidy(title), "subtitle": tidy(subtitle), "thumbnail": thumbnail, "items": clean_all(items)})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artist(id: Option<&str>, name: &str) -> ArtistId {
        serde_json::from_value(json!({"id": id, "name": name})).unwrap()
    }

    #[test]
    fn upscales_google_cover_urls() {
        let u = "https://yt3.googleusercontent.com/abc-DEF_x=w120-h120-l90-rj";
        assert_eq!(upscale(u, 544), "https://yt3.googleusercontent.com/abc-DEF_x=w544-h544-l90-rj");
        // centred artist crop keeps its `p` flag
        assert_eq!(
            upscale("https://lh3.googleusercontent.com/q=w120-h120-p-l90-rj", 544),
            "https://lh3.googleusercontent.com/q=w544-h544-p-l90-rj"
        );
        // square-size form
        assert_eq!(upscale("https://yt3.googleusercontent.com/z=s88-c-k", 544), "https://yt3.googleusercontent.com/z=s544-c-k");
    }

    #[test]
    fn upscale_never_shrinks_and_leaves_other_hosts_alone() {
        assert_eq!(
            upscale("https://yt3.googleusercontent.com/a=w1200-h1200-l90-rj", 544),
            "https://yt3.googleusercontent.com/a=w1200-h1200-l90-rj"
        );
        let vid = "https://i.ytimg.com/vi/BCHJ9CxvDek/hq720.jpg?sqp=-oaymwEKCKAGEMIDIABIWg&rs=AMzJL3ltMSAd6SijGWCdpnJOVCRzOpvzwg";
        assert_eq!(upscale(vid, 544), vid);
        // no size params at all, or an empty / odd url
        assert_eq!(upscale("https://yt3.googleusercontent.com/plain", 544), "https://yt3.googleusercontent.com/plain");
        assert_eq!(upscale("", 544), "");
        // tokens that merely start with w/h/s but are not sizes stay put
        assert_eq!(
            upscale("https://lh3.googleusercontent.com/a=w300-h300-no-rw", 544),
            "https://lh3.googleusercontent.com/a=w544-h544-no-rw"
        );
    }

    fn song(title: &str, vid: &str) -> Value {
        json!({"title": title, "subtitle": "", "artist": "", "kind": "song", "videoId": vid, "browseId": "", "thumbnail": "", "duration": "", "section": ""})
    }

    #[test]
    fn tidy_flattens_whitespace_and_control_chars() {
        assert_eq!(tidy("  Hello\n\tWorld \u{0}  "), "Hello World");
        assert_eq!(tidy(""), "");
        assert_eq!(tidy("a\u{200b}b"), "a\u{200b}b"); // zero-width space is not a control char: left alone
    }

    #[test]
    fn tidy_cuts_absurdly_long_text_on_a_char_boundary() {
        let long = "é".repeat(500);
        let t = tidy(&long);
        assert_eq!(t.chars().count(), 301); // 300 + the ellipsis
        assert!(t.ends_with('…'));
    }

    #[test]
    fn songs_without_an_id_are_dropped_and_blank_titles_replaced() {
        assert!(clean(song("x", "")).is_none());
        assert_eq!(clean(song("   ", "abc")).unwrap()["title"], "Unknown title");
        assert_eq!(clean(song("Fine", "abc")).unwrap()["title"], "Fine");
    }

    #[test]
    fn non_songs_need_a_browse_id() {
        let mut album = song("A", "");
        album["kind"] = json!("album");
        assert!(clean(album.clone()).is_none());
        album["browseId"] = json!("MPREb_1");
        assert!(clean(album.clone()).is_some());
        album["title"] = json!("");
        assert_eq!(clean(album).unwrap()["title"], "Untitled");
    }

    #[test]
    fn missing_or_wrongly_typed_fields_become_empty_strings() {
        let broken = json!({"kind": "song", "videoId": "abc", "title": null, "thumbnail": 5});
        let c = clean(broken).unwrap();
        assert_eq!(c["thumbnail"], "");
        assert_eq!(c["artistId"], "");
        assert_eq!(c["subtitle"], "");
        assert_eq!(c["title"], "Unknown title");
    }

    #[test]
    fn page_filters_bad_entries_but_keeps_good_ones() {
        let p = page("  Title \n", "", "", vec![song("one", "a"), song("bad", ""), song("two", "b")]);
        assert_eq!(p["title"], "Title");
        let titles: Vec<&str> = p["items"].as_array().unwrap().iter().map(|i| i["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["one", "two"]);
    }

    #[test]
    fn subscriber_counts_are_abbreviated() {
        assert_eq!(subscribers(812), "812 subscribers");
        assert_eq!(subscribers(35_400), "35K subscribers");
        assert_eq!(subscribers(1_300_000), "1.3M subscribers");
    }

    #[test]
    fn durations_format_as_m_ss() {
        assert_eq!(fmt_dur(0), "0:00");
        assert_eq!(fmt_dur(59), "0:59");
        assert_eq!(fmt_dur(61), "1:01");
        assert_eq!(fmt_dur(3599), "59:59");
        assert_eq!(fmt_dur(3600), "60:00"); // no hours: matches the player bar
    }

    #[test]
    fn link_ids_skip_artists_without_a_channel() {
        let artists = [artist(None, "Various"), artist(Some("UC123"), "Real Artist")];
        let album: AlbumId = serde_json::from_value(json!({"id": "MPREb_x", "name": "A"})).unwrap();
        assert_eq!(link_ids(&artists, Some(&album)), ("UC123".into(), "MPREb_x".into()));
    }

    #[test]
    fn link_ids_are_empty_when_unknown() {
        assert_eq!(link_ids(&[], None), (String::new(), String::new()));
        assert_eq!(link_ids(&[artist(None, "x")], None).0, "");
    }
}
