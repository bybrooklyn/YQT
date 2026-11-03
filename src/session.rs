// SPDX-License-Identifier: GPL-3.0-only
//! The saved listening session: queue, current track and position, restored on launch.
//! Pure file handling – no Qt – so it is unit-tested; `Player` just calls it.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub items: Vec<Value>,
    pub index: usize,
    /// Playback position in milliseconds.
    pub position: i64,
}

impl Session {
    pub fn default_path() -> PathBuf {
        crate::state::data_dir().join("session.json")
    }

    /// Read a session. Anything unusable (missing, corrupt, empty queue) is simply "no session".
    pub fn load_from(path: &Path) -> Option<Session> {
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        let items = doc["items"].as_array().filter(|a| !a.is_empty())?.clone();
        let index = (doc["index"].as_u64().unwrap_or(0) as usize).min(items.len() - 1);
        Some(Session { items, index, position: doc["position"].as_i64().unwrap_or(0).max(0) })
    }

    /// Write via a temp file + rename, so a crash mid-write can't leave a half-written session.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let doc = json!({"items": self.items, "index": self.index, "position": self.position.max(0)});
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, doc.to_string())?;
        std::fs::rename(&tmp, path)
    }

    pub fn clear_at(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    pub fn load() -> Option<Session> {
        Self::load_from(&Self::default_path())
    }
    pub fn save(&self) {
        let _ = self.save_to(&Self::default_path());
    }
    pub fn clear() {
        Self::clear_at(&Self::default_path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ytm-session-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("session.json")
    }

    fn songs(n: usize) -> Vec<Value> {
        (0..n).map(|i| json!({"title": format!("t{i}"), "videoId": format!("v{i}")})).collect()
    }

    #[test]
    fn round_trip() {
        let p = tmp("rt");
        let s = Session { items: songs(3), index: 2, position: 42_000 };
        s.save_to(&p).unwrap();
        assert_eq!(Session::load_from(&p), Some(s));
        assert!(!p.with_extension("json.tmp").exists(), "temp file must not linger");
    }

    #[test]
    fn missing_file_is_no_session() {
        assert_eq!(Session::load_from(&tmp("missing")), None);
    }

    #[test]
    fn corrupt_file_is_no_session() {
        let p = tmp("corrupt");
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(Session::load_from(&p), None);
    }

    #[test]
    fn empty_queue_is_no_session() {
        let p = tmp("empty");
        std::fs::write(&p, r#"{"items": [], "index": 0, "position": 5}"#).unwrap();
        assert_eq!(Session::load_from(&p), None);
    }

    #[test]
    fn index_and_position_are_sanitised() {
        let p = tmp("clamp");
        std::fs::write(&p, serde_json::to_string(&json!({"items": songs(2), "index": 99, "position": -7})).unwrap()).unwrap();
        let s = Session::load_from(&p).unwrap();
        assert_eq!((s.index, s.position), (1, 0));
    }

    #[test]
    fn missing_fields_default_sensibly() {
        let p = tmp("partial");
        std::fs::write(&p, serde_json::to_string(&json!({"items": songs(1)})).unwrap()).unwrap();
        let s = Session::load_from(&p).unwrap();
        assert_eq!((s.index, s.position), (0, 0));
    }

    #[test]
    fn clear_removes_the_file() {
        let p = tmp("clear");
        Session { items: songs(1), index: 0, position: 0 }.save_to(&p).unwrap();
        Session::clear_at(&p);
        assert!(!p.exists());
        Session::clear_at(&p); // clearing twice is fine
    }
}
