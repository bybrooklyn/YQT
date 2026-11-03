// SPDX-License-Identifier: GPL-3.0-only
//! SQLite index of the downloaded songs: enough metadata to show the Downloads page with no network.
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

/// One downloaded song.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Song {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub cover: String,
    pub duration_ms: i64,
    pub size: i64,
    /// File extension (`m4a`, `webm`, …); the file is `<dir>/<videoId>.<format>`.
    pub format: String,
    pub mime: String,
    /// Unix seconds.
    pub added: i64,
    pub last_played: Option<i64>,
    /// Pinned directly or because it belongs to a "keep offline" collection.
    pub pinned: bool,
}

pub struct Store {
    db: Connection,
}

const COLUMNS: &str = "video_id, title, artist, album, cover, duration_ms, size, format, mime, added, last_played, \
    (pinned OR EXISTS (SELECT 1 FROM pins WHERE pins.video_id = songs.video_id))";

fn row_to_song(r: &rusqlite::Row<'_>) -> rusqlite::Result<Song> {
    Ok(Song {
        video_id: r.get(0)?,
        title: r.get(1)?,
        artist: r.get(2)?,
        album: r.get(3)?,
        cover: r.get(4)?,
        duration_ms: r.get(5)?,
        size: r.get(6)?,
        format: r.get(7)?,
        mime: r.get(8)?,
        added: r.get(9)?,
        last_played: r.get(10)?,
        pinned: r.get::<_, i64>(11)? != 0,
    })
}

impl Store {
    /// Open (creating) the index at `path`; the file is made private.
    pub fn open(path: &Path) -> rusqlite::Result<Store> {
        if let Some(dir) = path.parent() {
            let _ = crate::state::ensure_private_dir(dir);
        }
        let s = Store::init(Connection::open(path)?)?;
        crate::state::tighten(path);
        Ok(s)
    }

    #[cfg(test)]
    pub fn open_memory() -> rusqlite::Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> rusqlite::Result<Store> {
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS songs (
                video_id TEXT PRIMARY KEY, title TEXT NOT NULL, artist TEXT NOT NULL, album TEXT NOT NULL,
                cover TEXT NOT NULL, duration_ms INTEGER NOT NULL, size INTEGER NOT NULL, format TEXT NOT NULL,
                mime TEXT NOT NULL, added INTEGER NOT NULL, last_played INTEGER, pinned INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS pins (
                collection TEXT NOT NULL, video_id TEXT NOT NULL, item TEXT NOT NULL,
                PRIMARY KEY (collection, video_id)
             );",
        )?;
        Ok(Store { db })
    }

    /// Insert or replace a song (keeps `pinned` and `last_played` of an existing row).
    pub fn upsert(&self, s: &Song) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO songs (video_id, title, artist, album, cover, duration_ms, size, format, mime, added, last_played, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(video_id) DO UPDATE SET title = ?2, artist = ?3, album = ?4, cover = ?5, duration_ms = ?6,
                size = ?7, format = ?8, mime = ?9",
            params![
                s.video_id,
                s.title,
                s.artist,
                s.album,
                s.cover,
                s.duration_ms,
                s.size,
                s.format,
                s.mime,
                s.added,
                s.last_played,
                i64::from(s.pinned)
            ],
        )?;
        Ok(())
    }

    #[allow(dead_code)] // used by the tests and by future per-song lookups
    pub fn get(&self, id: &str) -> rusqlite::Result<Option<Song>> {
        self.db.query_row(&format!("SELECT {COLUMNS} FROM songs WHERE video_id = ?1"), [id], row_to_song).optional()
    }

    /// Newest first.
    pub fn list(&self) -> rusqlite::Result<Vec<Song>> {
        let mut st = self.db.prepare(&format!("SELECT {COLUMNS} FROM songs ORDER BY added DESC, video_id"))?;
        let rows = st.query_map([], row_to_song)?;
        rows.collect()
    }

    pub fn remove(&self, id: &str) -> rusqlite::Result<bool> {
        Ok(self.db.execute("DELETE FROM songs WHERE video_id = ?1", [id])? > 0)
    }

    pub fn total_bytes(&self) -> rusqlite::Result<i64> {
        self.db.query_row("SELECT COALESCE(SUM(size), 0) FROM songs", [], |r| r.get(0))
    }

    pub fn touch_played(&self, id: &str, now: i64) -> rusqlite::Result<()> {
        self.db.execute("UPDATE songs SET last_played = ?2 WHERE video_id = ?1", params![id, now])?;
        Ok(())
    }

    /// Pin or unpin one song directly.
    pub fn set_pinned(&self, id: &str, on: bool) -> rusqlite::Result<bool> {
        Ok(self.db.execute("UPDATE songs SET pinned = ?2 WHERE video_id = ?1", params![id, i64::from(on)])? > 0)
    }

    /// "Keep offline": remember the members of a collection (`items` = (video id, item JSON)); `on = false` forgets it.
    pub fn set_collection(&self, collection: &str, items: &[(String, String)], on: bool) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM pins WHERE collection = ?1", [collection])?;
        if on {
            for (id, item) in items {
                self.db.execute(
                    "INSERT OR REPLACE INTO pins (collection, video_id, item) VALUES (?1, ?2, ?3)",
                    params![collection, id, item],
                )?;
            }
        }
        Ok(())
    }

    pub fn is_collection_kept(&self, collection: &str) -> rusqlite::Result<bool> {
        self.db.query_row("SELECT EXISTS (SELECT 1 FROM pins WHERE collection = ?1)", [collection], |r| r.get::<_, i64>(0)).map(|n| n != 0)
    }

    /// Every song of every kept collection: (video id, item JSON).
    pub fn pinned_members(&self) -> rusqlite::Result<Vec<(String, String)>> {
        let mut st = self.db.prepare("SELECT video_id, item FROM pins GROUP BY video_id ORDER BY MIN(rowid)")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, size: i64, added: i64) -> Song {
        Song {
            video_id: id.into(),
            title: format!("Title {id}"),
            artist: "Artist".into(),
            album: "Album".into(),
            cover: "https://example.com/c.jpg".into(),
            duration_ms: 200_000,
            size,
            format: "m4a".into(),
            mime: "audio/mp4".into(),
            added,
            last_played: None,
            pinned: false,
        }
    }

    #[test]
    fn crud_round_trip() {
        let s = Store::open_memory().unwrap();
        assert!(s.list().unwrap().is_empty());
        assert_eq!(s.total_bytes().unwrap(), 0);
        s.upsert(&song("aaaaaaaaaaa", 100, 10)).unwrap();
        s.upsert(&song("bbbbbbbbbbb", 250, 20)).unwrap();
        assert_eq!(s.get("aaaaaaaaaaa").unwrap().unwrap(), song("aaaaaaaaaaa", 100, 10));
        assert_eq!(s.get("zzzzzzzzzzz").unwrap(), None);
        let ids: Vec<_> = s.list().unwrap().into_iter().map(|x| x.video_id).collect();
        assert_eq!(ids, ["bbbbbbbbbbb", "aaaaaaaaaaa"], "newest first");
        assert_eq!(s.total_bytes().unwrap(), 350);
        assert!(s.remove("aaaaaaaaaaa").unwrap());
        assert!(!s.remove("aaaaaaaaaaa").unwrap());
        assert_eq!(s.total_bytes().unwrap(), 250);
    }

    #[test]
    fn upsert_keeps_play_history_and_pin() {
        let s = Store::open_memory().unwrap();
        s.upsert(&song("aaaaaaaaaaa", 100, 10)).unwrap();
        s.touch_played("aaaaaaaaaaa", 99).unwrap();
        assert!(s.set_pinned("aaaaaaaaaaa", true).unwrap());
        let mut again = song("aaaaaaaaaaa", 120, 50);
        again.title = "New title".into();
        s.upsert(&again).unwrap();
        let got = s.get("aaaaaaaaaaa").unwrap().unwrap();
        assert_eq!((got.title.as_str(), got.size, got.added, got.last_played, got.pinned), ("New title", 120, 10, Some(99), true));
        assert!(!s.set_pinned("zzzzzzzzzzz", true).unwrap());
    }

    #[test]
    fn collections_pin_their_members() {
        let s = Store::open_memory().unwrap();
        s.upsert(&song("aaaaaaaaaaa", 1, 1)).unwrap();
        s.upsert(&song("bbbbbbbbbbb", 1, 2)).unwrap();
        let items = vec![("aaaaaaaaaaa".to_owned(), "{}".to_owned()), ("ccccccccccc".to_owned(), "{\"x\":1}".to_owned())];
        s.set_collection("PL1", &items, true).unwrap();
        assert!(s.is_collection_kept("PL1").unwrap() && !s.is_collection_kept("PL2").unwrap());
        assert!(s.get("aaaaaaaaaaa").unwrap().unwrap().pinned);
        assert!(!s.get("bbbbbbbbbbb").unwrap().unwrap().pinned);
        assert_eq!(s.pinned_members().unwrap().len(), 2, "includes the not-yet-downloaded member");
        s.set_collection("PL1", &[], false).unwrap();
        assert!(!s.get("aaaaaaaaaaa").unwrap().unwrap().pinned);
        assert!(s.pinned_members().unwrap().is_empty());
    }

    #[test]
    fn a_song_in_two_collections_stays_pinned_until_both_are_dropped() {
        let s = Store::open_memory().unwrap();
        s.upsert(&song("aaaaaaaaaaa", 1, 1)).unwrap();
        let it = vec![("aaaaaaaaaaa".to_owned(), "{}".to_owned())];
        s.set_collection("A", &it, true).unwrap();
        s.set_collection("B", &it, true).unwrap();
        s.set_collection("A", &[], false).unwrap();
        assert!(s.get("aaaaaaaaaaa").unwrap().unwrap().pinned);
        s.set_collection("B", &[], false).unwrap();
        assert!(!s.get("aaaaaaaaaaa").unwrap().unwrap().pinned);
        assert_eq!(s.pinned_members().unwrap().len(), 0);
    }

    #[test]
    fn the_index_file_is_private_and_reopens() {
        let dir = std::env::temp_dir().join(format!("ytm-dl-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("downloads.sqlite");
        {
            let s = Store::open(&path).unwrap();
            s.upsert(&song("aaaaaaaaaaa", 5, 1)).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        }
        assert_eq!(Store::open(&path).unwrap().list().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
