// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// One of the page's sections: heading, a line of context, and cards.
ColumnLayout {
    id: sec
    property string title
    property string subtitle
    property string icon
    default property alias content: body.data

    readonly property bool matched: {
        const kids = body.children
        for (let i = 0; i < kids.length; ++i)
            if (kids[i].matched === true) return true
        return false
    }

    visible: matched
    Layout.fillWidth: true
    spacing: Kirigami.Units.smallSpacing

    RowLayout {
        Layout.topMargin: Kirigami.Units.gridUnit
        spacing: Kirigami.Units.smallSpacing
        Kirigami.Icon {
            source: sec.icon
            visible: sec.icon !== ""
            implicitWidth: Kirigami.Units.iconSizes.smallMedium
            implicitHeight: implicitWidth
            Layout.alignment: Qt.AlignVCenter
        }
        Kirigami.Heading {
            level: 1
            text: sec.title
            Accessible.role: Accessible.Heading
        }
    }
    QQC2.Label {
        text: sec.subtitle
        visible: text !== ""
        wrapMode: Text.Wrap
        opacity: 0.7
        Layout.fillWidth: true
        Layout.bottomMargin: Kirigami.Units.smallSpacing
    }
    ColumnLayout {
        id: body
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing
    }
}
