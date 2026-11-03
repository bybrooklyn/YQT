// SPDX-License-Identifier: GPL-3.0-only
//! `Accent`: the dominant colour of a cover image, for tinting pages, lyrics and highlights.
//!
//! This is the **contract + a stub**: QML code can use it now (`Accent { source: Player.cover }`), and the
//! downloads/cover-cache workstream replaces the stub body with the real extraction (decode the cached cover,
//! pick a pleasant saturated dominant colour) without changing this surface.
//!
//! QML usage:
//! ```qml
//! Accent { id: accent; source: Player.cover }
//! color: accent.ready ? accent.color : Kirigami.Theme.highlightColor
//! ```
use core::pin::Pin;
use cxx_qt_lib::QString;

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
        /// Image URL to take the colour from (set by QML).
        #[qproperty(QString, source)]
        /// `#rrggbb` once known, empty before.
        #[qproperty(QString, color)]
        /// True when `color` is the real accent of `source`.
        #[qproperty(bool, ready)]
        type Accent = super::AccentRust;
    }
}

#[derive(Default)]
pub struct AccentRust {
    source: QString,
    color: QString,
    ready: bool,
}

impl qobject::Accent {
    /// Hook for the real implementation: called whenever `source` changes. The stub only resets the state.
    #[allow(dead_code)]
    pub fn reset(mut self: Pin<&mut Self>) {
        self.as_mut().set_color(QString::default());
        self.as_mut().set_ready(false);
    }
}

/// Parse `#rrggbb` into 0–255 channels. Shared by the colour code and its tests.
#[allow(dead_code)]
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((c(0)?, c(2)?, c(4)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colours_parse() {
        assert_eq!(parse_hex("#a1b2c3"), Some((0xa1, 0xb2, 0xc3)));
        assert_eq!(parse_hex("a1b2c3"), None);
        assert_eq!(parse_hex("#abc"), None);
        assert_eq!(parse_hex("#zzzzzz"), None);
        assert_eq!(parse_hex("#ééé"), None);
    }
}
