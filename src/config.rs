// SPDX-License-Identifier: GPL-3.0-only
//! User configuration: `<config dir>/ytm/config.toml`.
//!
//! Every tunable that used to be a literal lives here. The file is written with the defaults on
//! first run; it is parsed leniently, so a missing key, a wrongly-typed value or even a syntax
//! error never stops the app – the affected value (or file) just falls back to its default.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub network: Network,
    pub playback: Playback,
    pub library: Library,
    pub lyrics: Lyrics,
    pub youtube: Youtube,
    pub ui: Ui,
    pub integration: Integration,
    pub audio: Audio,
    pub downloads: Downloads,
    pub appearance: Appearance,
    pub keys: Keys,
    pub reporting: Reporting,
    pub window: Window,
    pub security: Security,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Network {
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
    /// How often a request that failed in a transient way (network blip, unreadable page) is tried again.
    pub retries: u32,
    /// Max silence on a stream connection before the proxy gives up.
    pub stream_read_timeout_secs: u64,
    /// googlevideo refuses unbounded ranges, so audio is fetched in chunks of this size.
    pub proxy_chunk_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Playback {
    pub stream_cache_minutes: u64,
    pub visitor_cache_minutes: u64,
    /// Resolve the next queue entry in the background so skips are instant.
    pub prefetch_next: bool,
    /// When the queue runs out, keep going with YouTube's radio for the last song.
    pub autoplay_radio: bool,
    pub session_save_secs: u64,
    /// "Previous" restarts the song instead of skipping back when it is past this point.
    pub restart_threshold_ms: u64,
    pub default_volume: f64,
    /// Known-good video used by Settings → Check streams.
    pub check_video_id: String,
    /// Where the stream check reads from; must be past the ~768 KB cutoff of walled URLs.
    pub check_offset_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub home_items: usize,
    pub history_items: usize,
    pub radio_items: usize,
    pub search_songs: usize,
    pub search_artists: usize,
    pub search_albums: usize,
    pub search_playlists: usize,
    pub suggestions: usize,
    /// How many liked songs to read in the background (to show the heart state everywhere). Songs you browse
    /// in Liked music are remembered beyond this.
    pub liked_limit: usize,
    /// Keep Home / Explore / artist / album pages for this long, so going back to them is instant (0 = off).
    pub page_cache_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Lyrics {
    /// Look lyrics up on third-party providers (they receive title, artist, duration). Off = YouTube Music's plain lyrics only.
    pub enabled: bool,
    /// Providers to ask, in order of preference: "lrclib", "netease", "youtube". Unknown names are ignored.
    /// All enabled providers are queried at once; a synced result always beats a plain one.
    pub providers: Vec<String>,
    pub lrclib_url: String,
    pub netease_url: String,
    /// Records with fewer lyric lines than this are treated as junk.
    pub min_lines: usize,
    /// Prefer line-by-line synced lyrics over plain text, even when the plain text is longer.
    pub prefer_synced: bool,
    /// Multiplier for the lyric text size (1.0 = default; the text also scales with the window).
    pub size_scale: f64,
    /// Fill the current line left-to-right between timestamps.
    pub karaoke: bool,
    /// Show animated dots during long instrumental gaps.
    pub gap_dots: bool,
    /// Tint the current line with the cover's accent colour.
    pub tint_with_accent: bool,
    /// Keep looked-up lyrics on disk this long (0 = don't cache).
    pub cache_days: u32,
}

/// Client identities. These go stale when YouTube changes things – edit here instead of rebuilding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Youtube {
    pub language: String,
    pub music_origin: String,
    pub web_remix_client_version: String,
    pub visionos_client_version: String,
    pub visionos_os_version: String,
    pub visionos_user_agent: String,
    pub innertube_api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    /// Qt Quick Controls style; empty = `org.kde.desktop` on every OS (Fusion only if that style is missing), so the app looks the same everywhere.
    pub style: String,
    pub suggest_debounce_ms: u64,
    /// Highlight the next lyric line this much early.
    pub lyrics_lead_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Integration {
    /// Publish playback to the desktop (MPRIS on Linux).
    pub mpris: bool,
}

/// Audio engine and equalizer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// "rust" (symphonia + opus-rs + cpal, no FFmpeg) or "qt" (Qt Multimedia, only if built with the `qt-media` feature).
    pub engine: String,
    pub eq_enabled: bool,
    /// Name of the active preset ("flat", "bass_boost", … or a user preset); informational, the bands below are what applies.
    pub eq_preset: String,
    /// Preamp in dB, applied before the bands (negative values leave headroom for boosted bands).
    pub eq_preamp_db: f64,
    /// Gain in dB for the ten bands 31 Hz, 62, 125, 250, 500, 1 k, 2 k, 4 k, 8 k, 16 k.
    pub eq_bands_db: Vec<f64>,
    /// Start the next track without a gap.
    pub gapless: bool,
    /// Overlap the end of a track with the start of the next one (0 = off).
    pub crossfade_ms: u64,
    /// Even out loudness between tracks using YouTube's loudness data.
    pub normalize: bool,
    /// Output device name; empty = the system default.
    pub output_device: String,
}

/// Offline downloads. Smart downloads are opt-in, like in YouTube Music's own apps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Downloads {
    /// Where files are stored; empty = `<data dir>/downloads`.
    pub dir: String,
    /// "best", "normal" or "data_saver".
    pub quality: String,
    /// Automatically keep chosen songs downloaded (the rules below).
    pub smart_enabled: bool,
    pub keep_liked: bool,
    /// Keep this many recently played songs.
    pub recent_n: u32,
    /// Keep this many upcoming queue entries ready.
    pub queue_ahead_n: u32,
    /// Storage budget for downloads; the oldest unpinned songs are removed first when it is exceeded. Raise it freely.
    pub cap_gb: f64,
    /// Remove songs not played for this many days (0 = never). Pinned playlists/albums are exempt.
    pub remove_unplayed_days: u32,
    pub pause_on_metered: bool,
    pub pause_on_low_battery: bool,
    pub low_battery_percent: u32,
    pub max_parallel: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// "system", "light", "dark", "breeze-light" or "breeze-dark".
    pub theme: String,
    /// Corner radius of large covers, as a percentage of their size ("slightly rounded").
    pub cover_radius_percent: f64,
    /// Tint pages, lyrics and highlights with the colour of the current cover.
    pub accent_from_cover: bool,
    /// Turn off decorative animation (lyric blur/fill, shelf transitions).
    pub reduce_motion: bool,
    /// "comfortable" or "compact".
    pub density: String,
}

/// Keyboard shortcuts, in Qt's key-sequence syntax. Every value must be unique.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub play_pause: String,
    pub next: String,
    pub previous: String,
    pub search: String,
    pub fullscreen: String,
    pub mini_player: String,
    pub now_playing: String,
    pub lyrics: String,
    pub volume_up: String,
    pub volume_down: String,
    pub seek_forward: String,
    pub seek_back: String,
}

/// Tell YouTube what you played, so it lands in your YouTube Music history and feeds recommendations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Reporting {
    /// Only does anything while signed in.
    pub enabled: bool,
    /// A play counts after this many seconds of listening; songs skipped earlier are not reported.
    pub min_play_secs: u64,
    /// Also send periodic watch-time pings (experimental).
    pub watchtime: bool,
    /// Tries for a report that fails; it is then dropped. Reporting never blocks playback.
    pub retries: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Window {
    /// Closing the main window keeps the music playing from the tray.
    pub close_to_tray: bool,
    pub mini_always_on_top: bool,
    pub remember_geometry: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Security {
    /// Require an unguessable token in the local stream proxy's URLs.
    pub proxy_token: bool,
    /// Keep RustyPipe's failure reports (they contain response bodies) this many days; 0 = keep none.
    pub report_retention_days: u32,
    /// Strip cookies, tokens and signed URLs from logs.
    pub redact_logs: bool,
    /// Allow plain-http URLs for configurable endpoints (lyrics providers…). Loopback is always allowed.
    pub allow_insecure_urls: bool,
}

impl Default for Network {
    fn default() -> Self {
        Self { connect_timeout_secs: 10, request_timeout_secs: 20, retries: 1, stream_read_timeout_secs: 30, proxy_chunk_bytes: 4_000_000 }
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            // Qt stays the default until the Rust engine has passed its tests; the engine work flips this.
            engine: "qt".into(),
            eq_enabled: false,
            eq_preset: "flat".into(),
            eq_preamp_db: 0.0,
            eq_bands_db: vec![0.0; 10],
            gapless: true,
            crossfade_ms: 0,
            normalize: true,
            output_device: String::new(),
        }
    }
}

impl Default for Downloads {
    fn default() -> Self {
        Self {
            dir: String::new(),
            quality: "normal".into(),
            smart_enabled: false,
            keep_liked: true,
            recent_n: 50,
            queue_ahead_n: 5,
            cap_gb: 4.0,
            remove_unplayed_days: 0,
            pause_on_metered: true,
            pause_on_low_battery: true,
            low_battery_percent: 20,
            max_parallel: 2,
        }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            cover_radius_percent: 5.0,
            accent_from_cover: true,
            reduce_motion: false,
            density: "comfortable".into(),
        }
    }
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            play_pause: "Space".into(),
            next: "Ctrl+Right".into(),
            previous: "Ctrl+Left".into(),
            search: "Ctrl+F".into(),
            fullscreen: "F11".into(),
            mini_player: "Ctrl+M".into(),
            now_playing: "Ctrl+P".into(),
            lyrics: "Ctrl+L".into(),
            volume_up: "Ctrl+Up".into(),
            volume_down: "Ctrl+Down".into(),
            seek_forward: "Shift+Right".into(),
            seek_back: "Shift+Left".into(),
        }
    }
}

impl Default for Reporting {
    fn default() -> Self {
        Self { enabled: true, min_play_secs: 30, watchtime: false, retries: 3 }
    }
}

impl Default for Window {
    fn default() -> Self {
        Self { close_to_tray: false, mini_always_on_top: true, remember_geometry: true }
    }
}

impl Default for Security {
    fn default() -> Self {
        Self { proxy_token: true, report_retention_days: 7, redact_logs: true, allow_insecure_urls: false }
    }
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            stream_cache_minutes: 30,
            visitor_cache_minutes: 15,
            prefetch_next: true,
            autoplay_radio: true,
            session_save_secs: 5,
            restart_threshold_ms: 3000,
            default_volume: 0.8,
            check_video_id: "dQw4w9WgXcQ".into(),
            check_offset_bytes: 3_000_000,
        }
    }
}

impl Default for Library {
    fn default() -> Self {
        Self {
            home_items: 12,
            history_items: 16,
            radio_items: 12,
            search_songs: 8,
            search_artists: 3,
            search_albums: 6,
            search_playlists: 4,
            suggestions: 8,
            liked_limit: 1000,
            page_cache_secs: 300,
        }
    }
}

impl Default for Lyrics {
    fn default() -> Self {
        Self {
            enabled: true,
            providers: vec!["lrclib".into(), "netease".into(), "youtube".into()],
            lrclib_url: "https://lrclib.net".into(),
            netease_url: "https://music.163.com".into(),
            min_lines: 4,
            prefer_synced: true,
            size_scale: 1.0,
            karaoke: true,
            gap_dots: true,
            tint_with_accent: true,
            cache_days: 30,
        }
    }
}

impl Default for Youtube {
    fn default() -> Self {
        Self {
            language: "en".into(),
            music_origin: "https://music.youtube.com".into(),
            web_remix_client_version: "1.20250101.01.00".into(),
            visionos_client_version: "1.02".into(),
            visionos_os_version: "26.5.23O471".into(),
            visionos_user_agent:
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15"
                    .into(),
            innertube_api_key: "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8".into(),
        }
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self { style: String::new(), suggest_debounce_ms: 220, lyrics_lead_ms: 250 }
    }
}

impl Default for Integration {
    fn default() -> Self {
        Self { mpris: true }
    }
}

const HEADER: &str = "# ytm configuration.\n# Delete a line (or the whole file) to restore its default. Invalid values are ignored with a warning on stderr.\n\n";

fn kind(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::String(_) => "string",
        toml::Value::Integer(_) | toml::Value::Float(_) => "number",
        toml::Value::Boolean(_) => "boolean",
        toml::Value::Array(_) => "array",
        toml::Value::Table(_) => "table",
        toml::Value::Datetime(_) => "datetime",
    }
}

/// Overlay `user` onto `base`, keeping the default wherever the user's value has the wrong type.
fn merge(base: &mut toml::Table, user: toml::Table, path: &str, warnings: &mut Vec<String>) {
    for (key, value) in user {
        let here = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
        match base.get_mut(&key) {
            None => warnings.push(format!("unknown setting `{here}` ignored")),
            Some(toml::Value::Table(bt)) => match value {
                toml::Value::Table(ut) => merge(bt, ut, &here, warnings),
                _ => warnings.push(format!("`{here}` must be a table; using defaults")),
            },
            Some(slot) if kind(slot) == kind(&value) => *slot = value,
            Some(slot) => warnings.push(format!("`{here}` must be a {}; using the default", kind(slot))),
        }
    }
}

impl Config {
    /// Parse a config file's text. Never fails: returns the effective config plus human-readable warnings.
    pub fn parse(text: &str) -> (Config, Vec<String>) {
        let mut warnings = Vec::new();
        let mut base = toml::Table::try_from(Config::default()).expect("defaults serialize");
        match text.parse::<toml::Table>() {
            Ok(user) => merge(&mut base, user, "", &mut warnings),
            Err(e) => warnings.push(format!("syntax error, using all defaults: {}", e.message())),
        }
        let cfg = base.try_into().unwrap_or_else(|e: toml::de::Error| {
            warnings.push(format!("invalid configuration, using all defaults: {e}"));
            Config::default()
        });
        (cfg, warnings)
    }

    pub fn template() -> String {
        format!("{HEADER}{}", toml::to_string_pretty(&Config::default()).expect("defaults serialize"))
    }
}

pub fn path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("ytm").join("config.toml")
}

/// The process-wide configuration (read once at first use).
pub fn get() -> &'static Config {
    static C: OnceLock<Config> = OnceLock::new();
    C.get_or_init(|| {
        let path = path();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                crate::state::tighten(&path);
                if let Some(dir) = path.parent() {
                    crate::state::tighten(dir);
                }
                let (cfg, warnings) = Config::parse(&text);
                for w in warnings {
                    eprintln!("ytm: {}: {w}", path.display());
                }
                cfg
            }
            Err(_) => {
                if let Some(dir) = path.parent() {
                    let _ = crate::state::ensure_private_dir(dir);
                }
                let _ = crate::state::write_private(&path, Config::template().as_bytes());
                Config::default()
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_all_defaults() {
        let (c, w) = Config::parse("");
        assert!(w.is_empty());
        assert_eq!(c.network.proxy_chunk_bytes, 4_000_000);
        assert_eq!(c.playback.stream_cache_minutes, 30);
    }

    #[test]
    fn template_round_trips_to_defaults() {
        let (c, w) = Config::parse(&Config::template());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(toml::to_string(&c).unwrap(), toml::to_string(&Config::default()).unwrap());
    }

    #[test]
    fn partial_file_overrides_only_what_it_names() {
        let (c, w) = Config::parse("[network]\nproxy_chunk_bytes = 1000\n[lyrics]\nenabled = false\n");
        assert!(w.is_empty());
        assert_eq!(c.network.proxy_chunk_bytes, 1000);
        assert_eq!(c.network.connect_timeout_secs, 10); // sibling untouched
        assert!(!c.lyrics.enabled);
        assert_eq!(c.lyrics.lrclib_url, "https://lrclib.net");
    }

    #[test]
    fn wrong_type_falls_back_per_field() {
        let (c, w) = Config::parse("[network]\nconnect_timeout_secs = \"fast\"\nproxy_chunk_bytes = 2000\n");
        assert_eq!(c.network.connect_timeout_secs, 10);
        assert_eq!(c.network.proxy_chunk_bytes, 2000); // the valid neighbour still applies
        assert!(w.iter().any(|m| m.contains("network.connect_timeout_secs")), "{w:?}");
    }

    #[test]
    fn syntax_error_falls_back_to_defaults() {
        let (c, w) = Config::parse("[network\nthis is not toml");
        assert_eq!(c.network.connect_timeout_secs, 10);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("syntax error"));
    }

    #[test]
    fn unknown_keys_warn_but_do_not_break() {
        let (c, w) = Config::parse("future_option = 1\n[network]\nbogus = true\n");
        assert_eq!(c.network.proxy_chunk_bytes, 4_000_000);
        assert_eq!(w.len(), 2);
    }

    #[test]
    fn integers_are_accepted_for_floats() {
        let (c, w) = Config::parse("[playback]\ndefault_volume = 1\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.playback.default_volume, 1.0);
    }

    // ---- the sections added for audio / downloads / appearance / keys / reporting / window / security ----

    #[test]
    fn new_sections_have_the_decided_defaults() {
        let c = Config::default();
        // downloads: opt-in smart downloads, 4 GB budget (the user's choice)
        assert!(!c.downloads.smart_enabled);
        assert_eq!(c.downloads.cap_gb, 4.0);
        assert_eq!(c.downloads.quality, "normal");
        assert!(c.downloads.pause_on_metered && c.downloads.pause_on_low_battery);
        // audio: ten flat bands, EQ off, Qt engine until the Rust one is proven
        assert_eq!(c.audio.eq_bands_db, vec![0.0; 10]);
        assert!(!c.audio.eq_enabled);
        assert_eq!(c.audio.engine, "qt");
        // lyrics: three providers, synced preferred
        assert_eq!(c.lyrics.providers, ["lrclib", "netease", "youtube"]);
        assert!(c.lyrics.prefer_synced && c.lyrics.karaoke && c.lyrics.tint_with_accent);
        // reporting is on (the user asked for it) but only acts when signed in
        assert!(c.reporting.enabled);
        assert_eq!(c.reporting.min_play_secs, 30);
        // window / security
        assert!(!c.window.close_to_tray);
        assert!(c.security.proxy_token && c.security.redact_logs && !c.security.allow_insecure_urls);
    }

    #[test]
    fn shortcuts_are_unique() {
        let k = Config::default().keys;
        let all = [
            &k.play_pause,
            &k.next,
            &k.previous,
            &k.search,
            &k.fullscreen,
            &k.mini_player,
            &k.now_playing,
            &k.lyrics,
            &k.volume_up,
            &k.volume_down,
            &k.seek_forward,
            &k.seek_back,
        ];
        let unique: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "two actions share a shortcut: {all:?}");
    }

    #[test]
    fn eq_bands_and_provider_lists_can_be_overridden() {
        let (c, w) = Config::parse(
            "[audio]\neq_enabled = true\neq_bands_db = [1, 2.5, 0, -3, 0, 0, 0, 0, 0, 4]\n[lyrics]\nproviders = [\"netease\", \"lrclib\"]\n",
        );
        assert!(w.is_empty(), "{w:?}");
        assert!(c.audio.eq_enabled);
        assert_eq!(c.audio.eq_bands_db.len(), 10);
        assert_eq!(c.audio.eq_bands_db[1], 2.5);
        assert_eq!(c.lyrics.providers, ["netease", "lrclib"]);
    }

    #[test]
    fn older_config_files_still_parse_and_get_the_new_defaults() {
        // what versions before these sections wrote
        let old = "[network]\nconnect_timeout_secs = 12\n[lyrics]\nenabled = true\nlrclib_url = \"https://lrclib.net\"\nmin_lines = 4\n[ui]\nsuggest_debounce_ms = 220\n";
        let (c, w) = Config::parse(old);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.network.connect_timeout_secs, 12);
        assert_eq!(c.downloads.cap_gb, 4.0);
        assert_eq!(c.lyrics.providers.len(), 3);
    }

    #[test]
    fn a_wrongly_typed_download_cap_falls_back_without_touching_its_neighbours() {
        let (c, w) = Config::parse("[downloads]\ncap_gb = \"lots\"\nquality = \"best\"\n");
        assert_eq!(c.downloads.cap_gb, 4.0);
        assert_eq!(c.downloads.quality, "best");
        assert!(w.iter().any(|m| m.contains("downloads.cap_gb")), "{w:?}");
    }
}
