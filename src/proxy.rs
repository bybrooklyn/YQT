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

/// Validate a request line + Host header; on success returns the video id.
fn authorize<'a>(ctx: &Ctx, method: &str, path: &'a str, host: Option<&str>) -> Result<&'a str, Reject> {
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
    let id = match &ctx.token {
        Some(token) => {
            let (given, id) = path.strip_prefix("/t/").and_then(|r| r.split_once('/')).ok_or(Reject::NotFound)?;
            if !ct_eq(given.as_bytes(), token.as_bytes()) {
                return Err(Reject::NotFound);
            }
            id
        }
        None => path.strip_prefix('/').ok_or(Reject::NotFound)?,
    };
    if valid_id(id) {
        Ok(id)
    } else {
        Err(Reject::NotFound)
    }
}

fn ctx() -> &'static Arc<Ctx> {
    CTX.get_or_init(|| {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let port = l.local_addr().unwrap().port();
        let token = config::get().security.proxy_token.then(new_token);
        let ctx = Arc::new(Ctx { port, token, chunk: config::get().network.proxy_chunk_bytes.max(64 * 1024) });
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
        Ok(id) => id,
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
        while let Ok(Some(bytes)) = resp.chunk().await {
            pos += bytes.len() as u64;
            sock.write_all(&bytes).await?; // client closing (seek/skip) ends the loop
        }
        if pos > end {
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
        Ctx { port: 4242, token: token.map(str::to_owned), chunk: 1 << 20 }
    }

    #[test]
    fn authorize_checks_each_rule() {
        let c = ctx_for(Some("secret"));
        let h = Some("127.0.0.1:4242");
        assert_eq!(authorize(&c, "GET", "/t/secret/abc_-1", h), Ok("abc_-1"));
        assert_eq!(authorize(&c, "HEAD", "/t/secret/abc", Some("LOCALHOST:4242")), Ok("abc"));
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
        assert_eq!(authorize(&c, "GET", "/abc", h), Ok("abc"));
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
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let c = Arc::new(Ctx { port, token: token.map(str::to_owned), chunk: 1 << 20 });
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

    #[tokio::test]
    async fn legacy_mode_end_to_end() {
        let up = fake_upstream().await;
        URLS.lock().unwrap().get_or_insert_default().insert("w7legacy".into(), (format!("http://127.0.0.1:{up}/x"), "ua".into()));
        let port = start(None).await;
        let r = raw(port, &req("GET", "/w7legacy", &format!("127.0.0.1:{port}"))).await;
        assert!(r.starts_with("HTTP/1.1 200 OK") && r.ends_with("hello"), "{r}");
    }
}
