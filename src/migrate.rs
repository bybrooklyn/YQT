// SPDX-License-Identifier: GPL-3.0-only
//! One-time move of the per-user folders from the old `ytm` name to `yqt`, so a rename never costs anyone their
//! sign-in, queue, downloads or settings. Runs first thing in `main()`, before anything reads those folders.
//!
//! Nothing of the user's is ever deleted: if a rename is not possible (another file system) the tree is copied and
//! the old one stays where it was. If both folders already exist, neither is changed and a warning is printed.
use std::path::{Path, PathBuf};

const OLD: &str = "ytm";
const NEW: &str = "yqt";
const MARKER: &str = ".migrated-from-ytm";

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to do (no old folder, or already migrated).
    Nothing,
    Moved,
    Copied,
    /// Both exist: left alone.
    Conflict,
    /// The old folder is a symbolic link (the user's own arrangement): not followed, not moved.
    Symlink,
    Failed(String),
}

/// Where a copy is assembled before it takes the new name, so a copy that was interrupted is never mistaken for the
/// finished folder.
const PARTIAL: &str = "yqt.partial";

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let ty = e.file_type()?;
        if ty.is_dir() {
            copy_tree(&src, &dst)?;
        } else if ty.is_file() {
            std::fs::copy(&src, &dst)?; // symlinks are skipped on purpose
        }
    }
    Ok(())
}

/// Move `<base>/ytm` to `<base>/yqt`.
pub fn migrate_dir(base: &Path) -> Outcome {
    let (old, new) = (base.join(OLD), base.join(NEW));
    if !old.is_dir() {
        return Outcome::Nothing;
    }
    if std::fs::symlink_metadata(&old).is_ok_and(|m| m.file_type().is_symlink()) {
        // Say so (once the new folder exists the user has moved on, or started fresh knowingly).
        return if new.exists() { Outcome::Nothing } else { Outcome::Symlink };
    }
    if new.exists() {
        return if new.join(MARKER).exists() { Outcome::Nothing } else { Outcome::Conflict };
    }
    let partial = base.join(PARTIAL);
    let _ = std::fs::remove_dir_all(&partial); // only ever our own unfinished copy from a run that was killed
    if std::fs::rename(&old, &new).is_ok() {
        let _ = std::fs::write(new.join(MARKER), NOTE);
        return Outcome::Moved;
    }
    match copy_complete(&old, &partial, &new) {
        Ok(()) => Outcome::Copied,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&partial);
            Outcome::Failed(e.to_string())
        }
    }
}

const NOTE: &str = "Moved here from the old `ytm` folder.\n";

/// Copy `old` into the side folder `partial` and give it the name `new` only when it is complete (marker included):
/// being killed half-way leaves `yqt.partial`, which the next run throws away, never a half-filled `yqt`.
fn copy_complete(old: &Path, partial: &Path, new: &Path) -> std::io::Result<()> {
    copy_tree(old, partial)?;
    std::fs::write(partial.join(MARKER), NOTE)?;
    std::fs::rename(partial, new)
}

fn copy_if_absent(old: &Path, new: &Path) -> Outcome {
    if !old.is_file() || new.exists() {
        return Outcome::Nothing;
    }
    match std::fs::copy(old, new) {
        Ok(_) => Outcome::Copied,
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Qt's QSettings file (volume, shuffle, repeat, recent searches). On Linux it is named after the organization
/// *name*, which was `ytm` and is `yqt`: `<config>/ytm/ytm.conf` then, `<config>/yqt/yqt.conf` now. The folder move
/// above brings the old file along as `<config>/yqt/ytm.conf`; when the folder could not be moved it is still in
/// `<config>/ytm`. Run after [`migrate_dir`]. The file is copied, never moved, and never over an existing one.
/// (`<config>/kde.org/ytm.conf`, the organization *domain*, is copied too: harmless where it does not exist.)
pub fn migrate_settings(config_base: &Path) -> Outcome {
    let new = config_base.join(NEW).join("yqt.conf");
    let mut outcome = Outcome::Nothing;
    for old in [config_base.join(NEW).join("ytm.conf"), config_base.join(OLD).join("ytm.conf")] {
        if outcome == Outcome::Nothing && config_base.join(NEW).is_dir() {
            outcome = copy_if_absent(&old, &new);
        }
    }
    let by_domain = copy_if_absent(&config_base.join("kde.org").join("ytm.conf"), &config_base.join("kde.org").join("yqt.conf"));
    if outcome == Outcome::Nothing {
        by_domain
    } else {
        outcome
    }
}

/// macOS keeps QSettings in `~/Library/Preferences/<reversed domain>.<application>.plist`. The domain was `kde.org`
/// (as `ytm`, then as `yqt`) and is `bybrooklyn.dev`; the newest old file wins.
pub fn migrate_mac_settings(home: &Path) -> Outcome {
    let prefs = home.join("Library").join("Preferences");
    let new = prefs.join("dev.bybrooklyn.yqt.plist");
    match copy_if_absent(&prefs.join("org.kde.yqt.plist"), &new) {
        Outcome::Nothing => copy_if_absent(&prefs.join("org.kde.ytm.plist"), &new),
        done => done,
    }
}

fn report(what: &str, o: &Outcome) {
    match o {
        Outcome::Nothing => {}
        Outcome::Moved => eprintln!("yqt: moved your {what} from the old `ytm` folder"),
        Outcome::Copied => eprintln!("yqt: copied your {what} from the old `ytm` folder (the old one is kept)"),
        Outcome::Conflict => eprintln!("yqt: both a `ytm` and a `yqt` {what} folder exist; using `yqt` and leaving `ytm` alone"),
        Outcome::Symlink => eprintln!(
            "yqt: your old `ytm` {what} folder is a symbolic link, so it was not moved; starting with a new `yqt` folder (point a `yqt` link at the same place to keep using it)"
        ),
        Outcome::Failed(e) => eprintln!("yqt: could not move your {what} from `ytm`: {e}"),
    }
}

/// What the registry copy did, in the terms the other migrations are reported in.
fn registry_outcome(r: Result<bool, String>) -> Outcome {
    match r {
        Ok(true) => Outcome::Copied,
        Ok(false) => Outcome::Nothing,
        Err(e) => Outcome::Failed(e),
    }
}

/// Migrate the data, config and cache folders and the Qt settings (a file, or the registry on Windows).
pub fn run() {
    let bases: [(&str, Option<PathBuf>); 3] = [("data", dirs::data_dir()), ("config", dirs::config_dir()), ("cache", dirs::cache_dir())];
    for (what, base) in bases {
        if let Some(base) = base.filter(|b| b.is_absolute()) {
            report(what, &migrate_dir(&base));
            if what == "config" {
                report("settings", &migrate_settings(&base));
            }
        }
    }
    // Windows keeps QSettings in the registry (`HKCU\Software\ytm\ytm`); a no-op everywhere else.
    report("settings", &registry_outcome(crate::platform::migrate_registry_settings()));
    if cfg!(target_os = "macos") {
        if let Some(home) = dirs::home_dir().filter(|h| h.is_absolute()) {
            report("settings", &migrate_mac_settings(&home));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("yqt-migrate-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn moves_once_and_keeps_contents() {
        let b = tmp("move");
        std::fs::create_dir_all(b.join("ytm/sub")).unwrap();
        std::fs::write(b.join("ytm/session.json"), "x").unwrap();
        std::fs::write(b.join("ytm/sub/a"), "y").unwrap();
        assert_eq!(migrate_dir(&b), Outcome::Moved);
        assert_eq!(std::fs::read_to_string(b.join("yqt/session.json")).unwrap(), "x");
        assert!(b.join("yqt/sub/a").is_file() && b.join("yqt").join(MARKER).is_file() && !b.join("ytm").exists());
        assert_eq!(migrate_dir(&b), Outcome::Nothing); // second run changes nothing
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn nothing_to_do_and_conflicts_touch_nothing() {
        let b = tmp("conflict");
        assert_eq!(migrate_dir(&b), Outcome::Nothing);
        std::fs::create_dir_all(b.join("ytm")).unwrap();
        std::fs::write(b.join("ytm/old"), "o").unwrap();
        std::fs::create_dir_all(b.join("yqt")).unwrap();
        assert_eq!(migrate_dir(&b), Outcome::Conflict);
        assert!(b.join("ytm/old").is_file() && !b.join("yqt/old").exists());
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn copy_fallback_keeps_the_original() {
        let b = tmp("copy");
        std::fs::create_dir_all(b.join("ytm/d")).unwrap();
        std::fs::write(b.join("ytm/d/f"), "z").unwrap();
        copy_tree(&b.join("ytm"), &b.join("yqt")).unwrap();
        assert!(b.join("ytm/d/f").is_file() && b.join("yqt/d/f").is_file());
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn copy_route_is_all_or_nothing() {
        // a copy that was killed half-way left `yqt.partial`: it is discarded, never used as the new folder
        let b = tmp("partial");
        std::fs::create_dir_all(b.join("ytm")).unwrap();
        std::fs::write(b.join("ytm/session.json"), "x").unwrap();
        std::fs::create_dir_all(b.join(PARTIAL)).unwrap();
        std::fs::write(b.join(PARTIAL).join("stale"), "half").unwrap();
        assert!(!b.join("yqt").exists(), "an unfinished copy must not count as the migrated folder");
        assert_eq!(migrate_dir(&b), Outcome::Moved);
        assert!(b.join("yqt/session.json").is_file() && !b.join("yqt/stale").exists() && !b.join(PARTIAL).exists());
        let _ = std::fs::remove_dir_all(b);

        // the copy itself: the new name appears only with everything in it, marker included, and the old stays
        let b = tmp("partial2");
        std::fs::create_dir_all(b.join("ytm/d")).unwrap();
        std::fs::write(b.join("ytm/d/f"), "z").unwrap();
        copy_complete(&b.join("ytm"), &b.join(PARTIAL), &b.join("yqt")).unwrap();
        assert!(b.join("yqt/d/f").is_file() && b.join("yqt").join(MARKER).is_file() && b.join("ytm/d/f").is_file());
        assert!(!b.join(PARTIAL).exists());
        assert_eq!(migrate_dir(&b), Outcome::Nothing); // already migrated

        // a source that cannot be read leaves no `yqt` behind
        assert!(copy_complete(&b.join("missing"), &b.join(PARTIAL), &b.join("yqt2")).is_err());
        assert!(!b.join("yqt2").exists());
        let _ = std::fs::remove_dir_all(b);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_old_folder_is_reported_not_followed() {
        let b = tmp("symlink");
        std::fs::create_dir_all(b.join("elsewhere")).unwrap();
        std::fs::write(b.join("elsewhere/session.json"), "x").unwrap();
        std::os::unix::fs::symlink(b.join("elsewhere"), b.join("ytm")).unwrap();
        assert_eq!(migrate_dir(&b), Outcome::Symlink);
        assert!(b.join("elsewhere/session.json").is_file() && !b.join("yqt").exists());
        std::fs::create_dir_all(b.join("yqt")).unwrap();
        assert_eq!(migrate_dir(&b), Outcome::Nothing); // not nagging once the new folder is in use
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn qt_settings_follow_the_organization_name_on_linux() {
        // what the old app wrote: <config>/ytm/{config.toml, ytm.conf}
        let b = tmp("qs-name");
        std::fs::create_dir_all(b.join("ytm")).unwrap();
        std::fs::write(b.join("ytm/config.toml"), "").unwrap();
        std::fs::write(b.join("ytm/ytm.conf"), "[player]\nvolume=0.4\n").unwrap();
        assert_eq!(migrate_dir(&b), Outcome::Moved);
        assert_eq!(migrate_settings(&b), Outcome::Copied);
        assert_eq!(std::fs::read_to_string(b.join("yqt/yqt.conf")).unwrap(), "[player]\nvolume=0.4\n");
        assert!(b.join("yqt/ytm.conf").is_file(), "nothing is deleted");
        std::fs::write(b.join("yqt/yqt.conf"), "mine").unwrap();
        assert_eq!(migrate_settings(&b), Outcome::Nothing);
        assert_eq!(std::fs::read_to_string(b.join("yqt/yqt.conf")).unwrap(), "mine");
        let _ = std::fs::remove_dir_all(b);

        // both folders exist (conflict): the old settings file is still picked up from <config>/ytm
        let b = tmp("qs-conflict");
        std::fs::create_dir_all(b.join("ytm")).unwrap();
        std::fs::create_dir_all(b.join("yqt")).unwrap();
        std::fs::write(b.join("ytm/ytm.conf"), "old").unwrap();
        assert_eq!(migrate_settings(&b), Outcome::Copied);
        assert_eq!(std::fs::read_to_string(b.join("yqt/yqt.conf")).unwrap(), "old");
        let _ = std::fs::remove_dir_all(b);

        // no new folder (nothing was migrated): nothing is created
        let b = tmp("qs-none");
        std::fs::create_dir_all(b.join("ytm")).unwrap();
        std::fs::write(b.join("ytm/ytm.conf"), "old").unwrap();
        assert_eq!(migrate_settings(&b), Outcome::Nothing);
        assert!(!b.join("yqt").exists());
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn registry_copy_is_reported_like_the_other_migrations() {
        assert_eq!(registry_outcome(Ok(true)), Outcome::Copied);
        assert_eq!(registry_outcome(Ok(false)), Outcome::Nothing);
        assert_eq!(registry_outcome(Err("error 5".into())), Outcome::Failed("error 5".into()));
    }

    #[test]
    fn mac_settings_plist_is_copied_once() {
        let b = tmp("qs-mac");
        std::fs::create_dir_all(b.join("Library/Preferences")).unwrap();
        assert_eq!(migrate_mac_settings(&b), Outcome::Nothing);
        std::fs::write(b.join("Library/Preferences/org.kde.ytm.plist"), "p").unwrap();
        assert_eq!(migrate_mac_settings(&b), Outcome::Copied);
        assert_eq!(migrate_mac_settings(&b), Outcome::Nothing);
        assert!(b.join("Library/Preferences/dev.bybrooklyn.yqt.plist").is_file());
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn qt_settings_are_copied_not_overwritten() {
        let b = tmp("qs");
        std::fs::create_dir_all(b.join("kde.org")).unwrap();
        std::fs::write(b.join("kde.org/ytm.conf"), "[player]\nvolume=0.4\n").unwrap();
        assert_eq!(migrate_settings(&b), Outcome::Copied);
        std::fs::write(b.join("kde.org/yqt.conf"), "mine").unwrap();
        assert_eq!(migrate_settings(&b), Outcome::Nothing);
        assert_eq!(std::fs::read_to_string(b.join("kde.org/yqt.conf")).unwrap(), "mine");
        let _ = std::fs::remove_dir_all(b);
    }
}
