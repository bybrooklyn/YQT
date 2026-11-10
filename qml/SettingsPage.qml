// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import dev.bybrooklyn.yqt
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.ScrollablePage {
    id: page
    title: qsTr("Settings")
    property string message
    property bool failed: false
    property bool busy: false
    property int loginId: -1
    readonly property var browsers: { try { return JSON.parse(Backend.browsers()) } catch (e) { return [] } }
    property bool showAdvanced: false

    function formatBytes(b) {
        if (b === undefined || b === null || isNaN(b)) return ""
        const u = ["B", "kB", "MB", "GB", "TB"]
        let i = 0
        while (b >= 1000 && i < u.length - 1) { b /= 1000; ++i }
        return (i === 0 ? String(Math.round(b)) : b.toFixed(1)) + " " + u[i]
    }

    Kirigami.PromptDialog {
        id: resetDialog
        title: qsTr("Reset all settings?")
        subtitle: qsTr("Every setting goes back to its default. Your account and downloads are not touched.")
        standardButtons: Kirigami.Dialog.Ok | Kirigami.Dialog.Cancel
        onAccepted: AppSettings.resetAll()
    }

    // One setting as a standard control, built from what the backend says about it (label, help, type, range,
    // choices; see `AppSettings.describe`). Every change is validated and saved by Rust and applies at once.
    component SettingField: ColumnLayout {
        id: field
        required property string path
        readonly property var meta: { try { return JSON.parse(AppSettings.describe(path)) } catch (e) { return ({}) } }
        readonly property int rev: AppSettings.revision
        readonly property var value: rev < 0 ? undefined : AppSettings.get(path)
        property string error: ""
        readonly property string kind: meta.kind || ""
        // an optional switch that must be on before this setting does anything
        property string gate: ""
        readonly property bool gateOn: gate === "" || (rev >= 0 && AppSettings.get(gate) === true)

        visible: !!meta.path && !meta.hidden && (!meta.advanced || page.showAdvanced)
        spacing: Kirigami.Units.smallSpacing
        Kirigami.FormData.label: kind === "bool" ? "" : (meta.label || "") + ":"
        Kirigami.FormData.buddyFor: kind === "bool" ? sw : null

        function commit(v) { error = AppSettings.set(path, v) }

        QQC2.Switch {
            id: sw
            visible: field.kind === "bool"
            text: field.meta.label || ""
            enabled: field.gateOn
            checked: field.value === true
            onToggled: field.commit(checked)
        }
        QQC2.SpinBox {
            id: intBox
            visible: field.kind === "int"
            enabled: field.gateOn
            editable: true
            from: field.meta.min === null || field.meta.min === undefined ? 0 : field.meta.min
            to: field.meta.max === null || field.meta.max === undefined ? 1000000 : field.meta.max
            stepSize: Math.max(1, field.meta.step || 1)
            value: Number(field.value || 0)
            textFromValue: (v) => field.meta.unit ? v + " " + field.meta.unit : String(v)
            valueFromText: (t) => parseInt(t)
            onValueModified: field.commit(value)
        }
        // A decimal number: a SpinBox counts whole steps, so scale it.
        QQC2.SpinBox {
            id: floatBox
            readonly property real scale: 100
            visible: field.kind === "float"
            enabled: field.gateOn
            editable: true
            from: (field.meta.min === null || field.meta.min === undefined ? 0 : field.meta.min) * scale
            to: (field.meta.max === null || field.meta.max === undefined ? 100000 : field.meta.max) * scale
            stepSize: Math.max(1, (field.meta.step || 0.1) * scale)
            value: Math.round(Number(field.value || 0) * scale)
            textFromValue: (v) => (v / scale).toFixed(2).replace(/\.?0+$/, "") + (field.meta.unit ? " " + field.meta.unit : "")
            valueFromText: (t) => Math.round(parseFloat(t) * scale)
            onValueModified: field.commit(value / scale)
        }
        QQC2.ComboBox {
            visible: field.kind === "choice"
            enabled: field.gateOn
            model: field.meta.choices || []
            textRole: "label"
            valueRole: "value"
            // re-evaluated once the model is in (count), and whenever the setting changes
            currentIndex: count >= 0 ? indexOfValue(field.value) : -1
            onActivated: field.commit(currentValue)
        }
        // A keyboard shortcut is captured by pressing it (KDE's key field). Without that module the text field
        // below takes the shortcut typed out, as before.
        Loader {
            id: keyField
            active: field.kind === "shortcut"
            visible: status === Loader.Ready
            source: active ? Qt.resolvedUrl("ShortcutField.qml") : ""
            onLoaded: {
                item.value = Qt.binding(() => field.value === undefined || field.value === null ? "" : String(field.value))
                item.enabled = Qt.binding(() => field.gateOn)
            }
            Connections {
                target: keyField.item
                ignoreUnknownSignals: true
                function onCommitted(value) { field.commit(value) }
            }
        }
        QQC2.TextField {
            visible: field.kind === "text" || field.kind === "url" || field.kind === "dir"
                     || (field.kind === "shortcut" && keyField.status !== Loader.Ready)
            enabled: field.gateOn
            Layout.preferredWidth: Kirigami.Units.gridUnit * 20
            placeholderText: field.kind === "dir" ? qsTr("Default location") : ""
            text: field.value === undefined || field.value === null ? "" : String(field.value)
            onEditingFinished: if (text !== String(field.value)) field.commit(text)
        }
        // A list of choices, each switched on or off (lyric providers).
        Repeater {
            model: field.kind === "textlist" ? (field.meta.choices || []) : []
            QQC2.CheckBox {
                required property var modelData
                text: modelData.label
                enabled: field.gateOn
                checked: (field.value || []).indexOf(modelData.value) >= 0
                onToggled: {
                    const cur = (field.value || []).filter(v => v !== modelData.value)
                    field.commit(checked ? cur.concat([modelData.value]) : cur)
                }
            }
        }

        QQC2.Label {
            Layout.maximumWidth: Kirigami.Units.gridUnit * 28
            visible: text.length > 0
            text: field.meta.description || ""
            wrapMode: Text.Wrap
            opacity: 0.7
            font: Kirigami.Theme.smallFont
        }
        QQC2.Label {
            Layout.maximumWidth: Kirigami.Units.gridUnit * 28
            visible: field.meta.restart === true && AppSettings.needsRestart(field.path)
            text: qsTr("Takes effect after restarting the app.")
            wrapMode: Text.Wrap
            color: Kirigami.Theme.neutralTextColor
            font: Kirigami.Theme.smallFont
        }
        QQC2.Label {
            Layout.maximumWidth: Kirigami.Units.gridUnit * 28
            visible: field.error.length > 0
            text: field.error
            wrapMode: Text.Wrap
            color: Kirigami.Theme.negativeTextColor
            font: Kirigami.Theme.smallFont
        }
    }

    // The ten-band equalizer: on/off, preset, preamp and one vertical slider per band (31 Hz to 16 kHz).
    component EqualizerField: ColumnLayout {
        id: eq
        readonly property var freqs: Equalizer.bandFrequencies()
        readonly property bool engineOn: AppConfig.audioEngine === "rust"
        spacing: Kirigami.Units.smallSpacing

        QQC2.Label {
            Layout.maximumWidth: Kirigami.Units.gridUnit * 28
            visible: !eq.engineOn
            wrapMode: Text.Wrap
            color: Kirigami.Theme.neutralTextColor
            text: qsTr("The equalizer works with the built-in audio engine. Choose it under “Audio engine” above (then restart).")
        }
        RowLayout {
            QQC2.Switch { text: qsTr("Equalizer"); checked: Equalizer.enabled; onToggled: { Equalizer.setEnabled(checked); Equalizer.persist() } }
            QQC2.ComboBox {
                id: presets
                model: Equalizer.presetNames()
                currentIndex: Math.max(0, find(Equalizer.preset))
                onActivated: { Equalizer.applyPreset(currentText); Equalizer.persist() }
            }
        }
        RowLayout {
            spacing: Kirigami.Units.smallSpacing
            Repeater {
                model: eq.freqs.length
                ColumnLayout {
                    required property int index
                    spacing: 0
                    QQC2.Label { Layout.alignment: Qt.AlignHCenter; text: (Equalizer.bandsDb[index] > 0 ? "+" : "") + Number(Equalizer.bandsDb[index]).toFixed(0); font: Kirigami.Theme.smallFont; opacity: 0.7 }
                    QQC2.Slider {
                        Layout.alignment: Qt.AlignHCenter
                        Layout.preferredHeight: Kirigami.Units.gridUnit * 8
                        orientation: Qt.Vertical
                        from: -12; to: 12; stepSize: 0.5
                        value: Equalizer.bandsDb[index]
                        onMoved: Equalizer.setBandDb(index, value)
                        onPressedChanged: if (!pressed) Equalizer.persist()
                    }
                    QQC2.Label {
                        Layout.alignment: Qt.AlignHCenter
                        text: eq.freqs[index] >= 1000 ? (eq.freqs[index] / 1000) + "k" : eq.freqs[index]
                        font: Kirigami.Theme.smallFont; opacity: 0.7
                    }
                }
            }
        }
        RowLayout {
            QQC2.Label { text: qsTr("Preamp") }
            QQC2.Slider {
                from: -12; to: 12; stepSize: 0.5
                value: Equalizer.preampDb
                onMoved: Equalizer.setPreampDb(value)
                onPressedChanged: if (!pressed) Equalizer.persist()
            }
            QQC2.Label { text: Number(Equalizer.preampDb).toFixed(1) + " dB" }
        }
    }

    Connections {
        target: Backend
        function onDone(id, err, json) {
            if (id !== page.loginId) return
            page.busy = false
            page.failed = !!err
            if (err) { page.message = err.split("\n\n")[0]; return }
            let who = ""
            try { who = JSON.parse(json).browser } catch (e) { who = "" }
            page.message = who ? qsTr("Signed in using %1.").arg(who) : qsTr("Signed in.")
            pasteArea.text = ""
            Backend.refreshLogin()
        }
    }

    ColumnLayout {
        Kirigami.InlineMessage {
            Layout.fillWidth: true
            visible: page.message.length > 0
            type: page.failed ? Kirigami.MessageType.Error : Kirigami.MessageType.Positive
            text: page.message
        }

        Kirigami.FormLayout {
            Layout.fillWidth: true

            // ---- account ----------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("YouTube Music account") }

            QQC2.Label {
                Kirigami.FormData.label: qsTr("Status:")
                text: Backend.loggedIn ? qsTr("Signed in") : qsTr("Not signed in")
            }

            ColumnLayout {
                visible: !Backend.loggedIn
                spacing: Kirigami.Units.smallSpacing
                QQC2.Label {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                    wrapMode: Text.Wrap
                    text: qsTr("Sign in to music.youtube.com in your browser as usual, then import that session here. Your Library, Liked music and History need it.")
                }
                QQC2.Button {
                    text: qsTr("Open YouTube Music")
                    icon.name: "internet-services"
                    onClicked: Qt.openUrlExternally("https://music.youtube.com")
                }
                Flow {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Repeater {
                        model: page.browsers
                        QQC2.Button {
                            required property var modelData
                            text: qsTr("Import from %1").arg(modelData.name)
                            icon.name: "user-identity"
                            highlighted: modelData.isDefault
                            enabled: !page.busy
                            onClicked: { page.message = ""; page.busy = true; page.loginId = Backend.loginImport(modelData.id) }
                        }
                    }
                    QQC2.BusyIndicator { running: page.busy; visible: running; implicitWidth: Kirigami.Units.iconSizes.medium; implicitHeight: implicitWidth }
                }
                Repeater {
                    model: page.browsers.filter(b => b.note)
                    QQC2.Label {
                        required property var modelData
                        Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                        wrapMode: Text.Wrap
                        opacity: 0.7
                        font: Kirigami.Theme.smallFont
                        text: modelData.name + ": " + modelData.note
                    }
                }
                QQC2.Label {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                    wrapMode: Text.Wrap
                    opacity: 0.7
                    font: Kirigami.Theme.smallFont
                    text: page.browsers.length > 0
                        ? qsTr("Works with Chrome, Chromium, Brave, Vivaldi, Edge, Opera, Firefox, Zen, LibreWolf and Waterfox. Your system keyring (KWallet, Keychain) may ask permission to share a Chromium browser’s encryption key.")
                        : qsTr("No supported browser found. Paste your cookies below instead.")
                }
            }

            ColumnLayout {
                visible: !Backend.loggedIn
                Kirigami.FormData.label: qsTr("Paste cookies:")
                spacing: Kirigami.Units.smallSpacing
                QQC2.TextArea {
                    id: pasteArea
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 28
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 5
                    wrapMode: TextEdit.WrapAnywhere
                    placeholderText: qsTr("A cookies.txt file’s contents, or the “Cookie:” request header from DevTools → Network on music.youtube.com")
                }
                QQC2.Button {
                    text: qsTr("Sign in with pasted cookies")
                    enabled: pasteArea.text.trim().length > 0 && !page.busy
                    onClicked: { page.message = ""; page.busy = true; page.loginId = Backend.loginCookie(pasteArea.text) }
                }
            }

            ColumnLayout {
                visible: Backend.loggedIn
                spacing: Kirigami.Units.smallSpacing
                QQC2.Button {
                    text: qsTr("Sign out")
                    icon.name: "system-log-out"
                    onClicked: { page.message = ""; Backend.logout() }
                }
                QQC2.Label {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                    wrapMode: Text.Wrap
                    opacity: 0.7
                    font: Kirigami.Theme.smallFont
                    text: qsTr("If the library ever says your session expired, sign in to music.youtube.com in your browser again and re-import. YouTube rotates cookies in the browser that’s using them.")
                }
            }

            // ---- playback ---------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("Playback") }

            Repeater { model: AppSettings.paths("audio"); delegate: SettingField { required property string modelData; path: modelData } }

            ColumnLayout {
                Kirigami.FormData.label: qsTr("Stream source:")
                spacing: Kirigami.Units.smallSpacing
                QQC2.Label {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                    wrapMode: Text.Wrap
                    text: Player.streamStatus || qsTr("Not checked yet")
                }
                QQC2.Button {
                    text: qsTr("Check streams")
                    icon.name: "view-refresh"
                    onClicked: { Player.streamStatus = qsTr("Checking…"); Player.testStream() }
                }
            }

            // ---- equalizer --------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("Equalizer") }
            EqualizerField {}

            // ---- downloads --------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("Downloads") }
            ColumnLayout {
                Kirigami.FormData.label: qsTr("Storage:")
                spacing: Kirigami.Units.smallSpacing
                QQC2.ProgressBar {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 20
                    from: 0
                    to: Math.max(1, Downloads.capBytes)
                    value: Math.min(Downloads.usedBytes, Downloads.capBytes)
                }
                QQC2.Label {
                    font: Kirigami.Theme.smallFont
                    opacity: 0.7
                    text: qsTr("%1 used of %2").arg(page.formatBytes(Downloads.usedBytes)).arg(page.formatBytes(Downloads.capBytes))
                }
                QQC2.Button {
                    text: qsTr("Show downloads")
                    icon.name: "folder-download"
                    onClicked: applicationWindow().go("downloads")
                }
            }
            Repeater { model: AppSettings.paths("downloads"); delegate: SettingField { required property string modelData; path: modelData } }
            Repeater { model: AppSettings.paths("smart"); delegate: SettingField {
                required property string modelData
                path: modelData
                // only the "what to keep" rules need smart downloads switched on; the pause rules apply to every download
                gate: ["downloads.keep_liked", "downloads.recent_n", "downloads.queue_ahead_n", "downloads.remove_unplayed_days"].indexOf(modelData) >= 0 ? "downloads.smart_enabled" : ""
            } }

            // ---- lyrics -----------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("Lyrics") }
            Repeater { model: AppSettings.paths("lyrics"); delegate: SettingField { required property string modelData; path: modelData } }

            // ---- appearance and window --------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("Appearance") }
            Repeater { model: AppSettings.paths("look"); delegate: SettingField { required property string modelData; path: modelData } }
            Repeater { model: AppSettings.paths("window"); delegate: SettingField { required property string modelData; path: modelData } }

            // ---- shortcuts --------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("Keyboard shortcuts") }
            Repeater { model: AppSettings.paths("keys"); delegate: SettingField { required property string modelData; path: modelData } }

            // ---- privacy ----------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("Privacy") }
            Repeater { model: AppSettings.paths("reporting"); delegate: SettingField { required property string modelData; path: modelData } }

            // ---- advanced ---------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("Advanced") }
            QQC2.CheckBox { text: qsTr("Show advanced settings"); checked: page.showAdvanced; onToggled: page.showAdvanced = checked }
            Repeater { model: page.showAdvanced ? AppSettings.paths("endpoints") : []; delegate: SettingField { required property string modelData; path: modelData } }
            RowLayout {
                QQC2.Button { text: qsTr("Open config folder"); icon.name: "folder-open"; onClicked: Qt.openUrlExternally(AppSettings.configFolderUrl()) }
                QQC2.Button { text: qsTr("Reset all settings"); icon.name: "edit-reset"; onClicked: resetDialog.open() }
            }
            QQC2.Label {
                Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                wrapMode: Text.Wrap
                opacity: 0.7
                font: Kirigami.Theme.smallFont
                text: qsTr("Settings are saved to %1 and apply right away.").arg(AppSettings.configPath)
            }

            // ---- about ------------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("About") }
            QQC2.Label {
                Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                wrapMode: Text.Wrap
                text: qsTr("Free software under the GNU GPL v3. Lyrics from LRCLIB, NetEase and YouTube Music.")
            }
            QQC2.Button { text: qsTr("About YQT"); icon.name: "help-about"; onClicked: applicationWindow().showAbout() }
        }
    }
}
