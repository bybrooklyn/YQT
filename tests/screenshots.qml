// SPDX-License-Identifier: GPL-3.0-only
// Takes the README screenshots (docs/img) from the real app, signed out, in a hidden session: `just screenshots`.
// Run by the harness build (`yqt --qml tests/screenshots.qml`, YQT_HARNESS_LIVE=1); the folder to write to is the
// program's last argument.
import QtQuick
import dev.bybrooklyn.yqt

QtObject {
    id: drv

    readonly property string out: Qt.application.arguments[Qt.application.arguments.length - 1] + "/"
    property var win: null
    property var album: null
    property var artist: null
    property var steps: []
    property int at: 0

    property Timer timer: Timer { onTriggered: drv.run() }
    function after(ms) { timer.interval = ms; timer.start() }
    function run() {
        const step = steps[at++]
        if (step) step(); else Qt.quit()
    }
    function page() { return win.pageStack.currentItem }
    // the whole window: page header, search bar, page and the player bar
    function shot(name, wait) {
        let item = win.contentItem
        while (item.parent) item = item.parent
        item.grabToImage(result => {
            console.log("screenshot", name, result.saveToFile(out + name + ".png") ? "saved" : "NOT SAVED")
            after(wait || 300)
        })
    }

    Component.onCompleted: {
        win = Qt.createComponent("qrc:/qt/qml/dev/bybrooklyn/yqt/qml/Main.qml").createObject(null)
        win.width = 1000; win.height = 720
        steps = [
            () => { win.search("Rick Astley Whenever You Need Somebody"); after(8000) },
            () => {
                album = page().items.find(i => i.kind === "album")
                artist = page().items.find(i => i.kind === "artist")
                console.log("found", album ? album.title : "no album", "/", artist ? artist.title : "no artist")
                win.open(album); after(7000)
            },
            () => {
                // a click on a song of the album: the queue is the album, playing from that song
                const p = page()
                let i = p.items.findIndex(x => x.kind === "song" && x.title.indexOf("Together Forever") >= 0)
                if (i < 0) i = p.items.findIndex(x => x.kind === "song")
                p.activate(i)
                console.log("album songs", p.songs().length, "playing row", Player.index, Player.title)
                after(40000)
            },
            () => shot("album"),
            () => { win.showLyrics(); after(6000) },
            () => shot("lyrics"),
            () => { page().tab = "queue"; after(1500) },
            () => shot("queue"),
            () => { win.pageStack.pop(); win.open(artist); after(8000) },
            () => shot("artist"),
            () => { win.go("home"); after(15000) },
            () => shot("home"),
            () => { win.go("settings"); after(3000) },
            () => shot("settings"),
        ]
        after(4000)
    }
}
