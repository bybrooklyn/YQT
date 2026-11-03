// SPDX-License-Identifier: GPL-3.0-only
//! Offline downloads, modelled on YouTube Music's *Smart downloads*.
//!
//! * [`quality`] stream choice + file-name rules, [`rules`] the planner (pure), [`store`] the SQLite index,
//!   [`manager`] the engine (queue, resume, retries, pause rules, smart loop).
//! * [`local_source`] / [`local_playback_url`]: the playback hook. `player::resolve()` should prefer a local
//!   file when there is one; the proxy serves it with `Range` support at `/t/<token>/dl/<id>`.
//! * QML: the `Downloads` singleton (`src/downloadsqml.rs`), plus `DownloadButton.qml` and `DownloadsPage.qml`.
pub mod manager;
pub mod quality;
pub mod rules;
pub mod store;

use std::path::{Path, PathBuf};

// ---- local playback hook -----------------------------------------------------------------------

/// A finished download on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSource {
    pub path: PathBuf,
    pub mime: &'static str,
    pub len: u64,
}

/// `<dir>/<id>.<ext>` if it exists as a regular file (not a symlink). The id is validated first, so no path
/// can be built from untrusted text.
pub fn local_source_in(dir: &Path, id: &str) -> Option<LocalSource> {
    if !quality::valid_id(id) {
        return None;
    }
    quality::EXTS.iter().find_map(|ext| {
        let path = dir.join(quality::file_name(id, ext)?);
        let meta = std::fs::symlink_metadata(&path).ok().filter(|m| m.is_file() && m.len() > 0)?;
        Some(LocalSource { path, mime: quality::mime_for_ext(ext)?, len: meta.len() })
    })
}

/// The downloaded file of `video_id` in the configured downloads folder.
pub fn local_source(video_id: &str) -> Option<LocalSource> {
    local_source_in(&manager::Settings::load().dir, video_id)
}

/// The URL (served by the local proxy, with Range support) to play `video_id` from disk, if it is downloaded.
/// `player::resolve()` should try this first; it also marks the song as played for the eviction order.
pub fn local_playback_url(video_id: &str) -> Option<String> {
    local_source(video_id)?;
    manager::played(video_id);
    Some(crate::proxy::download_url(video_id))
}

/// What a `Range` header asks for, against a file of a given length.
#[derive(Debug, PartialEq, Eq)]
pub enum RangeOutcome {
    /// No (usable) range: send everything with 200.
    Full,
    /// Send bytes `start..=end` with 206.
    Partial { start: u64, end: u64 },
    /// 416.
    Unsatisfiable,
}

/// Parse a `Range: bytes=…` header value. Multi-range and malformed headers are ignored (full body), as
/// RFC 9110 allows.
pub fn parse_byte_range(header: Option<&str>, len: u64) -> RangeOutcome {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else { return RangeOutcome::Full };
    if spec.contains(',') {
        return RangeOutcome::Full;
    }
    let Some((a, b)) = spec.split_once('-') else { return RangeOutcome::Full };
    let (a, b) = (a.trim(), b.trim());
    let num = |s: &str| s.parse::<u64>().ok();
    if len == 0 {
        return RangeOutcome::Unsatisfiable;
    }
    match (a.is_empty(), b.is_empty()) {
        (true, true) => RangeOutcome::Full,
        // suffix: the last n bytes
        (true, false) => match num(b) {
            Some(0) => RangeOutcome::Unsatisfiable,
            Some(n) => RangeOutcome::Partial { start: len.saturating_sub(n), end: len - 1 },
            None => RangeOutcome::Full,
        },
        (false, _) => {
            let Some(start) = num(a) else { return RangeOutcome::Full };
            let end = if b.is_empty() {
                len - 1
            } else {
                match num(b) {
                    Some(e) if e >= start => e.min(len - 1),
                    _ => return RangeOutcome::Full, // end before start = invalid
                }
            };
            if start >= len {
                RangeOutcome::Unsatisfiable
            } else {
                RangeOutcome::Partial { start, end }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ytm-dl-mod-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn local_source_finds_only_validated_regular_files() {
        let dir = tmpdir("local");
        std::fs::write(dir.join("dQw4w9WgXcQ.webm"), b"12345").unwrap();
        std::fs::write(dir.join("aaaaaaaaaaa.m4a.part"), b"partial").unwrap();
        std::fs::write(dir.join("bbbbbbbbbbb.m4a"), b"").unwrap();
        let s = local_source_in(&dir, "dQw4w9WgXcQ").unwrap();
        assert_eq!((s.len, s.mime, s.path.clone()), (5, "audio/webm", dir.join("dQw4w9WgXcQ.webm")));
        assert_eq!(local_source_in(&dir, "aaaaaaaaaaa"), None, "partials are not playable");
        assert_eq!(local_source_in(&dir, "bbbbbbbbbbb"), None, "empty files are not playable");
        assert_eq!(local_source_in(&dir, "zzzzzzzzzzz"), None);
        for bad in ["../x", "dQw4w9WgXcQ/../../x", "", "dQw4w9WgXc.", "dQw4w9WgXcQ\0"] {
            assert_eq!(local_source_in(&dir, bad), None, "{bad:?}");
        }
        #[cfg(unix)]
        {
            // a symlink planted in the folder must not be followed
            std::os::unix::fs::symlink("/etc/hostname", dir.join("ccccccccccc.m4a")).unwrap();
            assert_eq!(local_source_in(&dir, "ccccccccccc"), None);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn range_table() {
        use RangeOutcome::*;
        let p = |start, end| Partial { start, end };
        let cases: &[(Option<&str>, u64, RangeOutcome)] = &[
            (None, 100, Full),
            (Some("bytes=0-"), 100, p(0, 99)),
            (Some("bytes=0-0"), 100, p(0, 0)),
            (Some("bytes=10-19"), 100, p(10, 19)),
            (Some("bytes=90-"), 100, p(90, 99)),
            (Some("bytes=90-500"), 100, p(90, 99)),
            (Some("bytes=-10"), 100, p(90, 99)),
            (Some("bytes=-500"), 100, p(0, 99)),
            (Some("bytes=-0"), 100, Unsatisfiable),
            (Some("bytes=100-"), 100, Unsatisfiable),
            (Some("bytes=150-160"), 100, Unsatisfiable),
            (Some("bytes=20-10"), 100, Full),
            (Some("bytes=0-4,10-14"), 100, Full),
            (Some("bytes=-"), 100, Full),
            (Some("bytes=a-b"), 100, Full),
            (Some("items=0-5"), 100, Full),
            (Some("garbage"), 100, Full),
            (Some(" bytes=5-6 "), 100, p(5, 6)),
            (Some("bytes=0-"), 0, Unsatisfiable),
            (Some("bytes=18446744073709551615-"), 100, Unsatisfiable),
            (Some("bytes=0-18446744073709551615"), 100, p(0, 99)),
            (Some("bytes=99999999999999999999-"), 100, Full),
        ];
        for (h, len, want) in cases {
            assert_eq!(&parse_byte_range(*h, *len), want, "{h:?} on {len}");
        }
    }
}
