// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// "Lyrics": a big, tinted, karaoke-style player for time-synced lines, or plain text. (Owned by the lyrics workstream.)
Item {
    id: lyricsPane

    /// True while this panel is the visible one; lyrics are only fetched then.
    property bool active: false
    /// Space kept free on the right for the scroll bar.
    property real gutter: 0
    readonly property var media: applicationWindow().media

    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    readonly property bool animate: !AppConfig.reduceMotion

    // ---- the cover's colour --------------------------------------------------------------
    Accent { id: accent; source: Player.cover }
    readonly property color accentColor: accent.ready && AppConfig.accentFromCover ? accent.color : Kirigami.Theme.highlightColor
    readonly property color fillColor: AppConfig.lyricsTint ? accentColor : Kirigami.Theme.textColor

    // ---- type size follows the pane width ------------------------------------------------
    readonly property real pixelSize: Math.max(20, Math.min(56, (width - gutter) * 0.055)) * AppConfig.lyricsSizeScale

    // ---- lookup --------------------------------------------------------------------------
    Lyrics { id: lyrics }
    property string lyricsFor              // videoId the current lyrics belong to

    function seconds() {
        const p = Player.duration.split(":").map(Number)
        if (p.some(isNaN)) return 0
        return p.reduce((acc, v) => acc * 60 + v, 0)
    }
    function fetchLyrics() {
        if (!Player.hasTrack || lyricsFor === Player.videoId) return
        lyricsFor = Player.videoId
        lyrics.request(Player.videoId, Player.title, Player.artist, seconds())
    }
    onActiveChanged: if (active) fetchLyrics()
    Component.onCompleted: if (active) fetchLyrics()
    Connections {
        target: Player
        // the title/artist/duration are set right after the id: look them up once they are all in
        function onVideoIdChanged() { lyricsPane.lyricsFor = ""; if (lyricsPane.active) Qt.callLater(lyricsPane.fetchLyrics) }
    }

    // ---- rows ----------------------------------------------------------------------------
    // [{start, text, end, gap}], then the credit as the very last row, so it only shows once you reach the end.
    readonly property var rows: {
        if (lyrics.state !== "synced") return []
        let items = []
        try { items = JSON.parse(lyrics.linesJson) } catch (e) { items = [] }
        const out = items.map(x => ({ start: x[0], text: x[1], end: x[2], gap: x[3], credit: false }))
        if (lyrics.source.length > 0)
            out.push({ start: -1, text: "— " + qsTr("Lyrics: %1").arg(lyrics.source), end: -1, gap: false, credit: true })
        return out
    }
    readonly property int lineCount: lyrics.state === "synced" ? rows.length - (lyrics.source.length > 0 ? 1 : 0) : 0

    // The line being sung. `rawLine` follows the player position exactly; `line` is what is shown.
    // Moving forward is applied at once. Moving *backward* only counts once the position has stayed
    // there for a moment, because the position can glitch backwards for an instant (stream range
    // requests, buffering) – which used to make the highlight flash off and on again.
    readonly property int rawLine: {
        const p = lyricsPane.media.smoothPosition + AppConfig.lyricsLeadMs
        const s = lyricsPane.rows
        let lo = 0, hi = Math.min(lyricsPane.lineCount, s.length) - 1, r = -1
        while (lo <= hi) {
            const mid = (lo + hi) >> 1
            if (s[mid].start <= p) { r = mid; lo = mid + 1 } else hi = mid - 1
        }
        return r
    }
    property int line: -1
    onRawLineChanged: {
        if (rawLine >= line) { backTimer.stop(); line = rawLine }
        else backTimer.restart()
    }
    Timer { id: backTimer; interval: 500; onTriggered: lyricsPane.line = lyricsPane.rawLine }
    onRowsChanged: { backTimer.stop(); line = rawLine; Qt.callLater(lyricsList.follow, true) }
    onLineChanged: lyricsList.follow(false)

    ListView {
        id: lyricsList
        anchors.fill: parent
        visible: lyrics.state === "synced"
        clip: true
        model: lyricsPane.rows
        topMargin: height * 0.4              // the current line rests ~40% from the top
        bottomMargin: height * 0.6
        reuseItems: false
        QQC2.ScrollBar.vertical: QQC2.ScrollBar {}

        NumberAnimation { id: scrollAnim; target: lyricsList; property: "contentY"; duration: 420; easing.type: Easing.OutCubic }
        function follow(instant) {
            const cur = lyricsPane.line
            if (dragging || flicking) return
            if (cur < 0) { scrollAnim.stop(); positionViewAtBeginning(); return }
            const it = itemAtIndex(cur)
            if (!it) { positionViewAtIndex(cur, ListView.Center); return }
            const to = it.y + it.height / 2 - height * 0.4
            scrollAnim.stop()
            if (instant === true || !lyricsPane.animate) { contentY = to; return }
            scrollAnim.from = contentY
            scrollAnim.to = to
            scrollAnim.start()
        }

        delegate: LyricLine {
            id: row
            required property int index
            required property var modelData
            width: ListView.view.width - lyricsPane.gutter
            text: modelData.text
            gap: modelData.gap
            credit: modelData.credit
            current: !modelData.credit && index === lyricsPane.line
            distance: Math.abs(index - lyricsPane.line)
            pixelSize: lyricsPane.pixelSize
            karaoke: AppConfig.lyricsKaraoke
            animate: lyricsPane.animate
            baseColor: Kirigami.Theme.textColor
            fillColor: lyricsPane.fillColor
            // Only the current row reads the clock every frame; the others are simply sung or not yet.
            progress: current
                ? Math.max(0, Math.min(1, (lyricsPane.media.smoothPosition + AppConfig.lyricsLeadMs - modelData.start) / Math.max(1, modelData.end - modelData.start)))
                : (index < lyricsPane.line ? 1 : 0)
            // instrumental dots can be switched off: the row then collapses
            visible: !modelData.gap || AppConfig.lyricsGapDots
            height: visible ? implicitHeight : 0
            onTapped: {
                lyricsPane.media.seek(modelData.start)
                backTimer.stop()
                lyricsPane.line = index          // follow an explicit seek immediately
            }
        }
    }

    // ---- plain text ----------------------------------------------------------------------
    QQC2.ScrollView {
        id: plainView
        anchors.fill: parent
        visible: lyrics.state === "plain"
        // Wrap to the pane: without this the label is as wide as its longest line.
        contentWidth: availableWidth
        QQC2.ScrollBar.horizontal.policy: QQC2.ScrollBar.AlwaysOff
        ColumnLayout {
            width: plainView.availableWidth - lyricsPane.gutter
            spacing: Kirigami.Units.largeSpacing
            Text {
                Layout.fillWidth: true
                Layout.margins: Kirigami.Units.largeSpacing
                text: lyrics.plain
                wrapMode: Text.Wrap
                horizontalAlignment: Text.AlignHCenter
                font.pixelSize: Math.max(16, lyricsPane.pixelSize * 0.8)
                font.weight: Font.DemiBold
                color: Kirigami.Theme.textColor
                opacity: 0.85
            }
            // same closing credit as the synced list: last thing in the text
            Text {
                Layout.fillWidth: true
                Layout.bottomMargin: Kirigami.Units.gridUnit * 2
                visible: lyrics.source.length > 0
                text: "— " + qsTr("Lyrics: %1").arg(lyrics.source)
                horizontalAlignment: Text.AlignHCenter
                font.pixelSize: Math.max(12, lyricsPane.pixelSize * 0.38)
                font.weight: Font.DemiBold
                color: Kirigami.Theme.textColor
                opacity: 0.38
            }
        }
    }

    // ---- states without lyrics -----------------------------------------------------------
    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - Kirigami.Units.gridUnit * 4
        visible: lyrics.state === "unavailable"
        icon.name: "view-media-lyrics"
        text: qsTr("Lyrics unavailable")
    }
    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - Kirigami.Units.gridUnit * 4
        visible: lyrics.state === "failed"
        icon.name: "dialog-warning"
        text: lyrics.errorText
        helpfulAction: Kirigami.Action {
            icon.name: "view-refresh"
            text: qsTr("Retry")
            onTriggered: lyrics.retry()
        }
        QQC2.ToolTip.visible: failedHover.hovered && lyrics.errorDetail.length > 0
        QQC2.ToolTip.text: lyrics.errorDetail
        HoverHandler { id: failedHover }
    }
    QQC2.BusyIndicator {
        anchors.centerIn: parent
        running: lyrics.state === "loading"
        visible: running
    }
}
