// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.ytm
import org.kde.kirigami as Kirigami

// The visual contents of the mini-player (see MiniPlayer.qml, which puts this in a frameless Window).
// Kept as its own Item so it can also be rendered/grabbed on its own. It reads the same Playback/Player state as the main window.
Rectangle {
    id: root

    /// The window this lives in (for startSystemMove/startSystemResize); null when embedded in a test harness.
    property var hostWindow: null
    signal closeRequested()
    signal showMainRequested()

    readonly property var media: applicationWindow().media

    implicitWidth: Kirigami.Units.gridUnit * 22
    implicitHeight: Kirigami.Units.gridUnit * 7

    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.Window
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    color: Kirigami.Theme.backgroundColor
    radius: Kirigami.Units.cornerRadius
    border.width: 1
    border.color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g, Kirigami.Theme.textColor.b, 0.25)

    function fmt(ms) {
        const s = Math.max(0, Math.floor(ms / 1000))
        return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0")
    }

    // Drag the whole body to move the window. Controls sit above this and keep their own clicks.
    DragHandler {
        target: null
        acceptedButtons: Qt.LeftButton
        onActiveChanged: if (active && root.hostWindow) root.hostWindow.startSystemMove()
    }

    RowLayout {
        anchors.fill: parent
        anchors.margins: Kirigami.Units.smallSpacing
        anchors.bottomMargin: Kirigami.Units.smallSpacing + progress.height
        spacing: Kirigami.Units.largeSpacing

        CoverImage {
            Layout.fillHeight: true
            Layout.preferredWidth: height
            Layout.maximumWidth: height
            source: Player.thumbnail
            decode: 256
            radiusPercent: AppConfig.coverRadiusPercent
        }

        ColumnLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 0

            RowLayout {
                Layout.fillWidth: true
                spacing: 0
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    QQC2.Label {
                        Layout.fillWidth: true
                        text: Player.hasTrack ? Player.title : qsTr("Nothing playing")
                        elide: Text.ElideRight
                        font.bold: true
                    }
                    QQC2.Label {
                        Layout.fillWidth: true
                        text: Player.error || (Player.loading ? qsTr("Loading…") : Player.artist)
                        color: Player.error ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.textColor
                        opacity: 0.7
                        elide: Text.ElideRight
                        visible: text.length > 0
                    }
                }
                QQC2.ToolButton {
                    icon.name: "view-restore"
                    display: QQC2.AbstractButton.IconOnly
                    onClicked: root.showMainRequested()
                    QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: qsTr("Back to main window")
                    Accessible.name: qsTr("Back to main window")
                }
                QQC2.ToolButton {
                    icon.name: "window-close"
                    display: QQC2.AbstractButton.IconOnly
                    onClicked: root.closeRequested()
                    QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: qsTr("Close mini-player")
                    Accessible.name: qsTr("Close mini-player")
                }
            }

            Item { Layout.fillHeight: true }

            RowLayout {
                Layout.fillWidth: true
                spacing: Kirigami.Units.smallSpacing
                TransportControls {
                    size: 0.9
                    compact: root.width < Kirigami.Units.gridUnit * 18
                    Layout.preferredWidth: implicitWidth
                    Layout.preferredHeight: implicitHeight
                }
                Item { Layout.fillWidth: true }
                QQC2.Label {
                    text: root.fmt(root.media.smoothPosition) + " / " + root.fmt(root.media.duration)
                    font: Kirigami.Theme.smallFont
                    opacity: 0.7
                    visible: root.width > Kirigami.Units.gridUnit * 16
                }
                QQC2.ToolButton {
                    id: likeBtn
                    visible: Backend.loggedIn
                    enabled: Player.hasTrack
                    readonly property bool liked: { Backend.likedRev; return Backend.isLiked(Player.videoId) }
                    icon.name: liked ? "starred-symbolic" : "non-starred-symbolic"
                    display: QQC2.AbstractButton.IconOnly
                    onClicked: Backend.setLiked(Player.videoId, !liked)
                    QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: liked ? qsTr("Remove from liked music") : qsTr("Like")
                    Accessible.name: QQC2.ToolTip.text
                }
            }
        }
    }

    // Slim, seekable progress along the bottom edge.
    Item {
        id: progress
        anchors { left: parent.left; right: parent.right; bottom: parent.bottom
                  leftMargin: root.radius; rightMargin: root.radius }
        height: area.containsMouse || area.pressed || activeFocus ? Kirigami.Units.smallSpacing * 2 : Kirigami.Units.smallSpacing
        activeFocusOnTab: true
        Accessible.role: Accessible.Slider
        Accessible.name: qsTr("Seek")

        readonly property real fraction: root.media.duration > 0 ? Math.min(1, root.media.smoothPosition / root.media.duration) : 0
        property real dragFraction: -1          // >= 0 while the user is scrubbing

        Behavior on height { NumberAnimation { duration: Kirigami.Units.shortDuration } }

        Keys.onLeftPressed: root.media.seek(Math.max(0, root.media.position - 5000))
        Keys.onRightPressed: root.media.seek(Math.min(root.media.duration, root.media.position + 5000))

        Rectangle {
            anchors.fill: parent
            radius: height / 2
            color: Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g, Kirigami.Theme.textColor.b, 0.18)
        }
        Rectangle {
            width: parent.width * (progress.dragFraction >= 0 ? progress.dragFraction : progress.fraction)
            height: parent.height
            radius: height / 2
            color: Kirigami.Theme.highlightColor
        }
        MouseArea {
            id: area
            anchors { fill: parent; topMargin: -Kirigami.Units.smallSpacing; bottomMargin: -Kirigami.Units.smallSpacing }
            hoverEnabled: true
            enabled: root.media.duration > 0
            preventStealing: true
            function frac(mx) { return Math.max(0, Math.min(1, mx / width)) }
            onPressed: mouse => { progress.forceActiveFocus(); progress.dragFraction = frac(mouse.x) }
            onPositionChanged: mouse => { if (pressed) progress.dragFraction = frac(mouse.x) }
            onReleased: {
                if (progress.dragFraction >= 0) root.media.seek(progress.dragFraction * root.media.duration)
                progress.dragFraction = -1
            }
            onCanceled: progress.dragFraction = -1
        }
    }

    // Bottom-right grip: resize with the window manager's own resize.
    MouseArea {
        visible: root.hostWindow !== null
        width: Kirigami.Units.gridUnit; height: width
        anchors { right: parent.right; bottom: parent.bottom }
        cursorShape: Qt.SizeFDiagCursor
        onPressed: if (root.hostWindow) root.hostWindow.startSystemResize(Qt.RightEdge | Qt.BottomEdge)
    }
}
