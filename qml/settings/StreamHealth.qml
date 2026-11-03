// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// Which stream route is in use, and a button to test it.
RowLayout {
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active
    Layout.fillWidth: true
    spacing: Kirigami.Units.largeSpacing

    QQC2.Label {
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        text: Player.streamStatus || qsTr("Not checked yet")
    }
    QQC2.Button {
        text: qsTr("Check streams")
        icon.name: "view-refresh"
        onClicked: { Player.streamStatus = qsTr("Checking…"); Player.testStream() }
    }
}
