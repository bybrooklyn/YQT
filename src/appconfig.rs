// SPDX-License-Identifier: GPL-3.0-only
//! The handful of settings the QML side needs, read from `config.toml`.
//!
//! The Settings backend calls [`push`] after every successful change, so these properties (and every QML
//! binding on them) update live.
use crate::config::{self, Config};
use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;
use std::sync::Mutex;

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
        #[qproperty(QString, theme)]
        #[qproperty(QString, density)]
        // window / tray / mini-player
        #[qproperty(bool, close_to_tray)]
        #[qproperty(bool, mini_always_on_top)]
        #[qproperty(bool, remember_geometry)]
        #[qproperty(QString, key_mini_player)]
        // keyboard shortcuts as JSON {"play_pause": "Space", ...}
        #[qproperty(QString, shortcuts_json)]
        // lyrics
        #[qproperty(f64, lyrics_size_scale)]
        #[qproperty(bool, lyrics_karaoke)]
        #[qproperty(bool, lyrics_gap_dots)]
        #[qproperty(bool, lyrics_tint)]
        // audio engine ("rust" or "qt"), read-only start values; the EQ's own state lives in the `Equalizer` singleton
        #[qproperty(QString, audio_engine)]
        #[qproperty(i32, audio_crossfade_ms)]
        #[qproperty(bool, audio_normalize)]
        #[qproperty(bool, audio_gapless)]
        #[qproperty(QString, audio_output_device)]
        type AppConfig = super::AppConfigRust;
    }

    impl cxx_qt::Threading for AppConfig {}
    impl cxx_qt::Initialize for AppConfig {}
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
    theme: QString,
    density: QString,
    close_to_tray: bool,
    mini_always_on_top: bool,
    remember_geometry: bool,
    key_mini_player: QString,
    shortcuts_json: QString,
    lyrics_size_scale: f64,
    lyrics_karaoke: bool,
    lyrics_gap_dots: bool,
    lyrics_tint: bool,
    audio_engine: QString,
    audio_crossfade_ms: i32,
    audio_normalize: bool,
    audio_gapless: bool,
    audio_output_device: QString,
}

fn ms(v: u64) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

fn shortcuts_json(c: &Config) -> QString {
    QString::from(&serde_json::to_string(&c.keys).unwrap_or_else(|_| "{}".into()))
}

impl AppConfigRust {
    fn from_config(c: &Config) -> Self {
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
            theme: QString::from(&c.appearance.theme),
            density: QString::from(&c.appearance.density),
            close_to_tray: c.window.close_to_tray,
            mini_always_on_top: c.window.mini_always_on_top,
            remember_geometry: c.window.remember_geometry,
            key_mini_player: QString::from(c.keys.mini_player.as_str()),
            shortcuts_json: shortcuts_json(c),
            lyrics_size_scale: c.lyrics.size_scale.clamp(0.5, 3.0),
            lyrics_karaoke: c.lyrics.karaoke,
            lyrics_gap_dots: c.lyrics.gap_dots,
            lyrics_tint: c.lyrics.tint_with_accent,
            audio_engine: QString::from(c.audio.engine.as_str()),
            audio_crossfade_ms: ms(c.audio.crossfade_ms).min(12_000),
            audio_normalize: c.audio.normalize,
            audio_gapless: c.audio.gapless,
            audio_output_device: QString::from(c.audio.output_device.as_str()),
        }
    }
}

impl Default for AppConfigRust {
    fn default() -> Self {
        Self::from_config(&config::current())
    }
}

static HANDLE: Mutex<Option<cxx_qt::CxxQtThread<qobject::AppConfig>>> = Mutex::new(None);

impl cxx_qt::Initialize for qobject::AppConfig {
    fn initialize(self: Pin<&mut Self>) {
        if let Ok(mut h) = HANDLE.lock() {
            *h = Some(self.qt_thread());
        }
    }
}

impl qobject::AppConfig {
    fn load(mut self: Pin<&mut Self>, c: &Config) {
        let n = AppConfigRust::from_config(c);
        self.as_mut().set_session_save_ms(n.session_save_ms);
        self.as_mut().set_restart_threshold_ms(n.restart_threshold_ms);
        self.as_mut().set_suggest_debounce_ms(n.suggest_debounce_ms);
        self.as_mut().set_lyrics_lead_ms(n.lyrics_lead_ms);
        self.as_mut().set_default_volume(n.default_volume);
        self.as_mut().set_page_cache_ms(n.page_cache_ms);
        self.as_mut().set_cover_radius_percent(n.cover_radius_percent);
        self.as_mut().set_accent_from_cover(n.accent_from_cover);
        self.as_mut().set_reduce_motion(n.reduce_motion);
        self.as_mut().set_theme(n.theme);
        self.as_mut().set_density(n.density);
        self.as_mut().set_close_to_tray(n.close_to_tray);
        self.as_mut().set_mini_always_on_top(n.mini_always_on_top);
        self.as_mut().set_remember_geometry(n.remember_geometry);
        self.as_mut().set_key_mini_player(n.key_mini_player);
        self.as_mut().set_shortcuts_json(n.shortcuts_json);
        self.as_mut().set_lyrics_size_scale(n.lyrics_size_scale);
        self.as_mut().set_lyrics_karaoke(n.lyrics_karaoke);
        self.as_mut().set_lyrics_gap_dots(n.lyrics_gap_dots);
        self.as_mut().set_lyrics_tint(n.lyrics_tint);
        self.as_mut().set_audio_engine(n.audio_engine);
        self.as_mut().set_audio_crossfade_ms(n.audio_crossfade_ms);
        self.as_mut().set_audio_normalize(n.audio_normalize);
        self.as_mut().set_audio_gapless(n.audio_gapless);
        self.as_mut().set_audio_output_device(n.audio_output_device);
    }
}

/// Make the `AppConfig` singleton reflect `cfg` (queued onto the Qt thread; a no-op before QML created the singleton).
pub fn push(cfg: Config) {
    let Ok(guard) = HANDLE.lock() else { return };
    if let Some(h) = guard.as_ref() {
        let _ = h.queue(move |obj| obj.load(&cfg));
    }
}
