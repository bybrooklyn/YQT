// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtCore
import org.kde.ytm
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
    // Audio goes through the Playback seam (Playback.qml): the Rust engine or Qt Multimedia, chosen in Settings
    // (audio.engine). The Rust Player resolves URLs and owns the queue. `smoothPosition` is the position extrapolated
    // every frame (the rule lives in Rust, src/clock.rs): read it for anything shown so bars and lyrics glide.
    property alias media: media

    Playback {
        id: media
        source: Player.source
        nextSource: Player.nextSource      // decoded ahead by the Rust engine for gapless playback and crossfade
        volume: Player.volume
        autoplay: Player.autoplay
        onFailed: message => Player.mediaFailed(message)
        onPlayingChanged: root.syncMpris()
        onDurationChanged: root.syncMpris()
        onLoaded: {
            if (Player.resumePos > 0) {
                media.seek(Player.resumePos)
                Player.resumePos = 0
            }
        }
        onEnded: {
            if (Player.repeat === 2) { media.seek(0); media.play() } else Player.next()
        }
    }
    readonly property real smoothPosition: media.smoothPosition
    function seek(ms) { media.seek(ms) }

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
        Player.syncMpris(media.playing, media.position, media.duration)
    }
    function previousTrack() {
        if (media.position > AppConfig.restartThresholdMs || Player.index <= 0) media.seek(0); else Player.previous()
    }
    Timer { interval: 1000; repeat: true; running: Player.hasTrack; onTriggered: root.syncMpris() }

    // Report plays back to YouTube (history, recommendations). The rules (threshold, once per play, signed-in only,
    // retries) all live in Rust (src/reporting.rs); this only says what is playing and how far along it is.
    Connections {
        target: Player
        function onVideoIdChanged() { Reporter.trackChanged(Player.videoId) }
    }
    Timer {
        interval: 1000; repeat: true; running: Player.hasTrack
        onTriggered: Reporter.tick(Math.round(media.position), media.playing)
    }
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
            case "seek": media.seek(media.position + arg); break
            case "setposition": media.seek(arg); break
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
        media.toggle()
    }

    // Keyboard shortcuts come from Settings → Keyboard shortcuts (config `[keys]`, delivered as JSON by AppConfig).
    readonly property var keys: { try { return JSON.parse(AppConfig.shortcutsJson) } catch (e) { return ({}) } }
    // A text field must keep Space and the arrow keys for itself.
    readonly property bool typing: !!root.activeFocusItem && root.activeFocusItem.inputMethodHints !== undefined
    function nudgeVolume(delta) { Player.volume = Math.max(0, Math.min(1, Player.volume + delta)) }

    Shortcut { sequence: root.keys.fullscreen || ""; enabled: !!root.keys.fullscreen; onActivated: root.toggleFullScreen() }
    Shortcut { sequence: "Escape"; enabled: root.visibility === Window.FullScreen; onActivated: root.visibility = Window.Windowed }
    Shortcut { sequence: root.keys.search || ""; enabled: !!root.keys.search; onActivated: root.focusSearch() }
    Shortcut { sequence: root.keys.play_pause || ""; enabled: !!root.keys.play_pause && !root.typing; onActivated: root.togglePlay() }
    Shortcut { sequence: root.keys.next || ""; enabled: !!root.keys.next; onActivated: Player.next() }
    Shortcut { sequence: root.keys.previous || ""; enabled: !!root.keys.previous; onActivated: root.previousTrack() }
    Shortcut { sequence: root.keys.now_playing || ""; enabled: !!root.keys.now_playing; onActivated: root.toggleNowPlaying() }
    Shortcut { sequence: root.keys.lyrics || ""; enabled: !!root.keys.lyrics; onActivated: root.showLyrics() }
    Shortcut { sequence: root.keys.volume_up || ""; enabled: !!root.keys.volume_up; onActivated: root.nudgeVolume(0.05) }
    Shortcut { sequence: root.keys.volume_down || ""; enabled: !!root.keys.volume_down; onActivated: root.nudgeVolume(-0.05) }
    Shortcut { sequence: root.keys.seek_forward || ""; enabled: !!root.keys.seek_forward && !root.typing; onActivated: media.seek(media.position + 5000) }
    Shortcut { sequence: root.keys.seek_back || ""; enabled: !!root.keys.seek_back && !root.typing; onActivated: media.seek(media.position - 5000) }
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
        case "downloads":
            pageStack.push(Qt.resolvedUrl("DownloadsPage.qml"))
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
        // Wider than the default so the labels are comfortable to read and to hit.
        width: Kirigami.Units.gridUnit * 18
        actions: [
            Kirigami.Action { text: qsTr("Home"); icon.name: "go-home"; onTriggered: root.go("home") },
            Kirigami.Action { text: qsTr("Explore"); icon.name: "view-list-icons"; onTriggered: root.go("explore") },
            Kirigami.Action { text: qsTr("Library"); icon.name: "folder-music"; onTriggered: root.go("library") },
            Kirigami.Action { text: qsTr("Downloads"); icon.name: "folder-download"; onTriggered: root.go("downloads") },
            Kirigami.Action { separator: true },
            Kirigami.Action { text: qsTr("Now Playing"); icon.name: "media-playback-start"; enabled: Player.hasTrack; onTriggered: root.showNowPlaying("queue") },
            Kirigami.Action { text: qsTr("Queue"); icon.name: "view-media-playlist"; onTriggered: root.go("queue") },
            Kirigami.Action { text: qsTr("Mini-player"); icon.name: "window-duplicate"; onTriggered: root.toggleMiniPlayer() },
            Kirigami.Action { separator: true },
            Kirigami.Action { text: qsTr("Settings"); icon.name: "configure"; onTriggered: root.go("settings") }
            , Kirigami.Action { text: root.visibility === Window.FullScreen ? qsTr("Exit Full Screen") : qsTr("Full Screen"); icon.name: root.visibility === Window.FullScreen ? "view-restore" : "view-fullscreen"; onTriggered: root.toggleFullScreen() }
        ]
    }

    footer: NowPlayingBar {}

    // Mini-player window and the system tray (a D-Bus StatusNotifierItem on Linux; where there is none, `available`
    // stays false and nothing changes).
    MiniPlayer { id: mini; main: root }
    TrayController { id: trayController; mini: mini }
    function toggleMiniPlayer() { mini.toggle() }
    Shortcut {
        sequence: AppConfig.keyMiniPlayer
        enabled: AppConfig.keyMiniPlayer.length > 0
        onActivated: root.toggleMiniPlayer()
    }
    // With "Close to the tray" on (and a tray that exists) closing the window only hides it and the music keeps
    // playing; the tray's Quit really quits. Otherwise closing quits as before, taking the mini-player with it.
    onClosing: close => {
        if (AppConfig.closeToTray && trayController.available) {
            close.accepted = false
            root.hide()
        } else {
            mini.visible = false
        }
    }

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
