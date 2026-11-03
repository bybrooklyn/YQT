// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// An artist as a round avatar with the name under it ("Fans also like"), or, with `compact`, as a small pill with
// the avatar beside the name (artist links in an album header). Emits `activated()`.
FocusScope {
    id: root

    property var item
    property bool compact: false
    property real avatarSize: Kirigami.Units.gridUnit * 7
    readonly property bool hovered: area.containsMouse

    signal activated()

    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    activeFocusOnTab: true
    implicitWidth: compact ? pill.implicitWidth : avatarSize + Kirigami.Units.largeSpacing
    implicitHeight: compact ? pill.implicitHeight : tile.implicitHeight + Kirigami.Units.smallSpacing * 2
    width: implicitWidth

    Accessible.role: Accessible.Button
    Accessible.name: item ? item.title : ""
    Accessible.onPressAction: root.activated()
    Keys.onReturnPressed: root.activated()
    Keys.onEnterPressed: root.activated()
    Keys.onSpacePressed: root.activated()

    // ---- avatar tile
    ColumnLayout {
        id: tile
        visible: !root.compact
        anchors { left: parent.left; right: parent.right; top: parent.top; topMargin: Kirigami.Units.smallSpacing }
        spacing: Kirigami.Units.smallSpacing

        CoverImage {
            Layout.alignment: Qt.AlignHCenter
            Layout.preferredWidth: root.avatarSize
            Layout.preferredHeight: root.avatarSize
            source: root.item ? root.item.thumbnail : ""
            decode: 320
            radiusPercent: 50
            placeholderIcon: "user-identity"

            Rectangle {
                anchors.fill: parent
                radius: width / 2
                color: "transparent"
                border.width: root.visualFocus ? 3 : 0
                border.color: Kirigami.Theme.highlightColor
            }
            Rectangle {
                anchors.fill: parent
                radius: width / 2
                color: Kirigami.Theme.textColor
                opacity: root.hovered ? 0.1 : 0
                Behavior on opacity { enabled: !AppConfig.reduceMotion; NumberAnimation { duration: 120 } }
            }
        }
        QQC2.Label {
            Layout.fillWidth: true
            text: root.item ? root.item.title : ""
            font.weight: Font.DemiBold
            horizontalAlignment: Text.AlignHCenter
            elide: Text.ElideRight
            maximumLineCount: 1
        }
        QQC2.Label {
            Layout.fillWidth: true
            visible: text.length > 0
            text: root.item && root.item.subtitle ? root.item.subtitle.replace(/^Artist • /, "") : ""
            font: Kirigami.Theme.smallFont
            opacity: 0.7
            horizontalAlignment: Text.AlignHCenter
            elide: Text.ElideRight
            maximumLineCount: 1
        }
    }

    // ---- compact pill
    Rectangle {
        id: pill
        visible: root.compact
        implicitWidth: pillRow.implicitWidth + Kirigami.Units.smallSpacing * 3
        implicitHeight: Kirigami.Units.gridUnit * 2
        radius: height / 2
        // a translucent surface, so it sits on any tinted hero background
        color: Qt.alpha(Kirigami.Theme.backgroundColor, root.hovered || root.activeFocus ? 0.55 : 0.78)
        border.width: root.visualFocus ? 2 : 0
        border.color: Kirigami.Theme.highlightColor
        RowLayout {
            id: pillRow
            anchors.centerIn: parent
            spacing: Kirigami.Units.smallSpacing
            QQC2.Label {
                text: root.item ? root.item.title : ""
                font.weight: Font.DemiBold
                color: Kirigami.Theme.linkColor
                elide: Text.ElideRight
                Layout.maximumWidth: Kirigami.Units.gridUnit * 14
            }
        }
    }

    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: { root.forceActiveFocus(Qt.MouseFocusReason); root.activated() }
    }
}
