// SPDX-License-Identifier: GPL-3.0-only
// A release build on Windows is a GUI app: no console window behind it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod accent;
mod appconfig;
mod audio;
// The QML faces of the audio engine live next to the other bridges (cxx-qt wants them in one directory).
mod audio_engine;
mod audio_eq;
mod auth;
mod backend;
mod clock;
mod config;
mod connectivity;
mod covercache;
mod downloads;
mod downloadsqml;
mod errors;
mod items;
mod logging;
mod migrate;
mod lyrics;
mod lyrics_qt;
mod metered;
mod models;
#[cfg(target_os = "linux")]
mod mpris;
#[cfg(not(target_os = "linux"))]
#[path = "mpris_stub.rs"]
mod mpris;
mod page;
// Keep the stub compiling (and in sync) on Linux too.
#[cfg(all(test, target_os = "linux"))]
#[path = "mpris_stub.rs"]
mod mpris_stub_check;
mod nav;
mod pagedata;
mod pages;
mod platform;
mod player;
mod proxy;
mod queuemodel;
mod reporting;
mod security;
mod session;
mod settings;
mod state;
mod tray;
mod vision;
mod ytapi;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QString, QUrl};

/// The same KDE look everywhere: `org.kde.desktop`. On Linux it comes with the system; elsewhere it is bundled next to
/// the exe (`qml/`) or found through `QML_IMPORT_PATH`, and Fusion is the fallback only when it really is absent.
fn default_style() -> String {
    const KDE: &str = "org.kde.desktop";
    if cfg!(target_os = "linux") {
        return KDE.to_owned();
    }
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
        dirs.push(exe_dir.join("qml"));
        dirs.push(exe_dir.join("../Resources/qml")); // macOS .app bundle
        dirs.push(exe_dir.join("../qml"));
    }
    for var in ["QML_IMPORT_PATH", "QML2_IMPORT_PATH"] {
        if let Some(v) = std::env::var_os(var) {
            dirs.extend(std::env::split_paths(&v));
        }
    }
    if dirs.iter().any(|d| d.join("org/kde/desktop").is_dir()) {
        KDE.to_owned()
    } else {
        tracing::warn!("org.kde.desktop style not found; falling back to Fusion (install qqc2-desktop-style)");
        "Fusion".to_owned()
    }
}

fn main() {
    state::tune_allocator(); // before any thread starts
    migrate::run(); // before anything reads the per-user folders
    if std::env::args().any(|a| a == "--print-config-path") {
        println!("{}", config::path().display());
        return;
    }
    logging::init();
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        let configured = config::get().ui.style.clone();
        let style = if !configured.is_empty() { configured } else { default_style() };
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", style);
    }
    let mut app = QGuiApplication::new();
    // QSettings (volume, shuffle, repeat) needs these; without them it silently stores nothing.
    if let Some(mut app) = app.as_mut() {
        app.as_mut().set_organization_name(&QString::from("yqt"));
        app.as_mut().set_organization_domain(&QString::from("kde.org"));
        app.as_mut().set_application_name(&QString::from("yqt"));
        // Lets KDE/Wayland match the window to org.kde.yqt.desktop and so to its icon.
        QGuiApplication::set_desktop_file_name(&QString::from("org.kde.yqt"));
    }
    let mut engine = QQmlApplicationEngine::new();
    if let Some(engine) = engine.as_mut() {
        #[cfg(feature = "harness")]
        let entry = {
            // canned pages and no stream lookups: nothing leaves the machine
            if std::env::var_os("YTM_DEMO").is_some() || std::env::args().any(|a| a == "--qml") {
                page::install_demo_source();
                player::disable_streams();
            }
            match std::env::args().skip_while(|a| a != "--qml").nth(1) {
                Some(path) => QUrl::from(&format!("file://{path}")),
                None => QUrl::from("qrc:/qt/qml/org/kde/ytm/qml/Main.qml"),
            }
        };
        #[cfg(not(feature = "harness"))]
        let entry = QUrl::from("qrc:/qt/qml/org/kde/ytm/qml/Main.qml");
        engine.load(&entry);
    }
    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
