// SPDX-License-Identifier: GPL-3.0-only
//! `Nav`: what the navigation drawer lists, plus the small text helpers the rich pages need, so QML holds
//! presentation only.
//!
//! QML: `Nav.entries` is a JSON array of `{name, label, icon, group, needsTrack, badge}` in display order (the
//! drawer draws a separator between groups). `name` is what `root.go(name)` understands, with two extras the drawer
//! maps itself: `nowplaying` → `root.showNowPlaying("queue")` and `fullscreen` → `root.toggleFullScreen()`.
//! `badge` names a live count the drawer fills in (`queue`). Invokables: `greeting()`, `excerpt(text, max)`,
//! `isSafeLink(url)`.
use crate::pages::{common, home};
use crate::security;
use cxx_qt_lib::QString;
use serde::Serialize;

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
        #[qml_singleton]
        #[qproperty(QString, entries)]
        type Nav = super::NavRust;

        /// "Good morning" etc. for the local time.
        #[qinvokable]
        fn greeting(self: &Nav) -> QString;
        /// `text` cut to at most `max` characters on a word boundary, with an ellipsis (unchanged when short enough).
        #[qinvokable]
        fn excerpt(self: &Nav, text: &QString, max: i32) -> QString;
        /// True for links that may be handed to `Qt.openUrlExternally`: `https` only.
        #[qinvokable]
        fn is_safe_link(self: &Nav, url: &QString) -> bool;
    }
}

pub struct NavRust {
    entries: QString,
}

impl Default for NavRust {
    fn default() -> Self {
        Self { entries: QString::from(&serde_json::to_string(&entries()).unwrap_or_else(|_| "[]".into())) }
    }
}

impl qobject::Nav {
    fn greeting(&self) -> QString {
        QString::from(home::greeting_now())
    }
    fn excerpt(&self, text: &QString, max: i32) -> QString {
        QString::from(&common::excerpt(&text.to_string(), usize::try_from(max).unwrap_or(0).max(1)))
    }
    fn is_safe_link(&self, url: &QString) -> bool {
        security::is_safe_external_link(&url.to_string())
    }
}

/// One drawer row.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub group: &'static str,
    /// Only meaningful while something is loaded in the player.
    pub needs_track: bool,
    /// Live count shown as a badge ("queue"), or "".
    pub badge: &'static str,
}

const fn e(name: &'static str, label: &'static str, icon: &'static str, group: &'static str) -> Entry {
    Entry { name, label, icon, group, needs_track: false, badge: "" }
}

/// The drawer, in display order: browse | playback | app.
pub fn entries() -> Vec<Entry> {
    vec![
        e("home", "Home", "go-home", "browse"),
        e("explore", "Explore", "compass", "browse"),
        e("library", "Library", "folder-music", "browse"),
        e("downloads", "Downloads", "folder-download", "browse"),
        Entry { needs_track: true, ..e("nowplaying", "Now playing", "media-playback-start", "playback") },
        Entry { badge: "queue", ..e("queue", "Queue", "view-media-playlist", "playback") },
        e("settings", "Settings", "configure", "app"),
        e("fullscreen", "Full screen", "view-fullscreen", "app"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_are_unique_and_groups_are_contiguous() {
        let list = entries();
        let names: HashSet<_> = list.iter().map(|e| e.name).collect();
        assert_eq!(names.len(), list.len());
        let mut seen: Vec<&str> = Vec::new();
        for e in &list {
            if seen.last() != Some(&e.group) {
                assert!(!seen.contains(&e.group), "group {} is split", e.group);
                seen.push(e.group);
            }
        }
        assert_eq!(seen, ["browse", "playback", "app"]);
    }

    #[test]
    fn the_first_entry_is_home_and_every_row_has_text_and_icon() {
        let list = entries();
        assert_eq!(list[0].name, "home");
        assert!(list.iter().all(|e| !e.label.is_empty() && !e.icon.is_empty()));
    }

    #[test]
    fn entries_serialise_in_the_shape_qml_reads() {
        let v = serde_json::to_value(entries()).unwrap();
        assert_eq!(v[0]["name"], "home");
        let by_name = |n: &str| v.as_array().unwrap().iter().find(|e| e["name"] == n).unwrap().clone();
        assert_eq!(by_name("nowplaying")["needsTrack"], true);
        assert_eq!(by_name("queue")["badge"], "queue");
        assert_eq!(by_name("downloads")["group"], "browse");
        assert!(v[0].get("needs_track").is_none());
    }
}
