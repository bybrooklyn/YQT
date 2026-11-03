// SPDX-License-Identifier: GPL-3.0-only
//! LRC text → timed lines. Pure, no I/O.
//!
//! Handles `[mm:ss.xx]` (also `[m:ss]`, `[mm:ss.xxx]`, `[mm:ss:xx]`), several stamps per line, enhanced LRC
//! (`<mm:ss.xx>` word stamps are dropped, the words kept), `[offset:±ms]`, CRLF, a byte-order mark and
//! metadata tags (`[ar:…]`, `[ti:…]`, …).

/// One timed line: (milliseconds, text). An empty text marks a pause.
pub type Line = (i64, String);

/// Parse LRC text into lines sorted by time.
pub fn parse_lrc(lrc: &str) -> Vec<Line> {
    let lrc = lrc.trim_start_matches('\u{feff}');
    let mut offset_ms: i64 = 0;
    let mut out: Vec<Line> = Vec::new();
    for raw in lrc.lines() {
        let mut rest = raw.trim();
        let mut stamps: Vec<i64> = Vec::new();
        while let Some(r) = rest.strip_prefix('[') {
            let Some((tag, after)) = r.split_once(']') else { break };
            if let Some(ms) = parse_stamp(tag) {
                stamps.push(ms);
            } else {
                if let Some(v) = tag.trim().strip_prefix("offset:") {
                    offset_ms = v.trim().trim_start_matches('+').parse().unwrap_or(0);
                }
                // metadata tag such as [ar:Artist]: the whole line is metadata
                stamps.clear();
                break;
            }
            rest = after;
        }
        if stamps.is_empty() {
            continue;
        }
        let text = strip_word_stamps(rest);
        for ms in stamps {
            out.push((ms, text.clone()));
        }
    }
    if offset_ms != 0 {
        // A positive offset makes the lyrics appear sooner (LRC convention).
        for (ms, _) in &mut out {
            *ms = (*ms - offset_ms).max(0);
        }
    }
    out.sort_by_key(|(ms, _)| *ms); // stable: equal stamps keep file order
    out
}

fn parse_stamp(tag: &str) -> Option<i64> {
    let (m, s) = tag.split_once(':')?;
    let m = m.trim();
    if m.is_empty() || !m.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let m: i64 = m.parse().ok()?;
    // `ss.xx`, or the rarer `ss:xx` for hundredths
    let s = s.trim().replacen(':', ".", 1);
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let s: f64 = s.parse().ok()?;
    Some(m.checked_mul(60_000)? + (s * 1000.0).round() as i64)
}

/// Remove enhanced-LRC word stamps `<mm:ss.xx>`, keeping the words.
fn strip_word_stamps(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        let (before, after) = rest.split_at(i);
        out.push_str(before);
        match after.find('>') {
            Some(j) if parse_stamp(&after[1..j]).is_some() => rest = &after[j + 1..],
            _ => {
                out.push('<');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    // collapse the double spaces the removed stamps can leave
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Number of lines with text.
pub fn non_empty(lines: &[Line]) -> usize {
    lines.iter().filter(|(_, t)| !t.trim().is_empty()).count()
}

/// Number of non-blank lines in plain text.
pub fn non_empty_plain(text: &str) -> usize {
    text.lines().filter(|l| !l.trim().is_empty()).count()
}

/// NetEase puts credit lines ("作词 : …") at the top of the lyrics; they are not lyrics.
pub fn drop_credit_lines(lines: Vec<Line>) -> Vec<Line> {
    const KEYS: [&str; 10] = ["作词", "作曲", "编曲", "制作", "制作人", "词", "曲", "混音", "母带", "监制"];
    let is_credit = |t: &str| t.trim().split_once([':', '：']).is_some_and(|(k, _)| KEYS.contains(&k.trim()));
    let skip = lines.iter().take_while(|(_, t)| t.trim().is_empty() || is_credit(t)).count();
    lines.into_iter().skip(skip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(l: &[Line]) -> Vec<&str> {
        l.iter().map(|(_, t)| t.as_str()).collect()
    }

    #[test]
    fn parse_table() {
        let cases: &[(&str, &[(i64, &str)])] = &[
            ("[00:01.50] hello", &[(1500, "hello")]),
            ("[01:02.250]x", &[(62250, "x")]),
            ("[1:02]x", &[(62000, "x")]),
            ("[00:03.00][01:00.25]chorus", &[(3000, "chorus"), (60250, "chorus")]),
            ("[00:05.00]", &[(5000, "")]),
            ("[00:10.00]b\n[00:02.00]a", &[(2000, "a"), (10000, "b")]),
            ("[00:01:50]colon hundredths", &[(1500, "colon hundredths")]),
            ("[ar:Someone]\n[ti:Song]\n[by:me]\n[00:01.00]a", &[(1000, "a")]),
            ("[00:01.00]a\r\n[00:02.00]b\r\n", &[(1000, "a"), (2000, "b")]),
            ("\u{feff}[00:01.00]bom", &[(1000, "bom")]),
            ("[00:01.00]<00:01.00>he<00:01.40>llo <00:01.80>world", &[(1000, "hello world")]),
            ("[00:01.00]a < b and b > c", &[(1000, "a < b and b > c")]),
            ("[offset:+500]\n[00:01.00]a\n[00:00.20]b", &[(0, "b"), (500, "a")]),
            ("[offset:-250]\n[00:01.00]a", &[(1250, "a")]),
            ("just text\nno stamps\n[broken", &[]),
            ("[00:xx.00]bad\n[::]bad", &[]),
            ("", &[]),
        ];
        for (input, want) in cases {
            let got = parse_lrc(input);
            let want: Vec<Line> = want.iter().map(|(m, t)| (*m, (*t).to_owned())).collect();
            assert_eq!(got, want, "input {input:?}");
        }
    }

    #[test]
    fn equal_stamps_keep_file_order() {
        let l = parse_lrc("[00:01.00]first\n[00:01.00]second");
        assert_eq!(texts(&l), ["first", "second"]);
    }

    #[test]
    fn counts() {
        let l = parse_lrc("[00:01.00]a\n[00:02.00]\n[00:03.00]b");
        assert_eq!(non_empty(&l), 2);
        assert_eq!(non_empty_plain("a\n\n  \nb\n"), 2);
    }

    #[test]
    fn credit_lines_dropped_from_the_top_only() {
        let l = parse_lrc("[00:00.00] 作词 : A\n[00:01.00] 作曲 : B\n[00:02.00]real line\n[00:03.00]曲 : keep me");
        assert_eq!(texts(&drop_credit_lines(l)), ["real line", "曲 : keep me"]);
    }
}
