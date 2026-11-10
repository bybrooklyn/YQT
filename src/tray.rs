// SPDX-License-Identifier: GPL-3.0-only
//! System-tray logic: which menu entries exist and what they say, and the now-playing tooltip.
//!
//! Everything here is pure (no Qt) and unit-tested. `TrayMenuModel` is the thin QObject QML (`TrayController.qml`)
//! reads: call `update(...)` whenever the state changes, then render `entryCount()` entries via `entryId(i)`,
//! `entryLabel(i)`, `entryEnabled(i)`, `entryChecked(i)`, `entryIsSeparator(i)`. QML never decides a label itself.
//!
//! The tray itself (Linux): a StatusNotifierItem over D-Bus via the pure-Rust `ksni` crate, so KDE Plasma, GNOME
//! (with the AppIndicator extension) and the other desktops that implement the spec show it natively. It does
//! **not** use `Qt.labs.platform`: that falls back to Qt Widgets classes where no native tray exists, and a
//! `QGuiApplication` aborts on those ("Cannot create a QWidget without QApplication"). When there is no
//! StatusNotifierWatcher (or on other systems for now) `available` stays false and the app simply has no tray.
// `update` takes the whole tray state as plain values, which is what QML can pass.
#![allow(clippy::too_many_arguments)]
use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};

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
        (true, _) => return "YQT".to_owned(),
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

/// The StatusNotifierItem backend: renders the same `Vec<MenuEntry>` the unit tests cover.
#[cfg(target_os = "linux")]
mod sni {
    use super::{Action, MenuEntry};
    use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};
    use std::sync::Arc;

    /// Called (on the tray's D-Bus task) when the user picks something; the model forwards it to the Qt thread.
    pub type OnAction = Arc<dyn Fn(Action) + Send + Sync>;

    pub struct SniTray {
        pub entries: Vec<MenuEntry>,
        pub tooltip: String,
        on_action: OnAction,
        icons: Vec<ksni::Icon>,
    }

    impl SniTray {
        pub fn new(entries: Vec<MenuEntry>, tooltip: String, on_action: OnAction) -> Self {
            Self { entries, tooltip, on_action, icons: icons() }
        }
    }

    /// RGBA (as decoded) → the ARGB32 byte order the StatusNotifierItem spec wants.
    pub fn argb_from_rgba(rgba: &[u8]) -> Vec<u8> {
        rgba.as_chunks::<4>().0.iter().flat_map(|&[r, g, b, a]| [a, r, g, b]).collect()
    }

    /// The app logo at two sizes (embedded, so the tray never depends on an icon theme).
    fn icons() -> Vec<ksni::Icon> {
        [&include_bytes!("../assets/tray-32.png")[..], &include_bytes!("../assets/tray-64.png")[..]]
            .iter()
            .filter_map(|png| image::load_from_memory_with_format(png, image::ImageFormat::Png).ok())
            .map(|img| {
                let img = img.to_rgba8();
                ksni::Icon { width: img.width() as i32, height: img.height() as i32, data: argb_from_rgba(img.as_raw()) }
            })
            .collect()
    }

    impl ksni::Tray for SniTray {
        fn id(&self) -> String {
            "yqt".into()
        }
        fn title(&self) -> String {
            "YQT".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            self.icons.clone()
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip { title: self.tooltip.clone(), ..Default::default() }
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            (self.on_action)(Action::ToggleWindow);
        }
        fn secondary_activate(&mut self, _x: i32, _y: i32) {
            (self.on_action)(Action::PlayPause);
        }
        fn menu(&self) -> Vec<MenuItem<Self>> {
            self.entries
                .iter()
                .map(|e| match e {
                    MenuEntry::Separator => MenuItem::Separator,
                    MenuEntry::Item { action, label, enabled, checked } => {
                        let action = *action;
                        let activate: Box<dyn Fn(&mut SniTray) + Send> = Box::new(move |t| (t.on_action)(action));
                        match checked {
                            Some(c) => {
                                CheckmarkItem { label: label.clone(), enabled: *enabled, checked: *c, activate, ..Default::default() }
                                    .into()
                            }
                            None => StandardItem { label: label.clone(), enabled: *enabled, activate, ..Default::default() }.into(),
                        }
                    }
                })
                .collect()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::tray::{tray_menu, TrayState};
        use ksni::Tray;
        use std::sync::Mutex;

        #[test]
        fn argb_reorders_channels() {
            assert_eq!(argb_from_rgba(&[1, 2, 3, 4, 10, 20, 30, 40]), vec![4, 1, 2, 3, 40, 10, 20, 30]);
        }

        #[test]
        fn the_logo_loads_at_both_sizes() {
            let icons = icons();
            assert_eq!(icons.iter().map(|i| i.width).collect::<Vec<_>>(), [32, 64]);
            assert!(icons.iter().all(|i| i.data.len() == (i.width * i.height * 4) as usize));
        }

        /// End to end against a stand-in for the desktop's tray host: a fake `org.kde.StatusNotifierWatcher` on the
        /// session bus. The tray must register itself, publish its icon and tooltip, and serve its menu over dbusmenu
        /// with the same entries the unit tests check. Needs a session bus, so it is opt-in:
        /// `dbus-run-session -- cargo test the_tray_registers -- --ignored`
        #[tokio::test(flavor = "multi_thread")]
        #[ignore = "needs a D-Bus session bus (run under dbus-run-session)"]
        async fn the_tray_registers_with_a_tray_host_and_serves_its_menu() {
            use ksni::TrayMethods;
            use std::time::Duration;
            use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

            struct Watcher(UnboundedSender<String>);
            #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
            impl Watcher {
                async fn register_status_notifier_item(&self, service: String) {
                    let _ = self.0.send(service);
                }
                async fn register_status_notifier_host(&self, _service: String) {}
                #[zbus(property)]
                async fn is_status_notifier_host_registered(&self) -> bool {
                    true
                }
                #[zbus(property)]
                async fn registered_status_notifier_items(&self) -> Vec<String> {
                    Vec::new()
                }
                #[zbus(property)]
                async fn protocol_version(&self) -> i32 {
                    0
                }
            }

            let (tx, mut rx) = unbounded_channel();
            let host = zbus::connection::Builder::session()
                .unwrap()
                .name("org.kde.StatusNotifierWatcher")
                .unwrap()
                .serve_at("/StatusNotifierWatcher", Watcher(tx))
                .unwrap()
                .build()
                .await
                .expect("the fake tray host");

            let state =
                TrayState { playing: true, has_track: true, signed_in: true, liked: false, window_visible: true, mini_visible: false };
            let clicked = Arc::new(Mutex::new(Vec::new()));
            let sink = clicked.clone();
            let tray = SniTray::new(tray_menu(&state), "Song — Artist".into(), Arc::new(move |a| sink.lock().unwrap().push(a)));
            let _handle = tray.spawn().await.expect("the tray registers with the host");
            let service = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("no registration within 5 s").unwrap();

            // The registered item publishes its title, tooltip and icon.
            let item = zbus::Proxy::new(&host, service.clone(), "/StatusNotifierItem", "org.kde.StatusNotifierItem").await.unwrap();
            let title: String = item.get_property("Title").await.unwrap();
            assert_eq!(title, "YQT");
            let icons: Vec<(i32, i32, Vec<u8>)> = item.get_property("IconPixmap").await.unwrap();
            assert_eq!(icons.iter().map(|i| i.0).collect::<Vec<_>>(), [32, 64], "our logo at both sizes");

            // The menu comes over dbusmenu with the labels from `tray_menu`.
            let menu = zbus::Proxy::new(&host, service, "/MenuBar", "com.canonical.dbusmenu").await.unwrap();
            let reply = menu.call_method("GetLayout", &(0i32, -1i32, Vec::<String>::new())).await.unwrap();
            let layout = String::from_utf8_lossy(reply.body().data().bytes()).into_owned();
            for label in ["Pause", "Previous", "Next", "Like", "Hide window", "Mini-player", "Quit"] {
                assert!(layout.contains(label), "the tray menu is missing {label:?}: {layout}");
            }

            // Clicking "Next" (item 3: Pause, Previous, Next, ...) and then "Quit" (item 9) reaches the app as actions.
            for id in [3i32, 9] {
                menu.call_method("Event", &(id, "clicked", zbus::zvariant::Value::from(0i32), 0u32)).await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(*clicked.lock().unwrap(), vec![Action::Next, Action::Quit]);
        }

        #[test]
        fn the_menu_mirrors_the_entries_and_activation_reports_the_action() {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let sink = seen.clone();
            let st = TrayState { playing: true, has_track: true, signed_in: true, liked: true, window_visible: true, mini_visible: false };
            let entries = tray_menu(&st);
            let mut tray = SniTray::new(entries.clone(), "x".into(), Arc::new(move |a| sink.lock().unwrap().push(a)));
            let menu = tray.menu();
            assert_eq!(menu.len(), entries.len());
            tray.activate(0, 0);
            tray.secondary_activate(0, 0);
            assert_eq!(*seen.lock().unwrap(), vec![Action::ToggleWindow, Action::PlayPause]);
        }
    }
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
        #[qproperty(bool, available)] // true once a system tray really exists
        type TrayMenuModel = super::TrayMenuModelRust;

        /// A tray entry was chosen: its stable id (`playpause`, `next`, `previous`, `like`, `window`, `mini`, `quit`).
        #[qsignal]
        fn activated(self: Pin<&mut TrayMenuModel>, action: QString);

        /// Show the tray icon (Linux: registers a StatusNotifierItem). Safe to call more than once; sets
        /// `available` only when a tray host answered.
        #[qinvokable]
        fn start(self: Pin<&mut TrayMenuModel>);

        /// Recompute the menu and tooltip for this state (and push them to the tray when it is up).
        #[qinvokable]
        fn update(
            self: Pin<&mut TrayMenuModel>,
            playing: bool,
            has_track: bool,
            signed_in: bool,
            liked: bool,
            window_visible: bool,
            mini_visible: bool,
            title: &QString,
            artist: &QString,
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

    impl cxx_qt::Threading for TrayMenuModel {}
}

#[derive(Default)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct TrayMenuModelRust {
    revision: i32,
    available: bool,
    entries: Vec<MenuEntry>,
    tooltip: String,
    started: bool,
    /// The running tray, once the D-Bus registration succeeded (shared with the task that creates it).
    #[cfg(target_os = "linux")]
    handle: Arc<Mutex<Option<Arc<ksni::Handle<sni::SniTray>>>>>,
}

impl qobject::TrayMenuModel {
    fn start(mut self: Pin<&mut Self>) {
        #[cfg(target_os = "linux")]
        {
            if self.rust().started {
                return;
            }
            self.as_mut().rust_mut().get_mut().started = true;
            let th = self.qt_thread();
            let th_ready = th.clone();
            let on_action: sni::OnAction = Arc::new(move |a| {
                let id = a.id();
                let _ = th.queue(move |mut m| m.as_mut().activated(QString::from(id)));
            });
            let tray = sni::SniTray::new(self.rust().entries.clone(), self.rust().tooltip.clone(), on_action);
            let slot = self.rust().handle.clone();
            crate::state::st().rt.spawn(async move {
                use ksni::TrayMethods;
                match tray.spawn().await {
                    Ok(handle) => {
                        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(handle));
                        let _ = th_ready.queue(|mut m| {
                            // The tray was built from the menu as it was when `start` ran; whatever changed while it
                            // was registering (a restored session, the sign-in state) is pushed now.
                            m.as_ref().push_to_tray();
                            m.as_mut().set_available(true);
                        });
                    }
                    // No StatusNotifierWatcher / no session bus: simply no tray.
                    Err(e) => tracing::info!("no system tray: {e}"),
                }
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update(
        mut self: Pin<&mut Self>,
        playing: bool,
        has_track: bool,
        signed_in: bool,
        liked: bool,
        window_visible: bool,
        mini_visible: bool,
        title: &QString,
        artist: &QString,
    ) {
        let st = TrayState { playing, has_track, signed_in, liked, window_visible, mini_visible };
        let entries = tray_menu(&st);
        let tip = tooltip_text(&title.to_string(), &artist.to_string());
        if entries == self.rust().entries && tip == self.rust().tooltip {
            return;
        }
        {
            let me = self.as_mut().rust_mut().get_mut();
            me.entries = entries;
            me.tooltip = tip;
        }
        let rev = self.revision().wrapping_add(1);
        self.as_mut().set_revision(rev);
        #[cfg(target_os = "linux")]
        self.as_ref().push_to_tray();
    }

    /// Send the current menu and tooltip to the running tray (nothing happens while there is none yet).
    #[cfg(target_os = "linux")]
    fn push_to_tray(&self) {
        let running = self.rust().handle.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(handle) = running {
            let (entries, tip) = (self.rust().entries.clone(), self.rust().tooltip.clone());
            crate::state::st().rt.spawn(async move {
                let _ = handle
                    .update(move |t| {
                        t.entries = entries;
                        t.tooltip = tip;
                    })
                    .await;
            });
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
        assert_eq!(tooltip_text("", "Band"), "YQT");
        assert_eq!(tooltip_text("", ""), "YQT");
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
