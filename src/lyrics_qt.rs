// SPDX-License-Identifier: GPL-3.0-only
//! The `Lyrics` QML object: one instance per lyrics panel.
//!
//! ```qml
//! Lyrics { id: lyrics }
//! lyrics.request(Player.videoId, Player.title, Player.artist, seconds)
//! // lyrics.state: "idle" | "loading" | "synced" | "plain" | "unavailable" | "failed"
//! // lyrics.linesJson: [[startMs, text, endMs, isGap], ...]   (synced only)
//! ```
//! `linesJson` is deliberately small data. When a Rust list model for lyric lines exists, build it from
//! `layout::items(&lines, track_ms)` (the same list this file serialises) and drop `linesJson`.
use crate::lyrics::layout;
use crate::lyrics::rank::Query;
use crate::lyrics::LyricsResult;
use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        /// "idle" | "loading" | "synced" | "plain" | "unavailable" | "failed"
        #[qproperty(QString, state)]
        /// Where the lyrics came from ("LRCLIB", "NetEase", "YouTube Music"); empty unless synced/plain.
        #[qproperty(QString, source)]
        #[qproperty(QString, plain)]
        /// Short headline of the failure (state "failed").
        #[qproperty(QString, error_text)]
        /// Technical detail of the failure, may be empty.
        #[qproperty(QString, error_detail)]
        /// `[[startMs, text, endMs, isGap], ...]`
        #[qproperty(QString, lines_json)]
        type Lyrics = super::LyricsRust;

        /// Look the lyrics up (results arrive through the properties). A newer request supersedes an older one.
        #[qinvokable]
        fn request(self: Pin<&mut Lyrics>, video_id: &QString, title: &QString, artist: &QString, seconds: i32);
        /// Ask again after a failure.
        #[qinvokable]
        fn retry(self: Pin<&mut Lyrics>);
    }

    impl cxx_qt::Threading for Lyrics {}
}

pub struct LyricsRust {
    state: QString,
    source: QString,
    plain: QString,
    error_text: QString,
    error_detail: QString,
    lines_json: QString,
    seq: u64,
    last: Option<Query>,
}

impl Default for LyricsRust {
    fn default() -> Self {
        Self {
            state: QString::from("idle"),
            source: QString::default(),
            plain: QString::default(),
            error_text: QString::default(),
            error_detail: QString::default(),
            lines_json: QString::from("[]"),
            seq: 0,
            last: None,
        }
    }
}

/// What the properties show for a result. Pure, so it can be tested without Qt.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct View {
    pub state: &'static str,
    pub source: String,
    pub plain: String,
    pub error_text: String,
    pub error_detail: String,
    pub lines_json: String,
}

pub fn view(result: &LyricsResult, track_ms: i64) -> View {
    let base = View { lines_json: "[]".into(), ..Default::default() };
    match result {
        LyricsResult::Synced { lines, source } => {
            View { state: "synced", source: source.clone(), lines_json: layout::to_json(&layout::items(lines, track_ms)), ..base }
        }
        LyricsResult::Plain { text, source } => View { state: "plain", source: source.clone(), plain: text.clone(), ..base },
        LyricsResult::Unavailable => View { state: "unavailable", ..base },
        LyricsResult::Failed(e) => {
            let s = e.ui_string();
            let (head, detail) = crate::errors::split(&s);
            View { state: "failed", error_text: head.to_owned(), error_detail: detail.to_owned(), ..base }
        }
    }
}

impl qobject::Lyrics {
    fn start(mut self: Pin<&mut Self>, q: Query) {
        let seq = {
            // SAFETY: only mutates plain Rust-side fields in place; the QObject is never moved out of its pin.
            let r = unsafe { self.as_mut().rust_mut().get_unchecked_mut() };
            r.seq += 1;
            r.last = Some(q.clone());
            r.seq
        };
        self.as_mut().set_state(QString::from("loading"));
        self.as_mut().set_source(QString::default());
        self.as_mut().set_plain(QString::default());
        self.as_mut().set_error_text(QString::default());
        self.as_mut().set_error_detail(QString::default());
        self.as_mut().set_lines_json(QString::from("[]"));

        let th = self.qt_thread();
        crate::state::st().rt.spawn(async move {
            let cfg = crate::config::current();
            let dir = crate::lyrics::cache::default_dir();
            let result =
                crate::lyrics::resolve(crate::state::http(), &cfg.lyrics, cfg.security.allow_insecure_urls, &q, dir.as_deref()).await;
            let v = view(&result, i64::from(q.duration_s) * 1000);
            let _ = th.queue(move |mut o| {
                if o.rust().seq != seq {
                    return; // a newer request has taken over
                }
                o.as_mut().set_source(QString::from(&v.source));
                o.as_mut().set_plain(QString::from(&v.plain));
                o.as_mut().set_error_text(QString::from(&v.error_text));
                o.as_mut().set_error_detail(QString::from(&v.error_detail));
                o.as_mut().set_lines_json(QString::from(&v.lines_json));
                o.as_mut().set_state(QString::from(v.state));
            });
        });
    }

    fn request(self: Pin<&mut Self>, video_id: &QString, title: &QString, artist: &QString, seconds: i32) {
        self.start(Query {
            video_id: video_id.to_string(),
            title: title.to_string(),
            artist: artist.to_string(),
            duration_s: seconds.max(0) as u32,
        });
    }

    fn retry(self: Pin<&mut Self>) {
        if let Some(q) = self.rust().last.clone() {
            self.start(q);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::{AppError, ErrorKind};

    #[test]
    fn unavailable_has_no_error_text() {
        let v = view(&LyricsResult::Unavailable, 0);
        assert_eq!(v.state, "unavailable");
        assert!(v.error_text.is_empty() && v.error_detail.is_empty() && v.plain.is_empty());
        assert_eq!(v.lines_json, "[]");
    }

    #[test]
    fn failed_splits_headline_from_detail() {
        let v = view(&LyricsResult::Failed(AppError::new(ErrorKind::Network, "Can't reach LRCLIB.", "connect error")), 0);
        assert_eq!(v.state, "failed");
        assert_eq!(v.error_text, "Can't reach LRCLIB.");
        assert_eq!(v.error_detail, "connect error");
        let v = view(&LyricsResult::Failed(AppError::new(ErrorKind::Internal, "oops", "")), 0);
        assert_eq!((v.error_text.as_str(), v.error_detail.as_str()), ("oops", ""));
    }

    #[test]
    fn synced_and_plain_views() {
        let lines = vec![(1000, "a".to_owned()), (3000, "b".to_owned())];
        let v = view(&LyricsResult::Synced { lines, source: "LRCLIB".into() }, 10_000);
        assert_eq!((v.state, v.source.as_str()), ("synced", "LRCLIB"));
        let j: serde_json::Value = serde_json::from_str(&v.lines_json).unwrap();
        assert_eq!(j.as_array().unwrap().len(), 2);
        assert_eq!(j[1][2], 10_000);
        let v = view(&LyricsResult::Plain { text: "x\ny".into(), source: "YouTube Music".into() }, 0);
        assert_eq!((v.state, v.plain.as_str()), ("plain", "x\ny"));
    }

    #[test]
    fn youtube_not_found_ends_as_unavailable_not_as_raw_error_text() {
        use crate::lyrics::testing::{query, Mock, Script};
        use rustypipe::error::{Error as RpError, ExtractionError};
        let not_found = RpError::Extraction(ExtractionError::NotFound { id: "MPLYt_x".into(), msg: "extraction error".into() });
        let candidates = crate::lyrics::youtube::from_result(Err(not_found), &query());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = rt.block_on(crate::lyrics::lookup(
            vec![Mock::provider("YouTube Music", Script::Found(candidates.unwrap()))],
            &query(),
            4,
            true,
        ));
        assert_eq!(result, LyricsResult::Unavailable);
        let v = view(&result, 0);
        assert_eq!(v.state, "unavailable");
        assert!(!v.error_text.contains("extraction"));
    }
}
