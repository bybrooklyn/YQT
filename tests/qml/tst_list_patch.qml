import QtQuick
import QtTest

// A list that is refreshed while it is scrolled must stay where it is. The Downloads page reloads its rows on every
// state change of a download: it patches a ListModel in place (qml/DownloadsPage.qml `reload`, mirrored here) instead of
// handing the view a new array, which rebuilt the list from the top. The "Add to playlist" picker appends to a
// ListModel for the same reason.
Item {
    id: root
    width: 300
    height: 300

    property var rows: []
    ListModel { id: rowModel }
    function shown(r) {
        return {videoId: String(r.videoId || ""), title: String(r.title || ""), state: String(r.state || ""),
                percent: Number(r.percent || 0), pinned: !!r.pinned}
    }
    function reload(next) {
        rows = next
        for (let i = 0; i < rows.length; ++i) {
            const r = shown(rows[i])
            let at = -1
            for (let j = i; j < rowModel.count && at < 0; ++j) if (rowModel.get(j).videoId === r.videoId) at = j
            if (at < 0) { rowModel.insert(i, r); continue }
            if (at !== i) rowModel.move(at, i, 1)
            rowModel.set(i, r)
        }
        if (rowModel.count > rows.length) rowModel.remove(rows.length, rowModel.count - rows.length)
    }

    ListView {
        id: view
        anchors.fill: parent
        model: rowModel
        delegate: Item {
            required property var model
            readonly property var modelData: model
            readonly property string shownText: modelData.title + " " + modelData.percent
            width: ListView.view.width
            height: 30
        }
    }

    TestCase {
        name: "ListPatch"
        when: windowShown

        function make(n) { const a = []; for (let i = 0; i < n; ++i) a.push({videoId: "v" + i, title: "t" + i, state: "downloaded", percent: 100}); return a }
        function ids() { const o = []; for (let i = 0; i < rowModel.count; ++i) o.push(rowModel.get(i).videoId); return o.join(",") }

        function test_a_reload_keeps_the_scroll_position_and_updates_rows_in_place() {
            root.reload(make(60))
            wait(50)
            view.contentY = 600
            wait(50)
            const row = view.itemAtIndex(22)
            verify(row, "row 22 is on screen")
            // a progress tick: the same rows, one changed
            const next = make(60)
            next[22].percent = 40
            next[22].state = "downloading"
            root.reload(next)
            wait(50)
            compare(view.contentY, 600, "the list did not jump back to the top")
            compare(view.itemAtIndex(22), row, "the row was updated, not rebuilt")
            compare(row.shownText, "t22 40")
        }

        function test_rows_that_appear_vanish_and_move_end_up_in_the_new_order() {
            root.reload(make(5))
            compare(ids(), "v0,v1,v2,v3,v4")
            const next = [{videoId: "new", title: "n"}].concat(make(5).filter(r => r.videoId !== "v1").reverse())
            root.reload(next)
            compare(ids(), "new,v4,v3,v2,v0")
            root.reload([])
            compare(rowModel.count, 0)
        }
    }
}
