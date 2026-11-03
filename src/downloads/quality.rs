// SPDX-License-Identifier: GPL-3.0-only
//! Which stream to download, and the file-name rules. Pure functions.
use crate::vision::{pick_format, AudioFormat, Quality};

/// A YouTube video id: exactly 11 characters of `[A-Za-z0-9_-]`. Anything else is refused before it can
/// ever become part of a path.
pub fn valid_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// File extensions a finished download can have (also what [`super::local_source_in`] looks for).
pub const EXTS: [&str; 4] = ["m4a", "webm", "ogg", "bin"];

/// Container extension for a stream mime type ("keep the container the stream came in").
pub fn ext_for_mime(mime: &str) -> &'static str {
    let m = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match m.as_str() {
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/webm" => "webm",
        "audio/ogg" => "ogg",
        _ => "bin",
    }
}

/// The mime type served for a stored file with this extension.
pub fn mime_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "m4a" => Some("audio/mp4"),
        "webm" => Some("audio/webm"),
        "ogg" => Some("audio/ogg"),
        "bin" => Some("application/octet-stream"),
        _ => None,
    }
}

/// Pick the stream for the `[downloads] quality` setting (`best` / `normal` / `data_saver`).
pub fn pick<'a>(formats: &'a [AudioFormat], setting: &str) -> Option<&'a AudioFormat> {
    pick_format(formats, Quality::from_setting(setting), None)
}

/// `<dir>/<id>.<ext>`, or `None` when the id or extension is not on the allow-lists.
pub fn file_name(id: &str, ext: &str) -> Option<String> {
    (valid_id(id) && EXTS.contains(&ext)).then(|| format!("{id}.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vision::parse_formats;
    use serde_json::json;

    #[test]
    fn ids_must_be_exactly_eleven_safe_chars() {
        assert!(valid_id("dQw4w9WgXcQ") && valid_id("a_b-c_d-e_f"));
        for bad in [
            "",
            "short",
            "dQw4w9WgXcQ1",
            "../etc/passwd",
            "..%2f..%2fetcpw",
            "dQw4w9WgXc/",
            "dQw4w9WgXc.",
            "dQw4w9WgXc\0",
            "dQw4w9WgXc\n",
            "dQw4w9WgXcé",
            "a b c d e f",
            "C:\\windows\\",
            "~~~~~~~~~~~",
        ] {
            assert!(!valid_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn file_names_are_only_built_from_checked_parts() {
        assert_eq!(file_name("dQw4w9WgXcQ", "m4a").as_deref(), Some("dQw4w9WgXcQ.m4a"));
        assert_eq!(file_name("../../../etc", "m4a"), None);
        assert_eq!(file_name("dQw4w9WgXcQ", "../x"), None);
        assert_eq!(file_name("dQw4w9WgXcQ", "sh"), None);
    }

    #[test]
    fn containers_follow_the_stream_mime() {
        assert_eq!(ext_for_mime("audio/mp4; codecs=\"mp4a.40.2\""), "m4a");
        assert_eq!(ext_for_mime("audio/webm; codecs=\"opus\""), "webm");
        assert_eq!(ext_for_mime("AUDIO/WEBM"), "webm");
        assert_eq!(ext_for_mime("text/html"), "bin");
        for e in EXTS {
            assert!(mime_for_ext(e).is_some());
        }
        assert_eq!(mime_for_ext("exe"), None);
    }

    #[test]
    fn quality_setting_maps_to_the_format_pick() {
        let f = parse_formats(&[
            json!({"itag":249,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":55_000,"url":"opus50"}),
            json!({"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":134_000,"url":"opus160"}),
            json!({"itag":139,"mimeType":"audio/mp4; codecs=\"mp4a.40.5\"","bitrate":49_000,"url":"aac48"}),
            json!({"itag":140,"mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":130_000,"url":"aac128"}),
        ]);
        assert_eq!(pick(&f, "best").unwrap().url, "opus160");
        assert_eq!(pick(&f, "normal").unwrap().url, "aac128");
        assert_eq!(pick(&f, "data_saver").unwrap().url, "aac48");
        assert_eq!(pick(&f, "garbage").unwrap().url, "aac128", "unknown setting = normal");
        assert!(pick(&[], "best").is_none());
    }
}
