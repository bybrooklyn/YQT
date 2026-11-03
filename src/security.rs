// SPDX-License-Identifier: GPL-3.0-only
//! Pure input-validation helpers shared by the rest of the app (no I/O, no Qt).
//!
//! * [`validate_endpoint`] – configurable service URLs (lyrics providers…): https only, no userinfo.
//! * [`is_safe_external_link`] – what QML may hand to `Qt.openUrlExternally`.
//! * [`is_trusted_youtube_host`] – host allow-list for URLs that came from YouTube responses.
//! * [`sanitize_header_value`] – refuse CR/LF/NUL before anything is put in an HTTP header.
#![allow(dead_code)] // consumed by other workstreams (lyrics, auth, QML bridge)

use reqwest::Url;

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost") || h == "127.0.0.1" || h == "::1"
}

/// `scheme://host…` written out literally (no backslashes, no empty authority).
fn has_explicit_host(url: &str) -> bool {
    matches!(url.split_once("://"), Some((_, rest)) if !rest.starts_with(['/', '?', '#']) && !url.contains('\\'))
}

/// Validate a configured service endpoint and return it normalised.
///
/// Accepts `https://…`; `http://` only for loopback hosts, or anywhere when `allow_insecure`.
/// Rejects userinfo (`user:pass@`), control characters, a missing host and every other scheme.
pub fn validate_endpoint(url: &str, allow_insecure: bool) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("empty URL".into());
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("URL contains control characters or whitespace".into());
    }
    // The URL parser is lenient (`https:///x`, `https:\\x`, `https:/x` all gain a host); we are not.
    if !has_explicit_host(url) {
        return Err("URL has no host".into());
    }
    let parsed = Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("URLs with embedded credentials are not allowed".into());
    }
    let host = parsed.host_str().filter(|h| !h.is_empty()).ok_or("URL has no host")?;
    match parsed.scheme() {
        "https" => {}
        "http" if allow_insecure || is_loopback_host(host) => {}
        "http" => return Err("plain http is only allowed for localhost (see security.allow_insecure_urls)".into()),
        other => return Err(format!("unsupported scheme `{other}`")),
    }
    Ok(parsed.to_string())
}

/// Links that may be opened in the user's browser: `https` with a host, no userinfo, no control chars.
pub fn is_safe_external_link(url: &str) -> bool {
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) || !has_explicit_host(url) {
        return false;
    }
    match Url::parse(url) {
        Ok(u) => u.scheme() == "https" && u.host_str().is_some_and(|h| !h.is_empty()) && u.username().is_empty() && u.password().is_none(),
        Err(_) => false,
    }
}

const TRUSTED_SUFFIXES: [&str; 5] = ["youtube.com", "google.com", "googlevideo.com", "ytimg.com", "googleusercontent.com"];

/// True for the listed Google/YouTube domains and their subdomains (exact match or a proper `.suffix`).
pub fn is_trusted_youtube_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    TRUSTED_SUFFIXES.iter().any(|s| host == *s || host.strip_suffix(s).is_some_and(|rest| rest.ends_with('.') && rest.len() > 1))
}

/// Return the value if it is safe to put in an HTTP header (no CR, LF or NUL); surrounding whitespace is trimmed.
pub fn sanitize_header_value(v: &str) -> Result<String, String> {
    if v.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
        return Err("header value contains CR, LF or NUL".into());
    }
    Ok(v.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_accepts_https_and_loopback_http() {
        assert_eq!(validate_endpoint("https://lrclib.net/api", false).unwrap(), "https://lrclib.net/api");
        assert!(validate_endpoint("http://127.0.0.1:8080/x", false).is_ok());
        assert!(validate_endpoint("http://localhost/x", false).is_ok());
        assert!(validate_endpoint("http://[::1]:9/x", false).is_ok());
        assert!(validate_endpoint("  https://a.example/ ", false).is_ok());
    }

    #[test]
    fn endpoint_rejects_insecure_unless_allowed() {
        assert!(validate_endpoint("http://lrclib.net/api", false).is_err());
        assert!(validate_endpoint("http://lrclib.net/api", true).is_ok());
    }

    #[test]
    fn endpoint_rejects_bad_schemes_hosts_userinfo_controls() {
        for bad in [
            "",
            "ftp://x.example/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://",
            "https:///path",
            "https:/example.com",
            "https:\\\\example.com",
            "https://?x",
            "https://user:pw@example.com/",
            "https://user@example.com/",
            "https://exa\nmple.com/",
            "https://example.com/\r\nX: y",
            "https://exa mple.com/",
            "not a url",
        ] {
            assert!(validate_endpoint(bad, true).is_err(), "{bad:?} must be rejected");
        }
        // lookalike: the host is `evil.io`, the userinfo trick must not pass
        assert!(validate_endpoint("https://lrclib.net@evil.io/", false).is_err());
    }

    #[test]
    fn external_links_are_https_only() {
        assert!(is_safe_external_link("https://en.wikipedia.org/wiki/Foo"));
        for bad in [
            "http://example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "https://u:p@example.com/",
            "https://x/\n",
            "",
            "ms-msdt:/id",
            "https://",
        ] {
            assert!(!is_safe_external_link(bad), "{bad:?}");
        }
    }

    #[test]
    fn trusted_hosts() {
        for ok in [
            "youtube.com",
            "music.youtube.com",
            "rr1---sn-abc.googlevideo.com",
            "i.ytimg.com",
            "lh3.googleusercontent.com",
            "accounts.google.com",
            "YouTube.COM",
            "music.youtube.com.",
        ] {
            assert!(is_trusted_youtube_host(ok), "{ok}");
        }
        for bad in [
            "evilyoutube.com",
            "youtube.com.evil.io",
            "notgoogle.com",
            "google.com.evil.io",
            "",
            "com",
            ".youtube.com",
            "youtube.com@evil.io",
            "evil.io/youtube.com",
            "xgooglevideo.com",
        ] {
            assert!(!is_trusted_youtube_host(bad), "{bad}");
        }
    }

    #[test]
    fn header_values_reject_injection() {
        assert_eq!(sanitize_header_value("  SID=abc; HSID=def ").unwrap(), "SID=abc; HSID=def");
        for bad in ["a\r\nX-Evil: 1", "a\nb", "a\rb", "a\0b", "\r", "x\u{0}"] {
            assert!(sanitize_header_value(bad).is_err(), "{bad:?}");
        }
    }
}
