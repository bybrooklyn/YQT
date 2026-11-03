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
                Layout.fillWidth: true
                currentIndex: page.tab === "lyrics" ? 1 : 0
                onCurrentIndexChanged: page.tab = currentIndex === 1 ? "lyrics" : "queue"
                QQC2.TabButton { text: qsTr("Up next") }
                QQC2.TabButton { text: qsTr("Lyrics") }
            }

            StackLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: page.tab === "lyrics" ? 1 : 0

                // -- queue
                ListView {
                    id: queueView
                    clip: true
                    model: { try { return JSON.parse(Player.queue) } catch (e) { return [] } }
                    Component.onCompleted: if (Player.index >= 0) positionViewAtIndex(Player.index, ListView.Center)
                    QQC2.ScrollBar.vertical: QQC2.ScrollBar {}

                    delegate: QQC2.ItemDelegate {
                        id: qrow
                        required property int index
                        required property var modelData
                        width: ListView.view.width
                        highlighted: index === Player.index
                        onClicked: Player.playIndex(index)
                        contentItem: RowLayout {
                            spacing: Kirigami.Units.largeSpacing
                            CoverImage {
                                source: qrow.modelData.thumbnail || ""
                                decode: 160
                                Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                                Layout.preferredHeight: Kirigami.Units.gridUnit * 2.5
                            }
                            ColumnLayout {
                                spacing: 0
                                Layout.fillWidth: true
                                QQC2.Label { text: qrow.modelData.title || ""; elide: Text.ElideRight; Layout.fillWidth: true; color: qrow.highlighted ? Kirigami.Theme.highlightedTextColor : Kirigami.Theme.textColor }
                                QQC2.Label { text: qrow.modelData.artist || ""; elide: Text.ElideRight; Layout.fillWidth: true; opacity: 0.7; font: Kirigami.Theme.smallFont }
                            }
                            QQC2.Label { text: qrow.modelData.duration || ""; opacity: 0.7 }
                            QQC2.ToolButton {
                                icon.name: "go-up"; display: QQC2.AbstractButton.IconOnly; text: qsTr("Move up")
                                enabled: qrow.index > 0
                                onClicked: Player.moveItem(qrow.index, qrow.index - 1)
                            }
                            QQC2.ToolButton {
                                icon.name: "go-down"; display: QQC2.AbstractButton.IconOnly; text: qsTr("Move down")
                                enabled: qrow.index < queueView.count - 1
                                onClicked: Player.moveItem(qrow.index, qrow.index + 1)
                            }
                            QQC2.ToolButton {
                                icon.name: "list-remove"; display: QQC2.AbstractButton.IconOnly; text: qsTr("Remove")
                                onClicked: Player.removeAt(qrow.index)
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

                    // The line being sung. `rawLine` follows the player position exactly; `line` is what is shown.
                    // Moving forward is applied at once. Moving *backward* only counts once the position has stayed
                    // there for a moment, because the position can glitch backwards for an instant (stream range
                    // requests, buffering) – which used to make the highlight flash off and on again.
                    readonly property int rawLine: {
                        const p = page.media.smoothPosition + AppConfig.lyricsLeadMs
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
                        anchors.bottomMargin: credit.visible ? credit.height + Kirigami.Units.smallSpacing : 0   // end above the credit
                        visible: page.synced.length > 0
                        clip: true
                        model: page.synced
                        bottomMargin: height / 2
                        topMargin: height / 3
                        QQC2.ScrollBar.vertical: QQC2.ScrollBar {}

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
                            implicitHeight: label.implicitHeight + Kirigami.Units.largeSpacing

                            QQC2.Label {
                                id: label
                                anchors.centerIn: parent
                                width: parent.width * 0.84          // headroom so the scaled line never clips
                                text: lineItem.modelData[1] || "♪"
                                wrapMode: Text.Wrap
                                horizontalAlignment: Text.AlignHCenter
                                font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.3
                                opacity: 0.4 + 0.6 * lineItem.glow
                                scale: 1 + 0.12 * lineItem.glow
                                transformOrigin: Item.Center
                            }
                            TapHandler {
                                onTapped: {
                                    page.media.seek(lineItem.modelData[0])
                                    backTimer.stop()
                                    lyricsPane.line = lineItem.index   // follow an explicit seek immediately
                                }
                            }
                        }
                    }

                    QQC2.ScrollView {
                        id: plainView
                        anchors.fill: parent
                        anchors.bottomMargin: credit.visible ? credit.height + Kirigami.Units.smallSpacing : 0
                        visible: page.synced.length === 0 && page.plain.length > 0
                        // Wrap to the pane: without this the label is as wide as its longest line, which clipped
                        // text on the right and added a horizontal scrollbar.
                        contentWidth: availableWidth
                        QQC2.ScrollBar.horizontal.policy: QQC2.ScrollBar.AlwaysOff
                        QQC2.Label {
                            width: plainView.availableWidth
                            text: page.plain
                            wrapMode: Text.Wrap
                            horizontalAlignment: Text.AlignHCenter
                            padding: Kirigami.Units.largeSpacing
                        }
                    }

                    QQC2.Label {
                        id: credit
                        anchors.bottom: parent.bottom
                        anchors.right: parent.right
                        visible: page.lyricsSource.length > 0
                        text: page.lyricsSource === "LRCLIB" ? qsTr("Synced lyrics: LRCLIB") : page.lyricsSource
                        font: Kirigami.Theme.smallFont
                        opacity: 0.5
                    }

                    Kirigami.PlaceholderMessage {
                        anchors.centerIn: parent
                        width: parent.width - Kirigami.Units.gridUnit * 4
                        visible: !page.lyricsBusy && page.synced.length === 0 && page.plain.length === 0
                        icon.name: "view-media-lyrics"
                        text: page.lyricsError || qsTr("No lyrics available")
                    }
                    QQC2.BusyIndicator { anchors.centerIn: parent; running: page.lyricsBusy; visible: running }
                }
            }
        }
    }
}
