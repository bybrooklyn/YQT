// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtMultimedia

// The Qt Multimedia backend of the Playback seam (audio.engine = "qt").
//
// This is the ONLY file that imports QtMultimedia or uses MediaPlayer, and Playback.qml only loads it when the
// Rust engine is not selected, so Qt Multimedia (and the FFmpeg it brings) is never loaded with audio.engine = "rust".
Item {
    id: qb
    visible: false
    width: 0
    height: 0

    // ---- inputs (bound by Playback.qml)
    property url source
    property real volume: 1.0
    property bool autoplay: true
    /// Unused here: Qt Multimedia cannot queue a following track (no gapless / crossfade).
    property url nextSource

    // ---- state
    readonly property int position: player.position
    readonly property int duration: player.duration
    readonly property bool playing: player.playbackState === MediaPlayer.PlayingState
    readonly property bool hasSource: player.source.toString() !== ""

    // ---- events
    signal loaded()
    signal ended()
    signal failed(string message)

    function play() { player.play() }
    function pause() { player.pause() }
    function stop() { player.stop() }
    function seek(ms) { player.position = Math.max(0, ms) }

    MediaPlayer {
        id: player
        source: qb.source
        audioOutput: AudioOutput { volume: Math.pow(qb.volume, 2) }

        onSourceChanged: { if (qb.autoplay && source.toString() !== "") play() }
        onErrorOccurred: (error, errorString) => qb.failed(errorString)
        onMediaStatusChanged: {
            if (mediaStatus === MediaPlayer.LoadedMedia) qb.loaded()
            else if (mediaStatus === MediaPlayer.EndOfMedia) qb.ended()
        }
    }
}
