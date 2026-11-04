// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Effects
import org.kde.ytm
import org.kde.kirigami as Kirigami

// Album/track cover that never looks broken: a neutral placeholder shows while the image loads, when there is
// no image, or when it fails to load, and the real image fades in over it once ready.
Item {
    id: root

    property string source
    /// Size the image is decoded at (px). Keep it near the displayed size ×2 so lists stay light.
    property int decode: 256
    property int fillMode: Image.PreserveAspectCrop
    /// Corner radius in px. Lists keep the small default; the big cover on the Now Playing page asks for more.
    property real radius: Kirigami.Units.smallSpacing / 2
    readonly property bool ready: img.status === Image.Ready
    readonly property bool rounded: radius > Kirigami.Units.smallSpacing

    Rectangle {
        anchors.fill: parent
        color: Kirigami.Theme.alternateBackgroundColor
        radius: root.radius

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
        visible: !root.rounded          // when rounded, the masked copy below is what is drawn
        opacity: status === Image.Ready ? 1 : 0
        Behavior on opacity { enabled: !AppConfig.reduceMotion; NumberAnimation { duration: 180 } }
    }

    // Rounded corners (only when a larger radius was asked for): the image masked by a rounded rectangle.
    Item {
        id: corners
        anchors.fill: parent
        visible: false
        layer.enabled: root.rounded
        Rectangle { anchors.fill: parent; radius: root.radius }
    }
    MultiEffect {
        anchors.fill: parent
        visible: root.rounded
        source: img
        maskEnabled: true
        maskSource: corners
        opacity: img.opacity
    }
}
