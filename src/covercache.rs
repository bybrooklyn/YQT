// SPDX-License-Identifier: GPL-3.0-only
//! Cover cache: fetch album art once, keep it on disk, serve it from the local proxy
//! (`/t/<token>/cover/<hash>`). Covers then load faster and keep working offline.
//!
//! * files live in `<cache dir>/yqt/covers/<sha1 of the URL>` (private permissions), size-capped with an LRU
//!   (the file's modification time is its last use);
//! * only `https` URLs on a real host name are fetched (no IP literals, no `localhost`), responses are capped
//!   at 8 MB and must decode as a JPEG / PNG / WebP image;
//! * QML: `Covers.url(source)` returns the local URL when cached, otherwise the original URL while the fetch
//!   runs in the background (`Covers.revision` changes when something new arrived).
use crate::state::{ensure_private_dir, write_private};
use core::pin::Pin;
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

pub const DEFAULT_CAP_BYTES: u64 = 200 * 1024 * 1024;
pub const MAX_ITEM_BYTES: usize = 8 * 1024 * 1024;
/// Refuse absurd pixel counts (decompression bombs).
const MAX_DIMENSION: u32 = 8192;
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq, Eq)]
pub enum CoverError {
    NotFetchable,
    TooLarge,
    NotAnImage,
    Http(String),
    Io(String),
}

// ---- pure helpers ------------------------------------------------------------------------------

/// 40 hex characters: the cache key of a URL.
pub fn hash_url(url: &str) -> String {
    Sha1::digest(url.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// A cache key as it may appear in a path or URL.
pub fn valid_hash(h: &str) -> bool {
    h.len() == 40 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Only `https://<dns name>/…` may be fetched. IP literals and `localhost` are refused so a hostile item can't
/// aim the app at the local network.
pub fn is_fetchable(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.bytes().any(|b| b < 0x21 || b == b'\\') {
        return false;
    }
    let host =
        authority.rsplit_once(':').map_or(authority, |(h, port)| if port.bytes().all(|b| b.is_ascii_digit()) { h } else { authority });
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.starts_with('[') || host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return false;
    }
    // an IPv4 literal has only digits and dots
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return false;
    }
    host.contains('.') && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

/// Pull the cache key out of one of our own cover URLs (`http://127.0.0.1:<port>/t/<token>/cover/<hash>`).
pub fn hash_from_local_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    let h = rest.rsplit_once("/cover/")?.1;
    valid_hash(h).then(|| h.to_owned())
}

/// Mime type from the file's first bytes (what the proxy serves it as).
pub fn sniff_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "application/octet-stream"
    }
}

/// Decode `bytes` as JPEG / PNG / WebP; refuses oversized dimensions. Returns the decoded image.
pub fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage, CoverError> {
    use image::ImageFormat as F;
    let fmt = image::guess_format(bytes).map_err(|_| CoverError::NotAnImage)?;
    if !matches!(fmt, F::Jpeg | F::Png | F::WebP) {
        return Err(CoverError::NotAnImage);
    }
    let (w, h) = image::ImageReader::with_format(std::io::Cursor::new(bytes), fmt).into_dimensions().map_err(|_| CoverError::NotAnImage)?;
    if w == 0 || h == 0 || w > MAX_DIMENSION || h > MAX_DIMENSION {
        return Err(CoverError::NotAnImage);
    }
    image::load_from_memory_with_format(bytes, fmt).map_err(|_| CoverError::NotAnImage)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub accessed: SystemTime,
}

/// Which entries to delete (least recently used first) so the rest fits in `cap` bytes.
pub fn lru_victims(entries: &[Entry], cap: u64) -> Vec<String> {
    let mut total: u64 = entries.iter().map(|e| e.size).sum();
    let mut by_age: Vec<&Entry> = entries.iter().collect();
    by_age.sort_by(|a, b| a.accessed.cmp(&b.accessed).then_with(|| a.name.cmp(&b.name)));
    let mut out = Vec::new();
    for e in by_age {
        if total <= cap {
            break;
        }
        total -= e.size;
        out.push(e.name.clone());
    }
    out
}

// ---- the on-disk cache -------------------------------------------------------------------------

pub struct Cache {
    dir: PathBuf,
    cap: u64,
}

impl Cache {
    pub fn new(dir: PathBuf, cap: u64) -> Cache {
        Cache { dir, cap }
    }

    pub fn path_of(&self, hash: &str) -> Option<PathBuf> {
        valid_hash(hash).then(|| self.dir.join(hash))
    }

    pub fn has(&self, hash: &str) -> bool {
        self.path_of(hash).is_some_and(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
    }

    /// Mark as used now (for the LRU).
    pub fn touch(&self, hash: &str) {
        if let Some(p) = self.path_of(hash) {
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(p) {
                let _ = f.set_modified(SystemTime::now());
            }
        }
    }

    pub fn read(&self, hash: &str) -> Option<Vec<u8>> {
        let p = self.path_of(hash)?;
        let meta = std::fs::symlink_metadata(&p).ok()?;
        (meta.is_file() && meta.len() <= MAX_ITEM_BYTES as u64).then(|| std::fs::read(p).ok()).flatten()
    }

    /// Validate and store the image fetched from `url`; trims the cache afterwards. Returns the key.
    pub fn put(&self, url: &str, bytes: &[u8]) -> Result<String, CoverError> {
        if bytes.len() > MAX_ITEM_BYTES {
            return Err(CoverError::TooLarge);
        }
        decode_image(bytes)?;
        ensure_private_dir(&self.dir).map_err(|e| CoverError::Io(e.to_string()))?;
        let hash = hash_url(url);
        let path = self.dir.join(&hash);
        write_private(&path, bytes).map_err(|e| CoverError::Io(e.to_string()))?;
        self.prune();
        Ok(hash)
    }

    fn entries(&self) -> Vec<Entry> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        rd.flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if !valid_hash(&name) {
                    return None; // temp files and strangers are left alone
                }
                let m = e.metadata().ok().filter(std::fs::Metadata::is_file)?;
                Some(Entry { name, size: m.len(), accessed: m.modified().unwrap_or(SystemTime::UNIX_EPOCH) })
            })
            .collect()
    }

    #[allow(dead_code)] // Settings → Storage ("cover cache: x MB")
    pub fn total_bytes(&self) -> u64 {
        self.entries().iter().map(|e| e.size).sum()
    }

    /// Delete least-recently-used files until the cache fits its cap.
    pub fn prune(&self) {
        for name in lru_victims(&self.entries(), self.cap) {
            let _ = std::fs::remove_file(self.dir.join(name));
        }
    }

    #[allow(dead_code)] // Settings → Storage ("clear covers")
    pub fn clear(&self) {
        for e in self.entries() {
            let _ = std::fs::remove_file(self.dir.join(e.name));
        }
    }
}

/// Download at most `max` bytes (the body is dropped as soon as it exceeds the cap). The final URL must
/// still be `https` when `require_https` is set (a redirect to plain http is a downgrade).
pub async fn download_limited(client: &reqwest::Client, url: &str, max: usize, require_https: bool) -> Result<Vec<u8>, CoverError> {
    let mut resp = client.get(url).send().await.map_err(|e| CoverError::Http(e.without_url().to_string()))?;
    if !resp.status().is_success() {
        return Err(CoverError::Http(format!("status {}", resp.status().as_u16())));
    }
    if require_https && resp.url().scheme() != "https" {
        return Err(CoverError::NotFetchable);
    }
    if resp.content_length().is_some_and(|n| n > max as u64) {
        return Err(CoverError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| CoverError::Http(e.without_url().to_string()))? {
        if body.len() + chunk.len() > max {
            return Err(CoverError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Fetch `url` into `cache` (https only).
pub async fn fetch_into(cache: &Cache, client: &reqwest::Client, url: &str) -> Result<String, CoverError> {
    if !is_fetchable(url) {
        return Err(CoverError::NotFetchable);
    }
    let bytes = download_limited(client, url, MAX_ITEM_BYTES, true).await?;
    let (url, cache_dir, cap) = (url.to_owned(), cache.dir.clone(), cache.cap);
    // decoding is CPU work: keep it off the async workers
    tokio::task::spawn_blocking(move || Cache::new(cache_dir, cap).put(&url, &bytes)).await.map_err(|e| CoverError::Io(e.to_string()))?
}

// ---- the process-wide cache + QML --------------------------------------------------------------

pub fn global() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| {
        let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("yqt").join("covers");
        Cache::new(dir, DEFAULT_CAP_BYTES)
    })
}

/// The client for cover downloads. Unlike the shared one, every redirect hop must pass the same safety rule as the
/// first URL (https, a DNS name, not localhost), so a redirect cannot aim a request at the local network.
fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        let policy = reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && is_fetchable(attempt.url().as_str()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });
        let n = &crate::config::get().network;
        reqwest::Client::builder()
            .redirect(policy)
            .connect_timeout(Duration::from_secs(n.connect_timeout_secs))
            .timeout(Duration::from_secs(n.request_timeout_secs))
            .user_agent("yqt/0.1 (GPL-3.0 YouTube Music client)")
            .build()
            .expect("cover http client")
    })
}

/// At most this many cover downloads/decodes run at once.
static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(6);
const MAX_FAILED_REMEMBERED: usize = 512;
static IN_FLIGHT: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static FAILED: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
type Sub = Box<dyn Fn() + Send + Sync>;
static SUBS: Mutex<Vec<Sub>> = Mutex::new(Vec::new());

/// Be told whenever a new cover arrived in the cache.
pub fn subscribe(f: impl Fn() + Send + Sync + 'static) {
    SUBS.lock().unwrap().push(Box::new(f));
}

/// Start fetching `url` in the background unless it is cached, in flight, recently failed, or we are offline.
pub fn prefetch(url: &str) {
    let hash = hash_url(url);
    if !is_fetchable(url) || global().has(&hash) || !crate::connectivity::is_online() {
        return;
    }
    if FAILED.lock().unwrap().get_or_insert_default().get(&hash).is_some_and(|t| t.elapsed() < RETRY_AFTER_FAILURE) {
        return;
    }
    if !IN_FLIGHT.lock().unwrap().get_or_insert_default().insert(hash.clone()) {
        return;
    }
    let url = url.to_owned();
    crate::state::st().rt.spawn(async move {
        // A long list asks for hundreds of covers at once; only a few are fetched and decoded at a time.
        let _slot = SLOTS.acquire().await;
        let r = fetch_into(global(), client(), &url).await;
        IN_FLIGHT.lock().unwrap().get_or_insert_default().remove(&hash);
        match r {
            Ok(_) => {
                FAILED.lock().unwrap().get_or_insert_default().remove(&hash);
                for s in SUBS.lock().unwrap().iter() {
                    s();
                }
            }
            Err(_) => {
                let mut failed = FAILED.lock().unwrap();
                let map = failed.get_or_insert_default();
                if map.len() >= MAX_FAILED_REMEMBERED {
                    map.retain(|_, t| t.elapsed() < RETRY_AFTER_FAILURE); // forget the ones that may be retried anyway
                }
                map.insert(hash, Instant::now());
            }
        }
    });
}

/// What `Covers.url(source)` answers.
pub fn resolve(source: &str) -> String {
    if !source.starts_with("https://") {
        return source.to_owned(); // already local, a file, qrc:, empty …
    }
    let hash = hash_url(source);
    if global().has(&hash) {
        global().touch(&hash);
        return crate::proxy::cover_url(&hash);
    }
    prefetch(source);
    source.to_owned()
}

/// The image bytes behind a cover source (cached URL, or our own local cover URL). Fetches if needed.
pub async fn load(source: &str) -> Option<Vec<u8>> {
    let hash = if let Some(h) = hash_from_local_url(source) {
        h
    } else if is_fetchable(source) {
        let h = hash_url(source);
        if !global().has(&h) {
            // the cover client, not the shared one: its redirect policy checks every hop (see `client`)
            fetch_into(global(), client(), source).await.ok()?;
        }
        h
    } else {
        return None;
    };
    let cache = global();
    cache.touch(&hash);
    tokio::task::spawn_blocking(move || cache.read(&hash)).await.ok().flatten()
}

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// Bumped whenever a cover finished downloading (re-evaluate `url()` bindings on it).
        #[qproperty(i32, revision)]
        type Covers = super::CoversRust;

        /// The URL to show for `source`: the cached local copy when there is one (the fetch starts otherwise).
        #[qinvokable]
        fn url(self: &Covers, source: &QString) -> QString;
    }

    impl cxx_qt::Threading for Covers {}
    impl cxx_qt::Initialize for Covers {}
}

#[derive(Default)]
pub struct CoversRust {
    revision: i32,
}

impl cxx_qt::Initialize for qobject::Covers {
    fn initialize(self: Pin<&mut Self>) {
        use cxx_qt::Threading;
        let th = self.qt_thread();
        subscribe(move || {
            let _ = th.queue(|mut c| {
                let r = c.revision().wrapping_add(1);
                c.as_mut().set_revision(r);
            });
        });
    }
}

impl qobject::Covers {
    fn url(&self, source: &cxx_qt_lib::QString) -> cxx_qt_lib::QString {
        cxx_qt_lib::QString::from(&resolve(&source.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ytm-covers-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    pub fn encode(fmt: ImageFormat, w: u32, h: u32, c: [u8; 3]) -> Vec<u8> {
        let img = RgbImage::from_pixel(w, h, Rgb(c));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img).write_to(&mut out, fmt).unwrap();
        out.into_inner()
    }

    #[test]
    fn hashes_are_stable_hex_and_validated() {
        let h = hash_url("https://lh3.googleusercontent.com/a=w544");
        assert!(valid_hash(&h));
        assert_eq!(h, hash_url("https://lh3.googleusercontent.com/a=w544"));
        assert_ne!(h, hash_url("https://lh3.googleusercontent.com/a=w545"));
        for bad in ["", "abc", &"A".repeat(40), &"g".repeat(40), &"a".repeat(41), "../../../etc/passwd", &format!("{}/x", "a".repeat(38))] {
            assert!(!valid_hash(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_https_on_real_hostnames_is_fetchable() {
        for ok in ["https://i.ytimg.com/vi/x/hq.jpg", "https://lh3.googleusercontent.com/a=w544-h544", "https://example.com:8443/a.png"] {
            assert!(is_fetchable(ok), "{ok}");
        }
        for bad in [
            "http://i.ytimg.com/a.jpg",
            "file:///etc/passwd",
            "ftp://x.com/a",
            "//x.com/a",
            "",
            "https://",
            "https:///a",
            "https://localhost/a",
            "https://127.0.0.1/a",
            "https://127.0.0.1:8080/a",
            "https://10.0.0.1/a",
            "https://[::1]/a",
            "https://user@evil.com/a",
            "https://printer.local/a",
            "https://foo.localhost/a",
            "https://intranet/a",
            "https://evil.com\\@good.com/",
            "https://exa mple.com/a",
            "HTTPS://example.com/a",
        ] {
            assert!(!is_fetchable(bad), "{bad}");
        }
    }

    #[test]
    fn local_cover_urls_round_trip() {
        let h = hash_url("https://x.com/a.jpg");
        assert_eq!(hash_from_local_url(&format!("http://127.0.0.1:4242/t/tok/cover/{h}")), Some(h.clone()));
        assert_eq!(hash_from_local_url(&format!("http://evil.com/t/tok/cover/{h}")), None);
        assert_eq!(hash_from_local_url("http://127.0.0.1:4242/t/tok/cover/../../x"), None);
    }

    #[test]
    fn images_are_recognised_and_garbage_is_not() {
        for f in [ImageFormat::Png, ImageFormat::Jpeg] {
            assert!(decode_image(&encode(f, 8, 8, [10, 20, 30])).is_ok(), "{f:?}");
        }
        let png = encode(ImageFormat::Png, 4, 4, [0, 0, 0]);
        assert_eq!(sniff_mime(&png), "image/png");
        assert_eq!(sniff_mime(&encode(ImageFormat::Jpeg, 4, 4, [0, 0, 0])), "image/jpeg");
        assert_eq!(sniff_mime(b"<html></html>"), "application/octet-stream");
        for bad in [&b"<html>not an image</html>"[..], b"", b"\x89PNG\r\n\x1a\ntruncated", &[0xff, 0xd8, 0xff, 0x00]] {
            assert_eq!(decode_image(bad).err(), Some(CoverError::NotAnImage));
        }
        // a real format we do not accept
        assert_eq!(decode_image(b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").err(), Some(CoverError::NotAnImage));
    }

    fn e(name: &str, size: u64, secs: u64) -> Entry {
        Entry { name: name.into(), size, accessed: SystemTime::UNIX_EPOCH + Duration::from_secs(secs) }
    }

    #[test]
    fn lru_victims_are_oldest_first_and_stop_at_the_cap() {
        let v = [e("c", 40, 30), e("a", 40, 10), e("b", 40, 20)];
        assert!(lru_victims(&v, 120).is_empty());
        assert_eq!(lru_victims(&v, 100), ["a"]);
        assert_eq!(lru_victims(&v, 50), ["a", "b"]);
        assert_eq!(lru_victims(&v, 0), ["a", "b", "c"]);
        assert!(lru_victims(&[], 0).is_empty());
    }

    #[test]
    fn put_stores_privately_and_serves_back() {
        let dir = tmpdir("put");
        let c = Cache::new(dir.clone(), 1 << 20);
        let png = encode(ImageFormat::Png, 16, 16, [200, 30, 30]);
        let h = c.put("https://x.com/a.png", &png).unwrap();
        assert!(c.has(&h) && !c.has(&hash_url("https://x.com/other")));
        assert_eq!(c.read(&h).unwrap(), png);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join(&h)).unwrap().permissions().mode() & 0o077, 0);
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o077, 0);
        }
        assert!(c.path_of("../x").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn put_rejects_non_images_and_oversized_bodies() {
        let dir = tmpdir("reject");
        let c = Cache::new(dir.clone(), 1 << 20);
        assert_eq!(c.put("https://x.com/a", b"<html>hello</html>"), Err(CoverError::NotAnImage));
        assert_eq!(c.put("https://x.com/a", &vec![0u8; MAX_ITEM_BYTES + 1]), Err(CoverError::TooLarge));
        assert_eq!(c.total_bytes(), 0);
        assert!(!c.has(&hash_url("https://x.com/a")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_cache_trims_itself_least_recently_used_first() {
        let dir = tmpdir("lru");
        let one = encode(ImageFormat::Png, 64, 64, [1, 2, 3]);
        let c = Cache::new(dir.clone(), (one.len() as u64) * 2 + 10); // room for two
        let a = c.put("https://x.com/a", &one).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let b = c.put("https://x.com/b", &one).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        c.touch(&a); // a is now newer than b
        std::thread::sleep(Duration::from_millis(30));
        let d = c.put("https://x.com/d", &one).unwrap();
        assert!(c.has(&a) && c.has(&d), "recently used and newest survive");
        assert!(!c.has(&b), "least recently used is evicted");
        assert!(c.total_bytes() <= (one.len() as u64) * 2 + 10);
        c.clear();
        assert_eq!(c.total_bytes(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A local HTTP server: `/img` = a PNG, `/big` = 3 MB of junk, `/html` = text.
    async fn server(png: Vec<u8>) -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let png = png.clone();
                tokio::spawn(async move {
                    let mut b = [0u8; 2048];
                    let n = s.read(&mut b).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&b[..n]).into_owned();
                    let body: Vec<u8> = if req.starts_with("GET /img") {
                        png
                    } else if req.starts_with("GET /big") {
                        vec![7u8; 3 * 1024 * 1024]
                    } else {
                        b"<html>no</html>".to_vec()
                    };
                    let _ = s
                        .write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes())
                        .await;
                    let _ = s.write_all(&body).await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn download_limited_enforces_the_size_cap() {
        let png = encode(ImageFormat::Png, 8, 8, [9, 9, 9]);
        let port = server(png.clone()).await;
        let c = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{port}");
        assert_eq!(download_limited(&c, &format!("{base}/img"), 1 << 20, false).await.unwrap(), png);
        assert_eq!(download_limited(&c, &format!("{base}/big"), 1 << 20, false).await, Err(CoverError::TooLarge));
        // plain http is refused when https is required
        assert_eq!(download_limited(&c, &format!("{base}/img"), 1 << 20, true).await, Err(CoverError::NotFetchable));
    }

    #[tokio::test]
    async fn fetch_into_refuses_non_https_without_touching_the_network() {
        let dir = tmpdir("nohttp");
        let c = Cache::new(dir.clone(), 1 << 20);
        let r = fetch_into(&c, &reqwest::Client::new(), "http://127.0.0.1:1/a.png").await;
        assert_eq!(r, Err(CoverError::NotFetchable));
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn a_non_image_response_is_not_cached() {
        let port = server(Vec::new()).await;
        let body = download_limited(&reqwest::Client::new(), &format!("http://127.0.0.1:{port}/html"), 1 << 20, false).await.unwrap();
        let dir = tmpdir("html");
        let c = Cache::new(dir.clone(), 1 << 20);
        assert_eq!(c.put("https://x.com/html", &body), Err(CoverError::NotAnImage));
        assert_eq!(c.total_bytes(), 0);
    }
}
