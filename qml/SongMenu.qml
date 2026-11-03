// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami
import org.kde.ytm

// The context menu of a song row: play next, queue, go to artist / album, like, add to playlist, download.
// Set `item` (a PageData song item) and call `popup()`. `controller` is the page's PageController (it knows the
// queue actions by video id).
QQC2.Menu {
    id: menu

    property var item: null
    property var controller
    readonly property string videoId: item && item.videoId ? item.videoId : ""
    readonly property bool liked: { Backend.likedRev; return videoId !== "" && Backend.isLiked(videoId) }
    /// "none", "queued", "downloading", "downloaded" or "failed"; read when the menu opens.
    property string dlState: "none"

    onAboutToShow: dlState = videoId ? Downloads.state(videoId) : "none"

    QQC2.MenuItem {
        text: qsTr("Play next")
        icon.name: "media-skip-forward"
        onTriggered: menu.controller.playNextSong(menu.videoId)
    }
    QQC2.MenuItem {
        text: qsTr("Add to queue")
        icon.name: "media-playlist-append"
        onTriggered: menu.controller.enqueueSong(menu.videoId)
    }
    QQC2.MenuSeparator { visible: menu.item && (menu.item.artistId || menu.item.albumId) }
    QQC2.MenuItem {
        visible: !!menu.item && !!menu.item.artistId
        height: visible ? implicitHeight : 0
        text: qsTr("Go to artist")
        icon.name: "view-media-artist"
        onTriggered: applicationWindow().open({kind: "artist", browseId: menu.item.artistId, title: menu.item.artist})
    }
    QQC2.MenuItem {
        visible: !!menu.item && !!menu.item.albumId
        height: visible ? implicitHeight : 0
        text: qsTr("Go to album")
        icon.name: "media-optical-audio"
        onTriggered: applicationWindow().open({kind: "album", browseId: menu.item.albumId, title: menu.item.title})
    }
    QQC2.MenuSeparator { visible: Backend.loggedIn }
    QQC2.MenuItem {
        visible: Backend.loggedIn
        height: visible ? implicitHeight : 0
        text: menu.liked ? qsTr("Remove from liked music") : qsTr("Like")
        icon.name: menu.liked ? "starred-symbolic" : "non-starred-symbolic"
        onTriggered: Backend.setLiked(menu.videoId, !menu.liked)
    }
    QQC2.MenuItem {
        visible: Backend.loggedIn
        height: visible ? implicitHeight : 0
        text: qsTr("Add to playlist…")
        icon.name: "list-add"
        onTriggered: applicationWindow().addToPlaylist({videoId: menu.videoId, title: menu.item.title})
    }
    QQC2.MenuSeparator {}
    QQC2.MenuItem {
        text: menu.dlState === "downloaded" ? qsTr("Remove download")
            : menu.dlState === "queued" || menu.dlState === "downloading" ? qsTr("Cancel download")
            : menu.dlState === "failed" ? qsTr("Retry download") : qsTr("Download")
        icon.name: menu.dlState === "downloaded" || menu.dlState === "queued" || menu.dlState === "downloading" ? "edit-delete" : "download"
        onTriggered: {
            switch (menu.dlState) {
            case "downloaded":
            case "queued":
            case "downloading": Downloads.remove(menu.videoId); break
            case "failed": Downloads.retry(menu.videoId); break
            default: Downloads.download(JSON.stringify(menu.item))
            }
        }
    }
}
