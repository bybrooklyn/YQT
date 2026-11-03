// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtMultimedia
import org.kde.ytm

// The single seam between the UI and whatever plays the audio.
//
// Everything in the UI talks to this object (position, duration, play/pause/seek, end-of-track, errors) and never
// to MediaPlayer directly. Today it is backed by Qt Multimedia; the Rust audio engine replaces the inside of this
// file without any other QML changing.
//
// `position` is what the player reports (coarse); `smoothPosition` is extrapolated every frame by PlaybackClock
// (src/clock.rs) and is what progress bars and synced lyrics should read.
Item {
    id: pb
    visible: false
    width: 0
    height: 0

    // ---- inputs
    property url source
    /// Linear 0..1; the perceptual (squared) curve is applied here.
    property real volume: 1.0
    /// Start playing as soon as a new source is ready (off while restoring a saved session).
    property bool autoplay: true

    // ---- state
    readonly property int position: player.position
    readonly property real smoothPosition: _smooth
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
    function toggle() { if (playing) pause(); else play() }
    /// Jump to `ms`; the smooth position follows at once instead of waiting for the next report.
    function seek(ms) {
        const target = Math.max(0, ms)
        player.position = target
        clock.snap(target, Date.now())
        _smooth = target
    }

    property real _smooth: 0

    PlaybackClock { id: clock }

    function _resync() {
        clock.report(player.position, Date.now(), pb.playing)
        _smooth = clock.positionAt(Date.now())
    }

    // Advance the displayed position every rendered frame while playing.
    FrameAnimation {
        running: pb.playing
        onTriggered: pb._smooth = clock.positionAt(Date.now())
    }

    MediaPlayer {
        id: player
        source: pb.source
        audioOutput: AudioOutput { volume: Math.pow(pb.volume, 2) }

        onSourceChanged: {
            clock.snap(0, Date.now())
            pb._smooth = 0
            if (pb.autoplay && source.toString() !== "") play()
        }
        onPositionChanged: pb._resync()
        onPlaybackStateChanged: pb._resync()
        onErrorOccurred: (error, errorString) => pb.failed(errorString)
        onMediaStatusChanged: {
            if (mediaStatus === MediaPlayer.LoadedMedia) pb.loaded()
            else if (mediaStatus === MediaPlayer.EndOfMedia) pb.ended()
        }
    }
}
