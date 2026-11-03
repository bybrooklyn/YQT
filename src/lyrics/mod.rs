// SPDX-License-Identifier: GPL-3.0-only
//! Lyrics from several providers, preferring line-by-line synced copies.
//!
//! Providers (`lrclib`, `netease`, `youtube`) sit behind the [`Provider`] trait and are asked concurrently;
//! one being down is not a failure. [`rank::pick`] chooses the winner, [`cache`] remembers it, and
//! `lyrics_qt` exposes the whole thing to QML as the `Lyrics` object. Providers only ever receive the title,
//! artist and duration of the track.
pub mod cache;
pub mod layout;
pub mod lrclib;
pub mod netease;
pub mod parse;

pub mod rank;
pub mod youtube;

use crate::config;
use crate::errors::{AppError, ErrorKind};
use parse::Line;
use rank::{Candidate, Pick, Query};
use serde_json::Value;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// Longest response body we are willing to read from a lyrics service.
pub const MAX_BODY: usize = 2 * 1024 * 1024;
/// How long one provider may take before we go on without it.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(12);

/// The typed answer for the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum LyricsResult {
    Synced {
        lines: Vec<Line>,
        source: String,
    },
    Plain {
        text: String,
        source: String,
    },
    /// Nobody has lyrics for this track (shown as exactly "Lyrics unavailable").
    Unavailable,
    /// Something went wrong and no provider could help (shown with a Retry button).
    Failed(AppError),
}

impl From<Pick> for LyricsResult {
    fn from(p: Pick) -> Self {
        match p {
            Pick::Synced { lines, source } => LyricsResult::Synced { lines, source: source.to_owned() },
            Pick::Plain { text, source } => LyricsResult::Plain { text, source: source.to_owned() },
        }
    }
}

pub type ProviderFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<Candidate>, AppError>> + Send + 'a>>;

/// A source of lyrics. `Ok(vec![])` means "I don't have it"; `Err` means "I couldn't ask".
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn fetch<'a>(&'a self, q: &'a Query) -> ProviderFuture<'a>;
}

// ---------------------------------------------------------------------------------------------
// HTTP helpers shared by the web providers
// ---------------------------------------------------------------------------------------------

/// Check a configured base URL and strip a trailing slash.
pub fn checked_base(url: &str, allow_insecure: bool) -> Result<String, AppError> {
    crate::security::validate_endpoint(url, allow_insecure)
        .map(|u| u.trim_end_matches('/').to_owned())
        .map_err(|e| AppError::new(ErrorKind::Config, "A lyrics service address in config.toml isn't acceptable.", e))
}

fn http_error(provider: &str, e: reqwest::Error) -> AppError {
    let e = e.without_url(); // the query string carries the title and artist; keep it out of messages
    if e.is_timeout() || e.is_connect() {
        AppError::new(ErrorKind::Network, format!("Can't reach {provider}."), e.to_string())
    } else {
        AppError::new(ErrorKind::Network, format!("{provider} sent an answer the app couldn't read."), e.to_string())
    }
}

/// GET `url` and parse the body as JSON. `Ok(None)` for 404. The body is cut off at [`MAX_BODY`].
pub async fn get_json(client: &reqwest::Client, provider: &str, url: &str, query: &[(&str, String)]) -> Result<Option<Value>, AppError> {
    let mut resp = client.get(url).query(query).send().await.map_err(|e| http_error(provider, e))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        let kind = if status.as_u16() == 429 { ErrorKind::RateLimit } else { ErrorKind::Network };
        return Err(AppError::new(kind, format!("{provider} is not answering right now."), format!("HTTP {status}")));
    }
    if resp.content_length().is_some_and(|n| n > MAX_BODY as u64) {
        return Err(too_big(provider));
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| http_error(provider, e))? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(too_big(provider));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| AppError::new(ErrorKind::Parse, format!("{provider} sent an answer the app couldn't read."), e.to_string()))
}

fn too_big(provider: &str) -> AppError {
    AppError::new(
        ErrorKind::Parse,
        format!("{provider} sent far more data than lyrics should be."),
        format!("body larger than {MAX_BODY} bytes"),
    )
}

// ---------------------------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------------------------

/// Build the providers named in the settings, most preferred first. Providers with a rejected address
/// are left out (and logged). `lyrics.enabled = false` leaves only YouTube Music's own plain lyrics.
pub fn providers_from_config(client: &reqwest::Client, cfg: &config::Lyrics, allow_insecure: bool) -> Vec<Arc<dyn Provider>> {
    let mut out: Vec<Arc<dyn Provider>> = Vec::new();
    let names: Vec<String> =
        if cfg.enabled { cfg.providers.iter().map(|s| s.trim().to_lowercase()).collect() } else { vec!["youtube".into()] };
    for name in names {
        if out.iter().any(|p| p.name().eq_ignore_ascii_case(&name)) {
            continue;
        }
        let built: Option<Result<Arc<dyn Provider>, AppError>> = match name.as_str() {
            "lrclib" => {
                Some(lrclib::Lrclib::new(client.clone(), &cfg.lrclib_url, allow_insecure).map(|p| Arc::new(p) as Arc<dyn Provider>))
            }
            "netease" => {
                Some(netease::NetEase::new(client.clone(), &cfg.netease_url, allow_insecure).map(|p| Arc::new(p) as Arc<dyn Provider>))
            }
            "youtube" => Some(Ok(Arc::new(youtube::YoutubeMusic) as Arc<dyn Provider>)),
            _ => None, // unknown names are ignored
        };
        match built {
            Some(Ok(p)) => out.push(p),
            Some(Err(e)) => tracing::warn!("lyrics provider `{name}` skipped: {}", e.detail),
            None => {}
        }
    }
    if out.is_empty() {
        out.push(Arc::new(youtube::YoutubeMusic));
    }
    out
}

/// Ask every provider at once and pick the best answer. A provider that errors, times out or panics only
/// counts as failed; the result is `Failed` only when *every* provider failed.
pub async fn lookup(providers: Vec<Arc<dyn Provider>>, q: &Query, min_lines: usize, prefer_synced: bool) -> LyricsResult {
    lookup_with_timeout(providers, q, min_lines, prefer_synced, PROVIDER_TIMEOUT).await
}

pub async fn lookup_with_timeout(
    providers: Vec<Arc<dyn Provider>>,
    q: &Query,
    min_lines: usize,
    prefer_synced: bool,
    limit: Duration,
) -> LyricsResult {
    let total = providers.len();
    let mut set = tokio::task::JoinSet::new();
    for (rank, p) in providers.into_iter().enumerate() {
        let q = q.clone();
        set.spawn(async move {
            let r = match tokio::time::timeout(limit, p.fetch(&q)).await {
                Ok(r) => r,
                Err(_) => Err(AppError::new(ErrorKind::Network, format!("{} took too long to answer.", p.name()), "timeout")),
            };
            (rank, p.name(), r)
        });
    }
    let mut cands: Vec<Candidate> = Vec::new();
    let mut failures: Vec<AppError> = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((rank, name, Ok(found))) => {
                tracing::debug!("lyrics: {name} offered {} record(s)", found.len());
                cands.extend(found.into_iter().map(|c| Candidate { provider_rank: rank, ..c }));
            }
            Ok((_, name, Err(e))) => {
                tracing::debug!("lyrics: {name} failed: {}", e.detail);
                failures.push(e);
            }
            Err(e) => failures.push(AppError::new(ErrorKind::Internal, "A lyrics service failed unexpectedly.", e.to_string())),
        }
    }
    if let Some(p) = rank::pick(cands, q, min_lines, prefer_synced) {
        return p.into();
    }
    if total > 0 && failures.len() >= total {
        // deterministic: the most informative failure (not a timeout/config one) first
        let first = failures.iter().find(|e| e.kind != ErrorKind::Config).unwrap_or(&failures[0]).clone();
        return LyricsResult::Failed(first);
    }
    LyricsResult::Unavailable
}

/// Everything the app does for one track: memory cache, disk cache, then the providers.
pub async fn resolve(client: &reqwest::Client, cfg: &config::Lyrics, allow_insecure: bool, q: &Query, disk: Option<&Path>) -> LyricsResult {
    let providers = providers_from_config(client, cfg, allow_insecure);
    let names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
    let key = cache::key(q, &names, cfg.min_lines);
    let now = cache::unix_now();
    if let Some(hit) = cache::memory().get(&key) {
        return hit;
    }
    let disk = disk.filter(|_| cfg.cache_days > 0);
    if let Some(dir) = disk {
        if let Some(hit) = cache::load(dir, &key, cfg.cache_days, now) {
            cache::memory().put(&key, hit.clone());
            return hit;
        }
    }
    let result = lookup(providers, q, cfg.min_lines, cfg.prefer_synced).await;
    if !matches!(result, LyricsResult::Failed(_)) {
        cache::memory().put(&key, result.clone());
        if let Some(dir) = disk {
            cache::store(dir, &key, &result, now);
        }
    }
    result
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A provider whose answer is scripted.
    pub enum Script {
        Found(Vec<Candidate>),
        Fail(AppError),
        Panic,
        Hang,
    }

    pub struct Mock {
        pub name: &'static str,
        pub script: std::sync::Mutex<Option<Script>>,
        pub calls: std::sync::atomic::AtomicUsize,
    }

    impl Mock {
        pub fn provider(name: &'static str, script: Script) -> Arc<dyn Provider> {
            Arc::new(Mock { name, script: std::sync::Mutex::new(Some(script)), calls: Default::default() })
        }
    }

    impl Provider for Mock {
        fn name(&self) -> &'static str {
            self.name
        }
        fn fetch<'a>(&'a self, _q: &'a Query) -> ProviderFuture<'a> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // scripts are reusable: clone what we need
            let s = self.script.lock().unwrap();
            let out = match s.as_ref().unwrap() {
                Script::Found(c) => Some(Ok(c.clone())),
                Script::Fail(e) => Some(Err(e.clone())),
                Script::Panic => panic!("provider blew up"),
                Script::Hang => None,
            };
            Box::pin(async move {
                match out {
                    Some(r) => r,
                    None => {
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                        Ok(Vec::new())
                    }
                }
            })
        }
    }

    pub fn synced_cand(provider: &'static str, n: usize) -> Candidate {
        let lrc: String = (0..n).map(|i| format!("[{:02}:{:02}.00]line {i}\n", i / 6, (i % 6) * 10)).collect();
        Candidate {
            provider,
            title: "Song Name".into(),
            artist: "The Band".into(),
            duration_s: Some(200.0),
            synced: parse::parse_lrc(&lrc),
            ..Default::default()
        }
    }

    pub fn plain_cand(provider: &'static str, n: usize) -> Candidate {
        Candidate {
            provider,
            title: "Song Name".into(),
            artist: "The Band".into(),
            duration_s: Some(200.0),
            plain: (0..n).map(|i| format!("line {i}\n")).collect(),
            ..Default::default()
        }
    }

    pub fn query() -> Query {
        Query { video_id: "vid12345678".into(), title: "Song Name".into(), artist: "The Band".into(), duration_s: 200 }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn net_err() -> AppError {
        AppError::new(ErrorKind::Network, "Can't reach X.", "boom")
    }

    #[tokio::test]
    async fn one_provider_down_is_not_a_failure() {
        let ps = vec![
            Mock::provider("a", Script::Fail(net_err())),
            Mock::provider("b", Script::Panic),
            Mock::provider("c", Script::Found(vec![plain_cand("c", 10)])),
        ];
        match lookup(ps, &query(), 4, true).await {
            LyricsResult::Plain { source, .. } => assert_eq!(source, "c"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn synced_from_any_provider_beats_plain_from_a_preferred_one() {
        let ps = vec![
            Mock::provider("a", Script::Found(vec![plain_cand("a", 50)])),
            Mock::provider("b", Script::Found(vec![synced_cand("b", 8)])),
        ];
        assert!(matches!(lookup(ps, &query(), 4, true).await, LyricsResult::Synced { source, .. } if source == "b"));
    }

    #[tokio::test]
    async fn all_failed_is_failed_but_some_answering_nothing_is_unavailable() {
        let ps = vec![Mock::provider("a", Script::Fail(net_err())), Mock::provider("b", Script::Panic)];
        assert!(matches!(lookup(ps, &query(), 4, true).await, LyricsResult::Failed(_)));

        let ps = vec![Mock::provider("a", Script::Fail(net_err())), Mock::provider("b", Script::Found(Vec::new()))];
        assert_eq!(lookup(ps, &query(), 4, true).await, LyricsResult::Unavailable);

        assert_eq!(lookup(Vec::new(), &query(), 4, true).await, LyricsResult::Unavailable);
    }

    #[tokio::test]
    async fn a_hanging_provider_is_cut_off() {
        let ps = vec![Mock::provider("slow", Script::Hang), Mock::provider("ok", Script::Found(vec![synced_cand("ok", 6)]))];
        let r = lookup_with_timeout(ps, &query(), 4, true, Duration::from_millis(150)).await;
        assert!(matches!(r, LyricsResult::Synced { .. }));
    }

    #[tokio::test]
    async fn provider_order_breaks_ties() {
        let ps = vec![
            Mock::provider("second", Script::Found(vec![synced_cand("second", 8)])),
            Mock::provider("first", Script::Found(vec![synced_cand("first", 8)])),
        ];
        // `second` is listed first, so it is the preferred one
        assert!(matches!(lookup(ps, &query(), 4, true).await, LyricsResult::Synced { source, .. } if source == "second"));
    }

    #[test]
    fn endpoints_are_validated() {
        assert!(checked_base("http://example.com", false).is_err());
        assert!(checked_base("ftp://example.com", true).is_err());
        assert!(checked_base("https://user:pw@example.com", false).is_err());
        assert_eq!(checked_base("https://lrclib.net/", false).unwrap(), "https://lrclib.net");
        assert!(checked_base("http://example.com", true).is_ok());
        assert!(checked_base("http://127.0.0.1:9", false).is_ok());
        let cfg = config::Lyrics {
            lrclib_url: "http://evil.example".into(),
            providers: vec!["lrclib".into(), "bogus".into()],
            ..Default::default()
        };
        let ps = providers_from_config(&reqwest::Client::new(), &cfg, false);
        // the rejected provider is dropped, unknown names ignored, and YouTube is the fallback
        assert_eq!(ps.iter().map(|p| p.name()).collect::<Vec<_>>(), ["YouTube Music"]);
    }

    #[test]
    fn provider_selection_follows_the_settings() {
        let c = reqwest::Client::new();
        let names = |cfg: &config::Lyrics| providers_from_config(&c, cfg, false).iter().map(|p| p.name()).collect::<Vec<_>>();
        assert_eq!(names(&config::Lyrics::default()), ["LRCLIB", "NetEase", "YouTube Music"]);
        let reordered = config::Lyrics { providers: vec!["netease".into(), "lrclib".into(), "NetEase".into()], ..Default::default() };
        assert_eq!(names(&reordered), ["NetEase", "LRCLIB"]);
        let off = config::Lyrics { enabled: false, ..Default::default() };
        assert_eq!(names(&off), ["YouTube Music"]);
    }

    #[tokio::test]
    async fn resolve_serves_the_disk_cache_without_asking_anyone() {
        let dir = std::env::temp_dir().join(format!("ytm-lyrics-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = config::Lyrics { providers: vec!["lrclib".into()], lrclib_url: "http://127.0.0.1:1".into(), ..Default::default() };
        let q = Query { video_id: "resolveCache".into(), ..query() };
        let key = cache::key(&q, &["LRCLIB"], cfg.min_lines);
        let cached = LyricsResult::Plain { text: "a\nb\nc\nd".into(), source: "LRCLIB".into() };
        cache::store(&dir, &key, &cached, cache::unix_now());
        let c = reqwest::Client::new();
        // port 1 refuses connections: only the cache can answer
        assert_eq!(resolve(&c, &cfg, false, &q, Some(&dir)).await, cached);
        // with the cache switched off (cache_days = 0) the same call has to ask, and fails
        let off = config::Lyrics { cache_days: 0, ..cfg };
        let q2 = Query { video_id: "resolveNoCache".into(), ..query() };
        assert!(matches!(resolve(&c, &off, false, &q2, Some(&dir)).await, LyricsResult::Failed(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- real sockets: body cap and status handling ----------------------------------------

    async fn serve_once(response: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let _ = s.write_all(&response).await;
                let _ = s.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    fn http_response(status: &str, body: &[u8], with_length: bool) -> Vec<u8> {
        let mut r = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\n").into_bytes();
        if with_length {
            r.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
        }
        r.extend_from_slice(b"\r\n");
        r.extend_from_slice(body);
        r
    }

    #[tokio::test]
    async fn get_json_reads_small_bodies_and_maps_404_and_errors() {
        let c = reqwest::Client::new();
        let url = serve_once(http_response("200 OK", br#"{"a":1}"#, true)).await;
        assert_eq!(get_json(&c, "X", &url, &[]).await.unwrap().unwrap()["a"], 1);
        let url = serve_once(http_response("404 Not Found", b"{}", true)).await;
        assert!(get_json(&c, "X", &url, &[]).await.unwrap().is_none());
        let url = serve_once(http_response("429 Too Many Requests", b"", true)).await;
        assert_eq!(get_json(&c, "X", &url, &[]).await.unwrap_err().kind, ErrorKind::RateLimit);
        let url = serve_once(http_response("500 Oops", b"", true)).await;
        assert_eq!(get_json(&c, "X", &url, &[]).await.unwrap_err().kind, ErrorKind::Network);
        let url = serve_once(http_response("200 OK", b"<html>", true)).await;
        assert_eq!(get_json(&c, "X", &url, &[]).await.unwrap_err().kind, ErrorKind::Parse);
    }

    #[tokio::test]
    async fn get_json_caps_the_body() {
        let c = reqwest::Client::new();
        let mut big = br#"{"pad":""#.to_vec();
        big.extend(std::iter::repeat_n(b'a', MAX_BODY + 10));
        big.extend_from_slice(br#""}"#);
        // announced length
        let url = serve_once(http_response("200 OK", &big, true)).await;
        assert_eq!(get_json(&c, "X", &url, &[]).await.unwrap_err().kind, ErrorKind::Parse);
        // no Content-Length (streamed until close)
        let url = serve_once(http_response("200 OK", &big, false)).await;
        assert_eq!(get_json(&c, "X", &url, &[]).await.unwrap_err().kind, ErrorKind::Parse);
    }

    /// Network check: every provider on five well-known songs, then the combined answer.
    /// Run with throw-away XDG dirs: `cargo test live_lyrics -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_lyrics_providers() {
        let songs = [
            ("dQw4w9WgXcQ", "Never Gonna Give You Up", "Rick Astley", 213),
            ("fJ9rUzIMcZQ", "Bohemian Rhapsody", "Queen", 355),
            ("4NRXx6U8ABQ", "Blinding Lights", "The Weeknd", 200),
            ("JGwWNGJdvx8", "Shape of You", "Ed Sheeran", 233),
            ("Zi_XLOBDo_Y", "Billie Jean", "Michael Jackson", 294),
        ];
        let cfg = config::Lyrics::default();
        let client = crate::state::http().clone();
        let providers = providers_from_config(&client, &cfg, false);
        for (id, title, artist, secs) in songs {
            let q = Query { video_id: id.into(), title: title.into(), artist: artist.into(), duration_s: secs };
            println!("== {title} - {artist}");
            for p in &providers {
                match p.fetch(&q).await {
                    Ok(c) => {
                        let desc: Vec<String> = c
                            .iter()
                            .map(|c| {
                                format!(
                                    "{}[{} synced, {} plain lines, {:?}s]",
                                    c.title,
                                    parse::non_empty(&c.synced),
                                    parse::non_empty_plain(&c.plain),
                                    c.duration_s
                                )
                            })
                            .collect();
                        println!("   {:<14} {} record(s) {}", p.name(), c.len(), desc.join(" | "));
                    }
                    Err(e) => println!("   {:<14} FAILED {}", p.name(), e.ui_string().replace("\n\n", " / ")),
                }
            }
            match lookup(providers.clone(), &q, cfg.min_lines, true).await {
                LyricsResult::Synced { lines, source } => println!("   -> winner {source}: SYNCED, {} lines", lines.len()),
                LyricsResult::Plain { text, source } => println!("   -> winner {source}: plain, {} lines", parse::non_empty_plain(&text)),
                LyricsResult::Unavailable => println!("   -> unavailable"),
                LyricsResult::Failed(e) => println!("   -> FAILED {}", e.ui_string()),
            }
        }
    }
}
