// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// Edits one key sequence: type it (Qt syntax) or press "Record" and hit the keys.
RowLayout {
    id: ed
    property string value
    property bool recording: false
    signal commit(string sequence)
    spacing: Kirigami.Units.smallSpacing

    QQC2.TextField {
        id: field
        Layout.fillWidth: true
        Layout.minimumWidth: Kirigami.Units.gridUnit * 6
        text: ed.recording ? qsTr("Press the keys…") : ed.value
        readOnly: ed.recording
        placeholderText: qsTr("Unbound")
        Accessible.name: qsTr("Shortcut")
        onEditingFinished: if (!ed.recording && text !== ed.value) ed.commit(text)
        onActiveFocusChanged: if (!activeFocus) ed.recording = false
        // Without this the app's own shortcuts would fire instead of being recorded.
        Keys.onShortcutOverride: event => { if (ed.recording) event.accepted = true }
        Keys.onPressed: event => {
            if (!ed.recording) return
            event.accepted = true
            if (event.key === Qt.Key_Escape && event.modifiers === Qt.NoModifier) {
                ed.recording = false
                return
            }
            const s = AppSettings.keyName(event.key, event.modifiers)
            if (s === "") return // only a modifier so far
            ed.recording = false
            ed.commit(s)
        }
    }
    QQC2.Button {
        text: ed.recording ? qsTr("Cancel") : qsTr("Record")
        icon.name: ed.recording ? "dialog-cancel" : "input-keyboard"
        onClicked: {
            ed.recording = !ed.recording
            if (ed.recording) field.forceActiveFocus()
        }
    }
    QQC2.ToolButton {
        icon.name: "edit-clear"
        text: qsTr("Unbind")
        display: QQC2.AbstractButton.IconOnly
        enabled: ed.value !== ""
        onClicked: ed.commit("")
        QQC2.ToolTip.visible: hovered
        QQC2.ToolTip.text: text
    }
}
