// SPDX-License-Identifier: GPL-3.0-only
//! YouTube Music's own home feed (`FEmusic_home`), the personalised shelves the official clients show: Quick picks,
//! Listen again, Mixed for you, Similar to … RustyPipe has no call for it, so the answer of the signed-in browse
//! request (`ytapi::home_feed`) is read here into the flat JSON items of `items.rs`, one `section` per shelf.
//!
//! The layout is undocumented and changes: anything not recognised is skipped, never an error.
use crate::items::{clean_all, upscale, COVER_PX};
use serde_json::{json, Value};

/// The items of every shelf on one page of the feed, and the token of the next page if there is one.
pub fn parse(resp: &Value) -> (Vec<Value>, Option<String>) {
    let first = &resp["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"];
    let list = if first.is_object() { first } else { &resp["continuationContents"]["sectionListContinuation"] };
    let mut items = Vec::new();
    for shelf in list["contents"].as_array().into_iter().flatten() {
        let shelf = &shelf["musicCarouselShelfRenderer"];
        let title = shelf_title(&shelf["header"]["musicCarouselShelfBasicHeaderRenderer"]);
        if title.is_empty() {
            continue;
        }
        for entry in shelf["contents"].as_array().into_iter().flatten() {
            let item =
                two_row(&entry["musicTwoRowItemRenderer"], &title).or_else(|| list_row(&entry["musicResponsiveListItemRenderer"], &title));
            items.extend(item);
        }
    }
    let next = list["continuations"][0]["nextContinuationData"]["continuation"].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    (clean_all(items), next)
}

/// The visitor id YouTube answered with. A next page is only served to the visitor the token was made for: asked
/// without it, YouTube answers an empty page.
pub fn visitor(resp: &Value) -> &str {
    resp["responseContext"]["visitorData"].as_str().unwrap_or("")
}

/// Whether YouTube says outright that it answered a signed-out visitor: the cookies were sent, but the session is no
/// longer accepted. The feed is then the anonymous one. Unknown (the marker is missing) counts as signed in.
pub fn answered_signed_out(resp: &Value) -> bool {
    let services = resp["responseContext"]["serviceTrackingParams"].as_array().into_iter().flatten();
    let mut params = services.flat_map(|s| s["params"].as_array().into_iter().flatten());
    params.any(|p| p["key"] == "logged_in" && p["value"] == "0")
}

/// The text of a `{runs: [{text}, …]}` value.
fn text(v: &Value) -> String {
    v["runs"].as_array().into_iter().flatten().filter_map(|r| r["text"].as_str()).collect()
}

/// "Listen again", or "Similar to Some Artist" when the shelf is about something: there the title is a link to it
/// and the line above it ("SIMILAR TO") says how. Above a plain title that line is only the account's name.
fn shelf_title(header: &Value) -> String {
    let title = text(&header["title"]);
    // "About something" = the title links to an artist, an album or a playlist. "Listen again" is a link as well (to
    // the listening history), and the line above it is the account's name: that gave "Bybrooklyn Listen again".
    let about_something = matches!(
        page_type(&header["title"]["runs"][0]["navigationEndpoint"]),
        "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL" | "MUSIC_PAGE_TYPE_ALBUM" | "MUSIC_PAGE_TYPE_PLAYLIST"
    );
    let strapline = text(&header["strapline"]);
    if about_something && !strapline.trim().is_empty() && !title.is_empty() {
        let mut chars = strapline.trim().chars();
        let lead: String = chars.next().into_iter().flat_map(char::to_uppercase).chain(chars.flat_map(char::to_lowercase)).collect();
        format!("{lead} {title}")
    } else {
        title
    }
}

/// The page type of a browse link: `MUSIC_PAGE_TYPE_ALBUM`, `…_ARTIST`, `…_PLAYLIST`, …
fn page_type(endpoint: &Value) -> &str {
    endpoint["browseEndpoint"]["browseEndpointContextSupportedConfigs"]["browseEndpointContextMusicConfig"]["pageType"]
        .as_str()
        .unwrap_or("")
}

fn browse_id(endpoint: &Value) -> &str {
    endpoint["browseEndpoint"]["browseId"].as_str().unwrap_or("")
}

/// The largest offered thumbnail, at cover size where the server renders any size.
fn thumbnail(renderer: &Value) -> String {
    let best = renderer["musicThumbnailRenderer"]["thumbnail"]["thumbnails"]
        .as_array()
        .into_iter()
        .flatten()
        .max_by_key(|t| t["width"].as_u64().unwrap_or(0))
        .and_then(|t| t["url"].as_str())
        .filter(|u| u.starts_with("https://"))
        .unwrap_or("");
    upscale(best, COVER_PX)
}

/// The names of the artists linked in some runs, the id of the first, and the id of a linked album.
fn links(runs: &Value) -> (Vec<String>, String, String) {
    let (mut names, mut artist_id, mut album_id) = (Vec::new(), String::new(), String::new());
    for run in runs["runs"].as_array().into_iter().flatten() {
        let endpoint = &run["navigationEndpoint"];
        match page_type(endpoint) {
            "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL" => {
                names.extend(run["text"].as_str().map(str::to_owned));
                if artist_id.is_empty() {
                    artist_id = browse_id(endpoint).to_owned();
                }
            }
            "MUSIC_PAGE_TYPE_ALBUM" if album_id.is_empty() => album_id = browse_id(endpoint).to_owned(),
            _ => {}
        }
    }
    (names, artist_id, album_id)
}

fn song(
    title: String,
    video_id: &str,
    thumb: String,
    section: &str,
    by: (Vec<String>, String, String),
    fallback_subtitle: String,
) -> Value {
    let (names, artist_id, album_id) = by;
    let subtitle = if names.is_empty() { fallback_subtitle } else { names.join(", ") };
    json!({"title": title, "subtitle": subtitle, "artist": names.first().cloned().unwrap_or_default(), "kind": "song",
           "videoId": video_id, "browseId": "", "thumbnail": thumb, "duration": "", "section": section,
           "artistId": artist_id, "albumId": album_id})
}

/// A card: an album, a playlist or mix, an artist, or a song / video.
fn two_row(r: &Value, section: &str) -> Option<Value> {
    if !r.is_object() {
        return None;
    }
    let (title, subtitle, thumb) = (text(&r["title"]), text(&r["subtitle"]), thumbnail(&r["thumbnailRenderer"]));
    let endpoint = &r["navigationEndpoint"];
    if let Some(video_id) = endpoint["watchEndpoint"]["videoId"].as_str() {
        return Some(song(title, video_id, thumb, section, links(&r["subtitle"]), subtitle));
    }
    let id = browse_id(endpoint);
    let (kind, id) = match page_type(endpoint) {
        "MUSIC_PAGE_TYPE_ALBUM" => ("album", id),
        "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL" => ("artist", id),
        // the rest of the app names a playlist by its id, without the browse prefix
        "MUSIC_PAGE_TYPE_PLAYLIST" => ("playlist", id.strip_prefix("VL").unwrap_or(id)),
        _ => return None,
    };
    Some(json!({"title": title, "subtitle": subtitle, "artist": "", "kind": kind, "videoId": "", "browseId": id,
                "thumbnail": thumb, "duration": "", "section": section}))
}

/// A song row, as in Quick picks: title, artists and album in columns.
fn list_row(r: &Value, section: &str) -> Option<Value> {
    if !r.is_object() {
        return None;
    }
    let column = |n: usize| &r["flexColumns"][n]["musicResponsiveListItemFlexColumnRenderer"]["text"];
    let video_id = r["playlistItemData"]["videoId"]
        .as_str()
        .or_else(|| column(0)["runs"][0]["navigationEndpoint"]["watchEndpoint"]["videoId"].as_str())?;
    let (names, artist_id, _) = links(column(1));
    let album_id = links(column(2)).2;
    let by = (names, artist_id, if album_id.is_empty() { links(column(1)).2 } else { album_id });
    Some(song(text(column(0)), video_id, thumbnail(&r["thumbnail"]), section, by, text(column(1))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browse(id: &str, page: &str) -> Value {
        json!({"browseEndpoint": {"browseId": id, "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": page}}}})
    }

    fn thumbs() -> Value {
        json!({"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [
            {"url": "https://lh3.googleusercontent.com/a=w60-h60-l90-rj", "width": 60, "height": 60},
            {"url": "https://lh3.googleusercontent.com/a=w226-h226-l90-rj", "width": 226, "height": 226}]}}})
    }

    fn card(title: &str, subtitle: Value, endpoint: Value) -> Value {
        json!({"musicTwoRowItemRenderer": {"title": {"runs": [{"text": title}]}, "subtitle": {"runs": subtitle},
               "thumbnailRenderer": thumbs(), "navigationEndpoint": endpoint}})
    }

    fn shelf(header: Value, contents: Value) -> Value {
        json!({"musicCarouselShelfRenderer": {"header": {"musicCarouselShelfBasicHeaderRenderer": header}, "contents": contents}})
    }

    fn page(shelves: Value, next: Option<&str>) -> Value {
        let mut list = json!({"contents": shelves});
        if let Some(token) = next {
            list["continuations"] = json!([{"nextContinuationData": {"continuation": token}}]);
        }
        json!({"contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {"content": {"sectionListRenderer": list}}}]}}})
    }

    #[test]
    fn cards_become_albums_playlists_artists_and_songs() {
        let resp = page(
            json!([shelf(
                json!({"title": {"runs": [{"text": "Listen again"}]}, "strapline": {"runs": [{"text": "BROOK"}]}}),
                json!([
                    card(
                        "An Album",
                        json!([{"text": "Album"}, {"text": " • "}, {"text": "Someone"}]),
                        browse("MPREb_1", "MUSIC_PAGE_TYPE_ALBUM")
                    ),
                    card("My Mix", json!([{"text": "Mix"}]), browse("VLRDCLAK5uy_x", "MUSIC_PAGE_TYPE_PLAYLIST")),
                    card("Someone", json!([{"text": "1M subscribers"}]), browse("UC1", "MUSIC_PAGE_TYPE_ARTIST")),
                    card(
                        "A Video",
                        json!([{"text": "Someone", "navigationEndpoint": browse("UC1", "MUSIC_PAGE_TYPE_ARTIST")}, {"text": " • 2M views"}]),
                        json!({"watchEndpoint": {"videoId": "vid1"}})
                    ),
                    card("A Podcast", json!([]), browse("MPSP1", "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE")),
                ])
            )]),
            None,
        );
        let (items, next) = parse(&resp);
        assert_eq!(next, None);
        let got: Vec<(&str, &str, &str)> =
            items.iter().map(|i| (i["kind"].as_str().unwrap(), i["browseId"].as_str().unwrap(), i["videoId"].as_str().unwrap())).collect();
        assert_eq!(got, [("album", "MPREb_1", ""), ("playlist", "RDCLAK5uy_x", ""), ("artist", "UC1", ""), ("song", "", "vid1")]);
        assert!(items.iter().all(|i| i["section"] == "Listen again"), "the account's name above a plain title is not part of it");
        assert_eq!(items[0]["subtitle"], "Album • Someone");
        assert_eq!(items[0]["thumbnail"], "https://lh3.googleusercontent.com/a=w544-h544-l90-rj", "the largest thumbnail, at cover size");
        assert_eq!((items[3]["artist"].as_str(), items[3]["artistId"].as_str()), (Some("Someone"), Some("UC1")));
    }

    #[test]
    fn quick_picks_rows_become_songs_with_their_links() {
        let row = json!({"musicResponsiveListItemRenderer": {
        "thumbnail": thumbs(),
        "playlistItemData": {"videoId": "vid2"},
        "flexColumns": [
            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "A Song", "navigationEndpoint": {"watchEndpoint": {"videoId": "vid2"}}}]}}},
            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                {"text": "One", "navigationEndpoint": browse("UC1", "MUSIC_PAGE_TYPE_ARTIST")}, {"text": " & "},
                {"text": "Two", "navigationEndpoint": browse("UC2", "MUSIC_PAGE_TYPE_ARTIST")}]}}},
            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "An Album", "navigationEndpoint": browse("MPREb_2", "MUSIC_PAGE_TYPE_ALBUM")}]}}}
        ]}});
        let (items, _) = parse(&page(json!([shelf(json!({"title": {"runs": [{"text": "Quick picks"}]}}), json!([row]))]), None));
        assert_eq!(items.len(), 1);
        let s = &items[0];
        assert_eq!((s["kind"].as_str(), s["title"].as_str(), s["videoId"].as_str()), (Some("song"), Some("A Song"), Some("vid2")));
        assert_eq!((s["subtitle"].as_str(), s["artist"].as_str()), (Some("One, Two"), Some("One")));
        assert_eq!((s["artistId"].as_str(), s["albumId"].as_str()), (Some("UC1"), Some("MPREb_2")));
    }

    #[test]
    fn a_shelf_about_something_says_so_in_its_title() {
        let header = json!({"title": {"runs": [{"text": "Some Artist", "navigationEndpoint": browse("UC9", "MUSIC_PAGE_TYPE_ARTIST")}]},
                            "strapline": {"runs": [{"text": "SIMILAR TO"}]}});
        let (items, _) = parse(&page(json!([shelf(header, json!([card("X", json!([]), browse("UC1", "MUSIC_PAGE_TYPE_ARTIST"))]))]), None));
        assert_eq!(items[0]["section"], "Similar to Some Artist");
    }

    #[test]
    fn a_title_that_links_to_a_feed_page_keeps_its_plain_name() {
        // "Listen again" links to the history page and sits under the account's name
        let link = json!({"browseEndpoint": {"browseId": "FEmusic_listen_again"}});
        let header = json!({"title": {"runs": [{"text": "Listen again", "navigationEndpoint": link}]},
                            "strapline": {"runs": [{"text": "BYBROOKLYN"}]}});
        let (items, _) = parse(&page(json!([shelf(header, json!([card("X", json!([]), browse("UC1", "MUSIC_PAGE_TYPE_ARTIST"))]))]), None));
        assert_eq!(items[0]["section"], "Listen again");
    }

    #[test]
    fn unknown_shelves_and_garbage_are_skipped_not_errors() {
        let resp = page(
            json!([
                {"musicTastebuilderShelfRenderer": {"primaryText": {"runs": [{"text": "Tell us"}]}}},
                shelf(json!({"title": {"runs": [{"text": "Odd"}]}}), json!([{"somethingNew": {}}, 5, card("No link", json!([]), json!({}))])),
                shelf(json!({}), json!([card("Untitled shelf", json!([]), browse("MPREb_1", "MUSIC_PAGE_TYPE_ALBUM"))])),
            ]),
            None,
        );
        assert_eq!(parse(&resp), (Vec::new(), None));
        for junk in [json!({}), json!(null), json!([1, 2]), json!({"contents": "no"}), json!({"error": {"code": 400}})] {
            assert_eq!(parse(&junk), (Vec::new(), None), "{junk}");
        }
    }

    #[test]
    fn the_visitor_id_is_read_from_the_answer() {
        assert_eq!(visitor(&json!({"responseContext": {"visitorData": "Cgt2aXNpdG9y"}})), "Cgt2aXNpdG9y");
        assert_eq!(visitor(&json!({})), "");
    }

    #[test]
    fn a_signed_out_answer_is_recognised_only_when_youtube_says_so() {
        let answer = |v: &str| {
            json!({"responseContext": {"serviceTrackingParams": [
            {"service": "CSI", "params": [{"key": "c", "value": "WEB_REMIX"}]},
            {"service": "GFEEDBACK", "params": [{"key": "logged_in", "value": v}]}]}})
        };
        assert!(answered_signed_out(&answer("0")));
        assert!(!answered_signed_out(&answer("1")));
        assert!(!answered_signed_out(&json!({})), "no marker: don't sign anyone out over it");
    }

    #[test]
    fn the_next_page_token_is_found_on_first_and_later_pages() {
        assert_eq!(parse(&page(json!([]), Some("tok1"))).1.as_deref(), Some("tok1"));
        let later = json!({"continuationContents": {"sectionListContinuation": {
            "contents": [shelf(json!({"title": {"runs": [{"text": "More"}]}}), json!([card("A", json!([]), browse("MPREb_3", "MUSIC_PAGE_TYPE_ALBUM"))]))],
            "continuations": [{"nextContinuationData": {"continuation": "tok2"}}]}}});
        let (items, next) = parse(&later);
        assert_eq!((items.len(), next.as_deref()), (1, Some("tok2")));
        assert_eq!(items[0]["section"], "More");
    }
}

/// Live, signed in (`cargo test --bin yqt live_feed_survey -- --ignored --nocapture`): which kinds of shelf and
/// card the account's feed is made of, and how many of them `parse` reads. Prints renderer names and counts only.
#[cfg(test)]
mod live {
    use super::*;
    use std::collections::BTreeMap;

    #[tokio::test]
    #[ignore]
    async fn live_feed_survey() {
        let mut resp = crate::ytapi::home_feed(None).await.expect("home feed");
        if answered_signed_out(&resp) {
            panic!("stored session not accepted: the feed is the anonymous one");
        }
        let visitor = visitor(&resp).to_owned();
        for page in 0..12 {
            let first = &resp["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][0]["tabRenderer"]["content"]["sectionListRenderer"];
            let list = if first.is_object() { first } else { &resp["continuationContents"]["sectionListContinuation"] };
            let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
            let mut shelves = Vec::new();
            for shelf in list["contents"].as_array().into_iter().flatten() {
                for (name, body) in shelf.as_object().into_iter().flatten() {
                    let entries = body["contents"].as_array().map_or(0, Vec::len);
                    let titled = !shelf_title(&body["header"]["musicCarouselShelfBasicHeaderRenderer"]).is_empty();
                    shelves.push(format!("{name}[{entries}{}]", if titled { "" } else { ", untitled" }));
                    for entry in body["contents"].as_array().into_iter().flatten() {
                        for key in entry.as_object().into_iter().flatten().map(|(k, _)| k) {
                            *kinds.entry(key.clone()).or_default() += 1;
                        }
                    }
                }
            }
            let header_keys: Vec<&String> = list["header"].as_object().into_iter().flatten().map(|(k, _)| k).collect();
            let (items, next) = parse(&resp);
            println!(
                "RES page {page}: shelves {shelves:?}; cards {kinds:?}; read {} items; header {header_keys:?}; next {}",
                items.len(),
                next.is_some()
            );
            let Some(token) = next else { break };
            resp = crate::ytapi::home_feed(Some((&token, &visitor))).await.expect("next page");
        }
    }
}
