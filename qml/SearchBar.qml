// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami
import org.kde.ytm

// The search field of a page, with its suggestion popup. `controller` is the page's PageController (it fetches the
// suggestions); `query` is the text to start with. Accepting opens the search page.
Kirigami.SearchField {
    id: field

    property var controller
    property string query

    placeholderText: qsTr("Search songs, albums, artists…")
    autoAccept: false
    Component.onCompleted: text = query
    onAccepted: {
        suggestPopup.close()
        if (text.trim() !== query) applicationWindow().search(text)
    }
    onTextEdited: suggestTimer.restart()
    onActiveFocusChanged: if (!activeFocus) suggestPopup.close()

    Timer {
        id: suggestTimer
        interval: AppConfig.suggestDebounceMs
        onTriggered: if (field.controller) field.controller.suggest(field.text)
    }
    Connections {
        target: field.controller
        function onSuggestionsChanged() {
            if (field.controller.suggestions.length > 0 && field.activeFocus) suggestPopup.open(); else suggestPopup.close()
        }
    }

    QQC2.Popup {
        id: suggestPopup
        y: field.height
        width: field.width
        padding: 0
        focus: false
        closePolicy: QQC2.Popup.CloseOnEscape
        contentItem: ListView {
            implicitHeight: Math.min(contentHeight, Kirigami.Units.gridUnit * 14)
            model: field.controller ? field.controller.suggestions : []
            clip: true
            delegate: QQC2.ItemDelegate {
                required property string modelData
                width: ListView.view.width
                text: modelData
                icon.name: "search"
                onClicked: { suggestPopup.close(); field.text = modelData; applicationWindow().search(modelData) }
            }
        }
    }
}
