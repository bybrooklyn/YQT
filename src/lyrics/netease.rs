// SPDX-License-Identifier: GPL-3.0-only
//! NetEase Cloud Music (unofficial public web API, no key). Search for the song, then fetch its LRC.
//! Gets the title and artist as the search text, nothing else.
use super::parse::{drop_credit_lines, parse_lrc};
use super::rank::{Candidate, Query};
use super::{checked_base, get_json, Provider, ProviderFuture};
use crate::errors::{AppError, ErrorKind};
use serde_json::Value;

pub const NAME: &str = "NetEase";
/// How many search hits get their lyrics fetched (the ranking then picks among them).
const TOP_HITS: usize = 3;

pub struct NetEase {
    client: reqwest::Client,
    base: String,
}

/// A search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Song {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub duration_s: Option<f64>,
}

impl NetEase {
    pub fn new(client: reqwest::Client, base_url: &str, allow_insecure: bool) -> Result<Self, AppError> {
        Ok(Self { client, base: checked_base(base_url, allow_insecure)? })
    }

    async fn run(&self, q: &Query) -> Result<Vec<Candidate>, AppError> {
        let text = format!("{} {}", q.title, q.artist);
        let search = [("s", text.trim().to_owned()), ("type", "1".into()), ("limit", "5".into())];
        let Some(v) = get_json(&self.client, NAME, &format!("{}/api/search/get", self.base), &search).await? else {
            return Ok(Vec::new());
        };
        let songs: Vec<Song> = songs_from(&v).into_iter().take(TOP_HITS).collect();
        let url = format!("{}/api/song/lyric", self.base);
        // the lyric requests run side by side
        let tasks: Vec<_> = songs
            .iter()
            .map(|s| {
                let params = [("id", s.id.to_string()), ("lv", "1".into()), ("kv", "1".into()), ("tv", "-1".into())];
                let (client, url) = (self.client.clone(), url.clone());
                tokio::spawn(async move { get_json(&client, NAME, &url, &params).await })
            })
            .collect();
        let mut answers = Vec::new();
        for t in tasks {
            answers.push(
                t.await.unwrap_or_else(|e| Err(AppError::new(ErrorKind::Internal, "NetEase lookup failed unexpectedly.", e.to_string()))),
            );
        }
        let mut out = Vec::new();
        let mut last_err = None;
        let mut any_ok = false;
        for (song, ans) in songs.iter().zip(answers) {
            match ans {
                Ok(Some(v)) => {
                    any_ok = true;
                    out.extend(candidate_from(song, &v));
                }
                Ok(None) => any_ok = true,
                Err(e) => last_err = Some(e),
            }
        }
        match (out.is_empty(), any_ok, last_err) {
            (true, false, Some(e)) => Err(e),
            _ => Ok(out),
        }
    }
}

impl Provider for NetEase {
    fn name(&self) -> &'static str {
        NAME
    }
    fn fetch<'a>(&'a self, q: &'a Query) -> ProviderFuture<'a> {
        Box::pin(self.run(q))
    }
}

/// `{"result":{"songs":[{"id":1,"name":"…","artists":[{"name":"…"}],"duration":213000}]},"code":200}`
pub fn songs_from(v: &Value) -> Vec<Song> {
    let Some(songs) = v["result"]["songs"].as_array() else { return Vec::new() };
    songs
        .iter()
        .filter_map(|s| {
            let id = s["id"].as_i64()?;
            let artist = s["artists"]
                .as_array()
                .or_else(|| s["ar"].as_array())
                .map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            let ms = s["duration"].as_f64().or_else(|| s["dt"].as_f64()).filter(|d| *d > 0.0);
            Some(Song { id, title: s["name"].as_str().unwrap_or("").to_owned(), artist, duration_s: ms.map(|d| d / 1000.0) })
        })
        .collect()
}

/// `{"lrc":{"lyric":"[00:01.00]…"},"tlyric":{"lyric":""},"nolyric":false}`. The translation (`tlyric`) is ignored:
/// mixing it in would double every line.
pub fn candidate_from(song: &Song, v: &Value) -> Option<Candidate> {
    if v["nolyric"].as_bool() == Some(true) || v["uncollected"].as_bool() == Some(true) {
        return None;
    }
    let text = v["lrc"]["lyric"].as_str()?;
    let synced = drop_credit_lines(parse_lrc(text));
    // Some records are plain text without stamps.
    let plain = if synced.is_empty() {
        text.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
    } else {
        String::new()
    };
    if synced.is_empty() && plain.is_empty() {
        return None;
    }
    Some(Candidate {
        provider: NAME,
        title: song.title.clone(),
        artist: song.artist.clone(),
        duration_s: song.duration_s,
        synced,
        plain,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn search_fixture() -> Value {
        json!({"result":{"songs":[
            {"id": 111, "name": "Song Name", "artists": [{"name": "The Band"}, {"name": "Guest"}], "duration": 200500},
            {"id": 222, "name": "Other", "artists": [], "duration": 0},
            {"name": "no id"}
        ],"songCount":3},"code":200})
    }

    #[test]
    fn search_hits() {
        let s = songs_from(&search_fixture());
        assert_eq!(s.len(), 2);
        assert_eq!(s[0], Song { id: 111, title: "Song Name".into(), artist: "The Band, Guest".into(), duration_s: Some(200.5) });
        assert_eq!(s[1].duration_s, None);
        assert!(songs_from(&json!({"result":{},"code":200})).is_empty());
        assert!(songs_from(&json!({"code":-460})).is_empty());
    }

    #[test]
    fn lyric_with_credits_and_translation() {
        let song = songs_from(&search_fixture()).remove(0);
        let v = json!({
            "lrc": {"version": 3, "lyric": "[00:00.00] 作词 : A\n[00:01.00] 作曲 : B\n[00:12.50]first line\n[00:15.00]second line\n[00:18.00]third\n[00:21.00]fourth\n"},
            "tlyric": {"version": 1, "lyric": "[00:12.50]译文一\n[00:15.00]译文二\n"},
            "code": 200
        });
        let c = candidate_from(&song, &v).unwrap();
        let texts: Vec<&str> = c.synced.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, ["first line", "second line", "third", "fourth"]);
        assert_eq!(c.synced[0].0, 12_500);
        assert_eq!(c.duration_s, Some(200.5));
        assert_eq!(c.artist, "The Band, Guest");
    }

    #[test]
    fn missing_lrc_nolyric_and_uncollected() {
        let song = Song { id: 1, title: "t".into(), artist: "a".into(), duration_s: None };
        assert!(candidate_from(&song, &json!({"code": 200})).is_none());
        assert!(candidate_from(&song, &json!({"nolyric": true, "code": 200})).is_none());
        assert!(candidate_from(&song, &json!({"uncollected": true, "lrc": {"lyric": "[00:01.00]x"}})).is_none());
        assert!(candidate_from(&song, &json!({"lrc": {"lyric": ""}})).is_none());
        assert!(candidate_from(&song, &json!({"lrc": {"lyric": "[00:00.00] 纯音乐，请欣赏"}})).is_some());
        // short record: the ranking rejects it as junk
    }

    #[test]
    fn unstamped_text_becomes_plain() {
        let song = Song { id: 1, title: "t".into(), artist: "a".into(), duration_s: None };
        let c = candidate_from(&song, &json!({"lrc": {"lyric": "one\n\ntwo\nthree"}})).unwrap();
        assert!(c.synced.is_empty());
        assert_eq!(c.plain, "one\ntwo\nthree");
    }

    #[test]
    fn rejects_insecure_base_url() {
        assert!(NetEase::new(reqwest::Client::new(), "http://music.example", false).is_err());
        assert!(NetEase::new(reqwest::Client::new(), "https://music.163.com", false).is_ok());
    }

    #[tokio::test]
    async fn search_then_lyric_over_a_real_socket() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let lrc: String = (0..6).map(|i| format!("[00:{:02}.00]line {i}\n", i * 4)).collect();
        let search = json!({"result":{"songs":[{"id": 111, "name": "Song Name", "artists": [{"name": "The Band"}], "duration": 200000}]},"code":200}).to_string();
        let lyric = json!({"lrc":{"lyric": lrc},"tlyric":{"lyric":""},"code":200}).to_string();
        tokio::spawn(async move {
            for _ in 0..2 {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = if req.starts_with("GET /api/search/get?") {
                    assert!(
                        req.contains("type=1") && req.contains("limit=5") && (req.contains("Song+Name") || req.contains("Song%20Name")),
                        "{req}"
                    );
                    &search
                } else {
                    assert!(req.starts_with("GET /api/song/lyric?") && req.contains("id=111") && req.contains("lv=1"), "{req}");
                    &lyric
                };
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                s.write_all(resp.as_bytes()).await.unwrap();
                s.shutdown().await.unwrap();
            }
        });
        let p = NetEase::new(reqwest::Client::new(), &format!("http://{addr}"), false).unwrap();
        let q = Query { video_id: "v".into(), title: "Song Name".into(), artist: "The Band".into(), duration_s: 200 };
        let c = p.fetch(&q).await.unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].synced.len(), 6);
    }
}
