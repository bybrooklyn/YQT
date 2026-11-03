// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// One scrolling page, six sections. Every setting row is generated from the Rust metadata table
// (AppSettings.describe); this file only decides which groups go where.
Kirigami.Page {
    id: page
    title: qsTr("Settings")
    padding: 0
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    property string query: ""
    property bool showAdvanced: false
    property int currentSection: 0
    readonly property var sectionItems: [secAccount, secPlayback, secDownloads, secLook, secPrivacy, secAdvanced]
    readonly property var sectionInfo: [
        { title: qsTr("Account"), icon: "user-identity" },
        { title: qsTr("Playback"), icon: "media-playback-start" },
        { title: qsTr("Downloads"), icon: "folder-download" },
        { title: qsTr("Appearance"), icon: "preferences-desktop-theme" },
        { title: qsTr("Privacy"), icon: "preferences-system-privacy" },
        { title: qsTr("Advanced"), icon: "configure" }
    ]
    readonly property bool nothingFound: {
        if (query.trim() === "") return false
        for (let i = 0; i < sectionItems.length; ++i)
            if (sectionItems[i].matched) return false
        return true
    }
    // Optional pieces built by other parts of the app: used when they exist.
    readonly property var downloadsService: { try { return Downloads } catch (e) { return null } }
    // Not an error when the file is not there (yet): the card then shows a hint instead.
    readonly property var equalizerComponent: Qt.createComponent(Qt.resolvedUrl("EqualizerPanel.qml"), Component.PreferSynchronous)
    readonly property bool equalizerExists: equalizerComponent.status === Component.Ready

    function jumpTo(i) {
        const item = sectionItems[i]
        if (!item || !item.visible) return
        const target = Math.max(0, Math.min(item.y + column.y - Kirigami.Units.smallSpacing, flick.contentHeight - flick.height))
        scrollAnim.stop()
        scrollAnim.to = target
        scrollAnim.duration = AppConfig.reduceMotion ? 0 : 220
        scrollAnim.start()
        page.currentSection = i
    }
    function updateCurrent() {
        let idx = 0
        for (let i = 0; i < sectionItems.length; ++i) {
            const s = sectionItems[i]
            if (s.visible && s.y + column.y - Kirigami.Units.gridUnit * 3 <= flick.contentY) idx = i
        }
        if (flick.contentY + flick.height >= flick.contentHeight - 2) {
            for (let j = sectionItems.length - 1; j >= 0; --j)
                if (sectionItems[j].visible) { idx = j; break }
        }
        page.currentSection = idx
    }
    function formatBytes(b) {
        if (b === undefined || b === null || isNaN(b)) return ""
        const u = ["B", "KB", "MB", "GB", "TB"]
        let i = 0
        while (b >= 1000 && i < u.length - 1) { b /= 1000; ++i }
        return (i === 0 ? String(Math.round(b)) : b.toFixed(1)) + " " + u[i]
    }

    // ---- sticky top: search and jump bar --------------------------------------------------------
    header: QQC2.ToolBar {
        Kirigami.Theme.inherit: false
        Kirigami.Theme.colorGroup: Kirigami.Theme.Active
        contentItem: ColumnLayout {
            spacing: Kirigami.Units.smallSpacing
            RowLayout {
                Layout.fillWidth: true
                spacing: Kirigami.Units.largeSpacing
                Kirigami.SearchField {
                    id: search
                    Layout.fillWidth: true
                    placeholderText: qsTr("Search settings")
                    Accessible.name: qsTr("Search settings")
                    onTextChanged: page.query = text
                    Keys.onEscapePressed: text = ""
                }
                QQC2.CheckBox {
                    text: qsTr("Show advanced")
                    checked: page.showAdvanced
                    onToggled: page.showAdvanced = checked
                }
            }
            ListView {
                id: jumpBar
                Layout.fillWidth: true
                implicitHeight: Kirigami.Units.gridUnit * 2
                orientation: ListView.Horizontal
                clip: true
                spacing: Kirigami.Units.smallSpacing
                boundsBehavior: Flickable.StopAtBounds
                model: page.sectionInfo
                currentIndex: page.currentSection
                delegate: QQC2.ToolButton {
                    required property var modelData
                    required property int index
                    text: modelData.title
                    icon.name: modelData.icon
                    checkable: true
                    checked: index === page.currentSection
                    enabled: page.sectionItems[index].visible
                    onClicked: page.jumpTo(index)
                    Accessible.name: qsTr("Jump to %1").arg(modelData.title)
                }
            }
        }
    }

    Flickable {
        id: flick
        anchors.fill: parent
        clip: true
        contentWidth: width
        contentHeight: column.height + Kirigami.Units.gridUnit * 3
        boundsBehavior: Flickable.StopAtBounds
        QQC2.ScrollBar.vertical: QQC2.ScrollBar {}
        onContentYChanged: page.updateCurrent()
        onHeightChanged: page.updateCurrent()

        NumberAnimation { id: scrollAnim; target: flick; property: "contentY"; easing.type: Easing.InOutQuad }

        ColumnLayout {
            id: column
            readonly property real gutter: Kirigami.Units.gridUnit
            width: Math.min(flick.width - 2 * gutter, Kirigami.Units.gridUnit * 52)
            x: Math.max(gutter, (flick.width - width) / 2)
            y: Kirigami.Units.smallSpacing
            spacing: Kirigami.Units.gridUnit

            Kirigami.PlaceholderMessage {
                visible: page.nothingFound
                Layout.fillWidth: true
                Layout.topMargin: Kirigami.Units.gridUnit * 3
                icon.name: "search"
                text: qsTr("No settings match “%1”").arg(page.query)
                explanation: page.showAdvanced ? "" : qsTr("Advanced settings are included in searches.")
            }

            // ============================== 1. Account ==============================
            SettingSection {
                id: secAccount
                title: qsTr("Account")
                subtitle: qsTr("Your library, liked music and history need a signed-in YouTube Music session.")
                icon: "user-identity"
                SettingCard {
                    title: qsTr("YouTube Music account")
                    query: page.query
                    hasStaticContent: true
                    keywords: "sign in login log out sign out cookies browser import session account"
                    BrowserImport {}
                }
            }

            // ============================== 2. Playback & equalizer ==============================
            SettingSection {
                id: secPlayback
                title: qsTr("Playback & equalizer")
                subtitle: qsTr("How songs are played, mixed and kept ready.")
                icon: "media-playback-start"
                SettingCard {
                    title: qsTr("Playback")
                    group: "audio"
                    query: page.query
                    showAdvanced: page.showAdvanced
                }
                SettingCard {
                    title: qsTr("Equalizer")
                    query: page.query
                    hasStaticContent: true
                    keywords: "equalizer eq bass treble preset bands"
                    Loader {
                        Layout.fillWidth: true
                        active: page.equalizerExists
                        sourceComponent: page.equalizerExists ? page.equalizerComponent : null
                    }
                    QQC2.Label {
                        visible: !page.equalizerExists
                        Layout.fillWidth: true
                        wrapMode: Text.Wrap
                        opacity: 0.7
                        text: qsTr("The equalizer is part of the built-in audio engine and is not available in this build yet.")
                    }
                }
                SettingCard {
                    title: qsTr("Stream source")
                    description: qsTr("Songs are streamed through YouTube’s visionOS route. Check it if playback fails.")
                    query: page.query
                    hasStaticContent: true
                    keywords: "stream health check source test playback fails"
                    StreamHealth {}
                }
            }

            // ============================== 3. Downloads ==============================
            SettingSection {
                id: secDownloads
                title: qsTr("Downloads")
                subtitle: qsTr("Keep music available offline.")
                icon: "folder-download"
                SettingCard {
                    title: qsTr("Storage")
                    group: "downloads"
                    query: page.query
                    showAdvanced: page.showAdvanced
                    hasStaticContent: true
                    keywords: "storage used cap limit gigabytes folder quality"
                    ColumnLayout {
                        id: usage
                        readonly property real usedBytes: page.downloadsService && page.downloadsService.usedBytes !== undefined ? Number(page.downloadsService.usedBytes) : -1
                        readonly property real capBytes: AppSettings.revision < 0 ? 0 : Number(AppSettings.get("downloads.cap_gb")) * 1e9
                        visible: usedBytes >= 0
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing
                        QQC2.ProgressBar {
                            Layout.fillWidth: true
                            from: 0
                            to: Math.max(1, usage.capBytes)
                            value: Math.min(usage.usedBytes, to)
                        }
                        QQC2.Label {
                            text: qsTr("%1 used of %2").arg(page.formatBytes(usage.usedBytes)).arg(page.formatBytes(usage.capBytes))
                            opacity: 0.7
                            font: Kirigami.Theme.smallFont
                        }
                    }
                }
                SettingCard {
                    title: qsTr("Smart downloads")
                    description: qsTr("Off by default. When on, the app keeps these songs downloaded for you and removes old ones to stay within your storage limit.")
                    group: "smart"
                    query: page.query
                    showAdvanced: page.showAdvanced
                    gate: "downloads.smart_enabled"
                    gates: ({ "downloads.low_battery_percent": "downloads.pause_on_low_battery" })
                }
            }

            // ============================== 4. Lyrics & appearance ==============================
            SettingSection {
                id: secLook
                title: qsTr("Lyrics & appearance")
                subtitle: qsTr("How the app looks, how lyrics are found and shown, and how you control it.")
                icon: "preferences-desktop-theme"
                SettingCard {
                    title: qsTr("Lyrics")
                    group: "lyrics"
                    query: page.query
                    showAdvanced: page.showAdvanced
                    gates: ({
                        "lyrics.providers": "lyrics.enabled",
                        "lyrics.prefer_synced": "lyrics.enabled",
                        "lyrics.cache_days": "lyrics.enabled",
                        "lyrics.min_lines": "lyrics.enabled"
                    })
                }
                SettingCard {
                    title: qsTr("Appearance")
                    group: "look"
                    query: page.query
                    showAdvanced: page.showAdvanced
                }
                SettingCard {
                    title: qsTr("Window")
                    group: "window"
                    query: page.query
                    showAdvanced: page.showAdvanced
                }
                SettingCard {
                    title: qsTr("Keyboard shortcuts")
                    description: qsTr("Press Record and then the keys you want. Two actions cannot share a shortcut.")
                    group: "keys"
                    query: page.query
                    showAdvanced: page.showAdvanced
                    hasStaticContent: true
                    keywords: "keyboard shortcuts keys hotkeys bindings"
                    QQC2.Button {
                        text: qsTr("Reset all shortcuts")
                        icon.name: "edit-undo"
                        onClicked: AppSettings.resetSection("keys")
                    }
                }
            }

            // ============================== 5. Privacy & network ==============================
            SettingSection {
                id: secPrivacy
                title: qsTr("Privacy & network")
                subtitle: qsTr("What leaves your computer, and the knobs that control it.")
                icon: "preferences-system-privacy"
                SettingCard {
                    title: qsTr("What is sent where")
                    description: qsTr("Generated from your current settings. Grey entries are switched off.")
                    query: page.query
                    hasStaticContent: true
                    keywords: "privacy sends data lyrics providers play reporting google covers streams telemetry"
                    PrivacyList {}
                }
                SettingCard {
                    title: qsTr("Play reporting")
                    group: "reporting"
                    query: page.query
                    showAdvanced: page.showAdvanced
                    gate: "reporting.enabled"
                }
                SettingCard {
                    title: qsTr("Security")
                    group: "security"
                    query: page.query
                    showAdvanced: page.showAdvanced
                }
                SettingCard {
                    title: qsTr("Network")
                    group: "network"
                    query: page.query
                    showAdvanced: page.showAdvanced
                }
            }

            // ============================== 6. Storage, advanced & about ==============================
            SettingSection {
                id: secAdvanced
                title: qsTr("Storage, advanced & about")
                subtitle: qsTr("Files, diagnostics and everything else.")
                icon: "configure"
                SettingCard {
                    title: qsTr("Files")
                    query: page.query
                    hasStaticContent: true
                    keywords: "config file folder logs open reset all defaults storage cache"
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing
                        QQC2.Label { text: qsTr("Settings file"); font.weight: Font.DemiBold }
                        QQC2.TextField {
                            Layout.fillWidth: true
                            readOnly: true
                            text: AppSettings.configPath
                            Accessible.name: qsTr("Settings file location")
                        }
                        QQC2.Label {
                            Layout.fillWidth: true
                            wrapMode: Text.Wrap
                            opacity: 0.7
                            font: Kirigami.Theme.smallFont
                            text: qsTr("You can edit it by hand: your comments and any keys the app does not know are kept when settings change here.")
                        }
                        Flow {
                            Layout.fillWidth: true
                            spacing: Kirigami.Units.smallSpacing
                            QQC2.Button {
                                text: qsTr("Open settings folder")
                                icon.name: "document-open-folder"
                                onClicked: Qt.openUrlExternally(AppSettings.configFolderUrl())
                            }
                            QQC2.Button {
                                text: qsTr("Open logs folder")
                                icon.name: "document-open-folder"
                                onClicked: Qt.openUrlExternally(AppSettings.logsFolderUrl())
                            }
                            QQC2.Button {
                                text: qsTr("Reset all settings…")
                                icon.name: "edit-undo"
                                onClicked: resetDialog.open()
                            }
                        }
                    }
                }
                SettingCard {
                    title: qsTr("Library")
                    description: qsTr("How much each page loads.")
                    group: "library"
                    advancedOnly: true
                    query: page.query
                    showAdvanced: page.showAdvanced
                }
                SettingCard {
                    title: qsTr("Caches & checks")
                    group: "cache"
                    advancedOnly: true
                    query: page.query
                    showAdvanced: page.showAdvanced
                }
                SettingCard {
                    title: qsTr("Service addresses")
                    description: qsTr("Checked before they are saved: https only, no embedded credentials.")
                    group: "endpoints"
                    gate: "lyrics.enabled"
                    advancedOnly: true
                    query: page.query
                    showAdvanced: page.showAdvanced
                }
                SettingCard {
                    title: qsTr("YouTube client")
                    description: qsTr("These identities go stale when YouTube changes things. Edit them here instead of rebuilding.")
                    group: "client"
                    advancedOnly: true
                    query: page.query
                    showAdvanced: page.showAdvanced
                }
                SettingCard {
                    title: qsTr("About")
                    query: page.query
                    hasStaticContent: true
                    keywords: "about version license licence gpl credits dependencies"
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing
                        QQC2.Label {
                            text: qsTr("YT Music %1").arg(AppSettings.version)
                            font.weight: Font.DemiBold
                        }
                        QQC2.Label {
                            Layout.fillWidth: true
                            wrapMode: Text.Wrap
                            text: qsTr("A small native YouTube Music client. Free software under the GNU General Public License, version 3 only.")
                        }
                        QQC2.Label {
                            Layout.fillWidth: true
                            wrapMode: Text.Wrap
                            opacity: 0.7
                            font: Kirigami.Theme.smallFont
                            text: qsTr("Built with Qt 6 and KDE Kirigami, Rust (cxx-qt, tokio, reqwest with rustls, RustyPipe, zbus, rusqlite, toml_edit). Lyrics from LRCLIB and NetEase Cloud Music. Not affiliated with or endorsed by YouTube or Google.")
                        }
                    }
                }
                SettingCard {
                    title: qsTr("Diagnostics")
                    description: qsTr("Useful when reporting a problem. Cookies, tokens and signed links are removed before you copy it.")
                    query: page.query
                    hasStaticContent: true
                    keywords: "diagnostics report version engine platform debug bug"
                    Diagnostics {}
                }
            }
        }
    }

    // Tab / arrow navigation moves focus; keep the focused control on screen.
    Connections {
        target: page.Window.window
        function onActiveFocusItemChanged() {
            const it = page.Window.window ? page.Window.window.activeFocusItem : null
            if (!it) return
            let p = it
            while (p && p !== column) p = p.parent
            if (!p) return
            const y = it.mapToItem(flick.contentItem, 0, 0).y
            const margin = Kirigami.Units.gridUnit * 2
            let target = flick.contentY
            if (y < flick.contentY + margin) target = y - margin
            else if (y + it.height > flick.contentY + flick.height - margin) target = y + it.height - flick.height + margin
            flick.contentY = Math.max(0, Math.min(target, flick.contentHeight - flick.height))
        }
    }

    Kirigami.PromptDialog {
        id: resetDialog
        title: qsTr("Reset all settings?")
        subtitle: qsTr("Every setting returns to its default. Your account, comments in the settings file and unknown keys are kept.")
        standardButtons: Kirigami.Dialog.Ok | Kirigami.Dialog.Cancel
        onAccepted: AppSettings.resetAll()
    }
}
