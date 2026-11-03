// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// The "now playing" view: a big cover on one side, Up next / Lyrics on the other.
// There are deliberately no transport controls here – the bar at the bottom is the one place to control playback.
// The two panels live in QueuePanel.qml and LyricsPanel.qml.
Kirigami.Page {
    id: page
    readonly property bool isNowPlaying: true
    property string tab: "queue"          // "queue" | "lyrics"
    readonly property bool wide: width > Kirigami.Units.gridUnit * 42
    /// Space reserved at the right of both panels for their scroll bars; the switcher above uses the same value.
    readonly property real gutter: Kirigami.Units.gridUnit * 0.9

    title: qsTr("Now Playing")
    padding: Kirigami.Units.largeSpacing

    function goToArtist() { applicationWindow().open({kind: "artist", browseId: Player.artistId, title: Player.artist}) }
    function goToAlbum() { applicationWindow().open({kind: "album", browseId: Player.albumId, title: Player.title}) }

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
                radiusPercent: AppConfig.coverRadiusPercent
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
            spacing: Kirigami.Units.largeSpacing

            PanelSwitcher {
                Layout.fillWidth: true
                Layout.rightMargin: page.gutter          // end where the panels' content ends, clear of the scroll bar
                labels: [qsTr("Up next"), qsTr("Lyrics")]
                currentIndex: page.tab === "lyrics" ? 1 : 0
                onCurrentIndexChanged: page.tab = currentIndex === 1 ? "lyrics" : "queue"
            }

            StackLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: page.tab === "lyrics" ? 1 : 0

                QueuePanel { gutter: page.gutter }
                LyricsPanel { active: page.tab === "lyrics"; gutter: page.gutter }
            }
        }
    }
}
