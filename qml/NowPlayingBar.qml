// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

QQC2.ToolBar {
    id: bar
    readonly property var media: applicationWindow().media
    visible: Player.hasTrack
    position: QQC2.ToolBar.Footer

    function fmt(ms) {
        const s = Math.floor(ms / 1000)
        return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0")
    }

    contentItem: ColumnLayout {
        spacing: 0

        RowLayout {
            Layout.fillWidth: true
            QQC2.Label { text: bar.fmt(media.smoothPosition); font: Kirigami.Theme.smallFont }
            QQC2.Slider {
                Layout.fillWidth: true
                from: 0
                to: Math.max(media.duration, 1)
                value: media.smoothPosition      // advances every frame, not once per player report
                enabled: media.duration > 0
                onMoved: media.seek(value)
            }
            QQC2.Label { text: bar.fmt(media.duration); font: Kirigami.Theme.smallFont }
        }

        RowLayout {
            spacing: Kirigami.Units.smallSpacing

            CoverImage {
                source: Player.thumbnail
                decode: 192
                Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                Layout.preferredHeight: Kirigami.Units.gridUnit * 2.5
                TapHandler { onTapped: applicationWindow().showNowPlaying("queue") }
            }
            ColumnLayout {
                spacing: 0
                Layout.fillWidth: true
                TapHandler { onTapped: applicationWindow().showNowPlaying("queue") }
                QQC2.Label {
                    text: Player.title
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                    font.bold: true
                }
                QQC2.Label {
                    text: Player.error || (Player.loading ? qsTr("Loading…") : (Player.artist))
                    color: Player.error ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.textColor
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                    opacity: 0.7
                }
            }

            QQC2.ToolButton {
                visible: Backend.loggedIn
                readonly property bool liked: { Backend.likedRev; return Backend.isLiked(Player.videoId) }
                icon.name: liked ? "starred-symbolic" : "non-starred-symbolic"
                onClicked: Backend.setLiked(Player.videoId, !liked)
                QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: liked ? qsTr("Remove from liked music") : qsTr("Like")
            }
            QQC2.ToolButton {
                visible: Backend.loggedIn
                icon.name: "list-add"
                onClicked: applicationWindow().addToPlaylist({videoId: Player.videoId, title: Player.title})
                QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: qsTr("Add to playlist")
            }
            QQC2.ToolButton {
                icon.name: "media-playlist-shuffle"
                checkable: true
                checked: Player.shuffle
                onClicked: Player.shuffle = checked
                QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: qsTr("Shuffle")
            }
            QQC2.ToolButton {
                icon.name: "media-skip-backward"
                onClicked: applicationWindow().previousTrack()
            }
            QQC2.ToolButton {
                icon.name: Player.loading ? "process-working" : (media.playing ? "media-playback-pause" : "media-playback-start")
                onClicked: media.toggle()
            }
            QQC2.ToolButton {
                icon.name: "media-skip-forward"
                onClicked: Player.next()
            }
            QQC2.ToolButton {
                icon.name: Player.repeat === 2 ? "media-playlist-repeat-song" : "media-playlist-repeat"
                checkable: true
                checked: Player.repeat !== 0
                onClicked: Player.repeat = (Player.repeat + 1) % 3
                QQC2.ToolTip.visible: hovered; QQC2.ToolTip.text: Player.repeat === 0 ? qsTr("Repeat: off") : (Player.repeat === 1 ? qsTr("Repeat: all") : qsTr("Repeat: one"))
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
                visible: applicationWindow().width > Kirigami.Units.gridUnit * 40
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
