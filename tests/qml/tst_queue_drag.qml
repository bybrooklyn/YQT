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

        function test_dragging_the_first_handle_down_reorders_the_list() {
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
    }
}
