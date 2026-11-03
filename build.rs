// SPDX-License-Identifier: GPL-3.0-only
use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(QmlModule::new("org.kde.ytm").qml_files([
        "qml/Main.qml",
        "qml/CoverImage.qml",
        "qml/ItemsPage.qml",
        "qml/NowPlayingBar.qml",
        "qml/Playback.qml",
        "qml/NowPlayingPage.qml",
        "qml/AddToPlaylistDialog.qml",
        "qml/SettingsPage.qml",
    ]))
    .qt_module("Quick")
    .qt_module("Qml")
    .files(["src/backend.rs", "src/player.rs", "src/appconfig.rs", "src/clock.rs"])
    .build();
}
