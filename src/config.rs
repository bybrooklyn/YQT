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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Network {
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
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
    /// Look lyrics up on LRCLIB (sends title, artist, duration). Off = YouTube Music's plain lyrics only.
    pub enabled: bool,
    pub lrclib_url: String,
    /// Records with fewer lyric lines than this are treated as junk.
    pub min_lines: usize,
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
    /// Qt Quick Controls style; empty = org.kde.desktop on Linux, Fusion elsewhere.
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

impl Default for Network {
    fn default() -> Self {
        Self { connect_timeout_secs: 10, request_timeout_secs: 20, stream_read_timeout_secs: 30, proxy_chunk_bytes: 4_000_000 }
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
        Self { enabled: true, lrclib_url: "https://lrclib.net".into(), min_lines: 4 }
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
                let (cfg, warnings) = Config::parse(&text);
                for w in warnings {
                    eprintln!("ytm: {}: {w}", path.display());
                }
                cfg
            }
            Err(_) => {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&path, Config::template());
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
}
