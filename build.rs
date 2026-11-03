// SPDX-License-Identifier: GPL-3.0-only
use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(QmlModule::new("org.kde.ytm").qml_files([
        "qml/Main.qml",
        "qml/CoverImage.qml",
        "qml/ItemsPage.qml",
        "qml/NowPlayingBar.qml",
        "qml/NowPlayingPage.qml",
        "qml/AddToPlaylistDialog.qml",
        "qml/SettingsPage.qml",
    ]))
    .qt_module("Quick")
    .qt_module("Qml")
    .files([
        "src/backend.rs",
        "src/player.rs",
        "src/appconfig.rs",
        "src/clock.rs",
        "src/accent.rs",
        "src/reporting.rs",
        "src/queuemodel.rs",
        "src/page.rs",
        "src/errors.rs",
        "src/nav.rs",
        "src/settings.rs",
        "src/tray.rs",
        "src/connectivity.rs",
        "src/covercache.rs",
        "src/downloadsqml.rs",
        "src/lyrics_qt.rs",
        "src/audio_engine.rs",
        "src/audio_eq.rs",
    ])
    .build();
}
