// SPDX-License-Identifier: GPL-3.0-only
//! The few signed-in write calls RustyPipe doesn't have: like/unlike and add-to-playlist.
//! Authenticated like the YouTube Music web app: Cookie + SAPISIDHASH.
use crate::auth::{stored_cookie, AUTH_MSG};
use crate::state::http;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

pub fn cookie_value<'a>(cookie: &'a str, name: &str) -> Option<&'a str> {
    cookie.split(';').filter_map(|p| p.trim().split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v)
}

/// `SAPISIDHASH <ts>_<sha1("<ts> <SAPISID> <origin>")>`
pub fn sapisid_hash(sapisid: &str, ts: u64, origin: &str) -> String {
    let digest = Sha1::digest(format!("{ts} {sapisid} {origin}").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("SAPISIDHASH {ts}_{hex}")
}

async fn post(endpoint: &str, mut body: Value) -> Result<Value, String> {
    let yt = &crate::config::get().youtube;
    let (origin, version) = (yt.music_origin.as_str(), yt.web_remix_client_version.as_str());
    let cookie = stored_cookie().ok_or(AUTH_MSG)?;
    let sapisid = cookie_value(&cookie, "SAPISID").or_else(|| cookie_value(&cookie, "__Secure-3PAPISID")).ok_or(AUTH_MSG)?;
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    body["context"] = json!({"client": {"clientName": "WEB_REMIX", "clientVersion": version, "hl": yt.language}});
    let resp = http()
        .post(format!("{origin}/youtubei/v1/{endpoint}?prettyPrint=false"))
        .header("Cookie", &cookie)
        .header("Authorization", sapisid_hash(sapisid, ts, origin))
        .header("Origin", origin)
        .header("X-Origin", origin)
        .header("X-Goog-AuthUser", "0")
        .header("X-YouTube-Client-Name", "67")
        .header("X-YouTube-Client-Version", version)
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    match resp.status().as_u16() {
        200 => resp.json().await.map_err(|e| e.to_string()),
        401 | 403 => Err(AUTH_MSG.into()),
        s => Err(format!("YouTube returned HTTP {s}")),
    }
}

pub async fn rate(video_id: &str, like: bool) -> Result<(), String> {
    post(if like { "like/like" } else { "like/removelike" }, json!({"target": {"videoId": video_id}})).await?;
    Ok(())
}

pub async fn add_to_playlist(playlist_id: &str, video_id: &str) -> Result<(), String> {
    let r = post(
        "browse/edit_playlist",
        json!({
            "playlistId": playlist_id.trim_start_matches("VL"),
            "actions": [{"action": "ACTION_ADD_VIDEO", "addedVideoId": video_id}],
            "dedupeOption": "DEDUPE_OPTION_SKIP"
        }),
    )
    .await?;
    match r["status"].as_str() {
        Some("STATUS_SUCCEEDED") => Ok(()),
        other => Err(format!("Couldn't add to that playlist ({})", other.unwrap_or("not your playlist?"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_sha1_hex() {
        // reference value from `printf "1 abc https://music.youtube.com" | sha1sum`
        let h = sapisid_hash("abc", 1, "https://music.youtube.com");
        let (prefix, hex) = h.rsplit_once('_').unwrap();
        assert_eq!(prefix, "SAPISIDHASH 1");
        assert_eq!(hex, "fdd4a2889de4fd5c886eeae17a803a8c57f01113");
        assert_eq!(hex.len(), 40);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, sapisid_hash("abc", 1, "https://music.youtube.com"));
        assert_ne!(h, sapisid_hash("abd", 1, "https://music.youtube.com"));
    }

    #[test]
    fn finds_cookie_values() {
        assert_eq!(cookie_value("a=1; SAPISID=x; b=2", "SAPISID"), Some("x"));
        assert_eq!(cookie_value("a=1", "SAPISID"), None);
    }
}
