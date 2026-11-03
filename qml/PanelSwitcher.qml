// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami

// Segmented "pill" selector for switching between panels (Up next / Lyrics).
//
// - Give it `Layout.fillWidth: true` and a `Layout.rightMargin` equal to the scroll bar gutter of the panel below,
//   so the whole pill (track and buttons) ends where the content ends instead of running under the scroll bar.
// - Colours are pinned to the *Active* colour group so the highlight never turns purple when the window is unfocused.
// - Standard TabBar underneath, so keyboard navigation and accessibility work as usual.
QQC2.TabBar {
    id: switcher

    /// Labels for the segments, in order. The current segment is `currentIndex`.
    property var labels: []

    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active
    Kirigami.Theme.colorSet: Kirigami.Theme.Button

    implicitHeight: Kirigami.Units.gridUnit * 2.4
    spacing: 0
    padding: Kirigami.Units.smallSpacing / 2

    background: Rectangle {
        radius: height / 2
        // the pill track sits slightly off the page background so it reads as one control
        color: Qt.alpha(Kirigami.Theme.textColor, 0.08)
        border.width: 1
        border.color: Qt.alpha(Kirigami.Theme.textColor, 0.06)
    }

    Repeater {
        model: switcher.labels
        QQC2.TabButton {
            required property string modelData
            required property int index
            text: modelData
            width: (switcher.availableWidth) / Math.max(1, switcher.labels.length)
            hoverEnabled: true

            contentItem: QQC2.Label {
                text: parent.text
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
                elide: Text.ElideRight
                font.weight: parent.checked ? Font.DemiBold : Font.Normal
                color: parent.checked ? Kirigami.Theme.highlightedTextColor : Kirigami.Theme.textColor
                Behavior on color { ColorAnimation { duration: 150 } }
            }

            background: Rectangle {
                radius: height / 2
                color: parent.checked ? Kirigami.Theme.highlightColor
                     : parent.hovered ? Qt.alpha(Kirigami.Theme.textColor, 0.08)
                     : "transparent"
                Behavior on color { ColorAnimation { duration: 150 } }
            }
        }
    }
}
