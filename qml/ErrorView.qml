// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

// The one place a failure is shown. It only displays what Rust decided (`AppError`: kind, headline, hint, detail,
// id, retry policy); the page controller exposes those as `error*` properties and this view binds to them.
//
//   ErrorView { kind: ctl.errorKind; headline: ctl.errorHeadline; ...; onRetryRequested: ctl.retry() }
Item {
    id: root

    property string kind                // "network" "auth" "offline" "parse" "notfound" … (ErrorKind names)
    property string headline
    property string hint
    property string detail
    property string errorId
    property string diagnostics         // already redacted by Rust
    property string retryMode: "manual" // "never" | "manual" | "auto"
    property int retryMs: 0             // > 0: retry by itself after this long, with a visible countdown

    signal retryRequested()             // the user pressed Retry
    signal autoRetryRequested()         // the countdown ran out
    signal openSettings()
    signal showDownloads()

    property bool detailsOpen: false
    property int remainingMs: 0
    readonly property bool counting: remainingMs > 0

    implicitHeight: column.implicitHeight + Kirigami.Units.gridUnit * 2

    function restartCountdown() { remainingMs = retryMs > 0 ? retryMs : 0 }
    onRetryMsChanged: restartCountdown()
    onHeadlineChanged: restartCountdown()
    Component.onCompleted: restartCountdown()

    Timer {
        interval: 250
        repeat: true
        running: root.counting
        onTriggered: {
            root.remainingMs = Math.max(0, root.remainingMs - interval)
            if (root.remainingMs === 0) root.autoRetryRequested()
        }
    }

    // "Copy details" goes through a hidden text field: QML has no direct clipboard API
    TextEdit { id: clip; visible: false }
    function copyDetails() {
        clip.text = diagnostics
        clip.selectAll()
        clip.copy()
        copied = true
        copiedTimer.restart()
    }
    property bool copied: false
    Timer { id: copiedTimer; interval: 2000; onTriggered: root.copied = false }

    ColumnLayout {
        id: column
        anchors.centerIn: parent
        width: Math.min(parent.width - Kirigami.Units.gridUnit * 2, Kirigami.Units.gridUnit * 28)
        spacing: Kirigami.Units.largeSpacing

        Kirigami.Icon {
            Layout.alignment: Qt.AlignHCenter
            Layout.preferredWidth: Kirigami.Units.iconSizes.huge
            Layout.preferredHeight: Kirigami.Units.iconSizes.huge
            source: root.kind === "offline" ? "network-disconnect"
                  : root.kind === "auth" ? "dialog-password"
                  : root.kind === "notfound" || root.kind === "unavailable" ? "dialog-information"
                  : "dialog-error"
        }

        Kirigami.Heading {
            Layout.fillWidth: true
            level: 3
            text: root.headline
            wrapMode: Text.Wrap
            horizontalAlignment: Text.AlignHCenter
        }

        QQC2.Label {
            Layout.fillWidth: true
            visible: text.length > 0
            text: root.hint
            wrapMode: Text.Wrap
            horizontalAlignment: Text.AlignHCenter
            opacity: 0.8
        }

        QQC2.Label {
            Layout.fillWidth: true
            visible: root.counting
            text: qsTr("Trying again in %1 s…").arg(Math.ceil(root.remainingMs / 1000))
            horizontalAlignment: Text.AlignHCenter
            color: Kirigami.Theme.disabledTextColor
        }

        RowLayout {
            Layout.alignment: Qt.AlignHCenter
            spacing: Kirigami.Units.smallSpacing

            QQC2.Button {
                visible: root.retryMode !== "never" && root.kind !== "auth"
                text: root.counting ? qsTr("Retry now") : qsTr("Retry")
                icon.name: "view-refresh"
                onClicked: root.retryRequested()
            }
            QQC2.Button {
                visible: root.counting
                text: qsTr("Stop")
                icon.name: "dialog-cancel"
                onClicked: root.remainingMs = 0
            }
            QQC2.Button {
                visible: root.kind === "auth"
                text: qsTr("Open Settings")
                icon.name: "configure"
                highlighted: true
                onClicked: root.openSettings()
            }
            QQC2.Button {
                visible: root.kind === "offline"
                text: qsTr("Show downloads")
                icon.name: "folder-download"
                onClicked: root.showDownloads()
            }
            QQC2.Button {
                visible: root.diagnostics.length > 0
                text: root.copied ? qsTr("Copied") : qsTr("Copy details")
                icon.name: "edit-copy"
                onClicked: root.copyDetails()
            }
        }

        // technical detail, folded away until asked for
        QQC2.ToolButton {
            Layout.alignment: Qt.AlignHCenter
            visible: root.detail.length > 0 || root.errorId.length > 0
            text: root.detailsOpen ? qsTr("Hide technical details") : qsTr("Technical details")
            icon.name: root.detailsOpen ? "arrow-up" : "arrow-down"
            onClicked: root.detailsOpen = !root.detailsOpen
        }
        QQC2.Label {
            Layout.fillWidth: true
            visible: root.detailsOpen
            text: (root.detail.length > 0 ? root.detail + "\n" : "") + qsTr("Error id: %1").arg(root.errorId)
            wrapMode: Text.WrapAnywhere
            font: Kirigami.Theme.smallFont
            opacity: 0.7
            horizontalAlignment: Text.AlignHCenter
        }
    }
}
