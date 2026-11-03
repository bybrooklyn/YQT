// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// One dense song row: a track number (album/playlist) or a small cover (top songs, history), title, artist and
// duration. The number turns into a play icon on hover; the row playing right now is highlighted.
FocusScope {
    id: root

    property var item
    /// Show `item.number` instead of a cover.
    property bool numbered: false
    readonly property bool current: !!item && !!item.videoId && Player.hasTrack && Player.videoId === item.videoId
    readonly property bool hovered: area.containsMouse

    signal activated()
    /// Right click.
    signal menuRequested()

    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    implicitHeight: Kirigami.Units.gridUnit * (numbered ? 2.8 : 3.4)
    activeFocusOnTab: true
    Accessible.role: Accessible.Button
    Accessible.name: item ? item.title + (item.artist ? ", " + item.artist : "") : ""
    Accessible.onPressAction: root.activated()
    Keys.onReturnPressed: root.activated()
    Keys.onEnterPressed: root.activated()
    Keys.onSpacePressed: root.activated()
    Keys.onMenuPressed: root.menuRequested()

    Rectangle {
        anchors.fill: parent
        radius: Kirigami.Units.cornerRadius
        color: root.current ? Kirigami.Theme.highlightColor : Kirigami.Theme.textColor
        opacity: root.current ? 0.16 : root.hovered || root.activeFocus ? 0.07 : 0
        Behavior on opacity { enabled: !AppConfig.reduceMotion; NumberAnimation { duration: 100 } }
    }
    Rectangle {
        anchors.fill: parent
        radius: Kirigami.Units.cornerRadius
        color: "transparent"
        border.width: root.visualFocus ? 2 : 0
        border.color: Kirigami.Theme.highlightColor
    }

    RowLayout {
        anchors { fill: parent; leftMargin: Kirigami.Units.smallSpacing * 2; rightMargin: Kirigami.Units.largeSpacing }
        spacing: Kirigami.Units.largeSpacing

        // number / play icon / cover
        Item {
            Layout.preferredWidth: root.numbered ? Kirigami.Units.gridUnit * 2.2 : Kirigami.Units.gridUnit * 2.4
            Layout.preferredHeight: root.numbered ? Kirigami.Units.gridUnit * 2.2 : Kirigami.Units.gridUnit * 2.4
            Layout.alignment: Qt.AlignVCenter

            QQC2.Label {
                visible: root.numbered && !root.hovered && !root.current
                anchors.centerIn: parent
                text: root.item && root.item.number ? root.item.number : ""
                opacity: 0.6
                font.features: { "tnum": 1 }
            }
            CoverImage {
                visible: !root.numbered
                anchors.fill: parent
                source: root.item ? root.item.thumbnail : ""
                decode: 128
                radiusPercent: Math.max(AppConfig.coverRadiusPercent, 8)
            }
            Rectangle {
                visible: !root.numbered && (root.hovered || root.current)
                anchors.fill: parent
                radius: Kirigami.Units.cornerRadius
                color: Qt.rgba(0, 0, 0, 0.45)
            }
            Kirigami.Icon {
                visible: root.hovered || root.current
                anchors.centerIn: parent
                width: Kirigami.Units.iconSizes.smallMedium
                height: width
                source: root.current && !root.hovered ? "media-playback-playing" : "media-playback-start"
                color: root.numbered ? (root.current ? Kirigami.Theme.highlightColor : Kirigami.Theme.textColor) : "white"
            }
        }

        ColumnLayout {
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignVCenter
            spacing: 0
            QQC2.Label {
                Layout.fillWidth: true
                text: root.item ? root.item.title : ""
                elide: Text.ElideRight
                maximumLineCount: 1
                font.weight: root.current ? Font.Bold : Font.Normal
                color: root.current ? Kirigami.Theme.highlightColor : Kirigami.Theme.textColor
            }
            QQC2.Label {
                Layout.fillWidth: true
                visible: text.length > 0
                text: root.item ? root.item.artist || root.item.subtitle || "" : ""
                elide: Text.ElideRight
                maximumLineCount: 1
                font: Kirigami.Theme.smallFont
                opacity: 0.7
            }
        }

        QQC2.Label {
            visible: text.length > 0
            Layout.alignment: Qt.AlignVCenter
            text: root.item ? root.item.duration : ""
            opacity: 0.65
            font.features: { "tnum": 1 }
        }
    }

    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        acceptedButtons: Qt.LeftButton | Qt.RightButton
        cursorShape: Qt.PointingHandCursor
        onClicked: mouse => {
            root.forceActiveFocus(Qt.MouseFocusReason)
            if (mouse.button === Qt.RightButton) root.menuRequested(); else root.activated()
        }
    }
}
