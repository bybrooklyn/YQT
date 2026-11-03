<p align="center"><img src="assets/banner.svg" alt="ytm: a native YouTube Music client" width="100%"></p>

# ytm

A native YouTube Music client written in **Rust**, **Qt 6** ([cxx-qt](https://github.com/KDAB/cxx-qt)) and **Kirigami**.
No browser engine, no Electron, no FFmpeg, no system OpenSSL: networking is rustls and the optional audio engine is pure Rust.

## Features

- **Browse and play:** Home, Explore, Library, search, albums, playlists and artists; queue, shuffle, repeat, radio.
- **Lyrics:** [LRCLIB](https://lrclib.net), NetEase and YouTube's plain lyrics are asked in parallel and line-synced lyrics always win. Each provider can be switched off; they only ever receive title, artist and duration.
- **Sign-in:** Settings → *Import from <browser>* (Chrome, Chromium, Brave, Vivaldi, Edge, Opera, Firefox, Zen, LibreWolf, Waterfox) or paste cookies. Enables Library, Liked music, History, personalised Home, likes and add-to-playlist.
- **Desktop integration:** MPRIS media keys on Linux; the queue, track and position are restored on launch.

Working on the Rust side but **not yet in the interface** (they get added to the original look one at a time): downloads and offline playback, the pure-Rust audio engine with a 10-band equalizer, the mini-player and system tray, and reporting plays back to YouTube.

## Build and run

```sh
just run        # or: cargo run
just check      # fmt + clippy + tests
just deny       # licences, advisories, bans (OpenSSL is banned)
just live       # live checks against YouTube / LRCLIB (network)
just            # list all recipes
```

Needs Rust, Qt 6 (base, declarative, and multimedia for the default player), Kirigami and, on Linux, qqc2-desktop-style and ALSA (for the Rust audio engine).
Kirigami on Windows and macOS comes from [KDE Craft](https://community.kde.org/Craft).

## How it works

- **Data:** [RustyPipe](https://github.com/TeamPiped/rustypipe) (InnerTube client) for search, browse, library and history.
- **Streams:** InnerTube's visionOS client (`src/vision.rs`), served through a small token-protected localhost proxy (`src/proxy.rs`). Settings → *Check streams* tests the route.
- **Logic lives in Rust**: list models, page controllers, paging and caching, errors, downloads, lyrics ranking, settings. QML is the view.

## Configuration

Everything tunable lives in `config.toml` (`just config` prints its path; it is created with the defaults on first run), and most of it is also in the Settings page.
Invalid values are ignored with a warning; the app never refuses to start because of this file.

## Privacy and security

See [SECURITY.md](SECURITY.md) for the threat model. In short: your cookies stay on your machine (private file permissions) and are only sent to YouTube; lyrics providers see what you play; downloads are stored unencrypted in the app's folder, so keep them for personal offline listening.

## Platforms

| | Linux | macOS | Windows |
|---|---|---|---|
| Builds & runs | ✅ developed and tested here | ⚠️ not built | ⚠️ not built |
| Sign-in: paste cookies | ✅ | ✅ | ✅ |
| Sign-in: Firefox-family import | ✅ | ✅ (logic tested) | ✅ (logic tested) |
| Sign-in: Chromium-family import | ✅ KWallet / libsecret | ✅ Keychain (logic tested) | ⚠️ DPAPI + AES-GCM; **not** Chrome/Edge/Brave 127+ (app-bound encryption; paste cookies or use Firefox); close the browser first |
| Media keys (MPRIS) | ✅ | ❌ not implemented | ❌ not implemented |
| Packaging | `just install` | `just package-macos` (experimental) | `just package-windows` (experimental) |

"Logic tested" means path discovery, key derivation and decryption are unit-tested with synthetic data on Linux, and the sign-in code is type-checked for
`x86_64-pc-windows-{msvc,gnu}` and `aarch64-apple-darwin`. The real macOS Keychain and Windows DPAPI calls, the Qt/Kirigami build on those systems and their CI jobs have **not** been run.

## Known gaps

- The Rust audio engine is tested against reference decoders, but nobody has listened to it for long yet, which is why Qt Multimedia is still the default.
- The *Theme* setting (light / dark) is honoured only by styles that follow Qt's colour-scheme request; the KDE desktop style follows your Plasma colour scheme instead.
- NetEase lyrics use an unofficial API and may stop working.

## License

GPL-3.0-only (see [LICENSE](LICENSE)). RustyPipe, which this app builds on, is also GPL-3.0.
