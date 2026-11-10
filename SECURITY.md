# Security

YQT is a local desktop app that signs in to YouTube Music with browser cookies, plays audio through a
localhost proxy, and talks to a few third-party services (lyrics). This page records the threat model,
what was found in the audit, what was fixed, and what is still a risk.

## Reporting a vulnerability

Please do not open a public issue for security problems. Use GitHub's private "Report a vulnerability"
(Security tab) on the project repository, or email the maintainer listed in the repository profile.
Include the version, OS, and steps to reproduce. Never include real cookies or tokens; redact them
(`YQT`'s log files are already redacted by default, see below).

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
| S-07 | Medium | Configurable endpoint URLs (lyrics) could be `http://`, carry userinfo, or contain control chars | Fixed: `security::validate_endpoint` is called by the lyrics providers and the Settings page |
| S-08 | Low | QML `Qt.openUrlExternally` with arbitrary schemes (`file:`, custom handlers) | Mitigated: `security::is_safe_external_link` (https only); QML callers must use it |
| S-09 | Low | Pasted cookie text: a lone `\r` survives `auth::parse_input`'s line joining | Mitigated: the `http` crate refuses CR/LF/NUL in header values, so the request fails instead of injecting; `security::sanitize_header_value` is provided so `parse_input` can reject it up front (W8) |
| S-10 | Low | `ytapi::post` and other `resp.json()` calls read untrusted bodies without a size cap | Residual: owned by other modules; recommended cap in the W7 report |
| S-11 | Info | `unsafe` blocks: cxx-qt `get_unchecked_mut` (Qt thread only) and Windows DPAPI | Reviewed; the DPAPI block has a SAFETY comment; comments for the cxx-qt ones are listed for the owners |
| S-12 | Info | Dependency advisories: `zbus_names`, `zvariant`, `zvariant_derive` unmaintained (RUSTSEC-2026-0370/0371/0372) | Residual: merged into zbus 6; needs the zbus 5 to 6 major upgrade; recorded as `ignore` with a reason |
| S-13 | Medium | OpenSSL (`native-tls`) in the TLS stack | Fixed: reqwest and RustyPipe use rustls with bundled roots; OpenSSL and native-tls are banned in `deny.toml` |
| S-14 | Medium | `youtube.music_origin` in `config.toml` could point anywhere, and the session cookie, SAPISIDHASH and Origin are sent to it | Fixed: `ytapi::trusted_origin` refuses anything that is not an https YouTube/Google host; tested |
| S-15 | Low | The proxy re-requested the same range forever on an empty upstream answer, and could relay more than its declared length | Fixed: a pass with no progress ends the response; bytes are capped at the declared range |
| S-16 | Low | The panic hook handed the raw payload to the default hook, bypassing log redaction | Fixed: the hook prints only the redacted note |
| S-17 | Low | The stale-file sweep removed any `*.part` in the downloads folder (e.g. a browser's) | Fixed: only this app's `<id>.<ext>.part` names |
| S-18 | Low | A stream-check error embedded the signed stream URL in the Settings UI | Fixed: `without_url()` |

| S-19 | Low | Stream URLs from a YouTube response were fetched without checking the host | Fixed: only https Google/YouTube media hosts (`vision::is_media_url`); tested, and checked live against public videos |
| S-20 | Low | Cover downloads followed redirects to any host (the https check ran after the request) | Fixed: a dedicated client whose redirect policy applies the same rule to every hop |
| S-21 | Low | API and watch-page bodies were read without a size cap | Fixed: `state::read_limited` (8 MiB) |
| S-22 | Low | A failed read of `config.toml` followed by a Settings change could save the template over the user's settings | Fixed: only a missing file falls back to the template, at start-up as well as in Settings; any other read error leaves the file alone |

| S-23 | Low | `serve_file` checked a path and then opened it: in a shared, writable downloads folder it could be swapped for a symlink | Fixed: after opening, the file must still be the same regular file the path names (never a symlink); tested |
| S-24 | Low | MPRIS `SetPosition` ignored the track id | Fixed: only a request naming the track that is playing is honoured, as the spec requires; tested |
| S-25 | Low | Two simultaneous cookie imports shared one temporary folder, and a killed import left a plaintext cookie-DB copy behind | Fixed: one private folder per import; stale ones are swept |
| S-26 | Low | Without a home/config folder the app fell back to `.` and would store the cookie and logs in the current directory | Fixed: it refuses to start with a clear message; tested |
| S-27 | Low | The proxy had no limit on simultaneous connections and never pruned its URL registry | Fixed: 64 connections, 512 registrations (oldest dropped) |

Nothing from the audit is open. What remains is outside the code: the Windows (DPAPI) and macOS (Keychain) paths are reviewed and type-checked but have never run, and a local process running as the same user can read anything the app can.

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
* The proxy serves at most 64 connections at once (S-27), but the limit is shared: a local process needs no token to
  open connections, and each one may sit idle for the 10 s head timeout, so by reconnecting it can keep the slots
  busy and stall playback for as long as it runs. It gains no access to any stream, file or cover that way.
