import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import QtTest
import org.kde.kirigami as Kirigami
import "../../qml"

// The like button of the player bar (qml/LikeButton.qml): spinner while the like is sent, tick when YouTube took
// it, then the star again; the bar does not shift, and a like on its way is not sent twice.
Item {
    id: host
    width: 420; height: 70
    Rectangle { anchors.fill: parent; color: Kirigami.Theme.backgroundColor }
    property int clicks: 0
    RowLayout {
        anchors.centerIn: parent; spacing: 24
        QQC2.Label { text: "empty" }   LikeButton { id: a; onToggled: host.clicks++ }
        QQC2.Label { text: "sending" } LikeButton { id: b; onToggled: host.clicks++ }
        QQC2.Label { text: "taken" }   LikeButton { id: c; liked: true }
        QQC2.Label { text: "liked" }   LikeButton { id: d; liked: true }
    }
    TestCase {
        name: "LikeButton"; when: windowShown
        function test_states() {
            b.sending = true
            c.accepted()
            wait(400)
            compare(b.width, a.width)                 // the bar does not shift while the spinner shows
            mouseClick(b); compare(host.clicks, 0)   // a like on its way is not sent twice
            mouseClick(a); compare(host.clicks, 1)
            verify(c.ticked)
            tryCompare(c, "ticked", false, 3000)      // the star comes back by itself...
            tryCompare(c, "opacity", 1, 2000)         // ...fully faded in
            b.sending = false
            compare(b.width, a.width)
        }
    }
}
