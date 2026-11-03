// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// Turn providers on or off and put them in order of preference.
ColumnLayout {
    id: po
    property var choices: []      // [{value, label}]
    property var enabledList: []  // values, in order
    signal commit(var list)
    spacing: 0

    // Enabled providers first (in their order), then the rest.
    readonly property var ordered: {
        const on = enabledList.map(v => choices.find(c => c.value === v)).filter(c => !!c)
        const off = choices.filter(c => enabledList.indexOf(c.value) < 0)
        return on.concat(off)
    }

    function toggle(value, on) {
        const l = enabledList.filter(v => v !== value)
        if (on) l.push(value)
        po.commit(l)
    }
    function move(value, delta) {
        const l = enabledList.slice()
        const i = l.indexOf(value)
        const j = i + delta
        if (i < 0 || j < 0 || j >= l.length) return
        l.splice(i, 1)
        l.splice(j, 0, value)
        po.commit(l)
    }

    Repeater {
        model: po.ordered
        RowLayout {
            id: line
            required property var modelData
            required property int index
            readonly property bool on: po.enabledList.indexOf(modelData.value) >= 0
            readonly property int rank: po.enabledList.indexOf(modelData.value)
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            QQC2.CheckBox {
                text: line.modelData.label
                checked: line.on
                Layout.fillWidth: true
                onToggled: { po.toggle(line.modelData.value, checked); checked = Qt.binding(() => line.on) }
            }
            QQC2.ToolButton {
                icon.name: "go-up"
                text: qsTr("Move up")
                display: QQC2.AbstractButton.IconOnly
                enabled: line.on && line.rank > 0
                onClicked: po.move(line.modelData.value, -1)
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.text: text
            }
            QQC2.ToolButton {
                icon.name: "go-down"
                text: qsTr("Move down")
                display: QQC2.AbstractButton.IconOnly
                enabled: line.on && line.rank < po.enabledList.length - 1
                onClicked: po.move(line.modelData.value, 1)
                QQC2.ToolTip.visible: hovered
                QQC2.ToolTip.text: text
            }
        }
    }
}
