// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Pick one of your playlists to add a song to. The list is lazy: the first page loads when the dialog opens and the
// rest as you scroll, so a big library doesn't make the dialog wait.
Kirigami.Dialog {
    id: dlg
    property var song: ({})
    property var playlists: []           // [{id, title, subtitle}]
    property string message
    property bool failed: false
    property bool busy: false
    property int listReq: -1
    property int addReq: -1
    property int moreReq: -1
    property int pagerId: 0
    property bool hasMore: false

    title: qsTr("Add to playlist")
    standardButtons: Kirigami.Dialog.Close
    preferredWidth: Kirigami.Units.gridUnit * 22

    function parse(json) { try { return JSON.parse(json) } catch (e) { return null } }
    function entries(items) { return items.map(p => ({id: p.browseId, title: p.title, subtitle: p.subtitle})) }
    function loadMore() {
        if (hasMore && pagerId > 0 && moreReq < 0 && !busy) moreReq = Backend.more(pagerId)
    }

    onOpened: {
        message = ""; failed = false; playlists = []; busy = true
        hasMore = false; pagerId = 0; moreReq = -1
        listReq = Backend.myPlaylists()
    }

    property QtObject backendWatcher: Connections {
        target: Backend
        function onDone(id, err, json) {
            if (id === dlg.listReq) {
                dlg.busy = false
                if (err) { dlg.failed = true; dlg.message = err.split("\n\n")[0]; return }
                const res = dlg.parse(json)
                if (!res) { dlg.failed = true; dlg.message = qsTr("Got an unreadable answer from the app."); return }
                dlg.playlists = dlg.entries(res.items || [])
                dlg.pagerId = res.pager || 0
                dlg.hasMore = !!res.more
            } else if (id === dlg.moreReq) {
                dlg.moreReq = -1
                if (err) { dlg.hasMore = false; return }
                const res = dlg.parse(json)
                if (!res) { dlg.hasMore = false; return }
                dlg.playlists = dlg.playlists.concat(dlg.entries(res.items || []))
                dlg.hasMore = !!res.more
            } else if (id === dlg.addReq) {
                dlg.busy = false
                dlg.failed = !!err
                if (err) { dlg.message = err.split("\n\n")[0]; return }
                dlg.close()
                applicationWindow().showPassiveNotification(qsTr("Added “%1”").arg(dlg.song.title))
            }
        }
    }

    ColumnLayout {
        spacing: Kirigami.Units.smallSpacing
        Kirigami.InlineMessage {
            Layout.fillWidth: true
            visible: dlg.message.length > 0
            type: dlg.failed ? Kirigami.MessageType.Error : Kirigami.MessageType.Information
            text: dlg.message
        }
        QQC2.BusyIndicator { Layout.alignment: Qt.AlignHCenter; running: dlg.busy; visible: running }
        ListView {
            id: list
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 14
            clip: true
            model: dlg.playlists
            // load the next page when scrolled near the end
            onContentYChanged: if (contentHeight - contentY - height < height) dlg.loadMore()
            onContentHeightChanged: if (contentHeight - contentY - height < height) dlg.loadMore()
            delegate: QQC2.ItemDelegate {
                required property var modelData
                width: ListView.view.width
                text: modelData.title
                icon.name: "view-media-playlist"
                enabled: !dlg.busy
                contentItem: ColumnLayout {
                    spacing: 0
                    QQC2.Label { text: modelData.title; elide: Text.ElideRight; Layout.fillWidth: true }
                    QQC2.Label { text: modelData.subtitle; font: Kirigami.Theme.smallFont; opacity: 0.7; visible: text.length > 0; elide: Text.ElideRight; Layout.fillWidth: true }
                }
                onClicked: { dlg.busy = true; dlg.message = ""; dlg.addReq = Backend.addToPlaylist(modelData.id, dlg.song.videoId) }
            }
        }
    }
}
