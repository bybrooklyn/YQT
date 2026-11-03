// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// Plain-language list of what the app sends where, generated in Rust from the live configuration.
ColumnLayout {
    id: list
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active
    Layout.fillWidth: true
    spacing: Kirigami.Units.largeSpacing

    readonly property var flows: {
        if (AppSettings.revision < 0) return []
        try { return JSON.parse(AppSettings.privacyFlows(Backend.loggedIn)) } catch (e) { return [] }
    }

    Repeater {
        model: list.flows
        RowLayout {
            required property var modelData
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            opacity: modelData.active ? 1 : 0.7
            Rectangle {
                Layout.alignment: Qt.AlignTop
                Layout.topMargin: Kirigami.Units.smallSpacing
                implicitWidth: Kirigami.Units.smallSpacing * 2
                implicitHeight: implicitWidth
                radius: width / 2
                color: modelData.active ? Kirigami.Theme.neutralTextColor : Kirigami.Theme.disabledTextColor
            }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 1
                Accessible.name: modelData.title + (modelData.active ? "" : qsTr(" (not happening)"))
                QQC2.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    font.weight: Font.DemiBold
                    text: modelData.title
                }
                QQC2.Label {
                    visible: !modelData.active && !!modelData.whyNot
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    text: modelData.whyNot || ""
                    color: Kirigami.Theme.positiveTextColor
                    font: Kirigami.Theme.smallFont
                }
                QQC2.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    text: qsTr("Sent to %1").arg(modelData.to)
                    font: Kirigami.Theme.smallFont
                    opacity: 0.8
                }
                QQC2.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    text: modelData.what
                    font: Kirigami.Theme.smallFont
                    opacity: 0.7
                }
            }
        }
    }
    QQC2.Label {
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        font: Kirigami.Theme.smallFont
        opacity: 0.7
        text: qsTr("Nothing else leaves your computer: there is no analytics, telemetry or crash upload.")
    }
}
