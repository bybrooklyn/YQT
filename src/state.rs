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
/// (unix), fsynced, then renamed over the target.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
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

/// Per-user data (cookies, session, RustyPipe cache). `dirs` picks the right place per OS; on Linux
/// that is `$XDG_DATA_HOME/ytm`, exactly where older versions already kept their files.
/// The directory is private (0700) and old permissions are tightened once per run.
pub fn data_dir() -> PathBuf {
    static HARDENED: OnceLock<()> = OnceLock::new();
    let dir = dirs::data_dir().unwrap_or_else(|| ".".into()).join("ytm");
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
            rt: tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime"),
            rp: builder.build().expect("rustypipe"),
        }
    })
}

/// Shared HTTP client for the small third-party/API calls (lyrics, likes, playlists).
pub fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        let n = &crate::config::get().network;
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(n.connect_timeout_secs))
            .timeout(std::time::Duration::from_secs(n.request_timeout_secs))
            .user_agent("ytm-qt/0.1 (GPL-3.0 YouTube Music client)")
            .build()
            .expect("http client")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
