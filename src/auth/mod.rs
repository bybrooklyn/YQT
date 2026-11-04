// SPDX-License-Identifier: GPL-3.0-only
//! Cookie-based sign-in.
//!
//! YouTube Music's library/liked/history endpoints only accept browser cookies (OAuth tokens work
//! for the TV client only), so we sign in by taking the `youtube.com` cookies of a browser session:
//! imported straight from an installed browser, or pasted by the user (Cookie header or cookies.txt).
//!
//! * `firefox`  – plain SQLite, same on every OS.
//! * `chromium` – encrypted values; the key comes from KWallet/libsecret (Linux), the Keychain
//!   (macOS) or DPAPI (Windows). Everything except those three tiny key lookups is plain, tested Rust.
//! * `detect`   – which browsers exist on this machine, default first.
mod chromium;
mod detect;
mod firefox;

use crate::state::data_dir;
use std::path::{Path, PathBuf};

pub use detect::detect;

pub const AUTH_MSG: &str = "Your session expired. Sign in again from Settings.";

#[derive(Debug, PartialEq)]
pub enum Input {
    /// A `name=value; name2=value2` header.
    Header(String),
    /// A Netscape cookies.txt file.
    Netscape(String),
}

/// Classify what the user pasted.
pub fn parse_input(text: &str) -> Result<Input, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("Nothing to import".into());
    }
    if t.contains('\0') {
        return Err("That text contains characters that can't be in cookies".into());
    }
    let looks_netscape = t.contains('\t') && t.lines().any(|l| l.split('\t').count() >= 7);
    if looks_netscape || t.starts_with("# Netscape") {
        return Ok(Input::Netscape(t.to_owned()));
    }
    let body = t.strip_prefix("Cookie:").or_else(|| t.strip_prefix("cookie:")).unwrap_or(t);
    // A header copied from dev tools may be wrapped over several lines, but only after a `;`.
    // Any other line break would let pasted text smuggle in a second header.
    let lines: Vec<&str> = body.lines().map(str::trim).collect();
    if lines.iter().take(lines.len().saturating_sub(1)).any(|l| !l.ends_with(';')) {
        return Err("That doesn't look like a single Cookie header (unexpected line break)".into());
    }
    let header = sanitize_header_value(&lines.join(" "))?;
    if !header.contains('=') {
        return Err("That doesn't look like cookies (expected name=value; …)".into());
    }
    Ok(Input::Header(header))
}

/// Accept a value only if it is safe to send as an HTTP header value: no CR, LF, NUL or other
/// control characters (tab excepted). The value itself is never echoed in the error.
pub fn sanitize_header_value(v: &str) -> Result<String, String> {
    if v.chars().any(|c| c.is_control() && c != '\t') {
        return Err("That text contains control characters and can't be used as a cookie header".into());
    }
    Ok(v.trim().to_owned())
}

pub fn has_login_cookie(header: &str) -> bool {
    header
        .split(';')
        .filter_map(|p| p.trim().split_once('='))
        .any(|(k, _)| matches!(k, "SAPISID" | "__Secure-3PAPISID" | "__Secure-1PAPISID"))
}

/// Join `name=value` parts into a header, or explain that the browser isn't signed in.
fn finish_header(parts: Vec<String>) -> Result<String, String> {
    let header = parts.join("; ");
    if has_login_cookie(&header) {
        Ok(header)
    } else {
        Err("not signed in to YouTube in that browser".into())
    }
}

/// The stored Cookie header (read from RustyPipe's cache) – needed for the write calls in ytapi.
pub fn stored_cookie() -> Option<String> {
    let text = std::fs::read_to_string(data_dir().join("rustypipe_cache.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v["auth_cookie"]["cookie"].as_str().map(str::to_owned)
}

/// A browser we know how to import a YouTube session from.
#[derive(Clone, Debug)]
pub struct Browser {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    /// Shown under the button when something limits importing (e.g. "close the browser first").
    pub note: Option<String>,
    family: Family,
    /// Cookie databases, most recently used first.
    dbs: Vec<Db>,
}

#[derive(Clone, Debug)]
enum Family {
    Firefox,
    Chromium(&'static chromium::Def),
}

#[derive(Clone, Debug)]
struct Db {
    path: PathBuf,
    /// The browser's user-data root (Chromium keeps its `Local State` key file there).
    root: PathBuf,
}

/// Copy a cookie DB (plus journal/WAL) somewhere private: browsers keep the original locked.
fn with_private_copy<T>(db: &Path, f: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    // One private folder per import (two at once must not delete each other's copy), and any left behind by an
    // import that was killed (power loss, SIGKILL) is swept first: it holds a plaintext copy of a cookie database.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = data_dir().join("cookie-import");
    sweep_stale_imports(&base);
    let unique = format!("{}-{}", std::process::id(), COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let guard = PrivateDir::create(&base.join(unique))?;
    let tmp = guard.path();
    let copy = tmp.join("cookies.sqlite");
    std::fs::copy(db, &copy).map_err(|e| {
        let hint = if cfg!(windows) { " If the browser is running, close it and try again." } else { "" };
        format!("can't read {}: {e}.{hint}", db.display())
    })?;
    for ext in ["-wal", "-shm", "-journal"] {
        let src = PathBuf::from(format!("{}{ext}", db.display()));
        if src.exists() {
            let _ = std::fs::copy(&src, tmp.join(format!("cookies.sqlite{ext}")));
        }
    }
    f(&copy)
    // `guard` removes the directory here, also when `f` panics (unwinding drops it).
}

/// A fresh, owner-only (0700 on unix) directory that is deleted when dropped.
/// Remove import folders older than this: they can only be left over from an import that never got to clean up.
const STALE_IMPORT_AGE: std::time::Duration = std::time::Duration::from_secs(600);

fn sweep_stale_imports(base: &Path) {
    let Ok(rd) = std::fs::read_dir(base) else { return };
    for e in rd.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a > STALE_IMPORT_AGE);
        // (symlinks are not followed: `remove_dir_all` removes a link itself, and only directories are considered)
        if old && e.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create(path: &Path) -> Result<PrivateDir, String> {
        let _ = std::fs::remove_dir_all(path);
        if let Some(parent) = path.parent() {
            // the parent holds nothing else but may be new: make it owner-only as well
            crate::state::ensure_private_dir(parent).map_err(|e| e.to_string())?;
        }
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(path).map_err(|e| e.to_string())?;
        Ok(PrivateDir(path.to_path_buf()))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Import from one browser (by `Browser::id`) – tries each of its profiles, newest first.
pub fn import_from_browser(id: &str) -> Result<(String, String), String> {
    let b = detect().into_iter().find(|b| b.id == id).ok_or_else(|| "That browser wasn't found".to_owned())?;
    let mut last = String::new();
    for db in &b.dbs {
        let r = match &b.family {
            Family::Firefox => firefox::read(&db.path),
            Family::Chromium(def) => chromium::read(&db.path, &mut *chromium::key_source(def, &db.root)),
        };
        match r {
            Ok(h) => return Ok((b.name.clone(), h)),
            Err(e) => last = e,
        }
    }
    Err(format!("{}: {last}. Sign in to music.youtube.com there first, or paste your cookies.", b.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_header_forms() {
        assert_eq!(parse_input("Cookie: a=1; b=2\n").unwrap(), Input::Header("a=1; b=2".into()));
        assert_eq!(parse_input("a=1;\nb=2").unwrap(), Input::Header("a=1; b=2".into()));
        assert!(parse_input("   ").is_err());
        assert!(parse_input("hello").is_err());
    }

    #[test]
    fn rejects_header_injection() {
        for evil in [
            "a=1\r\nX-Evil: 1",
            "a=1\nX-Evil: 1",
            "Cookie: a=1\r\nCookie: b=2",
            "a=1\0; b=2",
            "a=1;\r\nb=2\u{7f}",
            "a=1; b=\u{1b}[2J",
            "a=1\rX-Evil: 1",
        ] {
            let r = parse_input(evil);
            assert!(r.is_err(), "accepted {evil:?}");
            let msg = r.unwrap_err();
            assert!(!msg.contains("a=1") && !msg.contains("Evil"), "error echoes the input: {msg}");
        }
        assert!(sanitize_header_value("a=1\r\nX: y").is_err());
        assert_eq!(sanitize_header_value("a=1; b=2").unwrap(), "a=1; b=2");
        // wrapped after `;` is a legitimate paste
        assert_eq!(parse_input("a=1;\r\nb=2").unwrap(), Input::Header("a=1; b=2".into()));
    }

    #[test]
    fn private_dir_is_owner_only_and_removed_on_drop_and_panic() {
        let base = std::env::temp_dir().join(format!("ytm-privdir-{}", std::process::id()));
        let dir = base.join("cookie-import");
        {
            let g = PrivateDir::create(&dir).unwrap();
            assert!(g.path().is_dir());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
            }
        }
        assert!(!dir.exists());
        let d2 = dir.clone();
        let r = std::panic::catch_unwind(move || {
            let _g = PrivateDir::create(&d2).unwrap();
            panic!("boom");
        });
        assert!(r.is_err());
        assert!(!dir.exists());
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn detects_netscape() {
        let t = "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\tabc\n";
        assert!(matches!(parse_input(t).unwrap(), Input::Netscape(_)));
    }

    #[test]
    fn login_cookie_detection() {
        assert!(has_login_cookie("a=1; SAPISID=xyz"));
        assert!(has_login_cookie("__Secure-3PAPISID=x"));
        assert!(!has_login_cookie("VISITOR_INFO1_LIVE=x; PREF=y"));
    }

    #[test]
    fn finish_header_requires_a_login_cookie() {
        assert!(finish_header(vec!["PREF=p".into()]).is_err());
        assert_eq!(finish_header(vec!["SAPISID=a".into(), "SID=b".into()]).unwrap(), "SAPISID=a; SID=b");
    }
}
