// SPDX-License-Identifier: GPL-3.0-only
//! Turns RustyPipe errors into messages for people.
//!
//! The result is `"headline"` or `"headline\n\ntechnical detail"`: the UI shows the headline large and
//! the detail small, so a parser failure never reaches the user as raw `no sectionListRenderer content`.
#![allow(dead_code)] // `AppError` and friends are the shared contract; the UI workstreams start using them

use crate::auth::AUTH_MSG;
use rustypipe::error::{Error, ExtractionError};
use serde::Serialize;

/// Split a message from [`friendly`] into (headline, detail).
#[allow(dead_code)] // used by tests and kept next to `friendly` so the format has one home
pub fn split(msg: &str) -> (&str, &str) {
    msg.split_once("\n\n").unwrap_or((msg, ""))
}

/// What went wrong, in terms the UI can act on (which buttons to offer, whether to retry by itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Couldn't reach the server.
    Network,
    /// The session is missing or no longer accepted → offer "Open Settings".
    Auth,
    /// The server answered with something we couldn't read.
    Parse,
    NotFound,
    RateLimit,
    /// No connection at all; downloaded music still works.
    Offline,
    /// Exists but can't be played/shown (removed, private, region-locked).
    Unavailable,
    /// Disk problems: full, read-only, permissions.
    Storage,
    /// A bad value in config.toml.
    Config,
    Internal,
}

/// Whether and how a failed action is tried again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum Retry {
    /// Retrying can't help (the item is gone, the session needs the user).
    Never,
    /// Offer a Retry button.
    Manual,
    /// Try again by itself after a delay, up to `max_attempts` times, then fall back to `Manual`.
    Auto { after_ms: u64, max_attempts: u32 },
}

impl ErrorKind {
    /// Advice shown under the headline when the producer didn't give a specific one.
    pub fn default_hint(self) -> &'static str {
        match self {
            ErrorKind::Network => "Check your internet connection.",
            ErrorKind::Auth => "Sign in again from Settings.",
            ErrorKind::Parse => "YouTube may have changed something. Try again later.",
            ErrorKind::NotFound | ErrorKind::Unavailable => "",
            ErrorKind::RateLimit => "Wait a minute before trying again.",
            ErrorKind::Offline => "You're offline. Downloaded music still plays.",
            ErrorKind::Storage => "Check free disk space and folder permissions.",
            ErrorKind::Config => "Fix or delete that setting in config.toml.",
            ErrorKind::Internal => "",
        }
    }

    pub fn default_retry(self) -> Retry {
        match self {
            ErrorKind::Network => Retry::Auto { after_ms: 1_500, max_attempts: 2 },
            ErrorKind::Offline => Retry::Auto { after_ms: 5_000, max_attempts: 3 },
            ErrorKind::RateLimit => Retry::Auto { after_ms: 60_000, max_attempts: 1 },
            ErrorKind::Auth | ErrorKind::NotFound | ErrorKind::Unavailable => Retry::Never,
            ErrorKind::Parse | ErrorKind::Storage | ErrorKind::Config | ErrorKind::Internal => Retry::Manual,
        }
    }
}

/// A failure, described for both people and code. The whole UI shows errors through this one type.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub kind: ErrorKind,
    /// One plain-language sentence.
    pub headline: String,
    /// What the user can do about it (may be empty).
    pub hint: String,
    /// Technical detail for the "details" line / "Copy details" (may be empty). Never put secrets here.
    pub detail: String,
    pub retry: Retry,
    /// Short stable id of this failure (same kind + detail ⇒ same id): lets users and logs refer to it.
    pub id: String,
}

impl AppError {
    pub fn new(kind: ErrorKind, headline: impl Into<String>, detail: impl Into<String>) -> AppError {
        let (headline, detail) = (headline.into(), detail.into());
        let id = Self::make_id(kind, if detail.is_empty() { &headline } else { &detail });
        AppError { kind, hint: kind.default_hint().to_owned(), retry: kind.default_retry(), headline, detail, id }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> AppError {
        self.hint = hint.into();
        self
    }

    pub fn with_retry(mut self, retry: Retry) -> AppError {
        self.retry = retry;
        self
    }

    /// FNV-1a over `kind:text`, 8 hex digits. Not a security feature, just a stable short reference.
    fn make_id(kind: ErrorKind, text: &str) -> String {
        let mut hash: u32 = 0x811c_9dc5;
        for b in format!("{kind:?}:{text}").bytes() {
            hash ^= u32::from(b);
            hash = hash.wrapping_mul(0x0100_0193);
        }
        format!("{hash:08x}")
    }

    /// Classify a RustyPipe error. `signed_in` tailors the advice for unreadable pages.
    pub fn from_rustypipe(e: &Error, signed_in: bool) -> AppError {
        let detail = e.to_string();
        match e {
            Error::Auth(_) | Error::HttpStatus(401 | 403, _) => {
                // the headline *is* the session message (backend.rs compares for equality to flip the signed-in state)
                let mut err = AppError::new(ErrorKind::Auth, AUTH_MSG, "");
                err.hint = String::new();
                err
            }
            Error::Extraction(x) => match x {
                ExtractionError::NotFound { .. } => AppError::new(ErrorKind::NotFound, "That item doesn't exist any more.", detail),
                ExtractionError::Unavailable { .. } => {
                    AppError::new(ErrorKind::Unavailable, "That isn't available (removed, private, or blocked in your region).", detail)
                }
                ExtractionError::BadRequest(_) => AppError::new(ErrorKind::Internal, "YouTube rejected the request.", detail),
                // InvalidData and friends: YouTube answered, but with a page the parser doesn't understand.
                // For a signed-in user the usual cause is a session that was just rotated (YouTube then answers
                // with a "sign in to see this" message page).
                _ if signed_in => AppError::new(
                    ErrorKind::Parse,
                    "YouTube sent a page the app couldn't read. Your session may have just rotated – try again in a moment, or sign in again from Settings.",
                    detail,
                ),
                _ => AppError::new(ErrorKind::Parse, "YouTube sent a page the app couldn't read. It may have changed – try again later.", detail),
            },
            Error::HttpStatus(429, _) => AppError::new(ErrorKind::RateLimit, "YouTube is rate-limiting requests. Wait a minute and try again.", detail),
            Error::HttpStatus(code, _) if *code >= 500 => {
                AppError::new(ErrorKind::Network, "YouTube is having problems right now. Try again shortly.", detail)
            }
            Error::HttpStatus(..) => AppError::new(ErrorKind::Internal, "YouTube refused the request.", detail),
            Error::Http(_) => AppError::new(ErrorKind::Network, "Can't reach YouTube. Check your connection and try again.", detail),
            Error::Other(_) => AppError::new(ErrorKind::Internal, detail, ""),
        }
    }

    /// `"headline"` or `"headline\n\ndetail"`: the compact string form the QML layer already understands.
    pub fn ui_string(&self) -> String {
        if self.detail.is_empty() {
            self.headline.clone()
        } else {
            format!("{}\n\n{}", self.headline, self.detail)
        }
    }

    /// Plain text for "Copy details". Contains no cookies or tokens by construction (callers must not put any in
    /// `detail`); the logging layer additionally redacts before it is written anywhere.
    pub fn diagnostics(&self) -> String {
        format!(
            "ytm error {}\nkind: {:?}\nmessage: {}\nhint: {}\ndetail: {}\nversion: {}",
            self.id,
            self.kind,
            self.headline,
            self.hint,
            self.detail,
            env!("CARGO_PKG_VERSION")
        )
    }
}

/// A user-facing message for `e`. `signed_in` tailors the advice for unreadable pages.
pub fn friendly(e: &Error, signed_in: bool) -> String {
    AppError::from_rustypipe(e, signed_in).ui_string()
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

    // ---- AppError ----

    #[test]
    fn app_error_classifies_each_failure_family() {
        let kind = |e: &Error| AppError::from_rustypipe(e, true).kind;
        assert_eq!(kind(&invalid()), ErrorKind::Parse);
        assert_eq!(kind(&Error::Http("dns".into())), ErrorKind::Network);
        assert_eq!(kind(&Error::HttpStatus(503, "".into())), ErrorKind::Network);
        assert_eq!(kind(&Error::HttpStatus(429, "".into())), ErrorKind::RateLimit);
        assert_eq!(kind(&Error::HttpStatus(401, "".into())), ErrorKind::Auth);
        assert_eq!(kind(&Error::HttpStatus(418, "".into())), ErrorKind::Internal);
        assert_eq!(kind(&Error::Other("x".into())), ErrorKind::Internal);
    }

    #[test]
    fn retry_policy_follows_the_kind() {
        let policy = |e: &Error| AppError::from_rustypipe(e, false).retry;
        assert_eq!(policy(&Error::HttpStatus(401, "".into())), Retry::Never);
        assert!(matches!(policy(&Error::Http("x".into())), Retry::Auto { .. }));
        assert_eq!(policy(&invalid()), Retry::Manual);
    }

    #[test]
    fn auth_error_keeps_the_exact_session_message_and_has_no_detail() {
        let e = AppError::from_rustypipe(&Error::HttpStatus(403, "x".into()), true);
        assert_eq!(e.ui_string(), AUTH_MSG);
        assert!(e.detail.is_empty());
    }

    #[test]
    fn ids_are_stable_short_and_distinguish_failures() {
        let a = AppError::new(ErrorKind::Network, "Can't reach it", "dns failure");
        let b = AppError::new(ErrorKind::Network, "Can't reach it", "dns failure");
        let c = AppError::new(ErrorKind::Network, "Can't reach it", "timeout");
        assert_eq!(a.id, b.id);
        assert_ne!(a.id, c.id);
        assert_eq!(a.id.len(), 8);
        assert!(a.id.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn builders_override_defaults_and_diagnostics_are_complete() {
        let e = AppError::new(ErrorKind::Storage, "Disk full", "ENOSPC").with_hint("Free some space").with_retry(Retry::Never);
        assert_eq!(e.hint, "Free some space");
        assert_eq!(e.retry, Retry::Never);
        let d = e.diagnostics();
        assert!(d.contains(&e.id) && d.contains("ENOSPC") && d.contains("Free some space"));
        assert!(d.contains("Storage"), "the kind is part of the diagnostics: {d}");
    }

    #[test]
    fn json_form_is_camel_case() {
        let v = serde_json::to_value(AppError::new(ErrorKind::Offline, "Offline", "")).unwrap();
        assert_eq!(v["kind"], "offline");
        assert_eq!(v["retry"]["policy"], "auto");
        assert!(v.get("headline").is_some() && v.get("id").is_some());
    }

    #[test]
    fn split_handles_missing_detail() {
        assert_eq!(split("only a headline"), ("only a headline", ""));
        assert_eq!(split("head\n\ndetail"), ("head", "detail"));
    }
}
