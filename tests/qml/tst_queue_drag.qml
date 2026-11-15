import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import QtTest
import org.kde.kirigami as Kirigami

// The queue's drag-to-reorder, driven with real (synthesised) mouse events. The delegate has the same structure as
// qml/NowPlayingPage.qml: a plain wrapper Item as the ListView delegate, a QQC2.ItemDelegate inside it, and the stock
// Kirigami.ListItemDragHandle inside that.
Item {
    id: root
    width: 400
    height: 400

    property var moves: []

    ListModel {
        id: model
        ListElement { name: "A" }
        ListElement { name: "B" }
        ListElement { name: "C" }
        ListElement { name: "D" }
    }

    ListView {
        id: view
        anchors.fill: parent
        model: model
        moveDisplaced: Transition { YAnimator { duration: 50 } }
        delegate: Item {
            id: slot
            required property int index
            required property string name
            width: ListView.view.width
            height: row.implicitHeight
            QQC2.ItemDelegate {
                id: row
                width: slot.width
                contentItem: RowLayout {
                    Kirigami.ListItemDragHandle {
                        objectName: "handle" + slot.index
                        listItem: row
                        listView: view
                        onMoveRequested: (oldIndex, newIndex) => {
                            root.moves.push([oldIndex, newIndex])
                            model.move(oldIndex, newIndex, 1)   // what QueueModel.moveRow does through Rust
                        }
                    }
                    QQC2.Label { text: slot.name; Layout.fillWidth: true }
                }
            }
        }
    }

    TestCase {
        name: "QueueDrag"
        when: windowShown

        function names() { const o = []; for (let i = 0; i < model.count; ++i) o.push(model.get(i).name); return o.join("") }
        function fill(rows) {
            model.clear()
            for (const n of rows) model.append({name: n})
            root.moves = []
        }

        function test_dragging_the_first_handle_down_reorders_the_list() {
            fill("ABCD")
            wait(300)
            compare(names(), "ABCD")
            const first = view.itemAtIndex(0)
            const handle = findChild(first, "handle0")
            verify(handle, "the drag handle exists")
            const start = handle.mapToItem(root, handle.width / 2, handle.height / 2)
            const rowH = first.height
            mousePress(root, start.x, start.y)
            // move in small steps, as a hand would: down past rows B and C
            for (let i = 1; i <= 12; ++i) mouseMove(root, start.x, start.y + i * rowH * 0.2, 20)
            mouseRelease(root, start.x, start.y + rowH * 2.4)
            wait(300)
            verify(root.moves.length > 0, "the handle asked for a move")
            compare(names(), "BCAD", "A was dragged below B and C")
        }

        // A fast drag across two rows: the handle asks for the second step from the delegate's index right after the
        // first, so the model must already have moved when moveRequested returns (QueueModel.moveRow moves its row
        // at once for this reason). A model that caught up a turn later got (1,2) then (1,3) and moved the wrong row.
        function test_a_fast_drag_across_two_rows_moves_the_dragged_row() {
            fill("ABCDE")
            wait(300)
            const second = view.itemAtIndex(1)
            const handle = findChild(second, "handle1")
            verify(handle, "the drag handle exists")
            const start = handle.mapToItem(root, handle.width / 2, handle.height / 2)
            const rowH = second.height
            mousePress(root, start.x, start.y)
            mouseMove(root, start.x, start.y + rowH * 0.3, 0)
            mouseMove(root, start.x, start.y + rowH * 1.1, 0)
            mouseMove(root, start.x, start.y + rowH * 2.1, 0)
            mouseRelease(root, start.x, start.y + rowH * 2.1, Qt.LeftButton, Qt.NoModifier, 0)
            wait(300)
            compare(JSON.stringify(root.moves), "[[1,2],[2,3]]", "the second step starts where the first one left B")
            compare(names(), "ACDBE", "B was dragged below C and D")
        }
    }
}
