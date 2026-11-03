// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// The rich pages (Home, Explore, Library, album, playlist, artist): a `RichPage` driven by a `PageController`.
//
// The controller (Rust) loads, pages, caches and retries, and hands over the whole page as `pageData`; this file only
// wires the view's signals to controller actions. Search results and Liked music stay on `ItemsPage` (dense lists).
RichPage {
    id: page

    property string target           // "home", "explore", "library", "album:ID", "playlist:ID", "artist:ID"
    property string filter           // initial library tab; the default comes from Rust
    /// What the navigation drawer highlights for this page ("home", "explore", "library"); empty for pushed pages.
    property string navName

    readonly property alias controller: ctl

    function focusSearch() { if (ctl.searchable) { searchField.forceActiveFocus(); searchField.selectAll() } }

    PageController { id: ctl }
    Component.onCompleted: ctl.load(target, "", filter, false)

    Connections {
        target: Backend
        function onLoggedInChanged() { ctl.loginChanged() }
    }

    pageData: ctl.pageData
    busy: ctl.state === "loading" || ctl.refreshing
    playable: ctl.songCount > 0
    greeting: target === "home" ? Nav.greeting() : ""
    emptyText: ctl.state !== "ready" || ctl.refreshing ? "" : target === "library" ? qsTr("Nothing in your library yet") : qsTr("Nothing here")

    onActivated: item => {
        if (item.kind === "song") ctl.playSong(item.videoId)
        else applicationWindow().open(item)
    }
    onPlayRequested: item => {
        if (item.kind === "song") ctl.playSong(item.videoId)
        else ctl.playCollection(item.kind, item.browseId, false)
    }
    onPlayAll: shuffle => ctl.playAll(shuffle)
    onMoreRequested: ctl.nearEnd(-1, ctl.count) // RichPage already judged "near the end" by the row at the bottom
    onItemMenuRequested: item => {
        if (item.kind !== "song") return
        songMenu.item = item
        songMenu.popup()
    }
    onAction: name => {
        switch (name) {
        case "radio": ctl.playCollection("playlist", page.radioId, false); break
        case "add_to_queue": ctl.addAllToQueue(); break
        case "download": Downloads.downloadAll(ctl.songsJson()); break
        }
    }

    SongMenu { id: songMenu; controller: ctl }

    header: ColumnLayout {
        visible: ctl.searchable || ctl.chipLabels.length > 0
        spacing: 0

        SearchBar {
            id: searchField
            visible: ctl.searchable
            controller: ctl
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
        }
        RowLayout {
            visible: ctl.chipLabels.length > 0
            Layout.margins: Kirigami.Units.smallSpacing
            Repeater {
                model: ctl.chipLabels
                QQC2.ToolButton {
                    required property int index
                    required property string modelData
                    text: modelData
                    checkable: true
                    autoExclusive: true
                    checked: ctl.filter === ctl.chipValues[index]
                    onClicked: ctl.selectFilter(ctl.chipValues[index])
                }
            }
        }
    }

    // failures replace the page; the view decides nothing about them
    overlay: [
        // opaque, so a greeting hero (Home) does not show through
        Rectangle {
            anchors.fill: parent
            visible: ctl.state === "error" || ctl.state === "offline"
            color: Kirigami.Theme.backgroundColor
        },
        ErrorView {
            anchors.fill: parent
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
            onOpenSettings: applicationWindow().go("settings")
            onShowDownloads: applicationWindow().go("downloads")
        }
    ]
}
