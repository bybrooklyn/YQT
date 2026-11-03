// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.kirigami as Kirigami

// Three dots for an instrumental break. While it is the current item they fill one after another as the
// break runs out (`progress` 0..1) and breathe gently; otherwise they sit dim.
Item {
    id: gap

    /// 0..1 through the break.
    property real progress: 0
    /// True while the break is playing.
    property bool active: false
    property real pixelSize: 28
    property color color: Kirigami.Theme.highlightColor
    /// Allow the breathing animation (off with reduce-motion).
    property bool animate: true

    implicitHeight: pixelSize * 1.6

    // 1 → 1.14 → 1 while active; stays at 1 otherwise.
    property real pulse: 1
    SequentialAnimation on pulse {
        running: gap.active && gap.animate && gap.visible
        loops: Animation.Infinite
        NumberAnimation { to: 1.14; duration: 700; easing.type: Easing.InOutSine }
        NumberAnimation { to: 1.0; duration: 700; easing.type: Easing.InOutSine }
        onRunningChanged: if (!running) gap.pulse = 1
    }

    Row {
        anchors.centerIn: parent
        spacing: gap.pixelSize * 0.45
        scale: gap.active ? gap.pulse : 1
        Repeater {
            model: 3
            Rectangle {
                required property int index
                width: gap.pixelSize * 0.4
                height: width
                radius: width / 2
                color: gap.color
                opacity: gap.active ? 0.25 + 0.75 * Math.max(0, Math.min(1, gap.progress * 3 - index)) : 0.25
            }
        }
    }
}
