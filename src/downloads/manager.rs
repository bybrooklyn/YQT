// SPDX-License-Identifier: GPL-3.0-only
//! The download engine: a queue of jobs, a few parallel workers, streaming to `*.part` files with resume,
//! retries with exponential backoff, progress/speed events, pause rules, and the smart-download loop.
//!
//! The pieces that matter are free functions with no globals so they are unit-tested:
//! [`fetch_to_file`] (ranged download + resume), [`backoff`], [`SpeedMeter`], [`finish`], [`reconcile`].
use super::quality::{self, valid_id};
use super::rules::{self, Action, PlanState, Rules, Stored};
use super::store::{Song, Store};
use crate::metered::{self, BatteryProbe, Metered, PauseReason};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

const MAX_ATTEMPTS: u32 = 5;
const WATCH_INTERVAL: Duration = Duration::from_secs(15);
const SMART_INTERVAL: Duration = Duration::from_secs(600);
const STALE_PART_AGE: Duration = Duration::from_secs(7 * 86_400);
const MAX_INPUT_ITEMS: usize = 5000;

// ---- items --------------------------------------------------------------------------------------

/// What the UI hands over for a song (the JSON the rows already use).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub cover: String,
    pub duration_ms: i64,
}

/// `"3:45"` / `"1:02:03"` → milliseconds; anything else = 0.
pub fn parse_duration_ms(s: &str) -> i64 {
    let mut total: i64 = 0;
    for part in s.trim().split(':') {
        // checked: a hostile "99999:99999:99999:…" must not overflow
        match part.parse::<i64>().ok().filter(|n| (0..100_000).contains(n)).and_then(|n| total.checked_mul(60)?.checked_add(n)) {
            Some(t) => total = t,
            None => return 0,
        }
    }
    total.checked_mul(1000).unwrap_or(0)
}

impl Item {
    /// Accepts the loose JSON of the page items; `None` when there is no valid video id.
    pub fn from_json(v: &Value) -> Option<Item> {
        let s = |k: &str| v[k].as_str().unwrap_or("").trim().to_owned();
        let id = s("videoId");
        if !valid_id(&id) {
            return None;
        }
        let artist = if s("artist").is_empty() { s("subtitle") } else { s("artist") };
        let cover = if s("cover").is_empty() { s("thumbnail") } else { s("cover") };
        Some(Item {
            id,
            title: s("title"),
            artist,
            album: s("album"),
            cover,
            duration_ms: v["durationMs"].as_i64().unwrap_or_else(|| parse_duration_ms(&s("duration"))),
        })
    }

    fn to_json(&self) -> Value {
        // the page items' own field names too, so a row can go straight into `Player.playList`
        let secs = self.duration_ms / 1000;
        let duration = if self.duration_ms > 0 { format!("{}:{:02}", secs / 60, secs % 60) } else { String::new() };
        json!({"kind": "song", "videoId": self.id, "title": self.title, "artist": self.artist, "subtitle": self.artist,
               "album": self.album, "cover": self.cover, "thumbnail": self.cover, "durationMs": self.duration_ms,
               "duration": duration})
    }
}

// ---- pure helpers ------------------------------------------------------------------------------

/// Delay before retry number `attempt` (0-based): 2, 4, 8, … seconds, at most a minute.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(2u64.saturating_mul(1u64 << attempt.min(10))).min(Duration::from_secs(60))
}

/// Smoothed download speed (exponential moving average over ≥ 250 ms windows).
#[derive(Debug, Default)]
pub struct SpeedMeter {
    last_ms: u64,
    last_bytes: u64,
    bps: f64,
    started: bool,
}

impl SpeedMeter {
    /// Feed the total bytes done at `now_ms`; returns bytes per second.
    pub fn update(&mut self, now_ms: u64, bytes: u64) -> u64 {
        if !self.started {
            (self.started, self.last_ms, self.last_bytes) = (true, now_ms, bytes);
            return 0;
        }
        let dt = now_ms.saturating_sub(self.last_ms);
        if dt >= 250 {
            let inst = bytes.saturating_sub(self.last_bytes) as f64 * 1000.0 / dt as f64;
            self.bps = if self.bps == 0.0 { inst } else { self.bps * 0.7 + inst * 0.3 };
            (self.last_ms, self.last_bytes) = (now_ms, bytes);
        }
        self.bps as u64
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DlError {
    Http(u16),
    Net(String),
    Io(String),
    Incomplete,
    Cancelled,
    Unavailable(String),
}

impl DlError {
    /// Worth another attempt (maybe with a fresh stream URL)?
    pub fn retryable(&self) -> bool {
        match self {
            DlError::Http(c) => matches!(*c, 403 | 408 | 429) || *c >= 500,
            DlError::Net(_) | DlError::Incomplete => true,
            DlError::Io(_) | DlError::Cancelled | DlError::Unavailable(_) => false,
        }
    }
    pub fn message(&self) -> String {
        match self {
            DlError::Http(c) => format!("the server answered {c}"),
            DlError::Net(e) => format!("network error: {e}"),
            DlError::Io(e) => format!("could not write the file: {e}"),
            DlError::Incomplete => "the download ended early".into(),
            DlError::Cancelled => "cancelled".into(),
            DlError::Unavailable(e) => e.clone(),
        }
    }
}

/// A failed stream lookup as a download error: a request that did not get through is worth retrying, YouTube's own
/// "no" is not.
fn formats_error(e: crate::vision::FormatsError) -> DlError {
    match e {
        crate::vision::FormatsError::Transient(m) => DlError::Net(m),
        crate::vision::FormatsError::Unavailable(m) => DlError::Unavailable(m),
    }
}

fn content_range_total(h: &reqwest::header::HeaderMap) -> Option<u64> {
    h.get("content-range")?.to_str().ok()?.rsplit('/').next()?.parse().ok()
}

fn open_part(path: &Path, truncate: bool) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let f = o.open(path)?;
    if truncate {
        f.set_len(0)?;
    }
    Ok(f)
}

/// Download `url` into `part`, **resuming** from whatever is already in the file. googlevideo refuses open-ended
/// ranges, so the body is requested in bounded `chunk`-byte ranges. `paused()` is polled between chunks (the
/// download waits while it is true), `cancelled` aborts. Returns the final size.
#[allow(clippy::too_many_arguments)] // one cohesive operation; a params struct would only move the list
pub async fn fetch_to_file(
    client: &reqwest::Client,
    url: &str,
    ua: &str,
    part: &Path,
    expected_total: Option<u64>,
    chunk: u64,
    paused: &(dyn Fn() -> bool + Sync),
    cancelled: &AtomicBool,
    on_progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<u64, DlError> {
    let io = |e: std::io::Error| DlError::Io(e.to_string());
    let net = |e: reqwest::Error| DlError::Net(e.without_url().to_string());
    let chunk = chunk.max(64 * 1024);
    let mut have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    let mut total = expected_total;
    if total.is_some_and(|t| have > t) {
        have = 0; // a leftover from some other stream: start over
    }
    let mut file = tokio::fs::File::from_std(open_part(part, have == 0).map_err(io)?);
    loop {
        if total.is_some_and(|t| have >= t) {
            break;
        }
        while paused() && !cancelled.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if cancelled.load(Ordering::Relaxed) {
            return Err(DlError::Cancelled);
        }
        let mut end = have + chunk - 1;
        if let Some(t) = total {
            end = end.min(t.saturating_sub(1));
        }
        let mut resp = client.get(url).header("User-Agent", ua).header("Range", format!("bytes={have}-{end}")).send().await.map_err(net)?;
        let status = resp.status().as_u16();
        let mut whole = false;
        match status {
            206 => {
                if let Some(t) = content_range_total(resp.headers()) {
                    if have > 0 && (have > t || total.is_some_and(|known| known != t)) {
                        // not the stream the partial came from: start over
                        file.set_len(0).await.map_err(io)?;
                        (have, total) = (0, Some(t));
                        continue;
                    }
                    total = Some(t);
                }
            }
            200 => {
                // the server ignored the range: take the whole body from the start
                if have > 0 {
                    file.set_len(0).await.map_err(io)?;
                    have = 0;
                }
                total = resp.content_length().or(total);
                whole = true;
            }
            416 if total.is_some_and(|t| have == t) => break,
            c => return Err(DlError::Http(c)),
        }
        let before = have;
        while let Some(bytes) = resp.chunk().await.map_err(net)? {
            file.write_all(&bytes).await.map_err(io)?;
            have += bytes.len() as u64;
            on_progress(have, total.unwrap_or(0));
            if cancelled.load(Ordering::Relaxed) {
                return Err(DlError::Cancelled);
            }
        }
        if have == before {
            return Err(DlError::Incomplete); // an empty body would loop forever
        }
        if whole {
            break;
        }
    }
    file.flush().await.map_err(io)?;
    file.sync_all().await.map_err(io)?;
    if total.is_some_and(|t| have != t) {
        return Err(DlError::Incomplete);
    }
    Ok(have)
}

/// Move a finished `.part` into place and record it. The final name is built only from the validated id and a
/// whitelisted extension.
pub fn finish(store: &Store, dir: &Path, item: &Item, ext: &str, mime: &str, part: &Path, now: i64) -> Result<Song, String> {
    let name = quality::file_name(&item.id, ext).ok_or("invalid file name")?;
    let dest = dir.join(name);
    std::fs::rename(part, &dest).map_err(|e| e.to_string())?;
    crate::state::tighten(&dest);
    let size = std::fs::metadata(&dest).map_err(|e| e.to_string())?.len() as i64;
    let song = Song {
        video_id: item.id.clone(),
        title: item.title.clone(),
        artist: item.artist.clone(),
        album: item.album.clone(),
        cover: item.cover.clone(),
        duration_ms: item.duration_ms,
        size,
        format: ext.to_owned(),
        mime: mime.to_owned(),
        added: now,
        last_played: None,
        pinned: false,
    };
    store.upsert(&song).map_err(|e| e.to_string())?;
    Ok(song)
}

/// Drop index rows whose file is gone; delete `.part` leftovers older than a week. Returns the removed ids.
///
/// An empty (missing, just created) folder while the index knows songs is not taken as "everything was deleted": far
/// more likely the folder is on a drive that is not mounted, or the setting was pointed somewhere new. The rows are
/// kept then (with their pins and play history), and [`present`] decides what counts as downloaded right now.
pub fn reconcile(store: &Store, dir: &Path) -> Vec<String> {
    let mut gone = Vec::new();
    let rows = store.list().unwrap_or_default();
    let folder_empty = std::fs::read_dir(dir).map_or(true, |mut rd| rd.next().is_none());
    if folder_empty && !rows.is_empty() {
        return gone;
    }
    for s in rows {
        let exists = quality::file_name(&s.video_id, &s.format).is_some_and(|n| dir.join(n).is_file());
        if !exists {
            let _ = store.remove(&s.video_id);
            gone.push(s.video_id);
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            // only our own partial files (`<11-char id>.<ext>.part`): the folder may be a shared one such as ~/Downloads
            let ours = e
                .file_name()
                .to_str()
                .is_some_and(|n| n.len() > 12 && n.is_char_boundary(11) && valid_id(&n[..11]) && n.as_bytes()[11] == b'.');
            if ours && p.extension().is_some_and(|x| x == "part") {
                let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|m| m.elapsed().ok()).is_some_and(|a| a > STALE_PART_AGE);
                if old {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    }
    gone
}

/// The index rows whose file is in `dir` right now: (ids, total bytes). Only these count as downloaded.
pub fn present(store: &Store, dir: &Path) -> (HashSet<String>, i64) {
    let mut ids = HashSet::new();
    let mut bytes = 0i64;
    for s in store.list().unwrap_or_default() {
        if quality::file_name(&s.video_id, &s.format).is_some_and(|n| dir.join(n).is_file()) {
            bytes = bytes.saturating_add(s.size.max(0));
            ids.insert(s.video_id);
        }
    }
    (ids, bytes)
}

/// Delete the partial files of `id` (every stream's), except the one named `keep`.
pub fn remove_parts(dir: &Path, id: &str, keep: Option<&str>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name();
        if name.to_str().is_some_and(|n| quality::is_part_of(n, id) && keep != Some(n)) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Delete every file a song can have on disk (final + partial). Only validated ids ever reach the file system.
pub fn delete_files(dir: &Path, id: &str) {
    if !valid_id(id) {
        return;
    }
    for ext in quality::EXTS {
        if let Some(n) = quality::file_name(id, ext) {
            let _ = std::fs::remove_file(dir.join(&n));
        }
    }
    remove_parts(dir, id, None);
}

// ---- global manager ----------------------------------------------------------------------------

/// What the UI is told about.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A song's state changed (queued / progress milestone / done / failed / removed).
    Changed(String),
    Progress {
        id: String,
        percent: i32,
        bps: i64,
    },
    /// Totals, pause state or offline state changed.
    Stats,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    Queued,
    Downloading { percent: i32, bps: i64 },
    Failed(String),
}

struct Job {
    item: Item,
    state: JobState,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inputs {
    liked: Vec<Item>,
    recent: Vec<Item>,
    queue: Vec<Item>,
    /// The song playing now and the next few (ids): never evicted.
    playing: Vec<String>,
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Job>,
    waiting: VecDeque<String>,
    active: usize,
    have: HashSet<String>,
    used: i64,
    user_paused: bool,
    inputs: Inputs,
}

/// A read of the relevant `[downloads]` settings.
#[derive(Debug, Clone)]
pub struct Settings {
    pub dir: PathBuf,
    pub quality: String,
    pub rules: Rules,
    pub max_parallel: usize,
    pub pause_on_metered: bool,
    pub pause_on_low_battery: bool,
    pub low_battery_percent: u32,
}

pub fn dir_for(cfg_dir: &str, data_dir: &Path) -> PathBuf {
    if cfg_dir.trim().is_empty() {
        data_dir.join("downloads")
    } else {
        PathBuf::from(cfg_dir.trim())
    }
}

/// The downloads folder of this run, latched at first use. A change of `downloads.dir` applies from the next start:
/// switching folders under a running index would show songs as downloaded that are not in the new folder and leave
/// removals pointing at the wrong place. At that start the index rows are kept (see [`reconcile`]), so files the
/// user moved into the new folder are simply picked up again.
fn session_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| dir_for(&crate::config::current().downloads.dir, &crate::state::data_dir())).clone()
}

impl Settings {
    pub fn load() -> Settings {
        let d = &crate::config::current().downloads;
        Settings {
            dir: session_dir(),
            quality: d.quality.clone(),
            rules: Rules::from_config(d),
            max_parallel: d.max_parallel.clamp(1, 8) as usize,
            pause_on_metered: d.pause_on_metered,
            pause_on_low_battery: d.pause_on_low_battery,
            low_battery_percent: d.low_battery_percent,
        }
    }
}

type Sink = Box<dyn Fn(Event) + Send + Sync>;

struct Manager {
    inner: Mutex<Inner>,
    store: Mutex<Option<Store>>,
    sink: Mutex<Option<Sink>>,
    pause: Mutex<Option<PauseReason>>,
    paused_flag: AtomicBool,
    metered_note: Mutex<Option<&'static str>>,
    wake: tokio::sync::Notify,
    client: reqwest::Client,
}

fn mgr() -> &'static Manager {
    static M: OnceLock<Manager> = OnceLock::new();
    M.get_or_init(|| Manager {
        inner: Mutex::new(Inner::default()),
        store: Mutex::new(None),
        sink: Mutex::new(None),
        pause: Mutex::new(None),
        paused_flag: AtomicBool::new(false),
        metered_note: Mutex::new(None),
        wake: tokio::sync::Notify::new(),
        client: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(crate::config::get().network.connect_timeout_secs))
            .read_timeout(Duration::from_secs(crate::config::get().network.stream_read_timeout_secs))
            .build()
            .expect("http client"),
    })
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn emit(ev: Event) {
    if let Some(s) = mgr().sink.lock().unwrap().as_ref() {
        s(ev);
    }
}

/// Where UI events go (called on background threads; the QML singleton forwards them to the Qt thread).
pub fn set_sink(f: impl Fn(Event) + Send + Sync + 'static) {
    *mgr().sink.lock().unwrap() = Some(Box::new(f));
}

fn with_store<R>(f: impl FnOnce(&Store) -> R) -> Option<R> {
    let mut g = mgr().store.lock().unwrap();
    if g.is_none() {
        *g = Store::open(&crate::state::data_dir().join("downloads.sqlite")).map_err(|e| tracing::warn!("downloads index: {e}")).ok();
    }
    g.as_ref().map(f)
}

/// One-time start-up: open the index, reconcile with the disk, start the pause watcher and the smart loop.
pub fn init() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let cfg = Settings::load();
        let _ = crate::state::ensure_private_dir(&cfg.dir);
        if let Some((have, used, again)) = with_store(|s| {
            reconcile(s, &cfg.dir);
            let (have, used) = present(s, &cfg.dir);
            // what the user had asked for when the app was last closed, in the same order
            let again = restorable(s, &have);
            (have, used, again)
        }) {
            let mut inner = mgr().inner.lock().unwrap();
            (inner.have, inner.used) = (have, used);
            for item in again {
                add_job(&mut inner, item); // (started by the watcher below, once it knows whether downloads are paused)
            }
        }
        let metered: Arc<dyn Metered> = Arc::from(metered::system_metered());
        *mgr().metered_note.lock().unwrap() = metered.note();
        crate::connectivity::subscribe(|_| {
            mgr().wake.notify_one();
        });
        crate::connectivity::start();
        let rt = crate::state::st().rt.handle().clone();
        rt.spawn(async move {
            let battery: Arc<dyn BatteryProbe> = Arc::new(metered::SystemBattery);
            // (not `Instant::now() - SMART_INTERVAL`: that panics on systems whose clock starts at boot, soon after boot)
            let mut last_smart: Option<Instant> = None;
            loop {
                refresh_pause(metered.clone(), battery.clone()).await;
                if last_smart.is_none() {
                    kick(); // first round: the downloads restored from the last run start now (unless paused)
                }
                if smart_due(last_smart) {
                    last_smart = Some(Instant::now());
                    evaluate();
                }
                tokio::select! {
                    () = tokio::time::sleep(WATCH_INTERVAL) => {}
                    () = mgr().wake.notified() => {}
                }
            }
        });
    });
}

/// Is the periodic smart-download pass due? `None` = it has not run yet in this session.
fn smart_due(last: Option<Instant>) -> bool {
    last.is_none_or(|t| t.elapsed() >= SMART_INTERVAL)
}

async fn refresh_pause(metered: Arc<dyn Metered>, battery: Arc<dyn BatteryProbe>) {
    let s = Settings::load();
    let (m, b) =
        tokio::task::spawn_blocking(move || (metered.state(), battery.status())).await.unwrap_or((metered::MeteredState::Unknown, None));
    let user = mgr().inner.lock().unwrap().user_paused;
    let reason = metered::pause_reason(
        user,
        !crate::connectivity::is_online(),
        s.pause_on_metered,
        m,
        s.pause_on_low_battery,
        s.low_battery_percent,
        b,
    );
    let changed = {
        let mut p = mgr().pause.lock().unwrap();
        let c = *p != reason;
        *p = reason;
        c
    };
    mgr().paused_flag.store(reason.is_some(), Ordering::Relaxed);
    if changed {
        emit(Event::Stats);
        if reason.is_none() {
            kick();
        }
    }
}

fn is_paused() -> bool {
    mgr().paused_flag.load(Ordering::Relaxed)
}

// ---- public operations ------------------------------------------------------------------------

pub fn pause_reason() -> Option<PauseReason> {
    *mgr().pause.lock().unwrap()
}
pub fn metered_note() -> Option<&'static str> {
    *mgr().metered_note.lock().unwrap()
}
pub fn used_bytes() -> i64 {
    mgr().inner.lock().unwrap().used
}
pub fn active_count() -> i32 {
    let i = mgr().inner.lock().unwrap();
    i32::try_from(i.jobs.values().filter(|j| !matches!(j.state, JobState::Failed(_))).count()).unwrap_or(i32::MAX)
}

pub fn set_paused(on: bool) {
    init(); // the watcher must be there: resuming is its decision
    mgr().inner.lock().unwrap().user_paused = on;
    if on {
        // pausing applies immediately
        *mgr().pause.lock().unwrap() = Some(PauseReason::User);
        mgr().paused_flag.store(true, Ordering::Relaxed);
    }
    // Resuming does not clear the pause here: being offline, on a metered connection or low on battery may still hold
    // the downloads back, and starting them for a moment would only make them fail. The watcher is woken instead; it
    // works out the real reason and starts the queue itself when there is none.
    emit(Event::Stats);
    mgr().wake.notify_one();
}

pub fn is_downloaded(id: &str) -> bool {
    mgr().inner.lock().unwrap().have.contains(id)
}

/// ("none" | "queued" | "downloading" | "downloaded" | "failed", percent 0-100).
pub fn state_of(id: &str) -> (&'static str, i32) {
    let i = mgr().inner.lock().unwrap();
    match i.jobs.get(id).map(|j| &j.state) {
        Some(JobState::Queued) => ("queued", 0),
        Some(JobState::Downloading { percent, .. }) => ("downloading", *percent),
        Some(JobState::Failed(_)) => ("failed", 0),
        None if i.have.contains(id) => ("downloaded", 100),
        None => ("none", 0),
    }
}

/// Put a song in the queue unless it is downloaded or already queued / downloading. `true` = a job was added.
fn add_job(i: &mut Inner, item: Item) -> bool {
    if !valid_id(&item.id)
        || i.have.contains(&item.id)
        || matches!(i.jobs.get(&item.id).map(|j| &j.state), Some(JobState::Queued | JobState::Downloading { .. }))
    {
        return false;
    }
    let id = item.id.clone();
    i.jobs.insert(id.clone(), Job { item, state: JobState::Queued, cancel: Arc::new(AtomicBool::new(false)) });
    i.waiting.push_back(id);
    true
}

/// What the index remembers of a download the user asked for: (video id, item JSON).
fn record_of(item: &Item) -> (String, String) {
    (item.id.clone(), item.to_json().to_string())
}

/// Queue downloads the user asked for. Returns (was a job added, the records to remember so they survive a restart):
/// every song that is not downloaded yet is remembered, also one the smart rules had queued already (the user now
/// wants it regardless of them). Smart downloads never come through here: the planner works them out again.
fn queue_manual(i: &mut Inner, items: Vec<Item>) -> (bool, Vec<(String, String)>) {
    let (mut added, mut records) = (false, Vec::new());
    for item in items {
        if !valid_id(&item.id) || i.have.contains(&item.id) {
            continue;
        }
        records.push(record_of(&item));
        added |= add_job(i, item);
    }
    (added, records)
}

/// The recorded downloads to queue again at start-up, in the order they were asked for. Records of songs that are
/// downloaded by now (or that cannot be read) are dropped.
pub fn restorable(store: &Store, have: &HashSet<String>) -> Vec<Item> {
    let mut out = Vec::new();
    for (id, json) in store.queued().unwrap_or_default() {
        let item = serde_json::from_str::<Value>(&json).ok().and_then(|v| Item::from_json(&v)).filter(|it| it.id == id);
        match item {
            Some(it) if !have.contains(&id) => out.push(it),
            _ => {
                let _ = store.queue_remove(&id);
            }
        }
    }
    out
}

/// Is a finished job done for good (so its record goes)? A failure that could still work on another try keeps the
/// record: the song is queued again at the next start.
fn done_for_good(result: &Result<Song, DlError>) -> bool {
    result.as_ref().map_or_else(|e| !e.retryable(), |_| true)
}

/// Drop the record of a download (it finished, was cancelled or removed, or failed for good).
fn forget_queued(id: &str) {
    with_store(|s| s.queue_remove(id).map_err(|e| tracing::warn!("downloads queue: {e}")));
}

/// Download a song because the user asked for it (remembered across restarts until it is done).
pub fn enqueue(item: Item) {
    enqueue_all(vec![item]);
}

/// [`enqueue`] for many songs at once (an album, a playlist page): one write to the index for all of them.
pub fn enqueue_all(items: Vec<Item>) {
    if !items.iter().any(|it| valid_id(&it.id)) {
        return;
    }
    init();
    let (added, records) = queue_manual(&mut mgr().inner.lock().unwrap(), items);
    if !records.is_empty() {
        // a failed write only costs the "survives a restart" part: the download itself goes ahead
        with_store(|s| s.queue_add(&records).map_err(|e| tracing::warn!("downloads queue: {e}")));
    }
    if added {
        emit(Event::Stats);
        kick();
    }
}

/// Queue a song the smart rules want. Not remembered: the planner derives it again after a restart.
fn enqueue_smart(item: Item) {
    if add_job(&mut mgr().inner.lock().unwrap(), item) {
        emit(Event::Stats);
        kick();
    }
}

/// Cancel a running/queued job and forget a failed one.
pub fn cancel(id: &str) {
    {
        let mut i = mgr().inner.lock().unwrap();
        if let Some(j) = i.jobs.remove(id) {
            j.cancel.store(true, Ordering::Relaxed);
        }
        i.waiting.retain(|x| x != id);
    }
    forget_queued(id);
}

/// Remove a song: stop its job, delete its files and its index row.
pub fn remove(id: &str) {
    if !valid_id(id) {
        return;
    }
    cancel(id);
    let dir = Settings::load().dir;
    delete_files(&dir, id);
    // (the index is used before the state lock is taken, never while holding it)
    let size = with_store(|s| {
        let size = s.get(id).ok().flatten().map_or(0, |x| x.size.max(0));
        let _ = s.remove(id);
        size
    })
    .unwrap_or(0);
    {
        let mut i = mgr().inner.lock().unwrap();
        // only a song that counted as downloaded (its file was in the folder) was part of the total
        if i.have.remove(id) {
            i.used = i.used.saturating_sub(size).max(0);
        }
    }
    emit(Event::Changed(id.to_owned()));
    emit(Event::Stats);
}

pub fn retry(id: &str) {
    let item = mgr().inner.lock().unwrap().jobs.get(id).filter(|j| matches!(j.state, JobState::Failed(_))).map(|j| j.item.clone());
    if let Some(item) = item {
        cancel(id);
        enqueue(item);
    }
}

pub fn pin(id: &str, on: bool) {
    if valid_id(id) {
        with_store(|s| s.set_pinned(id, on));
        emit(Event::Changed(id.to_owned()));
    }
}

/// "Keep offline" for a collection (album / playlist): its songs are downloaded and exempt from eviction.
pub fn keep_offline(collection: &str, items: &[Item], on: bool) {
    init();
    let members: Vec<(String, String)> = items.iter().map(|i| (i.id.clone(), i.to_json().to_string())).collect();
    with_store(|s| s.set_collection(collection, &members, on));
    for i in items {
        emit(Event::Changed(i.id.clone()));
    }
    evaluate();
}

pub fn is_kept_offline(collection: &str) -> bool {
    with_store(|s| s.is_collection_kept(collection).unwrap_or(false)).unwrap_or(false)
}

pub fn played(id: &str) {
    if valid_id(id) {
        with_store(|s| s.touch_played(id, now_secs()));
    }
}

/// Tell the planner which songs are in use right now (the playing one and the next few), so eviction leaves them alone
/// whether or not smart downloads are on. Cheap: it only records them, the next planner run honours them.
pub fn set_playing(ids: Vec<String>) {
    mgr().inner.lock().unwrap().inputs.playing = ids;
}

/// Inputs for the smart rules. `kind` = `"liked"`, `"recent"` or `"queue"` (the next songs, in play order).
pub fn set_inputs(kind: &str, items: Vec<Item>) {
    {
        let mut i = mgr().inner.lock().unwrap();
        let mut items = items;
        items.truncate(MAX_INPUT_ITEMS);
        match kind {
            "liked" => i.inputs.liked = items,
            "recent" => i.inputs.recent = items,
            "queue" => i.inputs.queue = items,
            _ => return,
        }
    }
    evaluate();
}

/// The Downloads page data: running jobs first, then the songs on disk (newest first).
pub fn list_json() -> String {
    let (jobs, have): (Vec<Value>, HashSet<String>) = {
        let i = mgr().inner.lock().unwrap();
        let mut jobs: Vec<Value> = i
            .jobs
            .values()
            .map(|j| {
                let mut v = j.item.to_json();
                let (state, percent, bps, err) = match &j.state {
                    JobState::Queued => ("queued", 0, 0, ""),
                    JobState::Downloading { percent, bps } => ("downloading", *percent, *bps, ""),
                    JobState::Failed(e) => ("failed", 0, 0, e.as_str()),
                };
                v["state"] = json!(state);
                v["percent"] = json!(percent);
                v["speed"] = json!(bps);
                v["error"] = json!(err);
                v["size"] = json!(0);
                v["pinned"] = json!(false);
                v
            })
            .collect();
        jobs.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str()));
        (jobs, i.have.clone())
    };
    let songs = with_store(|s| s.list().unwrap_or_default()).unwrap_or_default();
    let mut out = jobs;
    for s in songs.into_iter().filter(|s| have.contains(&s.video_id)) {
        let item = Item {
            id: s.video_id.clone(),
            title: s.title.clone(),
            artist: s.artist.clone(),
            album: s.album.clone(),
            cover: s.cover.clone(),
            duration_ms: s.duration_ms,
        };
        let mut v = serde_json::to_value(&s).unwrap_or(Value::Null);
        for (k, val) in item.to_json().as_object().into_iter().flatten() {
            if v.get(k).is_none() {
                v[k] = val.clone();
            }
        }
        v["state"] = json!("downloaded");
        v["percent"] = json!(100);
        v["speed"] = json!(0);
        out.push(v);
    }
    Value::Array(out).to_string()
}

// ---- workers -----------------------------------------------------------------------------------

fn kick() {
    let max = Settings::load().max_parallel;
    loop {
        let next = {
            let mut i = mgr().inner.lock().unwrap();
            if is_paused() || i.active >= max {
                return;
            }
            let mut found = None;
            while let Some(id) = i.waiting.pop_front() {
                if let Some(j) = i.jobs.get_mut(&id) {
                    j.state = JobState::Downloading { percent: 0, bps: 0 };
                    found = Some((j.item.clone(), j.cancel.clone()));
                    break;
                }
            }
            match found {
                Some(f) => {
                    i.active += 1;
                    f
                }
                None => return,
            }
        };
        emit(Event::Changed(next.0.id.clone()));
        crate::state::st().rt.spawn(run_job(next.0, next.1));
    }
}

/// Apply a finished job to the shared state. A job only speaks for its song while it is still *the* job of that id
/// (`cancel` is its identity): one that was removed, or replaced by a newer download of the same song, changes
/// nothing. Returns `true` when it left a finished file behind that nobody wants (it completed while being removed).
fn settle(i: &mut Inner, id: &str, cancel: &Arc<AtomicBool>, result: &Result<Song, DlError>) -> bool {
    i.active = i.active.saturating_sub(1);
    let mine = i.jobs.get(id).is_some_and(|j| Arc::ptr_eq(&j.cancel, cancel));
    match result {
        Ok(song) if mine => {
            i.jobs.remove(id);
            if i.have.insert(id.to_owned()) {
                i.used += song.size;
            }
            false
        }
        // (a newer job of the same song will replace the file itself; a song that is downloaded again keeps it)
        Ok(_) => !i.jobs.contains_key(id) && !i.have.contains(id),
        Err(DlError::Cancelled) => false,
        Err(e) => {
            if let Some(j) = i.jobs.get_mut(id).filter(|_| mine) {
                j.state = JobState::Failed(e.message());
            }
            false
        }
    }
}

async fn run_job(item: Item, cancel: Arc<AtomicBool>) {
    let mut result = run_attempts(&item, &cancel).await;
    let (mine, orphaned) = {
        let mut i = mgr().inner.lock().unwrap();
        let mine = i.jobs.get(&item.id).is_some_and(|j| Arc::ptr_eq(&j.cancel, &cancel));
        (mine, settle(&mut i, &item.id, &cancel, &result))
    };
    if mine && done_for_good(&result) {
        forget_queued(&item.id); // (a job that was cancelled or replaced leaves the record to whoever owns it now)
    }
    if orphaned {
        // removed while its last bytes were being written: take the file and the row away again
        delete_files(&Settings::load().dir, &item.id);
        with_store(|s| s.remove(&item.id));
        result = Err(DlError::Cancelled);
    }
    if let Ok(song) = &result {
        crate::covercache::prefetch(&song.cover); // so the Downloads page has its art offline
    }
    emit(Event::Changed(item.id.clone()));
    emit(Event::Stats);
    kick();
    if result.is_ok() {
        evaluate();
    }
}

async fn run_attempts(item: &Item, cancel: &Arc<AtomicBool>) -> Result<Song, DlError> {
    let cfg = Settings::load();
    crate::state::ensure_private_dir(&cfg.dir).map_err(|e| DlError::Io(e.to_string()))?;
    let mut last = DlError::Net("not started".into());
    for attempt in 0..MAX_ATTEMPTS {
        if cancel.load(Ordering::Relaxed) {
            return Err(DlError::Cancelled);
        }
        if attempt > 0 {
            tokio::time::sleep(backoff(attempt - 1)).await;
        }
        match attempt_once(item, &cfg, cancel).await {
            Ok(song) => return Ok(song),
            Err(e) if e.retryable() => {
                if matches!(e, DlError::Net(_)) {
                    // maybe the connection is gone: have it checked now, so the next attempt waits (paused as
                    // "offline") instead of burning the remaining attempts within seconds
                    crate::connectivity::check_now();
                }
                last = e;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

async fn attempt_once(item: &Item, cfg: &Settings, cancel: &Arc<AtomicBool>) -> Result<Song, DlError> {
    while is_paused() && !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let formats = crate::vision::audio_formats_typed(&item.id).await.map_err(formats_error)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(DlError::Cancelled);
    }
    let f = quality::pick(&formats, &cfg.quality).ok_or_else(|| DlError::Unavailable("no downloadable audio stream".into()))?;
    let ext = quality::ext_for_mime(&f.mime);
    let name =
        quality::part_name(&item.id, ext, f.itag, f.content_length).ok_or_else(|| DlError::Unavailable("invalid video id".into()))?;
    let part = cfg.dir.join(&name);
    // only a partial of this very stream can be resumed: one of another container or another quality goes
    remove_parts(&cfg.dir, &item.id, Some(&name));
    // free-space check before writing a lot
    let need = f.content_length.unwrap_or(rules::DEFAULT_SONG_BYTES);
    let free = free_space(&cfg.dir);
    if !rules::fits_free_space(free, need, 64 * 1024 * 1024) {
        return Err(DlError::Io("not enough free disk space".into()));
    }
    let started = Instant::now();
    let mut meter = SpeedMeter::default();
    let mut last_emit: Option<Instant> = None;
    let id = item.id.clone();
    let mut progress = move |done: u64, total: u64| {
        let bps = meter.update(started.elapsed().as_millis() as u64, done);
        if last_emit.is_some_and(|t| t.elapsed() < Duration::from_millis(250)) {
            return;
        }
        last_emit = Some(Instant::now());
        let percent = done.saturating_mul(100).checked_div(total).map_or(0, |p| p.min(100) as i32);
        if let Some(j) = mgr().inner.lock().unwrap().jobs.get_mut(&id) {
            j.state = JobState::Downloading { percent, bps: bps as i64 };
        }
        emit(Event::Progress { id: id.clone(), percent, bps: bps as i64 });
    };
    fetch_to_file(
        &mgr().client,
        &f.url,
        crate::vision::user_agent(),
        &part,
        f.content_length,
        crate::config::get().network.proxy_chunk_bytes,
        &is_paused,
        cancel,
        &mut progress,
    )
    .await?;
    if cancel.load(Ordering::Relaxed) {
        return Err(DlError::Cancelled); // removed during the last chunk: do not put the file in place
    }
    let (mime, dir, item2) = (f.mime.clone(), cfg.dir.clone(), item.clone());
    let ext = ext.to_owned();
    tokio::task::spawn_blocking(move || {
        with_store(|s| finish(s, &dir, &item2, &ext, &mime, &part, now_secs())).unwrap_or(Err("no index".into()))
    })
    .await
    .map_err(|e| DlError::Io(e.to_string()))?
    .map_err(DlError::Io)
}

pub fn free_space(dir: &Path) -> Option<u64> {
    fs4::available_space(dir).ok()
}

// ---- smart downloads ---------------------------------------------------------------------------

/// Re-run the planner against the current state and apply it (cheap; called whenever inputs change).
pub fn evaluate() {
    let cfg = Settings::load();
    let Some(songs) = with_store(|s| s.list().unwrap_or_default()) else { return };
    let pinned_members = with_store(|s| s.pinned_members().unwrap_or_default()).unwrap_or_default();
    let (state, lookup) = {
        let i = mgr().inner.lock().unwrap();
        let mut lookup: HashMap<String, Item> = HashMap::new();
        for it in i.inputs.liked.iter().chain(&i.inputs.recent).chain(&i.inputs.queue) {
            lookup.entry(it.id.clone()).or_insert_with(|| it.clone());
        }
        let mut pinned_wanted = Vec::new();
        for (id, json) in &pinned_members {
            if let Ok(v) = serde_json::from_str::<Value>(json) {
                if let Some(it) = Item::from_json(&v) {
                    lookup.entry(id.clone()).or_insert(it);
                }
            }
            pinned_wanted.push(id.clone());
        }
        let ids = |v: &[Item]| v.iter().map(|x| x.id.clone()).collect::<Vec<_>>();
        let st = PlanState {
            stored: songs
                .iter()
                // only what is in the folder now (rows of an absent folder are kept, but they are not "stored")
                .filter(|s| i.have.contains(&s.video_id))
                .map(|s| Stored {
                    id: s.video_id.clone(),
                    size: s.size.max(0) as u64,
                    added: s.added,
                    last_played: s.last_played,
                    pinned: s.pinned,
                })
                .collect(),
            in_flight: i.jobs.keys().cloned().collect(),
            liked: ids(&i.inputs.liked),
            recent: ids(&i.inputs.recent),
            queue_ahead: ids(&i.inputs.queue),
            pinned_wanted,
            in_use: i.inputs.playing.clone(),
            free_bytes: free_space(&cfg.dir),
        };
        (st, lookup)
    };
    for action in rules::plan(&state, &cfg.rules, now_secs()) {
        match action {
            Action::Evict { id, .. } => remove(&id),
            Action::Download { id, .. } => {
                if let Some(item) = lookup.get(&id) {
                    enqueue_smart(item.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ytm-dl-mgr-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[derive(Default)]
    struct Seen {
        ranges: Mutex<Vec<String>>,
    }

    /// A mock googlevideo. `honour_range = false` makes it answer 200 with the whole body.
    async fn mock(body: Vec<u8>, honour_range: bool, status_override: Option<u16>) -> (u16, Arc<Seen>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = Arc::new(Seen::default());
        let s2 = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = l.accept().await {
                let (body, seen) = (body.clone(), s2.clone());
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let range = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("range: bytes=").map(str::to_owned));
                    seen.ranges.lock().unwrap().push(range.clone().unwrap_or_default());
                    if let Some(code) = status_override {
                        let _ =
                            sock.write_all(format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
                        return;
                    }
                    let total = body.len();
                    let resp = match (range, honour_range) {
                        (Some(r), true) => {
                            let (a, b) = r.split_once('-').unwrap();
                            let a: usize = a.parse().unwrap();
                            if a >= total {
                                let _ = sock.write_all(format!("HTTP/1.1 416 X\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
                                return;
                            }
                            let b: usize = b.parse::<usize>().unwrap().min(total - 1);
                            let mut h = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b + 1 - a).into_bytes();
                            h.extend_from_slice(&body[a..=b]);
                            h
                        }
                        _ => {
                            let mut h = format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n").into_bytes();
                            h.extend_from_slice(&body);
                            h
                        }
                    };
                    let _ = sock.write_all(&resp).await;
                });
            }
        });
        (port, seen)
    }

    async fn fetch(url: &str, part: &Path, total: Option<u64>, chunk: u64) -> Result<u64, DlError> {
        let never = AtomicBool::new(false);
        let mut seen = Vec::new();
        let r =
            fetch_to_file(&reqwest::Client::new(), url, "ua", part, total, chunk, &|| false, &never, &mut |d, t| seen.push((d, t))).await;
        if r.is_ok() {
            assert!(seen.windows(2).all(|w| w[0].0 < w[1].0), "progress only grows");
        }
        r
    }

    #[tokio::test]
    async fn downloads_in_bounded_ranges() {
        let dir = tmpdir("full");
        let body = data(300_000);
        let (port, seen) = mock(body.clone(), true, None).await;
        let part = dir.join("a.part");
        let n = fetch(&format!("http://127.0.0.1:{port}/x"), &part, None, 100_000).await.unwrap();
        assert_eq!(n, 300_000);
        assert_eq!(std::fs::read(&part).unwrap(), body);
        let ranges = seen.ranges.lock().unwrap().clone();
        assert_eq!(ranges, ["0-99999", "100000-199999", "200000-299999"], "never an open-ended range");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&part).unwrap().permissions().mode() & 0o077, 0);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn resumes_from_a_partial_file() {
        let dir = tmpdir("resume");
        let body = data(300_000);
        let (port, seen) = mock(body.clone(), true, None).await;
        let part = dir.join("a.part");
        std::fs::write(&part, &body[..130_000]).unwrap();
        let n = fetch(&format!("http://127.0.0.1:{port}/x"), &part, Some(300_000), 100_000).await.unwrap();
        assert_eq!(n, 300_000);
        assert_eq!(std::fs::read(&part).unwrap(), body, "no gap, no duplicate bytes");
        assert_eq!(
            seen.ranges.lock().unwrap().first().map(String::as_str),
            Some("130000-229999"),
            "first request continues at the partial's end"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_already_complete_partial_needs_no_request() {
        let dir = tmpdir("complete");
        let body = data(70_000);
        let (port, seen) = mock(body.clone(), true, None).await;
        let part = dir.join("a.part");
        std::fs::write(&part, &body).unwrap();
        assert_eq!(fetch(&format!("http://127.0.0.1:{port}/x"), &part, Some(70_000), 1 << 20).await.unwrap(), 70_000);
        assert!(seen.ranges.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_server_that_ignores_ranges_restarts_cleanly() {
        let dir = tmpdir("norange");
        let body = data(150_000);
        let (port, _) = mock(body.clone(), false, None).await;
        let part = dir.join("a.part");
        std::fs::write(&part, &body[..50_000]).unwrap();
        let n = fetch(&format!("http://127.0.0.1:{port}/x"), &part, None, 64 * 1024).await.unwrap();
        assert_eq!(n, 150_000);
        assert_eq!(std::fs::read(&part).unwrap(), body);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_partial_larger_than_the_stream_starts_over() {
        let dir = tmpdir("stale");
        let body = data(100_000);
        let (port, _) = mock(body.clone(), true, None).await;
        let part = dir.join("a.part");
        std::fs::write(&part, vec![9u8; 250_000]).unwrap();
        assert_eq!(fetch(&format!("http://127.0.0.1:{port}/x"), &part, Some(100_000), 64 * 1024).await.unwrap(), 100_000);
        assert_eq!(std::fs::read(&part).unwrap(), body);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn http_errors_are_classified() {
        let dir = tmpdir("errs");
        for (code, retry) in [(404u16, false), (410, false), (403, true), (429, true), (503, true)] {
            let (port, _) = mock(vec![], true, Some(code)).await;
            let e = fetch(&format!("http://127.0.0.1:{port}/x"), &dir.join(format!("{code}.part")), None, 64 * 1024).await.unwrap_err();
            assert_eq!(e, DlError::Http(code));
            assert_eq!(e.retryable(), retry, "{code}");
        }
        assert!(DlError::Net("x".into()).retryable() && DlError::Incomplete.retryable());
        assert!(!DlError::Io("x".into()).retryable() && !DlError::Cancelled.retryable());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_short_body_is_incomplete_not_success() {
        // the server claims 100 000 bytes in Content-Range but sends fewer, then closes
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let first = Arc::new(AtomicBool::new(true));
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let first = first.clone();
                tokio::spawn(async move {
                    let mut b = [0u8; 2048];
                    let _ = s.read(&mut b).await;
                    let reply: &[u8] = if first.swap(false, Ordering::SeqCst) {
                        b"HTTP/1.1 206 P\r\nContent-Range: bytes 0-99999/100000\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789"
                    } else {
                        b"HTTP/1.1 503 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    };
                    let _ = s.write_all(reply).await;
                });
            }
        });
        let dir = tmpdir("short");
        let e = fetch(&format!("http://127.0.0.1:{port}/x"), &dir.join("a.part"), None, 100_000).await;
        assert!(e.is_err(), "a truncated stream must not look finished: {e:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn cancel_and_pause_are_honoured() {
        let dir = tmpdir("cancel");
        let (port, _) = mock(data(200_000), true, None).await;
        let cancelled = AtomicBool::new(true);
        let r = fetch_to_file(
            &reqwest::Client::new(),
            &format!("http://127.0.0.1:{port}/x"),
            "ua",
            &dir.join("a.part"),
            None,
            64 * 1024,
            &|| false,
            &cancelled,
            &mut |_, _| {},
        )
        .await;
        assert_eq!(r, Err(DlError::Cancelled));
        // paused + cancelled together must not hang
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            fetch_to_file(
                &reqwest::Client::new(),
                &format!("http://127.0.0.1:{port}/x"),
                "ua",
                &dir.join("b.part"),
                None,
                64 * 1024,
                &|| true,
                &cancelled,
                &mut |_, _| {},
            ),
        )
        .await;
        assert_eq!(r.unwrap(), Err(DlError::Cancelled));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let s: Vec<u64> = (0..8).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(s, [2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(backoff(u32::MAX).as_secs(), 60);
    }

    #[test]
    fn speed_meter_smooths() {
        let mut m = SpeedMeter::default();
        assert_eq!(m.update(0, 0), 0);
        assert_eq!(m.update(100, 100_000), 0, "window too short to measure");
        let v = m.update(1000, 1_000_000);
        assert!((900_000..=1_100_000).contains(&v), "{v}");
        let slower = m.update(2000, 1_100_000);
        assert!(slower < v && slower > 100_000, "moving average, not a jump: {slower}");
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration_ms("3:45"), 225_000);
        assert_eq!(parse_duration_ms("1:02:03"), 3_723_000);
        for bad in ["", "abc", "1:xx", "-1:00", ":"] {
            assert_eq!(parse_duration_ms(bad), 0, "{bad}");
        }
        // absurdly long input must not overflow (it used to panic in debug builds and wrap in release ones)
        assert_eq!(parse_duration_ms(&["99999"; 14].join(":")), 0);
        assert_eq!(parse_duration_ms(&["99999"; 40].join(":")), 0);
    }

    #[test]
    fn items_need_a_valid_video_id() {
        let ok =
            serde_json::json!({"videoId":"dQw4w9WgXcQ","title":"T","subtitle":"Sub","thumbnail":"https://x.com/a.jpg","duration":"3:32"});
        let it = Item::from_json(&ok).unwrap();
        assert_eq!(
            (it.title.as_str(), it.artist.as_str(), it.cover.as_str(), it.duration_ms),
            ("T", "Sub", "https://x.com/a.jpg", 212_000)
        );
        let both = serde_json::json!({"videoId":"dQw4w9WgXcQ","artist":"A","subtitle":"S","cover":"c","thumbnail":"t","durationMs":5});
        let it = Item::from_json(&both).unwrap();
        assert_eq!((it.artist.as_str(), it.cover.as_str(), it.duration_ms), ("A", "c", 5));
        for bad in ["", "../../etc/passwd", "short", "dQw4w9WgXcQ/../x"] {
            assert!(Item::from_json(&serde_json::json!({"videoId": bad})).is_none(), "{bad}");
        }
        assert!(Item::from_json(&serde_json::json!({"title":"no id"})).is_none());
    }

    #[test]
    fn finishing_renames_records_and_removes() {
        let dir = tmpdir("finish");
        let store = Store::open_memory().unwrap();
        let item = Item { id: "dQw4w9WgXcQ".into(), title: "T".into(), artist: "A".into(), ..Default::default() };
        let part = dir.join("dQw4w9WgXcQ.m4a.part");
        std::fs::write(&part, b"audio!").unwrap();
        let song = finish(&store, &dir, &item, "m4a", "audio/mp4", &part, 1234).unwrap();
        assert_eq!((song.size, song.added), (6, 1234));
        assert!(!part.exists() && dir.join("dQw4w9WgXcQ.m4a").is_file());
        assert_eq!(store.get("dQw4w9WgXcQ").unwrap().unwrap().format, "m4a");
        // a hostile id or extension never reaches the file system
        let bad = Item { id: "../../evil".into(), ..Default::default() };
        let p2 = dir.join("x.part");
        std::fs::write(&p2, b"x").unwrap();
        assert!(finish(&store, &dir, &bad, "m4a", "audio/mp4", &p2, 1).is_err());
        assert!(finish(&store, &dir, &item, "../x", "audio/mp4", &p2, 1).is_err());
        delete_files(&dir, "dQw4w9WgXcQ");
        assert!(!dir.join("dQw4w9WgXcQ.m4a").exists());
        delete_files(&dir, "../../x"); // ignored
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reconcile_drops_rows_without_files() {
        let dir = tmpdir("recon");
        let store = Store::open_memory().unwrap();
        for id in ["aaaaaaaaaaa", "bbbbbbbbbbb"] {
            let item = Item { id: id.into(), ..Default::default() };
            let part = dir.join("p.part");
            std::fs::write(&part, b"x").unwrap();
            finish(&store, &dir, &item, "webm", "audio/webm", &part, 1).unwrap();
        }
        std::fs::remove_file(dir.join("bbbbbbbbbbb.webm")).unwrap();
        assert_eq!(reconcile(&store, &dir), ["bbbbbbbbbbb"]);
        assert_eq!(store.list().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Live roundtrip (network; `just live`): download a 19-second public video's audio at "data saver" quality
    /// into a temp dir, check size and container, then prove resume by cutting the file in half and finishing it.
    /// Needs `XDG_*` pointing at temp dirs (the config is created on first use).
    #[tokio::test]
    #[ignore]
    async fn live_download_roundtrip() {
        let id = "jNQXAC9IVRw";
        let formats = crate::vision::audio_formats(id).await.expect("formats");
        let f = quality::pick(&formats, "data_saver").expect("a format");
        let ext = quality::ext_for_mime(&f.mime);
        println!("{id}: itag {} {} {} bps, {:?} bytes", f.itag, f.mime, f.bitrate, f.content_length);
        let dir = tmpdir("live");
        let part = dir.join(format!("{id}.{ext}.part"));
        let client = reqwest::Client::new();
        let never = AtomicBool::new(false);
        let ua = crate::vision::user_agent();
        let size = fetch_to_file(&client, &f.url, ua, &part, f.content_length, 256 * 1024, &|| false, &never, &mut |_, _| {})
            .await
            .expect("download");
        assert!(size > 10_000, "a real audio file, got {size} bytes");
        assert_eq!(Some(size), f.content_length, "size matches what the format announced");
        let whole = std::fs::read(&part).unwrap();
        let magic = &whole[..12.min(whole.len())];
        match ext {
            "webm" => assert_eq!(&magic[..4], b"\x1aE\xdf\xa3", "EBML header"),
            "m4a" => assert_eq!(&magic[4..8], b"ftyp"),
            other => panic!("unexpected container {other}"),
        }
        // resume: keep the first half, fetch the rest, expect identical bytes
        std::fs::write(&part, &whole[..whole.len() / 2]).unwrap();
        let again = fetch_to_file(&client, &f.url, ua, &part, f.content_length, 256 * 1024, &|| false, &never, &mut |_, _| {})
            .await
            .expect("resume");
        assert_eq!(again, size);
        assert_eq!(std::fs::read(&part).unwrap(), whole, "resumed file is byte-identical");
        // and the finished file is playable through the local hook
        let store = Store::open_memory().unwrap();
        let item = Item { id: id.into(), title: "Me at the zoo".into(), ..Default::default() };
        finish(&store, &dir, &item, ext, &f.mime, &part, 1).unwrap();
        let src = super::super::local_source_in(&dir, id).expect("local source");
        assert_eq!(src.len, size);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_empty_or_missing_folder_does_not_wipe_the_index() {
        let dir = tmpdir("recon-absent");
        let store = Store::open_memory().unwrap();
        for id in ["aaaaaaaaaaa", "bbbbbbbbbbb"] {
            let part = dir.join("p.part");
            std::fs::write(&part, b"xyz").unwrap();
            finish(&store, &dir, &Item { id: id.into(), ..Default::default() }, "webm", "audio/webm", &part, 1).unwrap();
        }
        store.set_pinned("aaaaaaaaaaa", true).unwrap();
        // the drive is not mounted / the setting points at a new place: an empty folder, or none at all
        let empty = tmpdir("recon-empty");
        let missing = dir.join("not-there");
        for absent in [&empty, &missing] {
            assert!(reconcile(&store, absent).is_empty());
            assert_eq!(store.list().unwrap().len(), 2, "rows (and their pins) survive");
            assert_eq!(present(&store, absent), (HashSet::new(), 0), "but nothing counts as downloaded there");
        }
        assert!(store.get("aaaaaaaaaaa").unwrap().unwrap().pinned);
        // back in the real folder everything is there again
        assert!(reconcile(&store, &dir).is_empty());
        let (ids, bytes) = present(&store, &dir);
        assert_eq!((ids.len(), bytes), (2, 6));
        // a folder that has files but lacks one song still loses just that row
        std::fs::remove_file(dir.join("bbbbbbbbbbb.webm")).unwrap();
        assert_eq!(reconcile(&store, &dir), ["bbbbbbbbbbb"]);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(empty);
    }

    #[test]
    fn partials_of_other_streams_are_removed_and_never_resumed() {
        let dir = tmpdir("parts");
        let id = "dQw4w9WgXcQ";
        let normal = quality::part_name(id, "m4a", 140, Some(3_400_000)).unwrap();
        let saver = quality::part_name(id, "m4a", 139, Some(1_300_000)).unwrap();
        let names = [normal.as_str(), saver.as_str(), "dQw4w9WgXcQ.m4a.part", "dQw4w9WgXcQ.251-9.webm.part"];
        for n in names.iter().chain(&["aaaaaaaaaaa.140-1.m4a.part", "movie.mkv.part", "dQw4w9WgXcQ.m4a"]) {
            std::fs::write(dir.join(n), b"partial").unwrap();
        }
        // the quality changed to data saver: its own partial stays, every other partial of the song goes
        remove_parts(&dir, id, Some(&saver));
        assert!(dir.join(&saver).exists());
        for gone in [normal.as_str(), "dQw4w9WgXcQ.m4a.part", "dQw4w9WgXcQ.251-9.webm.part"] {
            assert!(!dir.join(gone).exists(), "{gone}");
        }
        for kept in ["aaaaaaaaaaa.140-1.m4a.part", "movie.mkv.part", "dQw4w9WgXcQ.m4a"] {
            assert!(dir.join(kept).exists(), "{kept}: not a partial of this song");
        }
        delete_files(&dir, id);
        assert!(!dir.join(&saver).exists() && !dir.join("dQw4w9WgXcQ.m4a").exists());
        assert!(dir.join("aaaaaaaaaaa.140-1.m4a.part").exists() && dir.join("movie.mkv.part").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn song(id: &str, size: i64) -> Song {
        Song {
            video_id: id.into(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            cover: String::new(),
            duration_ms: 0,
            size,
            format: "m4a".into(),
            mime: "audio/mp4".into(),
            added: 1,
            last_played: None,
            pinned: false,
        }
    }

    fn inner_with_job(id: &str) -> (Inner, Arc<AtomicBool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut i = Inner { active: 1, ..Default::default() };
        let item = Item { id: id.into(), ..Default::default() };
        i.jobs.insert(id.into(), Job { item, state: JobState::Downloading { percent: 0, bps: 0 }, cancel: cancel.clone() });
        (i, cancel)
    }

    #[test]
    fn a_finished_job_only_counts_while_it_is_still_the_songs_job() {
        let id = "dQw4w9WgXcQ";
        // the normal case
        let (mut i, cancel) = inner_with_job(id);
        assert!(!settle(&mut i, id, &cancel, &Ok(song(id, 10))));
        assert!(i.have.contains(id) && i.jobs.is_empty() && i.used == 10 && i.active == 0);

        // removed while finishing: not downloaded, and the leftover file is reported
        let (mut i, cancel) = inner_with_job(id);
        i.jobs.remove(id);
        assert!(settle(&mut i, id, &cancel, &Ok(song(id, 10))), "the file must be cleaned up");
        assert!(!i.have.contains(id) && i.used == 0);

        // removed and queued again: the old job must not finish or fail the new one
        let (mut i, old) = inner_with_job(id);
        let (newer, _) = inner_with_job(id);
        i.jobs = newer.jobs;
        assert!(!settle(&mut i, id, &old, &Ok(song(id, 10))), "the newer job will replace the file");
        assert!(!i.have.contains(id) && i.jobs.contains_key(id));
        i.active = 1;
        assert!(!settle(&mut i, id, &old, &Err(DlError::Http(404))));
        assert!(matches!(i.jobs[id].state, JobState::Downloading { .. }), "a stale failure does not mark the new job failed");

        // its own failure does
        let (mut i, cancel) = inner_with_job(id);
        assert!(!settle(&mut i, id, &cancel, &Err(DlError::Http(404))));
        assert!(matches!(i.jobs[id].state, JobState::Failed(_)));
        let (mut i, cancel) = inner_with_job(id);
        assert!(!settle(&mut i, id, &cancel, &Err(DlError::Cancelled)));
        assert!(matches!(i.jobs[id].state, JobState::Downloading { .. }));
    }

    fn item(id: &str, title: &str) -> Item {
        Item {
            id: id.into(),
            title: title.into(),
            artist: "A".into(),
            album: "Al".into(),
            cover: "https://x.com/c.jpg".into(),
            duration_ms: 61_000,
        }
    }

    #[test]
    fn queued_downloads_survive_a_restart_in_order() {
        let dir = tmpdir("queue-restart");
        let path = dir.join("downloads.sqlite");
        let asked = vec![item("ccccccccccc", "Third letter first"), item("aaaaaaaaaaa", "A"), item("bbbbbbbbbbb", "B")];
        {
            // first run: the user queues three songs, then quits before any of them is done
            let store = Store::open(&path).unwrap();
            let mut i = Inner::default();
            let (added, records) = queue_manual(&mut i, asked.clone());
            assert!(added && i.waiting == ["ccccccccccc", "aaaaaaaaaaa", "bbbbbbbbbbb"]);
            store.queue_add(&records).unwrap();
        }
        // next run: a fresh state, the same index file
        let store = Store::open(&path).unwrap();
        let again = restorable(&store, &HashSet::new());
        assert_eq!(again, asked, "same songs, same order, with their titles and covers");
        let mut i = Inner::default();
        for it in again {
            assert!(add_job(&mut i, it));
        }
        assert_eq!(i.waiting, ["ccccccccccc", "aaaaaaaaaaa", "bbbbbbbbbbb"]);
        assert!(i.jobs.values().all(|j| j.state == JobState::Queued));
        assert_eq!(store.queued().unwrap().len(), 3, "still remembered until they are done");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn finished_cancelled_and_removed_downloads_do_not_come_back() {
        let store = Store::open_memory().unwrap();
        let mut i = Inner::default();
        let ids = ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc", "ddddddddddd", "eeeeeeeeeee"];
        let (_, records) = queue_manual(&mut i, ids.iter().map(|id| item(id, "T")).collect());
        store.queue_add(&records).unwrap();
        // a: finished (what `run_job` does for its own finished job); b: cancelled / removed (what `cancel` does)
        assert!(done_for_good(&Ok(song("aaaaaaaaaaa", 1))));
        store.queue_remove("aaaaaaaaaaa").unwrap();
        store.queue_remove("bbbbbbbbbbb").unwrap();
        // c: failed for good goes, d: failed but worth another try stays
        assert!(done_for_good(&Err(DlError::Unavailable("private".into()))) && done_for_good(&Err(DlError::Http(404))));
        store.queue_remove("ccccccccccc").unwrap();
        assert!(!done_for_good(&Err(DlError::Net("offline".into()))) && !done_for_good(&Err(DlError::Incomplete)));
        // e: its record is still there, but the file arrived some other way (it is downloaded)
        let have: HashSet<String> = ["eeeeeeeeeee".to_owned()].into();
        let again: Vec<String> = restorable(&store, &have).into_iter().map(|x| x.id).collect();
        assert_eq!(again, ["ddddddddddd"]);
        assert_eq!(store.queued().unwrap().len(), 1, "the record of the downloaded song was dropped too");
        // an unreadable record is dropped instead of blocking the others
        store
            .queue_add(&[
                ("fffffffffff".to_owned(), "not json".to_owned()),
                ("ggggggggggg".to_owned(), "{\"videoId\":\"other\"}".to_owned()),
            ])
            .unwrap();
        assert_eq!(restorable(&store, &HashSet::new()).len(), 1);
        assert_eq!(store.queued().unwrap().len(), 1);
    }

    #[test]
    fn only_downloads_the_user_asked_for_are_remembered() {
        let store = Store::open_memory().unwrap();
        let mut i = Inner::default();
        i.have.insert("hhhhhhhhhhh".into());
        // the smart rules queue songs through `add_job` alone: a job, but nothing to remember
        assert!(add_job(&mut i, item("sssssssssss", "smart")) && add_job(&mut i, item("ttttttttttt", "smart too")));
        assert!(store.queued().unwrap().is_empty());
        assert!(restorable(&store, &i.have).is_empty(), "after a restart the planner decides about them again");
        // the user asks for: a new song, one the smart rules had queued, a downloaded one and nonsense
        let (added, records) = queue_manual(
            &mut i,
            vec![item("mmmmmmmmmmm", "manual"), item("sssssssssss", "smart"), item("hhhhhhhhhhh", "have"), item("../x", "bad")],
        );
        assert!(added);
        store.queue_add(&records).unwrap();
        let remembered: Vec<String> = store.queued().unwrap().into_iter().map(|x| x.0).collect();
        assert_eq!(remembered, ["mmmmmmmmmmm", "sssssssssss"], "not the other smart one, not the downloaded one");
        assert_eq!(i.waiting, ["sssssssssss", "ttttttttttt", "mmmmmmmmmmm"], "no duplicate job for the one already queued");
        // asking again for something queued adds no job
        assert!(!queue_manual(&mut i, vec![item("mmmmmmmmmmm", "manual")]).0);
    }

    #[test]
    fn a_failed_lookup_is_retried_only_when_the_network_was_the_problem() {
        use crate::vision::FormatsError;
        assert!(formats_error(FormatsError::Transient("dns".into())).retryable(), "a blip must not fail the song for good");
        assert!(!formats_error(FormatsError::Unavailable("private".into())).retryable());
    }

    #[test]
    fn the_first_smart_pass_is_due_without_clock_arithmetic() {
        assert!(smart_due(None));
        assert!(!smart_due(Some(Instant::now())));
    }

    #[test]
    fn the_downloads_folder_defaults_into_the_data_dir() {
        let data = Path::new("/data/yqt");
        assert_eq!(dir_for("", data), Path::new("/data/yqt/downloads"));
        assert_eq!(dir_for("   ", data), Path::new("/data/yqt/downloads"));
        assert_eq!(dir_for("/music/offline", data), Path::new("/music/offline"));
    }
}
