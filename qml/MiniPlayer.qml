// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Window
import QtCore
import org.kde.ytm
import org.kde.kirigami as Kirigami

// A compact, frameless, optionally always-on-top player. Shown on demand (tray, shortcut, `root.toggleMiniPlayer()`).
// It reads the very same Playback/Player state as the main window, so it cannot drift from it.
Window {
    id: win

    flags: Qt.Tool | Qt.FramelessWindowHint | (AppConfig.miniAlwaysOnTop ? Qt.WindowStaysOnTopHint : 0)
    color: "transparent"
    title: Player.hasTrack ? qsTr("%1 — Mini-player").arg(Player.title) : qsTr("Mini-player")
    width: Kirigami.Units.gridUnit * 22
    height: Kirigami.Units.gridUnit * 7
    minimumWidth: Kirigami.Units.gridUnit * 14
    minimumHeight: Kirigami.Units.gridUnit * 5
    maximumHeight: Kirigami.Units.gridUnit * 14

    // Position/size are remembered only when `window.remember_geometry` is on.
    Settings {
        id: geo
        category: "miniPlayer"
        property int x: -1
        property int y: -1
        property int w: 0
        property int h: 0
    }
    function restoreGeometry() {
        if (!AppConfig.rememberGeometry) return
        if (geo.w >= minimumWidth && geo.h >= minimumHeight) { width = geo.w; height = geo.h }
        if (geo.x >= 0 && geo.y >= 0) { x = geo.x; y = geo.y }
    }
    function saveGeometry() {
        if (!AppConfig.rememberGeometry || !visible) return
        geo.x = x; geo.y = y; geo.w = width; geo.h = height
    }
    Component.onCompleted: restoreGeometry()
    onVisibleChanged: if (!visible) saveGeometry()
    Component.onDestruction: saveGeometry()

    function toggle() { visible = !visible }
    Shortcut { sequence: "Escape"; enabled: win.active; onActivated: win.visible = false }

    MiniPlayerContent {
        anchors.fill: parent
        hostWindow: win
        onCloseRequested: win.visible = false
        onShowMainRequested: {
            const w = applicationWindow()
            w.show(); w.raise(); w.requestActivate()
            win.visible = false
        }
    }
}
