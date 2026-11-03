// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Dialogs

// Native folder chooser. Kept in its own file so the page still works where QtQuick.Dialogs is unavailable
// (the Loader that creates this simply fails and the text field stays usable).
Item {
    id: picker
    property string current
    signal picked(string path)
    function open() { dialog.open() }

    FolderDialog {
        id: dialog
        title: qsTr("Choose a folder")
        currentFolder: picker.current !== "" ? "file://" + picker.current : ""
        onAccepted: picker.picked(decodeURIComponent(selectedFolder.toString().replace(/^file:\/\/(\/(?=[A-Za-z]:))?/, "")))
    }
}
