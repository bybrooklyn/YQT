// SPDX-License-Identifier: GPL-3.0-only
//! Choosing the best lyrics out of everything the providers returned. Pure.
//!
//! Order of preference: synced ≫ plain (regardless of length) → a length within a few seconds of the track's
//! (so an extended mix doesn't win by sheer line count) → more non-empty lines → duration closest to the
//! track's → title/artist similarity → provider preference order. Junk (too few lines) and records that
//! are plainly another song are dropped first.
use super::parse::{non_empty, non_empty_plain, Line};
use std::cmp::Reverse;

/// What we are looking lyrics up for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    /// 0 = unknown.
    pub duration_s: u32,
}

/// One record offered by a provider.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub provider: &'static str,
    /// Position of the provider in `config.lyrics.providers` (0 = most preferred).
    pub provider_rank: usize,
    pub title: String,
    pub artist: String,
    pub duration_s: Option<f64>,
    pub synced: Vec<Line>,
    pub plain: String,
}

/// The chosen lyrics.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    Synced { lines: Vec<Line>, source: &'static str },
    Plain { text: String, source: &'static str },
}

/// Candidates whose title+artist similarity (permille) is below this are a different song.
const MIN_SIMILARITY: u32 = 400;
/// Records whose length differs from the track's by more than this are probably another version.
const SAME_VERSION_S: u32 = 10;
/// Duration difference assumed when either side doesn't know its length.
const UNKNOWN_DIFF_S: u32 = 4;

/// Lower-case, drop `(…)`/`[…]` groups and `feat.` tails, turn punctuation into spaces.
pub fn normalize(s: &str) -> String {
    let mut depth = 0u32;
    let mut cleaned = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '(' | '[' | '（' | '【' => depth += 1,
            ')' | ']' | '）' | '】' => depth = depth.saturating_sub(1),
            _ if depth == 0 => cleaned.push(c),
            _ => {}
        }
    }
    let lower = cleaned.to_lowercase();
    let mut words: Vec<String> = Vec::new();
    for w in lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        // "feat. X", "ft X", "featuring X": everything after is a guest, not part of the name
        if matches!(w, "feat" | "ft" | "featuring") && !words.is_empty() {
            break;
        }
        words.push(w.to_owned());
    }
    words.join(" ")
}

/// Similarity of two names in permille (1000 = same after normalising).
pub fn similarity(a: &str, b: &str) -> u32 {
    let (a, b) = (normalize(a), normalize(b));
    if a.is_empty() || b.is_empty() {
        return 500; // unknown on one side: neutral
    }
    if a == b {
        return 1000;
    }
    let ta: Vec<&str> = a.split(' ').collect();
    let tb: Vec<&str> = b.split(' ').collect();
    let common = ta.iter().filter(|w| tb.contains(w)).count();
    // Dice coefficient on word sets; a name contained in the other ("Song" in "Song Remastered") still scores well.
    let dice = 2000 * common / (ta.len() + tb.len());
    let contained = common == ta.len().min(tb.len());
    (dice as u32).max(if contained { 800 } else { 0 })
}

fn track_similarity(c: &Candidate, q: &Query) -> u32 {
    let t = similarity(&c.title, &q.title);
    // artists come as "A, B" / "A & B" / "A feat. B": compare as word sets against the first artist too
    let a = similarity(&c.artist, &q.artist);
    (t * 2 + a) / 3
}

fn duration_diff_s(c: &Candidate, q: &Query) -> u32 {
    match (c.duration_s, q.duration_s) {
        (Some(d), s) if s > 0 && d > 0.0 => (d - f64::from(s)).abs().round() as u32,
        _ => UNKNOWN_DIFF_S,
    }
}

/// Pick the best usable candidate, or `None` when there is nothing believable.
pub fn pick(cands: Vec<Candidate>, q: &Query, min_lines: usize, prefer_synced: bool) -> Option<Pick> {
    struct Scored {
        // synced, plausible length, line count, closeness of length, title/artist match, provider order
        key: (bool, bool, usize, Reverse<u32>, u32, Reverse<usize>),
        pick: Pick,
    }
    let mut best: Option<Scored> = None;
    for c in cands {
        let sim = track_similarity(&c, q);
        if sim < MIN_SIMILARITY {
            continue;
        }
        let synced_lines = non_empty(&c.synced);
        let plain_lines = non_empty_plain(&c.plain);
        // Synced is used when believable; otherwise fall back to this record's plain text.
        let (is_synced, lines) = if synced_lines >= min_lines.max(1) {
            (true, synced_lines)
        } else if plain_lines >= min_lines.max(1) {
            (false, plain_lines)
        } else {
            continue; // junk, e.g. a lone "probe" line
        };
        let diff = duration_diff_s(&c, q);
        // A record for a clearly different length (extended mix, live version) must not win just by having more lines.
        let key = (is_synced && prefer_synced, diff <= SAME_VERSION_S, lines, Reverse(diff), sim, Reverse(c.provider_rank));
        let pick = if is_synced {
            Pick::Synced { lines: c.synced, source: c.provider }
        } else {
            Pick::Plain { text: c.plain, source: c.provider }
        };
        if best.as_ref().is_none_or(|b| key > b.key) {
            best = Some(Scored { key, pick });
        }
    }
    best.map(|b| b.pick)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::parse::parse_lrc;

    fn lrc(n: usize) -> Vec<Line> {
        parse_lrc(&(0..n).map(|i| format!("[{:02}:{:02}.00]line {i}\n", i / 6, (i % 6) * 10)).collect::<String>())
    }
    fn plain(n: usize) -> String {
        (0..n).map(|i| format!("line {i}\n")).collect()
    }
    fn q() -> Query {
        Query { video_id: "v".into(), title: "Song Name".into(), artist: "The Band".into(), duration_s: 200 }
    }
    fn cand(provider: &'static str, rank: usize) -> Candidate {
        Candidate {
            provider,
            provider_rank: rank,
            title: "Song Name".into(),
            artist: "The Band".into(),
            duration_s: Some(200.0),
            ..Default::default()
        }
    }
    fn winner(p: Option<Pick>) -> (&'static str, bool) {
        match p.expect("a pick") {
            Pick::Synced { source, .. } => (source, true),
            Pick::Plain { source, .. } => (source, false),
        }
    }

    #[test]
    fn normalisation() {
        assert_eq!(normalize("Song Name (Remastered 2011) [Live]"), "song name");
        assert_eq!(normalize("Hello, World!"), "hello world");
        assert_eq!(normalize("Track feat. Someone Else"), "track");
        assert_eq!(normalize("Track ft Someone"), "track");
        assert_eq!(normalize("feat only"), "feat only"); // a leading "feat" is a real word
        assert_eq!(normalize("  "), "");
        assert_eq!(similarity("Song Name (Live)", "song-name"), 1000);
        assert!(similarity("Totally Different", "Song Name") < MIN_SIMILARITY);
        assert!(similarity("Song Name Remastered", "Song Name") >= 800);
        assert_eq!(similarity("", "x"), 500);
    }

    #[test]
    fn synced_beats_longer_plain() {
        let mut a = cand("yt", 2);
        a.plain = plain(80);
        let mut b = cand("lrclib", 0);
        b.synced = lrc(10);
        assert_eq!(winner(pick(vec![a.clone(), b.clone()], &q(), 4, true)), ("lrclib", true));
        // with the preference off, the longer text wins
        assert_eq!(winner(pick(vec![a, b], &q(), 4, false)), ("yt", false));
    }

    #[test]
    fn more_lines_then_duration_then_fuzzy_then_provider() {
        let mut a = cand("a", 0);
        a.synced = lrc(20);
        let mut b = cand("b", 1);
        b.synced = lrc(30);
        assert_eq!(winner(pick(vec![a.clone(), b.clone()], &q(), 4, true)).0, "b");

        // same line count: closest duration
        b.synced = lrc(20);
        a.duration_s = Some(215.0);
        b.duration_s = Some(201.0);
        assert_eq!(winner(pick(vec![a.clone(), b.clone()], &q(), 4, true)).0, "b");

        // same duration: better title match
        a.duration_s = Some(200.0);
        b.duration_s = Some(200.0);
        b.title = "Song Name Extra Words".into();
        assert_eq!(winner(pick(vec![b.clone(), a.clone()], &q(), 4, true)).0, "a");

        // all equal: provider order decides, whatever the input order
        let mut c = cand("c", 0);
        c.synced = lrc(20);
        let mut d = cand("d", 1);
        d.synced = lrc(20);
        assert_eq!(winner(pick(vec![d.clone(), c.clone()], &q(), 4, true)).0, "c");
        assert_eq!(winner(pick(vec![c, d], &q(), 4, true)).0, "c");
    }

    #[test]
    fn a_longer_record_for_another_version_does_not_win_on_line_count() {
        let mut extended = cand("netease", 1);
        extended.synced = lrc(40);
        extended.duration_s = Some(290.0); // query is 200 s
        let mut exact = cand("lrclib", 0);
        exact.synced = lrc(25);
        exact.duration_s = Some(201.0);
        assert_eq!(winner(pick(vec![extended.clone(), exact], &q(), 4, true)).0, "lrclib");
        // but it is still better than nothing
        assert_eq!(winner(pick(vec![extended], &q(), 4, true)).0, "netease");
    }

    #[test]
    fn unknown_duration_is_neutral_not_fatal() {
        let mut a = cand("a", 0);
        a.duration_s = None;
        a.synced = lrc(8);
        assert_eq!(winner(pick(vec![a], &Query { duration_s: 0, ..q() }, 4, true)).0, "a");
    }

    #[test]
    fn junk_and_other_songs_rejected() {
        let mut probe = cand("lrclib", 0);
        probe.synced = parse_lrc("[00:00.00]probe");
        probe.plain = "probe".into();
        assert!(pick(vec![probe.clone()], &q(), 4, true).is_none());

        let mut other = cand("netease", 1);
        other.title = "Completely Unrelated".into();
        other.artist = "Nobody".into();
        other.synced = lrc(40);
        assert!(pick(vec![other.clone()], &q(), 4, true).is_none());

        let mut ok = cand("youtube", 2);
        ok.plain = plain(12);
        // the junk and the wrong song never beat the real thing, even though one is synced and long
        assert_eq!(winner(pick(vec![probe, other, ok], &q(), 4, true)), ("youtube", false));
    }

    #[test]
    fn junk_synced_falls_back_to_the_same_records_plain_text() {
        let mut a = cand("lrclib", 0);
        a.synced = parse_lrc("[00:00.00]probe");
        a.plain = plain(9);
        assert_eq!(winner(pick(vec![a], &q(), 4, true)), ("lrclib", false));
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(pick(Vec::new(), &q(), 4, true).is_none());
    }
}
