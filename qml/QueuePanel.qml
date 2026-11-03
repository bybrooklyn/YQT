// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// "Up next": the play queue. (Owned by the queue workstream: drag-to-reorder and the ⋮ menu land here.)
ListView {
    id: queueView

    /// Space kept free on the right for the scroll bar; rows end where the panel switcher above ends.
    property real gutter: 0

    clip: true
    model: { try { return JSON.parse(Player.queue) } catch (e) { return [] } }
    Component.onCompleted: if (Player.index >= 0) positionViewAtIndex(Player.index, ListView.Center)
    QQC2.ScrollBar.vertical: QQC2.ScrollBar {}

    delegate: QQC2.ItemDelegate {
        id: qrow
        required property int index
        required property var modelData
        width: ListView.view.width - queueView.gutter
        highlighted: index === Player.index
        onClicked: Player.playIndex(index)
        contentItem: RowLayout {
            spacing: Kirigami.Units.largeSpacing
            CoverImage {
                source: qrow.modelData.thumbnail || ""
                decode: 160
                Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                Layout.preferredHeight: Kirigami.Units.gridUnit * 2.5
            }
            ColumnLayout {
                spacing: 0
                Layout.fillWidth: true
                QQC2.Label { text: qrow.modelData.title || ""; elide: Text.ElideRight; Layout.fillWidth: true; color: qrow.highlighted ? Kirigami.Theme.highlightedTextColor : Kirigami.Theme.textColor }
                QQC2.Label { text: qrow.modelData.artist || ""; elide: Text.ElideRight; Layout.fillWidth: true; opacity: 0.7; font: Kirigami.Theme.smallFont }
            }
            QQC2.Label { text: qrow.modelData.duration || ""; opacity: 0.7 }
            QQC2.ToolButton {
                icon.name: "go-up"; display: QQC2.AbstractButton.IconOnly; text: qsTr("Move up")
                enabled: qrow.index > 0
                onClicked: Player.moveItem(qrow.index, qrow.index - 1)
            }
            QQC2.ToolButton {
                icon.name: "go-down"; display: QQC2.AbstractButton.IconOnly; text: qsTr("Move down")
                enabled: qrow.index < queueView.count - 1
                onClicked: Player.moveItem(qrow.index, qrow.index + 1)
            }
            QQC2.ToolButton {
                icon.name: "list-remove"; display: QQC2.AbstractButton.IconOnly; text: qsTr("Remove")
                onClicked: Player.removeAt(qrow.index)
            }
        }
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        visible: queueView.count === 0
        icon.name: "view-media-playlist"
        text: qsTr("The queue is empty")
    }
}
