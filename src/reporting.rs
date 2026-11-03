// SPDX-License-Identifier: GPL-3.0-only
//! Play reporting: tell YouTube Music that a song was listened to, so it shows up in History and
//! feeds recommendations.
//!
//! Mechanism (as in ytmusicapi `add_history_item`): take `playbackTracking.videostatsPlaybackUrl.baseUrl`
//! from a signed-in `/player` response and GET it with `ver=2&c=WEB_REMIX&cpn=<16 random chars>`;
//! HTTP 204 means success.
//!
//! * [`PlayTracker`] – pure state machine deciding *when* a play counts (actually listened time).
//! * [`report_play`] / [`report_with`] – the network side, with retries; never blocks playback.
//! * `Reporter` – the QML singleton that feeds the tracker (`trackChanged`, `tick`).
//!
//! `reporting.watchtime` (periodic watch-time pings) is **not implemented**: the ping parameters
//! could not be verified from any source. The config flag exists but currently does nothing.
use crate::config;
use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt_lib::QString;
use rand::Rng;
use std::time::Duration;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        type Reporter = super::ReporterRust;

        /// The current track changed (empty id = nothing playing). Starts a new play.
        #[qinvokable]
        fn track_changed(self: Pin<&mut Reporter>, video_id: &QString);
        /// Playback position report, e.g. every second. Fires the report once enough was listened to.
        #[qinvokable]
        fn tick(self: Pin<&mut Reporter>, position_ms: i64, playing: bool);
    }
}

pub struct ReporterRust {
    tracker: PlayTracker,
    started: std::time::Instant,
}

impl Default for ReporterRust {
    fn default() -> Self {
        let r = &config::get().reporting;
        Self { tracker: PlayTracker::new(r.min_play_secs, r.enabled), started: std::time::Instant::now() }
    }
}

impl qobject::Reporter {
    fn track_changed(self: Pin<&mut Self>, video_id: &QString) {
        let r = &config::get().reporting;
        // Signed out → never report. Checked per track so signing in/out takes effect immediately.
        let active = r.enabled && crate::auth::stored_cookie().is_some();
        let me = self.rust_mut().get_mut();
        me.tracker.set_min_play_secs(r.min_play_secs);
        me.tracker.set_active(active);
        me.tracker.track_changed(&video_id.to_string());
    }

    fn tick(self: Pin<&mut Self>, position_ms: i64, playing: bool) {
        let me = self.rust_mut().get_mut();
        let now_ms = i64::try_from(me.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        if let Some(id) = me.tracker.tick(position_ms, playing, now_ms) {
            spawn_report(id);
        }
    }
}

/// Fire-and-forget on the app's runtime; failures are only logged (without any URL or cookie).
pub fn spawn_report(video_id: String) {
    crate::state::st().rt.spawn(async move {
        if let Err(e) = report_play(&video_id).await {
            eprintln!("play report for {video_id} failed: {e}");
        }
    });
}

// ---- cpn and query parameters ------------------------------------------------------------------

/// The 64 characters a client playback nonce is made of.
pub const CPN_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
pub const CPN_LEN: usize = 16;

/// A client playback nonce: 16 characters, each uniform over the 64-char alphabet.
pub fn new_cpn() -> String {
    cpn_from(&mut rand::rng())
}

pub fn cpn_from(rng: &mut impl Rng) -> String {
    (0..CPN_LEN).map(|_| CPN_ALPHABET[rng.random_range(0..CPN_ALPHABET.len())] as char).collect()
}

/// The query parameters the tracking GET needs on top of what the base URL already carries.
pub fn report_params(cpn: &str) -> [(&'static str, String); 3] {
    [("ver", "2".to_owned()), ("c", "WEB_REMIX".to_owned()), ("cpn", cpn.to_owned())]
}

/// `base` with `ver`, `c` and `cpn` set (replacing any the base already has).
pub fn with_report_params(base: &str, cpn: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(base).map_err(|_| "invalid tracking URL".to_owned())?;
    let params = report_params(cpn);
    let kept: Vec<(String, String)> =
        url.query_pairs().filter(|(k, _)| !params.iter().any(|(p, _)| p == k)).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    {
        let mut q = url.query_pairs_mut();
        q.clear();
        q.extend_pairs(kept);
        q.extend_pairs(params);
    }
    Ok(url)
}

/// Delay before try number `attempt + 1` (attempt 0 = after the first failure): 0.5 s, 1 s, 2 s … max 8 s.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_millis((500u64 << attempt.min(4)).min(8_000))
}

// ---- when does a play count --------------------------------------------------------------------

/// A forward jump bigger than the elapsed wall time by more than this is a seek, not listening.
const SEEK_TOLERANCE_MS: i64 = 1_000;
/// Going back at least this far, to near the start, after a play was reported = the song restarted.
const RESTART_REWIND_MS: i64 = 10_000;
const RESTART_ZONE_MS: i64 = 5_000;

#[derive(Debug, Clone, Copy)]
struct Last {
    pos: i64,
    playing: bool,
    now: i64,
}

/// Decides when a play is worth reporting. Feed it `track_changed` and `tick`; it hands back the
/// video id exactly once per play, after `min_play_secs` of *actually listened* time (time only counts
/// between two consecutive ticks that are both playing and moved forward in step with the clock –
/// pauses and seeks add nothing).
#[derive(Debug)]
pub struct PlayTracker {
    active: bool,
    min_ms: i64,
    video: Option<String>,
    listened_ms: i64,
    reported: bool,
    last: Option<Last>,
}

impl PlayTracker {
    pub fn new(min_play_secs: u64, active: bool) -> Self {
        PlayTracker { active, min_ms: secs_to_ms(min_play_secs), video: None, listened_ms: 0, reported: false, last: None }
    }

    /// `false` when reporting is disabled or nobody is signed in: nothing is ever reported.
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    pub fn set_min_play_secs(&mut self, secs: u64) {
        self.min_ms = secs_to_ms(secs);
    }

    /// A new track (or the same one again) starts: everything resets.
    pub fn track_changed(&mut self, video_id: &str) {
        self.video = (!video_id.is_empty()).then(|| video_id.to_owned());
        self.listened_ms = 0;
        self.reported = false;
        self.last = None;
    }

    /// Returns the video id when this tick makes the current play count.
    pub fn tick(&mut self, position_ms: i64, playing: bool, now_ms: i64) -> Option<String> {
        let video = self.video.clone()?;
        if let Some(prev) = self.last {
            if self.reported && position_ms + RESTART_REWIND_MS <= prev.pos && position_ms <= RESTART_ZONE_MS {
                // repeat-one (or a manual restart) after a counted play: a new play begins
                self.listened_ms = 0;
                self.reported = false;
            } else if playing && prev.playing {
                let moved = position_ms - prev.pos;
                let elapsed = now_ms - prev.now;
                if moved >= 0 && moved <= elapsed.max(0) + SEEK_TOLERANCE_MS {
                    self.listened_ms += moved;
                }
            }
        }
        self.last = Some(Last { pos: position_ms, playing, now: now_ms });
        if self.active && playing && !self.reported && self.listened_ms >= self.min_ms {
            self.reported = true;
            return Some(video);
        }
        None
    }
}

fn secs_to_ms(s: u64) -> i64 {
    i64::try_from(s.saturating_mul(1000)).unwrap_or(i64::MAX)
}

// ---- the report itself -------------------------------------------------------------------------

/// Everything `report_with` depends on, injectable for tests.
pub struct Deps {
    pub client: reqwest::Client,
    /// May this URL receive the user's cookies?
    pub allow_url: Box<dyn Fn(&str) -> bool + Send + Sync>,
    pub delay: Box<dyn Fn(u32) -> Duration + Send + Sync>,
}

impl Deps {
    pub fn production() -> Deps {
        Deps { client: crate::state::http().clone(), allow_url: Box::new(crate::ytapi::is_trusted_youtube_url), delay: Box::new(backoff) }
    }
}

fn retryable(status: u16) -> bool {
    status == 429 || status >= 500
}

/// GET the tracking URL (with the report params and `headers`), trying up to `tries` times.
/// Errors never contain the URL or any header value.
pub async fn report_with(deps: &Deps, tracking_url: &str, headers: &[(&'static str, String)], tries: u32) -> Result<(), String> {
    if !(deps.allow_url)(tracking_url) {
        return Err("refusing to report to an untrusted host".into());
    }
    let url = with_report_params(tracking_url, &new_cpn())?;
    let tries = tries.max(1);
    let mut last = String::new();
    for attempt in 0..tries {
        if attempt > 0 {
            tokio::time::sleep((deps.delay)(attempt - 1)).await;
        }
        let mut req = deps.client.get(url.clone());
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        match req.send().await {
            Ok(resp) => {
                let s = resp.status().as_u16();
                if (200..300).contains(&s) {
                    return Ok(()); // YouTube answers 204
                }
                last = format!("HTTP {s}");
                if !retryable(s) {
                    break;
                }
            }
            Err(e) => last = e.without_url().to_string(),
        }
    }
    Err(format!("{last} (after {tries} tries at most)"))
}

/// Report one play of `video_id` (signed-in WEB_REMIX metadata call, then the tracking GET).
/// Honours `[reporting]`; returns a `Result` only so the caller can log it.
pub async fn report_play(video_id: &str) -> Result<(), String> {
    let cfg = &config::get().reporting;
    if !cfg.enabled {
        return Ok(());
    }
    let url = crate::ytapi::player_tracking_url(video_id).await?;
    let headers = crate::ytapi::auth_headers()?;
    report_with(&Deps::production(), &url, &headers, cfg.retries).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // ---- cpn / params / backoff

    #[test]
    fn cpn_has_16_alphabet_chars_and_varies() {
        let a = new_cpn();
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| CPN_ALPHABET.contains(&b)), "{a}");
        assert_ne!(a, new_cpn());
        assert_eq!(CPN_ALPHABET.iter().collect::<std::collections::HashSet<_>>().len(), 64);
    }

    #[test]
    fn cpn_covers_the_whole_alphabet_roughly_uniformly() {
        let mut counts = std::collections::HashMap::new();
        for _ in 0..4000 {
            for c in new_cpn().chars() {
                *counts.entry(c).or_insert(0u32) += 1;
            }
        }
        assert_eq!(counts.len(), 64);
        // expected 1000 each; a biased `&63` of a byte would still pass this, uniform must stay within ±25%
        assert!(counts.values().all(|&n| (750..1250).contains(&n)), "{counts:?}");
    }

    #[test]
    fn params_are_set_and_replace_existing() {
        assert_eq!(report_params("abc"), [("ver", "2".to_owned()), ("c", "WEB_REMIX".to_owned()), ("cpn", "abc".to_owned())]);
        let u = with_report_params("https://s.youtube.com/api/stats/playback?docid=1&ei=x&cpn=old&ver=9", "NEWCPN").unwrap();
        assert_eq!(u.path(), "/api/stats/playback");
        let q: Vec<(String, String)> = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert_eq!(q.iter().filter(|(k, _)| k == "cpn").count(), 1);
        assert!(q.contains(&("docid".into(), "1".into())));
        assert!(q.contains(&("ver".into(), "2".into())));
        assert!(q.contains(&("c".into(), "WEB_REMIX".into())));
        assert!(q.contains(&("cpn".into(), "NEWCPN".into())));
        assert!(with_report_params("not a url", "x").is_err());
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let ms: Vec<u128> = (0..8).map(|a| backoff(a).as_millis()).collect();
        assert_eq!(ms, [500, 1000, 2000, 4000, 8000, 8000, 8000, 8000]);
        assert_eq!(backoff(u32::MAX).as_millis(), 8000);
    }

    // ---- tracker

    #[derive(Clone, Copy)]
    enum Ev {
        Track(&'static str),
        /// position ms, playing, clock ms
        Tick(i64, bool, i64),
    }
    use Ev::*;

    /// Run events; return (event index, video id) of every report.
    fn run(min_secs: u64, active: bool, evs: &[Ev]) -> Vec<(usize, String)> {
        let mut t = PlayTracker::new(min_secs, active);
        let mut out = Vec::new();
        for (i, e) in evs.iter().enumerate() {
            match *e {
                Track(id) => t.track_changed(id),
                Tick(p, pl, now) => {
                    if let Some(v) = t.tick(p, pl, now) {
                        out.push((i, v));
                    }
                }
            }
        }
        out
    }

    /// `n` playing ticks one second apart starting at position/clock 0.
    fn listen(n: i64) -> Vec<Ev> {
        (0..=n).map(|s| Tick(s * 1000, true, s * 1000)).collect()
    }

    fn with(first: Ev, rest: Vec<Ev>) -> Vec<Ev> {
        std::iter::once(first).chain(rest).collect()
    }

    #[test]
    fn reports_once_after_the_threshold() {
        let evs = with(Track("a"), listen(40));
        // tick index 0 is Track; ticks s=0..=40 are events 1..=41; 30 s listened at s=30 → event 31
        assert_eq!(run(30, true, &evs), vec![(31, "a".to_owned())]);
    }

    #[test]
    fn short_listens_never_report() {
        assert!(run(30, true, &with(Track("a"), listen(29))).is_empty());
    }

    #[test]
    fn skipping_before_the_threshold_never_reports_and_new_track_resets() {
        let mut evs = with(Track("a"), listen(20));
        evs.push(Track("b"));
        evs.extend(listen(20));
        assert!(run(30, true, &evs).is_empty(), "20+20 s on two tracks must not add up");
        evs.push(Track("c"));
        evs.extend(listen(31));
        let r = run(30, true, &evs);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].1, "c");
    }

    #[test]
    fn pauses_do_not_count() {
        // 20 s playing, 100 s paused (position frozen), then 15 s playing: 35 s total → report only at the end
        let mut evs = with(Track("a"), listen(20));
        for s in 21..=120 {
            evs.push(Tick(20_000, false, s * 1000));
        }
        for s in 0..=15 {
            evs.push(Tick(20_000 + s * 1000, true, (121 + s) * 1000));
        }
        let r = run(30, true, &evs);
        assert_eq!(r.len(), 1);
        // the first playing tick after the pause counts nothing, so 20 + 10 s → report on the 11th second after resume
        assert_eq!(r[0].0, 1 + 21 + 100 + 10);
    }

    #[test]
    fn seeks_and_jumps_do_not_count() {
        // 10 s listened, jump forward 120 s, listen 10 more: 20 s < 30
        let mut evs = with(Track("a"), listen(10));
        evs.push(Tick(130_000, true, 11_000));
        for s in 1..=10 {
            evs.push(Tick(130_000 + s * 1000, true, 11_000 + s * 1000));
        }
        assert!(run(30, true, &evs).is_empty());
        // seeking backwards is not listening either
        let mut evs = with(Track("a"), listen(10));
        evs.push(Tick(2_000, true, 11_000));
        assert!(run(30, true, &evs).is_empty());
    }

    #[test]
    fn scrubbing_forward_cannot_fake_a_play() {
        let evs = with(Track("a"), (0..100).map(|s| Tick(s * 30_000, true, s * 1000)).collect());
        assert!(run(30, true, &evs).is_empty());
    }

    #[test]
    fn repeat_one_starts_a_new_play() {
        // 60 s song looped: play through (report), wrap to 0, play through again (report again)
        let mut evs = with(Track("a"), (0..=60).map(|s| Tick(s * 1000, true, s * 1000)).collect());
        evs.extend((0..=60).map(|s| Tick(s * 1000, true, (61 + s) * 1000)));
        let r = run(30, true, &evs);
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(r.iter().all(|(_, v)| v == "a"));
    }

    #[test]
    fn seek_back_before_reporting_is_not_a_new_play() {
        let mut evs = with(Track("a"), listen(20));
        evs.push(Tick(0, true, 21_000)); // back to start, not yet reported
        evs.extend((1..=15).map(|s| Tick(s * 1000, true, 21_000 + s * 1000)));
        assert_eq!(run(30, true, &evs).len(), 1); // 20 + 15 s counted as one play
    }

    #[test]
    fn never_when_disabled_or_signed_out() {
        assert!(run(30, false, &with(Track("a"), listen(100))).is_empty());
    }

    #[test]
    fn ticks_without_a_track_are_ignored() {
        assert!(run(30, true, &listen(100)).is_empty());
        assert!(run(30, true, &with(Track(""), listen(100))).is_empty());
    }

    #[test]
    fn becoming_active_mid_track_counts_from_then() {
        let mut t = PlayTracker::new(30, false);
        t.track_changed("a");
        for s in 0..=40 {
            assert!(t.tick(s * 1000, true, s * 1000).is_none());
        }
        t.set_active(true);
        // already past the threshold: reports on the next playing tick, once
        assert_eq!(t.tick(41_000, true, 41_000).as_deref(), Some("a"));
        assert_eq!(t.tick(42_000, true, 42_000), None);
    }

    #[test]
    fn same_track_again_is_a_new_play() {
        let mut evs = with(Track("a"), listen(31));
        evs.push(Track("a"));
        evs.extend(listen(31));
        assert_eq!(run(30, true, &evs).len(), 2);
    }

    #[test]
    fn zero_threshold_reports_on_first_playing_tick_only() {
        let evs = with(Track("a"), vec![Tick(0, false, 0), Tick(0, true, 1000), Tick(1000, true, 2000)]);
        assert_eq!(run(0, true, &evs), vec![(2, "a".to_owned())]);
    }

    // ---- network, against a local mock server

    type Seen = Arc<Mutex<Vec<String>>>;

    /// Serves one scripted status per connection; records each request head.
    async fn mock(statuses: Vec<u16>) -> (String, Seen) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Seen = Arc::default();
        let seen2 = seen.clone();
        tokio::spawn(async move {
            for status in statuses {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                seen2.lock().unwrap().push(String::from_utf8_lossy(&buf).into_owned());
                let reason = if status == 204 { "No Content" } else { "Status" };
                let resp = format!("HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}/api/stats/playback?docid=abc&ei=xyz"), seen)
    }

    fn test_deps(allow: impl Fn(&str) -> bool + Send + Sync + 'static) -> Deps {
        Deps { client: reqwest::Client::new(), allow_url: Box::new(allow), delay: Box::new(|_| Duration::from_millis(1)) }
    }

    fn allow_local(u: &str) -> bool {
        u.starts_with("http://127.0.0.1:")
    }

    fn hdrs() -> Vec<(&'static str, String)> {
        vec![
            ("Cookie", "SAPISID=fake-sapisid; SID=fake".into()),
            ("Authorization", "SAPISIDHASH 1_deadbeef".into()),
            ("X-Origin", "https://music.youtube.com".into()),
        ]
    }

    #[tokio::test]
    async fn sends_the_exact_get_and_accepts_204() {
        let (url, seen) = mock(vec![204]).await;
        report_with(&test_deps(allow_local), &url, &hdrs(), 3).await.unwrap();
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        let head = &reqs[0];
        let line = head.lines().next().unwrap();
        assert!(line.starts_with("GET /api/stats/playback?"), "{line}");
        assert!(line.ends_with(" HTTP/1.1"), "{line}");
        let query = line.split_once('?').unwrap().1.trim_end_matches(" HTTP/1.1");
        let pairs: Vec<(&str, &str)> = query.split('&').filter_map(|p| p.split_once('=')).collect();
        assert!(pairs.contains(&("docid", "abc")) && pairs.contains(&("ei", "xyz")));
        assert!(pairs.contains(&("ver", "2")) && pairs.contains(&("c", "WEB_REMIX")));
        let cpn = pairs.iter().find(|(k, _)| *k == "cpn").unwrap().1;
        assert_eq!(cpn.len(), 16);
        assert!(cpn.bytes().all(|b| CPN_ALPHABET.contains(&b)));
        let lower = head.to_lowercase();
        assert!(lower.contains("cookie: sapisid=fake-sapisid; sid=fake"), "{head}");
        assert!(lower.contains("authorization: sapisidhash 1_deadbeef"), "{head}");
        assert!(lower.contains("x-origin: https://music.youtube.com"), "{head}");
    }

    #[tokio::test]
    async fn retries_on_500_then_succeeds_with_the_same_cpn() {
        let (url, seen) = mock(vec![500, 503, 204]).await;
        report_with(&test_deps(allow_local), &url, &hdrs(), 3).await.unwrap();
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        let cpn = |r: &str| r.split("cpn=").nth(1).unwrap().split(['&', ' ']).next().unwrap().to_owned();
        assert_eq!(cpn(&reqs[0]), cpn(&reqs[2]));
    }

    #[tokio::test]
    async fn gives_up_after_the_configured_tries() {
        let (url, seen) = mock(vec![500, 500, 500, 204]).await;
        let e = report_with(&test_deps(allow_local), &url, &hdrs(), 3).await.unwrap_err();
        assert!(e.contains("HTTP 500"), "{e}");
        assert_eq!(seen.lock().unwrap().len(), 3);
        // the error leaks neither URL nor cookies
        assert!(!e.contains("127.0.0.1") && !e.contains("fake"), "{e}");
    }

    #[tokio::test]
    async fn client_errors_are_not_retried() {
        let (url, seen) = mock(vec![403, 204]).await;
        assert!(report_with(&test_deps(allow_local), &url, &hdrs(), 5).await.is_err());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn untrusted_hosts_are_refused_before_any_request() {
        let (url, seen) = mock(vec![204]).await;
        // production predicate: http://127.0.0.1 is not a trusted https YouTube URL
        let e =
            report_with(&Deps { allow_url: Box::new(crate::ytapi::is_trusted_youtube_url), ..test_deps(allow_local) }, &url, &hdrs(), 3)
                .await
                .unwrap_err();
        assert!(e.contains("untrusted"), "{e}");
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn connection_failures_are_reported_without_the_url() {
        // nothing listens on this port
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let e = report_with(&test_deps(allow_local), &format!("http://127.0.0.1:{port}/x?docid=SECRET"), &hdrs(), 2).await.unwrap_err();
        assert!(!e.contains("SECRET") && !e.contains(&port.to_string()), "{e}");
    }
}
