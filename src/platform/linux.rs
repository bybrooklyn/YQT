// SPDX-License-Identifier: GPL-3.0-only
//! Linux: KWallet and the freedesktop Secret Service over D-Bus (zbus), and the default browser from
//! `mimeapps.list`. The D-Bus conversation is a thin shell around small pure functions.
use super::{PlatformError, SecretStore};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

/// Ask at most this long: the wallet may be waiting for the user to click "allow" / type a passphrase.
const WALLET_TIMEOUT: Duration = Duration::from_secs(60);
/// KWallet's idea of who is asking.
const APP_ID: &str = "ytm";

pub fn stores(plasma: bool) -> Vec<Box<dyn SecretStore>> {
    if plasma {
        vec![Box::new(KWallet), Box::new(SecretService)]
    } else {
        vec![Box::new(SecretService), Box::new(KWallet)]
    }
}

// ---- pure protocol helpers ---------------------------------------------------------------------

/// `"Chrome Safe Storage"` → wallet folder `"Chrome Keys"` and entry key `"Chrome Safe Storage"`.
pub fn kwallet_names(service: &str) -> (String, String) {
    let product = service.strip_suffix(" Safe Storage").unwrap_or(service);
    (format!("{product} Keys"), service.to_owned())
}

/// kwalletd returns a negative handle when the wallet couldn't be opened (or access was denied).
pub fn kwallet_handle(h: i32) -> Result<i32, PlatformError> {
    if h < 0 {
        Err(PlatformError::Failed("the wallet could not be opened (access denied?)".into()))
    } else {
        Ok(h)
    }
}

/// Attributes libsecret-based apps (Chromium) store their Safe Storage item under.
pub fn secret_attrs(app: &str) -> HashMap<&str, &str> {
    HashMap::from([("application", app)])
}

/// The secret bytes of a Secret Service item → the password text.
pub fn secret_text(value: Vec<u8>) -> Option<String> {
    String::from_utf8(value).ok().filter(|s| !s.is_empty())
}

/// An empty object path ("/") means "no prompt needed".
pub fn needs_prompt(path: &str) -> bool {
    !path.is_empty() && path != "/"
}

// ---- running async D-Bus code from sync code ---------------------------------------------------

/// Run a future on a private single-thread runtime on its own thread: safe to call from anywhere,
/// including inside the app's tokio runtime.
fn run_blocking<T: Send + 'static>(fut: impl Future<Output = Result<T, PlatformError>> + Send + 'static) -> Result<T, PlatformError> {
    std::thread::Builder::new()
        .name("ytm-secrets".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| PlatformError::Unavailable(e.to_string()))?;
            rt.block_on(async {
                tokio::time::timeout(WALLET_TIMEOUT + Duration::from_secs(5), fut).await.unwrap_or(Err(PlatformError::TimedOut))
            })
        })
        .map_err(|e| PlatformError::Unavailable(e.to_string()))?
        .join()
        .unwrap_or_else(|_| Err(PlatformError::Failed("secret lookup thread panicked".into())))
}

fn zerr(e: zbus::Error) -> PlatformError {
    // Never include message bodies: only the error kind/name.
    PlatformError::Failed(e.to_string())
}

async fn session_bus() -> Result<Connection, PlatformError> {
    Connection::session().await.map_err(|e| PlatformError::Unavailable(format!("no D-Bus session bus: {e}")))
}

// ---- KWallet -----------------------------------------------------------------------------------

pub struct KWallet;

impl SecretStore for KWallet {
    fn password(&self, service: &str, _account_or_app: &str) -> Result<Option<String>, PlatformError> {
        let (folder, key) = kwallet_names(service);
        run_blocking(async move {
            let conn = session_bus().await?;
            let mut last = PlatformError::Unavailable("kwalletd is not running".into());
            for (dest, path) in [("org.kde.kwalletd6", "/modules/kwalletd6"), ("org.kde.kwalletd5", "/modules/kwalletd5")] {
                match kwallet_read(&conn, dest, path, &folder, &key).await {
                    Ok(v) => return Ok(v),
                    Err(e) => last = e,
                }
            }
            Err(last)
        })
    }
}

async fn kwallet_read(conn: &Connection, dest: &str, path: &str, folder: &str, key: &str) -> Result<Option<String>, PlatformError> {
    let p = Proxy::new(conn, dest.to_owned(), path.to_owned(), "org.kde.KWallet")
        .await
        .map_err(|e| PlatformError::Unavailable(e.to_string()))?;
    let wallet: String = p.call("localWallet", &()).await.map_err(|e| PlatformError::Unavailable(e.to_string()))?;
    let handle: i32 = p.call("open", &(wallet.as_str(), 0i64, APP_ID)).await.map_err(zerr)?;
    let handle = kwallet_handle(handle)?;
    let result = async {
        let has: bool = p.call("hasEntry", &(handle, folder, key, APP_ID)).await.map_err(zerr)?;
        if !has {
            return Ok(None);
        }
        let pw: String = p.call("readPassword", &(handle, folder, key, APP_ID)).await.map_err(zerr)?;
        Ok(Some(pw).filter(|s| !s.is_empty()))
    }
    .await;
    let _: Result<i32, _> = p.call("close", &(handle, false, APP_ID)).await;
    result
}

// ---- Secret Service ----------------------------------------------------------------------------

pub struct SecretService;

const SS_DEST: &str = "org.freedesktop.secrets";
const SS_PATH: &str = "/org/freedesktop/secrets";

impl SecretStore for SecretService {
    fn password(&self, _service: &str, account_or_app: &str) -> Result<Option<String>, PlatformError> {
        let app = account_or_app.to_owned();
        run_blocking(async move {
            let conn = session_bus().await?;
            secret_service_read(&conn, &app).await
        })
    }
}

async fn secret_service_read(conn: &Connection, app: &str) -> Result<Option<String>, PlatformError> {
    let svc = Proxy::new(conn, SS_DEST, SS_PATH, "org.freedesktop.Secret.Service")
        .await
        .map_err(|e| PlatformError::Unavailable(e.to_string()))?;
    // "plain": the secret travels unencrypted over the (local, per-user) session bus.
    let (_out, session): (OwnedValue, OwnedObjectPath) =
        svc.call("OpenSession", &("plain", Value::from(""))).await.map_err(|e| PlatformError::Unavailable(e.to_string()))?;
    async fn search(svc: &Proxy<'_>, app: &str) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), PlatformError> {
        svc.call("SearchItems", &secret_attrs(app)).await.map_err(zerr)
    }
    let (mut unlocked, locked) = search(&svc, app).await?;
    if unlocked.is_empty() && !locked.is_empty() {
        let paths: Vec<ObjectPath<'_>> = locked.iter().map(|p| p.as_ref()).collect();
        let (_done, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = svc.call("Unlock", &(paths,)).await.map_err(zerr)?;
        if needs_prompt(prompt.as_str()) {
            let pp = Proxy::new(conn, SS_DEST, prompt.as_ref(), "org.freedesktop.Secret.Prompt").await.map_err(zerr)?;
            let _: () = pp.call("Prompt", &("",)).await.map_err(zerr)?;
        }
        // Wait for the user to unlock (the prompt is asynchronous).
        let deadline = tokio::time::Instant::now() + WALLET_TIMEOUT;
        loop {
            unlocked = search(&svc, app).await?.0;
            if !unlocked.is_empty() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(PlatformError::TimedOut);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    let Some(item) = unlocked.first() else { return Ok(None) };
    let items = vec![item.as_ref()];
    let secrets: HashMap<OwnedObjectPath, (OwnedObjectPath, Vec<u8>, Vec<u8>, String)> =
        svc.call("GetSecrets", &(items, session.as_ref())).await.map_err(zerr)?;
    Ok(secrets.into_values().next().and_then(|(_s, _params, value, _ct)| secret_text(value)))
}

// ---- default browser ---------------------------------------------------------------------------

/// First `.desktop` file named for one of the web-ish MIME types, across `mimeapps.list` files given
/// highest priority first. Only the `[Default Applications]` section counts.
pub fn parse_mimeapps(files: &[String]) -> Option<String> {
    const KEYS: [&str; 3] = ["x-scheme-handler/https", "x-scheme-handler/http", "text/html"];
    for key in KEYS {
        for text in files {
            let mut in_defaults = false;
            for line in text.lines().map(str::trim) {
                if line.starts_with('[') {
                    in_defaults = line == "[Default Applications]";
                } else if in_defaults {
                    if let Some((k, v)) = line.split_once('=') {
                        if k.trim() == key {
                            if let Some(first) = v.split(';').map(str::trim).find(|s| !s.is_empty()) {
                                return Some(first.to_owned());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// `mimeapps.list` locations per the XDG MIME apps spec, highest priority first.
pub fn mimeapps_paths(home: Option<PathBuf>, env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let split = |v: String| v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect::<Vec<_>>();
    let config_home =
        env("XDG_CONFIG_HOME").filter(|s| !s.is_empty()).map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".config")));
    let data_home =
        env("XDG_DATA_HOME").filter(|s| !s.is_empty()).map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    let config_dirs = env("XDG_CONFIG_DIRS").map(split).filter(|v| !v.is_empty()).unwrap_or_else(|| vec![PathBuf::from("/etc/xdg")]);
    let data_dirs = env("XDG_DATA_DIRS")
        .map(split)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")]);

    let mut out = Vec::new();
    out.extend(config_home.map(|d| d.join("mimeapps.list")));
    out.extend(config_dirs.into_iter().map(|d| d.join("mimeapps.list")));
    out.extend(data_home.map(|d| d.join("applications/mimeapps.list")));
    out.extend(data_dirs.into_iter().map(|d| d.join("applications/mimeapps.list")));
    out
}

pub fn default_browser() -> Option<String> {
    let paths = mimeapps_paths(dirs::home_dir(), &|k| std::env::var(k).ok());
    let texts: Vec<String> = paths.iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect();
    parse_mimeapps(&texts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kwallet_folder_and_key_follow_chromium_naming() {
        assert_eq!(kwallet_names("Chrome Safe Storage"), ("Chrome Keys".into(), "Chrome Safe Storage".into()));
        assert_eq!(kwallet_names("Microsoft Edge Safe Storage").0, "Microsoft Edge Keys");
    }

    #[test]
    fn kwallet_negative_handle_is_an_error() {
        assert_eq!(kwallet_handle(5), Ok(5));
        assert!(kwallet_handle(-1).is_err());
    }

    #[test]
    fn secret_service_parsing() {
        assert_eq!(secret_attrs("chrome").get("application"), Some(&"chrome"));
        assert_eq!(secret_text(b"hunter2".to_vec()).as_deref(), Some("hunter2"));
        assert_eq!(secret_text(Vec::new()), None);
        assert_eq!(secret_text(vec![0xff, 0xfe]), None);
        assert!(!needs_prompt("/"));
        assert!(!needs_prompt(""));
        assert!(needs_prompt("/org/freedesktop/secrets/prompt/u1"));
    }

    #[test]
    fn store_order_depends_on_desktop() {
        assert_eq!(stores(true).len(), 2);
        assert_eq!(stores(false).len(), 2);
    }

    #[test]
    fn mimeapps_prefers_https_then_http_then_html() {
        let user = "[Added Associations]\nx-scheme-handler/https=wrong.desktop;\n[Default Applications]\ntext/html=html.desktop\nx-scheme-handler/http=http.desktop;other.desktop;\n";
        let system = "[Default Applications]\nx-scheme-handler/https=sys.desktop\n";
        assert_eq!(parse_mimeapps(&[user.into(), system.into()]).as_deref(), Some("sys.desktop"));
        assert_eq!(parse_mimeapps(&[user.into()]).as_deref(), Some("http.desktop"));
        assert_eq!(parse_mimeapps(&["[Default Applications]\ntext/html = firefox.desktop;\n".into()]).as_deref(), Some("firefox.desktop"));
        assert_eq!(parse_mimeapps(&["[Added Associations]\ntext/html=x.desktop\n".into()]), None);
        assert_eq!(parse_mimeapps(&[]), None);
    }

    #[test]
    fn mimeapps_paths_follow_xdg() {
        let env = |k: &str| match k {
            "XDG_DATA_DIRS" => Some("/a:/b".to_owned()),
            _ => None,
        };
        let p = mimeapps_paths(Some("/home/u".into()), &env);
        assert_eq!(p[0], PathBuf::from("/home/u/.config/mimeapps.list"));
        assert!(p.contains(&PathBuf::from("/etc/xdg/mimeapps.list")));
        assert!(p.contains(&PathBuf::from("/home/u/.local/share/applications/mimeapps.list")));
        assert_eq!(&p[p.len() - 2..], [PathBuf::from("/a/applications/mimeapps.list"), PathBuf::from("/b/applications/mimeapps.list")]);
    }
}
