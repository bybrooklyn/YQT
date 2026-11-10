// SPDX-License-Identifier: GPL-3.0-only
import QtQuick
import org.kde.kquickcontrols as KQuickControls

// KDE's own "press the keys" field for one keyboard shortcut. Settings loads it through a Loader, so where the
// module is not installed (a bundle without it) the plain text field stays instead.
KQuickControls.KeySequenceItem {
    id: root
    /// The shortcut as stored in the settings file ("Ctrl+K", "Space", "" for none).
    property string value: ""
    signal committed(string value)

    modifierlessAllowed: true          // Space, the arrow keys, the media keys
    multiKeyShortcutsAllowed: false
    // These are shortcuts inside this window only: nothing to clash with across the desktop.
    checkForConflictsAgainst: KQuickControls.ShortcutType.None

    onValueChanged: if (String(keySequence) !== value) keySequence = value
    Component.onCompleted: keySequence = value
    onKeySequenceModified: if (String(keySequence) !== value) committed(String(keySequence))
}
