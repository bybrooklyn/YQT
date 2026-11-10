// SPDX-License-Identifier: GPL-3.0-only
//! Static description of every setting: label, help text, type, range, choices, whether a change
//! needs a restart. The Settings page is generated from this table, so QML holds no setting logic.
//! A test walks `Config::default()` and fails if a path has no entry here.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int,
    Float,
    Text,
    /// One of `choices`.
    Choice,
    /// A service URL, validated by `security::validate_endpoint`.
    Url,
    /// A directory that must be writable (empty = default location).
    Dir,
    /// A Qt key sequence.
    Shortcut,
    /// A list of strings, each one of `choices`.
    TextList,
    FloatList,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Bool => "bool",
            Kind::Int => "int",
            Kind::Float => "float",
            Kind::Text => "text",
            Kind::Choice => "choice",
            Kind::Url => "url",
            Kind::Dir => "dir",
            Kind::Shortcut => "shortcut",
            Kind::TextList => "textlist",
            Kind::FloatList => "floatlist",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Meta {
    pub path: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: Kind,
    pub min: f64,
    pub max: f64,
    /// Upper end of the slider in the UI; typed values may go up to `max`.
    pub soft_max: f64,
    pub step: f64,
    pub unit: &'static str,
    /// (value, label)
    pub choices: &'static [(&'static str, &'static str)],
    /// Only takes effect after the app is restarted.
    pub restart: bool,
    /// Which card of the page shows it.
    pub group: &'static str,
    /// Not listed on the page (edited by a dedicated panel, e.g. the equalizer).
    pub hidden: bool,
    /// Tucked behind "Advanced" in the UI.
    pub advanced: bool,
    pub slider: bool,
    /// Shown prominently whenever the value differs from the default.
    pub warn: &'static str,
}

impl Meta {
    const fn new(path: &'static str, group: &'static str, kind: Kind, label: &'static str, description: &'static str) -> Meta {
        Meta {
            path,
            label,
            description,
            kind,
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
            soft_max: f64::INFINITY,
            step: 1.0,
            unit: "",
            choices: &[],
            restart: false,
            group,
            hidden: false,
            advanced: false,
            slider: false,
            warn: "",
        }
    }
    const fn range(mut self, min: f64, max: f64) -> Meta {
        self.min = min;
        self.max = max;
        if self.soft_max > max {
            self.soft_max = max;
        }
        self
    }
    const fn soft(mut self, soft_max: f64) -> Meta {
        self.soft_max = soft_max;
        self
    }
    const fn at_least(mut self, min: f64) -> Meta {
        self.min = min;
        self
    }
    const fn step(mut self, step: f64) -> Meta {
        self.step = step;
        self
    }
    const fn unit(mut self, unit: &'static str) -> Meta {
        self.unit = unit;
        self
    }
    const fn choices(mut self, c: &'static [(&'static str, &'static str)]) -> Meta {
        self.choices = c;
        self
    }
    const fn restart(mut self) -> Meta {
        self.restart = true;
        self
    }
    const fn hidden(mut self) -> Meta {
        self.hidden = true;
        self
    }
    const fn adv(mut self) -> Meta {
        self.advanced = true;
        self
    }
    const fn slider(mut self) -> Meta {
        self.slider = true;
        self
    }
    const fn warn(mut self, w: &'static str) -> Meta {
        self.warn = w;
        self
    }

    /// The `[table]` of the config file this setting lives in.
    pub fn section(&self) -> &str {
        self.path.split('.').next().unwrap_or("")
    }

    /// The key inside its table.
    pub fn key(&self) -> &str {
        self.path.split_once('.').map_or(self.path, |(_, k)| k)
    }
}

use Kind::*;

const fn b(path: &'static str, g: &'static str, label: &'static str, d: &'static str) -> Meta {
    Meta::new(path, g, Bool, label, d)
}
const fn n(path: &'static str, g: &'static str, label: &'static str, d: &'static str) -> Meta {
    Meta::new(path, g, Int, label, d).at_least(0.0)
}
const fn f(path: &'static str, g: &'static str, label: &'static str, d: &'static str) -> Meta {
    Meta::new(path, g, Float, label, d)
}
const fn t(path: &'static str, g: &'static str, label: &'static str, d: &'static str) -> Meta {
    Meta::new(path, g, Text, label, d)
}
const fn k(path: &'static str, label: &'static str) -> Meta {
    Meta::new(path, "keys", Shortcut, label, "Qt key-sequence syntax, for example Ctrl+Shift+P. Leave empty to unbind.")
}

const THEMES: &[(&str, &str)] = &[
    ("system", "Follow the system"),
    ("light", "Light"),
    ("dark", "Dark"),
    ("breeze-light", "Breeze Light"),
    ("breeze-dark", "Breeze Dark"),
];
const PROVIDERS: &[(&str, &str)] = &[("lrclib", "LRCLIB"), ("netease", "NetEase Cloud Music"), ("youtube", "YouTube Music")];

pub static META: &[Meta] = &[
    // ---- playback & audio ----
    Meta::new(
        "audio.engine",
        "audio",
        Choice,
        "Audio engine",
        "Qt Multimedia is the default. The built-in Rust engine is needed for gapless playback, crossfade, loudness normalisation, the equalizer and choosing an output device.",
    )
    .choices(&[("rust", "Built-in (Rust)"), ("qt", "Qt Multimedia")])
    .restart(),
    b("audio.gapless", "audio", "Gapless playback", "Start the next track without a gap. Needs the built-in (Rust) audio engine."),
    n("audio.crossfade_ms", "audio", "Crossfade", "Overlap the end of a track with the start of the next one. 0 turns it off. Needs the built-in (Rust) audio engine.")
        .range(0.0, 12000.0)
        .step(250.0)
        .unit("ms")
        .slider(),
    b("audio.normalize", "audio", "Normalise loudness", "Even out volume differences between tracks using YouTube's loudness data. Needs the built-in (Rust) audio engine."),
    t("audio.output_device", "audio", "Output device", "Name of the audio output; empty uses the system default. Needs the built-in (Rust) audio engine.").restart(),
    f("playback.default_volume", "audio", "Starting volume", "Volume used on first start.").range(0.0, 1.0).step(0.05).slider(),
    b("playback.prefetch_next", "audio", "Prepare the next song", "Resolve the next queue entry in the background so skipping is instant."),
    b("playback.autoplay_radio", "audio", "Keep playing when the queue ends", "Continue with YouTube's radio for the last song."),
    n(
        "playback.restart_threshold_ms",
        "audio",
        "Previous restarts after",
        "\"Previous\" restarts the song instead of skipping back once it is past this point.",
    )
    .range(0.0, 30000.0)
    .step(500.0)
    .unit("ms")
    .adv(),
    // equalizer (edited by the equalizer panel)
    b("audio.eq_enabled", "eq", "Equalizer", "Apply the equalizer.").hidden(),
    t("audio.eq_preset", "eq", "Equalizer preset", "Name of the active preset.").hidden(),
    f("audio.eq_preamp_db", "eq", "Preamp", "Gain applied before the bands.").range(-24.0, 24.0).step(0.5).unit("dB").hidden(),
    Meta::new("audio.eq_bands_db", "eq", FloatList, "Equalizer bands", "Ten gains in dB: 31 Hz to 16 kHz.").range(-24.0, 24.0).hidden(),
    // ---- downloads ----
    Meta::new("downloads.dir", "downloads", Dir, "Download folder", "Where downloaded songs are stored. Empty uses the app's data folder.")
        .restart(),
    Meta::new("downloads.quality", "downloads", Choice, "Download quality", "Higher quality uses more storage.").choices(&[
        ("best", "Best"),
        ("normal", "Normal"),
        ("data_saver", "Data saver"),
    ]),
    f(
        "downloads.cap_gb",
        "downloads",
        "Storage limit",
        "Downloads never use more than this. When it is reached the oldest unpinned songs are removed first. There is no upper limit; raise it freely.",
    )
    .at_least(0.1)
    .soft(128.0)
    .step(0.5)
    .unit("GB")
    .slider(),
    b("downloads.smart_enabled", "smart", "Smart downloads", "Automatically keep songs you are likely to play available offline, using the rules below."),
    b("downloads.keep_liked", "smart", "Keep liked songs", "Download your liked songs."),
    n("downloads.recent_n", "smart", "Recently played", "Keep this many recently played songs.").range(0.0, 5000.0).soft(500.0).unit("songs"),
    n("downloads.queue_ahead_n", "smart", "Upcoming in the queue", "Keep this many upcoming queue entries ready.").range(0.0, 200.0).soft(50.0).unit("songs"),
    n(
        "downloads.remove_unplayed_days",
        "smart",
        "Remove unplayed after",
        "Remove songs not played for this many days. 0 keeps them. Pinned playlists and albums are exempt.",
    )
    .range(0.0, 3650.0)
    .soft(365.0)
    .unit("days"),
    b("downloads.pause_on_metered", "smart", "Pause on metered connections", "Do not download over a connection your system marks as metered."),
    b("downloads.pause_on_low_battery", "smart", "Pause on low battery", "Do not download while the battery is low and unplugged."),
    n("downloads.low_battery_percent", "smart", "Low battery threshold", "Battery level below which downloads pause.").range(1.0, 90.0).unit("%").slider(),
    n("downloads.max_parallel", "smart", "Parallel downloads", "How many songs are downloaded at the same time.").range(1.0, 8.0),
    // ---- lyrics ----
    b(
        "lyrics.enabled",
        "lyrics",
        "Look up lyrics on other services",
        "Off means YouTube Music's own plain lyrics only. The providers receive the title, artist and duration of the song.",
    ),
    Meta::new(
        "lyrics.providers",
        "lyrics",
        TextList,
        "Lyric providers",
        "Which services to ask, in order of preference. All enabled ones are asked at once; synced lyrics beat plain ones.",
    )
    .choices(PROVIDERS),
    b("lyrics.prefer_synced", "lyrics", "Prefer synced lyrics", "Choose line-by-line synced lyrics over plain text even when the plain text is longer."),
    f("lyrics.size_scale", "lyrics", "Text size", "Multiplier for the lyric text (1.0 is the default; it also scales with the window).")
        .range(0.5, 3.0)
        .step(0.05)
        .slider(),
    b("lyrics.karaoke", "lyrics", "Karaoke fill", "Fill the current line from left to right between timestamps."),
    // Not shown until the interface implements them (a setting that does nothing is worse than none).
    b("lyrics.gap_dots", "lyrics", "Dots in instrumental gaps", "Show animated dots during long instrumental parts.").hidden(),
    b("lyrics.tint_with_accent", "lyrics", "Tint the current line", "Colour the current line with the cover's accent."),
    n("lyrics.cache_days", "lyrics", "Remember lyrics for", "Keep looked-up lyrics on disk this long. 0 does not cache.").range(0.0, 365.0).unit("days"),
    n("lyrics.min_lines", "lyrics", "Minimum lines", "Lyric records with fewer lines are treated as junk.").range(1.0, 50.0).adv(),
    Meta::new("lyrics.lrclib_url", "endpoints", Url, "LRCLIB address", "Must be https (http only for localhost).").adv(),
    Meta::new("lyrics.netease_url", "endpoints", Url, "NetEase address", "Must be https (http only for localhost).").adv(),
    // ---- appearance ----
    // Hidden: the KDE style follows the desktop's colour scheme and ignores an app-level request; see README "Known gaps".
    Meta::new("appearance.theme", "look", Choice, "Theme", "Light or dark colours. The system option follows your desktop.").choices(THEMES).hidden(),
    f("appearance.cover_radius_percent", "look", "Cover rounding", "Corner radius of large covers as a percentage of their size.")
        .range(0.0, 50.0)
        .step(1.0)
        .unit("%")
        .slider(),
    b("appearance.accent_from_cover", "look", "Colours from the cover", "Tint pages, lyrics and highlights with the current cover's colour."),
    b("appearance.reduce_motion", "look", "Reduce motion", "Turn off decorative animation such as lyric blur and shelf transitions."),
    Meta::new("appearance.density", "look", Choice, "Density", "Compact fits more on screen.")
        .choices(&[("comfortable", "Comfortable"), ("compact", "Compact")])
        .hidden(),
    t("ui.style", "look", "Style override", "Qt Quick Controls style. Empty uses org.kde.desktop (Fusion if it is missing). Examples: Fusion, Material.")
        .restart()
        .adv(),
    n("ui.suggest_debounce_ms", "look", "Search suggestion delay", "Wait this long after typing before asking for suggestions.").range(0.0, 2000.0).unit("ms").adv(),
    n("ui.lyrics_lead_ms", "look", "Lyric lead", "Highlight the next lyric line this much early.").range(0.0, 2000.0).unit("ms").adv(),
    // ---- window ----
    b("window.close_to_tray", "window", "Close to the tray", "Closing the window keeps the music playing from the system tray."),
    b("window.mini_always_on_top", "window", "Mini-player on top", "Keep the mini-player above other windows."),
    b("window.remember_geometry", "window", "Remember window size and position", "Restore the window as you left it.").hidden(),
    b("integration.mpris", "window", "Desktop media controls", "Publish playback to the desktop (MPRIS on Linux) for media keys and widgets.").restart(),
    b("integration.notifications", "window", "Error notifications", "Show errors as desktop notifications while the window is in the background. Nothing else is announced."),
    // ---- shortcuts ----
    k("keys.play_pause", "Play / pause"),
    k("keys.next", "Next song"),
    k("keys.previous", "Previous song"),
    k("keys.search", "Search"),
    k("keys.fullscreen", "Full screen"),
    k("keys.mini_player", "Mini-player"),
    k("keys.now_playing", "Now playing"),
    k("keys.lyrics", "Lyrics"),
    k("keys.volume_up", "Volume up"),
    k("keys.volume_down", "Volume down"),
    k("keys.seek_forward", "Seek forward"),
    k("keys.seek_back", "Seek back"),
    // ---- reporting ----
    b(
        "reporting.enabled",
        "reporting",
        "Tell YouTube what I played",
        "Songs you listen to are added to your YouTube Music history and shape your recommendations. Only while signed in.",
    ),
    n("reporting.min_play_secs", "reporting", "Count a play after", "Songs skipped earlier than this are not reported.").range(0.0, 600.0).unit("s"),
    b("reporting.watchtime", "reporting", "Watch-time pings (experimental)", "Not implemented yet; the switch is reserved.").adv(),
    n("reporting.retries", "reporting", "Report retries", "Tries for a report that fails before it is dropped. Never blocks playback.").range(0.0, 10.0).adv(),
    // ---- security ----
    b(
        "security.proxy_token",
        "security",
        "Protect the local stream proxy",
        "Require an unguessable token in the local stream proxy's addresses so other programs on this computer cannot use it.",
    )
    .restart()
    .warn("Any program on this computer can now fetch audio through the app's local proxy."),
    b("security.redact_logs", "security", "Hide secrets in logs", "Strip cookies, tokens and signed links from the log files.")
        .restart()
        .warn("Log files may now contain cookies, tokens and signed links. Do not share them."),
    n(
        "security.report_retention_days",
        "security",
        "Keep failure reports",
        "RustyPipe failure reports contain response bodies; delete them after this many days. 0 keeps none.",
    )
    .range(0.0, 365.0)
    .unit("days"),
    b(
        "security.allow_insecure_urls",
        "security",
        "Allow plain http addresses",
        "Lets lyric providers and similar endpoints use unencrypted http. Localhost is always allowed.",
    )
    .warn("Unencrypted addresses can be read and changed by anyone on the network."),
    // ---- network ----
    n("network.connect_timeout_secs", "network", "Connect timeout", "How long to wait for a server to answer.").range(1.0, 120.0).unit("s").restart(),
    n("network.request_timeout_secs", "network", "Request timeout", "How long a whole request may take.").range(1.0, 300.0).unit("s").restart(),
    n("network.retries", "network", "Retries", "How often a request that failed in a transient way is tried again.").range(0.0, 10.0).restart(),
    n("network.stream_read_timeout_secs", "network", "Stream silence timeout", "Maximum silence on a stream connection before giving up.")
        .range(1.0, 300.0)
        .unit("s")
        .restart()
        .adv(),
    n(
        "network.proxy_chunk_bytes",
        "network",
        "Stream chunk size",
        "Audio is fetched in chunks of this size because googlevideo refuses unbounded ranges.",
    )
    .range(65536.0, 64_000_000.0)
    .unit("bytes")
    .restart()
    .adv(),
    // ---- library ----
    n("library.home_items", "library", "Home items", "How many items each Home row loads.").range(1.0, 200.0),
    n("library.history_items", "library", "History items", "How many history entries to show.").range(1.0, 200.0),
    n("library.radio_items", "library", "Radio items", "How many songs a radio starts with.").range(1.0, 200.0),
    n("library.search_songs", "library", "Songs per search", "Songs shown in search results.").range(1.0, 100.0),
    n("library.search_artists", "library", "Artists per search", "Artists shown in search results.").range(0.0, 50.0),
    n("library.search_albums", "library", "Albums per search", "Albums shown in search results.").range(0.0, 100.0),
    n("library.search_playlists", "library", "Playlists per search", "Playlists shown in search results.").range(0.0, 100.0),
    n("library.suggestions", "library", "Search suggestions", "How many suggestions to show while typing.").range(0.0, 30.0),
    n("library.liked_limit", "library", "Liked songs to read", "How many liked songs are read in the background to show heart states everywhere.")
        .range(0.0, 20000.0)
        .soft(5000.0),
    n("library.page_cache_secs", "library", "Page cache", "Keep Home, Explore, artist and album pages this long so going back is instant. 0 turns it off.")
        .range(0.0, 86400.0)
        .unit("s"),
    // ---- playback caches / health check ----
    n("playback.stream_cache_minutes", "cache", "Stream address cache", "Reuse resolved stream addresses for this long.").range(1.0, 300.0).unit("min").adv(),
    n("playback.visitor_cache_minutes", "cache", "Visitor id cache", "Reuse the anonymous visitor id for this long.").range(1.0, 300.0).unit("min").adv(),
    n("playback.session_save_secs", "cache", "Save session every", "How often the queue and position are saved.").range(1.0, 600.0).unit("s").adv(),
    t("playback.check_video_id", "cache", "Stream check video", "Known-good video used by the stream check.").adv(),
    n(
        "playback.check_offset_bytes",
        "cache",
        "Stream check offset",
        "Where the stream check reads from; must be past the ~768 KB cutoff of walled addresses.",
    )
    .range(0.0, 100_000_000.0)
    .unit("bytes")
    .adv(),
    // ---- YouTube client identities ----
    t("youtube.language", "client", "Language", "Language code sent to YouTube (for example en, de, ja).").adv(),
    Meta::new("youtube.music_origin", "client", Url, "YouTube Music address", "Must be https.").adv(),
    t("youtube.android_vr_client_version", "client", "Play-report client version", "Client version used to record plays on YouTube. Goes stale when YouTube changes things; edit instead of rebuilding.").adv(),
    t("youtube.web_remix_client_version", "client", "Web client version", "Goes stale when YouTube changes things; edit instead of rebuilding.").adv(),
    t("youtube.visionos_client_version", "client", "visionOS client version", "Version of the client identity used for streams.").restart().adv(),
    t("youtube.visionos_os_version", "client", "visionOS version", "OS version of the client identity used for streams.").restart().adv(),
    t("youtube.visionos_user_agent", "client", "visionOS user agent", "User agent of the client identity used for streams.").restart().adv(),
    t("youtube.innertube_api_key", "client", "Innertube API key", "Public key the web client sends.").adv(),
];

pub fn find(path: &str) -> Option<&'static Meta> {
    META.iter().find(|m| m.path == path)
}

pub fn in_group(group: &str) -> impl Iterator<Item = &'static Meta> + '_ {
    META.iter().filter(move |m| m.group == group)
}

pub fn in_section(section: &str) -> impl Iterator<Item = &'static Meta> + '_ {
    META.iter().filter(move |m| m.section() == section)
}
