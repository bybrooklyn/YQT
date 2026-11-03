// SPDX-License-Identifier: GPL-3.0-only
mod accent;
mod appconfig;
mod auth;
mod backend;
mod clock;
mod config;
mod errors;
mod items;
mod logging;
mod lyrics;
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
mod pagedata;
mod pages;
mod platform;
mod player;
mod proxy;
mod queuemodel;
mod reporting;
mod security;
mod session;
mod state;
mod vision;
mod ytapi;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QString, QUrl};

fn main() {
    if std::env::args().any(|a| a == "--print-config-path") {
        println!("{}", config::path().display());
        return;
    }
    logging::init();
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        let configured = config::get().ui.style.clone();
        let style = if !configured.is_empty() {
            configured
        } else if cfg!(target_os = "linux") {
            "org.kde.desktop".to_owned() // follows the Plasma theme
        } else {
            "Fusion".to_owned() // ships with Qt everywhere
        };
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", style);
    }
    let mut app = QGuiApplication::new();
    // QSettings (volume, shuffle, repeat) needs these; without them it silently stores nothing.
    if let Some(mut app) = app.as_mut() {
        app.as_mut().set_organization_name(&QString::from("ytm"));
        app.as_mut().set_organization_domain(&QString::from("kde.org"));
        app.as_mut().set_application_name(&QString::from("ytm"));
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
