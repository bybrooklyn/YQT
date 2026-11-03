// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.ytm
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.ScrollablePage {
    id: page
    title: qsTr("Settings")
    property string message
    property bool failed: false
    property bool busy: false
    property int loginId: -1
    readonly property var browsers: { try { return JSON.parse(Backend.browsers()) } catch (e) { return [] } }

    Connections {
        target: Backend
        function onDone(id, err, json) {
            if (id !== page.loginId) return
            page.busy = false
            page.failed = !!err
            if (err) { page.message = err.split("\n\n")[0]; return }
            let who = ""
            try { who = JSON.parse(json).browser } catch (e) { who = "" }
            page.message = who ? qsTr("Signed in using %1.").arg(who) : qsTr("Signed in.")
            pasteArea.text = ""
            Backend.refreshLogin()
        }
    }

    ColumnLayout {
        Kirigami.InlineMessage {
            Layout.fillWidth: true
            visible: page.message.length > 0
            type: page.failed ? Kirigami.MessageType.Error : Kirigami.MessageType.Positive
            text: page.message
        }

        Kirigami.FormLayout {
            Layout.fillWidth: true

            // ---- account ----------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("YouTube Music account") }

            QQC2.Label {
                Kirigami.FormData.label: qsTr("Status:")
                text: Backend.loggedIn ? qsTr("Signed in") : qsTr("Not signed in")
            }

            ColumnLayout {
                visible: !Backend.loggedIn
                spacing: Kirigami.Units.smallSpacing
                QQC2.Label {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                    wrapMode: Text.Wrap
                    text: qsTr("Sign in to music.youtube.com in your browser as usual, then import that session here. Your Library, Liked music and History need it.")
                }
                QQC2.Button {
                    text: qsTr("Open YouTube Music")
                    icon.name: "internet-services"
                    onClicked: Qt.openUrlExternally("https://music.youtube.com")
                }
                Flow {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Repeater {
                        model: page.browsers
                        QQC2.Button {
                            required property var modelData
                            text: qsTr("Import from %1").arg(modelData.name)
                            icon.name: "user-identity"
                            highlighted: modelData.isDefault
                            enabled: !page.busy
                            onClicked: { page.message = ""; page.busy = true; page.loginId = Backend.loginImport(modelData.id) }
                        }
                    }
                    QQC2.BusyIndicator { running: page.busy; visible: running; implicitWidth: Kirigami.Units.iconSizes.medium; implicitHeight: implicitWidth }
                }
                Repeater {
                    model: page.browsers.filter(b => b.note)
                    QQC2.Label {
                        required property var modelData
                        Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                        wrapMode: Text.Wrap
                        opacity: 0.7
                        font: Kirigami.Theme.smallFont
                        text: modelData.name + ": " + modelData.note
                    }
                }
                QQC2.Label {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                    wrapMode: Text.Wrap
                    opacity: 0.7
                    font: Kirigami.Theme.smallFont
                    text: page.browsers.length > 0
                        ? qsTr("Works with Chrome, Chromium, Brave, Vivaldi, Edge, Opera, Firefox, Zen, LibreWolf and Waterfox. Your system keyring (KWallet, Keychain) may ask permission to share a Chromium browser’s encryption key.")
                        : qsTr("No supported browser found. Paste your cookies below instead.")
                }
            }

            ColumnLayout {
                visible: !Backend.loggedIn
                Kirigami.FormData.label: qsTr("Paste cookies:")
                spacing: Kirigami.Units.smallSpacing
                QQC2.TextArea {
                    id: pasteArea
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 28
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 5
                    wrapMode: TextEdit.WrapAnywhere
                    placeholderText: qsTr("A cookies.txt file’s contents, or the “Cookie:” request header from DevTools → Network on music.youtube.com")
                }
                QQC2.Button {
                    text: qsTr("Sign in with pasted cookies")
                    enabled: pasteArea.text.trim().length > 0 && !page.busy
                    onClicked: { page.message = ""; page.busy = true; page.loginId = Backend.loginCookie(pasteArea.text) }
                }
            }

            ColumnLayout {
                visible: Backend.loggedIn
                spacing: Kirigami.Units.smallSpacing
                QQC2.Button {
                    text: qsTr("Sign out")
                    icon.name: "system-log-out"
                    onClicked: { page.message = ""; Backend.logout() }
                }
                QQC2.Label {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                    wrapMode: Text.Wrap
                    opacity: 0.7
                    font: Kirigami.Theme.smallFont
                    text: qsTr("If the library ever says your session expired, sign in to music.youtube.com in your browser again and re-import. YouTube rotates cookies in the browser that’s using them.")
                }
            }

            // ---- playback ---------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("Playback") }

            ColumnLayout {
                Kirigami.FormData.label: qsTr("Stream source:")
                spacing: Kirigami.Units.smallSpacing
                QQC2.Label {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                    wrapMode: Text.Wrap
                    text: Player.streamStatus || qsTr("Not checked yet")
                }
                QQC2.Button {
                    text: qsTr("Check streams")
                    icon.name: "view-refresh"
                    onClicked: { Player.streamStatus = qsTr("Checking…"); Player.testStream() }
                }
            }

            // ---- about ------------------------------------------------------------------------
            Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: qsTr("About") }
            QQC2.Label {
                Layout.maximumWidth: Kirigami.Units.gridUnit * 28
                wrapMode: Text.Wrap
                text: qsTr("Free software under the GNU GPL v3. Lyrics sync via LRCLIB.")
            }
        }
    }
}
