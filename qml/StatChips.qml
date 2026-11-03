// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// The facts under a title as one quiet line: "Album • 2019 • 12 songs • 46 min".
// `stats` is PageData's `[{text, icon}]`.
Flow {
    id: root

    property var stats: []
    property real textOpacity: 0.75

    spacing: Kirigami.Units.smallSpacing
    visible: (stats || []).length > 0

    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    Repeater {
        model: root.stats || []
        delegate: Row {
            required property var modelData
            required property int index
            spacing: Kirigami.Units.smallSpacing
            QQC2.Label {
                visible: parent.index > 0
                text: "•"
                opacity: root.textOpacity * 0.6
            }
            Kirigami.Icon {
                visible: !!parent.modelData.icon
                source: parent.modelData.icon || ""
                width: Kirigami.Units.iconSizes.small
                height: width
                anchors.verticalCenter: parent.verticalCenter
            }
            QQC2.Label {
                text: parent.modelData.text
                opacity: root.textOpacity
            }
        }
    }
}
