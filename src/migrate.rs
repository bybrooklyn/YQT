// SPDX-License-Identifier: GPL-3.0-only
//! One-time move of the per-user folders from the old `ytm` name to `yqt`, so a rename never costs anyone their
//! sign-in, queue, downloads or settings. Runs first thing in `main()`, before anything reads those folders.
//!
//! Nothing is ever deleted: if a rename is not possible (another file system) the tree is copied and the old one
//! stays where it was. If both folders already exist, nothing is touched and a warning is printed.
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
    Failed(String),
}

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
    if !old.is_dir() || std::fs::symlink_metadata(&old).is_ok_and(|m| m.file_type().is_symlink()) {
        return Outcome::Nothing;
    }
    if new.exists() {
        return if new.join(MARKER).exists() { Outcome::Nothing } else { Outcome::Conflict };
    }
    let outcome = match std::fs::rename(&old, &new) {
        Ok(()) => Outcome::Moved,
        Err(_) => match copy_tree(&old, &new) {
            Ok(()) => Outcome::Copied,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&new); // only our half-made copy
                return Outcome::Failed(e.to_string());
            }
        },
    };
    let _ = std::fs::write(new.join(MARKER), "Moved here from the old `ytm` folder.\n");
    outcome
}

/// Qt's QSettings file (volume, shuffle, repeat): `<config>/kde.org/ytm.conf` to `yqt.conf`.
pub fn migrate_settings(config_base: &Path) -> Outcome {
    let (old, new) = (config_base.join("kde.org").join("ytm.conf"), config_base.join("kde.org").join("yqt.conf"));
    if !old.is_file() || new.exists() {
        return Outcome::Nothing;
    }
    match std::fs::copy(&old, &new) {
        Ok(_) => Outcome::Copied,
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

fn report(what: &str, o: &Outcome) {
    match o {
        Outcome::Nothing => {}
        Outcome::Moved => eprintln!("yqt: moved your {what} from the old `ytm` folder"),
        Outcome::Copied => eprintln!("yqt: copied your {what} from the old `ytm` folder (the old one is kept)"),
        Outcome::Conflict => eprintln!("yqt: both a `ytm` and a `yqt` {what} folder exist; using `yqt` and leaving `ytm` alone"),
        Outcome::Failed(e) => eprintln!("yqt: could not move your {what} from `ytm`: {e}"),
    }
}

/// Migrate the data, config and cache folders and the Qt settings file.
pub fn run() {
    let bases: [(&str, Option<PathBuf>); 3] =
        [("data", dirs::data_dir()), ("config", dirs::config_dir()), ("cache", dirs::cache_dir())];
    for (what, base) in bases {
        if let Some(base) = base.filter(|b| b.is_absolute()) {
            report(what, &migrate_dir(&base));
            if what == "config" {
                report("settings", &migrate_settings(&base));
            }
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
