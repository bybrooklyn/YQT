// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// "Up next": the play queue, straight from the Rust `QueueModel`.
//
// Rows are reordered by dragging them anywhere (mouse / pen) or by their handle (touch). While a row is dragged the
// others slide aside and a highlighted gap shows where it will land; the view scrolls by itself near its edges.
// Dropping commits with `QueueModel.moveRow(from, to)` – the Rust queue keeps the playing track playing.
// Every row also has a ⋮ menu, which a right click opens too.
ListView {
    id: queueView

    /// Space kept free on the right for the scroll bar; rows end where the panel switcher above ends.
    property real gutter: 0
    /// Height of one row (all rows are the same height, which keeps the drag maths exact).
    readonly property real rowHeight: Kirigami.Units.gridUnit * 3.5

    // ---- drag state ----------------------------------------------------------------------------------------
    property int dragFrom: -1            // row being dragged (-1: none)
    property int dragTo: -1              // row it would land on if dropped now
    property real dragTop: 0             // current top of the dragged row, in content coordinates
    property real dragStartContentY: 0
    property real dragPointerY: 0        // pointer position in view coordinates (for auto-scroll)
    property bool dropping: false        // dropped, waiting for the model to report the move

    function beginDrag(i) {
        dropping = false
        dragFrom = i; dragTo = i
        dragTop = i * rowHeight
        dragStartContentY = contentY
    }
    function updateDrag(i, dy, pointerY) {
        if (dragFrom !== i || dropping) return
        dragPointerY = pointerY
        const top = i * rowHeight + dy + (contentY - dragStartContentY)
        dragTop = Math.max(0, Math.min((count - 1) * rowHeight, top))
        dragTo = Math.max(0, Math.min(count - 1, Math.round(dragTop / rowHeight)))
    }
    function endDrag(i) {
        if (dragFrom !== i || dropping) return
        if (dragTo !== dragFrom) {
            dropping = true
            dragTop = dragTo * rowHeight
            QueueModel.moveRow(dragFrom, dragTo)
            settleTimer.restart()      // in case the model never reports the move
        } else {
            dragFrom = -1; dragTo = -1
        }
    }
    function settle() { dropping = false; dragFrom = -1; dragTo = -1 }

    Timer { id: settleTimer; interval: 400; onTriggered: queueView.settle() }
    Connections {
        target: QueueModel
        function onRowsMoved() { settleTimer.stop(); queueView.settle() }
        function onModelReset() { settleTimer.stop(); queueView.settle() }
    }

    // scroll while a row is held near the top / bottom edge
    Timer {
        interval: 16
        repeat: true
        running: queueView.dragFrom >= 0 && !queueView.dropping
        onTriggered: {
            const edge = queueView.rowHeight
            let speed = 0
            if (queueView.dragPointerY < edge) speed = -(edge - queueView.dragPointerY) / edge
            else if (queueView.dragPointerY > queueView.height - edge) speed = (queueView.dragPointerY - (queueView.height - edge)) / edge
            if (speed === 0) return
            const max = Math.max(0, queueView.contentHeight - queueView.height)
            const before = queueView.contentY
            queueView.contentY = Math.max(0, Math.min(max, before + speed * Kirigami.Units.gridUnit * 0.8))
            if (queueView.contentY !== before) {   // keep the held row under the pointer
                queueView.dragTop = Math.max(0, Math.min((queueView.count - 1) * queueView.rowHeight, queueView.dragTop + (queueView.contentY - before)))
                queueView.dragTo = Math.max(0, Math.min(queueView.count - 1, Math.round(queueView.dragTop / queueView.rowHeight)))
            }
        }
    }

    clip: true
    model: QueueModel
    interactive: dragFrom < 0
    boundsBehavior: Flickable.StopAtBounds
    // open on the playing row, but never scrolled past either end
    function showCurrent() {
        // (rows are a fixed height, so the position can be computed; positionViewAtIndex near the top
        //  leaves the view's origin shifted and hides the first rows)
        const want = (Player.index + 0.5) * rowHeight - height / 2
        if (Player.index < 0 || count === 0 || want <= 0) return
        contentY = Math.min(want, Math.max(0, count * rowHeight - height))
    }
    Component.onCompleted: Qt.callLater(showCurrent)
    QQC2.ScrollBar.vertical: QQC2.ScrollBar {}

    // the gap the dragged row will drop into
    Rectangle {
        parent: queueView.contentItem
        visible: queueView.dragFrom >= 0 && queueView.dragTo !== queueView.dragFrom
        x: 0
        y: queueView.dragTo * queueView.rowHeight
        width: queueView.width - queueView.gutter
        height: queueView.rowHeight
        z: 0
        radius: Kirigami.Units.smallSpacing
        color: Qt.alpha(Kirigami.Theme.highlightColor, 0.18)
        border.color: Kirigami.Theme.highlightColor
        border.width: 2
    }

    delegate: Item {
        id: slot
        required property int index
        required property string title
        required property string artist
        required property string thumbnail
        required property string duration
        required property string videoId
        required property string artistId
        required property string albumId

        readonly property bool held: queueView.dragFrom === index
        // rows between the grabbed row and its target slide one slot towards the grabbed row's old place
        readonly property real shift: {
            const f = queueView.dragFrom, t = queueView.dragTo
            if (f < 0 || held) return 0
            if (f < t && index > f && index <= t) return -queueView.rowHeight
            if (f > t && index >= t && index < f) return queueView.rowHeight
            return 0
        }
        readonly property bool menuOpen: rowMenu.visible
        readonly property bool current: index === Player.index
        readonly property bool liked: { Backend.likedRev; return Backend.isLiked(videoId) }

        width: queueView.width - queueView.gutter
        height: queueView.rowHeight
        z: held ? 10 : 1

        function startDrag() { queueView.beginDrag(index) }
        function moveDrag(handler) {
            const p = queueView.mapFromItem(slot, handler.centroid.position)
            queueView.updateDrag(index, handler.activeTranslation.y, p.y)
        }

        QQC2.ItemDelegate {
            id: body
            width: parent.width
            height: parent.height
            y: slot.held ? queueView.dragTop - slot.y : slot.shift
            Behavior on y { enabled: !slot.held; NumberAnimation { duration: Kirigami.Units.shortDuration; easing.type: Easing.OutCubic } }
            highlighted: slot.current
            opacity: slot.held ? 0.92 : 1
            onClicked: QueueModel.playRow(slot.index)

            // drag from anywhere on the row with a mouse or pen ...
            DragHandler {
                id: rowDrag
                target: null
                xAxis.enabled: false
                acceptedDevices: PointerDevice.Mouse | PointerDevice.Stylus
                grabPermissions: PointerHandler.CanTakeOverFromAnything
                onActiveChanged: active ? slot.startDrag() : queueView.endDrag(slot.index)
                onActiveTranslationChanged: if (active) slot.moveDrag(rowDrag)
            }
            TapHandler { acceptedButtons: Qt.RightButton; onTapped: rowMenu.popup() }

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing

                // ... or from the handle with a finger
                Kirigami.Icon {
                    id: grip
                    source: "handle-sort"
                    Layout.preferredWidth: Kirigami.Units.iconSizes.smallMedium
                    Layout.preferredHeight: Kirigami.Units.iconSizes.smallMedium
                    opacity: 0.6
                    DragHandler {
                        id: gripDrag
                        target: null
                        xAxis.enabled: false
                        acceptedDevices: PointerDevice.TouchScreen
                        grabPermissions: PointerHandler.CanTakeOverFromAnything
                        onActiveChanged: active ? slot.startDrag() : queueView.endDrag(slot.index)
                        onActiveTranslationChanged: if (active) slot.moveDrag(gripDrag)
                    }
                }
                CoverImage {
                    source: slot.thumbnail
                    kind: "song"
                    decode: 160
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 2.5
                }
                ColumnLayout {
                    spacing: 0
                    Layout.fillWidth: true
                    QQC2.Label { text: slot.title; elide: Text.ElideRight; Layout.fillWidth: true; color: slot.current ? Kirigami.Theme.highlightedTextColor : Kirigami.Theme.textColor }
                    QQC2.Label { text: slot.artist; elide: Text.ElideRight; Layout.fillWidth: true; opacity: 0.7; font: Kirigami.Theme.smallFont }
                }
                QQC2.Label { text: slot.duration; opacity: 0.7 }
                QQC2.ToolButton {
                    icon.name: "overflow-menu"
                    display: QQC2.AbstractButton.IconOnly
                    text: qsTr("More")
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.text: text
                    onClicked: rowMenu.popup()
                }
            }

            QQC2.Menu {
                id: rowMenu
                QQC2.MenuItem { text: qsTr("Play next"); icon.name: "media-skip-forward"; enabled: !slot.current; onTriggered: QueueModel.playRowNext(slot.index) }
                QQC2.MenuItem { text: qsTr("Remove"); icon.name: "list-remove"; onTriggered: QueueModel.removeRow(slot.index) }
                QQC2.MenuSeparator {}
                QQC2.MenuItem {
                    visible: Backend.loggedIn
                    height: visible ? implicitHeight : 0
                    text: qsTr("Add to playlist…")
                    icon.name: "list-add"
                    onTriggered: applicationWindow().addToPlaylist({videoId: slot.videoId, title: slot.title})
                }
                QQC2.MenuItem {
                    visible: slot.artistId !== ""
                    height: visible ? implicitHeight : 0
                    text: qsTr("Go to artist")
                    icon.name: "view-media-artist"
                    onTriggered: applicationWindow().open({kind: "artist", browseId: slot.artistId, title: slot.artist})
                }
                QQC2.MenuItem {
                    visible: slot.albumId !== ""
                    height: visible ? implicitHeight : 0
                    text: qsTr("Go to album")
                    icon.name: "media-optical-audio"
                    onTriggered: applicationWindow().open({kind: "album", browseId: slot.albumId, title: slot.title})
                }
                QQC2.MenuItem {
                    visible: Backend.loggedIn
                    height: visible ? implicitHeight : 0
                    text: slot.liked ? qsTr("Remove from liked music") : qsTr("Like")
                    icon.name: slot.liked ? "starred-symbolic" : "non-starred-symbolic"
                    onTriggered: Backend.setLiked(slot.videoId, !slot.liked)
                }
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
