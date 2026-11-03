// SPDX-License-Identifier: GPL-3.0-only
//! Tiny localhost HTTP proxy for audio streams.
//!
//! Qt's ffmpeg backend isn't guaranteed to speak https (distro builds without TLS
//! exist), so the player gets `http://127.0.0.1:<port>/<videoId>` and this proxy
//! fetches the real googlevideo URL over https, forwarding Range requests so
//! seeking works.
use crate::config;
use crate::state::st;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static URLS: Mutex<Option<HashMap<String, (String, String)>>> = Mutex::new(None);
static PORT: OnceLock<u16> = OnceLock::new();

fn port() -> u16 {
    *PORT.get_or_init(|| {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        l.set_nonblocking(true).ok();
        let port = l.local_addr().unwrap().port();
        st().rt.spawn(async move {
            let l = TcpListener::from_std(l).expect("proxy listener");
            let client = reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(config::get().network.connect_timeout_secs))
                .read_timeout(std::time::Duration::from_secs(config::get().network.stream_read_timeout_secs))
                .build()
                .expect("http client");
            while let Ok((sock, _)) = l.accept().await {
                let client = client.clone();
                tokio::spawn(async move {
                    let _ = handle(sock, client).await;
                });
            }
        });
        port
    })
}

/// Register a real stream URL and get the local URL to hand to the media player.
pub fn local_url(id: &str, real: &str, ua: &str) -> String {
    URLS.lock().unwrap().get_or_insert_default().insert(id.to_owned(), (real.to_owned(), ua.to_owned()));
    format!("http://127.0.0.1:{}/{}", port(), id)
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

async fn handle(mut sock: TcpStream, client: reqwest::Client) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = sock.read(&mut chunk).await?;
        if n == 0 || buf.len() > 16 * 1024 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or("/"));
    let range = lines.filter_map(|l| l.split_once(':')).find(|(k, _)| k.eq_ignore_ascii_case("range")).map(|(_, v)| v.trim().to_owned());

    let id = path.trim_start_matches('/');
    let real = URLS.lock().unwrap().get_or_insert_default().get(id).cloned();
    let Some((real, ua)) = real else {
        return sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    };

    // googlevideo refuses open-ended / unbounded Range requests, so everything is
    // fetched in bounded chunks (like yt-dlp's http_chunk_size) and stitched together.
    let ch = config::get().network.proxy_chunk_bytes.max(64 * 1024);
    let (start, req_end) = parse_range(range.as_deref());
    let fetch = |a: u64, b: u64| client.get(&real).header("User-Agent", &ua).header("Range", format!("bytes={a}-{b}")).send();
    let mut resp = match fetch(start, req_end.map_or(start + ch - 1, |e| e.min(start + ch - 1))).await {
        Ok(r) => r,
        Err(_) => return sock.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await,
    };
    let status = resp.status();
    if status.as_u16() != 206 {
        let out = format!(
            "HTTP/1.1 {} {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        );
        return sock.write_all(out.as_bytes()).await;
    }
    let total: u64 = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('/').next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let end = req_end.unwrap_or(total.saturating_sub(1)).min(total.saturating_sub(1));
    let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_owned();
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
        resp = match fetch(pos, end.min(pos + ch - 1)).await {
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
}
