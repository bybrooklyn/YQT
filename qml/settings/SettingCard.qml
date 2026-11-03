// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// A titled group of settings. With `group` set it lists that group's rows (from the Rust metadata table);
// anything declared inside is added below them. `matched` tells the section whether the card has anything to show.
Rectangle {
    id: card
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    property string title
    property string description
    property string group
    property string query
    property bool showAdvanced: false
    // Static content (anything not generated from `group`) is searchable through these words.
    property bool hasStaticContent: false
    property string keywords
    // path -> path of the switch that must be on for it to be usable
    property var gates: ({})
    property string gate
    default property alias content: extra.data

    // Only shown when "Show advanced" is on or a search is active.
    property bool advancedOnly: false
    // The search matches the card itself (its title, help or keywords): then all its rows are shown.
    readonly property bool selfMatch: {
        const q = query.trim().toLowerCase()
        return q !== "" && (title + " " + description + " " + keywords).toLowerCase().indexOf(q) >= 0
    }

    readonly property bool matched: {
        if (advancedOnly && !showAdvanced && query.trim() === "") return false
        const kids = col.children
        for (let i = 0; i < kids.length; ++i)
            if (kids[i].matched === true) return true
        if (!hasStaticContent) return false
        return query.trim() === "" || selfMatch
    }

    visible: matched
    Layout.fillWidth: true
    implicitHeight: col.implicitHeight + 2 * Kirigami.Units.largeSpacing
    radius: Kirigami.Units.smallSpacing * 2
    color: Qt.tint(Kirigami.Theme.backgroundColor, Qt.alpha(Kirigami.Theme.textColor, 0.045))
    border.width: 1
    border.color: Qt.alpha(Kirigami.Theme.textColor, 0.14)

    ColumnLayout {
        id: col
        anchors {
            left: parent.left
            right: parent.right
            top: parent.top
            margins: Kirigami.Units.largeSpacing
        }
        spacing: Kirigami.Units.largeSpacing

        ColumnLayout {
            visible: card.title !== ""
            spacing: 2
            Layout.fillWidth: true
            Kirigami.Heading {
                level: 3
                text: card.title
                Layout.fillWidth: true
                wrapMode: Text.Wrap
            }
            QQC2.Label {
                visible: card.description !== ""
                text: card.description
                wrapMode: Text.Wrap
                opacity: 0.7
                Layout.fillWidth: true
            }
        }

        Repeater {
            model: card.group !== "" ? AppSettings.paths(card.group) : []
            SettingRow {
                required property string modelData
                path: modelData
                query: card.query
                showAdvanced: card.showAdvanced
                forceMatch: card.selfMatch
                gate: card.gates[modelData] !== undefined ? card.gates[modelData] : card.gate
            }
        }

        ColumnLayout {
            id: extra
            Layout.fillWidth: true
            spacing: Kirigami.Units.largeSpacing
        }
    }
}
