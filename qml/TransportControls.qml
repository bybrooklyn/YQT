// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.ytm
import org.kde.kirigami as Kirigami

// The transport row shared by the bottom bar and the mini-player:
//   [shuffle]  ⏮ previous   ▶/⏸ play-pause (prominent, round, largest)   ⏭ next  [repeat]
//
// It drives the app through the existing seam only: applicationWindow().media (the Playback object),
// applicationWindow().previousTrack(), Player.next(), Player.shuffle / Player.repeat.
//
// Sizing: `size` scales every button (1 = mini-player size; the bottom bar uses ≈ 1.4). `compact` tightens the spacing
// and shrinks the buttons for very narrow windows.
Item {
    id: root

    property real size: 1.0
    property bool compact: false
    property bool showShuffleRepeat: false

    // Logical icon names live here and nowhere else, so they can be re-mapped to bundled icons later.
    readonly property var icons: ({
        previous: "media-skip-backward",
        next: "media-skip-forward",
        play: "media-playback-start",
        pause: "media-playback-pause",
        busy: "process-working",
        shuffle: "media-playlist-shuffle",
        repeat: "media-playlist-repeat",
        repeatOne: "media-playlist-repeat-song"
    })

    readonly property var media: applicationWindow().media
    readonly property bool playing: media ? media.playing : false

    readonly property real _scale: size * (compact ? 0.85 : 1.0)
    readonly property real _small: Kirigami.Units.gridUnit * 2.0 * _scale
    readonly property real _big: Kirigami.Units.gridUnit * 3.0 * _scale

    implicitWidth: row.implicitWidth
    implicitHeight: _big

    // Never follow the window's focus state (the "turns purple when unfocused" bug).
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.Button
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    // One transport button: flat and round, hover/pressed/focus feedback from the pinned theme colours.
    component TcButton: QQC2.AbstractButton {
        id: btn
        property string iconName
        property bool prominent: false
        property string tip
        property bool lit: false      // shuffle/repeat "on"
        implicitWidth: prominent ? root._big : root._small
        implicitHeight: implicitWidth
        hoverEnabled: true
        focusPolicy: Qt.TabFocus
        QQC2.ToolTip.visible: hovered && tip.length > 0
        QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        QQC2.ToolTip.text: tip
        Accessible.name: tip
        Accessible.role: Accessible.Button
        Keys.onReturnPressed: clicked()
        Keys.onEnterPressed: clicked()

        background: Rectangle {
            radius: width / 2
            color: btn.prominent
                ? (btn.down ? Qt.darker(Kirigami.Theme.highlightColor, 1.25)
                            : (btn.hovered ? Qt.lighter(Kirigami.Theme.highlightColor, 1.12) : Kirigami.Theme.highlightColor))
                : (btn.down ? Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g, Kirigami.Theme.textColor.b, 0.18)
                            : (btn.hovered ? Qt.rgba(Kirigami.Theme.textColor.r, Kirigami.Theme.textColor.g, Kirigami.Theme.textColor.b, 0.10) : "transparent"))
            opacity: btn.enabled ? 1 : 0.4
            border.width: btn.visualFocus ? 2 : 0
            border.color: Kirigami.Theme.focusColor
        }
        contentItem: Kirigami.Icon {
            source: btn.iconName
            isMask: true
            color: btn.prominent ? Kirigami.Theme.highlightedTextColor
                                 : (btn.lit ? Kirigami.Theme.highlightColor : Kirigami.Theme.textColor)
            opacity: btn.enabled ? 1 : 0.4
        }
        // padding so the glyph is smaller than the round hit area
        padding: Math.round(width * (prominent ? 0.27 : 0.24))
    }

    RowLayout {
        id: row
        anchors.verticalCenter: parent.verticalCenter
        anchors.horizontalCenter: parent.horizontalCenter
        spacing: root.compact ? Kirigami.Units.smallSpacing / 2 : Kirigami.Units.smallSpacing

        TcButton {
            visible: root.showShuffleRepeat
            iconName: root.icons.shuffle
            lit: Player.shuffle
            tip: Player.shuffle ? qsTr("Shuffle: on") : qsTr("Shuffle: off")
            onClicked: Player.shuffle = !Player.shuffle
            Accessible.checkable: true
            Accessible.checked: Player.shuffle
        }
        TcButton {
            iconName: root.icons.previous
            tip: qsTr("Previous")
            enabled: Player.hasTrack
            onClicked: applicationWindow().previousTrack()
        }
        TcButton {
            prominent: true
            iconName: Player.loading ? root.icons.busy : (root.playing ? root.icons.pause : root.icons.play)
            tip: root.playing ? qsTr("Pause") : qsTr("Play")
            enabled: Player.hasTrack
            onClicked: root.media.toggle()
        }
        TcButton {
            iconName: root.icons.next
            tip: qsTr("Next")
            enabled: Player.hasTrack
            onClicked: Player.next()
        }
        TcButton {
            visible: root.showShuffleRepeat
            iconName: Player.repeat === 2 ? root.icons.repeatOne : root.icons.repeat
            lit: Player.repeat !== 0
            tip: Player.repeat === 0 ? qsTr("Repeat: off") : (Player.repeat === 1 ? qsTr("Repeat: all") : qsTr("Repeat: one"))
            onClicked: Player.repeat = (Player.repeat + 1) % 3
        }
    }
}
