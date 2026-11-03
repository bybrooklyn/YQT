// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// The one place that controls playback.
//
//   ───────────── slim progress strip along the top edge ─────────────
//   [⏮ ▶ ⏭]  [cover]  Title / Artist         0:42 / 3:33   ♡ ＋ ⤮ ⟳ ♫ ☰ 🔊
//
// Transport (previous / play-pause / next) sits on the LEFT, with play as the prominent round button.
QQC2.ToolBar {
    id: bar
    readonly property var media: applicationWindow().media
    readonly property bool narrow: width < Kirigami.Units.gridUnit * 40
    readonly property bool liked: { Backend.likedRev; return Backend.isLiked(Player.videoId) }

    visible: Player.hasTrack
    position: QQC2.ToolBar.Footer
    padding: 0
    topPadding: 0

    // Pinned to the *Active* colour group: when the window loses focus the style would otherwise switch to the
    // Inactive group and the blue progress/highlight turns dark purple.
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.Window
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    function fmt(ms) {
        const s = Math.floor(Math.max(0, ms) / 1000)
        return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0")
    }

    contentItem: ColumnLayout {
        spacing: 0

        // ---- progress strip (top edge) ------------------------------------------------------------------------
        QQC2.Slider {
            id: seek
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 0.9     // generous hit area, slim visuals
            padding: 0
            from: 0
            to: Math.max(bar.media.duration, 1)
            value: bar.media.smoothPosition   // advances every frame, not once per player report
            enabled: bar.media.duration > 0
            onMoved: bar.media.seek(value)
            hoverEnabled: true

            background: Item {
                x: seek.leftPadding
                y: seek.topPadding
                width: seek.availableWidth
                height: seek.availableHeight

                Rectangle {   // the visible track: thin, grows on hover
                    id: track
                    width: parent.width
                    height: (seek.hovered || seek.pressed) ? Kirigami.Units.smallSpacing * 1.5 : Kirigami.Units.smallSpacing * 0.75
                    anchors.top: parent.top
                    color: Qt.alpha(Kirigami.Theme.textColor, 0.14)
                    Behavior on height { NumberAnimation { duration: 120 } }

                    Rectangle {
                        width: seek.visualPosition * parent.width
                        height: parent.height
                        color: Kirigami.Theme.highlightColor
                    }
                }
            }
            handle: Rectangle {
                visible: seek.hovered || seek.pressed
                x: seek.leftPadding + seek.visualPosition * (seek.availableWidth - width)
                y: track.height / 2 - height / 2
                width: Kirigami.Units.gridUnit * 0.8
                height: width
                radius: width / 2
                color: Kirigami.Theme.highlightColor
            }
        }

        // ---- main row ---------------------------------------------------------------------------------------------
        RowLayout {
            Layout.fillWidth: true
            Layout.leftMargin: Kirigami.Units.largeSpacing
            Layout.rightMargin: Kirigami.Units.largeSpacing
            Layout.topMargin: Kirigami.Units.smallSpacing
            Layout.bottomMargin: Kirigami.Units.smallSpacing
            spacing: Kirigami.Units.largeSpacing

            // -- transport, on the left
            RowLayout {
                spacing: Kirigami.Units.smallSpacing

                QQC2.ToolButton {
                    icon.name: "media-skip-backward"
                    icon.width: Kirigami.Units.iconSizes.medium
                    icon.height: Kirigami.Units.iconSizes.medium
                    onClicked: applicationWindow().previousTrack()
                    QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: qsTr("Previous")
                }
                QQC2.RoundButton {
                    id: playPause
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 3
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 3
                    highlighted: true
                    icon.name: Player.loading ? "process-working" : (bar.media.playing ? "media-playback-pause" : "media-playback-start")
                    icon.width: Kirigami.Units.iconSizes.medium
                    icon.height: Kirigami.Units.iconSizes.medium
                    onClicked: bar.media.toggle()
                    QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: bar.media.playing ? qsTr("Pause") : qsTr("Play")
                }
                QQC2.ToolButton {
                    icon.name: "media-skip-forward"
                    icon.width: Kirigami.Units.iconSizes.medium
                    icon.height: Kirigami.Units.iconSizes.medium
                    onClicked: Player.next()
                    QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: qsTr("Next")
                }
            }

            // -- cover + title
            CoverImage {
                source: Player.thumbnail
                decode: 256
                radiusPercent: AppConfig.coverRadiusPercent
                Layout.preferredWidth: Kirigami.Units.gridUnit * 3.6
                Layout.preferredHeight: Kirigami.Units.gridUnit * 3.6
                TapHandler { onTapped: applicationWindow().toggleNowPlaying() }
                HoverHandler { cursorShape: Qt.PointingHandCursor }
            }
            ColumnLayout {
                spacing: 0
                Layout.fillWidth: true
                Layout.minimumWidth: Kirigami.Units.gridUnit * 6
                TapHandler { onTapped: applicationWindow().toggleNowPlaying() }
                HoverHandler { cursorShape: Qt.PointingHandCursor }
                QQC2.Label {
                    text: Player.title
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                    font.bold: true
                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.1
                }
                QQC2.Label {
                    text: Player.error || (Player.loading ? qsTr("Loading…") : Player.artist)
                    color: Player.error ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.textColor
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                    opacity: 0.7
                }
            }

            // -- elapsed / total
            QQC2.Label {
                visible: !bar.narrow
                text: bar.fmt(bar.media.smoothPosition) + " / " + bar.fmt(bar.media.duration)
                font: Kirigami.Theme.smallFont
                opacity: 0.7
            }

            // -- right-hand controls
            QQC2.ToolButton {
                visible: Backend.loggedIn
                icon.name: bar.liked ? "starred-symbolic" : "non-starred-symbolic"
                onClicked: Backend.setLiked(Player.videoId, !bar.liked)
                QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: bar.liked ? qsTr("Remove from liked music") : qsTr("Like")
            }
            QQC2.ToolButton {
                visible: Backend.loggedIn && !bar.narrow
                icon.name: "list-add"
                onClicked: applicationWindow().addToPlaylist({videoId: Player.videoId, title: Player.title})
                QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: qsTr("Add to playlist")
            }
            QQC2.ToolButton {
                visible: !bar.narrow
                icon.name: "media-playlist-shuffle"
                checkable: true
                checked: Player.shuffle
                onClicked: Player.shuffle = checked
                QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: qsTr("Shuffle")
            }
            QQC2.ToolButton {
                visible: !bar.narrow
                icon.name: Player.repeat === 2 ? "media-playlist-repeat-song" : "media-playlist-repeat"
                checkable: true
                checked: Player.repeat !== 0
                onClicked: Player.repeat = (Player.repeat + 1) % 3
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.text: Player.repeat === 0 ? qsTr("Repeat: off") : (Player.repeat === 1 ? qsTr("Repeat: all") : qsTr("Repeat: one"))
            }
            QQC2.ToolButton {
                icon.name: "view-media-lyrics"
                onClicked: applicationWindow().showLyrics()
                QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: qsTr("Lyrics")
            }
            QQC2.ToolButton {
                icon.name: "view-media-playlist"
                onClicked: applicationWindow().go("queue")
                QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: qsTr("Queue")
            }
            RowLayout {
                visible: !bar.narrow
                spacing: Kirigami.Units.smallSpacing
                Kirigami.Icon { source: "audio-volume-high"; implicitWidth: Kirigami.Units.iconSizes.smallMedium; implicitHeight: implicitWidth }
                QQC2.Slider {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 6
                    from: 0; to: 1
                    value: Player.volume
                    onMoved: Player.volume = value
                }
            }
        }
    }
}
