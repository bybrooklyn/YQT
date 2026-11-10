// SPDX-License-Identifier: GPL-3.0-only
//! Firefox-family browsers: unencrypted `moz_cookies` in `<profile>/cookies.sqlite`.
use super::{finish_header, has_column, unix_now, with_private_copy};
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
        Os::Linux => {
            let mut roots: Vec<PathBuf> = def.linux.iter().map(|r| d.home.join(r)).collect();
            // Newer Firefox follows the XDG layout on a fresh install: `$XDG_CONFIG_HOME/mozilla/firefox`.
            if def.id == "firefox" {
                roots.push(d.roaming.join("mozilla/firefox"));
            }
            roots
        }
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
    // Each container (and each partitioned or private jar) is its own cookie jar, told apart by `originAttributes`
    // ("" = the normal one). Jars are never mixed: two accounts in two containers would give a header that is neither.
    let jar = if has_column(&conn, "moz_cookies", "originAttributes") { "originAttributes" } else { "''" };
    // `expiry` is in seconds (milliseconds in newer versions, told apart by size); 0 or missing = none recorded.
    let live = if has_column(&conn, "moz_cookies", "expiry") {
        "(expiry IS NULL OR expiry <= 0 OR (CASE WHEN expiry > 100000000000 THEN expiry / 1000 ELSE expiry END) > ?1)"
    } else {
        "?1 = ?1"
    };
    let sql = format!(
        "SELECT name, value, {jar} FROM moz_cookies WHERE (host = '.youtube.com' OR host = 'youtube.com' OR host = 'music.youtube.com' OR host = '.music.youtube.com') AND {live} ORDER BY {jar}, host DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([unix_now()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .map_err(|e| e.to_string())?;
    // One header per jar, the normal jar first (it sorts first); the first one that is signed in wins.
    let mut jars: Vec<(String, Vec<String>, std::collections::HashSet<String>)> = Vec::new();
    for (k, v, attrs) in rows.flatten() {
        if jars.last().is_none_or(|(a, _, _)| *a != attrs) {
            jars.push((attrs, Vec::new(), std::collections::HashSet::new()));
        }
        if let Some((_, parts, seen)) = jars.last_mut() {
            if seen.insert(k.clone()) {
                parts.push(format!("{k}={v}"));
            }
        }
    }
    let mut result = finish_header(Vec::new());
    for (_, parts, _) in jars {
        result = finish_header(parts);
        if result.is_ok() {
            break;
        }
    }
    result
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
        // the XDG location of newer versions, for Firefox only
        assert!(profile_roots(firefox, Os::Linux, &dirs()).contains(&PathBuf::from("/home/u/.config/mozilla/firefox")));
        assert!(profile_roots(&DEFS[1], Os::Linux, &dirs()).iter().all(|p| !p.ends_with("mozilla/firefox")));
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

    /// A DB with the columns of a real one: (name, value, host, originAttributes, expiry).
    fn full_db(name: &str, rows: &[(&str, &str, &str, &str, i64)]) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ytm-test-ff-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("cookies.sqlite");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute("CREATE TABLE moz_cookies (name TEXT, value TEXT, host TEXT, originAttributes TEXT, expiry INTEGER)", []).unwrap();
        for (n, v, h, a, e) in rows {
            c.execute("INSERT INTO moz_cookies VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params![n, v, h, a, e]).unwrap();
        }
        (dir, db)
    }

    #[test]
    fn containers_are_never_mixed_and_the_normal_jar_wins() {
        let later = unix_now() + 86_400;
        // the container's rows come first in the table, as they would after being refreshed
        let (dir, db) = full_db(
            "jars",
            &[
                ("SAPISID", "container-sapisid", ".youtube.com", "^userContextId=2", later),
                ("SIDTS", "container-ts", ".youtube.com", "^userContextId=2", later),
                ("SAPISID", "main-sapisid", ".youtube.com", "", later),
                ("SID", "main-sid", ".youtube.com", "", later),
            ],
        );
        let h = cookies_from_db(&db).unwrap();
        assert!(h.contains("SAPISID=main-sapisid") && h.contains("SID=main-sid") && !h.contains("container"), "{h}");
        std::fs::remove_dir_all(dir).ok();

        // signed in only inside a container: that jar is used, whole
        let (dir, db) = full_db(
            "jar-only",
            &[
                ("PREF", "main-pref", ".youtube.com", "", later),
                ("SAPISID", "container-sapisid", ".youtube.com", "^userContextId=2", later),
                ("SID", "container-sid", ".youtube.com", "^userContextId=2", later),
            ],
        );
        let h = cookies_from_db(&db).unwrap();
        assert!(h.contains("SAPISID=container-sapisid") && h.contains("SID=container-sid") && !h.contains("main"), "{h}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn expired_cookies_are_left_out() {
        let now = unix_now();
        let (dir, db) = full_db(
            "expiry",
            &[
                ("SAPISID", "live", ".youtube.com", "", now + 3600),
                ("SID", "stale", ".youtube.com", "", now - 3600),
                ("HSID", "live-ms", ".youtube.com", "", (now + 3600) * 1000), // newer versions store milliseconds
                ("SSID", "stale-ms", ".youtube.com", "", (now - 3600) * 1000),
                ("PREF", "no-expiry", ".youtube.com", "", 0),
            ],
        );
        let h = cookies_from_db(&db).unwrap();
        assert!(h.contains("SAPISID=live") && h.contains("HSID=live-ms") && h.contains("PREF=no-expiry"), "{h}");
        assert!(!h.contains("stale"), "{h}");
        std::fs::remove_dir_all(dir).ok();

        // an expired login cookie is not a sign-in
        let (dir, db) = full_db("expired-login", &[("SAPISID", "old", ".youtube.com", "", now - 10)]);
        assert!(cookies_from_db(&db).is_err());
        std::fs::remove_dir_all(dir).ok();
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
