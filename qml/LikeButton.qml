// SPDX-License-Identifier: GPL-3.0-only
// The heart of the playing song. A click shows what happens to it: a spinner while the like is on its way to
// YouTube, a tick when YouTube has taken it, and then the star again (filled or empty), faded back in.
// Knows nothing about the backend: the bar says when a like is `sending` and calls `accepted()`.
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

QQC2.ToolButton {
    id: button

    property bool liked: false
    property bool sending: false
    property bool ticked: false
    signal toggled(bool like)

    function accepted() { ticked = true; back.restart() }

    icon.name: sending ? "" : ticked ? "checkmark-symbolic" : liked ? "starred-symbolic" : "non-starred-symbolic"
    // (without an icon the button would narrow while the spinner shows, and the bar would shift: keep its width)
    property real iconWidth: 0
    onImplicitWidthChanged: if (!sending) iconWidth = implicitWidth
    Component.onCompleted: if (!sending) iconWidth = implicitWidth
    Layout.minimumWidth: iconWidth
    onClicked: if (!sending) toggled(!liked)

    QQC2.BusyIndicator {
        anchors.centerIn: parent
        width: Kirigami.Units.iconSizes.smallMedium; height: width
        running: button.sending
        visible: running
    }

    // the tick stays a moment, fades out, and the star fades in
    SequentialAnimation {
        id: back
        PauseAnimation { duration: 900 }
        NumberAnimation { target: button; property: "opacity"; to: 0; duration: Kirigami.Units.longDuration }
        ScriptAction { script: button.ticked = false }
        NumberAnimation { target: button; property: "opacity"; to: 1; duration: Kirigami.Units.longDuration }
    }
}
