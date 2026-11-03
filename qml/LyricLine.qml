// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.kirigami as Kirigami

// One row of the lyric player: a sung line, an instrumental gap (dots) or the closing credit.
// Every row uses the same font and size so the layout never shifts when the current line changes;
// only colour and opacity change. The karaoke fill is a second copy of the text, in the fill colour,
// clipped per visual line to how far through the line we are.
Item {
    id: root

    property string text
    /// An instrumental break: shows dots instead of text.
    property bool gap: false
    /// The closing "— Lyrics: …" row: smaller, dim, not interactive.
    property bool credit: false
    property bool current: false
    /// Rows between this one and the current one (0 = current).
    property int distance: 0
    property real pixelSize: 28
    /// 0..1 through this line (only meaningful for the current line; 1 once sung, 0 before).
    property real progress: 0
    property bool karaoke: true
    property bool animate: true
    property color baseColor: Kirigami.Theme.textColor
    property color fillColor: Kirigami.Theme.highlightColor
    signal tapped()

    readonly property bool fillWanted: karaoke && animate && !gap && !credit
    // 1 for the current row, 0 for the others; animated so the colour change is smooth.
    property real glow: current ? 1 : 0
    Behavior on glow { enabled: root.animate; NumberAnimation { duration: 280; easing.type: Easing.OutCubic } }

    readonly property real dim: credit ? 0.38 : Math.max(0.16, 0.6 - 0.12 * (Math.max(distance, 1) - 1))
    readonly property real restOpacity: current ? (fillWanted ? 0.5 : 1) : dim
    readonly property real shownOpacity: dim + (restOpacity - dim) * glow
    readonly property real textSize: credit ? pixelSize * 0.62 : pixelSize

    implicitHeight: gap ? dots.implicitHeight : label.implicitHeight + pixelSize * 0.7

    // visual line rectangles of the label, collected while it lays out: [{x, y, w, h}, ...]
    property var lineRects: []
    property var collecting: []

    Text {
        id: label
        visible: !root.gap
        anchors.centerIn: parent
        width: parent.width * 0.84          // headroom so a line never clips at the edges
        text: root.text
        wrapMode: Text.Wrap
        horizontalAlignment: Text.AlignHCenter
        font.pixelSize: root.textSize
        font.weight: Font.DemiBold
        color: (root.current && !root.fillWanted) ? root.fillColor : root.baseColor
        opacity: root.shownOpacity
        onLineLaidOut: line => {
            if (line.number === 0) root.collecting = []
            root.collecting.push({ x: line.x, y: line.y, w: line.implicitWidth, h: line.height })
            root.lineRects = root.collecting.slice()     // assign a new array so bindings notice
        }
    }

    // ---- karaoke fill --------------------------------------------------------------------
    Loader {
        id: fill
        anchors.fill: parent
        active: root.fillWanted && root.glow > 0.01 && root.lineRects.length > 0
        sourceComponent: Item {
            readonly property real total: {
                let t = 0
                for (const r of root.lineRects) t += r.w
                return t
            }
            opacity: root.glow
            Repeater {
                model: root.lineRects
                Item {
                    id: seg
                    required property var modelData
                    required property int index
                    readonly property real before: {
                        let t = 0
                        for (let i = 0; i < index; ++i) t += root.lineRects[i].w
                        return t
                    }
                    readonly property real filled: Math.max(0, Math.min(modelData.w, root.progress * total - before))
                    // lines are centred: the line's own left edge, then the filled part of it
                    x: label.x + (label.width - modelData.w) / 2
                    y: label.y + modelData.y
                    width: filled
                    height: modelData.h
                    clip: true
                    Text {
                        x: -((label.width - seg.modelData.w) / 2)
                        y: -seg.modelData.y
                        width: label.width
                        text: label.text
                        wrapMode: label.wrapMode
                        horizontalAlignment: label.horizontalAlignment
                        font: label.font
                        color: root.fillColor
                    }
                }
            }
        }
    }

    LyricGap {
        id: dots
        visible: root.gap
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        pixelSize: root.pixelSize
        color: root.fillColor
        active: root.current
        progress: root.progress
        animate: root.animate
        opacity: root.current ? 1 : root.dim + 0.1
    }

    TapHandler {
        enabled: !root.credit
        onTapped: root.tapped()
    }
}
