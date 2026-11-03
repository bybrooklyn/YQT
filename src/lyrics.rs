// SPDX-License-Identifier: GPL-3.0-only
//! Time-synced lyrics from LRCLIB (https://lrclib.net), with YouTube Music's plain
//! lyrics as the fallback. LRCLIB gets the track title, artist and duration – nothing else.
use crate::state::http;
use serde_json::Value;

/// Parse LRC text (`[mm:ss.xx] line`, several stamps per line allowed) into sorted (ms, text).
pub fn parse_lrc(lrc: &str) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    for line in lrc.lines() {
        let mut rest = line.trim();
        let mut stamps = Vec::new();
        while let Some(r) = rest.strip_prefix('[') {
            let Some((tag, after)) = r.split_once(']') else { break };
            match parse_stamp(tag) {
                Some(ms) => stamps.push(ms),
                None => {
                    stamps.clear(); // metadata tag like [ar:Artist]
                    rest = "";
                    break;
                }
            }
            rest = after;
        }
        let text = rest.trim();
        for ms in stamps {
            out.push((ms, text.to_owned()));
        }
    }
    out.sort_by_key(|(ms, _)| *ms);
    out
}

fn parse_stamp(tag: &str) -> Option<i64> {
    let (m, s) = tag.split_once(':')?;
    let m: i64 = m.trim().parse().ok()?;
    let s: f64 = s.trim().parse().ok()?;
    Some(m * 60_000 + (s * 1000.0).round() as i64)
}

/// LRCLIB is user-submitted: some records are junk (e.g. a single "probe" line), so
/// only accept entries with a believable amount of text.
fn usable(v: &Value, min_lines: usize) -> Option<(Vec<(i64, String)>, String)> {
    let synced = v["syncedLyrics"].as_str().map(parse_lrc).unwrap_or_default();
    let plain = v["plainLyrics"].as_str().unwrap_or("").to_owned();
    let synced_ok = synced.iter().filter(|(_, l)| !l.is_empty()).count() >= min_lines;
    let plain_ok = plain.lines().filter(|l| !l.trim().is_empty()).count() >= min_lines;
    if !synced_ok && !plain_ok {
        return None;
    }
    Some((if synced_ok { synced } else { Vec::new() }, plain))
}

/// Returns (synced lines, plain text) from LRCLIB, if it knows the track.
pub async fn lrclib(title: &str, artist: &str, duration_s: u32) -> Option<(Vec<(i64, String)>, String)> {
    let cfg = &crate::config::get().lyrics;
    if !cfg.enabled {
        return None;
    }
    let mut q = vec![("track_name", title.to_owned()), ("artist_name", artist.to_owned())];
    if duration_s > 0 {
        q.push(("duration", duration_s.to_string()));
    }
    if let Ok(r) = http().get(format!("{}/api/get", cfg.lrclib_url.trim_end_matches('/'))).query(&q).send().await {
        if r.status().is_success() {
            if let Some(found) = r.json::<Value>().await.ok().as_ref().and_then(|v| usable(v, cfg.min_lines)) {
                return Some(found);
            }
        }
    }
    // Exact match missing or junk: search, preferring synced entries whose length is closest to ours.
    let r = http()
        .get(format!("{}/api/search", cfg.lrclib_url.trim_end_matches('/')))
        .query(&[("track_name", title), ("artist_name", artist)])
        .send()
        .await
        .ok()?;
    let list: Value = r.json().await.ok()?;
    list.as_array()?
        .iter()
        .filter_map(|v| {
            let found = usable(v, cfg.min_lines)?;
            let diff = (v["duration"].as_f64().unwrap_or(0.0) - duration_s as f64).abs() as i64;
            Some(((found.0.is_empty(), diff), found))
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, found)| found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stamps_and_skips_metadata() {
        let l = parse_lrc("[ar:Someone]\n[00:01.50] hello\n[00:03.00][01:00.25]chorus\n[00:05.00]\n");
        assert_eq!(l[0], (1500, "hello".into()));
        assert_eq!(l[1], (3000, "chorus".into()));
        assert!(l.contains(&(60250, "chorus".into())));
        assert!(l.contains(&(5000, "".into())));
        assert_eq!(l.len(), 4);
    }

    #[test]
    fn sorted_output() {
        let l = parse_lrc("[00:10.00]b\n[00:02.00]a");
        assert_eq!(l[0].1, "a");
    }

    #[test]
    fn junk_records_are_rejected() {
        let junk = serde_json::json!({"syncedLyrics": "[00:00.00]probe", "plainLyrics": "probe"});
        assert!(usable(&junk, 4).is_none());
        let lrc: String = (0..6).map(|i| format!("[00:{:02}.00]line {i}\n", i * 5)).collect();
        let good = serde_json::json!({"syncedLyrics": lrc, "plainLyrics": ""});
        assert_eq!(usable(&good, 4).unwrap().0.len(), 6);
        let plain_only = serde_json::json!({"plainLyrics": "a\nb\nc\nd\ne"});
        let (s, p) = usable(&plain_only, 4).unwrap();
        assert!(s.is_empty() && p.starts_with('a'));
    }

    #[test]
    fn garbage_is_ignored() {
        assert!(parse_lrc("just text\nno stamps").is_empty());
    }
}
