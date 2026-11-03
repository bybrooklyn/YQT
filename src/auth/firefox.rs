// SPDX-License-Identifier: GPL-3.0-only
//! Firefox-family browsers: unencrypted `moz_cookies` in `<profile>/cookies.sqlite`.
use super::{finish_header, with_private_copy};
use crate::auth::detect::{mtime, Dirs, Os};
use std::path::{Path, PathBuf};

/// (id, display name, profile roots relative to $HOME on Linux, profile roots on macOS / Windows)
pub struct Def {
    pub id: &'static str,
    pub name: &'static str,
    pub linux: &'static [&'static str],
    /// Relative to `~/Library/Application Support`.
    pub mac: &'static [&'static str],
    /// Relative to `%APPDATA%`.
    pub win: &'static [&'static str],
}

pub const DEFS: &[Def] = &[
    Def {
        id: "firefox",
        name: "Firefox",
        linux: &[".mozilla/firefox", ".var/app/org.mozilla.firefox/.mozilla/firefox", "snap/firefox/common/.mozilla/firefox"],
        mac: &["Firefox/Profiles"],
        win: &["Mozilla/Firefox/Profiles"],
    },
    Def { id: "zen", name: "Zen", linux: &[".zen", ".var/app/app.zen_browser.zen/.zen"], mac: &["zen/Profiles"], win: &["zen/Profiles"] },
    Def {
        id: "librewolf",
        name: "LibreWolf",
        linux: &[".librewolf", ".var/app/io.gitlab.librewolf-community/.librewolf"],
        mac: &["LibreWolf/Profiles"],
        win: &["LibreWolf/Profiles"],
    },
    Def { id: "waterfox", name: "Waterfox", linux: &[".waterfox"], mac: &["Waterfox/Profiles"], win: &["Waterfox/Profiles"] },
];

/// Where profile folders live for `def` on `os`.
pub fn profile_roots(def: &Def, os: Os, d: &Dirs) -> Vec<PathBuf> {
    match os {
        Os::Linux => def.linux.iter().map(|r| d.home.join(r)).collect(),
        Os::Mac => def.mac.iter().map(|r| d.home.join("Library/Application Support").join(r)).collect(),
        Os::Windows => def.win.iter().map(|r| d.roaming.join(r)).collect(),
    }
}

/// `cookies.sqlite` of every profile under `root`, most recently used first.
pub fn cookie_dbs(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut dbs: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("cookies.sqlite")).filter(|p| p.is_file()).collect();
    dbs.sort_by_key(|p| std::cmp::Reverse(mtime(p)));
    dbs
}

/// Read the youtube.com cookies out of a Firefox cookie DB as a Cookie header.
pub fn read(db: &Path) -> Result<String, String> {
    with_private_copy(db, cookies_from_db)
}

pub fn cookies_from_db(copy: &Path) -> Result<String, String> {
    let conn = rusqlite::Connection::open_with_flags(copy, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT name, value FROM moz_cookies WHERE host = '.youtube.com' OR host = 'youtube.com' OR host = 'music.youtube.com' OR host = '.music.youtube.com' ORDER BY host DESC")
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?;
    let mut seen = std::collections::HashSet::new();
    let mut parts = Vec::new();
    for (k, v) in rows.flatten() {
        if seen.insert(k.clone()) {
            parts.push(format!("{k}={v}"));
        }
    }
    finish_header(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> Dirs {
        Dirs { home: "/home/u".into(), local: "/home/u/.local/share".into(), roaming: "/home/u/.config".into() }
    }

    #[test]
    fn roots_per_os() {
        let firefox = &DEFS[0];
        assert_eq!(profile_roots(firefox, Os::Linux, &dirs())[0], PathBuf::from("/home/u/.mozilla/firefox"));
        assert_eq!(profile_roots(firefox, Os::Mac, &dirs())[0], PathBuf::from("/home/u/Library/Application Support/Firefox/Profiles"));
        let win =
            Dirs { home: "C:/Users/u".into(), local: "C:/Users/u/AppData/Local".into(), roaming: "C:/Users/u/AppData/Roaming".into() };
        assert_eq!(profile_roots(firefox, Os::Windows, &win)[0], PathBuf::from("C:/Users/u/AppData/Roaming/Mozilla/Firefox/Profiles"));
    }

    #[test]
    fn reads_firefox_db() {
        let dir = std::env::temp_dir().join(format!("ytm-test-ff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("cookies.sqlite");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute("CREATE TABLE moz_cookies (name TEXT, value TEXT, host TEXT)", []).unwrap();
        for (n, v, h) in [
            ("SAPISID", "good", ".youtube.com"),
            ("SID", "s", ".youtube.com"),
            ("SAPISID", "google-only", ".google.com"),
            ("PREF", "p", "music.youtube.com"),
        ] {
            c.execute("INSERT INTO moz_cookies VALUES (?1, ?2, ?3)", [n, v, h]).unwrap();
        }
        drop(c);
        let h = cookies_from_db(&db).unwrap();
        assert!(h.contains("SAPISID=good") && h.contains("SID=s") && h.contains("PREF=p"));
        assert!(!h.contains("google-only"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_logged_out_db() {
        let dir = std::env::temp_dir().join(format!("ytm-test-ff-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("cookies.sqlite");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute("CREATE TABLE moz_cookies (name TEXT, value TEXT, host TEXT)", []).unwrap();
        c.execute("INSERT INTO moz_cookies VALUES ('PREF','p','.youtube.com')", []).unwrap();
        drop(c);
        assert!(cookies_from_db(&db).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
