// SPDX-License-Identifier: GPL-3.0-only
//! Which browsers are installed (have a cookie database), the default one first.
//! All path logic takes the OS and directories as parameters so it can be tested for every OS on any OS.
use super::{chromium, firefox, Browser, Db, Family};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Mac,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }
}

/// The user directories the browser tables are relative to.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub home: PathBuf,
    /// `%LOCALAPPDATA%` on Windows.
    pub local: PathBuf,
    /// `%APPDATA%` on Windows.
    pub roaming: PathBuf,
}

impl Dirs {
    pub fn current() -> Option<Dirs> {
        Some(Dirs { home: dirs::home_dir()?, local: dirs::data_local_dir()?, roaming: dirs::config_dir()? })
    }
}

pub fn mtime(p: &Path) -> Option<SystemTime> {
    p.metadata().and_then(|m| m.modified()).ok()
}

/// Map an OS-specific "default browser" token (desktop file, bundle id, ProgId) to one of our ids.
/// More specific names first: `org.chromium.Chromium` must not match "chrome".
pub fn default_browser_id(token: &str) -> Option<&'static str> {
    let t = token.to_lowercase();
    const TABLE: &[(&str, &str)] = &[
        ("chromium", "chromium"),
        ("brave", "brave"),
        ("vivaldi", "vivaldi"),
        ("msedge", "edge"),
        ("microsoft-edge", "edge"),
        ("microsoft.edge", "edge"), // flatpak: com.microsoft.Edge.desktop
        ("edgemac", "edge"),
        ("opera", "opera"),
        ("librewolf", "librewolf"),
        ("waterfox", "waterfox"),
        ("zen", "zen"),
        ("firefox", "firefox"),
        ("chrome", "chrome"),
    ];
    TABLE.iter().find(|(needle, _)| t.contains(needle)).map(|(_, id)| *id)
}

/// The OS's raw answer for "default web browser" (empty if unknown), read in-process.
fn default_token(_os: Os) -> String {
    use crate::platform::{DefaultBrowser, NativeDefaultBrowser};
    NativeDefaultBrowser.token().unwrap_or_default()
}

const WINDOWS_CHROMIUM_NOTE: &str =
    "Close the browser first. Chrome, Edge and Brave 127+ protect cookies from other apps – if import fails, paste your cookies or use Firefox.";

/// Every supported browser that has a cookie database, default first, then by recent use.
pub fn detect() -> Vec<Browser> {
    let os = Os::current();
    match Dirs::current() {
        Some(d) => detect_with(os, &d, &default_token(os)),
        None => Vec::new(),
    }
}

pub fn detect_with(os: Os, d: &Dirs, default_token: &str) -> Vec<Browser> {
    let default_id = default_browser_id(default_token);
    let mut found: Vec<Browser> = Vec::new();

    for def in chromium::DEFS {
        let dbs: Vec<Db> = chromium::user_data_roots(def, os, d)
            .into_iter()
            .flat_map(|root| chromium::cookie_dbs(&root).into_iter().map(move |path| Db { path, root: root.clone() }))
            .collect();
        if !dbs.is_empty() {
            let note = (os == Os::Windows).then(|| WINDOWS_CHROMIUM_NOTE.to_owned());
            found.push(Browser { id: def.id.into(), name: def.name.into(), is_default: false, note, family: Family::Chromium(def), dbs });
        }
    }
    for def in firefox::DEFS {
        let dbs: Vec<Db> = firefox::profile_roots(def, os, d)
            .into_iter()
            .flat_map(|root| firefox::cookie_dbs(&root).into_iter().map(move |path| Db { path, root: root.clone() }))
            .collect();
        if !dbs.is_empty() {
            found.push(Browser { id: def.id.into(), name: def.name.into(), is_default: false, note: None, family: Family::Firefox, dbs });
        }
    }
    for b in &mut found {
        b.is_default = default_id == Some(b.id.as_str());
    }
    found.sort_by_key(|b| (!b.is_default, std::cmp::Reverse(b.dbs.first().and_then(|d| mtime(&d.path)))));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ids_from_each_os_flavour() {
        assert_eq!(default_browser_id("chromium.desktop"), Some("chromium"));
        assert_eq!(default_browser_id("google-chrome.desktop"), Some("chrome"));
        assert_eq!(default_browser_id("org.chromium.Chromium"), Some("chromium")); // not "chrome"
        assert_eq!(default_browser_id("com.google.chrome"), Some("chrome"));
        assert_eq!(default_browser_id("ChromeHTML"), Some("chrome"));
        assert_eq!(default_browser_id("MSEdgeHTM"), Some("edge"));
        assert_eq!(default_browser_id("com.microsoft.Edge.desktop"), Some("edge"));
        assert_eq!(default_browser_id("com.microsoft.edgemac"), Some("edge"));
        assert_eq!(default_browser_id("BraveHTML"), Some("brave"));
        assert_eq!(default_browser_id("FirefoxURL-308046B0AF4A39CB"), Some("firefox"));
        assert_eq!(default_browser_id("app.zen_browser.zen.desktop"), Some("zen"));
        assert_eq!(default_browser_id("org.mozilla.firefox"), Some("firefox"));
        assert_eq!(default_browser_id("something-else"), None);
        assert_eq!(default_browser_id(""), None);
    }

    /// Build a fake Linux home with two browsers and check detection + ordering.
    #[test]
    fn detect_orders_default_first() {
        let home = std::env::temp_dir().join(format!("ytm-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let touch = |rel: &str| {
            let p = home.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"").unwrap();
        };
        touch(".config/chromium/Default/Cookies");
        touch(".config/BraveSoftware/Brave-Browser/Default/Network/Cookies");
        touch(".mozilla/firefox/abc.default/cookies.sqlite");
        let d = Dirs { home: home.clone(), local: home.join(".local/share"), roaming: home.join(".config") };

        let names = |token: &str| detect_with(Os::Linux, &d, token).iter().map(|b| b.id.clone()).collect::<Vec<_>>();
        let by_brave = names("brave-browser.desktop");
        assert_eq!(by_brave[0], "brave");
        assert_eq!(by_brave.len(), 3);
        assert_eq!(names("firefox.desktop")[0], "firefox");
        assert!(detect_with(Os::Linux, &d, "").iter().all(|b| !b.is_default));
        // Linux has no extra caveats; Windows warns about the exclusive lock / app-bound encryption.
        assert!(detect_with(Os::Linux, &d, "").iter().all(|b| b.note.is_none()));
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn nothing_installed_means_empty_list() {
        let d = Dirs { home: "/nonexistent/x".into(), local: "/nonexistent/y".into(), roaming: "/nonexistent/z".into() };
        assert!(detect_with(Os::Linux, &d, "chromium.desktop").is_empty());
    }
}

/// `cargo test -- --ignored --nocapture detected_browsers`: which browsers does this machine expose?
#[cfg(test)]
#[test]
#[ignore]
fn detected_browsers() {
    for b in detect() {
        println!("{:<10} {:<16} default={:<5} profiles={} note={:?}", b.id, b.name, b.is_default, b.dbs.len(), b.note);
    }
}
