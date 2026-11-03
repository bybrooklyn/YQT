// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// Account status, browser import, pasted cookies and sign-out.
ColumnLayout {
    id: acct
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active
    Layout.fillWidth: true
    spacing: Kirigami.Units.smallSpacing

    property string message
    property bool failed: false
    property bool busy: false
    property int loginId: -1
    readonly property var browsers: { try { return JSON.parse(Backend.browsers()) } catch (e) { return [] } }

    Connections {
        target: Backend
        function onDone(id, err, json) {
            if (id !== acct.loginId) return
            acct.busy = false
            acct.failed = !!err
            if (err) { acct.message = err.split("\n\n")[0]; return }
            let who = ""
            try { who = JSON.parse(json).browser } catch (e) { who = "" }
            acct.message = who ? qsTr("Signed in using %1.").arg(who) : qsTr("Signed in.")
            pasteArea.text = ""
            Backend.refreshLogin()
        }
    }

    RowLayout {
        spacing: Kirigami.Units.smallSpacing
        Rectangle {
            implicitWidth: Kirigami.Units.smallSpacing * 2
            implicitHeight: implicitWidth
            radius: width / 2
            color: Backend.loggedIn ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.disabledTextColor
        }
        QQC2.Label {
            text: Backend.loggedIn ? qsTr("Signed in") : qsTr("Not signed in")
            font.weight: Font.DemiBold
        }
    }

    Kirigami.InlineMessage {
        Layout.fillWidth: true
        visible: acct.message.length > 0
        type: acct.failed ? Kirigami.MessageType.Error : Kirigami.MessageType.Positive
        text: acct.message
    }

    ColumnLayout {
        visible: !Backend.loggedIn
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing
        QQC2.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            text: qsTr("Sign in to music.youtube.com in your browser as usual, then import that session here. Your Library, Liked music and History need it.")
        }
        QQC2.Button {
            text: qsTr("Open YouTube Music")
            icon.name: "internet-services"
            onClicked: Qt.openUrlExternally("https://music.youtube.com")
        }
        Flow {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing
            Repeater {
                model: acct.browsers
                QQC2.Button {
                    required property var modelData
                    text: qsTr("Import from %1").arg(modelData.name)
                    icon.name: "user-identity"
                    highlighted: modelData.isDefault
                    enabled: !acct.busy
                    onClicked: { acct.message = ""; acct.busy = true; acct.loginId = Backend.loginImport(modelData.id) }
                }
            }
            QQC2.BusyIndicator { running: acct.busy; visible: running; implicitWidth: Kirigami.Units.iconSizes.medium; implicitHeight: implicitWidth }
        }
        Repeater {
            model: acct.browsers.filter(b => b.note)
            QQC2.Label {
                required property var modelData
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                opacity: 0.7
                font: Kirigami.Theme.smallFont
                text: modelData.name + ": " + modelData.note
            }
        }
        QQC2.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            opacity: 0.7
            font: Kirigami.Theme.smallFont
            text: acct.browsers.length > 0
                ? qsTr("Works with Chrome, Chromium, Brave, Vivaldi, Edge, Opera, Firefox, Zen, LibreWolf and Waterfox. Your system keyring (KWallet, Keychain) may ask permission to share a Chromium browser’s encryption key.")
                : qsTr("No supported browser found. Paste your cookies below instead.")
        }

        Kirigami.Separator { Layout.fillWidth: true; Layout.topMargin: Kirigami.Units.smallSpacing }
        QQC2.Label { text: qsTr("Paste cookies instead"); font.weight: Font.DemiBold }
        QQC2.TextArea {
            id: pasteArea
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 5
            wrapMode: TextEdit.WrapAnywhere
            placeholderText: qsTr("A cookies.txt file’s contents, or the “Cookie:” request header from DevTools → Network on music.youtube.com")
            Accessible.name: qsTr("Pasted cookies")
        }
        QQC2.Button {
            text: qsTr("Sign in with pasted cookies")
            icon.name: "dialog-ok"
            enabled: pasteArea.text.trim().length > 0 && !acct.busy
            onClicked: { acct.message = ""; acct.busy = true; acct.loginId = Backend.loginCookie(pasteArea.text) }
        }
    }

    ColumnLayout {
        visible: Backend.loggedIn
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing
        QQC2.Button {
            text: qsTr("Sign out")
            icon.name: "system-log-out"
            onClicked: { acct.message = ""; Backend.logout() }
        }
        QQC2.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            opacity: 0.7
            font: Kirigami.Theme.smallFont
            text: qsTr("If the library ever says your session expired, sign in to music.youtube.com in your browser again and re-import. YouTube rotates cookies in the browser that’s using them.")
        }
    }
}
