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
        // A like YouTube refused: the heart was already put back; say why instead of failing silently.
        function onLikeFailed(videoId, message) {
            root.showPassiveNotification(qsTr("Couldn’t update your liked songs: %1").arg(message.split("\n\n")[0]), "long")
        }
    }
    // Audio playback goes through the Playback seam (see Playback.qml); the Rust Player resolves URLs and owns the queue.
    property alias media: media

    Playback {
        id: media
        source: Player.source
        nextSource: Player.nextSource
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
        // The player opens *on top of* the current page (so closing it returns there); every other destination replaces the stack.
        if (name === "queue") { showNowPlaying("queue"); return }
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

    globalDrawer: NavDrawer {}

    footer: NowPlayingBar {}

    // ---- mini-player + system tray (W9) ----
    MiniPlayer { id: mini }
    TrayController { id: trayController; mini: mini }
    function toggleMiniPlayer() { mini.toggle() }
    Shortcut {
        sequence: AppConfig.keyMiniPlayer
        enabled: AppConfig.keyMiniPlayer.length > 0
        onActivated: root.toggleMiniPlayer()
    }
    // With `window.close_to_tray` on (and a tray that exists) closing the window only hides it; music keeps playing
    // and the tray's Quit really quits. Otherwise closing quits as before, taking the mini-player with it.
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
