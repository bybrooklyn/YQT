// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Effects
import org.kde.ytm
import org.kde.kirigami as Kirigami

// Album/track cover that never looks broken: a neutral placeholder shows while the image loads, when there is
// no image, or when it fails to load, and the real image fades in over it once ready.
//
// `radiusPercent` rounds the corners (as a percentage of the shorter side; 0 = square). Rounding uses a mask
// effect, so it needs a GPU scene-graph backend; on the software backend the corners simply stay square.
Item {
    id: root

    property string source
    /// Size the image is decoded at (px). Keep it near the displayed size ×2 so lists stay light.
    property int decode: 256
    property int fillMode: Image.PreserveAspectCrop
    /// Corner radius as a percentage of the shorter side (e.g. 5 = slightly rounded, 50 = circle).
    property real radiusPercent: 0
    /// What the art stands for: `song`, `album`, `artist`, `playlist` or `liked` (Liked music). Decides the
    /// placeholder, which is always drawn so an art slot is never blank. Rows get it from the `placeholder` role.
    property string kind: "song"
    /// Icon shown in the placeholder; follows `kind` unless a caller sets it.
    property string placeholderIcon: {
        switch (kind) {
        case "artist": return "view-media-artist"
        case "playlist": return "view-media-playlist"
        case "liked": return "starred-symbolic"
        default: return "media-optical-audio"   // song, album
        }
    }
    readonly property bool liked: kind === "liked"
    readonly property bool ready: img.status === Image.Ready
    readonly property real _radius: Math.min(width, height) * Math.max(0, Math.min(radiusPercent, 50)) / 100

    // http(s) covers go through the cover cache: the local copy when there is one (so covers work offline and
    // load fast), otherwise the original URL while the cache fetches it in the background.
    property string _resolved: ""
    function _resolve() {
        _resolved = (source.indexOf("https://") === 0) ? Covers.url(source) : source
    }
    onSourceChanged: _resolve()
    Component.onCompleted: _resolve()
    Connections {
        target: Covers
        // a new cover arrived: switch over only if this one has not loaded yet (no flicker for loaded images)
        function onRevisionChanged() {
            if (img.status !== Image.Ready && root.source.indexOf("https://") === 0) root._resolve()
        }
    }

    // everything that must be clipped to the rounded shape lives in `content`
    Item {
        id: content
        anchors.fill: parent
        layer.enabled: root.radiusPercent > 0
        layer.effect: MultiEffect {
            maskEnabled: true
            maskSource: mask
            maskThresholdMin: 0.5
            maskSpreadAtMin: 1.0
        }

        Rectangle {
            anchors.fill: parent
            color: Kirigami.Theme.alternateBackgroundColor
            // Liked music: the purple "thumbs-up" look of YouTube's own cover
            gradient: root.liked ? likedGradient : null

            Gradient {
                id: likedGradient
                orientation: Gradient.Vertical
                GradientStop { position: 0.0; color: "#8e5bd9" }
                GradientStop { position: 1.0; color: "#4a2a8f" }
            }

            Kirigami.Icon {
                anchors.centerIn: parent
                width: Math.min(parent.width, parent.height) * 0.5
                height: width
                source: root.placeholderIcon
                color: root.liked ? "white" : Kirigami.Theme.textColor
                isMask: root.liked
                opacity: root.liked ? 0.95 : 0.35
            }
        }

        Image {
            id: img
            anchors.fill: parent
            source: root._resolved
            sourceSize: Qt.size(root.decode, root.decode)
            asynchronous: true
            cache: true
            fillMode: root.fillMode
            opacity: status === Image.Ready ? 1 : 0
            Behavior on opacity { NumberAnimation { duration: 180 } }
        }
    }

    // the mask: an opaque rounded rectangle, invisible itself
    Item {
        id: mask
        anchors.fill: parent
        visible: false
        layer.enabled: true
        Rectangle {
            anchors.fill: parent
            radius: root._radius
            color: "black"
        }
    }
}
