// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// The search field, shared by every page except the player (Main.qml's window header).
// Shows YouTube's suggestions while typing and your recent searches while the field is empty.
Item {
    id: bar
    implicitHeight: searchField.implicitHeight + Kirigami.Units.smallSpacing * 2
    implicitWidth: searchField.implicitWidth

    property string query: ""     // the search shown on the current page, if any
    property var suggestions: []
    property int suggestId: -1

    function focusSearch() { searchField.forceActiveFocus(); searchField.selectAll() }
    onQueryChanged: searchField.text = query

    Connections {
        target: Backend
        function onDone(id, err, json) {
            if (id !== bar.suggestId || err) return
            try { bar.suggestions = JSON.parse(json) } catch (e) { return }
            suggestPopup.visible = bar.suggestions.length > 0 && searchField.activeFocus
        }
    }

    Kirigami.SearchField {
        id: searchField
        anchors.fill: parent
        anchors.margins: Kirigami.Units.smallSpacing
        placeholderText: qsTr("Search songs, albums, artists…")
        autoAccept: false
        Component.onCompleted: text = bar.query
        onAccepted: {
            suggestPopup.close()
            if (text.trim() !== bar.query) applicationWindow().search(text)
        }
        onTextEdited: suggestTimer.restart()
        onActiveFocusChanged: {
            if (!activeFocus) suggestPopup.close()
            else if (text.trim().length === 0 && applicationWindow().recentSearches.length > 0) suggestPopup.open()
        }

        Timer {
            id: suggestTimer
            interval: AppConfig.suggestDebounceMs
            onTriggered: {
                const t = searchField.text.trim()
                if (t.length > 1) bar.suggestId = Backend.suggest(t)
                else if (t.length === 0 && applicationWindow().recentSearches.length > 0) suggestPopup.open()
                else suggestPopup.close()
            }
        }

        QQC2.Popup {
            id: suggestPopup
            y: searchField.height
            width: searchField.width
            padding: 0
            focus: false
            closePolicy: QQC2.Popup.CloseOnEscape
            contentItem: ListView {
                implicitHeight: Math.min(contentHeight, Kirigami.Units.gridUnit * 14)
                readonly property bool recents: searchField.text.trim().length === 0
                model: recents ? applicationWindow().recentSearches : bar.suggestions
                clip: true
                footer: QQC2.ItemDelegate {
                    visible: ListView.view.recents
                    width: ListView.view.width
                    height: visible ? implicitHeight : 0
                    text: qsTr("Clear recent searches")
                    icon.name: "edit-clear-history"
                    onClicked: { suggestPopup.close(); applicationWindow().clearRecentSearches() }
                }
                delegate: QQC2.ItemDelegate {
                    required property string modelData
                    width: ListView.view.width
                    text: modelData
                    icon.name: ListView.view.recents ? "document-open-recent" : "search"
                    onClicked: { suggestPopup.close(); searchField.text = modelData; applicationWindow().search(modelData) }
                }
            }
        }
    }
}
