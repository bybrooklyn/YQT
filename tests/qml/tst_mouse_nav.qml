import QtQuick
import QtQuick.Controls as QQC2
import QtTest
import org.kde.kirigami as Kirigami

// The mouse's side buttons go back and forward through the pages (qml/Main.qml, mirrored here): one MouseArea over
// the whole window that takes only those two buttons, so ordinary clicks still reach what is underneath.
Item {
    id: host
    width: 600
    height: 400

    Kirigami.ApplicationWindow {
        id: win
        width: 600
        height: 400
        visible: true

        property int leftClicks: 0

        MouseArea {
            id: sideButtons
            parent: QQC2.Overlay.overlay
            anchors.fill: parent
            acceptedButtons: Qt.BackButton | Qt.ForwardButton
            onClicked: mouse => {
                if (mouse.button === Qt.BackButton) win.pageStack.goBack()
                else win.pageStack.goForward()
            }
        }

        Component {
            id: page
            Kirigami.Page {
                MouseArea { anchors.fill: parent; onClicked: win.leftClicks++ }
            }
        }

        Component.onCompleted: {
            pageStack.columnView.columnResizeMode = Kirigami.ColumnView.SingleColumn
            pageStack.push(page, {title: "one"})
            pageStack.push(page, {title: "two"})
        }
    }

    TestCase {
        name: "MouseSideButtons"
        when: windowShown

        function test_back_then_forward() {
            tryCompare(win.pageStack, "depth", 2)
            tryCompare(win.pageStack, "currentIndex", 1)
            mouseClick(sideButtons, 300, 200, Qt.BackButton)
            tryCompare(win.pageStack, "currentIndex", 0)
            mouseClick(sideButtons, 300, 200, Qt.ForwardButton)
            tryCompare(win.pageStack, "currentIndex", 1)
        }

        function test_left_click_passes_through() {
            const before = win.leftClicks
            mouseClick(win.contentItem, 300, 250, Qt.LeftButton)
            tryCompare(win, "leftClicks", before + 1)
            compare(win.pageStack.currentIndex, 1)
        }
    }
}
