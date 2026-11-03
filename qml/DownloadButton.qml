// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami

// Download toggle for one song. `item` is the song's item object (needs `videoId`; `title`, `artist`/`subtitle`,
// `cover`/`thumbnail`, `duration` are stored for the Downloads page).
//   not downloaded -> click downloads;  queued / downloading -> click cancels;
//   downloaded -> click removes;        failed -> click retries.
QQC2.ToolButton {
    id: btn

    property var item
    readonly property string videoId: item && item.videoId ? item.videoId : ""
    /// "none", "queued", "downloading", "downloaded" or "failed"
    property string dlState: "none"
    property int percent: 0

    function refresh() {
        dlState = videoId ? Downloads.state(videoId) : "none"
        percent = videoId ? Downloads.percent(videoId) : 0
    }

    visible: videoId.length > 0
    enabled: videoId.length > 0
    icon.name: dlState === "downloaded" ? "emblem-ok-symbolic"
             : dlState === "failed" ? "dialog-error"
             : dlState === "queued" || dlState === "downloading" ? "process-stop"
             : "download"
    text: dlState === "downloading" ? percent + "%" : ""
    display: dlState === "downloading" ? QQC2.AbstractButton.TextBesideIcon : QQC2.AbstractButton.IconOnly
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    QQC2.ToolTip.visible: hovered
    QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
    QQC2.ToolTip.text: dlState === "downloaded" ? qsTr("Downloaded - click to remove")
                     : dlState === "downloading" ? qsTr("Downloading %1% - click to cancel").arg(percent)
                     : dlState === "queued" ? qsTr("Waiting to download - click to cancel")
                     : dlState === "failed" ? qsTr("Download failed - click to retry")
                     : qsTr("Download for offline listening")

    onClicked: {
        if (!videoId) return
        switch (dlState) {
        case "downloaded":
        case "queued":
        case "downloading": Downloads.remove(videoId); break
        case "failed": Downloads.retry(videoId); break
        default: Downloads.download(JSON.stringify(item))
        }
        refresh()
    }

    onVideoIdChanged: refresh()
    Component.onCompleted: refresh()

    Connections {
        target: Downloads
        function onChanged(id) { if (id === btn.videoId) btn.refresh() }
        function onProgress(id, pct, speed) { if (id === btn.videoId) { btn.dlState = "downloading"; btn.percent = pct } }
    }
}
