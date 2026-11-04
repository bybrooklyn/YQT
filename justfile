# ytm – tiny YouTube Music client (Rust + Qt 6 + Kirigami)
# Run `just` to list recipes. Needs: just, a Rust toolchain, Qt 6 (base, declarative, multimedia) and Kirigami.

set shell := ["bash", "-euo", "pipefail", "-c"]
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

default:
    @just --list

# Debug build
build:
    cargo build

# Optimized build
release:
    cargo build --release

# Run the app (debug)
run *ARGS:
    cargo run -- {{ARGS}}

# Run the app (optimized)
run-release *ARGS:
    cargo run --release -- {{ARGS}}

# Unit tests
test:
    cargo test

# Type-check the Windows and macOS platform modules against the real target APIs (needs the rustup targets)
check-platforms:
    cd tests/xcheck && CARGO_TARGET_DIR=../../target/xcheck cargo check --target x86_64-pc-windows-gnu
    cd tests/xcheck && CARGO_TARGET_DIR=../../target/xcheck cargo check --target x86_64-pc-windows-msvc
    cd tests/xcheck && CARGO_TARGET_DIR=../../target/xcheck cargo check --target aarch64-apple-darwin

# The system tray against a fake tray host on a private D-Bus session (registration, icon, menu, clicks)
tray-test:
    dbus-run-session -- cargo test --bin ytm the_tray_registers -- --ignored

# QML behaviour tests with real synthesised mouse events (queue drag-to-reorder), headless
qml-test:
    QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software QT_QUICK_CONTROLS_STYLE=org.kde.desktop /usr/lib/qt6/bin/qmltestrunner -input tests/qml

# Live checks against YouTube / LRCLIB (needs network): stream route, search, pagination, lyrics
live:
    cargo test -- --ignored --nocapture live_ vision_route

# Which browsers can sign-in import from on this machine?
browsers:
    cargo test -- --ignored --nocapture detected_browsers

# Format, lint and test – run before committing
check: fmt-check clippy test

# Dependency audit: security advisories, licences, banned crates (OpenSSL), registries
deny:
    cargo deny check advisories licenses bans sources

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

clippy:
    cargo clippy --all-targets -- -D warnings

# Print where the settings file lives (created with defaults on first run)
config:
    cargo run --quiet -- --print-config-path

# Lint the QML files (Linux/macOS)
[unix]
qmllint:
    qmllint -I /usr/lib/qt6/qml qml/*.qml

# Start the app headless (no window on your screen) for a few seconds and show Qt warnings
[unix]
smoke SECONDS="8":
    QT_ASSUME_STDERR_HAS_CONSOLE=1 QT_QPA_PLATFORM=offscreen timeout {{SECONDS}} ./target/debug/ytm 2>&1 \
        | grep -v 'graphics scene\|DrawerHandle\|Binding loop\|ffmpeg' || true

# Install the binary and desktop entry for the current user
[linux]
install: release
    install -Dm755 target/release/ytm ~/.local/bin/ytm
    install -Dm644 org.kde.ytm.desktop ~/.local/share/applications/org.kde.ytm.desktop

[linux]
uninstall:
    rm -f ~/.local/bin/ytm ~/.local/share/applications/org.kde.ytm.desktop

# EXPERIMENTAL – untested: bundle Qt + QML modules next to ytm.exe (add Kirigami's QML dir with --qmldir/-qmlimport as needed)
[windows]
package-windows: release
    New-Item -ItemType Directory -Force dist | Out-Null
    Copy-Item target/release/ytm.exe dist/
    windeployqt --qmldir qml --release dist/ytm.exe

# EXPERIMENTAL – untested: build a macOS .app bundle and let macdeployqt pull in Qt
[macos]
package-macos: release
    mkdir -p dist/ytm.app/Contents/MacOS dist/ytm.app/Contents/Resources
    cp target/release/ytm dist/ytm.app/Contents/MacOS/ytm
    cp packaging/macos/Info.plist dist/ytm.app/Contents/Info.plist
    macdeployqt dist/ytm.app -qmldir=qml

# Remove build output
clean:
    cargo clean
