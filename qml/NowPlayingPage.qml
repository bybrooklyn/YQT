// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// The "now playing" view: a big cover on one side, Up next / Lyrics on the other.
// There are deliberately no transport controls here – the bar at the bottom is the one place to control playback.
Kirigami.Page {
    id: page
    readonly property bool isNowPlaying: true
    property string tab: "queue"          // "queue" | "lyrics"
    readonly property var media: applicationWindow().media
    readonly property bool wide: width > Kirigami.Units.gridUnit * 42

    title: qsTr("Now Playing")
    padding: Kirigami.Units.largeSpacing

    function goToArtist() { applicationWindow().open({kind: "artist", browseId: Player.artistId, title: Player.artist}) }
    function goToAlbum() { applicationWindow().open({kind: "album", browseId: Player.albumId, title: Player.title}) }
    function parse(json) { try { return JSON.parse(json) } catch (e) { return null } }

    // ---- lyrics state -------------------------------------------------------------------
    property var synced: []               // [[ms, "line"], ...]
    property string plain
    property string lyricsSource
    property string lyricsError
    property bool lyricsBusy: false
    property int lyricsReq: -1
    property string lyricsFor              // videoId the current lyrics belong to

    function seconds() {
        const p = Player.duration.split(":").map(Number)
        return p.length === 2 && !isNaN(p[0]) && !isNaN(p[1]) ? p[0] * 60 + p[1] : 0
    }
    function fetchLyrics() {
        if (!Player.hasTrack || lyricsFor === Player.videoId) return
        lyricsFor = Player.videoId
        synced = []; plain = ""; lyricsError = ""; lyricsSource = ""
        lyricsBusy = true
        lyricsReq = Backend.lyrics(Player.videoId, Player.title, Player.artist, seconds())
    }
    onTabChanged: if (tab === "lyrics") fetchLyrics()
    Component.onCompleted: if (tab === "lyrics") fetchLyrics()

    Connections {
        target: Player
        function onVideoIdChanged() { page.lyricsFor = ""; if (page.tab === "lyrics") page.fetchLyrics() }
    }
    Connections {
        target: Backend
        function onDone(id, err, json) {
            if (id !== page.lyricsReq) return
            page.lyricsBusy = false
            if (err) { page.lyricsError = err.split("\n\n")[0]; return }
            const r = page.parse(json)
            if (!r) { page.lyricsError = qsTr("Got an unreadable answer from the app."); return }
            page.synced = r.synced || []
            page.plain = r.plain || ""
            page.lyricsSource = r.source || ""
        }
    }

    GridLayout {
        anchors.fill: parent
        columns: page.wide ? 2 : 1
        columnSpacing: Kirigami.Units.veryLargeSpacing * 2
        rowSpacing: Kirigami.Units.largeSpacing

        // ---- cover ----------------------------------------------------------------------------------
        ColumnLayout {
            Layout.fillWidth: !page.wide
            Layout.preferredWidth: page.wide ? page.width * 0.46 : -1
            Layout.fillHeight: page.wide
            Layout.alignment: Qt.AlignTop
            spacing: Kirigami.Units.largeSpacing

            Item { Layout.fillHeight: page.wide }

            CoverImage {
                // As large as the space allows. Sized from the page, not from this layout, to avoid binding loops.
                readonly property real avail: page.wide
                    ? Math.min(page.width * 0.46, page.height - Kirigami.Units.gridUnit * 9)
                    : Math.min(page.width - Kirigami.Units.largeSpacing * 2, page.height * 0.40)
                Layout.alignment: Qt.AlignHCenter
                Layout.preferredWidth: Math.max(Kirigami.Units.gridUnit * 8, avail)
                Layout.preferredHeight: Layout.preferredWidth
                source: Player.cover || Player.thumbnail
                decode: 1200
                radius: width * AppConfig.coverRadiusPercent / 100   // slightly rounded
            }

            Kirigami.Heading {
                Layout.fillWidth: true
                text: Player.title
                level: 2
                horizontalAlignment: Text.AlignHCenter
                elide: Text.ElideRight
                maximumLineCount: 2
                wrapMode: Text.Wrap
            }
            QQC2.Label {
                id: artistLabel
                readonly property bool linked: !Player.error && !Player.loading && Player.artistId.length > 0
                Layout.fillWidth: true
                text: Player.error || (Player.loading ? qsTr("Loading…") : Player.artist)
                color: Player.error ? Kirigami.Theme.negativeTextColor : (linked ? Kirigami.Theme.linkColor : Kirigami.Theme.textColor)
                opacity: linked ? 1 : 0.7
                font.underline: linked && artistHover.hovered
                horizontalAlignment: Text.AlignHCenter
                elide: Text.ElideRight
                HoverHandler { id: artistHover; cursorShape: artistLabel.linked ? Qt.PointingHandCursor : Qt.ArrowCursor }
                TapHandler { enabled: artistLabel.linked; onTapped: page.goToArtist() }
            }
            QQC2.ToolButton {
                Layout.alignment: Qt.AlignHCenter
                visible: Player.albumId.length > 0
                icon.name: "media-optical-audio"
                text: qsTr("Go to album")
                onClicked: page.goToAlbum()
            }

            Item { Layout.fillHeight: page.wide }
        }

        // ---- Up next / Lyrics ------------------------------------------------------------------
        ColumnLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: Kirigami.Units.smallSpacing

            QQC2.TabBar {
                id: tabs
                Layout.fillWidth: true
                // Line up with the list below instead of running over the width its scrollbar takes.
                Layout.rightMargin: page.tab === "lyrics" ? lyricsList.QQC2.ScrollBar.vertical.width : queueView.QQC2.ScrollBar.vertical.width
                currentIndex: page.tab === "lyrics" ? 1 : 0
                onCurrentIndexChanged: page.tab = currentIndex === 1 ? "lyrics" : "queue"
                // Two equal halves that fill the width of the list below.
                QQC2.TabButton { text: qsTr("Up next"); width: tabs.width / 2 }
                QQC2.TabButton { text: qsTr("Lyrics"); width: tabs.width / 2 }
            }

            StackLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: page.tab === "lyrics" ? 1 : 0

                // -- queue
                ListView {
                    id: queueView
                    clip: true
                    // The Rust queue model: rows can be moved and removed without the list being rebuilt.
                    model: QueueModel
                    Component.onCompleted: if (Player.index >= 0) positionViewAtIndex(Player.index, ListView.Center)
                    QQC2.ScrollBar.vertical: QQC2.ScrollBar {}
                    moveDisplaced: Transition { YAnimator { duration: Kirigami.Units.longDuration; easing.type: Easing.InOutQuad } }

                    // The dragged row must be a child of the delegate, not the delegate itself (Kirigami.ListItemDragHandle).
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
                        readonly property var song: ({videoId: videoId, title: title, artist: artist, artistId: artistId, albumId: albumId, thumbnail: thumbnail, duration: duration})
                        width: ListView.view.width
                        height: qrow.implicitHeight

                        QQC2.ItemDelegate {
                            id: qrow
                            width: slot.width
                            highlighted: slot.index === Player.index
                            onClicked: Player.playIndex(slot.index)
                            contentItem: RowLayout {
                                spacing: Kirigami.Units.largeSpacing
                                Kirigami.ListItemDragHandle {
                                    listItem: qrow
                                    listView: queueView
                                    onMoveRequested: (oldIndex, newIndex) => QueueModel.moveRow(oldIndex, newIndex)
                                }
                                CoverImage {
                                    source: slot.thumbnail
                                    decode: 160
                                    Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                                    Layout.preferredHeight: Kirigami.Units.gridUnit * 2.5
                                }
                                ColumnLayout {
                                    spacing: 0
                                    Layout.fillWidth: true
                                    QQC2.Label { text: slot.title; elide: Text.ElideRight; Layout.fillWidth: true; color: qrow.highlighted ? Kirigami.Theme.highlightedTextColor : Kirigami.Theme.textColor }
                                    QQC2.Label { text: slot.artist; elide: Text.ElideRight; Layout.fillWidth: true; opacity: 0.7; font: Kirigami.Theme.smallFont }
                                }
                                QQC2.Label { text: slot.duration; opacity: 0.7 }
                                QQC2.ToolButton {
                                    icon.name: "overflow-menu"; display: QQC2.AbstractButton.IconOnly; text: qsTr("More")
                                    onClicked: rowMenu.popup()
                                }
                            }
                            // Right-click opens the same menu as the ⋮ button.
                            TapHandler { acceptedButtons: Qt.RightButton; onTapped: rowMenu.popup() }

                            QQC2.Menu {
                                id: rowMenu
                                QQC2.MenuItem { icon.name: "media-playback-start"; text: qsTr("Play"); onTriggered: Player.playIndex(slot.index) }
                                QQC2.MenuItem { icon.name: "go-next"; text: qsTr("Play next"); enabled: slot.index !== Player.index; onTriggered: QueueModel.playRowNext(slot.index) }
                                QQC2.MenuSeparator {}
                                QQC2.MenuItem { icon.name: "user"; text: qsTr("Go to artist"); visible: slot.artistId.length > 0; height: visible ? implicitHeight : 0; onTriggered: applicationWindow().open({kind: "artist", browseId: slot.artistId, title: slot.artist}) }
                                QQC2.MenuItem { icon.name: "media-optical-audio"; text: qsTr("Go to album"); visible: slot.albumId.length > 0; height: visible ? implicitHeight : 0; onTriggered: applicationWindow().open({kind: "album", browseId: slot.albumId, title: slot.title}) }
                                QQC2.MenuItem {
                                    readonly property bool liked: { Backend.likedRev; return Backend.isLiked(slot.videoId) }
                                    visible: Backend.loggedIn; height: visible ? implicitHeight : 0
                                    icon.name: liked ? "starred-symbolic" : "non-starred-symbolic"
                                    text: liked ? qsTr("Remove from liked music") : qsTr("Like")
                                    onTriggered: Backend.setLiked(slot.videoId, !liked)
                                }
                                QQC2.MenuItem { icon.name: "list-add"; text: qsTr("Add to playlist…"); visible: Backend.loggedIn; height: visible ? implicitHeight : 0; onTriggered: applicationWindow().addToPlaylist({videoId: slot.videoId, title: slot.title}) }
                                QQC2.MenuSeparator {}
                                QQC2.MenuItem { icon.name: "list-remove"; text: qsTr("Remove from queue"); onTriggered: QueueModel.removeRow(slot.index) }
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

                // -- lyrics
                Item {
                    id: lyricsPane

                    // The cover's dominant colour (computed in Rust); the theme highlight until it is ready.
                    Accent { id: accent; source: Player.cover || Player.thumbnail }
                    readonly property color tint: accent.ready ? accent.color : Kirigami.Theme.highlightColor
                    // Shown only as the very last line, set like a lyric.
                    readonly property string creditText: page.lyricsSource.length > 0 ? qsTr("Lyrics from %1").arg(page.lyricsSource) : ""

                    // The line being sung. `rawLine` follows the player position exactly; `line` is what is shown.
                    // Moving forward is applied at once. Moving *backward* only counts once the position has stayed
                    // there for a moment, because the position can glitch backwards for an instant (stream range
                    // requests, buffering) – which used to make the highlight flash off and on again.
                    readonly property int rawLine: {
                        const p = applicationWindow().smoothPosition + AppConfig.lyricsLeadMs
                        const s = page.synced
                        let lo = 0, hi = s.length - 1, r = -1
                        while (lo <= hi) {
                            const mid = (lo + hi) >> 1
                            if (s[mid][0] <= p) { r = mid; lo = mid + 1 } else hi = mid - 1
                        }
                        return r
                    }
                    property int line: -1
                    onRawLineChanged: {
                        if (rawLine >= line) { backTimer.stop(); line = rawLine }
                        else backTimer.restart()
                    }
                    Timer { id: backTimer; interval: 500; onTriggered: lyricsPane.line = lyricsPane.rawLine }
                    Connections { target: page; function onSyncedChanged() { backTimer.stop(); lyricsPane.line = lyricsPane.rawLine } }
                    onLineChanged: lyricsList.follow()

                    ListView {
                        id: lyricsList
                        anchors.fill: parent
                        visible: page.synced.length > 0
                        clip: true
                        model: page.synced
                        bottomMargin: height / 2
                        topMargin: height / 3
                        QQC2.ScrollBar.vertical: QQC2.ScrollBar {}
                        // The source, as a quiet last line under the final lyric.
                        footer: QQC2.Label {
                            width: ListView.view.width
                            visible: lyricsPane.creditText.length > 0
                            height: visible ? Kirigami.Units.gridUnit * 4 : 0
                            verticalAlignment: Text.AlignVCenter
                            horizontalAlignment: Text.AlignHCenter
                            text: lyricsPane.creditText
                            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.3
                            opacity: 0.3
                        }

                        NumberAnimation { id: scrollAnim; target: lyricsList; property: "contentY"; duration: 350; easing.type: Easing.OutCubic }
                        function follow() {
                            const cur = lyricsPane.line
                            if (cur < 0 || dragging || flicking) return
                            const it = itemAtIndex(cur)
                            if (!it) { positionViewAtIndex(cur, ListView.Center); return }
                            scrollAnim.stop()
                            scrollAnim.from = contentY
                            scrollAnim.to = it.y + it.height / 2 - height / 2
                            scrollAnim.start()
                        }

                        // Every line has the same font and the same layout; the current one is only brighter and a
                        // touch larger via `scale`/`opacity`, driven by one animated value so they move together.
                        // (Changing size or weight would re-wrap the text and shift the lines under it.)
                        delegate: Item {
                            id: lineItem
                            required property int index
                            required property var modelData
                            readonly property bool active: index === lyricsPane.line
                            property real glow: active ? 1 : 0
                            Behavior on glow { NumberAnimation { duration: 260; easing.type: Easing.OutCubic } }

                            width: ListView.view.width
                            implicitHeight: label.implicitHeight + Kirigami.Units.largeSpacing * 1.5

                            // How far through its own line the song is (0..1): the karaoke fill.
                            readonly property real progress: {
                                if (!active) return 0
                                const t0 = modelData[0]
                                const next = lineItem.index + 1 < page.synced.length ? page.synced[lineItem.index + 1][0] : t0 + 4000
                                return Math.max(0, Math.min(1, (applicationWindow().smoothPosition + AppConfig.lyricsLeadMs - t0) / Math.max(1, next - t0)))
                            }

                            QQC2.Label {
                                id: label
                                anchors.centerIn: parent
                                width: parent.width * 0.84          // headroom so the scaled line never clips
                                text: lineItem.modelData[1] || "♪"
                                wrapMode: Text.Wrap
                                horizontalAlignment: Text.AlignHCenter
                                font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.9 * AppConfig.lyricsSizeScale
                                font.bold: true
                                opacity: 0.35 + 0.65 * lineItem.glow
                                scale: 1 + 0.1 * lineItem.glow
                                transformOrigin: Item.Center
                                // The line being sung takes the cover's colour.
                                color: lineItem.active && AppConfig.lyricsTint ? lyricsPane.tint : Kirigami.Theme.textColor

                                // Karaoke: the same text in the highlight colour, revealed left to right as the line is sung.
                                // Only for a single-line lyric (a wrapped one just keeps the tint).
                                Item {
                                    visible: AppConfig.lyricsKaraoke && lineItem.active && label.lineCount === 1
                                    anchors.verticalCenter: parent.verticalCenter
                                    x: (label.width - label.contentWidth) / 2
                                    width: label.contentWidth * lineItem.progress
                                    height: label.height
                                    clip: true
                                    QQC2.Label {
                                        x: 0
                                        width: label.width
                                        height: label.height
                                        // keep the text where the base label draws it: offset back by the clip's own x
                                        leftPadding: 0
                                        text: label.text
                                        font: label.font
                                        horizontalAlignment: Text.AlignLeft
                                        color: Kirigami.Theme.highlightColor
                                    }
                                }
                            }
                            TapHandler {
                                onTapped: {
                                    applicationWindow().seek(lineItem.modelData[0])
                                    backTimer.stop()
                                    lyricsPane.line = lineItem.index   // follow an explicit seek immediately
                                }
                            }
                        }
                    }

                    QQC2.ScrollView {
                        id: plainView
                        anchors.fill: parent
                        visible: page.synced.length === 0 && page.plain.length > 0
                        // Wrap to the pane: without this the label is as wide as its longest line, which clipped
                        // text on the right and added a horizontal scrollbar.
                        contentWidth: availableWidth
                        QQC2.ScrollBar.horizontal.policy: QQC2.ScrollBar.AlwaysOff
                        QQC2.Label {
                            width: plainView.availableWidth
                            // the credit follows the text as its last line
                            text: page.plain + (lyricsPane.creditText.length > 0 ? "\n\n" + lyricsPane.creditText : "")
                            wrapMode: Text.Wrap
                            horizontalAlignment: Text.AlignHCenter
                            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.3 * AppConfig.lyricsSizeScale
                            padding: Kirigami.Units.largeSpacing
                        }
                    }

                    Kirigami.PlaceholderMessage {
                        anchors.centerIn: parent
                        width: parent.width - Kirigami.Units.gridUnit * 4
                        visible: !page.lyricsBusy && page.synced.length === 0 && page.plain.length === 0
                        icon.name: "view-media-lyrics"
                        text: page.lyricsError || qsTr("Lyrics unavailable")
                    }
                    QQC2.BusyIndicator { anchors.centerIn: parent; running: page.lyricsBusy; visible: running }
                }
            }
        }
    }
}
