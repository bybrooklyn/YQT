// SPDX-License-Identifier: GPL-3.0-only
//! In-process OS integration: secret stores, the default browser, desktop detection.
//! No shell-outs anywhere (a unit test enforces that for the whole crate).
use std::fmt;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformError {
    /// No such backend on this OS / session (no D-Bus, no wallet daemon…).
    Unavailable(String),
    /// The backend answered with an error, or the user declined access.
    Failed(String),
    TimedOut,
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlatformError::Unavailable(s) => write!(f, "unavailable: {s}"),
            PlatformError::Failed(s) => write!(f, "failed: {s}"),
            PlatformError::TimedOut => write!(f, "timed out"),
        }
    }
}

impl std::error::Error for PlatformError {}

/// Looks up a password the OS keeps for another application.
///
/// `service` is the entry name (e.g. `"Chrome Safe Storage"`); `account_or_app` is the Keychain
/// account on macOS (`"Chrome"`) and the libsecret `application` attribute on Linux (`"chrome"`).
/// `Ok(None)` means the store works but has no such entry.
pub trait SecretStore {
    fn password(&self, service: &str, account_or_app: &str) -> Result<Option<String>, PlatformError>;
}

/// The stores that make sense on this OS, in the order they should be asked.
pub fn native_secret_stores() -> Vec<Box<dyn SecretStore>> {
    #[cfg(target_os = "linux")]
    {
        linux::stores(Desktop::detect().is_plasma())
    }
    #[cfg(target_os = "macos")]
    {
        vec![Box::new(macos::Keychain)]
    }
    #[cfg(windows)]
    {
        Vec::new()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

/// Windows Data Protection API (decrypt a blob bound to the current user). `None` elsewhere.
pub fn dpapi_unprotect(data: &[u8]) -> Option<Vec<u8>> {
    #[cfg(windows)]
    {
        windows::dpapi_unprotect(data)
    }
    #[cfg(not(windows))]
    {
        let _ = data;
        None
    }
}

/// Where QSettings keeps the app's values on Windows: `HKCU\Software\<organization>\<application>`, both of which
/// were `ytm` and are `yqt`. Categories ("player", "search") are subkeys.
#[cfg_attr(not(windows), allow(dead_code))]
const OLD_SETTINGS_KEY: &str = r"Software\ytm\ytm";
#[cfg_attr(not(windows), allow(dead_code))]
const NEW_SETTINGS_KEY: &str = r"Software\yqt\yqt";

/// The old settings are carried over only when there are some and the new place is still empty: an existing new
/// key is the user's current state and is never written to.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn should_copy_settings(old_exists: bool, new_exists: bool) -> bool {
    old_exists && !new_exists
}

/// Windows: copy the Qt settings (volume, shuffle, repeat, recent searches) from the registry key of the app's old
/// name to the new one, once. `Ok(true)` when they were copied, `Ok(false)` when there was nothing to do (always,
/// on other systems: there they are files, handled by `migrate`).
pub fn migrate_registry_settings() -> Result<bool, String> {
    #[cfg(windows)]
    {
        if !should_copy_settings(windows::key_exists(OLD_SETTINGS_KEY), windows::key_exists(NEW_SETTINGS_KEY)) {
            return Ok(false);
        }
        windows::copy_key(OLD_SETTINGS_KEY, NEW_SETTINGS_KEY)
    }
    #[cfg(not(windows))]
    {
        Ok(false)
    }
}

/// The OS's raw "default web browser" token (desktop file, bundle id or ProgId); `None` if unknown.
pub trait DefaultBrowser {
    fn token(&self) -> Option<String>;
}

pub struct NativeDefaultBrowser;

impl DefaultBrowser for NativeDefaultBrowser {
    fn token(&self) -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            linux::default_browser()
        }
        #[cfg(target_os = "macos")]
        {
            macos::default_browser()
        }
        #[cfg(windows)]
        {
            windows::default_browser()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            None
        }
    }
}

/// `com.apple.launchservices.secure.plist` → bundle id registered for the `https` scheme.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_launchservices_plist(bytes: &[u8]) -> Option<String> {
    let v = plist::Value::from_reader(std::io::Cursor::new(bytes)).ok()?;
    let handlers = v.as_dictionary()?.get("LSHandlers")?.as_array()?;
    handlers.iter().filter_map(|h| h.as_dictionary()).find_map(|d| {
        let scheme = d.get("LSHandlerURLScheme")?.as_string()?;
        if !scheme.eq_ignore_ascii_case("https") {
            return None;
        }
        let id = d.get("LSHandlerRoleAll").or_else(|| d.get("LSHandlerRoleViewer"))?.as_string()?;
        (!id.is_empty() && id != "-").then(|| id.to_owned())
    })
}

/// Which desktop environment we run in.
#[derive(Debug, Clone, Default)]
pub struct Desktop {
    current: Option<String>,
    kde_full_session: Option<String>,
}

impl Desktop {
    pub fn detect() -> Desktop {
        Desktop { current: std::env::var("XDG_CURRENT_DESKTOP").ok(), kde_full_session: std::env::var("KDE_FULL_SESSION").ok() }
    }

    #[cfg(test)]
    pub fn new(xdg_current_desktop: Option<&str>, kde_full_session: Option<&str>) -> Desktop {
        Desktop { current: xdg_current_desktop.map(str::to_owned), kde_full_session: kde_full_session.map(str::to_owned) }
    }

    pub fn is_plasma(&self) -> bool {
        let in_list = self.current.as_deref().is_some_and(|c| c.split(':').any(|p| p.eq_ignore_ascii_case("kde")));
        in_list || self.kde_full_session.as_deref().is_some_and(|v| !v.is_empty())
    }
}

/// Desktop notifications. Others implement this later; the default does nothing.
#[allow(dead_code)] // for later workstreams to implement and use
pub trait Notifier {
    fn notify(&self, title: &str, body: &str);
}

#[allow(dead_code)]
pub struct NullNotifier;

impl Notifier for NullNotifier {
    fn notify(&self, _title: &str, _body: &str) {}
}

/// Ask several stores in order and return every password found (a browser may have its key in more
/// than one place). Errors are only reported when nothing was found and at least one store failed.
pub fn all_passwords(stores: &[Box<dyn SecretStore>], service: &str, account_or_app: &str) -> Result<Vec<String>, PlatformError> {
    let mut found = Vec::new();
    let mut last_err = None;
    for s in stores {
        match s.password(service, account_or_app) {
            Ok(Some(p)) => found.push(p),
            Ok(None) => {}
            Err(e) => last_err = Some(e),
        }
    }
    match (found.is_empty(), last_err) {
        (true, Some(e)) => Err(e),
        _ => Ok(found),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Result<Option<&'static str>, PlatformError>);
    impl SecretStore for Fake {
        fn password(&self, _s: &str, _a: &str) -> Result<Option<String>, PlatformError> {
            self.0.clone().map(|o| o.map(str::to_owned))
        }
    }

    #[test]
    fn registry_settings_are_copied_only_into_an_empty_place() {
        assert!(should_copy_settings(true, false));
        assert!(!should_copy_settings(true, true), "an existing new key must never be overwritten");
        assert!(!should_copy_settings(false, false), "nothing to copy");
        assert!(!should_copy_settings(false, true));
        // QSettings: HKCU\Software\<organization>\<application>
        assert_eq!((OLD_SETTINGS_KEY, NEW_SETTINGS_KEY), (r"Software\ytm\ytm", r"Software\yqt\yqt"));
        #[cfg(not(windows))]
        assert_eq!(migrate_registry_settings(), Ok(false));
    }

    #[test]
    fn parses_launchservices_plist() {
        use plist::{Dictionary, Value};
        let handler = |scheme: &str, id: &str| {
            let mut d = Dictionary::new();
            d.insert("LSHandlerURLScheme".into(), Value::String(scheme.into()));
            d.insert("LSHandlerRoleAll".into(), Value::String(id.into()));
            Value::Dictionary(d)
        };
        let mut root = Dictionary::new();
        root.insert("LSHandlers".into(), Value::Array(vec![handler("mailto", "com.apple.mail"), handler("https", "org.mozilla.firefox")]));
        let mut xml = Vec::new();
        plist::to_writer_xml(&mut xml, &Value::Dictionary(root)).unwrap();
        assert_eq!(parse_launchservices_plist(&xml).as_deref(), Some("org.mozilla.firefox"));
        let mut bin = Vec::new();
        let mut root = Dictionary::new();
        root.insert("LSHandlers".into(), Value::Array(vec![handler("https", "-")]));
        plist::to_writer_binary(&mut bin, &Value::Dictionary(root)).unwrap();
        assert_eq!(parse_launchservices_plist(&bin), None);
        assert_eq!(parse_launchservices_plist(b"garbage"), None);
    }

    #[test]
    fn plasma_detection() {
        assert!(Desktop::new(Some("KDE"), None).is_plasma());
        assert!(!Desktop::new(Some("ubuntu:GNOME"), None).is_plasma());
        assert!(Desktop::new(Some("something:KDE"), None).is_plasma());
        assert!(Desktop::new(None, Some("true")).is_plasma());
        assert!(!Desktop::new(None, Some("")).is_plasma());
        assert!(!Desktop::new(None, None).is_plasma());
    }

    #[test]
    fn all_passwords_collects_and_tolerates_failures() {
        let stores: Vec<Box<dyn SecretStore>> =
            vec![Box::new(Fake(Err(PlatformError::TimedOut))), Box::new(Fake(Ok(Some("pw")))), Box::new(Fake(Ok(None)))];
        assert_eq!(all_passwords(&stores, "s", "a").unwrap(), ["pw"]);
        let failing: Vec<Box<dyn SecretStore>> = vec![Box::new(Fake(Err(PlatformError::TimedOut))), Box::new(Fake(Ok(None)))];
        assert_eq!(all_passwords(&failing, "s", "a"), Err(PlatformError::TimedOut));
        let empty: Vec<Box<dyn SecretStore>> = vec![Box::new(Fake(Ok(None)))];
        assert!(all_passwords(&empty, "s", "a").unwrap().is_empty());
    }

    #[test]
    fn error_display_is_plain() {
        assert_eq!(PlatformError::TimedOut.to_string(), "timed out");
        assert_eq!(PlatformError::Failed("x".into()).to_string(), "failed: x");
    }

    /// The whole app must stay free of shell-outs: everything goes through in-process code.
    #[test]
    fn no_shell_outs_anywhere() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        assert!(!files.is_empty());
        // Built from pieces so this file does not contain the pattern itself.
        let needle = format!("{}::new(", "Command");
        let mut hits = Vec::new();
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap();
            for (i, line) in text.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                if line.contains(&needle) {
                    hits.push(format!("{}:{}", f.display(), i + 1));
                }
            }
        }
        assert!(hits.is_empty(), "shell-outs found: {hits:?}");
    }
}
