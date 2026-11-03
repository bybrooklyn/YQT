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

/// The audio codecs YouTube serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// Opus in WebM (itag 249/250/251).
    Opus,
    /// AAC-LC in MP4 (itag 139/140).
    Aac,
    Other,
}

impl Codec {
    fn from_mime(mime: &str) -> Codec {
        let m = mime.to_ascii_lowercase();
        if m.contains("opus") {
            Codec::Opus
        } else if m.contains("mp4a") {
            Codec::Aac
        } else {
            Codec::Other
        }
    }
}

/// How good the audio should be. Used by downloads ("Best / Normal / Data saver") and the audio engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)] // DataSaver/Normal are chosen by the downloads workstream
pub enum Quality {
    Best,
    #[default]
    Normal,
    DataSaver,
}

impl Quality {
    /// From the `downloads.quality` setting; anything unknown means Normal.
    #[allow(dead_code)] // used by the downloads workstream
    pub fn from_setting(s: &str) -> Quality {
        match s {
            "best" => Quality::Best,
            "data_saver" => Quality::DataSaver,
            _ => Quality::Normal,
        }
    }
}

/// Bitrate "Normal" aims for (bits per second): about what YouTube Music streams by default.
const NORMAL_TARGET_BPS: u64 = 128_000;

/// One audio-only stream of a video, with a plain (unciphered) URL.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFormat {
    pub itag: u32,
    pub url: String,
    pub mime: String,
    pub codec: Codec,
    /// Peak bitrate in bits per second.
    pub bitrate: u64,
    pub content_length: Option<u64>,
    pub duration_ms: Option<u64>,
    /// YouTube's loudness of this track in dB relative to its reference (negative = quieter than the reference);
    /// the audio engine uses it to even out volume between tracks.
    pub loudness_db: Option<f64>,
    pub sample_rate: Option<u32>,
}

static LOUDNESS: Mutex<Option<std::collections::HashMap<String, f64>>> = Mutex::new(None);
/// Remember at most this many tracks' loudness (a listening session is far smaller; the map is just cleared when full).
const LOUDNESS_CAP: usize = 4096;

/// YouTube's loudness (dB relative to its reference) of a video whose formats were resolved earlier this run.
/// The audio engine uses it for normalization.
pub fn loudness(video_id: &str) -> Option<f64> {
    LOUDNESS.lock().ok()?.as_ref()?.get(video_id).copied()
}

fn remember_loudness(video_id: &str, formats: &[AudioFormat]) {
    let Some(db) = formats.iter().find_map(|f| f.loudness_db).filter(|d| d.is_finite()) else { return };
    if let Ok(mut m) = LOUDNESS.lock() {
        let map = m.get_or_insert_default();
        if map.len() >= LOUDNESS_CAP {
            map.clear();
        }
        map.insert(video_id.to_owned(), db);
    }
}

/// Every plain-URL audio format of a video. A stale visitor id is the usual reason for a refusal,
/// so one failure retries with a fresh session.
pub async fn audio_formats(video_id: &str) -> Result<Vec<AudioFormat>, String> {
    match audio_formats_once(video_id).await {
        Ok(r) => Ok(r),
        Err(_) => {
            *VISITOR.lock().unwrap() = None;
            audio_formats_once(video_id).await
        }
    }
}

/// Best audio-only stream URL for a video, plus its mime type (highest bitrate, any codec).
pub async fn audio_url(video_id: &str) -> Result<(String, String), String> {
    let formats = audio_formats(video_id).await?;
    pick_format(&formats, Quality::Best, None)
        .map(|f| (f.url.clone(), f.mime.clone()))
        .ok_or_else(|| "visionOS: no audio format with a plain URL".to_owned())
}

async fn audio_formats_once(video_id: &str) -> Result<Vec<AudioFormat>, String> {
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
    let parsed = parse_formats(formats);
    if parsed.is_empty() {
        return Err("visionOS: no audio format with a plain URL".to_owned());
    }
    remember_loudness(video_id, &parsed);
    Ok(parsed)
}

/// YouTube sends several numbers as JSON strings ("contentLength": "3405210"); accept both forms.
fn num(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// The audio-only formats of an `adaptiveFormats` array that carry a plain (unciphered) URL.
pub fn parse_formats(formats: &[Value]) -> Vec<AudioFormat> {
    formats
        .iter()
        .filter_map(|f| {
            let mime = f["mimeType"].as_str().filter(|m| m.starts_with("audio/"))?;
            let url = f["url"].as_str()?;
            Some(AudioFormat {
                itag: f["itag"].as_u64().unwrap_or(0) as u32,
                url: url.to_owned(),
                mime: mime.to_owned(),
                codec: Codec::from_mime(mime),
                bitrate: f["bitrate"].as_u64().unwrap_or(0),
                content_length: num(&f["contentLength"]),
                duration_ms: num(&f["approxDurationMs"]),
                loudness_db: f["loudnessDb"].as_f64(),
                sample_rate: num(&f["audioSampleRate"]).map(|r| r as u32),
            })
        })
        .collect()
}

/// Choose a format by quality. `prefer` narrows to one codec when the video has it (the Rust audio engine can
/// decode both; a caller that can't, or wants a specific container for downloads, says so), else all are considered.
/// Best = highest bitrate, DataSaver = lowest, Normal = closest to ~128 kbps.
pub fn pick_format(formats: &[AudioFormat], quality: Quality, prefer: Option<Codec>) -> Option<&AudioFormat> {
    let preferred: Vec<&AudioFormat> = match prefer {
        Some(c) => formats.iter().filter(|f| f.codec == c).collect(),
        None => Vec::new(),
    };
    let pool: Vec<&AudioFormat> = if preferred.is_empty() { formats.iter().collect() } else { preferred };
    match quality {
        Quality::Best => pool.into_iter().max_by_key(|f| f.bitrate),
        Quality::DataSaver => pool.into_iter().min_by_key(|f| f.bitrate),
        Quality::Normal => pool.into_iter().min_by_key(|f| (f.bitrate as i64 - NORMAL_TARGET_BPS as i64).unsigned_abs()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a real visionOS response looks like: Opus + AAC at several bitrates, numbers sometimes as strings.
    fn sample() -> Vec<Value> {
        vec![
            json!({"itag":137,"mimeType":"video/mp4; codecs=\"avc1\"","bitrate":9_000_000,"url":"video"}),
            json!({"itag":249,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":55_000,"url":"opus50","contentLength":"1200000","approxDurationMs":"213000","loudnessDb":-3.5,"audioSampleRate":"48000"}),
            json!({"itag":250,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":78_000,"url":"opus70"}),
            json!({"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":134_000,"url":"opus160"}),
            json!({"itag":139,"mimeType":"audio/mp4; codecs=\"mp4a.40.5\"","bitrate":49_000,"url":"aac48"}),
            json!({"itag":140,"mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":130_000,"url":"aac128"}),
            json!({"itag":141,"mimeType":"audio/webm","bitrate":999_000,"signatureCipher":"x"}),
        ]
    }

    #[test]
    fn parses_only_plain_audio_and_reads_string_numbers() {
        let f = parse_formats(&sample());
        assert_eq!(f.len(), 5, "video and ciphered entries are dropped");
        let opus50 = f.iter().find(|x| x.itag == 249).unwrap();
        assert_eq!(
            (opus50.codec, opus50.content_length, opus50.duration_ms, opus50.sample_rate),
            (Codec::Opus, Some(1_200_000), Some(213_000), Some(48_000))
        );
        assert_eq!(opus50.loudness_db, Some(-3.5));
        assert_eq!(f.iter().find(|x| x.itag == 140).unwrap().codec, Codec::Aac);
        assert!(parse_formats(&[]).is_empty());
    }

    #[test]
    fn quality_picks_highest_closest_to_128k_and_lowest() {
        let f = parse_formats(&sample());
        assert_eq!(pick_format(&f, Quality::Best, None).unwrap().url, "opus160");
        assert_eq!(pick_format(&f, Quality::DataSaver, None).unwrap().url, "aac48");
        // 130 kbps AAC (itag 140) and 134 kbps Opus are both near 128; AAC is closer
        assert_eq!(pick_format(&f, Quality::Normal, None).unwrap().url, "aac128");
    }

    #[test]
    fn codec_preference_narrows_the_pool_and_falls_back_when_absent() {
        let f = parse_formats(&sample());
        assert_eq!(pick_format(&f, Quality::Best, Some(Codec::Aac)).unwrap().url, "aac128");
        assert_eq!(pick_format(&f, Quality::DataSaver, Some(Codec::Opus)).unwrap().url, "opus50");
        let aac_only: Vec<AudioFormat> = f.iter().filter(|x| x.codec == Codec::Aac).cloned().collect();
        assert_eq!(pick_format(&aac_only, Quality::Best, Some(Codec::Opus)).unwrap().url, "aac128", "no opus available → any codec");
        assert!(pick_format(&[], Quality::Best, None).is_none());
    }

    #[test]
    fn quality_setting_strings() {
        assert_eq!(Quality::from_setting("best"), Quality::Best);
        assert_eq!(Quality::from_setting("data_saver"), Quality::DataSaver);
        assert_eq!(Quality::from_setting("normal"), Quality::Normal);
        assert_eq!(Quality::from_setting("whatever"), Quality::Normal);
    }

    #[test]
    fn the_default_pick_matches_the_old_behaviour() {
        // audio_url() = highest bitrate, any codec: what the app has always played
        let f = parse_formats(&sample());
        assert_eq!(pick_format(&f, Quality::Best, None).unwrap().mime, "audio/webm; codecs=\"opus\"");
    }

    #[test]
    fn loudness_is_remembered_per_video() {
        let f = parse_formats(&sample());
        remember_loudness("loud-test-id", &f);
        assert_eq!(loudness("loud-test-id"), Some(-3.5));
        assert_eq!(loudness("never-seen"), None);
        remember_loudness("no-loudness", &parse_formats(&sample()[2..3]));
        assert_eq!(loudness("no-loudness"), None);
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
