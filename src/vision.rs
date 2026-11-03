// SPDX-License-Identifier: GPL-3.0-only
//! Stream URLs from InnerTube's visionOS client.
//!
//! RustyPipe's iOS-signed URLs stop serving after ~1 minute of media, and the
//! web/TV/Android clients need cipher solving. URLs signed for the visionOS
//! client are plain and unwalled (technique borrowed from SwiftyV's playback
//! ladder): seed an anonymous watch-page session for a visitor id, then call
//! `/player` as client 101.
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config;

/// The user agent the stream URLs are signed for (the proxy must send the very same one).
pub fn user_agent() -> &'static str {
    &config::get().youtube.visionos_user_agent
}

static VISITOR: Mutex<Option<(String, Instant)>> = Mutex::new(None);

fn client() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        let n = &config::get().network;
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(n.connect_timeout_secs))
            .timeout(Duration::from_secs(n.request_timeout_secs))
            .build()
            .expect("http client")
    })
}

/// Pull `"VISITOR_DATA":"..."` out of a watch page and unescape the JSON string.
fn extract_visitor(html: &str) -> Option<String> {
    for key in ["\"VISITOR_DATA\"", "\"visitorData\""] {
        let Some(i) = html.find(key) else { continue };
        let rest = html[i + key.len()..].trim_start().strip_prefix(':')?.trim_start();
        let rest = rest.strip_prefix('"')?;
        let end = rest.find('"')?;
        let raw = &rest[..end];
        let v: String = serde_json::from_str(&format!("\"{raw}\"")).unwrap_or_else(|_| raw.to_owned());
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}

async fn visitor(video_id: &str) -> Result<String, String> {
    if let Some((v, at)) = VISITOR.lock().unwrap().as_ref() {
        if at.elapsed() < Duration::from_secs(config::get().playback.visitor_cache_minutes * 60) {
            return Ok(v.clone());
        }
    }
    let html = client()
        .get("https://www.youtube.com/watch")
        .query(&[("v", video_id), ("bpctr", "9999999999"), ("has_verified", "1")])
        .header("User-Agent", user_agent())
        .header("Accept-Language", &config::get().youtube.language)
        .header("Cookie", "SOCS=CAI")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let v = extract_visitor(&html).ok_or("no visitor data on watch page")?;
    *VISITOR.lock().unwrap() = Some((v.clone(), Instant::now()));
    Ok(v)
}

/// Best audio-only stream URL for a video, plus its mime type. A stale visitor id is the
/// usual reason for a refusal, so one failure retries with a fresh session.
pub async fn audio_url(video_id: &str) -> Result<(String, String), String> {
    match audio_url_once(video_id).await {
        Ok(r) => Ok(r),
        Err(_) => {
            *VISITOR.lock().unwrap() = None;
            audio_url_once(video_id).await
        }
    }
}

async fn audio_url_once(video_id: &str) -> Result<(String, String), String> {
    let yt = &config::get().youtube;
    let vd = visitor(video_id).await?;
    let body = json!({
        "context": {"client": {
            "clientName": "VISIONOS", "clientVersion": yt.visionos_client_version,
            "deviceMake": "Apple", "deviceModel": "RealityDevice17,1",
            "userAgent": yt.visionos_user_agent, "osName": "visionOS", "osVersion": yt.visionos_os_version,
            "visitorData": vd
        }},
        "videoId": video_id,
        "racyCheckOk": true, "contentCheckOk": true,
        "playbackContext": {"contentPlaybackContext": {"html5Preference": "HTML5_PREF_WANTS"}}
    });
    let resp: Value = client()
        .post("https://www.youtube.com/youtubei/v1/player")
        .query(&[("key", yt.innertube_api_key.as_str()), ("prettyPrint", "false")])
        .header("User-Agent", user_agent())
        .header("Content-Type", "application/json")
        .header("Origin", "https://www.youtube.com")
        .header("X-Goog-Visitor-Id", &vd)
        .header("X-YouTube-Client-Name", "101")
        .header("X-YouTube-Client-Version", &yt.visionos_client_version)
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp["playabilityStatus"]["status"].as_str().unwrap_or("");
    let formats = resp["streamingData"]["adaptiveFormats"].as_array();
    let Some(formats) = formats else {
        let why = resp["playabilityStatus"]["reason"].as_str().unwrap_or(status);
        return Err(format!("visionOS: no streams ({why})"));
    };
    pick_audio(formats).ok_or_else(|| "visionOS: no audio format with a plain URL".to_owned())
}

/// Highest-bitrate audio-only format that carries a plain (unciphered) URL.
fn pick_audio(formats: &[Value]) -> Option<(String, String)> {
    formats
        .iter()
        .filter(|f| f["mimeType"].as_str().is_some_and(|m| m.starts_with("audio/")) && f["url"].is_string())
        .max_by_key(|f| f["bitrate"].as_u64().unwrap_or(0))
        .map(|f| (f["url"].as_str().unwrap().to_owned(), f["mimeType"].as_str().unwrap_or("").to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_best_plain_audio() {
        let f = vec![
            json!({"mimeType":"video/mp4","bitrate":9_000_000,"url":"v"}),
            json!({"mimeType":"audio/webm; codecs=\"opus\"","bitrate":128_000,"url":"lo"}),
            json!({"mimeType":"audio/mp4","bitrate":256_000,"url":"hi"}),
            json!({"mimeType":"audio/webm","bitrate":999_000,"signatureCipher":"x"}),
        ];
        assert_eq!(pick_audio(&f).unwrap().0, "hi");
        assert!(pick_audio(&[]).is_none());
    }

    #[test]
    fn extracts_visitor_data() {
        let html = r#"..,"VISITOR_DATA":"Cgt4\u003d\u003d","x":1"#;
        assert_eq!(extract_visitor(html).as_deref(), Some("Cgt4=="));
        assert!(extract_visitor("nothing here").is_none());
    }
}

/// Live check against YouTube (`cargo test -- --ignored --nocapture`): does the visionOS route still work?
#[cfg(test)]
mod live {
    #[tokio::test]
    #[ignore]
    async fn vision_route_resolves_streams() {
        for id in ["dQw4w9WgXcQ", "9bZkp7q19f0", "kJQP7kiw5Fk"] {
            match super::audio_url(id).await {
                Ok((url, mime)) => println!("{id}: OK {mime} {}…", &url[..60.min(url.len())]),
                Err(e) => println!("{id}: FAIL {e}"),
            }
        }
    }
}
