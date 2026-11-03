// SPDX-License-Identifier: GPL-3.0-only
//! LRCLIB (https://lrclib.net): community lyrics with timestamps. Gets the title, artist and duration, nothing else.
use super::parse::parse_lrc;
use super::rank::{Candidate, Query};
use super::{checked_base, get_json, Provider, ProviderFuture};
use crate::errors::AppError;
use serde_json::Value;

pub const NAME: &str = "LRCLIB";

pub struct Lrclib {
    client: reqwest::Client,
    base: String,
}

impl Lrclib {
    pub fn new(client: reqwest::Client, base_url: &str, allow_insecure: bool) -> Result<Self, AppError> {
        Ok(Self { client, base: checked_base(base_url, allow_insecure)? })
    }

    async fn run(&self, q: &Query) -> Result<Vec<Candidate>, AppError> {
        let mut params = vec![("track_name", q.title.clone()), ("artist_name", q.artist.clone())];
        if q.duration_s > 0 {
            params.push(("duration", q.duration_s.to_string()));
        }
        // Exact match first: it is the most reliable record when it exists.
        let exact = get_json(&self.client, NAME, &format!("{}/api/get", self.base), &params).await;
        if let Ok(Some(v)) = &exact {
            let c = candidates_from(v);
            if !c.is_empty() {
                return Ok(c);
            }
        }
        // Missing or junk: search and let the ranking choose among the answers.
        let search = [("track_name", q.title.clone()), ("artist_name", q.artist.clone())];
        match get_json(&self.client, NAME, &format!("{}/api/search", self.base), &search).await? {
            Some(v) => Ok(candidates_from(&v)),
            None => Ok(Vec::new()),
        }
    }
}

impl Provider for Lrclib {
    fn name(&self) -> &'static str {
        NAME
    }
    fn fetch<'a>(&'a self, q: &'a Query) -> ProviderFuture<'a> {
        Box::pin(self.run(q))
    }
}

/// LRCLIB answers `/api/get` with one object and `/api/search` with an array of them.
pub fn candidates_from(v: &Value) -> Vec<Candidate> {
    match v {
        Value::Array(a) => a.iter().filter_map(candidate_from).collect(),
        Value::Object(_) => candidate_from(v).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn candidate_from(v: &Value) -> Option<Candidate> {
    if v["instrumental"].as_bool() == Some(true) {
        return None;
    }
    let synced = v["syncedLyrics"].as_str().map(parse_lrc).unwrap_or_default();
    let plain = v["plainLyrics"].as_str().unwrap_or("").to_owned();
    if synced.is_empty() && plain.trim().is_empty() {
        return None;
    }
    Some(Candidate {
        provider: NAME,
        title: v["trackName"].as_str().or_else(|| v["name"].as_str()).unwrap_or("").to_owned(),
        artist: v["artistName"].as_str().unwrap_or("").to_owned(),
        duration_s: v["duration"].as_f64(),
        synced,
        plain,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::rank::pick;
    use crate::lyrics::rank::Pick;
    use serde_json::json;

    fn lrc(n: usize) -> String {
        (0..n).map(|i| format!("[00:{:02}.00]line {i}\n", i * 5)).collect()
    }

    #[test]
    fn get_record_with_synced_lyrics() {
        let v = json!({"trackName":"Song Name","artistName":"The Band","duration":200.4,"syncedLyrics":lrc(8),"plainLyrics":"a\nb"});
        let c = candidates_from(&v);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].synced.len(), 8);
        assert_eq!(c[0].duration_s, Some(200.4));
        assert_eq!(c[0].provider, "LRCLIB");
    }

    #[test]
    fn search_array_instrumental_and_empty_records() {
        let v = json!([
            {"trackName":"A","artistName":"B","instrumental":true,"plainLyrics":"x\ny\nz\nw"},
            {"trackName":"A","artistName":"B","syncedLyrics":null,"plainLyrics":null},
            {"trackName":"A","artistName":"B","syncedLyrics":lrc(5)},
        ]);
        let c = candidates_from(&v);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].synced.len(), 5);
        assert!(candidates_from(&json!("nope")).is_empty());
        assert!(candidates_from(&json!({"code":404,"name":"TrackNotFound"})).is_empty());
    }

    #[test]
    fn the_probe_record_is_rejected_by_ranking() {
        let probe = json!({"trackName":"Song Name","artistName":"The Band","duration":200.0,"syncedLyrics":"[00:00.00]probe","plainLyrics":"probe"});
        let good = json!({"trackName":"Song Name","artistName":"The Band","duration":199.0,"syncedLyrics":lrc(9)});
        let mut cands = candidates_from(&probe);
        cands.extend(candidates_from(&good));
        let q = Query { video_id: "v".into(), title: "Song Name".into(), artist: "The Band".into(), duration_s: 200 };
        match pick(cands, &q, 4, true) {
            Some(Pick::Synced { lines, .. }) => assert_eq!(lines.len(), 9),
            other => panic!("{other:?}"),
        }
        assert!(pick(candidates_from(&probe), &q, 4, true).is_none());
    }

    #[test]
    fn rejects_insecure_base_url() {
        assert!(Lrclib::new(reqwest::Client::new(), "http://lrclib.example", false).is_err());
        assert!(Lrclib::new(reqwest::Client::new(), "https://lrclib.net/", false).is_ok());
    }

    #[tokio::test]
    async fn get_then_search_over_a_real_socket() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let search_body = json!([{"trackName":"Song Name","artistName":"The Band","duration":200.0,"syncedLyrics":lrc(7)}]).to_string();
        tokio::spawn(async move {
            // /api/get → 404, /api/search → one record
            for _ in 0..2 {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let resp = if req.starts_with("GET /api/get?") {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_owned()
                } else {
                    assert!(req.starts_with("GET /api/search?"), "{req}");
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{search_body}", search_body.len())
                };
                s.write_all(resp.as_bytes()).await.unwrap();
                s.shutdown().await.unwrap();
            }
        });
        let p = Lrclib::new(reqwest::Client::new(), &format!("http://{addr}"), false).unwrap();
        let q = Query { video_id: "v".into(), title: "Song Name".into(), artist: "The Band".into(), duration_s: 200 };
        let c = p.fetch(&q).await.unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].synced.len(), 7);
    }
}
