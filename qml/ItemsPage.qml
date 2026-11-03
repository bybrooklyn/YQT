// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// One page for everything list-shaped: home, search results, albums, playlists, artists, library.
//
// This file is only a view. The `PageController` (Rust) is the list model and does the loading, paging, caching,
// retrying and playing; it tells us what the page is (filters, whether it is a collection, whether it needs an
// account) and what to show (rows, hero, error).
Kirigami.ScrollablePage {
    id: page

    property string target           // "home", "album:ID", "playlist:ID", "artist:ID", "library", ...
    property string query            // a non-empty query makes this a search page
    property string filter           // initial chip (search type / library tab); the default comes from Rust
    property string emptyText: qsTr("Nothing here")

    // Accepted for compatibility with callers that still pass them; the controller now derives all of these.
    property var chips
    property bool collection
    property bool searchable
    property bool needsLogin
    property bool reloadOnLogin

    readonly property alias controller: ctl

    function focusSearch() { if (ctl.searchable) { searchField.forceActiveFocus(); searchField.selectAll() } }

    PageController { id: ctl }
    Component.onCompleted: ctl.load(target, query, filter, false)

    Connections {
        target: Backend
        function onLoggedInChanged() { ctl.loginChanged() }
    }

    function activate(row, kind, browseId, title) {
        if (kind === "song") ctl.playItem(row)
        else applicationWindow().open({kind: kind, browseId: browseId, title: title})
    }

    header: ColumnLayout {
        visible: ctl.searchable || ctl.chipLabels.length > 0
        spacing: 0

        Kirigami.SearchField {
            id: searchField
            visible: ctl.searchable
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            placeholderText: qsTr("Search songs, albums, artists…")
            autoAccept: false
            Component.onCompleted: text = page.query
            onAccepted: {
                suggestPopup.close()
                if (text.trim() !== page.query) applicationWindow().search(text)
            }
            onTextEdited: suggestTimer.restart()
            onActiveFocusChanged: if (!activeFocus) suggestPopup.close()

            Timer {
                id: suggestTimer
                interval: AppConfig.suggestDebounceMs
                onTriggered: ctl.suggest(searchField.text)
            }
            Connections {
                target: ctl
                function onSuggestionsChanged() { if (ctl.suggestions.length > 0 && searchField.activeFocus) suggestPopup.open(); else suggestPopup.close() }
            }

            QQC2.Popup {
                id: suggestPopup
                y: searchField.height
                width: searchField.width
                padding: 0
                focus: false
                closePolicy: QQC2.Popup.CloseOnEscape
                contentItem: ListView {
                    implicitHeight: Math.min(contentHeight, Kirigami.Units.gridUnit * 14)
                    model: ctl.suggestions
                    clip: true
                    delegate: QQC2.ItemDelegate {
                        required property string modelData
                        width: ListView.view.width
                        text: modelData
                        icon.name: "search"
                        onClicked: { suggestPopup.close(); searchField.text = modelData; applicationWindow().search(modelData) }
                    }
                }
            }
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

    ListView {
        id: view
        model: ctl
        clip: true
        reuseItems: true

        // Paging is the controller's rule; we only report which row sits at the viewport's bottom edge
        // (-1: past the last row, -2: rows not laid out yet). Throttled so it runs after layout, never inside it.
        function bottomIndex() {
            const y = contentY + height - 1
            for (const dy of [0, 6, 24, 60]) {         // the edge may fall on a section header: look a little higher
                const i = indexAt(width / 2, y - dy)
                if (i >= 0) return i
            }
            return atYEnd ? -1 : -2
        }
        function reportScroll() {
            if (count > 0 && height > 0) ctl.nearEnd(bottomIndex(), count)
        }
        Timer { id: scrollTimer; interval: 40; onTriggered: view.reportScroll() }
        onContentYChanged: if (!scrollTimer.running) scrollTimer.start()
        onContentHeightChanged: if (!scrollTimer.running) scrollTimer.start()
        onCountChanged: if (!scrollTimer.running) scrollTimer.start()

        header: RowLayout {
            width: view.width
            visible: ctl.hasHero
            height: visible ? implicitHeight : 0
            spacing: Kirigami.Units.largeSpacing
            CoverImage {
                source: ctl.heroCover
                kind: "album"
                decode: 600
                radiusPercent: AppConfig.coverRadiusPercent
                Layout.preferredWidth: Kirigami.Units.gridUnit * 8
                Layout.preferredHeight: Kirigami.Units.gridUnit * 8
                Layout.margins: Kirigami.Units.largeSpacing
            }
            ColumnLayout {
                Layout.fillWidth: true
                Kirigami.Heading { text: ctl.heroTitle; level: 1; wrapMode: Text.Wrap; Layout.fillWidth: true }
                QQC2.Label { text: ctl.heroSubtitle; wrapMode: Text.Wrap; opacity: 0.7; Layout.fillWidth: true; visible: text.length > 0 }
                QQC2.Label { text: ctl.heroStats.join(" • "); opacity: 0.7; visible: text.length > 0 }
                QQC2.Label { text: ctl.heroDescription; wrapMode: Text.Wrap; maximumLineCount: 4; elide: Text.ElideRight; Layout.fillWidth: true; visible: text.length > 0 }
                Kirigami.UrlButton { url: ctl.heroWikipediaUrl; text: qsTr("Wikipedia"); visible: ctl.heroWikipediaUrl.length > 0 }
                RowLayout {
                    QQC2.Button {
                        text: qsTr("Play")
                        icon.name: "media-playback-start"
                        enabled: ctl.songCount > 0
                        onClicked: ctl.playAll(false)
                    }
                    QQC2.Button {
                        text: qsTr("Shuffle")
                        icon.name: "media-playlist-shuffle"
                        enabled: ctl.songCount > 0
                        onClicked: ctl.playAll(true)
                    }
                    QQC2.Button {
                        text: qsTr("Add to queue")
                        icon.name: "media-playlist-append"
                        enabled: ctl.songCount > 0
                        onClicked: ctl.addAllToQueue()
                    }
                }
            }
        }

        footer: Item {
            width: view.width
            height: ctl.loadingMore || ctl.refreshing ? Kirigami.Units.gridUnit * 3 : 0
            QQC2.BusyIndicator { anchors.centerIn: parent; running: ctl.loadingMore || ctl.refreshing; visible: running }
        }

        section.property: "section"
        section.delegate: Kirigami.ListSectionHeader {
            required property string section
            width: view.width
            label: section
            visible: section.length > 0
            height: visible ? implicitHeight : 0
        }

        delegate: QQC2.ItemDelegate {
            id: row
            required property int index
            required property string title
            required property string subtitle
            required property string artist
            required property string kind
            required property string videoId
            required property string browseId
            required property string thumbnail
            required property string duration
            required property string artistId
            required property string albumId
            required property string placeholder

            readonly property bool liked: { Backend.likedRev; return Backend.isLiked(videoId) }

            width: ListView.view.width
            highlighted: kind === "song" && Player.hasTrack && Player.videoId === videoId
            onClicked: page.activate(index, kind, browseId, title)
            TapHandler { acceptedButtons: Qt.RightButton; enabled: row.kind === "song"; onTapped: rowMenu.popup() }

            QQC2.Menu {
                id: rowMenu
                QQC2.MenuItem { text: qsTr("Play next"); icon.name: "media-skip-forward"; onTriggered: ctl.playNext(row.index) }
                QQC2.MenuItem { text: qsTr("Add to queue"); icon.name: "media-playlist-append"; onTriggered: ctl.enqueue(row.index) }
                QQC2.MenuSeparator { visible: Backend.loggedIn }
                QQC2.MenuItem {
                    visible: row.artistId !== ""
                    height: visible ? implicitHeight : 0
                    text: qsTr("Go to artist")
                    icon.name: "view-media-artist"
                    onTriggered: applicationWindow().open({kind: "artist", browseId: row.artistId, title: row.artist})
                }
                QQC2.MenuItem {
                    visible: row.albumId !== ""
                    height: visible ? implicitHeight : 0
                    text: qsTr("Go to album")
                    icon.name: "media-optical-audio"
                    onTriggered: applicationWindow().open({kind: "album", browseId: row.albumId, title: row.title})
                }
                QQC2.MenuItem {
                    visible: Backend.loggedIn
                    height: visible ? implicitHeight : 0
                    text: row.liked ? qsTr("Remove from liked music") : qsTr("Like")
                    icon.name: row.liked ? "starred-symbolic" : "non-starred-symbolic"
                    onTriggered: Backend.setLiked(row.videoId, !row.liked)
                }
                QQC2.MenuItem {
                    visible: Backend.loggedIn
                    height: visible ? implicitHeight : 0
                    text: qsTr("Add to playlist…")
                    icon.name: "list-add"
                    onTriggered: applicationWindow().addToPlaylist({videoId: row.videoId, title: row.title})
                }
            }

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing
                CoverImage {
                    source: row.thumbnail
                    kind: row.placeholder
                    decode: 160
                    radiusPercent: row.kind === "artist" ? 50 : 0
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 2.5
                }
                ColumnLayout {
                    spacing: 0
                    Layout.fillWidth: true
                    QQC2.Label { text: row.title; elide: Text.ElideRight; Layout.fillWidth: true; color: row.highlighted ? Kirigami.Theme.highlightedTextColor : Kirigami.Theme.textColor }
                    QQC2.Label {
                        text: row.subtitle
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                        visible: text.length > 0
                        font: Kirigami.Theme.smallFont
                        opacity: 0.7
                    }
                }
                Kirigami.Icon {
                    visible: row.kind === "song" && row.liked
                    source: "starred-symbolic"
                    implicitWidth: Kirigami.Units.iconSizes.small
                    implicitHeight: implicitWidth
                    opacity: 0.7
                }
                QQC2.Label { text: row.duration; opacity: 0.7; visible: row.duration.length > 0 }
                QQC2.ToolButton {
                    visible: row.kind === "song"
                    icon.name: "overflow-menu"
                    display: QQC2.AbstractButton.IconOnly
                    text: qsTr("More")
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.text: text
                    onClicked: rowMenu.popup()
                }
            }
        }

        // failures replace the page; the view decides nothing about them
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

        Kirigami.PlaceholderMessage {
            anchors.centerIn: parent
            width: parent.width - Kirigami.Units.gridUnit * 4
            visible: ctl.state === "ready" && ctl.count === 0 && !ctl.refreshing
            icon.name: "edit-none"
            text: page.emptyText
        }

        QQC2.BusyIndicator {
            anchors.centerIn: parent
            running: ctl.state === "loading" && ctl.count === 0
            visible: running
        }
    }
}
