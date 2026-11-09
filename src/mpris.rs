// SPDX-License-Identifier: GPL-3.0-only
//! MPRIS2 (org.mpris.MediaPlayer2) so media keys, KDE Connect and the Plasma
//! media widget can see and control playback.
//!
//! Rust holds no audio state itself: QML pushes a snapshot in via `update`, and
//! incoming D-Bus calls are forwarded to QML as `command(name, arg)` signals.
use crate::state::st;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

const PATH: &str = "/org/mpris/MediaPlayer2";

#[derive(Clone, Default, PartialEq)]
pub struct Snap {
    pub has_track: bool,
    pub playing: bool,
    pub title: String,
    pub artist: String,
    pub art: String,
    pub video_id: String,
    pub length_us: i64,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat: i32, // 0 off, 1 all, 2 one
}

static SNAP: Mutex<Option<Snap>> = Mutex::new(None);
static POSITION_US: Mutex<i64> = Mutex::new(0);
static CONN: OnceLock<zbus::Connection> = OnceLock::new();
type CommandSink = Box<dyn Fn(&str, i64) + Send + Sync>;
static SEND: OnceLock<CommandSink> = OnceLock::new();

fn snap() -> Snap {
    SNAP.lock().unwrap().clone().unwrap_or_default()
}

fn send(cmd: &str, arg: i64) {
    if let Some(f) = SEND.get() {
        f(cmd, arg);
    }
}

struct Root;

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {
        send("raise", 0);
    }
    fn quit(&self) {
        send("quit", 0);
    }
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> &str {
        "YQT"
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        "org.kde.yqt"
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec![]
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        vec![]
    }
}

struct PlayerIface;

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl PlayerIface {
    fn next(&self) {
        send("next", 0);
    }
    fn previous(&self) {
        send("previous", 0);
    }
    fn pause(&self) {
        send("pause", 0);
    }
    fn play_pause(&self) {
        send("playpause", 0);
    }
    fn stop(&self) {
        send("stop", 0);
    }
    fn play(&self) {
        send("play", 0);
    }
    fn seek(&self, offset_us: i64) {
        send("seek", offset_us / 1000);
    }
    fn set_position(&self, track: ObjectPath<'_>, position_us: i64) {
        // The spec: ignore the request unless it names the track that is playing now (a late request for the previous
        // song must not seek the new one).
        let s = snap();
        if !s.has_track || track.as_str() != track_path(&s) {
            return;
        }
        send("setposition", position_us / 1000);
    }
    fn open_uri(&self, _uri: &str) {}

    #[zbus(property)]
    fn playback_status(&self) -> &str {
        let s = snap();
        match (s.has_track, s.playing) {
            (false, _) => "Stopped",
            (true, true) => "Playing",
            (true, false) => "Paused",
        }
    }
    #[zbus(property)]
    fn loop_status(&self) -> &str {
        match snap().repeat {
            1 => "Playlist",
            2 => "Track",
            _ => "None",
        }
    }
    #[zbus(property)]
    fn set_loop_status(&self, v: &str) {
        send(
            "repeat",
            match v {
                "Playlist" => 1,
                "Track" => 2,
                _ => 0,
            },
        );
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn shuffle(&self) -> bool {
        snap().shuffle
    }
    #[zbus(property)]
    fn set_shuffle(&self, v: bool) {
        send("shuffle", v as i64);
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        snap().volume
    }
    #[zbus(property)]
    fn set_volume(&self, v: f64) {
        send("volume", (v.clamp(0.0, 1.0) * 1000.0) as i64);
    }
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        *POSITION_US.lock().unwrap()
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        metadata(&snap())
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        snap().has_track
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        snap().has_track
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        snap().has_track
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        snap().has_track
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        snap().has_track
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

/// The MPRIS object path that identifies the current track (`mpris:trackid`); clients must quote it in `SetPosition`.
fn track_path(s: &Snap) -> String {
    if s.has_track {
        let safe: String = s.video_id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        format!("/org/kde/ytm/track/{safe}")
    } else {
        "/org/mpris/MediaPlayer2/TrackList/NoTrack".to_owned()
    }
}

fn metadata(s: &Snap) -> HashMap<String, OwnedValue> {
    let mut m: HashMap<String, OwnedValue> = HashMap::new();
    let mut put = |k: &str, v: Value<'_>| {
        if let Ok(v) = OwnedValue::try_from(v) {
            m.insert(k.to_owned(), v);
        }
    };
    if let Ok(p) = ObjectPath::try_from(track_path(s)) {
        put("mpris:trackid", Value::from(p));
    }
    if s.has_track {
        put("mpris:length", Value::from(s.length_us));
        put("xesam:title", Value::from(s.title.clone()));
        put("xesam:artist", Value::from(vec![s.artist.clone()]));
        if !s.art.is_empty() {
            put("mpris:artUrl", Value::from(s.art.clone()));
        }
    }
    m
}

/// Claim the bus name and start serving. `forward` is called (on a tokio thread)
/// for every command from a D-Bus client.
pub fn start(forward: impl Fn(&str, i64) + Send + Sync + 'static) {
    if SEND.set(Box::new(forward)).is_err() {
        return; // already running
    }
    st().rt.spawn(async {
        let built = zbus::connection::Builder::session()
            .and_then(|b| b.name("org.mpris.MediaPlayer2.yqt"))
            .and_then(|b| b.serve_at(PATH, Root))
            .and_then(|b| b.serve_at(PATH, PlayerIface));
        match built {
            Ok(b) => match b.build().await {
                Ok(c) => {
                    let _ = CONN.set(c);
                }
                Err(e) => tracing::info!("MPRIS unavailable: {e}"),
            },
            Err(e) => tracing::info!("MPRIS unavailable: {e}"),
        }
    });
}

/// Push the current state; emits PropertiesChanged for whatever differs.
pub fn update(new: Snap, position_us: i64) {
    *POSITION_US.lock().unwrap() = position_us;
    let old = snap();
    if old == new {
        return;
    }
    *SNAP.lock().unwrap() = Some(new.clone());
    let Some(conn) = CONN.get().cloned() else { return };
    st().rt.spawn(async move {
        let Ok(iface) = conn.object_server().interface::<_, PlayerIface>(PATH).await else { return };
        let em: &SignalEmitter<'_> = iface.signal_emitter();
        let i = iface.get().await;
        if old.has_track != new.has_track || old.playing != new.playing {
            let _ = i.playback_status_changed(em).await;
            let _ = i.can_go_next_changed(em).await;
            let _ = i.can_go_previous_changed(em).await;
            let _ = i.can_play_changed(em).await;
            let _ = i.can_pause_changed(em).await;
            let _ = i.can_seek_changed(em).await;
        }
        if old.video_id != new.video_id
            || old.title != new.title
            || old.artist != new.artist
            || old.art != new.art
            || old.length_us != new.length_us
            || old.has_track != new.has_track
        {
            let _ = i.metadata_changed(em).await;
        }
        if old.volume != new.volume {
            let _ = i.volume_changed(em).await;
        }
        if old.shuffle != new.shuffle {
            let _ = i.shuffle_changed(em).await;
        }
        if old.repeat != new.repeat {
            let _ = i.loop_status_changed(em).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_track_id_names_the_playing_song_and_nothing_else() {
        let playing = Snap { has_track: true, video_id: "dQw4w9WgXcQ".into(), ..Default::default() };
        assert_eq!(track_path(&playing), "/org/kde/ytm/track/dQw4w9WgXcQ");
        // characters that are not valid in an object path are dropped, so the path is always valid
        let odd = Snap { has_track: true, video_id: "a-b/c_d".into(), ..Default::default() };
        assert_eq!(track_path(&odd), "/org/kde/ytm/track/abcd");
        assert!(ObjectPath::try_from(track_path(&odd)).is_ok());
        // nothing playing: the spec's NoTrack path, which never equals a real track
        assert_eq!(track_path(&Snap::default()), "/org/mpris/MediaPlayer2/TrackList/NoTrack");
        assert_ne!(track_path(&playing), track_path(&Snap::default()));
    }
}
