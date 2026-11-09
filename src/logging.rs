// SPDX-License-Identifier: GPL-3.0-only
//! Logging: `tracing` to stderr and to a daily-rotated file in `<data>/logs/` (7 files kept),
//! filtered by `RUST_LOG`, with every line passed through [`redact`] (unless
//! `security.redact_logs = false`). A panic hook records a redacted crash note.
use regex::{Captures, Regex};
use std::io::Write;
use std::sync::OnceLock;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

const MASK: &str = "[REDACTED]";

struct Rules {
    header: Regex,
    json_key: Regex,
    scheme: Regex,
    cookie_pair: Regex,
    param: Regex,
    proxy_path: Regex,
    long_token: Regex,
}

fn rules() -> &'static Rules {
    static R: OnceLock<Rules> = OnceLock::new();
    R.get_or_init(|| Rules {
        // `Cookie: …` / `Set-Cookie: …` / `Authorization: …` – the rest of the line is the secret.
        header: Regex::new(r"(?i)\b(set-cookie|cookie|proxy-authorization|authorization|x-goog-visitor-id|x-goog-authuser|x-goog-pageid)\s*:[^\r\n]*").unwrap(),
        // JSON / Debug-ish `"refresh_token": "…"`.
        json_key: Regex::new(
            r#"(?i)("(?:cookie|set-cookie|authorization|refresh_token|access_token|id_token|client_secret|visitor_?data|x-goog-visitor-id|sapisid\w*|password)"\s*:\s*)"[^"]*""#,
        )
        .unwrap(),
        // `Bearer xyz`, `SAPISIDHASH 123_abc`, …
        scheme: Regex::new(r"(?i)\b(bearer|sapisidhash|sapisid1phash|sapisid3phash)\s+[^\s,;]+").unwrap(),
        // Cookie pairs by name: SID=…, SAPISID=…, __Secure-1PSID=…, __Secure-3PSIDTS=…, LOGIN_INFO=…
        cookie_pair: Regex::new(r#"((?-i:\b(?:__Secure-[A-Za-z0-9_]+|__Host-[A-Za-z0-9_]+|[A-Za-z0-9_]*SID[A-Za-z0-9_]*|LOGIN_INFO|VISITOR_INFO1_LIVE|VISITOR_PRIVACY_METADATA|YSC))=)[^;\s"',&]+"#).unwrap(),
        // Query / form parameters.
        param: Regex::new(
            r#"(?i)((?:^|[?&;\s"'(,{]))(sig|lsig|pot|n|signature|key|api_key|visitordata|visitor_id|refresh_token|access_token|id_token|client_secret|token|password)=[^&\s"'#;)]*"#,
        )
        .unwrap(),
        // Our own proxy URLs: `/t/<hex token>/`.
        proxy_path: Regex::new(r"/t/[0-9A-Fa-f]{16,}").unwrap(),
        long_token: Regex::new(r"[A-Za-z0-9_-]{40,}").unwrap(),
    })
}

/// Mask secrets in a log line / message. Pure; idempotent.
pub fn redact(input: &str) -> String {
    let r = rules();
    let s = r.header.replace_all(input, |c: &Captures| format!("{}: {MASK}", &c[1]));
    let s = r.json_key.replace_all(&s, |c: &Captures| format!("{}\"{MASK}\"", &c[1]));
    let s = r.scheme.replace_all(&s, |c: &Captures| format!("{} {MASK}", &c[1]));
    let s = r.cookie_pair.replace_all(&s, |c: &Captures| format!("{}{MASK}", &c[1]));
    let s = r.param.replace_all(&s, |c: &Captures| format!("{}{}={MASK}", &c[1], &c[2]));
    let s = r.proxy_path.replace_all(&s, format!("/t/{MASK}"));
    // Long opaque tokens: only when they look random (digit, or mixed case) so words survive.
    let s = r.long_token.replace_all(&s, |c: &Captures| {
        let t = &c[0];
        let mixed = t.bytes().any(|b| b.is_ascii_uppercase()) && t.bytes().any(|b| b.is_ascii_lowercase());
        if t.bytes().any(|b| b.is_ascii_digit()) || mixed {
            MASK.to_owned()
        } else {
            t.to_owned()
        }
    });
    s.into_owned()
}

/// Buffers one event and writes it (redacted) when dropped/flushed.
struct RedactWriter<W: Write> {
    inner: W,
    buf: Vec<u8>,
    redact: bool,
}

impl<W: Write> RedactWriter<W> {
    fn emit(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.buf).into_owned();
        self.buf.clear();
        if self.redact {
            self.inner.write_all(redact(&text).as_bytes())
        } else {
            self.inner.write_all(text.as_bytes())
        }
    }
}

impl<W: Write> Write for RedactWriter<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.emit()?;
        self.inner.flush()
    }
}

impl<W: Write> Drop for RedactWriter<W> {
    fn drop(&mut self) {
        let _ = self.emit();
    }
}

#[derive(Clone)]
struct RedactMaker<M> {
    inner: M,
    redact: bool,
}

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for RedactMaker<M> {
    type Writer = RedactWriter<M::Writer>;
    fn make_writer(&'a self) -> Self::Writer {
        RedactWriter { inner: self.inner.make_writer(), buf: Vec::new(), redact: self.redact }
    }
}

/// Remove all but the newest `keep` rotated `yqt.*.log` files and tighten the rest to 0600.
fn tidy_logs(dir: &std::path::Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("yqt.") && n.to_string_lossy().ends_with(".log")))
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(keep);
    for p in files.drain(..excess) {
        let _ = std::fs::remove_file(p);
    }
    for p in files {
        crate::state::tighten(&p);
    }
}

/// Install the global subscriber and the panic hook. Safe to call once, early in `main()`.
pub fn init() {
    static GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();
    let redact = crate::config::get().security.redact_logs;
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,yqt=info"));

    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(RedactMaker { inner: std::io::stderr, redact })
        .with_target(false)
        .with_filter(filter());

    let dir = crate::state::data_dir().join("logs");
    let file_layer = crate::state::ensure_private_dir(&dir).ok().and_then(|_| {
        tidy_logs(&dir, 7);
        let appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("yqt")
            .filename_suffix("log")
            .max_log_files(7)
            .build(&dir)
            .ok();
        tidy_logs(&dir, 7); // the appender just created today's file: make it 0600 too
        appender
    });
    let registry = tracing_subscriber::registry();
    let ok = if let Some(appender) = file_layer {
        let (nb, guard) = tracing_appender::non_blocking(appender);
        let _ = GUARD.set(guard);
        let file = tracing_subscriber::fmt::layer().with_writer(RedactMaker { inner: nb, redact }).with_ansi(false).with_filter(filter());
        registry.with(stderr).with(file).try_init().is_ok()
    } else {
        registry.with(stderr).try_init().is_ok()
    };
    if ok {
        install_panic_hook(redact);
        tracing::info!(version = env!("CARGO_PKG_VERSION"), redact, "yqt starting");
    }
}

fn install_panic_hook(redact_on: bool) {
    // Replace the default hook instead of chaining to it: the default one prints the raw payload and location,
    // bypassing the redaction below (a panic message can carry a signed stream URL or a cookie).
    std::panic::set_hook(Box::new(move |info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_owned()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "(non-string panic payload)".to_owned()
        };
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let note = crash_note(&msg, &loc, redact_on);
        tracing::error!("{note}");
        eprintln!("{note}");
    }));
}

/// The single-line crash note logged by the panic hook.
fn crash_note(msg: &str, loc: &str, redact_on: bool) -> String {
    let note = format!("panic at {loc}: {}", msg.replace(['\r', '\n'], " "));
    if redact_on {
        redact(&note)
    } else {
        note
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gone(input: &str, secret: &str) {
        let out = redact(input);
        assert!(!out.contains(secret), "{secret:?} survived in {out:?} (from {input:?})");
        assert!(out.contains(MASK), "no mask in {out:?}");
        assert_eq!(redact(&out), out, "redaction must be idempotent");
    }

    #[test]
    fn headers() {
        gone("Cookie: SID=abc123; HSID=zzz", "abc123");
        gone("cookie: foo=bar", "foo=bar");
        gone("Set-Cookie: SAPISID=s3cret; Path=/", "s3cret");
        gone("Authorization: Bearer ya29.a0AfH6SMB", "ya29");
        gone("Authorization: SAPISIDHASH 1700000000_deadbeef", "deadbeef");
        gone("X-Goog-Visitor-Id: CgtWaXNpdG9yWFla", "CgtWaXNpdG9yWFla");
        gone("x-goog-authuser: 3", "3");
        let out = redact("GET / HTTP/1.1\r\nCookie: a=b\r\nHost: x\r\n");
        assert!(out.contains("Host: x"), "other lines survive: {out}");
    }

    #[test]
    fn schemes_and_cookie_names() {
        gone("got token Bearer abcDEF123", "abcDEF123");
        gone("header was SAPISIDHASH 1_abcdef extra", "abcdef");
        gone("SAPISID=hunter2", "hunter2");
        gone("__Secure-3PSID=g.a000abc; other", "g.a000abc");
        gone("__Secure-1PSIDTS=sidts-CjIB", "sidts-CjIB");
        gone("SID=xyz", "xyz");
        gone("LOGIN_INFO=AFmmF2swR", "AFmmF2swR");
        gone("APISID=q1w2e3", "q1w2e3");
        // normal text with similar words is left alone
        assert_eq!(redact("inside=1 and resid=2 consider"), "inside=1 and resid=2 consider");
    }

    #[test]
    fn url_params() {
        let u = "https://rr3---sn-x.googlevideo.com/videoplayback?expire=1&sig=AOq0QJ8w&lsig=ABc&pot=MnQ&n=xyz&key=yt8&itag=251&signature=ZZ&visitorData=CgtABC";
        let out = redact(u);
        for s in ["AOq0QJ8w", "ABc", "MnQ", "xyz", "yt8", "ZZ", "CgtABC"] {
            assert!(!out.contains(s), "{s} in {out}");
        }
        assert!(out.contains("itag=251") && out.contains("expire=1"), "{out}");
        assert!(out.contains("googlevideo.com/videoplayback"), "{out}");
        gone("refresh_token=1//0gAbC", "1//0gAbC");
        gone("POST body access_token=ya29xyz&x=1", "ya29xyz");
        gone("?n=abc", "abc");
        gone("key=AIzaSyDemo", "AIzaSyDemo");
        // `n` inside a word is not a param
        assert_eq!(redact("when=5 plan=b"), "when=5 plan=b");
    }

    #[test]
    fn json_fields() {
        gone(r#"{"refresh_token": "1//abc", "x": 1}"#, "1//abc");
        gone(r#"{"visitorData":"CgtZZZ"}"#, "CgtZZZ");
        gone(r#"{"Cookie" : "SID=q"}"#, "SID=q");
        assert!(redact(r#"{"title":"Hello"}"#).contains("Hello"));
    }

    #[test]
    fn long_tokens() {
        let tok = "dGhpc0lzQUxvbmdCYXNlNjRUb2tlbl9XaXRoLUNoYXJzMTIzNDU2";
        assert!(tok.len() >= 40);
        gone(&format!("token {tok} end"), tok);
        // a 39-char token is not masked, a long lowercase word is not either
        let short = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b";
        assert_eq!(short.len(), 39);
        assert_eq!(redact(short), short);
        let word = "internationalizationlocalizationconsiderations";
        assert_eq!(redact(word), word);
        // video ids are 11 chars and survive
        assert_eq!(redact("playing dQw4w9WgXcQ"), "playing dQw4w9WgXcQ");
    }

    #[test]
    fn own_proxy_urls() {
        let tok = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let out = redact(&format!("http://127.0.0.1:5000/t/{tok}/dQw4w9WgXcQ"));
        assert!(!out.contains(tok), "{out}");
        assert!(out.contains("dQw4w9WgXcQ"), "{out}");
    }

    #[test]
    fn plain_text_is_untouched_and_unicode_is_fine() {
        for s in ["", "player: resolved 3 items in 120 ms", "Ünïcödé 音楽 – no secrets", "error: connection refused (os error 111)"]
        {
            assert_eq!(redact(s), s);
        }
    }

    #[test]
    fn writer_redacts_whole_event_and_can_be_disabled() {
        use std::sync::{Arc, Mutex};
        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink::default();
        {
            let mut w = RedactWriter { inner: sink.clone(), buf: Vec::new(), redact: true };
            // a secret split across two writes must still be caught
            w.write_all(b"Coo").unwrap();
            w.write_all(b"kie: SID=abc\n").unwrap();
        }
        let got = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert_eq!(got, format!("Cookie: {MASK}\n"));

        let sink = Sink::default();
        drop({
            let mut w = RedactWriter { inner: sink.clone(), buf: Vec::new(), redact: false };
            w.write_all(b"Cookie: SID=abc").unwrap();
            w
        });
        assert_eq!(sink.0.lock().unwrap().as_slice(), b"Cookie: SID=abc");
    }

    #[test]
    fn crash_note_is_single_line_and_redacted() {
        let n = crash_note("bad\nSAPISID=topsecret", "src/x.rs:1", true);
        assert!(!n.contains('\n') && !n.contains("topsecret"), "{n}");
        assert!(n.starts_with("panic at src/x.rs:1"));
        assert!(crash_note("SAPISID=topsecret", "l", false).contains("topsecret"));
    }

    #[cfg(unix)]
    #[test]
    fn tidy_keeps_newest_seven_and_tightens() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("yqt-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for i in 1..=9 {
            let p = d.join(format!("yqt.2026-01-0{i}.log"));
            std::fs::write(&p, "x").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        std::fs::write(d.join("other.txt"), "keep").unwrap();
        tidy_logs(&d, 7);
        let mut names: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names.len(), 8);
        assert!(!names.contains(&"yqt.2026-01-01.log".to_owned()) && names.contains(&"other.txt".to_owned()));
        let mode = std::fs::metadata(d.join("yqt.2026-01-09.log")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
