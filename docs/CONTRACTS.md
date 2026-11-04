# Contracts between workstreams

These are the shared surfaces. A workstream may *use* them freely and *implement* the ones it owns; changing a contract
means asking the integrator. Everything here exists in code (compile-checked) unless marked **planned**.

## Config (`src/config.rs`) — read-only for everyone
`config::current()` returns the live `Config` (a clone that follows every change made in Settings); read it **per use** so a setting applies
without a restart. `config::get()` is the startup snapshot, only for things that really are fixed at startup (the proxy port/token, logging).
Sections: `network playback library lyrics youtube ui integration audio downloads appearance keys reporting window security`.
Never read the file yourself and **never call `config::get()`/`current()` in unit tests** (it creates files in the real config dir): pass the
values you need as parameters, or build `Config::default()`. Settings writes go through the `AppSettings` singleton (`src/settings.rs`).
QML reads the UI-relevant ones through the `AppConfig` singleton (`src/appconfig.rs`, updated live; add a property there when QML needs a new one;
it includes the `audio*` properties for the engine and the `lyrics*` ones for the lyric player).

## Playback (`qml/Playback.qml`) — one seam for all audio
Properties: `source, nextSource, volume, autoplay` (in); `position` (raw, coarse), `smoothPosition` (per-frame, use this for progress bars and
lyrics), `duration, playing, hasSource` (out). Methods: `play() pause() stop() toggle() seek(ms)`. Signals: `loaded() ended() failed(message)`.
`Main.qml` owns the one `Playback` instance (`applicationWindow().media`) and exposes `smoothPosition` and `seek(ms)`; read those, never the raw player.
A `Loader` picks the backend from `audio.engine`: `RustPlayback.qml` (the pure-Rust engine, `src/audio/`, exposed as the `AudioEngine` and
`Equalizer` QML types in `src/audio_engine.rs` / `src/audio_eq.rs`) or `QtPlayback.qml` (the only file that may touch `MediaPlayer`).
`Player.nextSource` is the already-resolved URL of the following track (empty on shuffle / repeat-one); the Rust engine decodes it ahead for
gapless playback and crossfade. `Player.resolve` serves a downloaded song from disk (via the proxy) before it asks YouTube.
`src/clock.rs` (`PlaybackClock`) is the smoothing rule; engines report positions through the same `report/snap/positionAt`.

## Downloads, covers, connectivity (`src/downloads/`, `src/downloadsqml.rs`, `src/covercache.rs`, `src/connectivity.rs`, `src/metered.rs`)
QML singletons: `Downloads` (`usedBytes capBytes activeCount paused pauseReason offline meteredNote`; `download(item) downloadAll(items) remove(id)
retry(id) isDownloaded(id) state(id) percent(id) pin keepOffline isKeptOffline pauseAll(bool) list() played(id) setSmartInputs(kind, items) localUrl(id)`;
signals `changed`, `progress`), `Covers.url(source)` (cached, served by the proxy so covers work offline), `Connectivity.online` / `checkNow()`.
Smart downloads are opt-in; the rules' inputs (`liked`, `recent`, `queue`) are fed from Rust (`Player::load`, `backend::load_liked`).

## Pages (`src/pagedata.rs`) — builders produce, the controller consumes
`PageData { hero: Option<Hero>, sections: Vec<Section>, pager, has_more }`; `Section { id, title, subtitle, style, items }` with
`SectionStyle = List | NumberedList | Cards | Artists | Grid`; `Item` mirrors the loose JSON fields QML already uses
(`kind, title, subtitle, artist, videoId, browseId, thumbnail, duration, artistId, albumId, number`), camelCase on the wire.
`PageData::cleaned()` drops dead items/sections. `Item::from_value` adapts today's `items.rs` JSON.
**Loading seam (`src/pages/mod.rs`)** — the only entry point the page controller calls:
`pages::load(&Request { target, query, filter }) -> Result<PageData, AppError>`, `pages::more(pager) -> Result<PageData, AppError>`
(only the *new* items), `pages::home_loaders() -> Vec<HomeRow { slot, load }>` (run concurrently, show rows as they finish).
The UI's `ItemsPage` still uses the legacy JSON loaders in `backend.rs` (`Backend.browse`); for album/playlist/artist pages those add `hero`,
`shelves` and `radio` fields built by `src/pages/{album,playlist,artist}.rs` (`with_page_data`), so the page builders stay the single source of truth. Targets: `home`, `explore`, `album:ID`, `playlist:ID` (`LIKED` = liked music), `artist:ID`, `library:<playlists|albums|artists|songs|history>`.
Rules: every list row/card has a cover (the QML `CoverImage` shows a themed placeholder; never leave art blank), `wikipedia_url` must be `https`.

## Errors (`src/errors.rs`)
`AppError { kind, headline, hint, detail, retry, id }`, `ErrorKind = Network | Auth | Parse | NotFound | RateLimit | Offline | Unavailable | Storage | Config | Internal`,
`Retry = Never | Manual | Auto { after_ms, max_attempts }`. `AppError::from_rustypipe(e, signed_in)`, `.ui_string()` (`"headline"` or `"headline\n\ndetail"`,
what QML splits on `\n\n`), `.diagnostics()` ("Copy details" text). The **Auth** headline is exactly `auth::AUTH_MSG` (the backend compares for equality).
The UI shows errors inline (the page's placeholder message with Retry); `ErrorCenter` (Rust) carries severities and offline state.

## Audio formats (`src/vision.rs`)
`audio_formats(video_id) -> Vec<AudioFormat { itag, url, mime, codec, bitrate, content_length, duration_ms, loudness_db, sample_rate }>`,
`pick_format(&formats, Quality, Option<Codec>)`, `Quality::from_setting("best"|"normal"|"data_saver")`, `Codec = Opus | Aac | Other`.
`audio_url()` keeps its old behaviour (highest bitrate). Streams are signed for `vision::user_agent()`; fetch them with that UA.

## Cover accent (`src/accent.rs`)
QML: `Accent { id: accent; source: Player.cover }` → `accent.color` (`#rrggbb`) and `accent.ready` (dominant saturated colour of the cover,
computed in Rust). Until ready use `Kirigami.Theme.highlightColor`.

## Lyrics (`src/lyrics/`, `src/lyrics_qt.rs`)
Providers (LRCLIB, NetEase, YouTube plain) run concurrently; synced lyrics always beat plain ones. QML type `Lyrics`: `state`
(`loading | synced | plain | unavailable | failed`), `source`, `plain`, `errorText`, `errorDetail`, `linesJson`; `request(...)`, `retry()`.
`unavailable` is not an error: the panel just says "Lyrics unavailable". The source credit is the last line of the lyrics.

## Tray and mini-player (`src/tray.rs`, `qml/MiniPlayer.qml`, `qml/TrayController.qml`)
`TrayMenuModel` (labels/enabled flags derived from playback state, unit-tested; on Linux it also owns the tray itself, a StatusNotifierItem
over D-Bus via `ksni`: `start()`, `update(...)`, signal `activated(action)`, property `available`). It deliberately does **not** use
`Qt.labs.platform`, which needs Qt Widgets and aborts under a `QGuiApplication`. `MiniPlayer.qml` is a plain window of stock controls that
reads the main window's state. Config: `window.close_to_tray`, `window.mini_always_on_top`, `keys.mini_player`.

## UI rule (from the owner)
The look is the original native KDE one: **stock Kirigami / QtQuick.Controls components only**, no hand-drawn controls, custom palettes or
gradients. Improve the existing QML files in place with small diffs; render every change before committing. A hand-built redesign was
rejected once already (see `backup/before-ui-revert`).

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
