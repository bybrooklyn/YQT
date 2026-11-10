// SPDX-License-Identifier: GPL-3.0-only
//! Lyrics cache: a short-lived in-memory map and `<cache dir>/yqt/lyrics/<hash>.json` on disk.
//!
//! Only found lyrics go to disk (a "nobody has it" answer may change, so it is only remembered for a few
//! minutes in memory); failures are never cached. Files are private (0600 in a 0700 folder).
use super::parse::Line;
use super::rank::Query;
use super::LyricsResult;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MEM_FOUND: Duration = Duration::from_secs(3600);
const MEM_UNAVAILABLE: Duration = Duration::from_secs(600);
const MEM_MAX: usize = 64;
const FORMAT: u32 = 1;

/// Cache key: video id + normalised title/artist + the provider set + the junk threshold, so changing the
/// settings never serves an answer that those settings would not have produced.
#[cfg(test)]
pub fn key(q: &Query, providers: &[&str], min_lines: usize) -> String {
    key_with(q, providers, min_lines, "")
}

/// [`key`] plus `settings`: every other setting that changes the answer (see [`settings_tag`]).
pub fn key_with(q: &Query, providers: &[&str], min_lines: usize, settings: &str) -> String {
    let text = format!(
        "{FORMAT}|{}|{}|{}|{}|{}|{min_lines}|{settings}",
        q.video_id,
        super::rank::normalize(&q.title),
        super::rank::normalize(&q.artist),
        q.duration_s / 5,
        providers.join(",")
    );
    Sha1::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The settings (besides the provider list and `min_lines`) that decide which lyrics win or where they come from.
pub fn settings_tag(cfg: &crate::config::Lyrics) -> String {
    format!("{}|{}|{}", cfg.prefer_synced, cfg.lrclib_url.trim(), cfg.netease_url.trim())
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `<cache dir>/yqt/lyrics`.
pub fn default_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("yqt").join("lyrics"))
}

#[derive(Serialize, Deserialize)]
struct Stored {
    v: u32,
    saved: u64,
    source: String,
    /// `[ms, text]` pairs; empty for plain lyrics
    lines: Vec<Line>,
    text: String,
}

fn path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.json"))
}

/// Read a cached result. Expired, unreadable or foreign files are ignored (expired ones deleted).
pub fn load(dir: &Path, key: &str, days: u32, now: u64) -> Option<LyricsResult> {
    if days == 0 || key.len() != 40 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let p = path(dir, key);
    let bytes = std::fs::read(&p).ok()?;
    let s: Stored = serde_json::from_slice(&bytes).ok()?;
    if s.v != FORMAT {
        return None;
    }
    if now.saturating_sub(s.saved) > u64::from(days) * 86_400 {
        let _ = std::fs::remove_file(&p);
        return None;
    }
    if !s.lines.is_empty() {
        Some(LyricsResult::Synced { lines: s.lines, source: s.source })
    } else if !s.text.is_empty() {
        Some(LyricsResult::Plain { text: s.text, source: s.source })
    } else {
        None
    }
}

/// Write a found result. Best effort: a read-only disk just means no cache.
pub fn store(dir: &Path, key: &str, result: &LyricsResult, now: u64) {
    let (source, lines, text) = match result {
        LyricsResult::Synced { lines, source } => (source.clone(), lines.clone(), String::new()),
        LyricsResult::Plain { text, source } => (source.clone(), Vec::new(), text.clone()),
        LyricsResult::Unavailable | LyricsResult::Failed(_) => return,
    };
    let Ok(json) = serde_json::to_vec(&Stored { v: FORMAT, saved: now, source, lines, text }) else { return };
    if crate::state::ensure_private_dir(dir).is_err() {
        return;
    }
    let _ = crate::state::write_private(&path(dir, key), &json);
}

/// Process-wide short-term memory of answers.
pub struct Memory {
    map: Mutex<HashMap<String, (Instant, LyricsResult)>>,
}

impl Memory {
    pub fn new() -> Self {
        Memory { map: Mutex::new(HashMap::new()) }
    }

    pub fn get(&self, key: &str) -> Option<LyricsResult> {
        self.get_at(key, Instant::now())
    }

    pub fn get_at(&self, key: &str, now: Instant) -> Option<LyricsResult> {
        let mut m = self.map.lock().ok()?;
        let (at, r) = m.get(key)?;
        let ttl = if matches!(r, LyricsResult::Unavailable) { MEM_UNAVAILABLE } else { MEM_FOUND };
        if now.saturating_duration_since(*at) > ttl {
            m.remove(key);
            return None;
        }
        Some(r.clone())
    }

    pub fn put(&self, key: &str, r: LyricsResult) {
        self.put_at(key, r, Instant::now());
    }

    pub fn put_at(&self, key: &str, r: LyricsResult, now: Instant) {
        if matches!(r, LyricsResult::Failed(_)) {
            return;
        }
        let Ok(mut m) = self.map.lock() else { return };
        if m.len() >= MEM_MAX && !m.contains_key(key) {
            if let Some(oldest) = m.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone()) {
                m.remove(&oldest);
            }
        }
        m.insert(key.to_owned(), (now, r));
    }
}

pub fn memory() -> &'static Memory {
    static M: OnceLock<Memory> = OnceLock::new();
    M.get_or_init(Memory::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::{AppError, ErrorKind};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ytm-lyrics-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn q(id: &str) -> Query {
        Query { video_id: id.into(), title: "Song (Live)".into(), artist: "Band".into(), duration_s: 200 }
    }

    fn synced() -> LyricsResult {
        LyricsResult::Synced { lines: vec![(1000, "a".into()), (2000, "".into())], source: "LRCLIB".into() }
    }

    #[test]
    fn key_is_stable_hex_and_sensitive_to_settings() {
        let k = key(&q("x"), &["LRCLIB"], 4);
        assert_eq!(k.len(), 40);
        assert_eq!(k, key(&q("x"), &["LRCLIB"], 4));
        assert_ne!(k, key(&q("y"), &["LRCLIB"], 4));
        assert_ne!(k, key(&q("x"), &["LRCLIB", "NetEase"], 4));
        assert_ne!(k, key(&q("x"), &["LRCLIB"], 6));
        // cosmetic differences in the title do not matter
        let mut other = q("x");
        other.title = "song".into();
        assert_eq!(k, key(&other, &["LRCLIB"], 4));
    }

    #[test]
    fn key_follows_the_synced_preference_and_the_service_addresses() {
        let cfg = crate::config::Lyrics::default();
        let k = |c: &crate::config::Lyrics| key_with(&q("x"), &["LRCLIB"], 4, &settings_tag(c));
        assert_eq!(k(&cfg), k(&cfg.clone()));
        assert_ne!(k(&cfg), k(&crate::config::Lyrics { prefer_synced: !cfg.prefer_synced, ..cfg.clone() }));
        assert_ne!(k(&cfg), k(&crate::config::Lyrics { lrclib_url: "https://lrc.example".into(), ..cfg.clone() }));
        assert_ne!(k(&cfg), k(&crate::config::Lyrics { netease_url: "https://ne.example".into(), ..cfg.clone() }));
    }

    #[test]
    fn disk_roundtrip_expiry_and_private_perms() {
        let dir = tmp("disk");
        let k = key(&q("a"), &["LRCLIB"], 4);
        store(&dir, &k, &synced(), 1_000_000);
        assert_eq!(load(&dir, &k, 30, 1_000_000 + 86_400), Some(synced()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(path(&dir, &k)).unwrap().permissions().mode() & 0o077, 0);
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o077, 0);
        }
        // days = 0 means off
        assert_eq!(load(&dir, &k, 0, 1_000_000), None);
        // expired: gone, and the file is removed
        assert_eq!(load(&dir, &k, 30, 1_000_000 + 31 * 86_400), None);
        assert!(!path(&dir, &k).exists());
        let plain = LyricsResult::Plain { text: "x\ny".into(), source: "YouTube Music".into() };
        store(&dir, &k, &plain, 5);
        assert_eq!(load(&dir, &k, 1, 6), Some(plain));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_found_lyrics_go_to_disk_and_garbage_is_ignored() {
        let dir = tmp("neg");
        let k = key(&q("b"), &["LRCLIB"], 4);
        store(&dir, &k, &LyricsResult::Unavailable, 1);
        store(&dir, &k, &LyricsResult::Failed(AppError::new(ErrorKind::Network, "x", "y")), 1);
        assert!(!path(&dir, &k).exists());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(path(&dir, &k), b"not json").unwrap();
        assert_eq!(load(&dir, &k, 30, 1), None);
        assert_eq!(load(&dir, "../../etc/passwd", 30, 1), None); // keys are hashes only
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_ttls_and_no_failures() {
        let m = Memory::new();
        let t0 = Instant::now();
        m.put_at("a", synced(), t0);
        m.put_at("n", LyricsResult::Unavailable, t0);
        m.put_at("f", LyricsResult::Failed(AppError::new(ErrorKind::Network, "x", "y")), t0);
        assert_eq!(m.get_at("a", t0 + Duration::from_secs(60)), Some(synced()));
        assert_eq!(m.get_at("f", t0), None);
        assert_eq!(m.get_at("n", t0 + Duration::from_secs(60)), Some(LyricsResult::Unavailable));
        assert_eq!(m.get_at("n", t0 + Duration::from_secs(601)), None); // "unavailable" is forgotten sooner
        assert_eq!(m.get_at("a", t0 + Duration::from_secs(3601)), None);
    }

    #[test]
    fn memory_is_bounded() {
        let m = Memory::new();
        let t0 = Instant::now();
        for i in 0..(MEM_MAX + 10) {
            m.put_at(&format!("k{i}"), synced(), t0 + Duration::from_millis(i as u64));
        }
        assert!(m.map.lock().unwrap().len() <= MEM_MAX);
        assert!(m.get_at(&format!("k{}", MEM_MAX + 9), t0 + Duration::from_secs(1)).is_some());
        assert!(m.get_at("k0", t0 + Duration::from_secs(1)).is_none());
    }
}
