# Security

ytm is a local desktop app that signs in to YouTube Music with browser cookies, plays audio through a
localhost proxy, and talks to a few third-party services (lyrics). This page records the threat model,
what was found in the audit, what was fixed, and what is still a risk.

## Reporting a vulnerability

Please do not open a public issue for security problems. Use GitHub's private "Report a vulnerability"
(Security tab) on the project repository, or email the maintainer listed in the repository profile.
Include the version, OS, and steps to reproduce. Never include real cookies or tokens; redact them
(`ytm`'s log files are already redacted by default, see below).

## Threat model

| Adversary | Can | We defend by |
|---|---|---|
| Other local user / local process | Read world-readable files, connect to localhost ports | Data and config dirs 0700, files 0600, atomic private writes; proxy token + Host check |
| Malicious web page in the user's browser | Send requests to `127.0.0.1:<port>`, DNS rebinding | Unguessable per-run token in the path, Host allow-list, GET/HEAD only, strict id charset |
| Malicious or compromised remote data (YouTube responses, lyrics providers, config-supplied URLs) | Return hostile JSON, URLs, header values | https-only endpoint validation, trusted-host helper, header-value sanitiser, size caps (see residual) |
| Supply chain | Malicious or vulnerable crate | `deny.toml` (advisories, licences, bans, crates.io only); `Cargo.lock` committed |
| Credential theft from disk | Read the cookie / session / logs / crash reports | File permissions, retention limits on RustyPipe reports, log redaction |

Out of scope: an attacker running as the same user (they can read the cookie file and the browser's own
profile anyway), a compromised OS, physical access.

## Findings

| ID | Severity | Description | Status |
|---|---|---|---|
| S-01 | High | Stream proxy on 127.0.0.1 served any registered stream to any local process or web page (`/<videoId>`) | Fixed: per-run 256-bit token (`/t/<token>/<id>`), constant-time compare, Host allow-list, GET/HEAD only, id regex, uniform 404; disable with `security.proxy_token=false` (legacy) |
| S-02 | Medium | DNS rebinding against the proxy | Fixed: `Host` must be `127.0.0.1:<port>` or `localhost:<port>` (403 otherwise) |
| S-03 | Medium | Data dir 0755, `session.json` 0644 (queue contents readable by other users) | Fixed: dir 0700, `write_private` (0600, temp+rename); old files tightened at startup, never loosened |
| S-04 | Medium | RustyPipe failure reports hold full response bodies (library/liked data), 0644 in a 0755 dir, never pruned | Fixed: dir 0700, pruned at startup by `security.report_retention_days` (default 7) and capped at 50 files; `0` deletes them and disables reporting |
| S-05 | Medium | Logs could leak cookies, tokens, signed stream URLs | Fixed: all log output passes `logging::redact` (cookies, Authorization/Bearer/SAPISIDHASH, SID-style cookie names, `sig`/`lsig`/`pot`/`n`/`key`/token params, visitor data, long tokens, proxy token); panic hook is redacted; logs 0600 in a 0700 dir, 7 daily files |
| S-06 | Low | Proxy: unbounded `Range` arithmetic could overflow/underflow; slow-loris on request head; hostile upstream `Content-Type` echoed into headers | Fixed: saturating math + 416, 10 s head timeout + 431 on >16 KiB, control-char content types replaced |
| S-07 | Medium | Configurable endpoint URLs (lyrics) could be `http://`, carry userinfo, or contain control chars | Mitigated: `security::validate_endpoint` written and tested; each consumer (lyrics, W3) must call it |
| S-08 | Low | QML `Qt.openUrlExternally` with arbitrary schemes (`file:`, custom handlers) | Mitigated: `security::is_safe_external_link` (https only); QML callers must use it |
| S-09 | Low | Pasted cookie text: a lone `\r` survives `auth::parse_input`'s line joining | Mitigated: the `http` crate refuses CR/LF/NUL in header values, so the request fails instead of injecting; `security::sanitize_header_value` is provided so `parse_input` can reject it up front (W8) |
| S-10 | Low | `ytapi::post` and other `resp.json()` calls read untrusted bodies without a size cap | Residual: owned by other modules; recommended cap in the W7 report |
| S-11 | Info | `unsafe` blocks: cxx-qt `get_unchecked_mut` (Qt thread only) and Windows DPAPI | Reviewed; the DPAPI block has a SAFETY comment; comments for the cxx-qt ones are listed for the owners |
| S-12 | Info | Dependency advisories: `zbus_names`, `zvariant`, `zvariant_derive` unmaintained (RUSTSEC-2026-0370/0371/0372) | Residual: merged into zbus 6; needs the zbus 5 to 6 major upgrade; recorded as `ignore` with a reason |
| S-13 | Medium | OpenSSL (`native-tls`) in the TLS stack | Residual until W8 switches to rustls; ban is prepared (commented out) in `deny.toml` |

## What changed (summary)

* `src/proxy.rs`: token, Host check, methods, id charset, head cap/timeout, safe arithmetic; tests include
  real-socket tests against a fake upstream.
* `src/state.rs`: `ensure_private_dir`, `write_private`, `tighten`, `harden_tree`, `prune_reports`; RustyPipe reporter
  configured from `security.report_retention_days`.
* `src/logging.rs`: `tracing` to stderr + rotating private file, `redact()`, redacting writer, panic hook.
* `src/security.rs`: `validate_endpoint`, `is_safe_external_link`, `is_trusted_youtube_host`, `sanitize_header_value`.
* `deny.toml`: `cargo deny check advisories licenses bans sources` passes.

## Residual risks

* The YouTube session cookie lives in `rustypipe_cache.json` (written by RustyPipe, mode 0600). Its safety rests
  on file permissions and full-disk encryption, not on an OS keyring. Anyone who can read your home directory as
  you can use the session.
* Importing cookies from a browser may trigger KWallet / Keychain / DPAPI prompts and briefly copies the cookie
  database to a private temp location.
* Lyrics providers (LRCLIB, NetEase) see the track title/artist (and duration) you play; YouTube sees what you play
  and your account.
* Redaction is pattern based: a secret in an unexpected shape may not be masked. Treat logs as sensitive anyway
  (they are 0600) and review before sharing.
* The proxy token is in URLs handed to Qt Multimedia and could appear in Qt's own debug output; it is per-run
  and useless after exit.
* Windows: permissions rely on the default per-user profile ACLs; no explicit ACL hardening is applied.
* The proxy does not limit the number of concurrent local connections.
