# ytm

A tiny native YouTube Music client: Rust + Qt 6 (cxx-qt) + Kirigami.

- **Data:** [RustyPipe](https://github.com/TeamPiped/rustypipe) (InnerTube client) for search, browse, library and history.
- **Streams:** InnerTube's visionOS client (`src/vision.rs`), served to Qt Multimedia through a small localhost proxy (`src/proxy.rs`). Settings → *Check streams* tests that the route is healthy.
- **UI:** QML with Kirigami (follows your Plasma theme on Linux).
- **Sign-in:** Settings → *Import from <browser>* (Chrome, Chromium, Brave, Vivaldi, Edge, Opera, Firefox, Zen, LibreWolf, Waterfox) or paste cookies. Enables Library, Liked music, History, personalized Home, likes and add-to-playlist.
- **Lyrics:** time-synced via [LRCLIB](https://lrclib.net) (sends only title, artist and duration; can be turned off), falling back to YouTube Music's plain lyrics.
- **Desktop integration:** MPRIS media keys / Plasma widget (Linux), and the queue, track and position are restored on launch.

```sh
just run        # or: cargo run
just check      # fmt + clippy + tests
just live       # live checks against YouTube / LRCLIB (network)
just            # list all recipes
```

Needs Qt 6 (base, declarative, multimedia), Kirigami and, on Linux, qqc2-desktop-style.

## Configuration

Everything tunable lives in `config.toml` (`just config` prints its path; it is created with the defaults on first run):
timeouts, stream chunk size and cache lifetime, list sizes, lyrics provider, and the YouTube client identities that go stale when
YouTube changes things. Invalid values are ignored with a warning on stderr – the app never refuses to start because of this file.

## Platforms

| | Linux | macOS | Windows |
|---|---|---|---|
| Builds & runs | ✅ developed and tested here | ⚠️ not built | ⚠️ not built |
| Sign-in: paste cookies | ✅ | ✅ | ✅ |
| Sign-in: Firefox-family import | ✅ | ✅ (logic tested) | ✅ (logic tested) |
| Sign-in: Chromium-family import | ✅ KWallet / libsecret | ✅ Keychain (logic tested) | ⚠️ DPAPI + AES-GCM; **not** Chrome/Edge/Brave 127+ (app-bound encryption – paste cookies or use Firefox); close the browser first |
| Media keys (MPRIS) | ✅ | ❌ not implemented | ❌ not implemented |
| Packaging | `just install` | `just package-macos` (experimental) | `just package-windows` (experimental) |

"Logic tested" means the path discovery, key derivation and decryption are unit-tested with synthetic data on Linux (including values cross-checked
against OpenSSL), and the whole sign-in code is type-checked for `x86_64-pc-windows-{msvc,gnu}` and `aarch64-apple-darwin`.
The actual macOS Keychain and Windows DPAPI calls, the Qt/Kirigami build on those systems, and the CI jobs for them have **not** been run.
Kirigami on Windows/macOS comes from [KDE Craft](https://community.kde.org/Craft).

## Layout

`src/backend.rs` (search/browse/library/lyrics/login → JSON via a `done` signal), `src/player.rs` (queue, radio, shuffle/repeat, stream resolve),
`src/session.rs` (saved session), `src/config.rs` + `src/appconfig.rs` (settings), `src/auth/` (cookie sign-in: `detect`, `firefox`, `chromium`),
`src/ytapi.rs` (like / add-to-playlist), `src/lyrics.rs`, `src/mpris.rs` (+ `mpris_stub.rs`), `src/items.rs`, `src/proxy.rs`, `qml/` (UI).

## License

GPL-3.0-only (see [LICENSE](LICENSE)). RustyPipe, which this app builds on, is also GPL-3.0.
