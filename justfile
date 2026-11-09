# YQT – tiny YouTube Music client (Rust + Qt 6 + Kirigami)
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
    dbus-run-session -- cargo test --bin yqt the_tray_registers -- --ignored

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
    QT_ASSUME_STDERR_HAS_CONSOLE=1 QT_QPA_PLATFORM=offscreen timeout {{SECONDS}} ./target/debug/yqt 2>&1 \
        | grep -v 'ffmpeg' || true

# Install the binary and desktop entry for the current user
[linux]
install: release
    install -Dm755 target/release/yqt ~/.local/bin/yqt
    install -Dm644 org.kde.yqt.desktop ~/.local/share/applications/org.kde.yqt.desktop
    install -Dm644 assets/logo.svg ~/.local/share/icons/hicolor/scalable/apps/yqt.svg
    -gtk-update-icon-cache -q ~/.local/share/icons/hicolor 2>/dev/null

[linux]
uninstall:
    rm -f ~/.local/bin/yqt ~/.local/share/applications/org.kde.yqt.desktop ~/.local/share/icons/hicolor/scalable/apps/yqt.svg

# One-time: download Windows Qt + matching Linux host tools into .cross/ (needs pipx; cross-builds use cargo-xwin + clang-cl)
[linux]
win-cross-qt VERSION="6.10.3":
    pipx list | grep -q aqtinstall || pipx install aqtinstall
    aqt install-qt windows desktop {{VERSION}} win64_msvc2022_64 -m qtmultimedia -O .cross/qt
    aqt install-qt linux desktop {{VERSION}} linux_gcc_64 -O .cross/qt

# Build target/x86_64-pc-windows-msvc/release/yqt.exe from Linux (run `just win-cross-qt` once first)
[linux]
win-cross:
    YTM_CROSS_QT_VERSION=6.10.3 QMAKE={{justfile_directory()}}/packaging/cross/qmake-shim.sh \
        cargo xwin build --release --target x86_64-pc-windows-msvc

# Cross-build, then assemble dist/yqt-cross: yqt.exe + Qt DLLs/plugins/QML via the Windows kit's windeployqt (run under wine)
[linux]
win-cross-dist: win-cross
    rm -rf dist/yqt-cross && mkdir -p dist/yqt-cross
    cp target/x86_64-pc-windows-msvc/release/yqt.exe dist/yqt-cross/
    WINEDEBUG=-all wine .cross/qt/6.10.3/msvc2022_64/bin/windeployqt6.exe --release --no-translations --no-compiler-runtime --qmldir qml dist/yqt-cross/yqt.exe

# Windows (run these from PowerShell; Qt + Kirigami come from KDE Craft, default C:\CraftRoot, override with YTM_QT_ROOT)
# One-time machine setup: Rust, MSVC Build Tools, just, KDE Craft with Qt 6 + Kirigami (long: builds/downloads several GB)
[windows]
win-setup:
    powershell -NoProfile -ExecutionPolicy Bypass -File packaging/windows/setup.ps1

# Check the Windows toolchain and Qt/Kirigami install, and say what is missing
[windows]
win-doctor:
    powershell -NoProfile -ExecutionPolicy Bypass -File packaging/windows/doctor.ps1

# Optimized Windows build against Craft's Qt
[windows]
win-build:
    powershell -NoProfile -ExecutionPolicy Bypass -File packaging/windows/with-env.ps1 cargo build --release

# Run the optimized build with the Qt/Kirigami environment set
[windows]
win-run *ARGS:
    powershell -NoProfile -ExecutionPolicy Bypass -File packaging/windows/with-env.ps1 cargo run --release -- {{ARGS}}

# Tests with the Qt/Kirigami environment set
[windows]
win-test:
    powershell -NoProfile -ExecutionPolicy Bypass -File packaging/windows/with-env.ps1 cargo test

# EXPERIMENTAL – untested: build, then bundle yqt.exe + Qt + Kirigami into dist\yqt-windows and a .zip
[windows]
package-windows: win-build
    powershell -NoProfile -ExecutionPolicy Bypass -File packaging/windows/package.ps1

# EXPERIMENTAL – untested: build a macOS .app bundle and let macdeployqt pull in Qt
[macos]
package-macos: release
    mkdir -p dist/yqt.app/Contents/MacOS dist/yqt.app/Contents/Resources
    cp target/release/yqt dist/yqt.app/Contents/MacOS/yqt
    cp packaging/macos/Info.plist dist/yqt.app/Contents/Info.plist
    macdeployqt dist/yqt.app -qmldir=qml

# Build bundled zips for Linux and Windows into dist/ (macOS: build on a Mac, see package-macos)
[linux]
export: export-linux export-windows
    @ls -lh dist/*.zip

# dist/yqt-linux-x86_64.zip: the release build plus the Qt, Kirigami and Breeze files it needs, run with ./yqt.sh
[linux]
export-linux:
    cargo build --release
    packaging/linux/bundle.sh target/release/yqt

# dist/yqt-windows-x86_64.zip: cross-compiled yqt.exe plus Qt DLLs/plugins/QML (needs `just win-cross-qt` once, cargo-xwin, wine)
[linux]
export-windows: win-cross-dist
    rm -rf dist/yqt-windows dist/yqt-windows-x86_64.zip
    mv dist/yqt-cross dist/yqt-windows
    cd dist && zip -qr yqt-windows-x86_64.zip yqt-windows

# Remove build output
clean:
    cargo clean
