// SPDX-License-Identifier: GPL-3.0-only
//! Chromium-family browsers (Chrome, Chromium, Brave, Vivaldi, Edge, Opera).
//!
//! Cookie values live in an SQLite `cookies` table, encrypted by the browser:
//!
//! | OS      | blob prefix | cipher          | key from                                       |
//! |---------|-------------|-----------------|------------------------------------------------|
//! | Linux   | `v10`       | AES-128-CBC     | fixed password "peanuts" (PBKDF2, 1 iteration) |
//! | Linux   | `v11`       | AES-128-CBC     | KWallet / libsecret "Safe Storage" password    |
//! | macOS   | `v10`       | AES-128-CBC     | Keychain "Safe Storage" password (1003 iters)  |
//! | Windows | `v10`       | AES-256-GCM     | `Local State` key, unwrapped with DPAPI        |
//! | Windows | `v20`       | (app-bound)     | **not readable by other apps – not attempted** |
//!
//! Only the three key lookups touch the OS; everything else is plain functions covered by tests.
use super::detect::{mtime, Dirs, Os};
use super::{finish_header, with_private_copy};
use crate::platform::{all_passwords, SecretStore};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
pub enum Base {
    Local,
    Roaming,
}

#[derive(Debug)]
pub struct Def {
    pub id: &'static str,
    pub name: &'static str,
    /// Name in "<product> Safe Storage" (keyring / Keychain entry).
    pub product: &'static str,
    /// libsecret `application` attribute.
    pub app: &'static str,
    /// Relative to $HOME.
    pub linux: &'static [&'static str],
    /// Relative to `~/Library/Application Support`.
    pub mac: &'static [&'static str],
    pub win: &'static [(Base, &'static str)],
}

pub const DEFS: &[Def] = &[
    Def {
        id: "chrome",
        name: "Google Chrome",
        product: "Chrome",
        app: "chrome",
        linux: &[".config/google-chrome", ".var/app/com.google.Chrome/config/google-chrome"],
        mac: &["Google/Chrome"],
        win: &[(Base::Local, "Google/Chrome/User Data")],
    },
    Def {
        id: "chromium",
        name: "Chromium",
        product: "Chromium",
        app: "chromium",
        linux: &[".config/chromium", ".var/app/org.chromium.Chromium/config/chromium", "snap/chromium/common/chromium"],
        mac: &["Chromium"],
        win: &[(Base::Local, "Chromium/User Data")],
    },
    Def {
        id: "brave",
        name: "Brave",
        product: "Brave",
        app: "brave",
        linux: &[".config/BraveSoftware/Brave-Browser", ".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser"],
        mac: &["BraveSoftware/Brave-Browser"],
        win: &[(Base::Local, "BraveSoftware/Brave-Browser/User Data")],
    },
    Def {
        id: "vivaldi",
        name: "Vivaldi",
        product: "Vivaldi",
        app: "vivaldi",
        linux: &[".config/vivaldi", ".var/app/com.vivaldi.Vivaldi/config/vivaldi"],
        mac: &["Vivaldi"],
        win: &[(Base::Local, "Vivaldi/User Data")],
    },
    Def {
        id: "edge",
        name: "Microsoft Edge",
        product: "Microsoft Edge",
        app: "microsoft-edge",
        linux: &[".config/microsoft-edge", ".var/app/com.microsoft.Edge/config/microsoft-edge"],
        mac: &["Microsoft Edge"],
        win: &[(Base::Local, "Microsoft/Edge/User Data")],
    },
    Def {
        id: "opera",
        name: "Opera",
        product: "Opera",
        app: "opera",
        linux: &[".config/opera", ".var/app/com.opera.Opera/config/opera"],
        mac: &["com.operasoftware.Opera"],
        win: &[(Base::Roaming, "Opera Software/Opera Stable")],
    },
];

/// The browser's user-data folders on `os`.
pub fn user_data_roots(def: &Def, os: Os, d: &Dirs) -> Vec<PathBuf> {
    match os {
        Os::Linux => def.linux.iter().map(|r| d.home.join(r)).collect(),
        Os::Mac => def.mac.iter().map(|r| d.home.join("Library/Application Support").join(r)).collect(),
        Os::Windows => {
            def.win.iter().map(|(base, r)| if matches!(base, Base::Local) { d.local.join(r) } else { d.roaming.join(r) }).collect()
        }
    }
}

/// Cookie databases under a user-data folder (`Default`, `Profile N`; Opera keeps them in the root),
/// most recently used first. Newer versions use `Network/Cookies`, older ones `Cookies`.
pub fn cookie_dbs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = vec![root.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(root) {
        dirs.extend(
            rd.flatten()
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    n == "Default" || n.starts_with("Profile ")
                })
                .map(|e| e.path()),
        );
    }
    let mut dbs: Vec<PathBuf> = dirs.iter().flat_map(|p| [p.join("Network/Cookies"), p.join("Cookies")]).filter(|p| p.is_file()).collect();
    dbs.sort_by_key(|p| std::cmp::Reverse(mtime(p)));
    dbs
}

// ---- crypto ------------------------------------------------------------------------------------

fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    use sha1::{Digest, Sha1};
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..20].copy_from_slice(&Sha1::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha1::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha1::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// PBKDF2-HMAC-SHA1 for a derived key of at most 20 bytes (one block) – all Chromium needs.
pub fn pbkdf2_sha1(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 20] {
    let mut msg = salt.to_vec();
    msg.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha1(password, &msg);
    let mut t = u;
    for _ in 1..iterations {
        u = hmac_sha1(password, &u);
        for (a, b) in t.iter_mut().zip(u) {
            *a ^= b;
        }
    }
    t
}

/// AES-128 key from a Safe Storage password. Linux uses 1 iteration, macOS 1003.
pub fn cbc_key(password: &str, iterations: u32) -> [u8; 16] {
    pbkdf2_sha1(password.as_bytes(), b"saltysalt", iterations)[..16].try_into().unwrap()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Cbc([u8; 16]),
    Gcm([u8; 32]),
}

impl Key {
    /// Decrypt the part of a `vNN` blob after the 3-byte prefix.
    fn decrypt(&self, body: &[u8]) -> Option<Vec<u8>> {
        match self {
            Key::Cbc(key) => {
                use aes::cipher::{block_padding::Pkcs7, BlockModeDecrypt, KeyIvInit};
                cbc::Decryptor::<aes::Aes128>::new(key.into(), (&[b' '; 16]).into()).decrypt_padded_vec::<Pkcs7>(body).ok()
            }
            Key::Gcm(key) => {
                use aes_gcm::aead::{generic_array::GenericArray, Aead};
                use aes_gcm::{Aes256Gcm, KeyInit};
                // nonce (12) | ciphertext | tag (16)
                if body.len() < 12 + 16 {
                    return None;
                }
                let (nonce, ct) = body.split_at(12);
                Aes256Gcm::new(GenericArray::from_slice(key)).decrypt(GenericArray::from_slice(nonce), ct).ok()
            }
        }
    }
}

/// Where decryption keys come from. One implementation per OS; tests plug in fixed keys.
pub trait KeySource {
    /// Candidate keys for blobs with this prefix (`b"v10"`, `b"v11"`).
    fn keys(&mut self, prefix: &[u8]) -> Vec<Key>;
}

#[derive(Debug, PartialEq)]
pub enum Decrypted {
    Text(String),
    /// A scheme other apps cannot read (Windows app-bound `v20`).
    Unsupported(&'static str),
    Failed,
}

/// Since DB version 24 each value is prefixed with a SHA-256 of the host before encryption.
fn finish_plain(plain: Vec<u8>, strip_hash: bool) -> Option<String> {
    let bytes = if strip_hash { plain.get(32..)?.to_vec() } else { plain };
    let s = String::from_utf8(bytes).ok()?;
    s.chars().all(|c| !c.is_control()).then_some(s)
}

pub fn decrypt_cookie(enc: &[u8], keys: &mut dyn KeySource, strip_hash: bool) -> Decrypted {
    let Some(prefix) = enc.get(..3) else { return Decrypted::Failed };
    match prefix {
        b"v10" | b"v11" => {
            let body = &enc[3..];
            for key in keys.keys(prefix) {
                if let Some(text) = key.decrypt(body).and_then(|p| finish_plain(p, strip_hash)) {
                    return Decrypted::Text(text);
                }
            }
            Decrypted::Failed
        }
        b"v20" => {
            Decrypted::Unsupported("this browser version uses app-bound encryption (Chrome/Edge/Brave 127+), which other apps can't read")
        }
        _ => Decrypted::Failed,
    }
}

// ---- reading the cookie table ------------------------------------------------------------------

pub fn read(db: &Path, keys: &mut dyn KeySource) -> Result<String, String> {
    with_private_copy(db, |copy| cookies_from_db(copy, keys))
}

pub fn cookies_from_db(copy: &Path, keys: &mut dyn KeySource) -> Result<String, String> {
    let conn = rusqlite::Connection::open_with_flags(copy, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let version: i64 = conn
        .query_row("SELECT value FROM meta WHERE key = 'version'", [], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut stmt = conn
        .prepare("SELECT name, value, encrypted_value FROM cookies WHERE host_key = '.youtube.com' OR host_key = 'youtube.com' OR host_key = 'music.youtube.com' OR host_key = '.music.youtube.com' ORDER BY host_key DESC")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, Vec<u8>)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|e| e.to_string())?.flatten().collect();

    let mut seen = std::collections::HashSet::new();
    let mut parts = Vec::new();
    let mut unsupported = None;
    for (name, value, enc) in rows {
        if !seen.insert(name.clone()) {
            continue;
        }
        if !value.is_empty() {
            parts.push(format!("{name}={value}"));
            continue;
        }
        match decrypt_cookie(&enc, keys, version >= 24) {
            Decrypted::Text(v) => parts.push(format!("{name}={v}")),
            Decrypted::Unsupported(why) => unsupported = Some(why),
            Decrypted::Failed => {}
        }
    }
    if parts.is_empty() {
        return Err(match unsupported {
            Some(why) => format!("{why}. Paste your cookies, or use Firefox"),
            None => "couldn't decrypt the cookies (is the keyring/Keychain locked, or are you signed out?)".into(),
        });
    }
    finish_header(parts)
}

// ---- key sources (the only OS-specific part) ---------------------------------------------------

pub fn key_source(def: &Def, root: &Path) -> Box<dyn KeySource> {
    match Os::current() {
        Os::Linux => Box::new(StoreKeys::new(
            crate::platform::native_secret_stores(),
            format!("{} Safe Storage", def.product),
            def.app.to_owned(),
            1,
            true,
        )),
        Os::Mac => Box::new(StoreKeys::new(
            crate::platform::native_secret_stores(),
            format!("{} Safe Storage", def.product),
            def.product.to_owned(),
            1003,
            false,
        )),
        Os::Windows => Box::new(WindowsKeys { root: root.to_path_buf(), cached: None }),
    }
}

/// Keys derived from a "Safe Storage" password held by the OS secret stores (KWallet / Secret
/// Service / Keychain). Reading may pop up the usual "allow access" prompt – that is the user's call.
pub struct StoreKeys {
    stores: Vec<Box<dyn SecretStore>>,
    service: String,
    account_or_app: String,
    iterations: u32,
    /// Linux "basic" password store: v10 blobs use the fixed password "peanuts".
    peanuts: bool,
    cached: Option<Vec<Key>>,
}

impl StoreKeys {
    pub fn new(stores: Vec<Box<dyn SecretStore>>, service: String, account_or_app: String, iterations: u32, peanuts: bool) -> Self {
        StoreKeys { stores, service, account_or_app, iterations, peanuts, cached: None }
    }
}

impl KeySource for StoreKeys {
    fn keys(&mut self, prefix: &[u8]) -> Vec<Key> {
        let peanuts = Key::Cbc(cbc_key("peanuts", 1));
        if self.peanuts && prefix == b"v10" {
            return vec![peanuts];
        }
        let (stores, service, acct, iters) = (&self.stores, &self.service, &self.account_or_app, self.iterations);
        let mut keys = self
            .cached
            .get_or_insert_with(|| {
                // Errors are deliberately dropped: a missing wallet just means that key source is unavailable.
                all_passwords(stores, service, acct).unwrap_or_default().iter().map(|p| Key::Cbc(cbc_key(p, iters))).collect()
            })
            .clone();
        if self.peanuts {
            keys.push(peanuts);
        }
        keys
    }
}

struct WindowsKeys {
    root: PathBuf,
    cached: Option<Vec<Key>>,
}

impl KeySource for WindowsKeys {
    fn keys(&mut self, _prefix: &[u8]) -> Vec<Key> {
        let root = &self.root;
        self.cached
            .get_or_insert_with(|| {
                std::fs::read_to_string(root.join("Local State"))
                    .ok()
                    .and_then(|json| local_state_key_blob(&json))
                    .and_then(|blob| dpapi_unprotect(&blob))
                    .and_then(|k| <[u8; 32]>::try_from(k).ok())
                    .map(|k| vec![Key::Gcm(k)])
                    .unwrap_or_default()
            })
            .clone()
    }
}

/// `Local State` → `os_crypt.encrypted_key` (base64, `DPAPI` + a DPAPI blob) → the DPAPI blob.
pub fn local_state_key_blob(json: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let raw = base64::engine::general_purpose::STANDARD.decode(v["os_crypt"]["encrypted_key"].as_str()?).ok()?;
    raw.strip_prefix(b"DPAPI").map(<[u8]>::to_vec)
}

use crate::platform::dpapi_unprotect;

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{block_padding::Pkcs7, BlockModeEncrypt, KeyIvInit};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn enc_cbc(prefix: &[u8], key: &[u8; 16], plain: &[u8]) -> Vec<u8> {
        let ct = cbc::Encryptor::<aes::Aes128>::new(key.into(), (&[b' '; 16]).into()).encrypt_padded_vec::<Pkcs7>(plain);
        [prefix, &ct].concat()
    }

    fn enc_gcm(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
        use aes_gcm::aead::{generic_array::GenericArray, Aead};
        use aes_gcm::{Aes256Gcm, KeyInit};
        let nonce = [7u8; 12];
        let ct = Aes256Gcm::new(GenericArray::from_slice(key)).encrypt(GenericArray::from_slice(&nonce), plain).unwrap();
        [b"v10".as_slice(), &nonce, &ct].concat()
    }

    /// Keys handed out for any prefix.
    struct Fixed(Vec<Key>);
    impl KeySource for Fixed {
        fn keys(&mut self, _prefix: &[u8]) -> Vec<Key> {
            self.0.clone()
        }
    }

    #[test]
    fn pbkdf2_matches_rfc6070() {
        assert_eq!(hex(&pbkdf2_sha1(b"password", b"salt", 1)), "0c60c80f961f0e71f3a9b524af6012062fe037a6");
        assert_eq!(hex(&pbkdf2_sha1(b"password", b"salt", 2)), "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957");
        assert_eq!(hex(&pbkdf2_sha1(b"password", b"salt", 4096)), "4b007901b765489abead49d926f721d065a429c1");
    }

    #[test]
    fn chromium_keys_match_openssl() {
        // `openssl kdf -keylen 16 -kdfopt digest:SHA1 -kdfopt pass:peanuts -kdfopt salt:saltysalt -kdfopt iter:N PBKDF2`
        assert_eq!(hex(&cbc_key("peanuts", 1)), "fd621fe5a2b402539dfa147ca9272778"); // Linux
        assert_eq!(hex(&cbc_key("peanuts", 1003)), "d9a09d499b4e1b7461f28e67972c6dbd");
        // macOS
    }

    #[test]
    fn cbc_round_trip_and_wrong_key() {
        let key = cbc_key("peanuts", 1);
        let blob = enc_cbc(b"v10", &key, b"secret-cookie-value");
        let mut ks = Fixed(vec![Key::Cbc(key)]);
        assert_eq!(decrypt_cookie(&blob, &mut ks, false), Decrypted::Text("secret-cookie-value".into()));
        let mut wrong = Fixed(vec![Key::Cbc(cbc_key("other", 1))]);
        assert_eq!(decrypt_cookie(&blob, &mut wrong, false), Decrypted::Failed);
    }

    #[test]
    fn gcm_round_trip_and_wrong_key() {
        let key = [9u8; 32];
        let blob = enc_gcm(&key, b"windows-cookie");
        assert_eq!(decrypt_cookie(&blob, &mut Fixed(vec![Key::Gcm(key)]), false), Decrypted::Text("windows-cookie".into()));
        assert_eq!(decrypt_cookie(&blob, &mut Fixed(vec![Key::Gcm([1u8; 32])]), false), Decrypted::Failed);
        // truncated blobs must not panic
        assert_eq!(decrypt_cookie(b"v10short", &mut Fixed(vec![Key::Gcm(key)]), false), Decrypted::Failed);
    }

    #[test]
    fn host_hash_prefix_is_stripped_on_new_databases() {
        let key = cbc_key("peanuts", 1);
        let blob = enc_cbc(b"v10", &key, &[&[7u8; 32][..], b"abc"].concat());
        assert_eq!(decrypt_cookie(&blob, &mut Fixed(vec![Key::Cbc(key)]), true), Decrypted::Text("abc".into()));
    }

    #[test]
    fn app_bound_v20_is_reported_not_attempted() {
        match decrypt_cookie(b"v20abcdefgh", &mut Fixed(vec![]), false) {
            Decrypted::Unsupported(why) => assert!(why.contains("app-bound")),
            other => panic!("{other:?}"),
        }
        assert_eq!(decrypt_cookie(b"zz", &mut Fixed(vec![]), false), Decrypted::Failed);
        assert_eq!(decrypt_cookie(b"", &mut Fixed(vec![]), false), Decrypted::Failed);
    }

    #[test]
    fn local_state_key_extraction() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode([b"DPAPI".as_slice(), &[1, 2, 3, 4]].concat());
        let json = format!(r#"{{"os_crypt": {{"encrypted_key": "{b64}"}}}}"#);
        assert_eq!(local_state_key_blob(&json), Some(vec![1, 2, 3, 4]));
        assert_eq!(local_state_key_blob(r#"{"os_crypt": {}}"#), None);
        assert_eq!(local_state_key_blob("not json"), None);
        let no_prefix = base64::engine::general_purpose::STANDARD.encode([1, 2, 3]);
        assert_eq!(local_state_key_blob(&format!(r#"{{"os_crypt": {{"encrypted_key": "{no_prefix}"}}}}"#)), None);
    }

    #[test]
    fn user_data_roots_per_os() {
        let chrome = &DEFS[0];
        let lin = Dirs { home: "/home/u".into(), local: "/home/u/.local/share".into(), roaming: "/home/u/.config".into() };
        assert_eq!(user_data_roots(chrome, Os::Linux, &lin)[0], PathBuf::from("/home/u/.config/google-chrome"));
        let mac = Dirs {
            home: "/Users/u".into(),
            local: "/Users/u/Library/Application Support".into(),
            roaming: "/Users/u/Library/Application Support".into(),
        };
        assert_eq!(user_data_roots(chrome, Os::Mac, &mac)[0], PathBuf::from("/Users/u/Library/Application Support/Google/Chrome"));
        let win =
            Dirs { home: "C:/Users/u".into(), local: "C:/Users/u/AppData/Local".into(), roaming: "C:/Users/u/AppData/Roaming".into() };
        assert_eq!(user_data_roots(chrome, Os::Windows, &win)[0], PathBuf::from("C:/Users/u/AppData/Local/Google/Chrome/User Data"));
        let opera = DEFS.iter().find(|d| d.id == "opera").unwrap();
        assert_eq!(user_data_roots(opera, Os::Windows, &win)[0], PathBuf::from("C:/Users/u/AppData/Roaming/Opera Software/Opera Stable"));
    }

    /// A Cookies DB laid out like a real one (version-24 meta table, host-hash prefixed values).
    fn make_db(name: &str, rows: &[(&str, &str, &str, Vec<u8>)]) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ytm-test-chromium-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("Cookies");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute("CREATE TABLE meta (key TEXT, value TEXT)", []).unwrap();
        c.execute("INSERT INTO meta VALUES ('version', '24')", []).unwrap();
        c.execute("CREATE TABLE cookies (host_key TEXT, name TEXT, value TEXT, encrypted_value BLOB)", []).unwrap();
        for (host, n, v, e) in rows {
            c.execute("INSERT INTO cookies VALUES (?1, ?2, ?3, ?4)", rusqlite::params![host, n, v, e]).unwrap();
        }
        (dir, db)
    }

    fn prefixed(v: &str) -> Vec<u8> {
        [&[0u8; 32][..], v.as_bytes()].concat()
    }

    #[test]
    fn reads_linux_and_mac_style_db() {
        for iterations in [1u32, 1003] {
            let key = cbc_key("pw", iterations);
            let (dir, db) = make_db(
                &format!("cbc{iterations}"),
                &[
                    (".youtube.com", "SAPISID", "", enc_cbc(b"v11", &key, &prefixed("sapisid-value"))),
                    (".youtube.com", "SID", "", enc_cbc(b"v10", &key, &prefixed("sid-value"))),
                    (".google.com", "SAPISID", "", enc_cbc(b"v10", &key, &prefixed("wrong-host"))),
                    (".youtube.com", "PREF", "plain-value", vec![]),
                ],
            );
            let h = cookies_from_db(&db, &mut Fixed(vec![Key::Cbc(key)])).unwrap();
            assert!(h.contains("SAPISID=sapisid-value") && h.contains("SID=sid-value") && h.contains("PREF=plain-value"), "{h}");
            assert!(!h.contains("wrong-host"));
            std::fs::remove_dir_all(dir).ok();
        }
    }

    #[test]
    fn reads_windows_style_gcm_db() {
        let key = [3u8; 32];
        let (dir, db) = make_db(
            "gcm",
            &[
                (".youtube.com", "SAPISID", "", enc_gcm(&key, &prefixed("win-sapisid"))),
                (".youtube.com", "SID", "", enc_gcm(&key, &prefixed("win-sid"))),
            ],
        );
        let h = cookies_from_db(&db, &mut Fixed(vec![Key::Gcm(key)])).unwrap();
        assert!(h.contains("SAPISID=win-sapisid") && h.contains("SID=win-sid"), "{h}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn app_bound_database_gives_a_helpful_error() {
        let (dir, db) = make_db("v20", &[(".youtube.com", "SAPISID", "", b"v20xxxxxxxxxxxxxxxxxxxxxxxx".to_vec())]);
        let e = cookies_from_db(&db, &mut Fixed(vec![])).unwrap_err();
        assert!(e.contains("app-bound") && e.contains("Firefox"), "{e}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn locked_keyring_gives_a_helpful_error() {
        let key = cbc_key("pw", 1);
        let (dir, db) = make_db("locked", &[(".youtube.com", "SAPISID", "", enc_cbc(b"v11", &key, &prefixed("x")))]);
        let e = cookies_from_db(&db, &mut Fixed(vec![])).unwrap_err();
        assert!(e.contains("keyring"), "{e}");
        std::fs::remove_dir_all(dir).ok();
    }

    struct FakeStore(Result<Option<&'static str>, crate::platform::PlatformError>);
    impl SecretStore for FakeStore {
        fn password(&self, service: &str, acct: &str) -> Result<Option<String>, crate::platform::PlatformError> {
            assert_eq!(service, "Chrome Safe Storage");
            assert_eq!(acct, "chrome");
            self.0.clone().map(|o| o.map(str::to_owned))
        }
    }

    fn store_keys(stores: Vec<Box<dyn SecretStore>>, iters: u32, peanuts: bool) -> StoreKeys {
        StoreKeys::new(stores, "Chrome Safe Storage".into(), "chrome".into(), iters, peanuts)
    }

    #[test]
    fn fake_secret_store_yields_working_keys() {
        let key = cbc_key("wallet-pw", 1);
        let blob = enc_cbc(b"v11", &key, &prefixed("cookie-val"));
        let stores: Vec<Box<dyn SecretStore>> =
            vec![Box::new(FakeStore(Err(crate::platform::PlatformError::TimedOut))), Box::new(FakeStore(Ok(Some("wallet-pw"))))];
        let mut ks = store_keys(stores, 1, true);
        assert_eq!(decrypt_cookie(&blob, &mut ks, true), Decrypted::Text("cookie-val".into()));
        // v10 on Linux never asks a wallet: only the fixed "peanuts" key
        assert_eq!(ks.keys(b"v10"), vec![Key::Cbc(cbc_key("peanuts", 1))]);
    }

    #[test]
    fn macos_style_store_uses_1003_iterations_and_no_peanuts() {
        let mut ks = store_keys(vec![Box::new(FakeStore(Ok(Some("kc-pw"))))], 1003, false);
        assert_eq!(ks.keys(b"v10"), vec![Key::Cbc(cbc_key("kc-pw", 1003))]);
        let mut none = store_keys(vec![Box::new(FakeStore(Ok(None)))], 1003, false);
        assert!(none.keys(b"v10").is_empty());
    }

    #[test]
    fn finds_profiles_and_both_cookie_locations() {
        let root = std::env::temp_dir().join(format!("ytm-test-profiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for rel in ["Default/Cookies", "Profile 2/Network/Cookies", "System Profile/Cookies", "Network/Cookies"] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        }
        let found = cookie_dbs(&root);
        assert_eq!(found.len(), 3, "{found:?}"); // Default, Profile 2, and the root (Opera layout); not "System Profile"
        assert!(found.iter().all(|p| !p.to_string_lossy().contains("System Profile")));
        std::fs::remove_dir_all(root).ok();
    }
}
