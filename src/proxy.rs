// SPDX-License-Identifier: GPL-3.0-only
//! Tiny localhost HTTP proxy for audio streams.
//!
//! Qt's ffmpeg backend isn't guaranteed to speak https (distro builds without TLS
//! exist), so the player gets `http://127.0.0.1:<port>/t/<token>/<videoId>` and this proxy
//! fetches the real googlevideo URL over https, forwarding Range requests so
//! seeking works.
//!
//! Hardening (anything on the machine can connect to a loopback port, and a web page can aim
//! DNS-rebinding requests at it):
//! * a random per-run 256-bit token is part of the path and compared in constant time;
//! * the `Host` header must be exactly `127.0.0.1:<port>` or `localhost:<port>`;
//! * only GET and HEAD; the id must match `[A-Za-z0-9_-]{1,64}`; anything else is a bare 404;
//! * request heads are capped at 16 KiB and must arrive within a few seconds;
//! * upstream URLs are never logged.
use crate::config;
use crate::state::st;
use rand::Rng;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static URLS: Mutex<Option<HashMap<String, (String, String)>>> = Mutex::new(None);
static CTX: OnceLock<Arc<Ctx>> = OnceLock::new();

const MAX_HEAD: usize = 16 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// What a request is checked against.
struct Ctx {
    port: u16,
    /// `None` = legacy `/<id>` URLs (config `security.proxy_token = false`).
    token: Option<String>,
    /// Upstream fetch granularity (`network.proxy_chunk_bytes`).
    chunk: u64,
    /// Overrides for where `/dl/` and `/cover/` read from (tests); `None` = the app's real folders.
    dl_dir: Option<std::path::PathBuf>,
    cover_dir: Option<std::path::PathBuf>,
}

/// Why a request was refused. The wire response never says more than the status line.
#[derive(Debug, PartialEq, Eq)]
enum Reject {
    Host,
    Method,
    NotFound,
}

fn new_token() -> String {
    let mut bytes = [0u8; 32]; // 256 bits from the OS-seeded CSPRNG
    rand::rng().fill(&mut bytes[..]);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time equality (the length is not secret).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// What an authorized request asks for.
#[derive(Debug, PartialEq, Eq)]
enum Route<'a> {
    /// `/t/<token>/<videoId>`: a remote audio stream.
    Stream(&'a str),
    /// `/t/<token>/dl/<videoId>`: a downloaded file from disk.
    Download(&'a str),
    /// `/t/<token>/cover/<hash>`: a cached cover image.
    Cover(&'a str),
}

/// Validate a request line + Host header; on success returns what is being asked for (every id/hash already
/// checked against its own charset).
fn authorize<'a>(ctx: &Ctx, method: &str, path: &'a str, host: Option<&str>) -> Result<Route<'a>, Reject> {
    let host_ok = host.is_some_and(|h| {
        let h = h.trim();
        h.eq_ignore_ascii_case(&format!("127.0.0.1:{}", ctx.port)) || h.eq_ignore_ascii_case(&format!("localhost:{}", ctx.port))
    });
    if !host_ok {
        return Err(Reject::Host);
    }
    if method != "GET" && method != "HEAD" {
        return Err(Reject::Method);
    }
    let rest = match &ctx.token {
        Some(token) => {
            let (given, rest) = path.strip_prefix("/t/").and_then(|r| r.split_once('/')).ok_or(Reject::NotFound)?;
            if !ct_eq(given.as_bytes(), token.as_bytes()) {
                return Err(Reject::NotFound);
            }
            rest
        }
        None => path.strip_prefix('/').ok_or(Reject::NotFound)?,
    };
    if let Some(id) = rest.strip_prefix("dl/") {
        return if crate::downloads::quality::valid_id(id) { Ok(Route::Download(id)) } else { Err(Reject::NotFound) };
    }
    if let Some(hash) = rest.strip_prefix("cover/") {
        return if crate::covercache::valid_hash(hash) { Ok(Route::Cover(hash)) } else { Err(Reject::NotFound) };
    }
    if valid_id(rest) {
        Ok(Route::Stream(rest))
    } else {
        Err(Reject::NotFound)
    }
}

fn ctx() -> &'static Arc<Ctx> {
    CTX.get_or_init(|| {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let port = l.local_addr().unwrap().port();
        let token = config::get().security.proxy_token.then(new_token);
        let ctx =
            Arc::new(Ctx { port, token, chunk: config::get().network.proxy_chunk_bytes.max(64 * 1024), dl_dir: None, cover_dir: None });
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(config::get().network.connect_timeout_secs))
            .read_timeout(Duration::from_secs(config::get().network.stream_read_timeout_secs))
            .build()
            .expect("http client");
        serve(l, ctx.clone(), client, st().rt.handle());
        ctx
    })
}

fn serve(l: std::net::TcpListener, ctx: Arc<Ctx>, client: reqwest::Client, rt: &tokio::runtime::Handle) {
    l.set_nonblocking(true).ok();
    rt.spawn(async move {
        let l = TcpListener::from_std(l).expect("proxy listener");
        while let Ok((sock, _)) = l.accept().await {
            let (client, ctx) = (client.clone(), ctx.clone());
            tokio::spawn(async move {
                let _ = handle(sock, client, &ctx).await;
            });
        }
    });
}

/// Register a real stream URL and get the local URL to hand to the media player.
pub fn local_url(id: &str, real: &str, ua: &str) -> String {
    URLS.lock().unwrap().get_or_insert_default().insert(id.to_owned(), (real.to_owned(), ua.to_owned()));
    let c = ctx();
    match &c.token {
        Some(t) => format!("http://127.0.0.1:{}/t/{}/{}", c.port, t, id),
        None => format!("http://127.0.0.1:{}/{}", c.port, id),
    }
}

fn base_url(path: &str) -> String {
    let c = ctx();
    match &c.token {
        Some(t) => format!("http://127.0.0.1:{}/t/{}/{}", c.port, t, path),
        None => format!("http://127.0.0.1:{}/{}", c.port, path),
    }
}

/// Local URL (with Range support) for a downloaded song; the caller has checked it exists.
pub fn download_url(id: &str) -> String {
    base_url(&format!("dl/{id}"))
}

/// Local URL of a cached cover (`hash` = `covercache::hash_url`).
pub fn cover_url(hash: &str) -> String {
    base_url(&format!("cover/{hash}"))
}

/// Serve a regular file from disk with `Range` support (200 / 206 / 416). `HEAD` sends only the headers.
async fn serve_file(
    sock: &mut TcpStream,
    path: &std::path::Path,
    mime: &str,
    method: &str,
    range: Option<&str>,
    cacheable: bool,
) -> std::io::Result<()> {
    use crate::downloads::{parse_byte_range, RangeOutcome};
    use tokio::io::AsyncSeekExt;
    let Ok(mut file) = tokio::fs::File::open(path).await else { return reply(sock, "404 Not Found").await };
    let len = match file.metadata().await {
        Ok(m) if m.is_file() => m.len(),
        _ => return reply(sock, "404 Not Found").await,
    };
    let extra = if cacheable { "Cache-Control: private, max-age=86400\r\n" } else { "" };
    let (start, end, head) = match parse_byte_range(range, len) {
        RangeOutcome::Unsatisfiable => {
            return sock
                .write_all(format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes())
                .await;
        }
        RangeOutcome::Partial { start, end } => (
            start,
            end,
            format!("HTTP/1.1 206 Partial Content\r\nContent-Type: {mime}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{len}\r\nContent-Length: {}\r\n{extra}X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n", end + 1 - start),
        ),
        RangeOutcome::Full => (
            0,
            len.saturating_sub(1),
            format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nAccept-Ranges: bytes\r\nContent-Length: {len}\r\n{extra}X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n"),
        ),
    };
    sock.write_all(head.as_bytes()).await?;
    if method == "HEAD" || len == 0 {
        return Ok(());
    }
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut left = end + 1 - start;
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..want]).await?;
        if n == 0 {
            break; // the file shrank under us
        }
        sock.write_all(&buf[..n]).await?; // client closing (seek/skip) ends the loop
        left -= n as u64;
    }
    Ok(())
}

async fn serve_cover(
    sock: &mut TcpStream,
    cache: &crate::covercache::Cache,
    hash: &str,
    method: &str,
    range: Option<&str>,
) -> std::io::Result<()> {
    let Some(path) = cache.path_of(hash).filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file())) else {
        return reply(sock, "404 Not Found").await;
    };
    let mut magic = [0u8; 16];
    let n = match tokio::fs::File::open(&path).await {
        Ok(mut f) => f.read(&mut magic).await.unwrap_or(0),
        Err(_) => 0,
    };
    let mime = crate::covercache::sniff_mime(&magic[..n]);
    if mime == "application/octet-stream" {
        return reply(sock, "404 Not Found").await; // never serve something that is not an image
    }
    cache.touch(hash);
    serve_file(sock, &path, mime, method, range, true).await
}

/// `bytes=a-b` / `bytes=a-` / none → (start, optional inclusive end).
fn parse_range(range: Option<&str>) -> (u64, Option<u64>) {
    match range.and_then(|r| r.strip_prefix("bytes=")) {
        Some(r) => {
            let (a, b) = r.split_once('-').unwrap_or(("0", ""));
            (a.trim().parse().unwrap_or(0), b.trim().parse().ok())
        }
        None => (0, None),
    }
}

async fn reply(sock: &mut TcpStream, status: &str) -> std::io::Result<()> {
    sock.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await
}

/// Read up to the end of the request head. `Ok(None)` = too large / closed / too slow.
async fn read_head(sock: &mut TcpStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let read = async {
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            if buf.len() > MAX_HEAD {
                return Ok(Some(false));
            }
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                return Ok(None);
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        Ok::<_, std::io::Error>(Some(true))
    };
    match tokio::time::timeout(HEAD_TIMEOUT, read).await {
        Ok(Ok(Some(true))) => Ok(Some(buf)),
        Ok(Ok(Some(false))) => {
            reply(sock, "431 Request Header Fields Too Large").await?;
            Ok(None)
        }
        Ok(Ok(None)) | Err(_) => Ok(None),
        Ok(Err(e)) => Err(e),
    }
}

async fn handle(mut sock: TcpStream, client: reqwest::Client, ctx: &Ctx) -> std::io::Result<()> {
    let Some(buf) = read_head(&mut sock).await? else { return Ok(()) };
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let (mut range, mut host) = (None, None);
    for (k, v) in lines.filter_map(|l| l.split_once(':')) {
        if k.eq_ignore_ascii_case("range") {
            range = Some(v.trim().to_owned());
        } else if k.eq_ignore_ascii_case("host") {
            host = Some(v.trim().to_owned());
        }
    }

    let id = match authorize(ctx, method, path, host.as_deref()) {
        Ok(Route::Stream(id)) => id,
        Ok(Route::Download(id)) => {
            let src = match &ctx.dl_dir {
                Some(d) => crate::downloads::local_source_in(d, id),
                None => crate::downloads::local_source(id),
            };
            return match src {
                Some(s) => serve_file(&mut sock, &s.path, s.mime, method, range.as_deref(), false).await,
                None => reply(&mut sock, "404 Not Found").await,
            };
        }
        Ok(Route::Cover(hash)) => {
            return match &ctx.cover_dir {
                Some(d) => {
                    let cache = crate::covercache::Cache::new(d.clone(), crate::covercache::DEFAULT_CAP_BYTES);
                    serve_cover(&mut sock, &cache, hash, method, range.as_deref()).await
                }
                None => serve_cover(&mut sock, crate::covercache::global(), hash, method, range.as_deref()).await,
            };
        }
        Err(Reject::Host) => return reply(&mut sock, "403 Forbidden").await,
        Err(Reject::Method) => {
            return sock
                .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nAllow: GET, HEAD\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
        }
        Err(Reject::NotFound) => return reply(&mut sock, "404 Not Found").await,
    };
    let real = URLS.lock().unwrap().get_or_insert_default().get(id).cloned();
    let Some((real, ua)) = real else {
        return reply(&mut sock, "404 Not Found").await;
    };

    // googlevideo refuses open-ended / unbounded Range requests, so everything is
    // fetched in bounded chunks (like yt-dlp's http_chunk_size) and stitched together.
    let ch = ctx.chunk;
    let (start, req_end) = parse_range(range.as_deref());
    let fetch = |a: u64, b: u64| client.get(&real).header("User-Agent", &ua).header("Range", format!("bytes={a}-{b}")).send();
    let mut resp = match fetch(start, req_end.map_or(start.saturating_add(ch - 1), |e| e.min(start.saturating_add(ch - 1)))).await {
        Ok(r) => r,
        Err(_) => return reply(&mut sock, "502 Bad Gateway").await, // never include the error: it carries the URL
    };
    let status = resp.status();
    if status.as_u16() != 206 {
        let out = format!("{} {}", status.as_u16(), status.canonical_reason().unwrap_or(""));
        return reply(&mut sock, &out).await;
    }
    let total: u64 = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('/').next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let end = req_end.unwrap_or(total.saturating_sub(1)).min(total.saturating_sub(1));
    if end < start {
        return reply(&mut sock, "416 Range Not Satisfiable").await;
    }
    let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_owned();
    // Don't let a hostile upstream smuggle header lines through the content type.
    let ctype = if ctype.bytes().any(|b| b < 0x20 || b == 0x7f) { "application/octet-stream".to_owned() } else { ctype };
    let out = if range.is_some() {
        format!("HTTP/1.1 206 Partial Content\r\nContent-Type: {ctype}\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", end + 1 - start)
    } else {
        format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nAccept-Ranges: bytes\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n")
    };
    sock.write_all(out.as_bytes()).await?;
    if method == "HEAD" {
        return Ok(());
    }
    let mut pos = start;
    loop {
        let before = pos;
        while let Ok(Some(bytes)) = resp.chunk().await {
            // never send more than the Content-Length we promised, whatever the upstream does
            let take = (bytes.len() as u64).min(end + 1 - pos) as usize;
            pos += take as u64;
            sock.write_all(&bytes[..take]).await?; // client closing (seek/skip) ends the loop
            if pos > end {
                return Ok(());
            }
        }
        // An empty or failing answer made no progress: stop instead of asking for the same range forever.
        if pos > end || pos == before {
            return Ok(());
        }
        resp = match fetch(pos, end.min(pos.saturating_add(ch - 1))).await {
            Ok(r) if r.status().as_u16() == 206 => r,
            _ => return Ok(()),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_forms() {
        assert_eq!(parse_range(None), (0, None));
        assert_eq!(parse_range(Some("bytes=100-")), (100, None));
        assert_eq!(parse_range(Some("bytes=100-199")), (100, Some(199)));
        assert_eq!(parse_range(Some("bytes=-500")), (0, Some(500)));
        assert_eq!(parse_range(Some("garbage")), (0, None));
    }

    #[test]
    fn tokens_are_256_bit_hex_and_distinct() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn id_charset() {
        assert!(valid_id("dQw4w9WgXcQ") && valid_id("a") && valid_id(&"a".repeat(64)));
        for bad in ["", "a b", "a/b", "a.b", "a%2f", "..", "a?x=1", "a#b", "é", "a\n", &"a".repeat(65)] {
            assert!(!valid_id(bad), "{bad:?}");
        }
    }

    fn ctx_for(token: Option<&str>) -> Ctx {
        Ctx { port: 4242, token: token.map(str::to_owned), chunk: 1 << 20, dl_dir: None, cover_dir: None }
    }

    #[test]
    fn authorize_checks_each_rule() {
        let c = ctx_for(Some("secret"));
        let h = Some("127.0.0.1:4242");
        assert_eq!(authorize(&c, "GET", "/t/secret/abc_-1", h), Ok(Route::Stream("abc_-1")));
        assert_eq!(authorize(&c, "HEAD", "/t/secret/abc", Some("LOCALHOST:4242")), Ok(Route::Stream("abc")));
        // host allow-list (DNS rebinding)
        for bad in [
            None,
            Some("evil.example"),
            Some("evil.example:4242"),
            Some("127.0.0.1"),
            Some("127.0.0.1:1"),
            Some("localhost"),
            Some("127.0.0.1:4242.evil.io"),
        ] {
            assert_eq!(authorize(&c, "GET", "/t/secret/abc", bad), Err(Reject::Host), "{bad:?}");
        }
        // methods
        for m in ["POST", "PUT", "DELETE", "OPTIONS", "get", ""] {
            assert_eq!(authorize(&c, m, "/t/secret/abc", h), Err(Reject::Method), "{m}");
        }
        // token
        for p in ["/t/wrong/abc", "/t//abc", "/t/secre/abc", "/t/secret2/abc", "/abc", "/t/secret", "/t/secret/", "/", ""] {
            assert_eq!(authorize(&c, "GET", p, h), Err(Reject::NotFound), "{p}");
        }
        // id tricks after a valid token
        for p in [
            "/t/secret/abc?x=1",
            "/t/secret/a/b",
            "/t/secret/../abc",
            "/t/secret/a%2e",
            "/t/secret/abc#f",
            &format!("/t/secret/{}", "a".repeat(65)),
        ] {
            assert_eq!(authorize(&c, "GET", p, h), Err(Reject::NotFound), "{p}");
        }
    }

    #[test]
    fn legacy_mode_uses_plain_ids() {
        let c = ctx_for(None);
        let h = Some("localhost:4242");
        assert_eq!(authorize(&c, "GET", "/abc", h), Ok(Route::Stream("abc")));
        assert_eq!(authorize(&c, "GET", "/t/secret/abc", h), Err(Reject::NotFound));
        assert_eq!(authorize(&c, "GET", "/a/b", h), Err(Reject::NotFound));
        assert_eq!(authorize(&c, "GET", "/", h), Err(Reject::NotFound));
        assert_eq!(authorize(&c, "POST", "/abc", h), Err(Reject::Method));
    }

    // ---- end to end over real sockets (no st(), no config::get(), no user data) ----

    /// A fake googlevideo: answers every ranged GET with a 5-byte 206.
    async fn fake_upstream() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut b = [0u8; 4096];
                    let _ = s.read(&mut b).await;
                    let _ = s
                        .write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-4/5\r\nContent-Type: audio/webm\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")
                        .await;
                });
            }
        });
        port
    }

    async fn start(token: Option<&str>) -> u16 {
        start_with(std::net::TcpListener::bind("127.0.0.1:0").unwrap(), token, None, None)
    }

    fn start_with(
        l: std::net::TcpListener,
        token: Option<&str>,
        dl_dir: Option<std::path::PathBuf>,
        cover_dir: Option<std::path::PathBuf>,
    ) -> u16 {
        let port = l.local_addr().unwrap().port();
        let c = Arc::new(Ctx { port, token: token.map(str::to_owned), chunk: 1 << 20, dl_dir, cover_dir });
        serve(l, c, reqwest::Client::new(), &tokio::runtime::Handle::current());
        port
    }

    async fn raw(port: u16, request: &[u8]) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let _ = s.write_all(request).await;
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).into_owned()
    }

    fn req(method: &str, path: &str, host: &str) -> Vec<u8> {
        format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n\r\n").into_bytes()
    }

    #[tokio::test]
    async fn end_to_end_rejections_and_success() {
        let up = fake_upstream().await;
        URLS.lock().unwrap().get_or_insert_default().insert("w7test".into(), (format!("http://127.0.0.1:{up}/x?sig=SECRET"), "ua".into()));
        let port = start(Some("tok")).await;
        let host = format!("127.0.0.1:{port}");

        let ok = raw(port, &req("GET", "/t/tok/w7test", &host)).await;
        assert!(ok.starts_with("HTTP/1.1 200 OK"), "{ok}");
        assert!(ok.ends_with("hello"), "{ok}");
        let head = raw(port, &req("HEAD", "/t/tok/w7test", &format!("localhost:{port}"))).await;
        assert!(head.starts_with("HTTP/1.1 200 OK") && !head.contains("hello"), "{head}");

        // wrong token / unknown id / bad id / legacy path: identical bare 404
        for p in ["/t/bad/w7test", "/t/tok/unknown", "/t/tok/a%2fb", "/w7test", "/t/tok/w7test?x"] {
            let r = raw(port, &req("GET", p, &host)).await;
            assert_eq!(r, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", "{p}");
        }
        // DNS rebinding
        let r = raw(port, &req("GET", "/t/tok/w7test", "evil.example")).await;
        assert!(r.starts_with("HTTP/1.1 403") && !r.contains("hello"), "{r}");
        // method
        let r = raw(port, &req("POST", "/t/tok/w7test", &host)).await;
        assert!(r.starts_with("HTTP/1.1 405"), "{r}");
        // oversized head
        let mut big = b"GET /t/tok/w7test HTTP/1.1\r\nX: ".to_vec();
        big.extend(std::iter::repeat_n(b'a', 40 * 1024));
        let r = raw(port, &big).await;
        assert!(r.starts_with("HTTP/1.1 431") && !r.contains("hello"), "{r}");
    }

    // ---- local files: /dl/<id> and /cover/<hash> ----

    #[test]
    fn authorize_routes_downloads_and_covers_with_the_same_checks() {
        let c = ctx_for(Some("secret"));
        let h = Some("127.0.0.1:4242");
        let hash = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(authorize(&c, "GET", "/t/secret/dl/dQw4w9WgXcQ", h), Ok(Route::Download("dQw4w9WgXcQ")));
        assert_eq!(authorize(&c, "HEAD", &format!("/t/secret/cover/{hash}"), h), Ok(Route::Cover(hash)));
        // token, host and method are still enforced
        assert_eq!(authorize(&c, "GET", "/t/wrong/dl/dQw4w9WgXcQ", h), Err(Reject::NotFound));
        assert_eq!(authorize(&c, "GET", "/dl/dQw4w9WgXcQ", h), Err(Reject::NotFound));
        assert_eq!(authorize(&c, "GET", "/t/secret/dl/dQw4w9WgXcQ", Some("evil.example")), Err(Reject::Host));
        assert_eq!(authorize(&c, "POST", "/t/secret/dl/dQw4w9WgXcQ", h), Err(Reject::Method));
        assert_eq!(authorize(&c, "DELETE", &format!("/t/secret/cover/{hash}"), h), Err(Reject::Method));
        // id / hash charsets
        for p in [
            "/t/secret/dl/",
            "/t/secret/dl/short",
            "/t/secret/dl/dQw4w9WgXcQ1",
            "/t/secret/dl/../x",
            "/t/secret/dl/dQw4w9WgXc%2f",
            "/t/secret/dl/dQw4w9WgXcQ?x=1",
            "/t/secret/dl/dl/dQw4w9WgXcQ",
            "/t/secret/cover/",
            "/t/secret/cover/../../etc/passwd",
            "/t/secret/cover/ABCDEF0123456789ABCDEF0123456789ABCDEF01",
            "/t/secret/cover/0123456789abcdef0123456789abcdef0123456",
            "/t/secret/covers/0123456789abcdef0123456789abcdef01234567",
        ] {
            assert_eq!(authorize(&c, "GET", p, h), Err(Reject::NotFound), "{p}");
        }
    }

    async fn raw_bytes(port: u16, request: &[u8]) -> Vec<u8> {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let _ = s.write_all(request).await;
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out)).await;
        out
    }

    fn ranged(path: &str, host: &str, range: &str) -> Vec<u8> {
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nRange: {range}\r\n\r\n").into_bytes()
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ytm-proxy-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn serves_downloads_from_disk_with_ranges() {
        let dir = tmpdir("dl");
        let body: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        std::fs::write(dir.join("dQw4w9WgXcQ.m4a"), &body).unwrap();
        let port = start_with(std::net::TcpListener::bind("127.0.0.1:0").unwrap(), Some("tok"), Some(dir.clone()), None);
        let host = format!("127.0.0.1:{port}");
        let path = "/t/tok/dl/dQw4w9WgXcQ";

        let full = raw_bytes(port, &req("GET", path, &host)).await;
        let text = String::from_utf8_lossy(&full[..200.min(full.len())]).into_owned();
        assert!(
            text.starts_with("HTTP/1.1 200 OK")
                && text.contains("Content-Length: 1000")
                && text.contains("audio/mp4")
                && text.contains("Accept-Ranges: bytes"),
            "{text}"
        );
        assert_eq!(&full[full.len() - 1000..], &body[..]);

        let part = raw_bytes(port, &ranged(path, &host, "bytes=100-199")).await;
        let split = part.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let head = String::from_utf8_lossy(&part[..split]).into_owned();
        assert!(
            head.starts_with("HTTP/1.1 206") && head.contains("Content-Range: bytes 100-199/1000") && head.contains("Content-Length: 100"),
            "{head}"
        );
        assert_eq!(&part[split..], &body[100..200]);

        let tail = raw_bytes(port, &ranged(path, &host, "bytes=990-")).await;
        assert_eq!(&tail[tail.len() - 10..], &body[990..]);
        let suffix = raw_bytes(port, &ranged(path, &host, "bytes=-5")).await;
        assert_eq!(&suffix[suffix.len() - 5..], &body[995..]);

        let bad = String::from_utf8_lossy(&raw_bytes(port, &ranged(path, &host, "bytes=1000-")).await).into_owned();
        assert!(bad.starts_with("HTTP/1.1 416") && bad.contains("Content-Range: bytes */1000"), "{bad}");

        let head_only = String::from_utf8_lossy(&raw_bytes(port, &req("HEAD", path, &host)).await).into_owned();
        assert!(head_only.starts_with("HTTP/1.1 200") && head_only.ends_with("\r\n\r\n"), "no body on HEAD");

        // rejections: token, host, method, unknown / invalid id → nothing from the folder is served
        let nf = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        for p in ["/t/bad/dl/dQw4w9WgXcQ", "/t/tok/dl/zzzzzzzzzzz", "/t/tok/dl/..%2f..%2fx", "/t/tok/dl/dQw4w9WgXcQ.m4a", "/dl/dQw4w9WgXcQ"]
        {
            assert_eq!(raw(port, &req("GET", p, &host)).await, nf, "{p}");
        }
        assert!(raw(port, &req("GET", path, "evil.example")).await.starts_with("HTTP/1.1 403"));
        assert!(raw(port, &req("POST", path, &host)).await.starts_with("HTTP/1.1 405"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn serves_cached_covers_but_never_non_images() {
        let dir = tmpdir("cover");
        let cache = crate::covercache::Cache::new(dir.clone(), 1 << 20);
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([200, 10, 10])))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        let hash = cache.put("https://x.example/a.png", &png).unwrap();
        // a planted non-image under a valid-looking name is refused
        let evil = crate::covercache::hash_url("https://x.example/evil");
        std::fs::write(dir.join(&evil), b"<html>secrets</html>").unwrap();
        let port = start_with(std::net::TcpListener::bind("127.0.0.1:0").unwrap(), Some("tok"), None, Some(dir.clone()));
        let host = format!("127.0.0.1:{port}");

        let r = raw_bytes(port, &req("GET", &format!("/t/tok/cover/{hash}"), &host)).await;
        let text = String::from_utf8_lossy(&r[..120.min(r.len())]).into_owned();
        assert!(text.starts_with("HTTP/1.1 200 OK") && text.contains("image/png"), "{text}");
        assert_eq!(&r[r.len() - png.len()..], &png[..]);
        let nf = raw(port, &req("GET", &format!("/t/tok/cover/{evil}"), &host)).await;
        assert!(nf.starts_with("HTTP/1.1 404") && !nf.contains("secrets"), "{nf}");
        let missing = crate::covercache::hash_url("https://x.example/none");
        assert!(raw(port, &req("GET", &format!("/t/tok/cover/{missing}"), &host)).await.starts_with("HTTP/1.1 404"));
        assert!(raw(port, &req("GET", &format!("/t/nope/cover/{hash}"), &host)).await.starts_with("HTTP/1.1 404"));
        assert!(raw(port, &req("GET", &format!("/t/tok/cover/{hash}"), "evil.example")).await.starts_with("HTTP/1.1 403"));
        assert!(raw(port, &req("PUT", &format!("/t/tok/cover/{hash}"), &host)).await.starts_with("HTTP/1.1 405"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn legacy_mode_end_to_end() {
        let up = fake_upstream().await;
        URLS.lock().unwrap().get_or_insert_default().insert("w7legacy".into(), (format!("http://127.0.0.1:{up}/x"), "ua".into()));
        let port = start(None).await;
        let r = raw(port, &req("GET", "/w7legacy", &format!("127.0.0.1:{port}"))).await;
        assert!(r.starts_with("HTTP/1.1 200 OK") && r.ends_with("hello"), "{r}");
    }
}
