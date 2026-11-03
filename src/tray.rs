// SPDX-License-Identifier: GPL-3.0-only
//! System-tray logic: which menu entries exist and what they say, and the now-playing tooltip.
//!
//! Everything here is pure (no Qt) and unit-tested. `TrayMenuModel` is the thin QObject QML (`TrayController.qml`)
//! reads: call `update(...)` whenever the state changes, then render `entryCount()` entries via `entryId(i)`,
//! `entryLabel(i)`, `entryEnabled(i)`, `entryChecked(i)`, `entryIsSeparator(i)`. QML never decides a label itself.
//!
//! Native Linux StatusNotifierItem seam: the QML side uses `Qt.labs.platform.SystemTrayIcon`, which on Plasma
//! already registers a StatusNotifierItem through Qt's D-Bus tray backend. A hand-rolled SNI (e.g. the `ksni`
//! crate) would only be needed to avoid depending on `Qt.labs.platform`; `tray_menu` is backend-agnostic, so such a
//! backend could render the same `Vec<MenuEntry>`.
use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt_lib::QString;

/// What an entry does when activated (`entryId` in QML).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    PlayPause,
    Next,
    Previous,
    ToggleLike,
    ToggleWindow,
    ToggleMini,
    Quit,
}

impl Action {
    /// Stable string id QML switches on.
    pub fn id(self) -> &'static str {
        match self {
            Action::PlayPause => "playpause",
            Action::Next => "next",
            Action::Previous => "previous",
            Action::ToggleLike => "like",
            Action::ToggleWindow => "window",
            Action::ToggleMini => "mini",
            Action::Quit => "quit",
        }
    }
}

/// Everything the menu depends on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrayState {
    pub playing: bool,
    pub has_track: bool,
    pub signed_in: bool,
    pub liked: bool,
    pub window_visible: bool,
    pub mini_visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    Item { action: Action, label: String, enabled: bool, checked: Option<bool> },
    Separator,
}

fn item(action: Action, label: &str, enabled: bool) -> MenuEntry {
    MenuEntry::Item { action, label: label.to_owned(), enabled, checked: None }
}

/// The tray menu for a given app state.
pub fn tray_menu(s: &TrayState) -> Vec<MenuEntry> {
    let mut v = vec![
        item(Action::PlayPause, if s.playing { "Pause" } else { "Play" }, s.has_track),
        item(Action::Previous, "Previous", s.has_track),
        item(Action::Next, "Next", s.has_track),
    ];
    // Liking needs an account; a signed-out user never sees the entry.
    if s.signed_in {
        v.push(item(Action::ToggleLike, if s.liked { "Unlike" } else { "Like" }, s.has_track));
    }
    v.push(MenuEntry::Separator);
    v.push(item(Action::ToggleWindow, if s.window_visible { "Hide window" } else { "Show window" }, true));
    v.push(MenuEntry::Item { action: Action::ToggleMini, label: "Mini-player".to_owned(), enabled: true, checked: Some(s.mini_visible) });
    v.push(MenuEntry::Separator);
    v.push(item(Action::Quit, "Quit", true));
    v
}

/// Longest tooltip we build, in characters (desktops truncate on their own, but huge strings are rude).
pub const TOOLTIP_MAX_CHARS: usize = 120;

/// `"Title — Artist"`, or just the title, or the app name when nothing is playing. Truncated on a character
/// boundary with an ellipsis.
pub fn tooltip_text(title: &str, artist: &str) -> String {
    let (t, a) = (title.trim(), artist.trim());
    let full = match (t.is_empty(), a.is_empty()) {
        (true, _) => return "YT Music".to_owned(),
        (false, true) => t.to_owned(),
        (false, false) => format!("{t} — {a}"),
    };
    truncate_chars(&full, TOOLTIP_MAX_CHARS)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(i32, revision)] // bumps on every change of the menu; bind to it to re-read the entries
        type TrayMenuModel = super::TrayMenuModelRust;

        /// Recompute the menu for this state.
        #[qinvokable]
        fn update(
            self: Pin<&mut TrayMenuModel>,
            playing: bool,
            has_track: bool,
            signed_in: bool,
            liked: bool,
            window_visible: bool,
            mini_visible: bool,
        );
        #[qinvokable]
        fn entry_count(self: &TrayMenuModel) -> i32;
        #[qinvokable]
        fn entry_is_separator(self: &TrayMenuModel, i: i32) -> bool;
        #[qinvokable]
        fn entry_id(self: &TrayMenuModel, i: i32) -> QString;
        #[qinvokable]
        fn entry_label(self: &TrayMenuModel, i: i32) -> QString;
        #[qinvokable]
        fn entry_enabled(self: &TrayMenuModel, i: i32) -> bool;
        #[qinvokable]
        fn entry_checkable(self: &TrayMenuModel, i: i32) -> bool;
        #[qinvokable]
        fn entry_checked(self: &TrayMenuModel, i: i32) -> bool;
        /// Now-playing tooltip text.
        #[qinvokable]
        fn tooltip(self: &TrayMenuModel, title: &QString, artist: &QString) -> QString;
    }
}

#[derive(Default)]
pub struct TrayMenuModelRust {
    revision: i32,
    entries: Vec<MenuEntry>,
}

impl qobject::TrayMenuModel {
    fn update(
        mut self: Pin<&mut Self>,
        playing: bool,
        has_track: bool,
        signed_in: bool,
        liked: bool,
        window_visible: bool,
        mini_visible: bool,
    ) {
        let st = TrayState { playing, has_track, signed_in, liked, window_visible, mini_visible };
        let entries = tray_menu(&st);
        if entries != self.rust().entries {
            self.as_mut().rust_mut().get_mut().entries = entries;
            let rev = self.revision().wrapping_add(1);
            self.as_mut().set_revision(rev);
        }
    }

    fn at(&self, i: i32) -> Option<&MenuEntry> {
        usize::try_from(i).ok().and_then(|i| self.rust().entries.get(i))
    }
    fn entry_count(&self) -> i32 {
        i32::try_from(self.rust().entries.len()).unwrap_or(i32::MAX)
    }
    fn entry_is_separator(&self, i: i32) -> bool {
        matches!(self.at(i), Some(MenuEntry::Separator))
    }
    fn entry_id(&self, i: i32) -> QString {
        match self.at(i) {
            Some(MenuEntry::Item { action, .. }) => QString::from(action.id()),
            _ => QString::default(),
        }
    }
    fn entry_label(&self, i: i32) -> QString {
        match self.at(i) {
            Some(MenuEntry::Item { label, .. }) => QString::from(label.as_str()),
            _ => QString::default(),
        }
    }
    fn entry_enabled(&self, i: i32) -> bool {
        matches!(self.at(i), Some(MenuEntry::Item { enabled: true, .. }))
    }
    fn entry_checkable(&self, i: i32) -> bool {
        matches!(self.at(i), Some(MenuEntry::Item { checked: Some(_), .. }))
    }
    fn entry_checked(&self, i: i32) -> bool {
        matches!(self.at(i), Some(MenuEntry::Item { checked: Some(true), .. }))
    }
    fn tooltip(&self, title: &QString, artist: &QString) -> QString {
        QString::from(tooltip_text(&title.to_string(), &artist.to_string()).as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(v: &[MenuEntry]) -> Vec<String> {
        v.iter()
            .map(|e| match e {
                MenuEntry::Item { label, .. } => label.clone(),
                MenuEntry::Separator => "-".into(),
            })
            .collect()
    }
    fn find(v: &[MenuEntry], a: Action) -> Option<(String, bool, Option<bool>)> {
        v.iter().find_map(|e| match e {
            MenuEntry::Item { action, label, enabled, checked } if *action == a => Some((label.clone(), *enabled, *checked)),
            _ => None,
        })
    }
    fn st(f: impl FnOnce(&mut TrayState)) -> TrayState {
        let mut s = TrayState::default();
        f(&mut s);
        s
    }

    #[test]
    fn idle_signed_out() {
        let m = tray_menu(&TrayState::default());
        assert_eq!(labels(&m), ["Play", "Previous", "Next", "-", "Show window", "Mini-player", "-", "Quit"]);
        // transport is disabled with nothing loaded; window, mini-player and quit always work
        for a in [Action::PlayPause, Action::Previous, Action::Next] {
            assert!(!find(&m, a).unwrap().1, "{a:?} must be disabled");
        }
        for a in [Action::ToggleWindow, Action::ToggleMini, Action::Quit] {
            assert!(find(&m, a).unwrap().1, "{a:?} must be enabled");
        }
    }

    #[test]
    fn play_pause_label_follows_state() {
        let playing = tray_menu(&st(|s| {
            s.has_track = true;
            s.playing = true;
        }));
        assert_eq!(find(&playing, Action::PlayPause).unwrap(), ("Pause".into(), true, None));
        let paused = tray_menu(&st(|s| s.has_track = true));
        assert_eq!(find(&paused, Action::PlayPause).unwrap(), ("Play".into(), true, None));
    }

    #[test]
    fn like_entry_only_when_signed_in() {
        assert!(find(&tray_menu(&st(|s| s.has_track = true)), Action::ToggleLike).is_none());
        let signed = tray_menu(&st(|s| {
            s.signed_in = true;
            s.has_track = true;
        }));
        assert_eq!(find(&signed, Action::ToggleLike).unwrap(), ("Like".into(), true, None));
        let liked = tray_menu(&st(|s| {
            s.signed_in = true;
            s.has_track = true;
            s.liked = true;
        }));
        assert_eq!(find(&liked, Action::ToggleLike).unwrap().0, "Unlike");
        // signed in but nothing playing: shown, disabled
        let none = tray_menu(&st(|s| s.signed_in = true));
        assert!(!find(&none, Action::ToggleLike).unwrap().1);
    }

    #[test]
    fn window_and_mini_toggles() {
        let m = tray_menu(&st(|s| {
            s.window_visible = true;
            s.mini_visible = true;
        }));
        assert_eq!(find(&m, Action::ToggleWindow).unwrap().0, "Hide window");
        assert_eq!(find(&m, Action::ToggleMini).unwrap().2, Some(true));
        let m = tray_menu(&TrayState::default());
        assert_eq!(find(&m, Action::ToggleWindow).unwrap().0, "Show window");
        assert_eq!(find(&m, Action::ToggleMini).unwrap().2, Some(false));
    }

    #[test]
    fn every_combination_is_well_formed() {
        for bits in 0u8..64 {
            let s = TrayState {
                playing: bits & 1 != 0,
                has_track: bits & 2 != 0,
                signed_in: bits & 4 != 0,
                liked: bits & 8 != 0,
                window_visible: bits & 16 != 0,
                mini_visible: bits & 32 != 0,
            };
            let m = tray_menu(&s);
            assert!(matches!(m.first(), Some(MenuEntry::Item { action: Action::PlayPause, .. })));
            assert!(matches!(m.last(), Some(MenuEntry::Item { action: Action::Quit, enabled: true, .. })));
            // never two separators in a row, never one at an edge
            assert!(!matches!(m.first(), Some(MenuEntry::Separator)) && !matches!(m.last(), Some(MenuEntry::Separator)));
            assert!(!m.windows(2).any(|w| w[0] == MenuEntry::Separator && w[1] == MenuEntry::Separator));
            assert_eq!(find(&m, Action::ToggleLike).is_some(), s.signed_in);
        }
    }

    #[test]
    fn action_ids_are_unique() {
        let all =
            [Action::PlayPause, Action::Next, Action::Previous, Action::ToggleLike, Action::ToggleWindow, Action::ToggleMini, Action::Quit];
        let mut ids: Vec<_> = all.iter().map(|a| a.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
    }

    #[test]
    fn tooltip_basic() {
        assert_eq!(tooltip_text("Song", "Band"), "Song — Band");
        assert_eq!(tooltip_text("Song", ""), "Song");
        assert_eq!(tooltip_text("  Song ", "  "), "Song");
        assert_eq!(tooltip_text("", "Band"), "YT Music");
        assert_eq!(tooltip_text("", ""), "YT Music");
    }

    #[test]
    fn tooltip_truncates_on_char_boundaries() {
        let long = "ありがとう".repeat(100); // multi-byte
        let t = tooltip_text(&long, "アーティスト");
        assert_eq!(t.chars().count(), TOOLTIP_MAX_CHARS);
        assert!(t.ends_with('…'));
        let emoji = "🎵".repeat(500);
        let t = tooltip_text(&emoji, "x");
        assert_eq!(t.chars().count(), TOOLTIP_MAX_CHARS);
        // exactly at the limit is untouched
        let exact = "a".repeat(TOOLTIP_MAX_CHARS);
        assert_eq!(tooltip_text(&exact, ""), exact);
        let over = "a".repeat(TOOLTIP_MAX_CHARS + 1);
        assert_eq!(tooltip_text(&over, "").chars().count(), TOOLTIP_MAX_CHARS);
    }
}
