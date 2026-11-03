// SPDX-License-Identifier: GPL-3.0-only
//! Turns RustyPipe errors into messages for people.
//!
//! The result is `"headline"` or `"headline\n\ntechnical detail"`: the UI shows the headline large and
//! the detail small, so a parser failure never reaches the user as raw `no sectionListRenderer content`.
use crate::auth::AUTH_MSG;
use rustypipe::error::{Error, ExtractionError};

/// Split a message from [`friendly`] into (headline, detail).
#[allow(dead_code)] // used by tests and kept next to `friendly` so the format has one home
pub fn split(msg: &str) -> (&str, &str) {
    msg.split_once("\n\n").unwrap_or((msg, ""))
}

fn with_detail(headline: &str, detail: &str) -> String {
    format!("{headline}\n\n{detail}")
}

/// A user-facing message for `e`. `signed_in` tailors the advice for unreadable pages.
pub fn friendly(e: &Error, signed_in: bool) -> String {
    let detail = e.to_string();
    match e {
        Error::Auth(_) => AUTH_MSG.to_owned(),
        Error::Extraction(x) => match x {
            ExtractionError::NotFound { .. } => with_detail("That item doesn't exist any more.", &detail),
            ExtractionError::Unavailable { .. } => {
                with_detail("That isn't available (removed, private, or blocked in your region).", &detail)
            }
            ExtractionError::BadRequest(_) => with_detail("YouTube rejected the request.", &detail),
            // InvalidData and friends: YouTube answered, but with a page the parser doesn't understand.
            // For a signed-in user the usual cause is a session that was just rotated (YouTube then answers
            // with a "sign in to see this" message page).
            _ if signed_in => with_detail(
                "YouTube sent a page the app couldn't read. Your session may have just rotated – try again in a moment, or sign in again from Settings.",
                &detail,
            ),
            _ => with_detail("YouTube sent a page the app couldn't read. It may have changed – try again later.", &detail),
        },
        Error::HttpStatus(429, _) => with_detail("YouTube is rate-limiting requests. Wait a minute and try again.", &detail),
        Error::HttpStatus(code, _) if *code >= 500 => with_detail("YouTube is having problems right now. Try again shortly.", &detail),
        Error::HttpStatus(401 | 403, _) => AUTH_MSG.to_owned(),
        Error::HttpStatus(..) => with_detail("YouTube refused the request.", &detail),
        Error::Http(_) => with_detail("Can't reach YouTube. Check your connection and try again.", &detail),
        Error::Other(_) => detail,
    }
}

/// Should an authenticated request that failed this way be retried once? These are the
/// transient "unreadable page" and rate/availability blips, not real, lasting errors.
pub fn worth_retrying(e: &Error) -> bool {
    matches!(e, Error::Extraction(ExtractionError::InvalidData(_)) | Error::Http(_) | Error::HttpStatus(500..=599, _))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn invalid() -> Error {
        Error::Extraction(ExtractionError::InvalidData(Cow::Borrowed("no sectionListRenderer content")))
    }

    #[test]
    fn unreadable_page_gets_a_plain_headline_and_keeps_the_detail() {
        let msg = friendly(&invalid(), true);
        let (head, detail) = split(&msg);
        assert!(head.contains("couldn't read") && head.contains("Settings"), "{head}");
        assert!(detail.contains("no sectionListRenderer content"), "{detail}");
        assert!(!head.contains("sectionListRenderer"), "raw parser text must not be the headline");
    }

    #[test]
    fn signed_out_users_are_not_told_to_sign_in_again() {
        let msg = friendly(&invalid(), false);
        assert!(!split(&msg).0.contains("Settings"));
    }

    #[test]
    fn auth_errors_are_exactly_the_session_message() {
        // backend.rs compares for equality to flip the signed-in state, so this must stay exact
        assert_eq!(friendly(&Error::HttpStatus(401, "x".into()), true), AUTH_MSG);
        assert_eq!(friendly(&Error::HttpStatus(403, "x".into()), true), AUTH_MSG);
    }

    #[test]
    fn network_and_server_problems_are_distinguished() {
        assert!(friendly(&Error::Http("dns".into()), false).contains("Can't reach YouTube"));
        assert!(friendly(&Error::HttpStatus(429, "".into()), false).contains("rate-limiting"));
        assert!(friendly(&Error::HttpStatus(503, "".into()), false).contains("having problems"));
        assert!(friendly(&Error::HttpStatus(418, "".into()), false).contains("refused"));
    }

    #[test]
    fn other_errors_pass_through() {
        assert_eq!(friendly(&Error::Other("boom".into()), false), "error: boom");
    }

    #[test]
    fn only_transient_failures_are_retried() {
        assert!(worth_retrying(&invalid()));
        assert!(worth_retrying(&Error::Http("reset".into())));
        assert!(worth_retrying(&Error::HttpStatus(502, "".into())));
        assert!(!worth_retrying(&Error::HttpStatus(404, "".into())));
        assert!(!worth_retrying(&Error::Other("x".into())));
    }

    #[test]
    fn split_handles_missing_detail() {
        assert_eq!(split("only a headline"), ("only a headline", ""));
        assert_eq!(split("head\n\ndetail"), ("head", "detail"));
    }
}
