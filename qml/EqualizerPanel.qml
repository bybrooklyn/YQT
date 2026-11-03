// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.ytm

// The 10-band equalizer: on/off, presets (built-in and your own), preamp, ten band sliders and a live curve of the
// resulting frequency response. All state lives in the `Equalizer` singleton (src/audio/qt_eq.rs).
ColumnLayout {
    id: panel
    spacing: Kirigami.Units.largeSpacing
    Kirigami.Theme.inherit: false
    Kirigami.Theme.colorGroup: Kirigami.Theme.Active

    readonly property var labels: ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"]
    readonly property real rangeDb: 12

    property var presetModel: Equalizer.presetNames()
    Connections {
        target: Equalizer
        function onPresetChanged() { panel.presetModel = Equalizer.presetNames() }
    }

    // ---- header: switch, presets
    RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing

        QQC2.Switch {
            id: onOff
            text: qsTr("Equalizer")
            checked: Equalizer.enabled
            onToggled: Equalizer.setEnabled(checked)
            Kirigami.Theme.inherit: false
            Kirigami.Theme.colorGroup: Kirigami.Theme.Active
        }
        Item { Layout.fillWidth: true }
        QQC2.Label { text: qsTr("Preset"); opacity: 0.8 }
        QQC2.ComboBox {
            id: presetBox
            Layout.minimumWidth: Kirigami.Units.gridUnit * 10
            model: panel.presetModel
            // "Custom" is not an entry: show it as a placeholder
            currentIndex: model.indexOf(Equalizer.preset)
            displayText: currentIndex >= 0 ? currentText : qsTr("Custom")
            onActivated: index => Equalizer.applyPreset(model[index])
            Kirigami.Theme.inherit: false
            Kirigami.Theme.colorGroup: Kirigami.Theme.Active
        }
        QQC2.Button {
            icon.name: "document-save"
            text: qsTr("Save…")
            onClicked: { nameField.text = Equalizer.isUserPreset(Equalizer.preset) ? Equalizer.preset : ""; saveDialog.open() }
            Kirigami.Theme.inherit: false
            Kirigami.Theme.colorGroup: Kirigami.Theme.Active
        }
        QQC2.Button {
            icon.name: "edit-delete"
            text: qsTr("Delete")
            enabled: Equalizer.isUserPreset(Equalizer.preset)
            onClicked: Equalizer.deletePreset(Equalizer.preset)
            Kirigami.Theme.inherit: false
            Kirigami.Theme.colorGroup: Kirigami.Theme.Active
        }
        QQC2.Button {
            icon.name: "edit-reset"
            text: qsTr("Reset")
            onClicked: Equalizer.applyPreset("Flat")
            Kirigami.Theme.inherit: false
            Kirigami.Theme.colorGroup: Kirigami.Theme.Active
        }
    }

    // ---- response curve
    Item {
        id: curveBox
        Layout.fillWidth: true
        Layout.preferredHeight: Kirigami.Units.gridUnit * 8
        opacity: Equalizer.enabled ? 1 : 0.45
        Behavior on opacity { NumberAnimation { duration: Kirigami.Units.shortDuration } }

        Canvas {
            id: curve
            anchors.fill: parent
            Kirigami.Theme.inherit: false
            Kirigami.Theme.colorGroup: Kirigami.Theme.Active
            renderTarget: Canvas.Image

            readonly property real fMin: 20
            readonly property real fMax: 20000
            readonly property real dbMax: 15
            function xOf(f, w) { return w * Math.log(f / fMin) / Math.log(fMax / fMin) }
            function yOf(db, h) { return h * (0.5 - db / (2 * dbMax)) }

            onPaint: {
                const ctx = getContext("2d")
                const w = width, h = height
                ctx.reset()
                ctx.fillStyle = Kirigami.Theme.backgroundColor
                ctx.fillRect(0, 0, w, h)
                const text = Kirigami.Theme.textColor
                ctx.font = Math.round(Kirigami.Units.gridUnit * 0.6) + "px sans-serif"
                ctx.textBaseline = "middle"
                // horizontal dB grid
                for (const db of [-12, -6, 0, 6, 12]) {
                    ctx.strokeStyle = Qt.rgba(text.r, text.g, text.b, db === 0 ? 0.45 : 0.15)
                    ctx.lineWidth = 1
                    ctx.beginPath(); ctx.moveTo(0, Math.round(yOf(db, h)) + 0.5); ctx.lineTo(w, Math.round(yOf(db, h)) + 0.5); ctx.stroke()
                    ctx.fillStyle = Qt.rgba(text.r, text.g, text.b, 0.6)
                    ctx.fillText((db > 0 ? "+" : "") + db, 4, yOf(db, h) - 7)
                }
                // vertical frequency grid at the band centres
                const fs = Equalizer.bandFrequencies()
                for (let i = 0; i < fs.length; i++) {
                    const x = Math.round(xOf(fs[i], w)) + 0.5
                    ctx.strokeStyle = Qt.rgba(text.r, text.g, text.b, 0.12)
                    ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, h); ctx.stroke()
                }
                // the response
                const n = Math.max(2, Math.floor(w / 3))
                const freqs = []
                for (let i = 0; i < n; i++) freqs.push(fMin * Math.pow(fMax / fMin, i / (n - 1)))
                const r = Equalizer.response(freqs)
                const hl = Kirigami.Theme.highlightColor
                ctx.beginPath()
                for (let i = 0; i < n; i++) {
                    const x = w * i / (n - 1)
                    const y = yOf(Math.max(-dbMax, Math.min(dbMax, r[i])), h)
                    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y)
                }
                ctx.strokeStyle = hl
                ctx.lineWidth = 2
                ctx.stroke()
                ctx.lineTo(w, yOf(0, h)); ctx.lineTo(0, yOf(0, h)); ctx.closePath()
                ctx.fillStyle = Qt.rgba(hl.r, hl.g, hl.b, 0.18)
                ctx.fill()
            }

            Connections {
                target: Equalizer
                function onBandsDbChanged() { curve.requestPaint() }
                function onPreampDbChanged() { curve.requestPaint() }
                function onEnabledChanged() { curve.requestPaint() }
            }
            Connections {
                target: Kirigami.Theme
                function onBackgroundColorChanged() { curve.requestPaint() }
                function onHighlightColorChanged() { curve.requestPaint() }
            }
            onWidthChanged: requestPaint()
            onHeightChanged: requestPaint()
            Component.onCompleted: requestPaint()
        }
        Rectangle { anchors.fill: parent; color: "transparent"; border.width: 1; border.color: Kirigami.Theme.disabledTextColor; radius: Kirigami.Units.smallSpacing; opacity: 0.5 }
    }

    // ---- sliders: preamp + ten bands
    RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing
        opacity: Equalizer.enabled ? 1 : 0.55

        component Fader: ColumnLayout {
            id: fader
            property string caption
            property real dbValue: 0
            signal moved(real db)
            signal released()
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: Kirigami.Units.smallSpacing

            QQC2.Label {
                Layout.alignment: Qt.AlignHCenter
                text: (fader.dbValue > 0 ? "+" : "") + fader.dbValue.toFixed(1)
                font.pointSize: Kirigami.Theme.smallFont.pointSize
                opacity: 0.8
            }
            QQC2.Slider {
                id: slider
                Layout.alignment: Qt.AlignHCenter
                Layout.fillHeight: true
                Layout.preferredHeight: Kirigami.Units.gridUnit * 9
                orientation: Qt.Vertical
                from: -panel.rangeDb
                to: panel.rangeDb
                stepSize: 0.5
                snapMode: QQC2.Slider.SnapAlways
                value: fader.dbValue
                onMoved: fader.moved(value)
                onPressedChanged: if (!pressed) fader.released()
                Kirigami.Theme.inherit: false
                Kirigami.Theme.colorGroup: Kirigami.Theme.Active
                // double click = back to 0 dB
                TapHandler { acceptedButtons: Qt.LeftButton; onDoubleTapped: { fader.moved(0); fader.released() } }
            }
            QQC2.Label {
                Layout.alignment: Qt.AlignHCenter
                text: fader.caption
                font.bold: true
            }
        }

        Fader {
            caption: qsTr("Preamp")
            dbValue: Equalizer.preampDb
            onMoved: db => Equalizer.setPreampDb(db)
            onReleased: Equalizer.persist()
            Layout.preferredWidth: Kirigami.Units.gridUnit * 3
            Layout.fillWidth: false
        }
        Kirigami.Separator { Layout.fillHeight: true }
        Repeater {
            model: 10
            Fader {
                required property int index
                caption: panel.labels[index]
                dbValue: Equalizer.bandsDb.length > index ? Equalizer.bandsDb[index] : 0
                onMoved: db => Equalizer.setBandDb(index, db)
                onReleased: Equalizer.persist()
            }
        }
    }

    QQC2.Label {
        Layout.fillWidth: true
        text: qsTr("Boosting bands can raise the overall level; lower the preamp if the sound distorts. A soft limiter prevents hard clipping.")
        wrapMode: Text.WordWrap
        opacity: 0.6
        font.pointSize: Kirigami.Theme.smallFont.pointSize
    }

    QQC2.Dialog {
        id: saveDialog
        title: qsTr("Save preset")
        modal: true
        standardButtons: QQC2.Dialog.Save | QQC2.Dialog.Cancel
        anchors.centerIn: QQC2.Overlay.overlay
        Kirigami.Theme.inherit: false
        Kirigami.Theme.colorGroup: Kirigami.Theme.Active
        ColumnLayout {
            QQC2.Label { text: qsTr("Name") }
            QQC2.TextField { id: nameField; Layout.minimumWidth: Kirigami.Units.gridUnit * 14; maximumLength: 40; onAccepted: saveDialog.accept() }
            QQC2.Label {
                visible: nameField.text.length > 0 && !panel.nameOk
                text: qsTr("Built-in preset names cannot be used.")
                color: Kirigami.Theme.negativeTextColor
            }
        }
        onAccepted: if (!Equalizer.savePreset(nameField.text)) nameField.forceActiveFocus()
    }
    readonly property bool nameOk: Equalizer.presetNames().slice(0, 10).map(n => n.toLowerCase()).indexOf(nameField.text.trim().toLowerCase()) < 0
}
