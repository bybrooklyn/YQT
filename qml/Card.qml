// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// A cover card for shelves and grids: cover (a themed placeholder when there is no art), title, subtitle, and a
// play button that fades in on hover or keyboard focus. Artists get a round avatar and centred text.
//
// Presentational: `item` is one PageData item ({kind, title, subtitle, thumbnail, ...}); the card only emits
// `activated()` (open / play it) and `playRequested()` (the hover button).
FocusScope {
    id: root

    property var item
    property real cardWidth: Kirigami.Units.gridUnit * 10
    /// Lines reserved for the subtitle (kept constant so cards in a row line up).
    property int subtitleLines: 2
    readonly property string kind: item && item.kind ? item.kind : "song"
    readonly property bool round: kind === "artist"
    readonly property bool hovered: area.containsMouse || playArea.containsMouse
    readonly property bool showPlay: !round && (hovered || activeFocus)

    signal activated()
    signal playRequested()

    // Pin the colour group: custom controls otherwise follow the window's focus state (inactive-window purple).
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    width: cardWidth
    implicitWidth: cardWidth
    implicitHeight: column.implicitHeight + pad * 2
    activeFocusOnTab: true
    readonly property real pad: Kirigami.Units.smallSpacing * 1.5

    Accessible.role: Accessible.Button
    Accessible.name: item ? item.title + (item.subtitle ? ", " + item.subtitle : "") : ""
    Accessible.onPressAction: root.activated()
    Keys.onReturnPressed: root.activated()
    Keys.onEnterPressed: root.activated()
    Keys.onSpacePressed: root.activated()

    function placeholderFor(k) {
        switch (k) {
        case "artist": return "user-identity"
        case "playlist": return "view-media-playlist"
        case "album": return "media-optical-audio"
        default: return "audio-x-generic"
        }
    }

    FontMetrics { id: smallMetrics; font: Kirigami.Theme.smallFont }

    Rectangle {
        anchors.fill: parent
        radius: Kirigami.Units.cornerRadius
        color: Kirigami.Theme.textColor
        opacity: root.hovered || root.activeFocus ? 0.07 : 0
        Behavior on opacity { enabled: !AppConfig.reduceMotion; NumberAnimation { duration: 120 } }
    }
    Rectangle {
        anchors.fill: parent
        radius: Kirigami.Units.cornerRadius
        color: "transparent"
        border.width: root.visualFocus ? 2 : 0
        border.color: Kirigami.Theme.highlightColor
    }

    ColumnLayout {
        id: column
        anchors { left: parent.left; right: parent.right; top: parent.top; margins: root.pad }
        spacing: Kirigami.Units.smallSpacing

        Item {
            Layout.fillWidth: true
            Layout.preferredHeight: width

            CoverImage {
                anchors.fill: parent
                source: root.item ? root.item.thumbnail : ""
                decode: 384
                radiusPercent: root.round ? 50 : Math.max(AppConfig.coverRadiusPercent, 4)
                placeholderIcon: root.placeholderFor(root.kind)
            }

            // hover play button
            Rectangle {
                id: playButton
                width: Kirigami.Units.gridUnit * 2.6
                height: width
                radius: width / 2
                anchors { right: parent.right; bottom: parent.bottom; margins: Kirigami.Units.smallSpacing * 2 }
                color: playArea.pressed ? Qt.darker(Kirigami.Theme.highlightColor, 1.2) : Kirigami.Theme.highlightColor
                opacity: root.showPlay ? 1 : 0
                scale: root.showPlay || AppConfig.reduceMotion ? 1 : 0.85
                visible: opacity > 0
                Behavior on opacity { enabled: !AppConfig.reduceMotion; NumberAnimation { duration: 140 } }
                Behavior on scale { enabled: !AppConfig.reduceMotion; NumberAnimation { duration: 140; easing.type: Easing.OutCubic } }
                Kirigami.Icon {
                    anchors.centerIn: parent
                    width: Kirigami.Units.iconSizes.smallMedium
                    height: width
                    source: "media-playback-start"
                    color: Kirigami.Theme.highlightedTextColor
                }
                MouseArea {
                    id: playArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.playRequested()
                }
                QQC2.ToolTip.visible: playArea.containsMouse
                QQC2.ToolTip.text: qsTr("Play")
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            }
        }

        QQC2.Label {
            Layout.fillWidth: true
            text: root.item ? root.item.title : ""
            font.weight: Font.DemiBold
            elide: Text.ElideRight
            maximumLineCount: 1
            horizontalAlignment: root.round ? Text.AlignHCenter : Text.AlignLeft
        }
        QQC2.Label {
            Layout.fillWidth: true
            Layout.preferredHeight: smallMetrics.height * root.subtitleLines
            text: root.item ? root.item.subtitle || root.item.artist || "" : ""
            font: Kirigami.Theme.smallFont
            opacity: 0.7
            wrapMode: Text.WordWrap
            elide: Text.ElideRight
            maximumLineCount: root.subtitleLines
            verticalAlignment: Text.AlignTop
            horizontalAlignment: root.round ? Text.AlignHCenter : Text.AlignLeft
        }
    }

    MouseArea {
        id: area
        anchors.fill: parent
        z: -1
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: { root.forceActiveFocus(Qt.MouseFocusReason); root.activated() }
    }
}
