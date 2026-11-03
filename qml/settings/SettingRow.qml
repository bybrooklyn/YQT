// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// One setting, generated from the Rust metadata (`AppSettings.describe(path)`): label, help, editor for its type,
// "changed" dot with reset button, restart hint, warning and inline validation message.
Item {
    id: row
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    required property string path
    property string query: ""
    property bool showAdvanced: false
    property bool forceMatch: false
    // Path of a switch that must be on before this setting does anything.
    property string gate: ""

    readonly property var meta: { try { return JSON.parse(AppSettings.describe(path)) } catch (e) { return {} } }
    property int bump: 0
    property string error: ""
    // `revision` changes after every save; reading it in a real expression (not a bare statement, which the QML
    // compiler may drop) keeps these bindings reactive.
    readonly property int rev: AppSettings.revision + bump
    readonly property var value: rev < 0 ? undefined : AppSettings.get(path)
    readonly property bool modified: rev < 0 ? false : !AppSettings.isDefault(path)
    readonly property bool restartPending: rev < 0 ? false : AppSettings.needsRestart(path)
    readonly property bool gateOn: rev < 0 ? true : (gate === "" || gate === path || AppSettings.get(gate) === true)
    readonly property string gateLabel: gate === "" || gate === path ? "" : (JSON.parse(AppSettings.describe(gate)).label || "")
    readonly property string kind: meta.kind || ""
    readonly property bool wide: width > Kirigami.Units.gridUnit * 36
    readonly property bool fullWidthEditor: kind === "text" || kind === "url" || kind === "dir" || kind === "shortcut" || kind === "textlist"

    readonly property bool matched: {
        if (!meta.path || meta.hidden) return false
        const q = query.trim().toLowerCase()
        if (q === "") return !meta.advanced || showAdvanced
        if (forceMatch) return true
        return (meta.label + " " + meta.description + " " + path).toLowerCase().indexOf(q) >= 0
    }

    function commit(v) {
        error = AppSettings.set(path, v)
        bump++
    }
    function doReset() {
        error = AppSettings.reset(path)
        bump++
    }
    function fmt(v) {
        const n = Number(v)
        return kind === "int" ? String(Math.round(n)) : String(+n.toFixed(3))
    }

    visible: matched
    Layout.fillWidth: true
    implicitHeight: grid.implicitHeight

    GridLayout {
        id: grid
        width: parent.width
        columns: row.wide ? 2 : 1
        columnSpacing: Kirigami.Units.gridUnit
        rowSpacing: Kirigami.Units.smallSpacing

        ColumnLayout {
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignTop
            Layout.preferredWidth: row.wide ? row.width * 0.45 : -1
            spacing: 2

            RowLayout {
                Layout.fillWidth: true
                spacing: Kirigami.Units.smallSpacing
                Rectangle {
                    visible: row.modified
                    implicitWidth: Kirigami.Units.smallSpacing * 1.5
                    implicitHeight: implicitWidth
                    radius: width / 2
                    color: Kirigami.Theme.highlightColor
                    Accessible.name: qsTr("Changed from the default")
                    HoverHandler { id: dotHover }
                    QQC2.ToolTip.visible: dotHover.hovered
                    QQC2.ToolTip.text: qsTr("Changed from the default")
                }
                QQC2.Label {
                    text: row.meta.label || row.path
                    font.weight: Font.DemiBold
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                    Accessible.role: Accessible.StaticText
                }
                QQC2.ToolButton {
                    visible: row.modified
                    icon.name: "edit-undo"
                    text: qsTr("Reset to default")
                    display: QQC2.AbstractButton.IconOnly
                    onClicked: row.doReset()
                    QQC2.ToolTip.visible: hovered
                    QQC2.ToolTip.text: qsTr("Reset to default")
                    Accessible.name: qsTr("Reset %1 to its default").arg(row.meta.label || row.path)
                }
            }
            QQC2.Label {
                text: row.meta.description || ""
                visible: text !== "" && row.kind !== "shortcut" // the card explains shortcuts once
                wrapMode: Text.Wrap
                opacity: 0.7
                font: Kirigami.Theme.smallFont
                Layout.fillWidth: true
            }
            QQC2.Label {
                visible: row.restartPending
                text: qsTr("Restart to apply")
                color: Kirigami.Theme.neutralTextColor
                font: Kirigami.Theme.smallFont
            }
            QQC2.Label {
                visible: !row.gateOn
                text: qsTr("Turn on “%1” to use this.").arg(row.gateLabel)
                color: Kirigami.Theme.neutralTextColor
                font: Kirigami.Theme.smallFont
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }
            Kirigami.InlineMessage {
                visible: !!row.meta.warn && row.modified
                type: Kirigami.MessageType.Warning
                text: row.meta.warn || ""
                Layout.fillWidth: true
            }
            QQC2.Label {
                visible: row.error !== ""
                text: row.error
                color: Kirigami.Theme.negativeTextColor
                wrapMode: Text.Wrap
                Layout.fillWidth: true
                Accessible.role: Accessible.AlertMessage
            }
        }

        Loader {
            id: editor
            enabled: row.gateOn
            opacity: row.gateOn ? 1 : 0.5
            Layout.alignment: row.wide ? (Qt.AlignRight | Qt.AlignTop) : (Qt.AlignLeft | Qt.AlignTop)
            Layout.fillWidth: !row.wide || row.fullWidthEditor || row.meta.slider === true
            Layout.preferredWidth: row.wide ? row.width * 0.5 : -1
            sourceComponent: {
                switch (row.kind) {
                case "bool": return boolEditor
                case "int":
                case "float": return numberEditor
                case "choice": return choiceEditor
                case "text":
                case "url": return textEditor
                case "dir": return dirEditor
                case "shortcut": return shortcutEditor
                case "textlist": return providerEditor
                }
                return null
            }
        }
    }

    Component {
        id: boolEditor
        QQC2.Switch {
            checked: row.value === true
            Accessible.name: row.meta.label || ""
            onToggled: { row.commit(checked); checked = Qt.binding(() => row.value === true) }
        }
    }

    Component {
        id: numberEditor
        RowLayout {
            spacing: Kirigami.Units.smallSpacing
            QQC2.Slider {
                visible: row.meta.slider === true
                Layout.fillWidth: true
                from: row.meta.min === null ? 0 : row.meta.min
                to: row.meta.softMax === null ? 100 : row.meta.softMax
                stepSize: row.meta.step || 1
                value: Math.min(Number(row.value), to)
                Accessible.name: row.meta.label || ""
                onPressedChanged: if (!pressed) row.commit(value)
                onMoved: if (!pressed) row.commit(value)
            }
            QQC2.TextField {
                id: num
                Layout.preferredWidth: Kirigami.Units.gridUnit * 6
                Layout.fillWidth: row.meta.slider !== true
                horizontalAlignment: Text.AlignRight
                text: row.fmt(row.value)
                inputMethodHints: Qt.ImhFormattedNumbersOnly
                validator: DoubleValidator {
                    bottom: row.meta.min === null ? -1e12 : row.meta.min
                    top: row.meta.max === null ? 1e12 : row.meta.max
                    decimals: row.kind === "int" ? 0 : 3
                    notation: DoubleValidator.StandardNotation
                }
                Accessible.name: row.meta.label || ""
                onEditingFinished: {
                    const n = Number(text.replace(",", "."))
                    if (isFinite(n) && text.trim() !== "") row.commit(n)
                    else { row.bump++; row.error = qsTr("Enter a number.") }
                }
                Keys.onUpPressed: row.commit(Number(row.value) + (row.meta.step || 1))
                Keys.onDownPressed: row.commit(Number(row.value) - (row.meta.step || 1))
            }
            QQC2.Label {
                visible: !!row.meta.unit
                text: row.meta.unit || ""
                opacity: 0.7
            }
        }
    }

    Component {
        id: choiceEditor
        QQC2.ComboBox {
            readonly property var items: row.meta.choices || []
            model: items
            textRole: "label"
            valueRole: "value"
            currentIndex: Math.max(0, items.findIndex(c => c.value === row.value))
            Accessible.name: row.meta.label || ""
            onActivated: {
                row.commit(currentValue)
                currentIndex = Qt.binding(() => Math.max(0, items.findIndex(c => c.value === row.value)))
            }
        }
    }

    Component {
        id: textEditor
        QQC2.TextField {
            text: String(row.value === undefined ? "" : row.value)
            placeholderText: row.meta.default === "" ? qsTr("Default") : ""
            Accessible.name: row.meta.label || ""
            onEditingFinished: if (text !== String(row.value)) row.commit(text)
        }
    }

    Component {
        id: dirEditor
        RowLayout {
            spacing: Kirigami.Units.smallSpacing
            QQC2.TextField {
                Layout.fillWidth: true
                text: String(row.value === undefined ? "" : row.value)
                placeholderText: qsTr("Default location")
                Accessible.name: row.meta.label || ""
                onEditingFinished: if (text !== String(row.value)) row.commit(text)
            }
            QQC2.Button {
                text: qsTr("Browse…")
                icon.name: "document-open-folder"
                visible: picker.status === Loader.Ready
                onClicked: picker.item.open()
            }
            Loader {
                id: picker
                source: "FolderPicker.qml"
                onLoaded: item.picked.connect(p => row.commit(p))
                Binding { target: picker.item; property: "current"; value: String(row.value); when: picker.status === Loader.Ready }
            }
        }
    }

    Component {
        id: shortcutEditor
        ShortcutEditor {
            value: String(row.value === undefined ? "" : row.value)
            onCommit: sequence => row.commit(sequence)
        }
    }

    Component {
        id: providerEditor
        ProviderOrder {
            choices: row.meta.choices || []
            enabledList: row.value || []
            onCommit: list => row.commit(list)
        }
    }
}
