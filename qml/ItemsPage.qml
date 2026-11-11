// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import dev.bybrooklyn.yqt
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// One page for everything list-shaped: home, search results, albums, playlists, artists, library.
//
// Loading is lazy: lists fetch their next page only when you scroll near the end, Home fills in row by row as each
// row arrives, and pages you have already seen are kept for a while so going back is instant.
Kirigami.ScrollablePage {
    id: page

    property string target           // "home", "album:ID", "playlist:ID", "artist:ID", "library", ...
    property string query
    property string filter           // search type, or library tab
    property var chips: []           // [[label, value], ...] tab-like filters (library); search has its own
    property bool collection: false  // album/playlist/artist page: clicking a song plays the whole list
    property bool searchable: true
    property bool needsLogin: false
    property bool reloadOnLogin: false
    property string emptyText: qsTr("Nothing here")

    property var items: []
    property string heading
    property string subheading
    property string headThumb
    // the rich parts of an album / artist / playlist page (see `with_page_data` in src/backend.rs)
    property var hero: null          // {description, wikipediaUrl, stats[{text}], ...}
    property var shelves: []         // [{title, style, items[]}]: albums, singles, similar artists, ...
    property var radio: null         // an artist's radio playlist
    // Home and Explore can show each of their sections as a horizontal shelf of cards. Off: they are the original
    // sectioned list (section headers over song/album rows), which is lighter and reads like the rest of the app.
    property bool homeAsShelves: false
    readonly property bool shelfMode: homeAsShelves && query.length === 0 && (target === "home" || target === "explore")
    readonly property var homeShelves: {
        if (!shelfMode) return []
        const out = [], at = {}
        for (const it of items) {
            const k = it.section || ""
            if (!(k in at)) { at[k] = out.length; out.push({title: k, items: []}) }
            out[at[k]].items.push(it)
        }
        return out
    }
    readonly property var allShelves: shelfMode ? homeShelves : shelves
    property string errorText        // headline, shown large
    property string errorDetail      // technical detail, shown small
    property bool busy: false
    property int reqId: -1

    // pagination
    property int pagerId: 0
    // The Rust side keeps each list's pager (and every item fetched so far) for as long as a page holds it. (A feed
    // that is still walking the list when this page lets go finishes first: see Backend.dropPager.)
    property int heldPager: 0
    onPagerIdChanged: {
        if (heldPager > 0 && heldPager !== pagerId) Backend.dropPager(heldPager)
        heldPager = pagerId
        if (pagerId > 0) Backend.holdPager(pagerId)
    }
    Component.onDestruction: if (pagerId > 0) Backend.dropPager(pagerId)
    property bool hasMore: false
    property bool loadingMore: false
    property int moreId: -1
    // After "Play" (or "Download") on a long list the remaining pages are fetched by Rust (Backend.feedQueue /
    // feedDownloads) and their songs appended to the queue (downloaded) there, so that goes on when this page closes
    // and stops when the queue is replaced. While it runs this is true, and the pages also arrive here, under
    // `moreId`, to be shown.
    property bool feedQueue: false

    // progressive pages (Home): rows by slot, so they can arrive in any order
    property var slots: ({})
    // the slots that arrived in the load that is running (a quiet refresh starts with the cached copy's)
    property var seenSlots: ({})
    readonly property bool progressive: target === "home" && query.length === 0


    readonly property var searchChips: [[qsTr("All"), ""], [qsTr("Songs"), "songs"], [qsTr("Albums"), "albums"], [qsTr("Artists"), "artists"], [qsTr("Playlists"), "playlists"]]
    readonly property var activeChips: query.length > 0 ? searchChips : chips
    readonly property bool authProblem: errorText.indexOf("Settings") >= 0

    function songs() { return items.filter(i => i.kind === "song") }
    function itemAt(i) { return i >= 0 && i < items.length ? items[i] : null }
    function field(i, key) { const it = itemAt(i); return it && it[key] ? it[key] : "" }
    function parse(json) { try { return JSON.parse(json) } catch (e) { return null } }

    function setError(message) {
        const cut = message.indexOf("\n\n")
        errorText = cut < 0 ? message : message.slice(0, cut)
        errorDetail = cut < 0 ? "" : message.slice(cut + 2)
    }
    function clearError() { errorText = ""; errorDetail = "" }

    // ---- page cache -------------------------------------------------------------------------------------------
    // Searches and long (paged) lists are not cached; everything else is, per target+tab.
    function cacheKey() { return query ? "" : target + "|" + filter }
    function cacheGet() { const k = cacheKey(); return k && AppConfig.pageCacheMs > 0 ? applicationWindow().pageCache[k] : null }
    // At most this many pages are kept; expired ones are dropped whenever a page is added.
    readonly property int pageCacheMax: 24
    function cachePut(entry) {
        const k = cacheKey()
        if (!k || AppConfig.pageCacheMs <= 0) return
        const cache = applicationWindow().pageCache, now = Date.now()
        for (const key in cache) if (now - cache[key].t > AppConfig.pageCacheMs) delete cache[key]
        const keys = Object.keys(cache).filter(key => key !== k).sort((a, b) => cache[a].t - cache[b].t)
        for (let i = 0; i < keys.length - (pageCacheMax - 1); ++i) delete cache[keys[i]]
        entry.t = now
        cache[k] = entry
    }

    function showResult(res) {
        heading = res.title || ""
        subheading = res.subtitle || ""
        headThumb = res.thumbnail || ""
        if (collection && heading.length > 0) title = heading   // "Go to album" only knows the song's name when it opens the page
        hero = res.hero || null
        shelves = res.shelves || []
        radio = res.radio || null
        pagerId = res.pager || 0
        hasMore = !!res.more
        model.clear()
        if (!shelfMode) for (const i of res.items || []) model.append(i)
        items = res.items || []
    }

    // What makes two rows the same row on screen.
    function rowKey(i) { return [i.kind, i.videoId, i.browseId, i.title, i.subtitle, i.thumbnail, i.section, i.duration].join("\u0001") }
    // Show `all` by changing only the stretch of the list that differs: the rows before and after it stay as they are
    // (no flicker, the scroll position holds). Home's rows arrive one section at a time, so that is "append" or
    // "insert one section"; a quiet refresh that brings the same rows changes nothing at all.
    function patchRows(all) {
        if (shelfMode) { model.clear(); items = all; return }
        if (model.count !== items.length) { model.clear(); items = [] }   // (never expected: start over rather than guess)
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
    function rebuildFromSlots() {
        const all = []
        for (const k of Object.keys(slots).map(Number).sort((a, b) => a - b)) for (const i of slots[k]) all.push(i)
        patchRows(all)
    }
    // Opening a card in a shelf (the list rows use `activate(i)` by index instead).
    function activateItem(item) {
        if (item.kind === "song") Player.playSong(JSON.stringify(item))
        else applicationWindow().open(item)
    }

    property bool pinTop: true

    function load(force) {
        pinTop = true
        clearError()
        // Answers still on their way belong to what was shown before: they must not land on (or be cached as) this load.
        reqId = -1; moreId = -1
        pagerId = 0; hasMore = false; loadingMore = false; feedQueue = false
        seenSlots = ({})
        if (needsLogin && !Backend.loggedIn) {
            setError(qsTr("Sign in from Settings to see your library"))
            model.clear(); items = []
            return
        }
        const hit = force ? null : cacheGet()   // `force`: the account changed, so cached pages are wrong
        const fresh = hit && (Date.now() - hit.t) < AppConfig.pageCacheMs
        if (hit) {
            if (hit.slots) { slots = Object.assign({}, hit.slots); rebuildFromSlots() }
            else showResult(hit.res)
        } else {
            slots = ({})
            model.clear(); items = []
        }
        if (fresh) { busy = false; return }
        busy = !hit                       // a cached copy is shown while a quiet refresh runs behind it
        if (query) reqId = Backend.search(query, filter)
        else if (progressive) reqId = Backend.browseHome()
        else reqId = Backend.browse(filter && target === "library" ? target + ":" + filter : target)
    }

    // Fetch the next page only when the list is close to its end (a feed fetches the rest by itself).
    // "Close" is judged by which row sits at the bottom of the viewport, not by contentHeight: a ListView only
    // *estimates* its height until delegates exist, and that estimate is far too small right after a page arrives.
    function nearEnd() {
        if (view.count === 0 || view.height <= 0) return false
        const bottom = view.indexAt(view.width / 2, view.contentY + view.height - 1)
        if (bottom >= 0) return bottom >= view.count - 10
        // -1: past the last row, but also on the page header or a section header. Only the first is the end of the
        // list, and then the last row exists (a tall header at the top of a long list must not load page after page).
        return view.itemAtIndex(view.count - 1) !== null
    }
    function maybeMore() {
        if (hasMore && !loadingMore && !busy && pagerId > 0 && nearEnd()) {
            loadingMore = true
            moreId = Backend.more(pagerId)
        }
    }
    // Hand the rest of the list to the Rust feed. Call it right after the queue was built (or the downloads asked for)
    // from the songs this page has, in the same handler: every page that reaches this page later, one that is on its
    // way right now included, is then also appended (downloaded) by Rust, exactly once. Asking again while a feed
    // runs joins it.
    function startFeed(download) {
        if (!hasMore || pagerId <= 0) return
        feedQueue = true; loadingMore = true
        moreId = download ? Backend.feedDownloads(pagerId) : Backend.feedQueue(pagerId, Player.queueEpoch())
    }
    function downloadAll() {
        Downloads.downloadAll(JSON.stringify(songs()))
        startFeed(true)
    }
    function startPlaying(list, start) {
        Player.playList(JSON.stringify(list), start)
        startFeed(false)
    }
    function enqueueAll() {
        for (const s of songs()) Player.enqueue(JSON.stringify(s))
        startFeed(false)
    }

    Connections {
        target: Backend
        function onPartial(id, slot, json) {
            if (id !== page.reqId) return
            const res = page.parse(json)
            if (!res) return
            page.busy = false
            const next = Object.assign({}, page.slots)
            next[slot] = res.items
            page.slots = next
            page.seenSlots[slot] = true
            page.rebuildFromSlots()
        }
        function onDone(id, err, json) {
            if (id === page.reqId) {
                page.busy = false
                if (err) {
                    // keep what is on screen (cached or partial); only show the error if there is nothing
                    if (page.items.length === 0) page.setError(err)
                    return
                }
                const res = page.parse(json)
                if (!res) { if (page.items.length === 0) page.setError(qsTr("Got an unreadable answer from the app.")); return }
                if (page.progressive) {
                    // Rows of the copy shown before this refresh that did not come again (the feed got shorter) go.
                    const kept = {}
                    for (const k in page.slots) if (page.seenSlots[k]) kept[k] = page.slots[k]
                    if (Object.keys(kept).length !== Object.keys(page.slots).length) { page.slots = kept; page.rebuildFromSlots() }
                    page.cachePut({slots: Object.assign({}, page.slots)})
                    return
                }
                page.showResult(res)
                if (!res.more) page.cachePut({res: res})
                // (no eager "fetch more" here: the list calls maybeMore itself once it has laid the rows out)
            } else if (id === page.moreId) {
                page.loadingMore = false
                page.feedQueue = false
                if (err) { page.hasMore = false; return }   // keep what we have
                const res = page.parse(json)
                if (!res) { page.hasMore = false; return }
                page.hasMore = !!res.more
                if (res.items.length > 0) {
                    for (const i of res.items) model.append(i)
                    page.items = page.items.concat(res.items)
                }
                // A page from a feed says whether the next one follows by itself (Rust has dealt with its songs); the
                // feed keeps the list busy for as long as it runs.
                page.feedQueue = page.loadingMore = !!res.feeding
            }
        }
        // (a page showing an error may be waiting for exactly this: its request was refused for want of a session)
        function onLoggedInChanged() {
            if (page.needsLogin || page.reloadOnLogin || page.errorText.length > 0) page.load(true)
        }
        // Same account, fresh cookies: what was fetched (or refused) with the stale ones is loaded again.
        function onSessionRenewed() {
            if (page.needsLogin || page.reloadOnLogin || page.errorText.length > 0) page.load(true)
        }
    }

    function activate(i) {
        const item = itemAt(i)
        if (!item) return
        if (item.kind === "song") {
            if (collection) {
                // the whole list from the clicked row (counted by position: a song can be in a list twice), and the
                // pages not loaded yet follow into the queue
                // In order: a "Shuffle" pressed on some page earlier must not make the song after this one a random
                // one. Shuffle is on only while the user asks for it (that button, or the toggle in the player bar).
                Player.shuffle = false
                startPlaying(songs(), items.slice(0, i).filter(x => x.kind === "song").length)
            } else {
                Player.playSong(JSON.stringify(item))
            }
        } else {
            applicationWindow().open(item)
        }
    }

    Component.onCompleted: load()
    onFilterChanged: if (query || target === "library") load()

    ListModel { id: model }

    // One context menu for the whole list, pointed at a row when it opens (a menu per row made long lists slow to build).
    function openMenu(i) { rowMenu.at = i; rowMenu.item = itemAt(i); rowMenu.popup() }
    QQC2.Menu {
        id: rowMenu
        property int at: -1
        property string dl: "none"          // this song's download state, read each time the menu opens
        // The song itself, taken when the menu opens: rows can shift under an open menu (Home fills in row by row).
        property var item: null
        function field(key) { return item && item[key] ? item[key] : "" }
        readonly property string videoId: item ? item.videoId : ""
        property bool liked: false
        onAboutToShow: { dl = Downloads.state(videoId); liked = Backend.isLiked(videoId) }
        QQC2.MenuItem { text: qsTr("Play next"); icon.name: "media-skip-forward"; onTriggered: Player.playNext(JSON.stringify(rowMenu.item)) }
        QQC2.MenuItem { text: qsTr("Add to queue"); icon.name: "media-playlist-append"; onTriggered: Player.enqueue(JSON.stringify(rowMenu.item)) }
        QQC2.MenuItem {
            text: rowMenu.dl === "downloaded" ? qsTr("Remove download") : (rowMenu.dl === "queued" || rowMenu.dl === "downloading" ? qsTr("Cancel download") : (rowMenu.dl === "failed" ? qsTr("Retry download") : qsTr("Download")))
            icon.name: rowMenu.dl === "downloaded" || rowMenu.dl === "queued" || rowMenu.dl === "downloading" ? "edit-delete" : "download"
            onTriggered: {
                if (rowMenu.dl === "failed") Downloads.retry(rowMenu.videoId)
                else if (rowMenu.dl === "none") Downloads.download(JSON.stringify(rowMenu.item))
                else Downloads.remove(rowMenu.videoId)
            }
        }
        QQC2.MenuSeparator { visible: Backend.loggedIn }
        QQC2.MenuItem {
            visible: rowMenu.field("artistId") !== ""
            height: visible ? implicitHeight : 0
            text: qsTr("Go to artist")
            icon.name: "view-media-artist"
            onTriggered: applicationWindow().open({kind: "artist", browseId: rowMenu.field("artistId"), title: rowMenu.field("artist")})
        }
        QQC2.MenuItem {
            visible: rowMenu.field("albumId") !== ""
            height: visible ? implicitHeight : 0
            text: qsTr("Go to album")
            icon.name: "media-optical-audio"
            onTriggered: applicationWindow().open({kind: "album", browseId: rowMenu.field("albumId"), title: rowMenu.field("title")})
        }
        // Plays this song with the rest of its album around it in the queue (a plain click is followed by a radio).
        QQC2.MenuItem {
            visible: rowMenu.field("albumId") !== "" && rowMenu.field("kind") === "song"
            height: visible ? implicitHeight : 0
            text: qsTr("Play album")
            icon.name: "media-playback-start"
            onTriggered: Player.playAlbum(JSON.stringify(rowMenu.item))
        }
        QQC2.MenuItem {
            visible: Backend.loggedIn
            height: visible ? implicitHeight : 0
            text: rowMenu.liked ? qsTr("Remove from liked music") : qsTr("Like")
            icon.name: rowMenu.liked ? "starred-symbolic" : "non-starred-symbolic"
            onTriggered: Backend.setLiked(rowMenu.videoId, !rowMenu.liked)
        }
        QQC2.MenuItem {
            visible: Backend.loggedIn
            height: visible ? implicitHeight : 0
            text: qsTr("Add to playlist…")
            icon.name: "list-add"
            onTriggered: applicationWindow().addToPlaylist(rowMenu.item)
        }
    }

    // One shelf: a heading and a horizontal row of cards (round for artists).
    component ShelfRow: ColumnLayout {
        id: shelf
        required property int index
        // The shelf at this position. Rows arrive one at a time on Home, so `allShelves` is rebuilt often; the cards are
        // only handed a new list when this shelf's own content changed, so the other shelves are not rebuilt and flicker.
        readonly property var modelData: page.allShelves[index] || ({title: "", items: []})
        property var cards: []
        property string signature: ""
        function refresh() {
            const its = modelData.items
            const sig = its.length + "|" + its.map(i => i.videoId || i.browseId || i.title).join(",")
            if (sig !== signature) { signature = sig; cards = its }
        }
        onModelDataChanged: refresh()
        Component.onCompleted: refresh()
        Layout.fillWidth: true
        spacing: 0
        Kirigami.Heading {
            text: shelf.modelData.title
            level: 3
            visible: text.length > 0
            Layout.leftMargin: Kirigami.Units.largeSpacing
            Layout.topMargin: Kirigami.Units.smallSpacing
        }
        ListView {
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 12.5
            orientation: ListView.Horizontal
            clip: true
            spacing: Kirigami.Units.smallSpacing
            leftMargin: Kirigami.Units.smallSpacing
            rightMargin: Kirigami.Units.smallSpacing
            model: shelf.cards
            QQC2.ScrollBar.horizontal: QQC2.ScrollBar {}
            delegate: QQC2.ItemDelegate {
                id: card
                required property var modelData
                readonly property bool round: modelData.kind === "artist"
                width: Kirigami.Units.gridUnit * 9
                height: ListView.view.height - Kirigami.Units.largeSpacing
                onClicked: page.activateItem(modelData)
                contentItem: ColumnLayout {
                    spacing: Kirigami.Units.smallSpacing
                    CoverImage {
                        Layout.fillWidth: true
                        Layout.preferredHeight: width
                        source: card.modelData.thumbnail || ""
                        decode: 400
                        radius: card.round ? width / 2 : Kirigami.Units.smallSpacing
                    }
                    QQC2.Label {
                        Layout.fillWidth: true
                        text: card.modelData.title || ""
                        wrapMode: Text.Wrap
                        maximumLineCount: 2
                        elide: Text.ElideRight
                        horizontalAlignment: card.round ? Text.AlignHCenter : Text.AlignLeft
                        color: Kirigami.Theme.textColor
                    }
                    QQC2.Label {
                        Layout.fillWidth: true
                        visible: text.length > 0
                        text: card.modelData.subtitle || ""
                        elide: Text.ElideRight
                        horizontalAlignment: card.round ? Text.AlignHCenter : Text.AlignLeft
                        font: Kirigami.Theme.smallFont
                        opacity: 0.7
                    }
                    Item { Layout.fillHeight: true }
                }
            }
        }
    }

    header: ColumnLayout {
        visible: page.activeChips.length > 0
        spacing: 0

        // Only built when there are filters: the desktop style's TabBar reads its first tab's height and complains with none.
        Loader {
            Layout.fillWidth: true
            active: page.activeChips.length > 0
            visible: active
            sourceComponent: QQC2.TabBar {
                currentIndex: {
                    for (let i = 0; i < page.activeChips.length; ++i)
                        if (page.activeChips[i][1] === page.filter) return i
                    return 0
                }
                onCurrentIndexChanged: {
                    const c = page.activeChips[currentIndex]
                    if (c && c[1] !== page.filter) page.filter = c[1]
                }
                Repeater {
                    model: page.activeChips
                    QQC2.TabButton {
                        required property var modelData
                        text: modelData[0]
                    }
                }
            }
        }
    }

    ListView {
        id: view
        model: model
        clip: true
        reuseItems: true
        // No row is "current" until the keyboard picks one: otherwise the first row of every page is drawn selected.
        currentIndex: -1
        // Lazy paging: look again whenever the scroll position or content size changes.
        onContentYChanged: page.maybeMore()
        onContentHeightChanged: page.maybeMore()

        // Until the user scrolls, stay at the top while the header fills in (description, shelves, images arrive late).
        onMovementStarted: page.pinTop = false

        header: ColumnLayout {
          width: view.width
          spacing: Kirigami.Units.largeSpacing
          onHeightChanged: if (page.pinTop) view.positionViewAtBeginning()

          RowLayout {
            Layout.fillWidth: true
            visible: page.collection && page.heading.length > 0
            spacing: Kirigami.Units.largeSpacing
            CoverImage {
                source: page.headThumb
                decode: 600
                Layout.preferredWidth: Kirigami.Units.gridUnit * 8
                Layout.preferredHeight: Kirigami.Units.gridUnit * 8
                Layout.margins: Kirigami.Units.largeSpacing
                Layout.alignment: Qt.AlignTop
                radius: width * AppConfig.coverRadiusPercent / 100
            }
            ColumnLayout {
                Layout.fillWidth: true
                Layout.alignment: Qt.AlignTop
                Layout.topMargin: Kirigami.Units.largeSpacing
                Kirigami.Heading { text: page.heading; level: 1; wrapMode: Text.Wrap; Layout.fillWidth: true }
                QQC2.Label { text: page.subheading; wrapMode: Text.Wrap; opacity: 0.7; Layout.fillWidth: true; visible: text.length > 0 }
                // year • songs • length (albums and playlists)
                QQC2.Label {
                    readonly property string line: page.hero && page.hero.stats ? page.hero.stats.map(s => s.text).join(" • ") : ""
                    text: line; visible: line.length > 0; Layout.fillWidth: true; wrapMode: Text.Wrap
                    font: Kirigami.Theme.smallFont; opacity: 0.7
                }
                RowLayout {
                    QQC2.Button {
                        text: qsTr("Play")
                        icon.name: "media-playback-start"
                        enabled: page.songs().length > 0
                        onClicked: { Player.shuffle = false; page.startPlaying(page.songs(), 0) }   // "Play" is in order
                    }
                    QQC2.Button {
                        text: qsTr("Shuffle")
                        icon.name: "media-playlist-shuffle"
                        enabled: page.songs().length > 0
                        onClicked: {
                            const s = page.songs()
                            Player.shuffle = true
                            page.startPlaying(s, Math.floor(Math.random() * s.length))
                        }
                    }
                    QQC2.Button {
                        text: qsTr("Add to queue")
                        icon.name: "media-playlist-append"
                        enabled: page.songs().length > 0
                        onClicked: page.enqueueAll()
                    }
                    QQC2.Button {
                        text: qsTr("Download")
                        icon.name: "download"
                        enabled: page.songs().length > 0
                        onClicked: page.downloadAll()
                    }
                    QQC2.Button {
                        visible: page.radio !== null
                        text: qsTr("Radio")
                        icon.name: "media-playlist-shuffle"
                        onClicked: applicationWindow().open(page.radio)
                    }
                }

                // The description or artist bio (YouTube's own text), collapsed to a few lines, with the Wikipedia link YouTube gives.
                QQC2.Label {
                    id: descr
                    property bool expanded: false
                    Layout.fillWidth: true
                    Layout.topMargin: Kirigami.Units.smallSpacing
                    text: page.hero && page.hero.description ? page.hero.description : ""
                    visible: text.length > 0
                    wrapMode: Text.Wrap
                    maximumLineCount: expanded ? 1000 : 3
                    elide: Text.ElideRight
                    opacity: 0.85
                }
                RowLayout {
                    visible: descr.visible
                    Layout.fillWidth: true
                    QQC2.ToolButton {
                        visible: descr.truncated || descr.expanded
                        text: descr.expanded ? qsTr("Show less") : qsTr("Read more")
                        onClicked: descr.expanded = !descr.expanded
                    }
                    QQC2.ToolButton {
                        visible: page.hero && page.hero.wikipediaUrl && page.hero.wikipediaUrl.length > 0
                        icon.name: "internet-services"
                        text: qsTr("From Wikipedia")
                        onClicked: Qt.openUrlExternally(page.hero.wikipediaUrl)
                    }
                }
            }
          }

          // Home's and Explore's sections as shelves of cards (an artist's shelves come after its songs, in the footer).
          Repeater {
            model: page.shelfMode ? page.allShelves.length : 0
            delegate: ShelfRow {}
          }
        }

        footer: ColumnLayout {
            width: view.width
            spacing: Kirigami.Units.largeSpacing
            // an artist's albums, singles, playlists and similar artists, below the top songs
            Repeater {
                model: page.shelfMode ? 0 : page.shelves.length
                delegate: ShelfRow {}
            }
            Item {
                Layout.fillWidth: true
                Layout.preferredHeight: page.loadingMore ? Kirigami.Units.gridUnit * 3 : 0
                QQC2.BusyIndicator { anchors.centerIn: parent; running: page.loadingMore; visible: running }
            }
        }

        section.property: "section"
        section.delegate: Kirigami.ListSectionHeader {
            required property string section
            width: view.width
            label: section
            visible: section.length > 0
            height: visible ? implicitHeight : 0
        }

        delegate: QQC2.ItemDelegate {
            id: row
            required property int index
            required property string title
            required property string subtitle
            required property string kind
            required property string videoId
            required property string thumbnail
            required property string duration

            readonly property bool liked: kind === "song" && Backend.loggedIn && Backend.likedRev >= 0 && Backend.isLiked(videoId)   // (the revision is *used*, so a change of it re-reads the heart)

            width: ListView.view.width
            highlighted: kind === "song" && Player.hasTrack && Player.videoId === videoId
            onClicked: page.activate(index)
            TapHandler { acceptedButtons: Qt.RightButton; enabled: row.kind === "song"; onTapped: page.openMenu(row.index) }

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing
                CoverImage {
                    source: row.thumbnail
                    decode: 160
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 2.5
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 2.5
                }
                ColumnLayout {
                    spacing: 0
                    Layout.fillWidth: true
                    QQC2.Label { text: row.title; elide: Text.ElideRight; Layout.fillWidth: true; color: row.highlighted ? Kirigami.Theme.highlightedTextColor : Kirigami.Theme.textColor }
                    QQC2.Label {
                        text: row.subtitle
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                        visible: text.length > 0
                        font: Kirigami.Theme.smallFont
                        opacity: 0.7
                    }
                }
                Kirigami.Icon {
                    visible: row.kind === "song" && row.liked
                    source: "starred-symbolic"
                    implicitWidth: Kirigami.Units.iconSizes.small
                    implicitHeight: implicitWidth
                    opacity: 0.7
                }
                QQC2.Label { text: row.duration; opacity: 0.7; visible: row.duration.length > 0 }
                QQC2.ToolButton {
                    visible: row.kind === "song"
                    icon.name: "overflow-menu"
                    display: QQC2.AbstractButton.IconOnly
                    text: qsTr("More")
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.text: text
                    onClicked: page.openMenu(row.index)
                }
            }
        }

        Kirigami.PlaceholderMessage {
            anchors.centerIn: parent
            width: parent.width - Kirigami.Units.gridUnit * 4
            visible: !page.busy && page.items.length === 0 && page.shelves.length === 0
            icon.name: page.errorText ? "dialog-error" : "edit-none"
            text: page.errorText || page.emptyText
            explanation: page.errorDetail
            helpfulAction: Kirigami.Action {
                text: page.authProblem ? qsTr("Open Settings") : qsTr("Retry")
                icon.name: page.authProblem ? "configure" : "view-refresh"
                onTriggered: page.authProblem ? applicationWindow().go("settings") : page.load()
            }
        }

        QQC2.BusyIndicator {
            anchors.centerIn: parent
            running: page.busy && page.items.length === 0
            visible: running
        }
    }
}
