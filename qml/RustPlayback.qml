// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm

// The pure-Rust backend of the Playback seam (audio.engine = "rust"): symphonia + opus-rs + rubato + EQ + cpal, no
// FFmpeg and no Qt Multimedia. The position comes from the engine's sample-accurate frame counter.
Item {
    id: rb
    visible: false
    width: 0
    height: 0

    // ---- inputs (bound by Playback.qml)
    property url source
    property real volume: 1.0
    property bool autoplay: true
    /// The track to continue with seamlessly; its start is decoded ahead so the switch has no gap (and can crossfade).
    property url nextSource

    // ---- state
    readonly property int position: engine.position
    readonly property int duration: engine.duration
    readonly property bool playing: engine.playing
    readonly property bool hasSource: source.toString() !== ""

    // ---- events
    signal loaded()
    signal ended()
    signal failed(string message)

    function play() { engine.play() }
    function pause() { engine.pause() }
    function stop() { engine.stop() }
    function seek(ms) { engine.seek(Math.max(0, ms)) }

    property double _endedAt: 0

    AudioEngine {
        id: engine
        onLoaded: rb.loaded()
        onEnded: { rb._endedAt = Date.now(); rb.ended() }
        onFailed: message => rb.failed(message)
    }

    // Player clears `source` between two tracks. Right after a track ended the engine may already be playing the next
    // one (gapless), so an empty source is only acted on if no new source follows shortly.
    Timer { id: clearTimer; interval: 400; onTriggered: engine.stop() }

    function _applySource() {
        const s = source.toString()
        if (s === "") {
            if (Date.now() - _endedAt < 2000) clearTimer.restart(); else engine.stop()
            return
        }
        clearTimer.stop()
        engine.load(s)           // a no-op when the engine has already moved on to this very track
        if (autoplay) engine.play(); else engine.pause()
        engine.setNext(nextSource.toString())
    }

    onSourceChanged: _applySource()
    onNextSourceChanged: engine.setNext(nextSource.toString())
    onVolumeChanged: engine.setVolume(Math.pow(volume, 2))

    // Settings changes apply to the running engine at once.
    Connections {
        target: AppConfig
        function onAudioCrossfadeMsChanged() { engine.setCrossfadeMs(AppConfig.audioCrossfadeMs) }
        function onAudioNormalizeChanged() { engine.setNormalize(AppConfig.audioNormalize) }
        function onAudioGaplessChanged() { engine.setGapless(AppConfig.audioGapless) }
        function onAudioOutputDeviceChanged() { engine.setOutputDevice(AppConfig.audioOutputDevice) }
    }

    Component.onCompleted: {
        engine.setVolume(Math.pow(volume, 2))
        engine.setOutputDevice(AppConfig.audioOutputDevice)
        engine.setCrossfadeMs(AppConfig.audioCrossfadeMs)
        engine.setNormalize(AppConfig.audioNormalize)
        engine.setGapless(AppConfig.audioGapless)
        if (source.toString() !== "") _applySource()
    }
}
