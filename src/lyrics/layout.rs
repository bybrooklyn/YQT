// SPDX-License-Identifier: GPL-3.0-only
//! Synced lines → what the lyric player shows: sung lines with the time they end (for the karaoke fill)
//! and "gap" entries (animated dots) for long instrumental stretches. Pure.
use super::parse::Line;

/// Pauses at least this long get dots.
pub const GAP_MIN_MS: i64 = 4000;
/// How long the last line is assumed to last.
const LAST_LINE_MS: i64 = 5000;

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub start: i64,
    /// When the line stops being sung (karaoke fill ends) / the gap ends.
    pub end: i64,
    pub text: String,
    pub gap: bool,
}

/// How long a line of this length takes to sing, roughly: used to find the pause *after* a line when the
/// file has no empty marker line.
pub fn sung_ms(text: &str) -> i64 {
    (text.chars().count() as i64 * 85 + 1000).clamp(1500, 7000)
}

/// Build the display list. `track_ms` (0 = unknown) bounds the last line.
pub fn items(lines: &[Line], track_ms: i64) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    let gap = |start: i64, end: i64| Item { start, end, text: String::new(), gap: true };
    if let Some((first, _)) = lines.iter().find(|(_, t)| !t.trim().is_empty()) {
        if *first >= GAP_MIN_MS {
            out.push(gap(0, *first)); // intro
        }
    }
    for (i, (start, text)) in lines.iter().enumerate() {
        let end = match lines.get(i + 1) {
            Some((n, _)) => *n,
            None if track_ms > *start => track_ms,
            None => *start + LAST_LINE_MS,
        };
        if text.trim().is_empty() {
            // an explicit pause marker: dots when it is long enough, otherwise nothing
            if end - start >= GAP_MIN_MS && !matches!(out.last(), Some(g) if g.gap && g.end > *start) && i + 1 < lines.len() {
                out.push(gap(*start, end));
            }
            continue;
        }
        let sung = sung_ms(text);
        if end - start - sung >= GAP_MIN_MS && i + 1 < lines.len() {
            out.push(Item { start: *start, end: start + sung, text: text.clone(), gap: false });
            out.push(gap(start + sung, end));
        } else {
            out.push(Item { start: *start, end, text: text.clone(), gap: false });
        }
    }
    out
}

/// The `[[ms, text, end, gap], …]` JSON the QML panel reads. The first two entries are the plain
/// `[ms, text]` pair, so a consumer that only cares about timing can ignore the rest.
pub fn to_json(items: &[Item]) -> String {
    let v: Vec<serde_json::Value> = items.iter().map(|i| serde_json::json!([i.start, i.text, i.end, i.gap])).collect();
    serde_json::Value::Array(v).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::parse::parse_lrc;

    fn it(start: i64, end: i64, text: &str, gap: bool) -> Item {
        Item { start, end, text: text.into(), gap }
    }

    #[test]
    fn plain_run_of_lines_ends_where_the_next_begins() {
        let l = parse_lrc("[00:01.00]one\n[00:03.00]two\n[00:05.00]three");
        // intro is only 1 s: no dots; the last line gets the default length
        assert_eq!(items(&l, 0), [it(1000, 3000, "one", false), it(3000, 5000, "two", false), it(5000, 10_000, "three", false)]);
        // a known track length bounds the last line
        assert_eq!(items(&l, 8000).last().unwrap().end, 8000);
    }

    #[test]
    fn long_intro_and_explicit_pause_markers_get_dots() {
        let l = parse_lrc("[00:10.00]one\n[00:12.00]two\n[00:14.00]\n[00:30.00]three\n[00:32.00]four\n[00:34.00]");
        let got = items(&l, 0);
        assert_eq!(got[0], it(0, 10_000, "", true));
        assert_eq!(got[1], it(10_000, 12_000, "one", false));
        assert_eq!(got[3], it(14_000, 30_000, "", true));
        // the trailing empty marker (end of song) is not a gap
        assert!(!got.last().unwrap().gap);
        assert_eq!(got.iter().filter(|i| i.gap).count(), 2);
    }

    #[test]
    fn short_pauses_are_dropped() {
        let l = parse_lrc("[00:01.00]a\n[00:03.00]\n[00:05.00]b\n[00:07.00]c");
        assert!(items(&l, 0).iter().all(|i| !i.gap));
        assert_eq!(items(&l, 0).len(), 3);
    }

    #[test]
    fn silent_stretch_after_a_line_without_a_marker_becomes_a_gap() {
        let l = parse_lrc("[00:01.00]short\n[00:30.00]after a long silence");
        let got = items(&l, 0);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].end, 1000 + sung_ms("short"));
        assert!(got[1].gap);
        assert_eq!((got[1].start, got[1].end), (got[0].end, 30_000));
        assert_eq!(got[2].text, "after a long silence");
    }

    #[test]
    fn items_are_contiguous_in_time_and_json_shape() {
        let l = parse_lrc("[00:05.00]a\n[00:20.00]b\n[00:22.00]");
        let got = items(&l, 0);
        for w in got.windows(2) {
            assert!(w[0].start <= w[1].start);
        }
        let j: serde_json::Value = serde_json::from_str(&to_json(&got)).unwrap();
        assert_eq!(j[1][0], 5000);
        assert_eq!(j[1][1], "a");
        assert_eq!(j[0][3], true);
        assert!(items(&[], 0).is_empty());
    }
}
