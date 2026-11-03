// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// The hamburger drawer: a slide-over, about 20 grid units wide (never wider than 90% of the window).
//   header      app name + account chip
//   search      submits to root.search(text)
//   entries     from Rust (`Nav.entries`), grouped: Home / Explore / Library | Now playing / Queue | Settings
//   footer      mini now-playing card (cover, title, play/pause through the Playback seam)
// It closes after a selection and is fully keyboard driven: Up/Down move between rows, Enter selects, Escape closes.
//
// Expectations on the ApplicationWindow (Main.qml): go(name), search(text), showNowPlaying(tab), toggleFullScreen(),
// and a `media` property exposing toggle() / playing. The current row is derived from the page on top of the stack
// (`navName` if the page defines it, else `target`; Now Playing and Settings are recognised); `currentOverride`
// wins when set.
Kirigami.OverlayDrawer {
    id: drawer

    property string currentOverride
    readonly property var win: applicationWindow()
    readonly property Item topPage: win && win.pageStack ? win.pageStack.currentItem : null
    readonly property var entries: {
        try { return JSON.parse(Nav.entries) } catch (e) { return [] }
    }
    readonly property string current: {
        if (currentOverride) return currentOverride
        const p = topPage
        if (!p) return ""
        if (p.navName) return p.navName
        if (p.isNowPlaying) return p.tab === "queue" ? "queue" : "nowplaying"
        if (String(p).indexOf("SettingsPage") === 0) return "settings"
        if (p.query) return ""
        return p.target === "home" || p.target === "explore" || p.target === "library" ? p.target : ""
    }

    function badgeFor(name) {
        if (name !== "queue") return ""
        let n = 0
        try { n = JSON.parse(Player.queue).length } catch (e) { n = 0 }
        return n > 0 ? String(n) : ""
    }

    function activate(name) {
        drawer.close()
        switch (name) {
        case "nowplaying":
        case "queue": win.showNowPlaying("queue"); break
        case "fullscreen": win.toggleFullScreen(); break
        default: win.go(name)
        }
    }

    edge: Qt.application.layoutDirection === Qt.RightToLeft ? Qt.RightEdge : Qt.LeftEdge
    modal: true
    handleClosedIcon.name: "application-menu"
    handleOpenIcon.name: "go-previous"
    width: Math.min(Kirigami.Units.gridUnit * 20, (win ? win.width : Kirigami.Units.gridUnit * 22) * 0.9)
    padding: 0

    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.Window
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    onDrawerOpenChanged: if (drawerOpen) Qt.callLater(() => { const r = rows.itemAt(Math.max(0, drawer.entries.findIndex(e => e.name === drawer.current))); if (r) r.forceActiveFocus(Qt.TabFocusReason) })

    contentItem: ColumnLayout {
        spacing: 0

        // ---- header
        Rectangle {
            Layout.fillWidth: true
            implicitHeight: headerColumn.implicitHeight + Kirigami.Units.gridUnit * 1.5
            gradient: Gradient {
                GradientStop { position: 0; color: Qt.alpha(Kirigami.Theme.highlightColor, 0.28) }
                GradientStop { position: 1; color: Qt.alpha(Kirigami.Theme.highlightColor, 0) }
            }
            ColumnLayout {
                id: headerColumn
                anchors { left: parent.left; right: parent.right; verticalCenter: parent.verticalCenter; leftMargin: Kirigami.Units.gridUnit; rightMargin: Kirigami.Units.gridUnit }
                spacing: Kirigami.Units.smallSpacing * 1.5
                RowLayout {
                    spacing: Kirigami.Units.largeSpacing
                    Kirigami.Icon { source: "multimedia-audio-player"; fallback: "audio-x-generic"; implicitWidth: Kirigami.Units.iconSizes.medium; implicitHeight: implicitWidth }
                    Kirigami.Heading { level: 2; text: qsTr("YT Music"); font.weight: Font.Bold; Layout.fillWidth: true }
                }
                // account chip
                QQC2.AbstractButton {
                    id: account
                    Layout.alignment: Qt.AlignLeft
                    padding: Kirigami.Units.smallSpacing * 1.5
                    leftPadding: Kirigami.Units.smallSpacing * 2
                    rightPadding: Kirigami.Units.largeSpacing
                    activeFocusOnTab: true
                    onClicked: drawer.activate("settings")
                    Accessible.role: Accessible.Button
                    Accessible.name: Backend.loggedIn ? qsTr("Signed in. Open settings") : qsTr("Not signed in. Open settings to sign in")
                    background: Rectangle {
                        radius: height / 2
                        color: Qt.alpha(Kirigami.Theme.textColor, account.hovered || account.visualFocus ? 0.16 : 0.09)
                        border.width: account.visualFocus ? 2 : 0
                        border.color: Kirigami.Theme.highlightColor
                    }
                    contentItem: RowLayout {
                        spacing: Kirigami.Units.smallSpacing
                        Kirigami.Icon {
                            source: Backend.loggedIn ? "user-online" : "user-offline"
                            implicitWidth: Kirigami.Units.iconSizes.small
                            implicitHeight: implicitWidth
                        }
                        QQC2.Label { text: Backend.loggedIn ? qsTr("Signed in") : qsTr("Sign in") }
                    }
                    HoverHandler { cursorShape: Qt.PointingHandCursor }
                }
            }
        }

        // ---- search
        Kirigami.SearchField {
            id: search
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.gridUnit * 0.75
            Layout.topMargin: Kirigami.Units.smallSpacing
            placeholderText: qsTr("Search music")
            onAccepted: {
                const text = search.text
                search.text = ""
                drawer.close()
                if (text.trim().length > 0) drawer.win.search(text)
            }
            KeyNavigation.down: rows.itemAt(0)
        }

        // ---- entries
        QQC2.ScrollView {
            id: scroll
            Layout.fillWidth: true
            Layout.fillHeight: true
            contentWidth: availableWidth
            QQC2.ScrollBar.horizontal.policy: QQC2.ScrollBar.AlwaysOff

            Column {
                id: column
                width: scroll.availableWidth
                spacing: 0

                Repeater {
                    id: rows
                    model: drawer.entries
                    delegate: Column {
                        id: entry
                        required property var modelData
                        required property int index
                        readonly property bool newGroup: index > 0 && drawer.entries[index - 1].group !== modelData.group
                        readonly property bool isCurrent: drawer.current === modelData.name
                        readonly property bool usable: !modelData.needsTrack || Player.hasTrack
                        readonly property string badge: drawer.badgeFor(modelData.badge || "")
                        width: column.width
                        function forceActiveFocus(reason) { row.forceActiveFocus(reason) }

                        Item { width: 1; height: Kirigami.Units.smallSpacing; visible: entry.newGroup }
                        Kirigami.Separator { width: parent.width - Kirigami.Units.gridUnit * 1.5; x: Kirigami.Units.gridUnit * 0.75; visible: entry.newGroup }
                        Item { width: 1; height: Kirigami.Units.smallSpacing; visible: entry.newGroup }

                        QQC2.AbstractButton {
                            id: row
                            width: parent.width - Kirigami.Units.gridUnit * 1.0
                            x: Kirigami.Units.gridUnit * 0.5
                            height: Kirigami.Units.gridUnit * 2.7
                            enabled: entry.usable
                            activeFocusOnTab: true
                            hoverEnabled: true
                            onClicked: drawer.activate(entry.modelData.name)
                            Keys.onReturnPressed: clicked()
                            Keys.onEnterPressed: clicked()
                            Keys.onDownPressed: { const n = rows.itemAt(entry.index + 1); if (n) n.forceActiveFocus(Qt.TabFocusReason) }
                            Keys.onUpPressed: {
                                const n = entry.index > 0 ? rows.itemAt(entry.index - 1) : null
                                if (n) n.forceActiveFocus(Qt.BacktabFocusReason); else search.forceActiveFocus()
                            }
                            Accessible.role: Accessible.MenuItem
                            Accessible.name: entry.modelData.label + (entry.badge ? ", " + entry.badge : "")
                            Accessible.checked: entry.isCurrent

                            background: Rectangle {
                                radius: Kirigami.Units.cornerRadius * 1.5
                                color: entry.isCurrent ? Qt.alpha(Kirigami.Theme.highlightColor, row.pressed ? 0.32 : row.hovered ? 0.26 : 0.2)
                                                       : Qt.alpha(Kirigami.Theme.textColor, row.pressed ? 0.14 : row.hovered ? 0.08 : 0)
                                border.width: row.visualFocus ? 2 : 0
                                border.color: Kirigami.Theme.highlightColor
                                Behavior on color { enabled: !AppConfig.reduceMotion; ColorAnimation { duration: 100 } }
                                // current-page marker
                                Rectangle {
                                    visible: entry.isCurrent
                                    width: 3
                                    height: parent.height * 0.5
                                    radius: 1.5
                                    anchors { left: parent.left; leftMargin: 3; verticalCenter: parent.verticalCenter }
                                    color: Kirigami.Theme.highlightColor
                                }
                            }
                            contentItem: RowLayout {
                                spacing: Kirigami.Units.largeSpacing
                                opacity: entry.usable ? 1 : 0.45
                                Item { Layout.preferredWidth: Kirigami.Units.smallSpacing }
                                Kirigami.Icon {
                                    source: entry.modelData.icon
                                    implicitWidth: Kirigami.Units.iconSizes.smallMedium
                                    implicitHeight: implicitWidth
                                    color: entry.isCurrent ? Kirigami.Theme.highlightColor : Kirigami.Theme.textColor
                                }
                                QQC2.Label {
                                    Layout.fillWidth: true
                                    text: entry.modelData.label
                                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.1
                                    font.weight: entry.isCurrent ? Font.DemiBold : Font.Normal
                                    elide: Text.ElideRight
                                }
                                Rectangle {
                                    visible: entry.badge.length > 0
                                    Layout.preferredWidth: Math.max(height, badgeText.implicitWidth + Kirigami.Units.largeSpacing)
                                    Layout.preferredHeight: Kirigami.Units.gridUnit * 1.4
                                    Layout.rightMargin: Kirigami.Units.smallSpacing
                                    radius: height / 2
                                    color: Qt.alpha(Kirigami.Theme.textColor, 0.14)
                                    QQC2.Label { id: badgeText; anchors.centerIn: parent; text: entry.badge; font: Kirigami.Theme.smallFont }
                                }
                            }
                            HoverHandler { cursorShape: Qt.PointingHandCursor }
                        }
                    }
                }
            }
        }

        // ---- mini now playing
        Rectangle {
            id: mini
            visible: Player.hasTrack
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.gridUnit * 0.75
            implicitHeight: Kirigami.Units.gridUnit * 4.2
            radius: Kirigami.Units.cornerRadius * 2
            color: Qt.alpha(Kirigami.Theme.textColor, miniArea.containsMouse ? 0.12 : 0.07)
            border.width: miniArea.activeFocus ? 2 : 0
            border.color: Kirigami.Theme.highlightColor

            MouseArea {
                id: miniArea
                anchors.fill: parent
                hoverEnabled: true
                activeFocusOnTab: true
                cursorShape: Qt.PointingHandCursor
                onClicked: drawer.activate("nowplaying")
                Keys.onReturnPressed: clicked(null)
                Keys.onSpacePressed: clicked(null)
                Accessible.role: Accessible.Button
                Accessible.name: qsTr("Open now playing: %1").arg(Player.title)
            }
            RowLayout {
                anchors { fill: parent; margins: Kirigami.Units.smallSpacing * 1.5 }
                spacing: Kirigami.Units.largeSpacing
                CoverImage {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 3
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 3
                    source: Player.thumbnail
                    decode: 128
                    radiusPercent: Math.max(AppConfig.coverRadiusPercent, 8)
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    QQC2.Label { Layout.fillWidth: true; text: Player.title; font.weight: Font.DemiBold; elide: Text.ElideRight; maximumLineCount: 1 }
                    QQC2.Label { Layout.fillWidth: true; text: Player.artist; font: Kirigami.Theme.smallFont; opacity: 0.7; elide: Text.ElideRight; maximumLineCount: 1 }
                }
                QQC2.RoundButton {
                    icon.name: drawer.win && drawer.win.media && drawer.win.media.playing ? "media-playback-pause" : "media-playback-start"
                    onClicked: drawer.win.media.toggle()
                    QQC2.ToolTip.text: drawer.win && drawer.win.media && drawer.win.media.playing ? qsTr("Pause") : qsTr("Play")
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                    Accessible.name: QQC2.ToolTip.text
                }
            }
        }
    }
}
