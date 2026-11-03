// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// A titled row of cards. `shelfStyle` is a PageData section style:
//   "cards"   – a horizontal carousel of cover cards (drag, wheel, arrow buttons, keyboard)
//   "artists" – a carousel of round avatars
//   "grid"    – wrapping cards (first two rows, "Show all" reveals the rest)
// `kicker` is a small line above the title ("More like" / the song). Emits `activated(item)` for a click and
// `playRequested(item)` for the hover play button; `showAll()` for the header button (shown when `hasMore`).
ColumnLayout {
    id: root

    property string title
    property string kicker
    property var items: []
    property string shelfStyle: "cards"
    property real cardWidth: Kirigami.Units.gridUnit * 10.5
    property bool hasMore: false
    /// Rows shown by the grid style before "Show all".
    property int gridRows: 2
    /// The page's vertical scroller: vertical wheel turns over a shelf scroll the page, not the shelf.
    property Flickable pageFlickable

    readonly property bool isGrid: shelfStyle === "grid"
    readonly property bool isArtists: shelfStyle === "artists"
    readonly property real tileWidth: isArtists ? Kirigami.Units.gridUnit * 8 : cardWidth
    property bool gridExpanded: false

    signal activated(var item)
    signal playRequested(var item)
    signal showAll()

    spacing: Kirigami.Units.smallSpacing
    visible: (items || []).length > 0

    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    // ---- header
    RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing

        ColumnLayout {
            Layout.fillWidth: true
            spacing: 0
            QQC2.Label {
                visible: root.kicker.length > 0
                text: root.kicker.toUpperCase()
                font: Kirigami.Theme.smallFont
                opacity: 0.65
                elide: Text.ElideRight
                Layout.fillWidth: true
            }
            Kirigami.Heading {
                level: 2
                text: root.title
                visible: text.length > 0
                elide: Text.ElideRight
                Layout.fillWidth: true
                Accessible.role: Accessible.Heading
            }
        }

        QQC2.ToolButton {
            visible: root.hasMore || (root.isGrid && root.items.length > root.gridCount)
            text: root.isGrid && !root.hasMore ? (root.gridExpanded ? qsTr("Show less") : qsTr("Show all")) : qsTr("Show all")
            onClicked: root.isGrid && !root.hasMore ? root.gridExpanded = !root.gridExpanded : root.showAll()
        }
        QQC2.ToolButton {
            visible: !root.isGrid && list.contentWidth > list.width + 1
            icon.name: "go-previous"
            enabled: !list.atXBeginning
            onClicked: root.scrollBy(-1)
            QQC2.ToolTip.text: qsTr("Scroll left")
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            Accessible.name: qsTr("Scroll left")
        }
        QQC2.ToolButton {
            visible: !root.isGrid && list.contentWidth > list.width + 1
            icon.name: "go-next"
            enabled: !list.atXEnd
            onClicked: root.scrollBy(1)
            QQC2.ToolTip.text: qsTr("Scroll right")
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
            Accessible.name: qsTr("Scroll right")
        }
    }

    // About one screenful per arrow click.
    function scrollBy(direction) {
        const step = Math.max(root.tileWidth, Math.floor(list.width / (root.tileWidth + list.spacing)) * (root.tileWidth + list.spacing) - root.tileWidth)
        const target = Math.max(0, Math.min(list.contentWidth - list.width, list.contentX + direction * step))
        if (AppConfig.reduceMotion) list.contentX = target
        else { slide.to = target; slide.restart() }
    }
    NumberAnimation { id: slide; target: list; property: "contentX"; duration: 260; easing.type: Easing.OutCubic }

    // ---- carousel
    ListView {
        id: list
        visible: !root.isGrid
        Layout.fillWidth: true
        Layout.preferredHeight: probe.cardHeight
        orientation: ListView.Horizontal
        spacing: Kirigami.Units.smallSpacing
        clip: true
        model: root.isGrid ? [] : root.items
        boundsBehavior: Flickable.StopAtBounds
        flickableDirection: Flickable.HorizontalFlick
        reuseItems: true
        // keep the focus ring of the first/last card inside the clip
        leftMargin: 0
        rightMargin: 0

        delegate: Loader {
            id: slot
            required property var modelData
            required property int index
            sourceComponent: root.isArtists ? artistDelegate : cardDelegate
            Binding { target: slot.item; property: "item"; value: slot.modelData; when: slot.item !== null }
            Connections {
                target: slot.item
                ignoreUnknownSignals: true
                function onActiveFocusChanged() { if (slot.item.activeFocus) list.positionViewAtIndex(slot.index, ListView.Contain) }
            }
        }

        WheelHandler {
            acceptedDevices: PointerDevice.Mouse | PointerDevice.TouchPad
            onWheel: event => {
                const horizontal = Math.abs(event.angleDelta.x) > Math.abs(event.angleDelta.y)
                if (horizontal || (event.modifiers & Qt.ShiftModifier)) {
                    const d = horizontal ? event.angleDelta.x : event.angleDelta.y
                    list.contentX = Math.max(0, Math.min(list.contentWidth - list.width, list.contentX - d))
                } else if (root.pageFlickable) {
                    const f = root.pageFlickable
                    const max = Math.max(0, f.contentHeight - f.height + f.originY)
                    f.contentY = Math.max(f.originY, Math.min(max, f.contentY - event.angleDelta.y))
                }
            }
        }
    }

    // Size reference so an empty or still-loading carousel has the height of one card.
    Loader {
        id: probe
        visible: false
        sourceComponent: root.isArtists ? artistDelegate : cardDelegate
        Binding { target: probe.item; property: "item"; value: ({ kind: "album", title: "x", subtitle: "x", thumbnail: "" }); when: probe.item !== null }
        readonly property real cardHeight: item ? item.implicitHeight : 0
    }

    // ---- grid
    readonly property real gridMinTile: tileWidth * 0.85
    readonly property int gridColumns: Math.max(1, Math.floor((width + gridFlow.spacing) / (gridMinTile + gridFlow.spacing)))
    readonly property int gridCount: gridExpanded ? items.length : Math.min(items.length, gridColumns * gridRows)
    // spread the columns over the full width instead of leaving a ragged gap on the right
    readonly property real gridTile: Math.floor((width - (gridColumns - 1) * gridFlow.spacing) / gridColumns)
    Flow {
        id: gridFlow
        visible: root.isGrid
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing
        Repeater {
            model: root.isGrid ? (root.items || []).slice(0, root.gridCount) : []
            delegate: Card {
                required property var modelData
                item: modelData
                cardWidth: root.gridTile
                subtitleLines: 1
                onActivated: root.activated(modelData)
                onPlayRequested: root.playRequested(modelData)
            }
        }
    }

    Component {
        id: cardDelegate
        Card {
            cardWidth: root.tileWidth
            onActivated: root.activated(item)
            onPlayRequested: root.playRequested(item)
            Keys.onRightPressed: nextItemInFocusChain(true).forceActiveFocus(Qt.TabFocusReason)
            Keys.onLeftPressed: nextItemInFocusChain(false).forceActiveFocus(Qt.BacktabFocusReason)
        }
    }
    Component {
        id: artistDelegate
        ArtistChip {
            avatarSize: root.tileWidth - Kirigami.Units.largeSpacing
            onActivated: root.activated(item)
            Keys.onRightPressed: nextItemInFocusChain(true).forceActiveFocus(Qt.TabFocusReason)
            Keys.onLeftPressed: nextItemInFocusChain(false).forceActiveFocus(Qt.BacktabFocusReason)
        }
    }
}
