// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// "Lyrics": time-synced lines that follow playback, or plain text. (Owned by the lyrics workstream.)
Item {
    id: lyricsPane

    /// True while this panel is the visible one; lyrics are only fetched then.
    property bool active: false
    /// Space kept free on the right for the scroll bar.
    property real gutter: 0
    readonly property var media: applicationWindow().media

    // ---- lyrics state -------------------------------------------------------------------
    property var synced: []               // [[ms, "line"], ...]
    property string plain
    property string lyricsSource
    property string lyricsError
    property bool lyricsBusy: false
    property int lyricsReq: -1
    property string lyricsFor              // videoId the current lyrics belong to

    function parse(json) { try { return JSON.parse(json) } catch (e) { return null } }
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
    onActiveChanged: if (active) fetchLyrics()
    Component.onCompleted: if (active) fetchLyrics()

    Connections {
        target: Player
        function onVideoIdChanged() { lyricsPane.lyricsFor = ""; if (lyricsPane.active) lyricsPane.fetchLyrics() }
    }
    Connections {
        target: Backend
        function onDone(id, err, json) {
            if (id !== lyricsPane.lyricsReq) return
            lyricsPane.lyricsBusy = false
            if (err) { lyricsPane.lyricsError = err.split("\n\n")[0]; return }
            const r = lyricsPane.parse(json)
            if (!r) { lyricsPane.lyricsError = qsTr("Got an unreadable answer from the app."); return }
            lyricsPane.synced = r.synced || []
            lyricsPane.plain = r.plain || ""
            lyricsPane.lyricsSource = r.source || ""
        }
    }

    // The line being sung. `rawLine` follows the player position exactly; `line` is what is shown.
    // Moving forward is applied at once. Moving *backward* only counts once the position has stayed
    // there for a moment, because the position can glitch backwards for an instant (stream range
    // requests, buffering) – which used to make the highlight flash off and on again.
    readonly property int rawLine: {
        const p = lyricsPane.media.smoothPosition + AppConfig.lyricsLeadMs
        const s = lyricsPane.synced
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
    onSyncedChanged: { backTimer.stop(); line = rawLine }
    onLineChanged: lyricsList.follow()

    ListView {
        id: lyricsList
        anchors.fill: parent
        anchors.bottomMargin: credit.visible ? credit.height + Kirigami.Units.smallSpacing : 0   // end above the credit
        visible: lyricsPane.synced.length > 0
        clip: true
        model: lyricsPane.synced
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

            width: ListView.view.width - lyricsPane.gutter
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
                    lyricsPane.media.seek(lineItem.modelData[0])
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
        visible: lyricsPane.synced.length === 0 && lyricsPane.plain.length > 0
        // Wrap to the pane: without this the label is as wide as its longest line, which clipped
        // text on the right and added a horizontal scrollbar.
        contentWidth: availableWidth
        QQC2.ScrollBar.horizontal.policy: QQC2.ScrollBar.AlwaysOff
        QQC2.Label {
            width: plainView.availableWidth - lyricsPane.gutter
            text: lyricsPane.plain
            wrapMode: Text.Wrap
            horizontalAlignment: Text.AlignHCenter
            padding: Kirigami.Units.largeSpacing
        }
    }

    QQC2.Label {
        id: credit
        anchors.bottom: parent.bottom
        anchors.right: parent.right
        anchors.rightMargin: lyricsPane.gutter
        visible: lyricsPane.lyricsSource.length > 0
        text: lyricsPane.lyricsSource === "LRCLIB" ? qsTr("Synced lyrics: LRCLIB") : lyricsPane.lyricsSource
        font: Kirigami.Theme.smallFont
        opacity: 0.5
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - Kirigami.Units.gridUnit * 4
        visible: !lyricsPane.lyricsBusy && lyricsPane.synced.length === 0 && lyricsPane.plain.length === 0
        icon.name: "view-media-lyrics"
        text: lyricsPane.lyricsError || qsTr("No lyrics available")
    }
    QQC2.BusyIndicator { anchors.centerIn: parent; running: lyricsPane.lyricsBusy; visible: running }
}
