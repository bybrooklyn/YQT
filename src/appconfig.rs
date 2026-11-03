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
        // appearance
        #[qproperty(f64, cover_radius_percent)]
        #[qproperty(bool, accent_from_cover)]
        #[qproperty(bool, reduce_motion)]
        // lyrics
        #[qproperty(f64, lyrics_size_scale)]
        #[qproperty(bool, lyrics_karaoke)]
        #[qproperty(bool, lyrics_gap_dots)]
        #[qproperty(bool, lyrics_tint)]
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
    cover_radius_percent: f64,
    accent_from_cover: bool,
    reduce_motion: bool,
    lyrics_size_scale: f64,
    lyrics_karaoke: bool,
    lyrics_gap_dots: bool,
    lyrics_tint: bool,
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
            cover_radius_percent: c.appearance.cover_radius_percent.clamp(0.0, 50.0),
            accent_from_cover: c.appearance.accent_from_cover,
            reduce_motion: c.appearance.reduce_motion,
            lyrics_size_scale: c.lyrics.size_scale.clamp(0.5, 3.0),
            lyrics_karaoke: c.lyrics.karaoke,
            lyrics_gap_dots: c.lyrics.gap_dots,
            lyrics_tint: c.lyrics.tint_with_accent,
        }
    }
}
