// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm

// The single seam between the UI and whatever plays the audio.
//
// Everything in the UI talks to this object (position, duration, play/pause/seek, end-of-track, errors) and never
// to a player directly. A Loader picks the backend from `audio.engine`: RustPlayback.qml (the pure-Rust engine) or
// QtPlayback.qml (Qt Multimedia, the only file that imports it). The Loader takes the backend by URL, so the one that
// is not selected is never even parsed and Qt Multimedia is not loaded when the Rust engine is active.
//
// `position` is what the backend reports (coarse); `smoothPosition` is extrapolated every frame by PlaybackClock
// (src/clock.rs) and is what progress bars and synced lyrics should read.
Item {
    id: pb
    visible: false
    width: 0
    height: 0

    // ---- inputs
    property url source
    /// Linear 0..1; the perceptual (squared) curve is applied by the backend.
    property real volume: 1.0
    /// Start playing as soon as a new source is ready (off while restoring a saved session).
    property bool autoplay: true
    /// Optional: the track that follows `source`. With the Rust engine it is decoded ahead for gapless / crossfade.
    property url nextSource
    /// Update `smoothPosition` every frame (karaoke lyrics) instead of every 250 ms.
    property bool smooth: false

    // ---- state
    readonly property int position: _b ? _b.position : 0
    readonly property real smoothPosition: _smooth
    readonly property int duration: _b ? _b.duration : 0
    readonly property bool playing: _b ? _b.playing : false
    readonly property bool hasSource: _b ? _b.hasSource : false

    // ---- events
    signal loaded()
    signal ended()
    signal failed(string message)

    function play() { if (_b) _b.play() }
    function pause() { if (_b) _b.pause() }
    function stop() { if (_b) _b.stop() }
    function toggle() { if (playing) pause(); else play() }
    /// Jump to `ms`; the smooth position follows at once instead of waiting for the next report.
    function seek(ms) {
        const target = Math.max(0, ms)
        if (_b) _b.seek(target)
        clock.snap(target, Date.now())
        _smooth = target
    }

    readonly property var _b: backend.item
    property real _smooth: 0

    PlaybackClock { id: clock }

    function _resync() {
        clock.report(pb.position, Date.now(), pb.playing)
        _smooth = clock.positionAt(Date.now())
    }

    onSourceChanged: {
        clock.snap(0, Date.now())
        _smooth = 0
    }

    // Advance the displayed position every rendered frame only while something needs it (`smooth`: the synced
    // lyrics are on screen). A FrameAnimation makes the window redraw at the monitor's refresh rate, so otherwise
    // the position is stepped four times a second, which is all a progress bar shows.
    FrameAnimation {
        running: pb.playing && pb.smooth
        onTriggered: pb._smooth = clock.positionAt(Date.now())
    }
    Timer {
        interval: 250
        repeat: true
        running: pb.playing && !pb.smooth
        onTriggered: pb._smooth = clock.positionAt(Date.now())
    }

    // The engine is chosen once, at start-up: changing "Audio engine" takes effect after a restart (as the setting says),
    // never by swapping the backend under a playing track.
    Loader {
        id: backend
        Component.onCompleted: source = AppConfig.audioEngine === "rust" ? "RustPlayback.qml" : "QtPlayback.qml"
    }
    // `source` goes last: the backend decides whether to start playing from `autoplay` the moment the source arrives.
    Binding { target: backend.item; property: "volume"; value: pb.volume; when: backend.item !== null }
    Binding { target: backend.item; property: "autoplay"; value: pb.autoplay; when: backend.item !== null }
    Binding { target: backend.item; property: "nextSource"; value: pb.nextSource; when: backend.item !== null }
    Binding { target: backend.item; property: "source"; value: pb.source; when: backend.item !== null }

    Connections {
        target: pb._b
        ignoreUnknownSignals: true
        function onPositionChanged() { pb._resync() }
        function onPlayingChanged() { pb._resync() }
        function onLoaded() { pb.loaded() }
        function onEnded() { pb.ended() }
        function onFailed(message) { pb.failed(message) }
    }
}
