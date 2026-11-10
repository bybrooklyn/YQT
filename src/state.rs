// SPDX-License-Identifier: GPL-3.0-only
//! Process-wide tokio runtime + RustyPipe client shared by the Qt objects, plus the private-file helpers.
use rustypipe::client::RustyPipe;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};
use tokio::runtime::Runtime;

pub struct State {
    pub rt: Runtime,
    pub rp: RustyPipe,
}

/// Name of the folder RustyPipe writes failure reports into (inside the data dir).
pub const REPORTS_DIR: &str = "rustypipe_reports";
/// Never keep more than this many failure reports, whatever the retention says.
pub const MAX_REPORTS: usize = 50;

// ---------------------------------------------------------------------------------------------
// private files
// ---------------------------------------------------------------------------------------------

/// Remove group/other access from `path` (never adds permissions). No-op on non-unix and on symlinks.
pub fn tighten(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::symlink_metadata(path) else { return };
        if meta.file_type().is_symlink() {
            return;
        }
        let mode = meta.permissions().mode();
        let new = mode & !0o077;
        if new != mode & 0o7777 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(new));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Create `dir` (and parents) if missing; the leaf is created 0700 on unix, and an existing leaf is tightened.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    tighten(dir);
    Ok(())
}

/// Atomically write `bytes` to `path` as a private file: temp file in the same directory created 0600
/// (unix), fsynced, then renamed over the target. Every call has its own temp file, so two writers of the same
/// path (two threads caching the same cover) cannot delete or rename each other's: the last rename simply wins.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{name}.{}.{serial}.tmp", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Tighten what older versions left world-readable: the dir itself, its top-level entries, and the
/// contents of the reports and logs folders. Never loosens anything.
pub fn harden_tree(dir: &Path) {
    tighten(dir);
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        tighten(&p);
        let sub = e.file_name();
        if sub == REPORTS_DIR || sub == "logs" {
            if let Ok(inner) = std::fs::read_dir(&p) {
                for f in inner.flatten() {
                    tighten(&f.path());
                }
            }
        }
    }
}

/// The per-user folder the system reported, or an error: without one (HOME unset, a stripped-down service
/// environment) the app must not fall back to the current directory, which could be anyone's and would then receive
/// the session cookie, the queue and the logs.
pub fn resolve_user_dir(found: Option<PathBuf>, what: &str) -> Result<PathBuf, String> {
    match found {
        Some(p) if p.is_absolute() => Ok(p),
        _ => Err(format!("cannot find your {what} folder (is HOME set?); refusing to store your data in the current directory")),
    }
}

/// [`resolve_user_dir`], ending the program with that message: there is no sensible way to carry on.
pub fn user_dir_or_exit(found: Option<PathBuf>, what: &str) -> PathBuf {
    resolve_user_dir(found, what).unwrap_or_else(|msg| {
        eprintln!("yqt: {msg}");
        std::process::exit(1)
    })
}

/// Per-user data (cookies, session, RustyPipe cache). `dirs` picks the right place per OS; on Linux
/// that is `$XDG_DATA_HOME/yqt` (moved there from `ytm` once, by `migrate`).
/// The directory is private (0700) and old permissions are tightened once per run.
/// Fewer malloc arenas: glibc otherwise gives busy threads (Qt, tokio, the GPU driver) their own arena of up to
/// 64 MB, and what is freed inside one is rarely returned. Call first thing in `main`, before any thread exists;
/// `MALLOC_ARENA_MAX` in the environment still wins.
pub fn tune_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        // SAFETY: plain libc call with valid constants, made while the process is still single-threaded.
        unsafe {
            libc::mallopt(libc::M_ARENA_MAX, 2);
        }
    }
}

/// Give memory freed by closed pages back to the system (glibc keeps it otherwise). Cheap; a no-op elsewhere.
pub fn trim_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim has no preconditions.
    unsafe {
        libc::malloc_trim(0);
    }
}

pub fn data_dir() -> PathBuf {
    static HARDENED: OnceLock<()> = OnceLock::new();
    let dir = user_dir_or_exit(dirs::data_dir(), "data").join("yqt");
    let _ = ensure_private_dir(&dir);
    HARDENED.get_or_init(|| harden_tree(&dir));
    dir
}

// ---------------------------------------------------------------------------------------------
// RustyPipe failure reports
// ---------------------------------------------------------------------------------------------

/// Delete reports older than `retention_days` (0 = all of them) and then the oldest beyond `max_keep`.
/// Returns how many files were removed. Only regular `*.json` files directly in `dir` are touched.
pub fn prune_reports(dir: &Path, retention_days: u32, max_keep: usize, now: SystemTime) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let cutoff = now.checked_sub(Duration::from_secs(u64::from(retention_days) * 86_400));
    let mut keep = Vec::new();
    let mut removed = 0;
    for e in rd.flatten() {
        let p = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
        if !meta.is_file() || p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let too_old = retention_days == 0 || cutoff.is_some_and(|c| modified < c);
        if too_old && std::fs::remove_file(&p).is_ok() {
            removed += 1;
        } else {
            keep.push((modified, p));
        }
    }
    keep.sort_by_key(|(m, _)| std::cmp::Reverse(*m)); // newest first
    for (_, p) in keep.into_iter().skip(max_keep) {
        if std::fs::remove_file(p).is_ok() {
            removed += 1;
        }
    }
    removed
}

pub fn st() -> &'static State {
    static S: OnceLock<State> = OnceLock::new();
    S.get_or_init(|| {
        let dir = data_dir();
        let sec = &crate::config::get().security;
        let reports = dir.join(REPORTS_DIR);
        let mut builder = RustyPipe::builder().storage_dir(&dir);
        if sec.report_retention_days == 0 {
            // Keep none: wipe what is there and stop RustyPipe from writing new ones.
            prune_reports(&reports, 0, 0, SystemTime::now());
            builder = builder.no_reporter();
        } else {
            let _ = ensure_private_dir(&reports);
            prune_reports(&reports, sec.report_retention_days, MAX_REPORTS, SystemTime::now());
        }
        State {
            rt: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .max_blocking_threads(8) // covers, accent colours, downloads: a burst must not spawn dozens of threads
                .enable_all()
                .build()
                .expect("tokio runtime"),
            rp: builder.build().expect("rustypipe"),
        }
    })
}

/// Read a response body, but never more than `max` bytes: a hostile or broken server cannot make us buffer without
/// bound (the request timeout limits time, this limits memory).
pub async fn read_limited(mut resp: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    if resp.content_length().is_some_and(|l| l > max as u64) {
        return Err("the response was larger than allowed".to_owned());
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.without_url().to_string())? {
        if buf.len() + chunk.len() > max {
            return Err("the response was larger than allowed".to_owned());
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Upper bounds for bodies we parse: API answers and the watch page.
pub const MAX_API_BODY: usize = 8 * 1024 * 1024;
pub const MAX_PAGE_BODY: usize = 8 * 1024 * 1024;

/// Shared HTTP client for the small third-party/API calls (lyrics, likes, playlists).
pub fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        let n = &crate::config::get().network;
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(n.connect_timeout_secs))
            .timeout(std::time::Duration::from_secs(n.request_timeout_secs))
            .user_agent("yqt/0.1 (GPL-3.0 YouTube Music client)")
            .build()
            .expect("http client")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_home_folder_the_current_directory_is_never_used() {
        assert_eq!(resolve_user_dir(Some(PathBuf::from("/home/u/.local/share")), "data").unwrap(), PathBuf::from("/home/u/.local/share"));
        assert!(resolve_user_dir(None, "data").is_err());
        assert!(resolve_user_dir(Some(PathBuf::from("relative/dir")), "data").is_err(), "a relative path would depend on the cwd");
        assert!(resolve_user_dir(Some(PathBuf::from(".")), "config").is_err());
        let msg = resolve_user_dir(None, "config").unwrap_err();
        assert!(msg.contains("config") && msg.contains("HOME"), "{msg}");
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ytm-state-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[cfg(unix)]
    fn chmod(p: &Path, m: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_dir_is_0700_and_nested_parents_work() {
        let root = tmpdir("dir");
        let d = root.join("a").join("b");
        ensure_private_dir(&d).unwrap();
        assert_eq!(mode(&d), 0o700);
        chmod(&d, 0o755);
        ensure_private_dir(&d).unwrap(); // existing dir gets tightened
        assert_eq!(mode(&d), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn write_private_is_0600_atomic_and_leaves_no_temp() {
        let root = tmpdir("wp");
        let f = root.join("session.json");
        write_private(&f, b"one").unwrap();
        assert_eq!(mode(&f), 0o600);
        write_private(&f, b"two").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"two");
        assert_eq!(mode(&f), 0o600);
        let names: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["session.json"]);
    }

    #[test]
    fn concurrent_writers_of_one_file_all_succeed_and_leave_a_whole_file() {
        let root = tmpdir("wp-race");
        let f = root.join("cover");
        for round in 0..40u8 {
            let writers: Vec<_> = (0..4u8)
                .map(|w| {
                    let f = f.clone();
                    std::thread::spawn(move || write_private(&f, &vec![round.wrapping_add(w); 64 * 1024]))
                })
                .collect();
            for w in writers {
                w.join().unwrap().expect("no writer may fail because of another one");
            }
            let got = std::fs::read(&f).unwrap();
            assert_eq!(got.len(), 64 * 1024);
            assert!(got.iter().all(|b| *b == got[0]), "the file is one writer's bytes, never a mix");
        }
        let names: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["cover"], "no temp file lingers");
    }

    #[cfg(unix)]
    #[test]
    fn write_private_replaces_a_world_readable_file_with_a_private_one() {
        let root = tmpdir("wp2");
        let f = root.join("config.toml");
        std::fs::write(&f, "old").unwrap();
        chmod(&f, 0o644);
        write_private(&f, b"new").unwrap();
        assert_eq!(mode(&f), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn tighten_only_removes_bits() {
        let root = tmpdir("tight");
        let a = root.join("a");
        std::fs::write(&a, "x").unwrap();
        chmod(&a, 0o644);
        tighten(&a);
        assert_eq!(mode(&a), 0o600);
        let b = root.join("b");
        std::fs::write(&b, "x").unwrap();
        chmod(&b, 0o400);
        tighten(&b);
        assert_eq!(mode(&b), 0o400, "an already tighter mode must not be loosened");
        let c = root.join("c");
        std::fs::write(&c, "x").unwrap();
        chmod(&c, 0o755);
        tighten(&c);
        assert_eq!(mode(&c), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn harden_tree_fixes_old_layout() {
        let root = tmpdir("harden");
        let reports = root.join(REPORTS_DIR);
        std::fs::create_dir_all(&reports).unwrap();
        let rep = reports.join("r.json");
        let sess = root.join("session.json");
        std::fs::write(&rep, "{}").unwrap();
        std::fs::write(&sess, "{}").unwrap();
        for p in [&root, &reports, &rep, &sess] {
            chmod(p, if p.is_dir() { 0o755 } else { 0o644 });
        }
        harden_tree(&root);
        assert_eq!((mode(&root), mode(&reports), mode(&rep), mode(&sess)), (0o700, 0o700, 0o600, 0o600));
    }

    #[test]
    fn prune_by_age_count_and_zero() {
        let root = tmpdir("prune");
        let now = SystemTime::now();
        let mk = |name: &str, age_days: u64| {
            let p = root.join(name);
            std::fs::write(&p, "{}").unwrap();
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(now - Duration::from_secs(age_days * 86_400 + 60)).unwrap();
            p
        };
        let fresh = mk("fresh.json", 1);
        let old = mk("old.json", 30);
        let other = mk("notes.txt", 90); // not a report: never touched
        assert_eq!(prune_reports(&root, 7, 50, now), 1);
        assert!(fresh.exists() && !old.exists() && other.exists());

        for i in 0..5 {
            mk(&format!("r{i}.json"), i);
        }
        // 6 json files now (fresh + r0..r4); keep newest 3
        assert_eq!(prune_reports(&root, 7, 3, now), 3);
        assert_eq!(std::fs::read_dir(&root).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).count(), 3);

        assert_eq!(prune_reports(&root, 0, 50, now), 3);
        assert!(other.exists());
        assert_eq!(prune_reports(&root.join("missing"), 0, 0, now), 0);
    }
}
