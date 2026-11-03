// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// Versions, engine and platform in one block that can be copied into a bug report (secrets are removed in Rust).
ColumnLayout {
    id: diag
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active
    Layout.fillWidth: true
    spacing: Kirigami.Units.smallSpacing

    property bool copied: false
    readonly property string report: {
        if (AppSettings.revision < 0) return ""
        return AppSettings.redact(AppSettings.diagnostics(Backend.loggedIn) + "\nStream source: " + (Player.streamStatus || "not checked"))
    }

    QQC2.TextArea {
        id: area
        Layout.fillWidth: true
        readOnly: true
        text: diag.report
        wrapMode: TextEdit.Wrap
        font.family: "monospace"
        font.pointSize: Math.max(8, Qt.application.font.pointSize * 0.9)
        Accessible.name: qsTr("Diagnostic report")
    }
    RowLayout {
        spacing: Kirigami.Units.smallSpacing
        QQC2.Button {
            text: qsTr("Copy report")
            icon.name: "edit-copy"
            onClicked: { area.selectAll(); area.copy(); area.deselect(); diag.copied = true; hide.restart() }
        }
        QQC2.Label {
            visible: diag.copied
            text: qsTr("Copied. Cookies, tokens and signed links were removed.")
            opacity: 0.7
        }
        Timer { id: hide; interval: 4000; onTriggered: diag.copied = false }
    }
}
