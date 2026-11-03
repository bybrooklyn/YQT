// SPDX-License-Identifier: GPL-3.0-only
//! The handful of settings the QML side needs, read from `config.toml`.
use crate::config;

#[cxx_qt::bridge]
pub mod qobject {
    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(i32, session_save_ms)]
        #[qproperty(i32, restart_threshold_ms)]
        #[qproperty(i32, suggest_debounce_ms)]
        #[qproperty(i32, lyrics_lead_ms)]
        #[qproperty(f64, default_volume)]
        #[qproperty(i32, page_cache_ms)]
        type AppConfig = super::AppConfigRust;
    }
}

pub struct AppConfigRust {
    session_save_ms: i32,
    restart_threshold_ms: i32,
    suggest_debounce_ms: i32,
    lyrics_lead_ms: i32,
    default_volume: f64,
    page_cache_ms: i32,
}

impl Default for AppConfigRust {
    fn default() -> Self {
        let c = config::get();
        let ms = |v: u64| i32::try_from(v).unwrap_or(i32::MAX);
        Self {
            session_save_ms: ms(c.playback.session_save_secs.saturating_mul(1000)).max(1000),
            restart_threshold_ms: ms(c.playback.restart_threshold_ms),
            suggest_debounce_ms: ms(c.ui.suggest_debounce_ms),
            lyrics_lead_ms: ms(c.ui.lyrics_lead_ms),
            default_volume: c.playback.default_volume.clamp(0.0, 1.0),
            page_cache_ms: ms(c.library.page_cache_secs.saturating_mul(1000)),
        }
    }
}
