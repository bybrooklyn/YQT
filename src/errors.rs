// SPDX-License-Identifier: GPL-3.0-only
//! Turns RustyPipe errors into messages for people.
//!
//! The result is `"headline"` or `"headline\n\ntechnical detail"`: the UI shows the headline large and
//! the detail small, so a parser failure never reaches the user as raw `no sectionListRenderer content`.
#![allow(dead_code)] // `AppError` and friends are the shared contract; the UI workstreams start using them

use crate::auth::AUTH_MSG;

/// Headline for a signed-in request that was answered with a page the parser can't read: most often YouTube's
/// "sign in to see this" page, sent because the session is no longer accepted.
pub const SESSION_SUSPECT_MSG: &str =
    "YouTube sent a page the app couldn't read. Your session may have just rotated – try again in a moment, or sign in again from Settings.";
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
            ErrorKind::Offline => "Downloaded music still plays.",
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
        Self::from_rustypipe_request(e, signed_in, true)
    }

    /// [`from_rustypipe`](Self::from_rustypipe) for a request known to have been sent with (`authenticated`) or
    /// without the user's session. A 401/403 answer to an anonymous request (a blocked address, a bot check) says
    /// nothing about the session and must not be reported as "your session expired".
    pub fn from_rustypipe_request(e: &Error, signed_in: bool, authenticated: bool) -> AppError {
        let detail = e.to_string();
        match e {
            Error::HttpStatus(401 | 403, _) if !authenticated => AppError::new(ErrorKind::Internal, "YouTube refused the request.", detail),
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
                // (Seen live: the Library answered `logged_in=0` with a "Looking for what you've liked?" page once
                // the copied session had gone stale. The backend takes this headline as a reason to check the
                // session, and renew it, see `backend::finish`.)
                _ if signed_in && authenticated => AppError::new(ErrorKind::Parse, SESSION_SUSPECT_MSG, detail),
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
            "yqt error {}\nkind: {:?}\nmessage: {}\nhint: {}\ndetail: {}\nversion: {}",
            self.id,
            self.kind,
            self.headline,
            self.hint,
            self.detail,
            env!("CARGO_PKG_VERSION")
        )
    }
}

// ---- severity, retry schedule, offline -------------------------------------------------------------------------

/// How loudly a failure is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// In place of the content that could not be shown (a page that failed to load).
    Inline,
    /// A passing notice for something in the background or a one-off action (a like that didn't go through).
    Toast,
    /// The user has to do something before anything works (sign in again, fix the config).
    Blocking,
}

impl ErrorKind {
    pub fn severity(self) -> Severity {
        match self {
            ErrorKind::Auth | ErrorKind::Config => Severity::Blocking,
            ErrorKind::RateLimit | ErrorKind::Storage | ErrorKind::Internal => Severity::Toast,
            ErrorKind::Network | ErrorKind::Parse | ErrorKind::NotFound | ErrorKind::Unavailable | ErrorKind::Offline => Severity::Inline,
        }
    }

    /// Stable lowercase name for QML (`"network"`, `"auth"`, …).
    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::Network => "network",
            ErrorKind::Auth => "auth",
            ErrorKind::Parse => "parse",
            ErrorKind::NotFound => "notfound",
            ErrorKind::RateLimit => "ratelimit",
            ErrorKind::Offline => "offline",
            ErrorKind::Unavailable => "unavailable",
            ErrorKind::Storage => "storage",
            ErrorKind::Config => "config",
            ErrorKind::Internal => "internal",
        }
    }
}

impl Retry {
    /// `"never"`, `"manual"` or `"auto"` for QML.
    pub fn name(self) -> &'static str {
        match self {
            Retry::Never => "never",
            Retry::Manual => "manual",
            Retry::Auto { .. } => "auto",
        }
    }

    /// The delay before the next automatic attempt, given how many automatic attempts were already made.
    /// `None` once they are used up (the user then gets a Retry button) or when the policy is not automatic.
    pub fn next_auto_delay_ms(self, attempts_made: u32) -> Option<u64> {
        match self {
            Retry::Auto { after_ms, max_attempts } if attempts_made < max_attempts => Some(after_ms),
            _ => None,
        }
    }

    /// Does the UI offer a Retry button?
    pub fn offers_button(self) -> bool {
        !matches!(self, Retry::Never)
    }
}

/// Source of truth for "is there a network connection at all". **Integration point:** the connectivity stream
/// implements this (e.g. its `Connectivity` singleton) and calls [`set_connectivity`] once at start-up. Until then
/// the app assumes it is online and network failures stay `Network`.
pub trait Connectivity: Send + Sync {
    fn online(&self) -> bool;
}

static CONNECTIVITY: std::sync::RwLock<Option<Box<dyn Connectivity>>> = std::sync::RwLock::new(None);

pub fn set_connectivity(c: Box<dyn Connectivity>) {
    *CONNECTIVITY.write().unwrap() = Some(c);
}

pub fn is_online() -> bool {
    CONNECTIVITY.read().unwrap().as_ref().is_none_or(|c| c.online())
}

impl AppError {
    /// No connection at all (downloaded music still works).
    pub fn offline(detail: impl Into<String>) -> AppError {
        AppError::new(ErrorKind::Offline, "You're offline.", detail)
    }

    /// A network failure while the device is known to be offline is an `Offline` error, with its own advice.
    pub fn for_connectivity(self, online: bool) -> AppError {
        if online || self.kind != ErrorKind::Network {
            self
        } else {
            AppError::offline(self.detail)
        }
    }

    /// "Copy details" text with anything secret-looking masked.
    pub fn diagnostics_redacted(&self) -> String {
        crate::logging::redact(&self.diagnostics())
    }
}

// ---- toasts -----------------------------------------------------------------------------------------------------

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        /// Passing notices for failures that don't replace a view. QML shows them (Main.qml connects `toast`).
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(QString, last_headline)]
        #[qproperty(QString, last_hint)]
        #[qproperty(QString, last_id)]
        #[qproperty(QString, last_diagnostics)] // redacted, for a "Copy details" action
        type ErrorCenter = super::ErrorCenterRust;

        /// Report an error from QML (`kind` is one of the `ErrorKind` names, e.g. "network").
        #[qinvokable]
        fn report(self: Pin<&mut ErrorCenter>, headline: &QString, detail: &QString, kind: &QString);

        #[qsignal]
        fn toast(self: Pin<&mut ErrorCenter>, headline: QString, hint: QString, id: QString);
    }

    impl cxx_qt::Threading for ErrorCenter {}
    impl cxx_qt::Initialize for ErrorCenter {}
}

use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;

#[derive(Default)]
pub struct ErrorCenterRust {
    last_headline: QString,
    last_hint: QString,
    last_id: QString,
    last_diagnostics: QString,
}

static CENTER: std::sync::Mutex<Option<cxx_qt::CxxQtThread<qobject::ErrorCenter>>> = std::sync::Mutex::new(None);

impl cxx_qt::Initialize for qobject::ErrorCenter {
    fn initialize(self: Pin<&mut Self>) {
        *CENTER.lock().unwrap() = Some(self.qt_thread());
    }
}

/// Show `e` as a toast from anywhere in Rust (any thread). Dropped silently until the QML side exists.
pub fn toast(e: &AppError) {
    let e = e.clone();
    if let Some(th) = CENTER.lock().unwrap().as_ref() {
        let _ = th.queue(move |c| c.show(&e));
    }
}

impl qobject::ErrorCenter {
    fn show(mut self: Pin<&mut Self>, e: &AppError) {
        let (h, hint, id) = (QString::from(&e.headline), QString::from(&e.hint), QString::from(&e.id));
        self.as_mut().set_last_headline(h.clone());
        self.as_mut().set_last_hint(hint.clone());
        self.as_mut().set_last_id(id.clone());
        self.as_mut().set_last_diagnostics(QString::from(&e.diagnostics_redacted()));
        self.toast(h, hint, id);
    }

    fn report(self: Pin<&mut Self>, headline: &QString, detail: &QString, kind: &QString) {
        let kind = match kind.to_string().as_str() {
            "network" => ErrorKind::Network,
            "auth" => ErrorKind::Auth,
            "offline" => ErrorKind::Offline,
            "ratelimit" => ErrorKind::RateLimit,
            "notfound" => ErrorKind::NotFound,
            "unavailable" => ErrorKind::Unavailable,
            "storage" => ErrorKind::Storage,
            _ => ErrorKind::Internal,
        };
        self.show(&AppError::new(kind, headline.to_string(), detail.to_string()));
    }
}

/// A user-facing message for `e`. `signed_in` tailors the advice for unreadable pages.
pub fn friendly(e: &Error, signed_in: bool) -> String {
    AppError::from_rustypipe(e, signed_in).ui_string()
}

/// [`friendly`] for a request known to have been sent with or without the user's session.
pub fn friendly_request(e: &Error, signed_in: bool, authenticated: bool) -> String {
    AppError::from_rustypipe_request(e, signed_in, authenticated).ui_string()
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
    fn a_refused_anonymous_request_is_not_a_session_problem() {
        for code in [401, 403] {
            let e = Error::HttpStatus(code, "x".into());
            for signed_in in [true, false] {
                let anon = AppError::from_rustypipe_request(&e, signed_in, false);
                assert_eq!(anon.kind, ErrorKind::Internal, "{code}");
                assert_ne!(anon.ui_string(), AUTH_MSG, "the backend signs the user out on this exact text");
                assert_ne!(friendly_request(&e, signed_in, false), AUTH_MSG);
            }
            // the same answer to a request that carried the session does mean the session is gone
            assert_eq!(friendly_request(&e, true, true), AUTH_MSG);
        }
        // RustyPipe's own "not signed in" is about the session whatever the request was
        assert_eq!(friendly_request(&Error::Auth(rustypipe::error::AuthError::NoLogin), false, false), AUTH_MSG);
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
    fn severity_table() {
        use ErrorKind::*;
        let table = [
            (Auth, Severity::Blocking),
            (Config, Severity::Blocking),
            (RateLimit, Severity::Toast),
            (Storage, Severity::Toast),
            (Internal, Severity::Toast),
            (Network, Severity::Inline),
            (Parse, Severity::Inline),
            (NotFound, Severity::Inline),
            (Unavailable, Severity::Inline),
            (Offline, Severity::Inline),
        ];
        for (kind, sev) in table {
            assert_eq!(kind.severity(), sev, "{kind:?}");
        }
    }

    #[test]
    fn kind_and_retry_names_are_stable_for_qml() {
        assert_eq!(ErrorKind::NotFound.name(), "notfound");
        assert_eq!(ErrorKind::Offline.name(), "offline");
        assert_eq!(Retry::Never.name(), "never");
        assert_eq!(Retry::Manual.name(), "manual");
        assert_eq!(Retry::Auto { after_ms: 1, max_attempts: 1 }.name(), "auto");
    }

    #[test]
    fn automatic_retries_are_bounded_then_fall_back_to_the_button() {
        let r = Retry::Auto { after_ms: 1_500, max_attempts: 2 };
        assert_eq!(r.next_auto_delay_ms(0), Some(1_500));
        assert_eq!(r.next_auto_delay_ms(1), Some(1_500));
        assert_eq!(r.next_auto_delay_ms(2), None);
        assert_eq!(Retry::Manual.next_auto_delay_ms(0), None);
        assert_eq!(Retry::Never.next_auto_delay_ms(0), None);
        assert!(Retry::Manual.offers_button() && r.offers_button() && !Retry::Never.offers_button());
    }

    #[test]
    fn offline_is_its_own_error_with_downloads_advice() {
        let e = AppError::offline("dns");
        assert_eq!(e.kind, ErrorKind::Offline);
        assert!(e.hint.contains("Downloaded"));
        assert!(matches!(e.retry, Retry::Auto { .. }));
    }

    #[test]
    fn network_errors_become_offline_only_when_the_device_is_offline() {
        let net = AppError::from_rustypipe(&Error::Http("dns".into()), false);
        assert_eq!(net.clone().for_connectivity(true).kind, ErrorKind::Network);
        let off = net.clone().for_connectivity(false);
        assert_eq!((off.kind, off.detail.as_str()), (ErrorKind::Offline, net.detail.as_str()));
        let parse = AppError::new(ErrorKind::Parse, "x", "y");
        assert_eq!(parse.for_connectivity(false).kind, ErrorKind::Parse, "only connection failures are rewritten");
    }

    #[test]
    fn without_a_connectivity_source_the_app_assumes_it_is_online() {
        assert!(is_online());
    }

    #[test]
    fn copied_details_are_redacted() {
        let e = AppError::new(ErrorKind::Internal, "Failed", "Cookie: SAPISID=abcdef123456; other=1");
        let d = e.diagnostics_redacted();
        assert!(!d.contains("abcdef123456"), "{d}");
        assert!(d.contains(&e.id));
    }

    #[test]
    fn split_handles_missing_detail() {
        assert_eq!(split("only a headline"), ("only a headline", ""));
        assert_eq!(split("head\n\ndetail"), ("head", "detail"));
    }
}
