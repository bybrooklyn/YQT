# Contracts between workstreams

These are the shared surfaces. A workstream may *use* them freely and *implement* the ones it owns; changing a contract
means asking the integrator. Everything here exists in code (compile-checked) unless marked **planned**.

## Config (`src/config.rs`) — read-only for everyone
`config::get()` returns `&'static Config`. Sections: `network playback library lyrics youtube ui integration audio downloads
appearance keys reporting window security`. Never read the file yourself and **never call `config::get()` in unit tests**
(it creates files in the real config dir): pass the values you need as parameters, or build `Config::default()`.
QML reads the UI-relevant ones through the `AppConfig` singleton (`src/appconfig.rs`; add a property there when QML needs a new one).

## Playback (`qml/Playback.qml`) — one seam for all audio
Properties: `source, volume, autoplay` (in); `position` (raw, coarse), `smoothPosition` (per-frame, use this for progress bars and
lyrics), `duration, playing, hasSource` (out). Methods: `play() pause() stop() toggle() seek(ms)`. Signals: `loaded() ended() failed(message)`.
Only this file may touch `MediaPlayer`. The Rust audio engine replaces its inside without changing this surface.
`src/clock.rs` (`PlaybackClock`) is the smoothing rule; engines report positions through the same `report/snap/positionAt`.

## Pages (`src/pagedata.rs`) — builders produce, the controller consumes
`PageData { hero: Option<Hero>, sections: Vec<Section>, pager, has_more }`; `Section { id, title, subtitle, style, items }` with
`SectionStyle = List | NumberedList | Cards | Artists | Grid`; `Item` mirrors the loose JSON fields QML already uses
(`kind, title, subtitle, artist, videoId, browseId, thumbnail, duration, artistId, albumId, number`), camelCase on the wire.
`PageData::cleaned()` drops dead items/sections. `Item::from_value` adapts today's `items.rs` JSON.
Rules: every list row/card has a cover (the QML `CoverImage` shows a themed placeholder; never leave art blank), `wikipedia_url` must be `https`.

## Errors (`src/errors.rs`)
`AppError { kind, headline, hint, detail, retry, id }`, `ErrorKind = Network | Auth | Parse | NotFound | RateLimit | Offline | Unavailable | Storage | Config | Internal`,
`Retry = Never | Manual | Auto { after_ms, max_attempts }`. `AppError::from_rustypipe(e, signed_in)`, `.ui_string()` (`"headline"` or `"headline\n\ndetail"`,
what QML splits on `\n\n`), `.diagnostics()` ("Copy details" text). The **Auth** headline is exactly `auth::AUTH_MSG` (the backend compares for equality).
The UI shows every error through one `ErrorView` component (**planned**, queue/errors workstream).

## Audio formats (`src/vision.rs`)
`audio_formats(video_id) -> Vec<AudioFormat { itag, url, mime, codec, bitrate, content_length, duration_ms, loudness_db, sample_rate }>`,
`pick_format(&formats, Quality, Option<Codec>)`, `Quality::from_setting("best"|"normal"|"data_saver")`, `Codec = Opus | Aac | Other`.
`audio_url()` keeps its old behaviour (highest bitrate). Streams are signed for `vision::user_agent()`; fetch them with that UA.

## Cover accent (`src/accent.rs`) — stub, owned by the downloads/cover-cache stream
QML: `Accent { id: accent; source: Player.cover }` → `accent.color` (`#rrggbb`) and `accent.ready`. Until ready use `Kirigami.Theme.highlightColor`.

## Security helpers (from the security stream, `src/security.rs`, `src/state.rs`, `src/logging.rs`)
`security::validate_endpoint(url, allow_insecure)`, `is_safe_external_link(url)` (https only — route every `Qt.openUrlExternally` through it),
`is_trusted_youtube_host(host)`, `sanitize_header_value(v)`; `state::write_private / ensure_private_dir / tighten`; `logging::redact(&str)`.
The stream proxy URLs now contain a per-run token; to serve local files or covers extend `proxy::authorize()` — do not open another listener.

## Platform adapters (`src/platform/`, from the bindings stream)
`SecretStore::password(service, account_or_app)`, `DefaultBrowser::token()`, `Desktop::detect().is_plasma()`, `Notifier` (null impl).
**No `Command::new` anywhere in `src/`** (a test enforces it). Use bindings, not helper programs.

## Play reporting (`src/reporting.rs`)
QML singleton `Reporter`: `trackChanged(videoId)` on every track change (empty = nothing playing), `tick(positionMs, playing)` about once a second.

## Conventions
- No Python. Edit files with the Write/Edit tools. Temp `XDG_*` dirs for any run. Never touch the real `~/.local/share/ytm` / `~/.config/ytm`.
- Commit messages are plain: **no `Co-Authored-By` or "Generated with" lines** (the repo owner's rule).
- `just check` (fmt + clippy `-D warnings` + tests) must pass before handing work back.
