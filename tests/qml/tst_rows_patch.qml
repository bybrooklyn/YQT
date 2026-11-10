import QtQuick
import QtTest

// Home fills in a section at a time, and refreshes quietly behind a cached copy. Its list must change only where the
// rows differ: clearing and refilling it on every arrival made it flicker and jump (qml/ItemsPage.qml `patchRows`,
// mirrored here).
Item {
    id: root
    width: 300
    height: 300

    property var items: []
    property bool shelfMode: false
    ListModel { id: model }

    function rowKey(i) { return [i.kind, i.videoId, i.browseId, i.title, i.subtitle, i.thumbnail, i.section, i.duration].join("\u0001") }
    function patchRows(all) {
        if (shelfMode) { model.clear(); items = all; return }
        if (model.count !== items.length) { model.clear(); items = [] }
        const old = items
        let head = 0
        while (head < old.length && head < all.length && rowKey(old[head]) === rowKey(all[head])) ++head
        let tail = 0
        while (tail < old.length - head && tail < all.length - head
               && rowKey(old[old.length - 1 - tail]) === rowKey(all[all.length - 1 - tail])) ++tail
        const gone = old.length - head - tail
        if (gone > 0) model.remove(head, gone)
        for (let i = head; i < all.length - tail; ++i) model.insert(i, all[i])
        items = all
    }

    // what the model was asked to do, to tell "untouched" from "rebuilt to the same content"
    property int inserted: 0
    property int removed: 0
    Connections {
        target: model
        function onRowsInserted(parent, first, last) { root.inserted += last - first + 1 }
        function onRowsRemoved(parent, first, last) { root.removed += last - first + 1 }
    }

    ListView {
        id: view
        anchors.fill: parent
        model: model
        delegate: Item {
            required property string title
            width: ListView.view.width
            height: 30
        }
    }

    TestCase {
        name: "RowsPatch"
        when: windowShown

        function section(name, n) {
            const a = []
            for (let i = 0; i < n; ++i)
                a.push({kind: "song", videoId: name + i, browseId: "", title: name + " " + i, subtitle: "", thumbnail: "", section: name, duration: ""})
            return a
        }
        function shown() { const o = []; for (let i = 0; i < model.count; ++i) o.push(model.get(i).videoId); return o.join(",") }
        function ids(a) { return a.map(i => i.videoId).join(",") }
        function reset() { model.clear(); root.items = []; root.inserted = 0; root.removed = 0 }

        function init() { reset() }

        function test_a_section_arriving_after_the_others_is_appended() {
            const a = section("a", 20)
            root.patchRows(a)
            compare(root.inserted, 20)
            const all = a.concat(section("b", 5))
            root.patchRows(all)
            compare(shown(), ids(all))
            compare(root.inserted, 25, "only the new rows were added")
            compare(root.removed, 0)
        }

        function test_a_section_arriving_before_the_others_is_inserted_and_the_view_stays_put() {
            const late = section("z", 40)
            root.patchRows(late)
            wait(50)
            view.contentY = 300
            wait(50)
            const row = view.itemAtIndex(12)
            verify(row, "row 12 is on screen")
            const all = section("a", 3).concat(late)
            root.patchRows(all)
            wait(50)
            compare(shown(), ids(all))
            compare(root.removed, 0, "nothing that was shown was taken away")
            compare(root.inserted, 43)
            compare(view.itemAtIndex(15), row, "the same row object is still the one for that song")
        }

        function test_the_same_rows_again_touch_nothing() {
            const a = section("a", 10)
            root.patchRows(a)
            root.inserted = 0
            root.patchRows(section("a", 10))   // equal content, new objects: a quiet refresh
            compare(root.inserted, 0)
            compare(root.removed, 0)
        }

        function test_changed_rows_in_the_middle_are_the_only_ones_replaced() {
            const a = section("a", 4), b = section("b", 4), c = section("c", 4)
            root.patchRows(a.concat(b, c))
            root.inserted = 0
            const b2 = section("b", 2).concat(section("x", 1))
            const all = a.concat(b2, c)
            root.patchRows(all)
            compare(shown(), ids(all))
            compare(root.removed, 2, "b2 and b3")
            compare(root.inserted, 1, "x0")
        }

        function test_shorter_longer_and_empty_lists() {
            const a = section("a", 6)
            root.patchRows(a)
            root.patchRows(a.slice(0, 2))
            compare(shown(), "a0,a1")
            root.patchRows([])
            compare(model.count, 0)
            root.patchRows(a)
            compare(shown(), ids(a))
            // a repeated row (a song can be in a list twice) must not confuse head and tail
            const twice = [a[0], a[0], a[0]]
            root.patchRows(twice)
            compare(model.count, 3)
            root.patchRows([a[0], a[0]])
            compare(model.count, 2)
        }

        function test_a_model_out_of_step_with_the_items_is_rebuilt() {
            root.patchRows(section("a", 3))
            model.append({kind: "song", videoId: "stray", browseId: "", title: "", subtitle: "", thumbnail: "", section: "", duration: ""})
            const b = section("b", 2)
            root.patchRows(b)
            compare(shown(), ids(b))
        }
    }
}
