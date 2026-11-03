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

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

/// The headers of a signed-in YouTube Music web request: Cookie + SAPISIDHASH + client identity.
/// Fails with `AUTH_MSG` when signed out. The values are secrets: never log them.
/// The WEB_REMIX client version to claim. YouTube rejects requests from long-outdated clients, and RustyPipe keeps
/// a fresh one in its cache (`clients.desktop_music.data.version`), so prefer that over the (slowly rotting) config default.
pub fn client_version() -> String {
    std::fs::read_to_string(crate::state::data_dir().join("rustypipe_cache.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| parse_client_version(&v))
        .unwrap_or_else(|| crate::config::get().youtube.web_remix_client_version.clone())
}

/// `clients.desktop_music.data.version` of RustyPipe's cache, if it looks like a client version (`1.20261031.09.00`).
pub fn parse_client_version(cache: &Value) -> Option<String> {
    let v = cache["clients"]["desktop_music"]["data"]["version"].as_str()?;
    let ok = !v.is_empty() && v.len() <= 32 && v.chars().all(|c| c.is_ascii_digit() || c == '.') && v.matches('.').count() >= 2;
    ok.then(|| v.to_owned())
}

pub fn auth_headers() -> Result<Vec<(&'static str, String)>, String> {
    let yt = &crate::config::get().youtube;
    let origin = yt.music_origin.as_str();
    let version_owned = client_version();
    let version = version_owned.as_str();
    let cookie = stored_cookie().ok_or(AUTH_MSG)?;
    let sapisid = cookie_value(&cookie, "SAPISID").or_else(|| cookie_value(&cookie, "__Secure-3PAPISID")).ok_or(AUTH_MSG)?;
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    let auth = sapisid_hash(sapisid, ts, origin);
    Ok(vec![
        ("Cookie", cookie),
        ("Authorization", auth),
        ("Origin", origin.to_owned()),
        ("X-Origin", origin.to_owned()),
        ("X-Goog-AuthUser", "0".into()),
        ("X-YouTube-Client-Name", "67".into()),
        ("X-YouTube-Client-Version", version.to_owned()),
        ("User-Agent", USER_AGENT.into()),
    ])
}

fn web_remix_context() -> Value {
    let yt = &crate::config::get().youtube;
    json!({"client": {"clientName": "WEB_REMIX", "clientVersion": client_version(), "hl": yt.language}})
}

async fn post(endpoint: &str, mut body: Value) -> Result<Value, String> {
    let origin = crate::config::get().youtube.music_origin.clone();
    let headers = auth_headers()?;
    body["context"] = web_remix_context();
    let mut req = http().post(format!("{origin}/youtubei/v1/{endpoint}?prettyPrint=false"));
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.json(&body).send().await.map_err(|e| e.without_url().to_string())?;
    match resp.status().as_u16() {
        200 => resp.json().await.map_err(|e| e.to_string()),
        401 | 403 => Err(AUTH_MSG.into()),
        s => {
            // YouTube explains most rejections in `error.message`; surface it (bounded, no request data echoed).
            let text = resp.text().await.unwrap_or_default();
            Err(format!("YouTube returned HTTP {s}{}", error_message(&text).map(|m| format!(": {m}")).unwrap_or_default()))
        }
    }
}

/// `error.message` / `error.status` from a YouTube error body, cut to a sane length.
pub fn error_message(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body.get(..body.len().min(64 * 1024))?).ok()?;
    let msg = v["error"]["message"].as_str().or_else(|| v["error"]["status"].as_str())?;
    let msg: String = msg.chars().filter(|c| !c.is_control()).take(160).collect();
    (!msg.is_empty()).then_some(msg)
}

/// `playbackTracking.videostatsPlaybackUrl.baseUrl` of a `/player` response.
pub fn extract_tracking_url(player: &Value) -> Option<String> {
    player["playbackTracking"]["videostatsPlaybackUrl"]["baseUrl"].as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Host check for URLs we will send the user's cookies to: https, no userinfo, and the host is
/// `youtube.com`/`google.com` or a true subdomain of one (not `evilyoutube.com`, not `youtube.com.evil.io`).
pub fn is_trusted_youtube_url(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else { return false };
    if u.scheme() != "https" || !u.username().is_empty() || u.password().is_some() || !matches!(u.port(), None | Some(443)) {
        return false;
    }
    let Some(host) = u.host_str() else { return false };
    // (IP literals can never match the domain suffixes below.)
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    ["youtube.com", "google.com"].iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// Ask YouTube (signed in, WEB_REMIX) for the playback-tracking URL of a video. The URL is
/// validated; an untrusted one is an error, so cookies are never sent anywhere unexpected.
pub async fn player_tracking_url(video_id: &str) -> Result<String, String> {
    let v = post("player", json!({"videoId": video_id, "racyCheckOk": true, "contentCheckOk": true})).await?;
    let url = extract_tracking_url(&v).ok_or("YouTube returned no playback-tracking URL")?;
    if is_trusted_youtube_url(&url) {
        Ok(url)
    } else {
        Err("YouTube returned an unexpected playback-tracking host".into())
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
    fn client_version_is_read_from_rustypipes_cache_and_validated() {
        let cache = json!({"clients": {"desktop_music": {"last_update": "x", "data": {"version": "1.20261031.09.00"}}}});
        assert_eq!(parse_client_version(&cache).as_deref(), Some("1.20261031.09.00"));
        // missing, empty, junk or absurd values fall back to the config default
        assert_eq!(parse_client_version(&json!({})), None);
        assert_eq!(parse_client_version(&json!({"clients": {"desktop_music": {"data": {"version": ""}}}})), None);
        assert_eq!(parse_client_version(&json!({"clients": {"desktop_music": {"data": {"version": "1.2; DROP"}}}})), None);
        assert_eq!(parse_client_version(&json!({"clients": {"desktop_music": {"data": {"version": "12"}}}})), None);
        assert_eq!(
            parse_client_version(&json!({"clients": {"desktop_music": {"data": {"version": "1.2.3.4.5.6.7.8.9.10.11.12.13.14.15"}}}})),
            None
        );
    }

    #[test]
    fn youtube_error_bodies_are_summarised_safely() {
        let body = r#"{"error":{"code":400,"message":"Precondition check failed.","status":"FAILED_PRECONDITION"}}"#;
        assert_eq!(error_message(body).as_deref(), Some("Precondition check failed."));
        assert_eq!(error_message(r#"{"error":{"status":"INVALID_ARGUMENT"}}"#).as_deref(), Some("INVALID_ARGUMENT"));
        assert_eq!(error_message("<html>not json</html>"), None);
        assert_eq!(error_message(r#"{"error":{"message":""}}"#), None);
        // control characters are dropped and the length is bounded
        let long = format!(r#"{{"error":{{"message":"a\nb{}"}}}}"#, "x".repeat(500));
        let m = error_message(&long).unwrap();
        assert!(m.chars().count() <= 160 && !m.contains('\n'));
    }

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
    fn trusted_urls() {
        for ok in [
            "https://s.youtube.com/api/stats/playback?docid=x",
            "https://music.youtube.com/a",
            "https://youtube.com/a",
            "https://www.google.com/x",
            "https://S.YouTube.com./x",
            "https://s.youtube.com:443/x",
        ] {
            assert!(is_trusted_youtube_url(ok), "{ok}");
        }
        for bad in [
            "http://s.youtube.com/x",
            "https://evilyoutube.com/x",
            "https://youtube.com.evil.io/x",
            "https://user@s.youtube.com/x",
            "https://user:pw@s.youtube.com/x",
            "https://s.youtube.com@evil.io/x",
            "https://evil.io/?u=s.youtube.com",
            "https://s.youtube.com:8443/x",
            "https://127.0.0.1/x",
            "https://[::1]/x",
            "ftp://s.youtube.com/x",
            "s.youtube.com/x",
            "",
        ] {
            assert!(!is_trusted_youtube_url(bad), "{bad}");
        }
    }

    #[test]
    fn extracts_tracking_url() {
        let v = json!({"playbackTracking": {"videostatsPlaybackUrl": {"baseUrl": "https://s.youtube.com/api/stats/playback?docid=1"}}});
        assert_eq!(extract_tracking_url(&v).as_deref(), Some("https://s.youtube.com/api/stats/playback?docid=1"));
        assert_eq!(extract_tracking_url(&json!({})), None);
        assert_eq!(extract_tracking_url(&json!({"playbackTracking": {"videostatsPlaybackUrl": {"baseUrl": ""}}})), None);
    }

    #[test]
    fn finds_cookie_values() {
        assert_eq!(cookie_value("a=1; SAPISID=x; b=2", "SAPISID"), Some("x"));
        assert_eq!(cookie_value("a=1", "SAPISID"), None);
    }
}
