// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.kirigami as Kirigami

// Album/track cover that never looks broken: a neutral placeholder shows while the image loads, when there is
// no image, or when it fails to load, and the real image fades in over it once ready.
Item {
    id: root

    property string source
    /// Size the image is decoded at (px). Keep it near the displayed size ×2 so lists stay light.
    property int decode: 256
    property int fillMode: Image.PreserveAspectCrop
    readonly property bool ready: img.status === Image.Ready

    Rectangle {
        anchors.fill: parent
        color: Kirigami.Theme.alternateBackgroundColor
        radius: Kirigami.Units.smallSpacing / 2

        Kirigami.Icon {
            anchors.centerIn: parent
            width: Math.min(parent.width, parent.height) * 0.5
            height: width
            source: "media-optical-audio"
            opacity: 0.35
        }
    }

    Image {
        id: img
        anchors.fill: parent
        source: root.source
        sourceSize: Qt.size(root.decode, root.decode)
        asynchronous: true
        cache: true
        fillMode: root.fillMode
        opacity: status === Image.Ready ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: 180 } }
    }
}
