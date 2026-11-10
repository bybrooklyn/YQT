// SPDX-License-Identifier: GPL-3.0-only
//! YouTube Music's own plain lyrics (via RustyPipe): the last resort when nobody has synced ones.
use super::rank::{Candidate, Query};
use super::{Provider, ProviderFuture};
use crate::errors::AppError;
use rustypipe::error::{Error as RpError, ExtractionError};

pub const NAME: &str = "YouTube Music";

pub struct YoutubeMusic;

impl Provider for YoutubeMusic {
    fn name(&self) -> &'static str {
        NAME
    }
    fn fetch<'a>(&'a self, q: &'a Query) -> ProviderFuture<'a> {
        Box::pin(async move {
            let rp = crate::state::st().rp.query();
            let result = async {
                let details = rp.music_details(&q.video_id).await?;
                let Some(id) = details.lyrics_id else { return Ok(None) };
                Ok(Some(rp.music_lyrics(id).await?.body))
            }
            .await;
            from_result(result, q)
        })
    }
}

/// Map RustyPipe's answer. "Not found / unavailable" just means there are no lyrics (e.g. the video isn't a
/// music track): that is an empty answer, never an error to show.
pub fn from_result(r: Result<Option<String>, RpError>, q: &Query) -> Result<Vec<Candidate>, AppError> {
    match r {
        Ok(Some(body)) if !body.trim().is_empty() => Ok(vec![Candidate {
            provider: NAME,
            // YouTube looked the lyrics up by video id, so they belong to this very track
            title: q.title.clone(),
            artist: q.artist.clone(),
            duration_s: (q.duration_s > 0).then_some(f64::from(q.duration_s)),
            plain: body,
            ..Default::default()
        }]),
        Ok(_) => Ok(Vec::new()),
        Err(RpError::Extraction(ExtractionError::NotFound { .. } | ExtractionError::Unavailable { .. })) => Ok(Vec::new()),
        Err(e) => Err(AppError::from_rustypipe_request(&e, false, false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorKind;
    use rustypipe::error::UnavailabilityReason;
    use std::borrow::Cow;

    fn q() -> Query {
        Query { video_id: "v".into(), title: "T".into(), artist: "A".into(), duration_s: 100 }
    }

    #[test]
    fn not_found_and_unavailable_mean_no_lyrics() {
        let nf = RpError::Extraction(ExtractionError::NotFound { id: "MPLYt_x".into(), msg: Cow::Borrowed("not found") });
        assert!(from_result(Err(nf), &q()).unwrap().is_empty());
        let un = RpError::Extraction(ExtractionError::Unavailable { reason: UnavailabilityReason::Deleted, msg: "gone".into() });
        assert!(from_result(Err(un), &q()).unwrap().is_empty());
        assert!(from_result(Ok(None), &q()).unwrap().is_empty());
        assert!(from_result(Ok(Some("  \n".into())), &q()).unwrap().is_empty());
    }

    #[test]
    fn other_errors_are_failures_and_text_becomes_a_plain_candidate() {
        let bad = RpError::Extraction(ExtractionError::InvalidData(Cow::Borrowed("no sectionListRenderer content")));
        let e = from_result(Err(bad), &q()).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Parse);
        let c = from_result(Ok(Some("a\nb\nc\nd".into())), &q()).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].plain, "a\nb\nc\nd");
        assert!(c[0].synced.is_empty());
        assert_eq!(c[0].duration_s, Some(100.0));
    }
}
