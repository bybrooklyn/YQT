// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import Qt.labs.platform as Platform
import org.kde.ytm
import org.kde.kirigami as Kirigami

// System-tray icon + menu. Everything about the menu (which entries, labels, enabled/checked) comes from Rust
// (`TrayMenuModel`, src/tray.rs); this file only renders it and routes the clicks.
// Degrades silently when the desktop has no tray (`available` is false): nothing is shown and nothing breaks.
QtObject {
    id: ctl

    /// The mini-player window to toggle (set by Main.qml).
    property var mini: null
    /// True when a tray exists, so closing the main window can safely hide it instead of quitting.
    readonly property bool available: tray.available

    // Bundled icon, used when the desktop's icon theme has no `multimedia-audio-player`.
    readonly property url fallbackIcon: "qrc:/qt/qml/org/kde/ytm/qml/icons/ytm-tray.svg"

    readonly property var win: applicationWindow()
    readonly property bool liked: { Backend.likedRev; return Backend.loggedIn && Player.hasTrack && Backend.isLiked(Player.videoId) }

    property TrayMenuModel menuModel: TrayMenuModel {}

    function refresh() {
        menuModel.update(win.media ? win.media.playing : false, Player.hasTrack, Backend.loggedIn, liked,
                         win.visible, ctl.mini ? ctl.mini.visible : false)
    }
    // Re-evaluates whenever any of these change.
    readonly property var _deps: [win.media ? win.media.playing : false, Player.hasTrack, Backend.loggedIn, liked,
                                  win.visible, ctl.mini ? ctl.mini.visible : false]
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

    property var tray: Platform.SystemTrayIcon {
        id: tray
        visible: available
        icon.name: "multimedia-audio-player"
        icon.source: ctl.fallbackIcon
        tooltip: ctl.menuModel.tooltip(Player.title, Player.artist)
        onActivated: reason => {
            if (reason === Platform.SystemTrayIcon.Trigger) ctl.toggleWindow()
            else if (reason === Platform.SystemTrayIcon.MiddleClick) ctl.activate("playpause")
        }

    }

    // The native menu is only created when a tray exists (Qt Labs Platform logs errors where there is no native menu
    // implementation, e.g. headless sessions). It is filled by the instantiator below.
    property var menuObj: null
    property Component _menuComp: Component { Platform.Menu {} }
    Component.onCompleted: {
        refresh()
        if (tray.available) {
            menuObj = _menuComp.createObject(ctl)
            tray.menu = menuObj
        }
    }

    property var _items: Instantiator {
        active: ctl.menuObj !== null
        model: ctl.menuModel.revision >= 0 ? ctl.menuModel.entryCount() : 0
        delegate: Platform.MenuItem {
            required property int index
            readonly property int rev: ctl.menuModel.revision
            separator: { rev; return ctl.menuModel.entryIsSeparator(index) }
            text: { rev; return ctl.menuModel.entryLabel(index) }
            enabled: { rev; return ctl.menuModel.entryEnabled(index) }
            checkable: { rev; return ctl.menuModel.entryCheckable(index) }
            checked: { rev; return ctl.menuModel.entryChecked(index) }
            onTriggered: ctl.activate(ctl.menuModel.entryId(index))
        }
        onObjectAdded: (index, object) => ctl.menuObj.insertItem(index, object)
        onObjectRemoved: (index, object) => { if (ctl.menuObj) ctl.menuObj.removeItem(object) }
    }
}
