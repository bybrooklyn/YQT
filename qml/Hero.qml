// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// The big header of a page. `hero` is PageData's hero:
//   {kind: "album"|"playlist"|"artist"|null, title, subtitle, cover, description, wikipediaUrl, stats[], actions[]}
//  - album / playlist: large rounded cover on a gradient tinted with the cover's accent colour, title, artist
//    links, "Album • 2019 • 12 songs • 46 min", description, actions
//  - artist: header image as a banner with the name over it, subscribers, bio with "Read more" and a Wikipedia link
//  - no kind (Home): just a greeting line
// Presentational only: it emits `actionTriggered(name)` for a button and `artistActivated(item)` for an artist link.
Item {
    id: root

    property var hero
    /// Artist items ({title, browseId, ...}) shown as links under the title.
    property var artists: []
    property bool compact: false
    /// False while the page has no songs: Play, Shuffle, Download and Add to queue wait.
    property bool playable: true
    readonly property string kind: hero && hero.kind ? hero.kind : ""
    readonly property bool isArtist: kind === "artist"
    readonly property bool isGreeting: !!hero && kind === ""
    // tolerate heroes that only carry some fields (the greeting has just a title)
    readonly property string heroTitle: hero && hero.title || ""
    readonly property string heroSubtitle: hero && hero.subtitle || ""
    readonly property string heroCover: hero && hero.cover || ""
    readonly property string heroDescription: hero && hero.description || ""
    readonly property color tint: accent.ready && AppConfig.accentFromCover ? accent.color : Kirigami.Theme.highlightColor
    readonly property real coverSize: compact ? Math.min(Kirigami.Units.gridUnit * 13, width - Kirigami.Units.gridUnit * 4) : Kirigami.Units.gridUnit * 14
    readonly property real margin: compact ? Kirigami.Units.largeSpacing : Kirigami.Units.gridUnit * 1.5

    signal actionTriggered(string name)
    signal artistActivated(var item)

    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    implicitHeight: isArtist ? artistLayout.implicitHeight + margin
                             : (isGreeting ? greetingLayout.implicitHeight : collectionLayout.implicitHeight) + margin * 2
    visible: !!hero

    Accent { id: accent; source: root.isArtist ? "" : root.heroCover }

    // tinted backdrop
    Rectangle {
        anchors.fill: parent
        visible: !root.isArtist
        gradient: Gradient {
            GradientStop { position: 0; color: Qt.alpha(root.tint, root.isGreeting ? 0.18 : 0.42) }
            GradientStop { position: 1; color: Qt.alpha(root.tint, 0) }
        }
    }

    // ---- greeting (Home)
    ColumnLayout {
        id: greetingLayout
        visible: root.isGreeting
        anchors { left: parent.left; right: parent.right; top: parent.top; margins: root.margin }
        Kirigami.Heading {
            Layout.fillWidth: true
            level: 1
            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 2
            font.weight: Font.Bold
            text: root.heroTitle
            wrapMode: Text.Wrap
        }
        QQC2.Label {
            Layout.fillWidth: true
            visible: text.length > 0
            text: root.heroSubtitle
            opacity: 0.7
        }
    }

    // ---- album / playlist
    GridLayout {
        id: collectionLayout
        visible: !root.isArtist && !root.isGreeting
        anchors { left: parent.left; right: parent.right; top: parent.top; margins: root.margin }
        columns: root.compact ? 1 : 2
        columnSpacing: Kirigami.Units.gridUnit * 1.5
        rowSpacing: Kirigami.Units.largeSpacing

        CoverImage {
            Layout.alignment: Qt.AlignTop | Qt.AlignLeft
            Layout.preferredWidth: root.coverSize
            Layout.preferredHeight: root.coverSize
            source: root.heroCover
            decode: 640
            radiusPercent: Math.max(AppConfig.coverRadiusPercent, 5)
            placeholderIcon: root.kind === "playlist" ? "view-media-playlist" : "media-optical-audio"
        }

        ColumnLayout {
            Layout.fillWidth: true
            Layout.alignment: Qt.AlignTop
            spacing: Kirigami.Units.smallSpacing

            Kirigami.Heading {
                Layout.fillWidth: true
                level: 1
                font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.7
                font.weight: Font.Bold
                text: root.heroTitle
                wrapMode: Text.Wrap
                maximumLineCount: 3
                elide: Text.ElideRight
            }
            // artist links, or the plain byline when no artist has a page
            Flow {
                Layout.fillWidth: true
                visible: root.artists.length > 0
                spacing: Kirigami.Units.smallSpacing
                layoutDirection: Qt.LeftToRight
                Repeater {
                    model: root.artists
                    delegate: ArtistChip {
                        required property var modelData
                        compact: true
                        item: modelData
                        onActivated: root.artistActivated(modelData)
                    }
                }
            }
            QQC2.Label {
                Layout.fillWidth: true
                visible: root.artists.length === 0 && text.length > 0
                text: root.heroSubtitle
                font.weight: Font.DemiBold
                wrapMode: Text.Wrap
            }
            StatChips {
                Layout.fillWidth: true
                stats: root.hero && root.hero.stats || []
            }
            ExpandableText {
                Layout.fillWidth: true
                Layout.topMargin: Kirigami.Units.smallSpacing
                text: root.heroDescription
                collapsedChars: 200
                linkUrl: root.hero ? root.hero.wikipediaUrl || "" : ""
            }
            ActionRow {
                Layout.topMargin: Kirigami.Units.smallSpacing
                Layout.fillWidth: true
                actions: root.hero ? root.hero.actions || [] : []
            }
        }
    }

    // ---- artist
    ColumnLayout {
        id: artistLayout
        visible: root.isArtist
        anchors { left: parent.left; right: parent.right; top: parent.top }
        spacing: 0

        Item {
            id: banner
            Layout.fillWidth: true
            Layout.preferredHeight: root.compact ? Kirigami.Units.gridUnit * 14 : Kirigami.Units.gridUnit * 18

            // fallback backdrop when there is no header image
            Rectangle {
                anchors.fill: parent
                gradient: Gradient {
                    GradientStop { position: 0; color: Qt.alpha(Kirigami.Theme.highlightColor, 0.45) }
                    GradientStop { position: 1; color: Qt.alpha(Kirigami.Theme.highlightColor, 0.05) }
                }
            }
            Image {
                id: bannerImage
                anchors.fill: parent
                source: root.isArtist ? root.heroCover : ""
                sourceSize.width: 1280
                asynchronous: true
                fillMode: Image.PreserveAspectCrop
                verticalAlignment: Image.AlignTop
                opacity: status === Image.Ready ? 1 : 0
                Behavior on opacity { enabled: !AppConfig.reduceMotion; NumberAnimation { duration: 200 } }
            }
            Kirigami.Icon {
                visible: bannerImage.status !== Image.Ready
                anchors.centerIn: parent
                width: Kirigami.Units.gridUnit * 5
                height: width
                source: "user-identity"
                opacity: 0.25
            }
            // fade the picture into the page so the name reads on any image
            Rectangle {
                anchors.fill: parent
                gradient: Gradient {
                    GradientStop { position: 0.35; color: "transparent" }
                    GradientStop { position: 1; color: Kirigami.Theme.backgroundColor }
                }
            }
            ColumnLayout {
                anchors { left: parent.left; right: parent.right; bottom: parent.bottom; leftMargin: root.margin; rightMargin: root.margin }
                spacing: 0
                Kirigami.Heading {
                    Layout.fillWidth: true
                    level: 1
                    font.pointSize: Kirigami.Theme.defaultFont.pointSize * (root.compact ? 2 : 2.6)
                    font.weight: Font.Bold
                    text: root.heroTitle
                    wrapMode: Text.Wrap
                    maximumLineCount: 2
                    elide: Text.ElideRight
                }
                QQC2.Label {
                    visible: text.length > 0
                    text: root.heroSubtitle
                    opacity: 0.8
                }
            }
        }

        ColumnLayout {
            Layout.fillWidth: true
            Layout.leftMargin: root.margin
            Layout.rightMargin: root.margin
            Layout.topMargin: Kirigami.Units.smallSpacing
            spacing: Kirigami.Units.largeSpacing
            ActionRow {
                Layout.fillWidth: true
                actions: root.hero ? root.hero.actions || [] : []
            }
            ExpandableText {
                Layout.fillWidth: true
                Layout.maximumWidth: Kirigami.Units.gridUnit * 48
                text: root.heroDescription
                collapsedChars: 280
                linkUrl: root.hero ? root.hero.wikipediaUrl || "" : ""
            }
        }
    }

    // ---- buttons
    component ActionRow: Flow {
        id: row
        property var actions: []
        spacing: Kirigami.Units.smallSpacing
        visible: actions.length > 0

        // `needsSongs`: nothing to do until the page has songs; `unavailable`: no backend for it yet (tooltip says so)
        readonly property var spec: ({
            "play": { text: qsTr("Play"), icon: "media-playback-start", primary: true, needsSongs: true },
            "shuffle": { text: qsTr("Shuffle"), icon: "media-playlist-shuffle", needsSongs: true },
            "radio": { text: qsTr("Radio"), icon: "radio" },
            "download": { text: qsTr("Download"), icon: "download", needsSongs: true },
            "add_to_queue": { text: qsTr("Add to queue"), icon: "list-add", needsSongs: true },
            "save_to_library": { text: qsTr("Save"), icon: "bookmark-new", unavailable: qsTr("Saving to your library is not available yet") }
        })

        Repeater {
            model: row.actions
            delegate: QQC2.Button {
                required property string modelData
                readonly property var info: row.spec[modelData] || ({ text: modelData, icon: "" })
                text: info.text
                icon.name: info.icon
                id: button
                highlighted: !!info.primary
                visible: !!row.spec[modelData]
                enabled: !info.unavailable && !(info.needsSongs && !root.playable)
                opacity: enabled ? 1 : 0.5
                QQC2.ToolTip.visible: !!info.unavailable && hovered
                QQC2.ToolTip.text: info.unavailable || ""
                QQC2.ToolTip.delay: Kirigami.Units.toolTipDelay
                // the primary action is a filled accent button whatever the style draws for `highlighted`
                background: Rectangle {
                    radius: Kirigami.Units.cornerRadius
                    color: !button.highlighted ? Qt.alpha(Kirigami.Theme.textColor, button.down ? 0.18 : button.hovered ? 0.12 : 0.07)
                                               : button.down ? Qt.darker(Kirigami.Theme.highlightColor, 1.2)
                                               : button.hovered ? Qt.lighter(Kirigami.Theme.highlightColor, 1.1) : Kirigami.Theme.highlightColor
                    border.width: button.visualFocus ? 2 : 0
                    border.color: Kirigami.Theme.textColor
                }
                icon.color: button.highlighted ? Kirigami.Theme.highlightedTextColor : Kirigami.Theme.textColor
                contentItem: RowLayout {
                    spacing: Kirigami.Units.smallSpacing
                    Kirigami.Icon { source: button.icon.name; color: button.icon.color; implicitWidth: Kirigami.Units.iconSizes.small; implicitHeight: implicitWidth }
                    QQC2.Label { text: button.text; font.weight: button.highlighted ? Font.DemiBold : Font.Normal; color: button.highlighted ? Kirigami.Theme.highlightedTextColor : Kirigami.Theme.textColor }
                }
                leftPadding: Kirigami.Units.largeSpacing * 1.5
                rightPadding: Kirigami.Units.largeSpacing * 1.5
                topPadding: Kirigami.Units.smallSpacing * 1.5
                bottomPadding: Kirigami.Units.smallSpacing * 1.5
                onClicked: root.actionTriggered(modelData)
                Accessible.name: info.text
            }
        }
    }
}
