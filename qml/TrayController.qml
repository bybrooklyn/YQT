// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import dev.bybrooklyn.yqt
import org.kde.kirigami as Kirigami

// System-tray icon + menu. The tray itself and everything about its menu (which entries, labels, enabled/checked,
// the tooltip) live in Rust (`TrayMenuModel`, src/tray.rs: a StatusNotifierItem on Linux); this file only feeds it
// the playback state and routes the clicks back. Where there is no tray (`available` stays false) nothing is shown
// and nothing breaks. It deliberately does not use Qt Labs Platform, which needs Qt Widgets and aborts without it.
QtObject {
    id: ctl

    /// The mini-player window to toggle (set by Main.qml).
    property var mini: null
    /// True when a tray exists, so closing the main window can safely hide it instead of quitting.
    readonly property bool available: menuModel.available

    readonly property var win: applicationWindow()
    readonly property bool liked: Backend.likedRev >= 0 && Backend.loggedIn && Player.hasTrack && Backend.isLiked(Player.videoId)

    property TrayMenuModel menuModel: TrayMenuModel {
        onActivated: action => ctl.activate(action)
    }

    function refresh() {
        menuModel.update(win.media ? win.media.playing : false, Player.hasTrack, Backend.loggedIn, liked,
                         win.visible, ctl.mini ? ctl.mini.visible : false, Player.title, Player.artist)
    }
    // Re-evaluates whenever any of these change.
    readonly property var _deps: [win.media ? win.media.playing : false, Player.hasTrack, Backend.loggedIn, liked,
                                  win.visible, ctl.mini ? ctl.mini.visible : false, Player.title, Player.artist]
    on_DepsChanged: refresh()

    function toggleWindow() {
        if (win.visible && !(win.visibility === Window.Minimized)) {
            win.hide()
        } else {
            win.show(); win.raise(); win.requestActivate()
        }
    }

    function activate(id) {
        switch (id) {
        case "playpause": win.togglePlay(); break
        case "next": Player.next(); break
        case "previous": win.previousTrack(); break
        case "like": Backend.setLiked(Player.videoId, !liked); break
        case "window": toggleWindow(); break
        case "mini": if (ctl.mini) ctl.mini.toggle(); break
        case "quit": Qt.quit(); break
        }
    }

    Component.onCompleted: {
        refresh()
        menuModel.start()
    }
}
