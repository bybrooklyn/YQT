// SPDX-License-Identifier: GPL-3.0-only
use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(QmlModule::new("org.kde.ytm").qml_files([
        "qml/Main.qml",
        "qml/CoverImage.qml",
        "qml/ItemsPage.qml",
        "qml/NowPlayingBar.qml",
        "qml/PanelSwitcher.qml",
        "qml/Playback.qml",
        "qml/QueuePanel.qml",
        "qml/LyricsPanel.qml",
        "qml/NowPlayingPage.qml",
        "qml/AddToPlaylistDialog.qml",
        "qml/SettingsPage.qml",
        "qml/TransportControls.qml",
        "qml/MiniPlayer.qml",
        "qml/MiniPlayerContent.qml",
        "qml/TrayController.qml",
    ]))
    // bundled fallback tray icon (used when the desktop icon theme has no `multimedia-audio-player`)
    .qrc_resources(["qml/icons/ytm-tray.svg"])
    .qt_module("Quick")
    .qt_module("Qml")
    .files(["src/backend.rs", "src/player.rs", "src/appconfig.rs", "src/clock.rs", "src/accent.rs", "src/reporting.rs", "src/tray.rs"])
    .build();
}
