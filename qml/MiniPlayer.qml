// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Window
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import dev.bybrooklyn.yqt
import org.kde.kirigami as Kirigami

// A small always-available player window: cover, title, transport and progress. It reads the same state as the main
// window (`main` is the ApplicationWindow), so it can never drift from it. Opened from the tray, the shortcut or the
// drawer; closing it only hides it.
Window {
    id: mini

    /// The main ApplicationWindow (Main.qml sets this).
    property var main: null
    readonly property var media: main ? main.media : null

    visible: false
    width: Kirigami.Units.gridUnit * 24
    height: Kirigami.Units.gridUnit * 6.5
    minimumWidth: Kirigami.Units.gridUnit * 18
    minimumHeight: Kirigami.Units.gridUnit * 6
    title: Player.hasTrack ? qsTr("%1 — YQT").arg(Player.title) : qsTr("YQT")
    // A normal top-level window (own taskbar entry, not tied to the main window, so it stays when that one is hidden to the tray).
    transientParent: null
    flags: Qt.Window | Qt.WindowCloseButtonHint | Qt.WindowTitleHint | (AppConfig.miniAlwaysOnTop ? Qt.WindowStaysOnTopHint : 0)

    function toggle() {
        if (visible) hide()
        else { show(); raise(); requestActivate() }
    }

    QQC2.Pane {
        anchors.fill: parent
        padding: Kirigami.Units.smallSpacing

        RowLayout {
            anchors.fill: parent
            spacing: Kirigami.Units.largeSpacing

            CoverImage {
                source: Player.thumbnail
                decode: 192
                Layout.preferredWidth: Kirigami.Units.gridUnit * 4.5
                Layout.preferredHeight: Kirigami.Units.gridUnit * 4.5
            }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 0
                QQC2.Label {
                    Layout.fillWidth: true
                    text: Player.hasTrack ? Player.title : qsTr("Nothing playing")
                    font.bold: true
                    elide: Text.ElideRight
                }
                QQC2.Label {
                    Layout.fillWidth: true
                    text: Player.artist
                    elide: Text.ElideRight
                    opacity: 0.7
                }
                RowLayout {
                    Layout.fillWidth: true
                    QQC2.ToolButton {
                        icon.name: "media-skip-backward"
                        enabled: Player.hasTrack
                        onClicked: if (mini.main) mini.main.previousTrack()
                    }
                    QQC2.ToolButton {
                        icon.name: Player.loading ? "process-working" : (mini.media && mini.media.playing ? "media-playback-pause" : "media-playback-start")
                        enabled: Player.hasTrack
                        onClicked: if (mini.main) mini.main.togglePlay()
                    }
                    QQC2.ToolButton {
                        icon.name: "media-skip-forward"
                        enabled: Player.hasTrack
                        onClicked: Player.next()
                    }
                    QQC2.Slider {
                        Layout.fillWidth: true
                        from: 0
                        to: Math.max(mini.media ? mini.media.duration : 0, 1)
                        value: mini.main ? mini.main.smoothPosition : 0
                        enabled: mini.media && mini.media.duration > 0
                        onMoved: if (mini.main) mini.main.seek(value)
                    }
                }
            }
        }
    }
}
