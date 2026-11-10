// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import dev.bybrooklyn.yqt
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Offline library: storage bar, running downloads and the downloaded songs. Everything shown comes from the
// local index and the cover cache, so the page works with no network.
Kirigami.ScrollablePage {
    id: page
    title: qsTr("Downloads")


    property var rows: []
    readonly property var songs: rows.filter(r => r.state === "downloaded")

    function fmtBytes(n) {
        if (n >= 1e9) return (n / 1e9).toFixed(n >= 1e10 ? 1 : 2) + " GB"
        if (n >= 1e6) return (n / 1e6).toFixed(n >= 1e8 ? 0 : 1) + " MB"
        if (n >= 1e3) return Math.round(n / 1e3) + " kB"
        return n + " B"
    }
    function fmtSpeed(bps) { return bps > 0 ? fmtBytes(bps) + "/s" : "" }
    // What the list shows: `rows` mirrored into a list model that is patched in place, so a reload (every state change
    // of a download) keeps the scroll position instead of rebuilding the list from the top.
    ListModel { id: rowModel }
    function shown(r) {
        return {videoId: String(r.videoId || ""), title: String(r.title || ""), artist: String(r.artist || ""),
                cover: String(r.cover || ""), state: String(r.state || ""), error: String(r.error || ""),
                percent: Number(r.percent || 0), speed: Number(r.speed || 0), size: Number(r.size || 0), pinned: !!r.pinned}
    }
    function reload() {
        try { rows = JSON.parse(Downloads.list()) } catch (e) { rows = [] }
        for (let i = 0; i < rows.length; ++i) {
            const r = shown(rows[i])
            let at = -1
            for (let j = i; j < rowModel.count && at < 0; ++j) if (rowModel.get(j).videoId === r.videoId) at = j
            if (at < 0) { rowModel.insert(i, r); continue }
            if (at !== i) rowModel.move(at, i, 1)
            rowModel.set(i, r)
        }
        if (rowModel.count > rows.length) rowModel.remove(rows.length, rowModel.count - rows.length)
    }
    function pauseText() {
        switch (Downloads.pauseReason) {
        case "user": return qsTr("Downloads are paused.")
        case "offline": return qsTr("You are offline - downloads will continue when the connection is back.")
        case "metered": return qsTr("Paused on a metered connection.")
        case "low_battery": return qsTr("Paused: the battery is low.")
        default: return ""
        }
    }

    Component.onCompleted: reload()
    Connections {
        target: Downloads
        function onChanged(id) { reloadTimer.restart() }
    }
    // progress ticks only update the rows that are visible; a list reload is for state changes
    Timer { id: reloadTimer; interval: 150; onTriggered: page.reload() }

    actions: [
        Kirigami.Action {
            text: Downloads.paused && Downloads.pauseReason === "user" ? qsTr("Resume all") : qsTr("Pause all")
            icon.name: Downloads.paused && Downloads.pauseReason === "user" ? "media-playback-start" : "media-playback-pause"
            enabled: Downloads.activeCount > 0 || (Downloads.paused && Downloads.pauseReason === "user")
            onTriggered: Downloads.pauseAll(!(Downloads.paused && Downloads.pauseReason === "user"))
        }
    ]

    ListView {
        id: list
        model: rowModel
        clip: true
        spacing: 0

        header: ColumnLayout {
            width: list.width
            spacing: Kirigami.Units.smallSpacing

            ColumnLayout {
                Layout.fillWidth: true
                Layout.margins: Kirigami.Units.largeSpacing
                spacing: Kirigami.Units.smallSpacing

                RowLayout {
                    Layout.fillWidth: true
                    QQC2.Label {
                        Layout.fillWidth: true
                        text: qsTr("%1 of %2 used").arg(page.fmtBytes(Downloads.usedBytes)).arg(page.fmtBytes(Downloads.capBytes))
                        font.bold: true
                    }
                    QQC2.Label {
                        visible: page.songs.length > 0
                        text: qsTr("%n song(s)", "", page.songs.length)
                        opacity: 0.7
                    }
                }
                QQC2.ProgressBar {
                    Layout.fillWidth: true
                    from: 0
                    to: Math.max(Downloads.capBytes, 1)
                    value: Math.min(Downloads.usedBytes, Math.max(Downloads.capBytes, 1))
                }
            }

            Kirigami.InlineMessage {
                Layout.fillWidth: true
                Layout.leftMargin: Kirigami.Units.largeSpacing
                Layout.rightMargin: Kirigami.Units.largeSpacing
                visible: Downloads.paused
                type: Kirigami.MessageType.Information
                text: page.pauseText()
            }
        }

        delegate: QQC2.ItemDelegate {
            id: row
            required property var model
            readonly property var modelData: model   // the row's fields, live (see `shown`)
            required property int index
            readonly property bool done: modelData.state === "downloaded"
            width: ListView.view.width
            highlighted: Player.hasTrack && Player.videoId === modelData.videoId

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing

                CoverImage {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 3
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 3
                    source: row.modelData.cover || ""
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    QQC2.Label { Layout.fillWidth: true; text: row.modelData.title || row.modelData.videoId; elide: Text.ElideRight; font.bold: true }
                    QQC2.Label {
                        Layout.fillWidth: true
                        opacity: 0.7
                        elide: Text.ElideRight
                        text: {
                            const m = row.modelData
                            if (row.done) return [m.artist, page.fmtBytes(m.size)].filter(x => x).join("  -  ")
                            if (m.state === "failed") return qsTr("Failed: %1").arg(m.error || "")
                            if (m.state === "queued") return qsTr("Waiting...")
                            return [qsTr("%1%").arg(m.percent), page.fmtSpeed(m.speed)].filter(x => x).join("  -  ")
                        }
                    }
                    QQC2.ProgressBar {
                        Layout.fillWidth: true
                        visible: row.modelData.state === "downloading"
                        from: 0; to: 100; value: row.modelData.percent
                    }
                }
                QQC2.ToolButton {
                    visible: row.done
                    checkable: true
                    checked: !!row.modelData.pinned
                    icon.name: "pin"
                    onToggled: { Downloads.pin(row.modelData.videoId, checked); reloadTimer.restart() }
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.text: checked ? qsTr("Pinned - never removed automatically") : qsTr("Pin: keep this song offline")
                }
                QQC2.ToolButton {
                    visible: row.modelData.state === "failed"
                    icon.name: "view-refresh"
                    onClicked: Downloads.retry(row.modelData.videoId)
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.text: qsTr("Retry")
                }
                QQC2.ToolButton {
                    icon.name: row.done ? "edit-delete" : "process-stop"
                    onClicked: Downloads.remove(row.modelData.videoId)
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.text: row.done ? qsTr("Remove from this device") : qsTr("Cancel")
                }
            }

            onClicked: {
                if (!done) return
                const idx = page.songs.findIndex(s => s.videoId === modelData.videoId)
                Player.playList(JSON.stringify(page.songs), Math.max(idx, 0))
            }
        }

        Kirigami.PlaceholderMessage {
            anchors.centerIn: parent
            width: parent.width - Kirigami.Units.gridUnit * 4
            visible: page.rows.length === 0
            icon.name: "folder-download"
            text: qsTr("No downloads yet")
            explanation: qsTr("Use the download button on a song, album or playlist, or turn on smart downloads in Settings, to listen offline.")
        }
    }
}
