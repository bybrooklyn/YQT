// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// Renders a PageData (see src/pagedata.rs, camelCase JSON as a JS object):
//   {hero: {...}|null, sections: [{id, title, subtitle, style, items[]}], pager, hasMore}
// as a hero header followed by its sections: lists of rows (`list`, `numbered_list`) and shelves of cards
// (`cards`, `artists`, `grid`). Everything is virtualised (one ListView), so a 500-song playlist is cheap.
//
// Presentational: the page controller owns loading and paging and just assigns `pageData`.
//   activated(item)        a row / card / artist link was clicked
//   playRequested(item)    the hover play button of a card
//   playAll(shuffle)       the hero's Play (false) / Shuffle (true)
//   action(name)           the other hero buttons: "radio" | "download" | "add_to_queue" | "save_to_library"
//   moreRequested()        the list is scrolled near its end and `pageData.hasMore` is true (once per growth)
//   itemMenuRequested(item) right click / menu key on a row
// Reserved section ids carry hero data and are never drawn as shelves: "hero-artists" (links under the title) and
// "hero-radio" (one playlist item whose browseId is the artist radio; exposed as `radioId`).
Kirigami.ScrollablePage {
    id: page

    property var pageData: null
    property bool busy: false
    property string emptyText
    /// Greeting line used as the hero when `pageData` has none (progressive Home; see `Nav.greeting()`).
    property string greeting
    readonly property string radioId: {
        const s = (pageData && pageData.sections || []).find(x => x.id === "hero-radio")
        return s && s.items.length ? s.items[0].browseId : ""
    }
    readonly property bool compact: width < Kirigami.Units.gridUnit * 34
    readonly property real sideMargin: compact ? Kirigami.Units.largeSpacing : Kirigami.Units.gridUnit * 1.5
    readonly property real cardWidth: compact ? Kirigami.Units.gridUnit * 9 : Kirigami.Units.gridUnit * 10.5
    /// Blocks in display order; the ListModel below only holds indices into this array.
    property var blocks: []
    property var _keys: []

    signal activated(var item)
    signal playRequested(var item)
    signal playAll(bool shuffle)
    signal action(string name)
    signal moreRequested()
    signal itemMenuRequested(var item)

    padding: 0
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    // ---- PageData → blocks -----------------------------------------------------------------------------------------
    function sectionById(id) {
        return (pageData && pageData.sections || []).find(s => s.id === id) || null
    }

    function build() {
        const out = []
        const p = pageData
        if (!p) return out
        const hero = p.hero || (greeting ? { title: greeting } : null)
        if (hero) out.push({ key: "hero:" + hero.title, type: "hero", hero: hero, artists: (sectionById("hero-artists") || { items: [] }).items })
        for (const s of p.sections || []) {
            if (s.id === "hero-artists" || s.id === "hero-radio" || !s.items || s.items.length === 0) continue
            if (s.style === "list" || s.style === "numbered_list") {
                if (s.title) out.push({ key: "head:" + s.id, type: "heading", title: s.title, kicker: s.subtitle || "" })
                const numbered = s.style === "numbered_list"
                for (let i = 0; i < s.items.length; ++i)
                    out.push({ key: "row:" + s.id + ":" + i + ":" + (s.items[i].videoId || s.items[i].browseId), type: "row", item: s.items[i], numbered: numbered })
            } else {
                out.push({ key: "shelf:" + s.id + ":" + s.items.length, type: "shelf", section: s })
            }
        }
        if (p.hasMore) out.push({ key: "tail", type: "tail" })
        return out
    }

    // Keep the existing delegates when the new page only appends (a paged list growing); otherwise start over.
    function rebuild() {
        const next = build()
        const keys = next.map(b => b.key)
        const old = _keys
        let appendOnly = old.length > 0 && keys.length >= old.length
        for (let i = 0; appendOnly && i < old.length; ++i) if (keys[i] !== old[i]) appendOnly = false
        const y = list.contentY
        blocks = next
        _keys = keys
        if (appendOnly) {
            // `tail` placeholder rows are replaced by the new tail
            for (let i = old.length; i < keys.length; ++i) index.append({ ref: i })
        } else {
            index.clear()
            for (let i = 0; i < keys.length; ++i) index.append({ ref: i })
            if (y > 0) Qt.callLater(() => { list.contentY = Math.min(y, Math.max(0, list.contentHeight - list.height)) })
        }
        askedFor = -1
        Qt.callLater(checkNearEnd)
    }
    onPageDataChanged: rebuild()
    onGreetingChanged: rebuild()

    // ---- paging: judge "near the end" by the row at the bottom of the viewport, not by contentHeight -------------------
    property int askedFor: -1
    function checkNearEnd() {
        if (!pageData || !pageData.hasMore || busy || blocks.length === 0 || askedFor === blocks.length) return
        const last = list.indexAt(list.contentX + 1, list.contentY + list.height - 1)
        if (last >= blocks.length - 4 || (last < 0 && list.contentHeight <= list.height)) {
            askedFor = blocks.length
            moreRequested()
        }
    }

    ListModel { id: index }

    ListView {
        id: list
        model: index
        reuseItems: false
        cacheBuffer: Kirigami.Units.gridUnit * 30
        boundsBehavior: Flickable.StopAtBounds
        onContentYChanged: page.checkNearEnd()
        onHeightChanged: page.checkNearEnd()

        delegate: Loader {
            id: slot
            required property int ref
            required property int index
            readonly property var block: page.blocks[ref] || null
            width: ListView.view.width
            sourceComponent: !block ? null : block.type === "hero" ? heroComp : block.type === "heading" ? headingComp
                           : block.type === "row" ? rowComp : block.type === "shelf" ? shelfComp : tailComp
            // keyboard focus brings its delegate into view
            onActiveFocusChanged: if (activeFocus) ListView.view.positionViewAtIndex(index, ListView.Contain)

        // (the components live inside the delegate so they can see `slot`)
        Component {
            id: heroComp
            Hero {
                hero: slot.block.hero
                artists: slot.block.artists
                compact: page.compact
                width: list.width
                onActionTriggered: name => {
                    if (name === "play") page.playAll(false)
                    else if (name === "shuffle") page.playAll(true)
                    else page.action(name)
                }
                onArtistActivated: item => page.activated(item)
            }
        }
        Component {
            id: headingComp
            Item {
                implicitHeight: heading.implicitHeight + Kirigami.Units.gridUnit
                Kirigami.Heading {
                    id: heading
                    anchors { left: parent.left; right: parent.right; bottom: parent.bottom; leftMargin: page.sideMargin; rightMargin: page.sideMargin; bottomMargin: Kirigami.Units.smallSpacing }
                    level: 2
                    text: slot.block.title
                    elide: Text.ElideRight
                    Accessible.role: Accessible.Heading
                }
            }
        }
        Component {
            id: rowComp
            Item {
                implicitHeight: row.implicitHeight
                TrackRow {
                    id: row
                    // rows stop growing on very wide windows so the title and the duration stay readable together
                    x: page.sideMargin - Kirigami.Units.smallSpacing
                    width: Math.min(parent.width - 2 * x, Kirigami.Units.gridUnit * 64)
                    item: slot.block.item
                    numbered: slot.block.numbered
                    onActivated: page.activated(slot.block.item)
                    onMenuRequested: page.itemMenuRequested(slot.block.item)
                }
            }
        }
        Component {
            id: shelfComp
            Item {
                implicitHeight: shelf.implicitHeight + Kirigami.Units.gridUnit * 1.5
                Shelf {
                    id: shelf
                    anchors { left: parent.left; right: parent.right; top: parent.top; topMargin: Kirigami.Units.gridUnit * 1.2; leftMargin: page.sideMargin; rightMargin: page.sideMargin }
                    title: slot.block.section.title
                    kicker: slot.block.section.subtitle || ""
                    items: slot.block.section.items
                    shelfStyle: slot.block.section.style
                    cardWidth: page.cardWidth
                    // a paged grid (library) shows everything it has; Home's "Listen again" shows two rows and expands
                    gridRows: page.pageData && (page.pageData.hasMore || page.pageData.sections.length <= 1) ? 1000 : 2
                    pageFlickable: list
                    onActivated: item => page.activated(item)
                    onPlayRequested: item => page.playRequested(item)
                }
            }
        }
        Component {
            id: tailComp
            Item {
                implicitHeight: Kirigami.Units.gridUnit * 4
                QQC2.BusyIndicator { anchors.centerIn: parent; running: true }
            }
        }
        }

        footer: Item { width: 1; height: Kirigami.Units.gridUnit }
    }

    Kirigami.LoadingPlaceholder {
        anchors.centerIn: parent
        visible: page.busy && page.blocks.length === 0
    }
    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - Kirigami.Units.gridUnit * 4
        visible: !page.busy && page.blocks.length === 0 && page.emptyText.length > 0
        text: page.emptyText
    }
}
