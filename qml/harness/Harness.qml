// SPDX-License-Identifier: GPL-3.0-only
// Developer harness (NOT part of the app / qrc). Needs a build with `--features harness`:
//
//   QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software QT_ASSUME_STDERR_HAS_CONSOLE=1 \
//     target/debug/ytm --qml $PWD/qml/harness/Harness.qml --scenario long --out /some/dir
//
// Scenarios: long | home | pages | errors | queue. Each drives the real ItemsPage / QueuePanel / ErrorView against
// canned data (src/page/demo.rs) with simulated input, writes PNGs to --out and prints `PASS`/`FAIL` lines
// (QtTest's own reporting is not wired up outside qmltestrunner, so checks are plain helpers here).
import QtQuick
import QtTest
import org.kde.kirigami as Kirigami
import org.kde.ytm

Kirigami.ApplicationWindow {
    id: win
    width: 900
    height: 680
    visible: true

    readonly property var cli: Qt.application.arguments
    function arg(name, fallback) { const i = cli.indexOf(name); return i >= 0 && i + 1 < cli.length ? cli[i + 1] : fallback }
    readonly property string scenario: arg("--scenario", "long")
    readonly property string outDir: arg("--out", ".")
    readonly property url itemsUrl: Qt.resolvedUrl("../ItemsPage.qml")

    // what the real Main.qml offers to pages
    property var opened: []
    function open(item) { opened = opened.concat([item]) }
    function search(text) { }
    function go(name) { lastGo = name }
    property string lastGo
    function addToPlaylist(song) { lastPlaylistSong = song }
    property var lastPlaylistSong

    Component { id: queuePageComp
        Kirigami.Page {
            title: "Queue"
            property alias panel: panel
            QueuePanel { id: panel; anchors.fill: parent; gutter: Kirigami.Units.gridUnit * 0.9 }
        }
    }

    property int failures: 0
    function ok(cond, msg) {
        if (cond) console.log("PASS " + msg)
        else { failures++; console.log("FAIL " + msg) }
        return cond
    }
    function eq(actual, expected, msg) {
        const a = JSON.stringify(actual), e = JSON.stringify(expected)
        return ok(a === e, msg + (a === e ? "" : "  (got " + a + ", wanted " + e + ")"))
    }
    // wait until fn() is true (polling the event loop)
    function holds(fn) { try { return !!fn() } catch (e) { return false } }
    function until(fn, ms, msg) {
        const t0 = Date.now()
        while (!holds(fn) && Date.now() - t0 < ms) tc.wait(20)
        return ok(holds(fn), msg)
    }

    // The offscreen window only lays out (and creates delegates) when a frame is rendered; a grab forces one.
    function render() {
        let done = false
        pageStack.grabToImage(function () { done = true })
        const t0 = Date.now()
        while (!done && Date.now() - t0 < 3000) tc.wait(10)
    }

    property bool grabbed: false
    function shot(name) {
        grabbed = false
        pageStack.grabToImage(function (r) { r.saveToFile(outDir + "/" + name + ".png"); grabbed = true })
        until(function () { return grabbed }, 5000, "screenshot " + name)
    }

    function show(target, props) {
        pageStack.clear()
        pageStack.push(itemsUrl, Object.assign({title: target || "Search", target: target}, props || {}))
        return pageStack.currentItem
    }

    // ---- a long list pages as you scroll, one request at a time, and stops at the end ------------------------------
    function runLong() {
        const page = show("demo:long")
        const ctl = page.controller
        until(function () { return ctl.state === "ready" }, 3000, "long: ready")
        eq(ctl.count, 40, "long: first page has 40 rows")
        ok(ctl.hasMore, "long: continues")
        tc.wait(300)
        eq(ctl.count, 40, "long: nothing loads while the viewport is far from the end")
        shot("long-1")

        const view = page.flickable
        view.positionViewAtEnd()
        until(function () { return ctl.count === 80 }, 3000, "long: second page arrives on scrolling to the end")
        tc.wait(700)
        eq(ctl.count, 80, "long: the second page did not chain into a third")
        ok(ctl.hasMore, "long: still continues")
        shot("long-2")

        view.positionViewAtEnd()
        until(function () { return ctl.count === 120 }, 3000, "long: third page")
        until(function () { return !ctl.hasMore }, 1000, "long: no more pages after the last")
        tc.wait(500)
        eq(ctl.count, 120, "long: stays at 120")
        ok(!ctl.loadingMore, "long: idle at the end")

        // "Play" on a long list feeds the remaining pages into the queue
        ctl.load("demo:long", "", "", true)
        until(function () { return ctl.count === 40 }, 3000, "long: reloaded")
        ctl.playAll(false)
        until(function () { return QueueModel.count === 120 }, 5000, "long: feeding appended every page to the queue")
        eq(ctl.count, 120, "long: the list grew too")
        shot("long-fed")
    }

    // ---- Home arrives row by row and lands in slot order -------------------------------------------------------------
    function runHome() {
        const page = show("home")
        const ctl = page.controller
        tc.wait(450)
        ok(ctl.count > 0, "home: first rows are already on screen")
        ok(ctl.refreshing, "home: later rows are still on their way")
        shot("home-early")
        until(function () { return !ctl.refreshing }, 4000, "home: all rows arrived")
        eq(ctl.count, 24, "home: 4 rows x 6")
        eq(ctl.state, "ready", "home: ready")
        tc.wait(150)
        eq(page.flickable.itemAtIndex(0).title, "Listen again song 0", "home: slot 0 first although it finished last")
        shot("home-final")
    }

    // ---- every kind of page ----------------------------------------------------------------------------------------------
    function runPages() {
        let page = show("album:demo")
        until(function () { return page.controller.state === "ready" }, 3000, "album: ready")
        ok(page.controller.hasHero, "album: hero")
        eq(page.controller.songCount, 12, "album: 12 songs")
        shot("page-album")

        page = show("library", {filter: "playlists"})
        until(function () { return page.controller.state === "ready" }, 3000, "library: ready")
        eq(page.controller.count, 3, "library: one Liked music row, not two")
        eq(page.controller.chipLabels.length, 5, "library: tabs come from Rust")
        shot("page-library")
        page.controller.selectFilter("albums")
        until(function () { return page.controller.state === "ready" }, 3000, "library: other tab")

        page = show("demo:empty")
        until(function () { return page.controller.state === "ready" }, 3000, "empty: ready")
        eq(page.controller.count, 0, "empty: no rows")
        shot("page-empty")

        page = show("", {query: "daft punk"})
        until(function () { return page.controller.state === "ready" }, 3000, "search: ready")
        eq(page.controller.chipLabels.length, 5, "search: filter chips")
        shot("page-search")

        page = show("album:demo")
        until(function () { return page.controller.state === "ready" }, 3000, "album again: ready")
        tc.wait(150)
        page.flickable.itemAtIndex(2).clicked()
        until(function () { return Player.title === "alb song 3" }, 2000, "click: the song plays")
        eq(QueueModel.count, 12, "click: a song on a collection page queues the whole list")
        eq(Player.index, 2, "click: from the clicked song")
    }

    // ---- the error view in each kind -----------------------------------------------------------------------------------
    function runErrors() {
        for (const k of ["network", "auth", "offline", "parse", "notfound", "ratelimit"]) {
            const page = show("demo:error:" + k)
            const ctl = page.controller
            until(function () { return ctl.state === "error" || ctl.state === "offline" }, 3000, k + ": error shown")
            eq(ctl.errorKind, k, k + ": kind")
            ok(ctl.errorDiagnostics.indexOf("secretvalue123") < 0, k + ": diagnostics are redacted")
            tc.wait(250)
            shot("error-" + k)
        }
        // an automatic policy counts down, retries by itself, and ends in a plain Retry button
        let page = show("demo:error:network")
        let ctl = page.controller
        until(function () { return ctl.state === "error" }, 3000, "auto: error")
        eq(ctl.errorRetry, "auto", "auto: policy")
        ok(ctl.errorRetryMs > 0, "auto: countdown " + ctl.errorRetryMs + " ms")
        until(function () { return ctl.errorRetry === "manual" }, 12000, "auto: after its attempts it falls back to the Retry button")
        eq(ctl.errorRetryMs, 0, "auto: no countdown left")
        shot("error-network-exhausted")
        page = show("demo:error:auth")
        ctl = page.controller
        until(function () { return ctl.state === "error" }, 3000, "auth: error")
        eq(ctl.errorRetry, "never", "auth: no retry")
    }

    // ---- the queue: drag to reorder, the menu -----------------------------------------------------------------------------
    function titles(panel, n) {
        render(); render()
        const out = []
        for (let i = 0; i < n; ++i) { const it = panel.itemAtIndex(i); out.push(it ? it.title : "?") }
        return out
    }

    function runQueue() {
        const songs = []
        for (let i = 0; i < 14; ++i)
            songs.push({kind: "song", title: "Song " + i, artist: "Artist " + (i % 3), videoId: "q" + i, duration: "3:0" + (i % 10), artistId: "UC" + i, albumId: "MP" + i, thumbnail: ""})
        Player.playList(JSON.stringify(songs), 1)
        pageStack.clear()
        pageStack.push(queuePageComp)
        const panel = pageStack.currentItem.panel
        until(function () { return QueueModel.count === 14 }, 3000, "queue: 14 rows reach the model")
        tc.wait(900)                         // let the page-stack push animation finish
        render()
        eq(Player.index, 1, "queue: playing row 1")
        eq(titles(panel, 4), ["Song 0", "Song 1", "Song 2", "Song 3"], "queue: initial order")
        shot("queue-1")

        // drag Song 5 (below the playing row) above it, with the mouse
        const rh = panel.rowHeight
        const x = panel.width / 2
        tc.mousePress(panel, x, rh * 5.5)
        tc.mouseMove(panel, x, rh * 5.0, 20, Qt.LeftButton)
        tc.mouseMove(panel, x, rh * 3.5, 20, Qt.LeftButton)
        tc.mouseMove(panel, x, rh * 1.5, 20, Qt.LeftButton)
        tc.wait(60)
        eq(panel.dragFrom, 5, "drag: grabbed row 5")
        eq(panel.dragTo, 1, "drag: hovering the gap at row 1")
        shot("queue-dragging")
        tc.mouseRelease(panel, x, rh * 1.5)
        until(function () { return panel.dragFrom < 0 }, 2000, "drag: settled")
        eq(titles(panel, 4), ["Song 0", "Song 5", "Song 1", "Song 2"], "drag: order after the drop")
        eq(Player.title, "Song 1", "drag: the playing track is unchanged")
        eq(Player.index, 2, "drag: its index followed it")
        shot("queue-2")

        // drag the first row to the bottom: the view scrolls by itself near the edge
        tc.mousePress(panel, x, rh * 0.5)
        tc.mouseMove(panel, x, rh * 1.0, 20, Qt.LeftButton)
        tc.mouseMove(panel, x, panel.height - rh * 0.3, 20, Qt.LeftButton)
        tc.wait(700)
        ok(panel.contentY > 0, "autoscroll: the view scrolled near its edge (contentY " + panel.contentY + ")")
        ok(panel.dragTo > 6, "autoscroll: the row is carried down the list (to " + panel.dragTo + ")")
        shot("queue-autoscroll")
        tc.mouseRelease(panel, x, panel.height - rh * 0.3)
        until(function () { return panel.dragFrom < 0 }, 2000, "autoscroll: settled")
        eq(Player.title, "Song 1", "autoscroll: still playing Song 1")
        eq(QueueModel.count, 14, "autoscroll: nothing lost")

        // a click that does not move must not reorder
        panel.contentY = 0
        render()
        const first = titles(panel, 3)
        tc.mouseClick(panel, x, rh * 0.5)
        tc.wait(100)
        eq(titles(panel, 3), first, "click: no reordering without a drag")

        // Play next on a far row moves it right after the playing one
        const before = Player.index
        render()
        const name = panel.itemAtIndex(before + 3).title
        QueueModel.playRowNext(before + 3)
        tc.wait(100)
        render()
        eq(panel.itemAtIndex(before + 1).title, name, "menu: Play next moves the row after the playing one")
        eq(Player.index, before, "menu: the playing track is not disturbed")

        const n = QueueModel.count
        QueueModel.removeRow(before + 1)
        until(function () { return QueueModel.count === n - 1 }, 2000, "menu: Remove")
        eq(Player.title, "Song 1", "menu: still playing Song 1")

        render()
        ok(!panel.itemAtIndex(2).menuOpen, "menu: closed to begin with")
        tc.mouseClick(panel, x, rh * 2.5, Qt.RightButton)
        tc.wait(250)
        ok(panel.itemAtIndex(2).menuOpen, "menu: a right click opens the row menu")
        // the first click elsewhere only dismisses that popup; the second presses the ⋮ button of row 4
        const bx = panel.width - panel.gutter - Kirigami.Units.gridUnit * 1.6
        tc.mouseClick(panel, bx, rh * 4.5)
        tc.wait(200)
        if (!panel.itemAtIndex(4).menuOpen) { tc.mouseClick(panel, bx, rh * 4.5); tc.wait(200) }
        ok(panel.itemAtIndex(4).menuOpen, "menu: the ⋮ button opens the same menu")
    }

    TestCase {
        id: tc
        name: "harness"
        when: win.visible
        function test_run() {
            console.log("HARNESS start " + win.scenario)
            try {
                switch (win.scenario) {
                case "long": win.runLong(); break
                case "home": win.runHome(); break
                case "pages": win.runPages(); break
                case "errors": win.runErrors(); break
                case "queue": win.runQueue(); break
                default: win.ok(false, "unknown scenario")
                }
            } catch (e) {
                win.ok(false, "exception: " + e + " " + e.stack)
            }
            console.log("RESULT " + win.scenario + " " + (win.failures === 0 ? "PASS" : "FAIL (" + win.failures + ")"))
            Qt.quit()
        }
    }
}
