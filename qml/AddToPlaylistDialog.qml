// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import dev.bybrooklyn.yqt
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Pick one of your playlists to add a song to. The list is lazy: the first page loads when the dialog opens and the
// rest as you scroll, so a big library doesn't make the dialog wait.
Kirigami.Dialog {
    id: dlg
    property var song: ({})
    // {browseId, title, subtitle} rows. A list model, appended to page by page: handing the view a new array for every
    // page made it jump back to the top.
    property ListModel playlists: ListModel {}
    property int openings: 0             // counts openings: an answer for an earlier one must not act on this one
    property int addFor: -1              // the opening the running "add" belongs to
    property string addTitle             // … and the song it adds
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
    function addEntries(items) {
        for (const p of items) playlists.append({browseId: p.browseId || "", title: p.title || "", subtitle: p.subtitle || ""})
    }
    function loadMore() {
        if (hasMore && pagerId > 0 && moreReq < 0 && !busy) moreReq = Backend.more(pagerId)
    }

    onOpened: {
        openings += 1
        message = ""; failed = false; playlists.clear(); busy = true
        hasMore = false; pagerId = 0; moreReq = -1
        listReq = Backend.myPlaylists()
    }
    // The Rust side keeps the list's pager until told to forget it.
    onClosed: {
        if (pagerId > 0) Backend.dropPager(pagerId)
        pagerId = 0; hasMore = false; listReq = -1; moreReq = -1
    }

    property QtObject backendWatcher: Connections {
        target: Backend
        function onDone(id, err, json) {
            if (id === dlg.listReq) {
                dlg.busy = false
                if (err) { dlg.failed = true; dlg.message = err.split("\n\n")[0]; return }
                const res = dlg.parse(json)
                if (!res) { dlg.failed = true; dlg.message = qsTr("Got an unreadable answer from the app."); return }
                dlg.addEntries(res.items || [])
                dlg.pagerId = res.pager || 0
                if (dlg.pagerId > 0) Backend.holdPager(dlg.pagerId)
                dlg.hasMore = !!res.more
            } else if (id === dlg.moreReq) {
                dlg.moreReq = -1
                if (err) { dlg.hasMore = false; return }
                const res = dlg.parse(json)
                if (!res) { dlg.hasMore = false; return }
                dlg.addEntries(res.items || [])
                dlg.hasMore = !!res.more
            } else if (id === dlg.addReq) {
                dlg.addReq = -1
                // The dialog may have been closed and opened for another song since: then only say what happened.
                const current = dlg.addFor === dlg.openings && dlg.visible
                if (current) { dlg.busy = false; dlg.failed = !!err }
                if (err) { if (current) dlg.message = err.split("\n\n")[0]; return }
                if (current) dlg.close()
                applicationWindow().showPassiveNotification(qsTr("Added “%1”").arg(dlg.addTitle))
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
                id: entry
                required property string browseId
                required property string title
                required property string subtitle
                width: ListView.view.width
                text: entry.title
                icon.name: "view-media-playlist"
                enabled: !dlg.busy
                contentItem: ColumnLayout {
                    spacing: 0
                    QQC2.Label { text: entry.title; elide: Text.ElideRight; Layout.fillWidth: true }
                    QQC2.Label { text: entry.subtitle; font: Kirigami.Theme.smallFont; opacity: 0.7; visible: text.length > 0; elide: Text.ElideRight; Layout.fillWidth: true }
                }
                onClicked: {
                    dlg.busy = true; dlg.message = ""
                    dlg.addFor = dlg.openings; dlg.addTitle = dlg.song.title || ""
                    dlg.addReq = Backend.addToPlaylist(entry.browseId, dlg.song.videoId)
                }
            }
        }
    }
}
