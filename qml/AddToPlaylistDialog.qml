// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Pick one of your playlists to add a song to. The list is a `PageController` in picker mode (Rust): the first page
// loads when the dialog opens and the rest as you scroll, so a big library doesn't make the dialog wait.
Kirigami.Dialog {
    id: dlg
    property var song: ({})              // {videoId, title}
    property string message
    property bool failed: false
    property bool busy: false
    property int addReq: -1

    title: qsTr("Add to playlist")
    standardButtons: Kirigami.Dialog.Close
    preferredWidth: Kirigami.Units.gridUnit * 22

    PageController { id: ctl }

    onOpened: {
        message = ""; failed = false; busy = false
        ctl.loadPicker()
    }

    property QtObject backendWatcher: Connections {
        target: Backend
        function onDone(id, err, json) {
            if (id !== dlg.addReq) return
            dlg.busy = false
            dlg.failed = !!err
            if (err) { dlg.message = err.split("\n\n")[0]; return }
            dlg.close()
            applicationWindow().showPassiveNotification(qsTr("Added “%1”").arg(dlg.song.title))
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
        QQC2.BusyIndicator { Layout.alignment: Qt.AlignHCenter; running: dlg.busy || ctl.state === "loading"; visible: running }
        ErrorView {
            Layout.fillWidth: true
            visible: ctl.state === "error" || ctl.state === "offline"
            kind: ctl.errorKind
            headline: ctl.errorHeadline
            hint: ctl.errorHint
            detail: ctl.errorDetail
            errorId: ctl.errorId
            diagnostics: ctl.errorDiagnostics
            retryMode: ctl.errorRetry
            retryMs: ctl.errorRetryMs
            onRetryRequested: ctl.retry()
            onAutoRetryRequested: ctl.autoRetry()
            onOpenSettings: { dlg.close(); applicationWindow().go("settings") }
            onShowDownloads: { dlg.close(); applicationWindow().go("downloads") }
        }
        ListView {
            id: list
            visible: ctl.state === "ready"
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 14
            clip: true
            model: ctl
            // the controller decides when the next page is due
            function reportScroll() {
                if (count <= 0 || height <= 0) return
                let i = indexAt(width / 2, contentY + height - 1)
                if (i < 0) i = atYEnd ? -1 : -2          // -1: past the last row, -2: not laid out yet
                ctl.nearEnd(i, count)
            }
            Timer { id: scrollTimer; interval: 40; onTriggered: list.reportScroll() }
            onContentYChanged: if (!scrollTimer.running) scrollTimer.start()
            onContentHeightChanged: if (!scrollTimer.running) scrollTimer.start()
            onCountChanged: if (!scrollTimer.running) scrollTimer.start()
            delegate: QQC2.ItemDelegate {
                id: item
                required property string title
                required property string subtitle
                required property string browseId
                required property string thumbnail
                required property string placeholder
                width: ListView.view.width
                enabled: !dlg.busy
                contentItem: RowLayout {
                    spacing: Kirigami.Units.largeSpacing
                    CoverImage {
                        source: item.thumbnail
                        kind: item.placeholder
                        decode: 128
                        Layout.preferredWidth: Kirigami.Units.gridUnit * 2
                        Layout.preferredHeight: Kirigami.Units.gridUnit * 2
                    }
                    ColumnLayout {
                        spacing: 0
                        Layout.fillWidth: true
                        QQC2.Label { text: item.title; elide: Text.ElideRight; Layout.fillWidth: true }
                        QQC2.Label { text: item.subtitle; font: Kirigami.Theme.smallFont; opacity: 0.7; visible: text.length > 0; elide: Text.ElideRight; Layout.fillWidth: true }
                    }
                }
                onClicked: { dlg.busy = true; dlg.message = ""; dlg.addReq = Backend.addToPlaylist(item.browseId, dlg.song.videoId) }
            }
        }
    }
}
