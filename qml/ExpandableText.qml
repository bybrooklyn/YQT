// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// A description (artist bio, album notes) that shows a short excerpt with "Read more", and an optional source
// link ("Wikipedia"). The excerpt is cut on a word boundary by Rust (`Nav.excerpt`), the link is only opened when
// `Nav.isSafeLink` accepts it (https, no credentials). Plain text only: nothing here is interpreted as markup.
ColumnLayout {
    id: root

    property string text
    /// Characters kept while collapsed.
    property int collapsedChars: 260
    property bool expanded: false
    /// Link shown after the text (e.g. the artist's Wikipedia page) and its label.
    property string linkUrl
    property string linkLabel: qsTr("Wikipedia")
    property string linkPrefix: qsTr("From")
    readonly property string excerpt: text.length > 0 ? Nav.excerpt(text, collapsedChars) : ""
    readonly property bool collapsible: excerpt !== text
    readonly property bool hasLink: linkUrl.length > 0 && Nav.isSafeLink(linkUrl)

    visible: text.length > 0 || hasLink
    spacing: Kirigami.Units.smallSpacing

    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    QQC2.Label {
        Layout.fillWidth: true
        visible: root.text.length > 0
        text: root.expanded || !root.collapsible ? root.text : root.excerpt
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        lineHeight: 1.15
        opacity: 0.9
    }

    RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing

        QQC2.AbstractButton {
            id: toggle
            visible: root.collapsible
            text: root.expanded ? qsTr("Show less") : qsTr("Read more")
            activeFocusOnTab: true
            padding: Kirigami.Units.smallSpacing
            Layout.leftMargin: -padding   // optically align the label with the text above
            onClicked: root.expanded = !root.expanded
            Accessible.role: Accessible.Button
            Accessible.name: text
            contentItem: QQC2.Label {
                text: toggle.text
                font.weight: Font.DemiBold
                color: Kirigami.Theme.linkColor
                font.underline: toggle.hovered || toggle.visualFocus
            }
            background: Rectangle {
                color: "transparent"
                radius: Kirigami.Units.cornerRadius
                border.width: toggle.visualFocus ? 2 : 0
                border.color: Kirigami.Theme.highlightColor
            }
            HoverHandler { cursorShape: Qt.PointingHandCursor }
        }

        QQC2.AbstractButton {
            id: link
            visible: root.hasLink
            activeFocusOnTab: true
            padding: Kirigami.Units.smallSpacing
            onClicked: if (Nav.isSafeLink(root.linkUrl)) Qt.openUrlExternally(root.linkUrl)
            Accessible.role: Accessible.Link
            Accessible.name: root.linkPrefix + " " + root.linkLabel
            contentItem: RowLayout {
                spacing: Kirigami.Units.smallSpacing
                Kirigami.Icon { source: "internet-services"; implicitWidth: Kirigami.Units.iconSizes.small; implicitHeight: implicitWidth }
                QQC2.Label {
                    text: root.linkPrefix + " " + root.linkLabel
                    color: Kirigami.Theme.linkColor
                    font.underline: link.hovered || link.visualFocus
                }
            }
            background: Rectangle {
                color: "transparent"
                radius: Kirigami.Units.cornerRadius
                border.width: link.visualFocus ? 2 : 0
                border.color: Kirigami.Theme.highlightColor
            }
            HoverHandler { cursorShape: Qt.PointingHandCursor }
            QQC2.ToolTip.visible: hovered
            QQC2.ToolTip.text: root.linkUrl
            QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
        }
        Item { Layout.fillWidth: true }
    }
}
