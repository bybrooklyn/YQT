// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtCore
import org.kde.ytm
import QtMultimedia
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami

Kirigami.ApplicationWindow {
    id: root

    title: Player.hasTrack ? qsTr("%1 — YT Music").arg(Player.title) : qsTr("YT Music")
    width: Kirigami.Units.gridUnit * 50
    height: Kirigami.Units.gridUnit * 36
    minimumWidth: Kirigami.Units.gridUnit * 20
    minimumHeight: Kirigami.Units.gridUnit * 20

    readonly property url itemsUrl: Qt.resolvedUrl("ItemsPage.qml")
    // Recently seen pages (see ItemsPage): makes going back instant. Personal pages differ per account, so a
    // sign-in or sign-out empties it.
    property var pageCache: ({})
    Connections {
        target: Backend
        function onLoggedInChanged() { root.pageCache = ({}) }
        // A like YouTube refused: the heart has already been put back; say why instead of failing silently.
        function onLikeFailed(videoId, message) {
            root.showPassiveNotification(qsTr("Couldn’t update your liked songs: %1").arg(message.split("\n\n")[0]), "long")
        }
    }
    // Playback lives in QtMultimedia; the Rust Player resolves URLs and owns the queue.
    property alias media: media

    MediaPlayer {
        id: media
        source: Player.source
        audioOutput: AudioOutput { volume: Math.pow(Player.volume, 2) }
        onSourceChanged: {
            clock.snap(0, Date.now()); root.smoothPosition = 0
            if (source.toString() !== "" && Player.autoplay) play()
        }
        onErrorOccurred: (error, errorString) => Player.mediaFailed(errorString)
        onPlaybackStateChanged: root.syncMpris()
        onDurationChanged: root.syncMpris()
        onMediaStatusChanged: {
            if (mediaStatus === MediaPlayer.LoadedMedia && Player.resumePos > 0) {
                position = Player.resumePos
                Player.resumePos = 0
            }
            if (mediaStatus === MediaPlayer.EndOfMedia) {
                if (Player.repeat === 2) { position = 0; play() } else Player.next()
            }
        }
    }

    // Smooth progress: the player only reports its position a few times a second. This extrapolates between the
    // reports on every frame (the rule lives in Rust, src/clock.rs), so the progress bar and the lyrics glide
    // instead of stepping. Read `smoothPosition` for anything shown; call `seek()` to jump.
    PlaybackClock { id: clock }
    property real smoothPosition: 0
    readonly property bool isPlaying: media.playbackState === MediaPlayer.PlayingState
    function resync() {
        clock.report(media.position, Date.now(), isPlaying)
        smoothPosition = clock.positionAt(Date.now())
    }
    function seek(ms) {
        const t = Math.max(0, ms)
        media.position = t
        clock.snap(t, Date.now())
        smoothPosition = t
    }
    Connections {
        target: media
        function onPositionChanged() { root.resync() }
        function onPlaybackStateChanged() { root.resync() }
    }
    FrameAnimation { running: root.isPlaying; onTriggered: root.smoothPosition = clock.positionAt(Date.now()) }

    Settings {
        id: prefs
        category: "player"
        property real volume: AppConfig.defaultVolume
        property bool shuffle: false
        property int repeat: 0
    }
    Component.onDestruction: {
        root.saveSession()
        prefs.volume = Player.volume
        prefs.shuffle = Player.shuffle
        prefs.repeat = Player.repeat
    }

    function syncMpris() {
        Player.syncMpris(media.playbackState === MediaPlayer.PlayingState, media.position, media.duration)
    }
    function previousTrack() {
        if (media.position > AppConfig.restartThresholdMs || Player.index <= 0) media.position = 0; else Player.previous()
    }
    Timer { interval: 1000; repeat: true; running: Player.hasTrack; onTriggered: root.syncMpris() }
    Connections {
        target: Player
        // Media keys, KDE Connect, the Plasma media widget (MPRIS) – see src/mpris.rs
        function onCommand(cmd, arg) {
            switch (cmd) {
            case "play": if (Player.hasTrack) media.play(); break
            case "pause": media.pause(); break
            case "playpause": root.togglePlay(); break
            case "stop": media.stop(); break
            case "next": Player.next(); break
            case "previous": root.previousTrack(); break
            case "seek": media.position = Math.max(0, media.position + arg); break
            case "setposition": media.position = arg; break
            case "shuffle": Player.shuffle = arg !== 0; break
            case "repeat": Player.repeat = arg; break
            case "volume": Player.volume = arg / 1000; break
            case "raise": root.show(); root.raise(); root.requestActivate(); break
            case "quit": Qt.quit(); break
            }
            root.syncMpris()
        }
        function onShuffleChanged() { root.syncMpris() }
        function onRepeatChanged() { root.syncMpris() }
        function onVolumeChanged() { root.syncMpris() }
        function onTitleChanged() { root.syncMpris() }
    }

    function saveSession() {
        Player.saveSession(Player.resumePos > 0 ? Player.resumePos : media.position)
    }
    Timer { interval: AppConfig.sessionSaveMs; repeat: true; running: Player.hasTrack; onTriggered: root.saveSession() }
    Connections { target: Player; function onIndexChanged() { Player.saveSession(0) }
        function onQueueChanged() { root.saveSession() } }

    function toggleFullScreen() {
        root.visibility = root.visibility === Window.FullScreen ? Window.Windowed : Window.FullScreen
    }
    function togglePlay() {
        if (!Player.hasTrack) return
        if (media.playbackState === MediaPlayer.PlayingState) media.pause(); else media.play()
    }

    Shortcut { sequences: ["F11", StandardKey.FullScreen]; onActivated: root.toggleFullScreen() }
    Shortcut { sequence: "Escape"; enabled: root.visibility === Window.FullScreen; onActivated: root.visibility = Window.Windowed }
    Shortcut { sequences: [StandardKey.Find]; onActivated: root.focusSearch() }
    Shortcut { sequence: "Ctrl+Right"; onActivated: Player.next() }
    Shortcut { sequence: "Ctrl+Left"; onActivated: root.previousTrack() }
    Shortcut { sequence: "Media Play"; onActivated: root.togglePlay() }
    Shortcut { sequence: "Media Next"; onActivated: Player.next() }
    Shortcut { sequence: "Media Previous"; onActivated: root.previousTrack() }

    function focusSearch() {
        const p = pageStack.currentItem
        if (p && p.focusSearch) p.focusSearch()
    }

    function go(name) {
        pageStack.clear()
        switch (name) {
        case "home":
            pageStack.push(itemsUrl, {title: qsTr("Home"), target: "home", reloadOnLogin: true})
            break
        case "explore":
            pageStack.push(itemsUrl, {title: qsTr("Explore"), target: "explore"})
            break
        case "library":
            pageStack.push(itemsUrl, {
                title: qsTr("Library"),
                target: "library",
                filter: "playlists",
                chips: [[qsTr("Playlists"), "playlists"], [qsTr("Albums"), "albums"], [qsTr("Artists"), "artists"], [qsTr("Songs"), "songs"], [qsTr("History"), "history"]],
                needsLogin: true,
                searchable: false,
                emptyText: qsTr("Nothing in your library yet")
            })
            break
        case "queue":
            showNowPlaying("queue")
            break
        case "settings":
            pageStack.push(Qt.resolvedUrl("SettingsPage.qml"))
            break
        }
    }

    function search(text) {
        if (text.trim().length === 0)
            return
        pageStack.clear()
        pageStack.push(itemsUrl, {title: qsTr("Search"), query: text.trim()})
    }

    function open(item) {
        pageStack.push(itemsUrl, {
            title: item.title, target: item.kind + ":" + item.browseId, collection: true, searchable: false
        })
    }

    function showNowPlaying(tab) {
        if (!Player.hasTrack) return
        const cur = pageStack.currentItem
        if (cur && cur.isNowPlaying) { cur.tab = tab; return }
        pageStack.push(Qt.resolvedUrl("NowPlayingPage.qml"), {tab: tab})
    }
    function showLyrics() { showNowPlaying("lyrics") }

    /// Pressing the bottom bar: while the player is open, go back to the page you were on; otherwise open the player.
    function toggleNowPlaying() {
        const cur = pageStack.currentItem
        if (cur && cur.isNowPlaying && pageStack.depth > 1) pageStack.pop()
        else showNowPlaying("queue")
    }

    function addToPlaylist(item) {
        addDialog.song = item
        addDialog.open()
    }
    AddToPlaylistDialog { id: addDialog }

    globalDrawer: Kirigami.GlobalDrawer {
        title: qsTr("YT Music")
        titleIcon: "multimedia-audio-player"
        actions: [
            Kirigami.Action { text: qsTr("Home"); icon.name: "go-home"; onTriggered: root.go("home") },
            Kirigami.Action { text: qsTr("Explore"); icon.name: "view-list-icons"; onTriggered: root.go("explore") },
            Kirigami.Action { text: qsTr("Library"); icon.name: "folder-music"; onTriggered: root.go("library") },
            Kirigami.Action { text: qsTr("Now Playing"); icon.name: "media-playback-start"; enabled: Player.hasTrack; onTriggered: root.showNowPlaying("queue") },
            Kirigami.Action { text: qsTr("Queue"); icon.name: "view-media-playlist"; onTriggered: root.go("queue") },
            Kirigami.Action { text: qsTr("Settings"); icon.name: "configure"; onTriggered: root.go("settings") }
            , Kirigami.Action { text: root.visibility === Window.FullScreen ? qsTr("Exit Full Screen") : qsTr("Full Screen"); icon.name: root.visibility === Window.FullScreen ? "view-restore" : "view-fullscreen"; onTriggered: root.toggleFullScreen() }
        ]
    }

    footer: NowPlayingBar {}

    Component.onCompleted: {
        pageStack.columnView.columnResizeMode = Kirigami.ColumnView.SingleColumn
        Player.startMpris()
        Player.restoreSession()
        Player.volume = prefs.volume
        Player.shuffle = prefs.shuffle
        Player.repeat = prefs.repeat
        Backend.refreshLogin()
        go("home")
    }
}
