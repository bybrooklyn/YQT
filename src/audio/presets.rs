// SPDX-License-Identifier: GPL-3.0-only
//! Built-in EQ presets, user presets and the `eq.json` file they (and the current EQ state) live in.
use super::eq::{EqParams, BANDS, MAX_DB};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A built-in preset. The preamp pulls the loudest point of the curve back to about 0 dB so a preset never
/// makes the music jump louder (and never needs the limiter for ordinary material).
pub struct Preset {
    pub key: &'static str,
    pub name: &'static str,
    pub preamp_db: f64,
    /// 31 Hz, 62, 125, 250, 500, 1 k, 2 k, 4 k, 8 k, 16 k.
    pub bands_db: [f64; BANDS],
}

pub const BUILTIN: [Preset; 10] = [
    Preset { key: "flat", name: "Flat", preamp_db: 0.0, bands_db: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0] },
    Preset { key: "bass_boost", name: "Bass boost", preamp_db: -7.0, bands_db: [6.0, 5.0, 4.0, 2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0] },
    Preset { key: "treble_boost", name: "Treble boost", preamp_db: -6.0, bands_db: [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 4.0, 5.0, 6.0] },
    Preset { key: "vocal", name: "Vocal", preamp_db: -5.0, bands_db: [-2.0, -3.0, -3.0, 1.0, 3.0, 4.0, 3.0, 2.0, 0.0, -1.0] },
    Preset { key: "rock", name: "Rock", preamp_db: -6.0, bands_db: [5.0, 4.0, 3.0, 1.0, -1.0, -1.0, 1.0, 3.0, 4.0, 5.0] },
    Preset { key: "pop", name: "Pop", preamp_db: -5.0, bands_db: [-1.0, 1.0, 3.0, 4.0, 3.0, 0.0, -1.0, -1.0, 1.0, 2.0] },
    Preset { key: "electronic", name: "Electronic", preamp_db: -6.0, bands_db: [5.0, 4.0, 1.0, 0.0, -2.0, 2.0, 1.0, 1.0, 4.0, 5.0] },
    Preset { key: "acoustic", name: "Acoustic", preamp_db: -5.0, bands_db: [4.0, 4.0, 3.0, 1.0, 2.0, 2.0, 3.0, 3.0, 3.0, 2.0] },
    Preset { key: "classical", name: "Classical", preamp_db: 0.0, bands_db: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -3.0, -3.0, -3.0, -5.0] },
    Preset { key: "loudness", name: "Loudness", preamp_db: -7.0, bands_db: [6.0, 4.0, 1.0, 0.0, -1.0, -1.0, 0.0, 2.0, 4.0, 5.0] },
];

pub fn builtin(name_or_key: &str) -> Option<&'static Preset> {
    let q = name_or_key.trim();
    BUILTIN.iter().find(|p| p.name.eq_ignore_ascii_case(q) || p.key.eq_ignore_ascii_case(q))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserPreset {
    pub preamp_db: f64,
    pub bands_db: Vec<f64>,
}

/// Everything persisted in `<data dir>/eq.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqFile {
    pub enabled: Option<bool>,
    pub preamp_db: Option<f64>,
    pub bands_db: Option<Vec<f64>>,
    pub limiter: Option<bool>,
    /// Name of the preset the current bands came from ("" / "Custom" = hand-tuned).
    pub preset: Option<String>,
    pub user_presets: BTreeMap<String, UserPreset>,
}

pub fn file_path() -> PathBuf {
    crate::state::data_dir().join("eq.json")
}

pub fn load(path: &Path) -> EqFile {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(path: &Path, f: &EqFile) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        crate::state::ensure_private_dir(dir)?;
    }
    let json = serde_json::to_vec_pretty(f).map_err(std::io::Error::other)?;
    crate::state::write_private(path, &json)
}

/// A user preset name must be 1..=40 printable characters and must not shadow a built-in one.
pub fn clean_name(raw: &str) -> Option<String> {
    let n: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let ok = !n.is_empty()
        && n.chars().count() <= 40
        && !n.chars().any(char::is_control)
        && builtin(&n).is_none()
        && !n.eq_ignore_ascii_case("custom");
    ok.then_some(n)
}

/// Params for a named preset (built-in or user), keeping `enabled`/`limiter` from `base`.
pub fn resolve(name: &str, user: &BTreeMap<String, UserPreset>, base: &EqParams) -> Option<(String, EqParams)> {
    if let Some(p) = builtin(name) {
        return Some((p.name.to_owned(), EqParams { preamp_db: p.preamp_db, bands_db: p.bands_db, ..base.clone() }));
    }
    let key = user.keys().find(|k| k.eq_ignore_ascii_case(name.trim()))?;
    let u = &user[key];
    Some((key.clone(), EqParams { preamp_db: u.preamp_db, bands_db: EqParams::bands_from_slice(&u.bands_db), ..base.clone() }.sanitized()))
}

/// The name of the preset that exactly matches `p`'s gains, else "Custom".
pub fn identify(p: &EqParams, user: &BTreeMap<String, UserPreset>) -> String {
    let same = |pre: f64, b: &[f64]| (pre - p.preamp_db).abs() < 1e-6 && b.iter().zip(&p.bands_db).all(|(a, c)| (a - c).abs() < 1e-6);
    if let Some(b) = BUILTIN.iter().find(|b| same(b.preamp_db, &b.bands_db)) {
        return b.name.to_owned();
    }
    user.iter()
        .find(|(_, u)| u.bands_db.len() == BANDS && same(u.preamp_db, &u.bands_db))
        .map_or_else(|| "Custom".to_owned(), |(n, _)| n.clone())
}

/// Max number of user presets (a file full of them is not a feature).
pub const MAX_USER: usize = 50;

pub fn clamp_db(v: f64) -> f64 {
    v.clamp(-MAX_DB, MAX_DB)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::eq::response_db;
    use std::collections::HashSet;

    #[test]
    fn table_is_sane() {
        let mut keys = HashSet::new();
        let mut names = HashSet::new();
        for p in &BUILTIN {
            assert!(keys.insert(p.key) && names.insert(p.name), "duplicate {}", p.name);
            assert!(p.bands_db.iter().all(|b| b.abs() <= MAX_DB), "{} out of range", p.name);
            assert!(p.preamp_db.abs() <= MAX_DB, "{} preamp", p.name);
            assert_eq!(p.key, p.name.to_lowercase().replace(' ', "_"));
        }
        for must in ["Flat", "Bass boost", "Treble boost", "Vocal", "Rock", "Pop", "Electronic", "Acoustic", "Classical", "Loudness"] {
            assert!(builtin(must).is_some(), "{must} missing");
        }
        let flat = builtin("flat").unwrap();
        assert!(flat.bands_db.iter().all(|&b| b == 0.0) && flat.preamp_db == 0.0);
    }

    #[test]
    fn presets_do_not_make_the_music_louder() {
        let freqs: Vec<f64> = (0..200).map(|i| 20.0 * (1000.0f64).powf(i as f64 / 199.0)).collect(); // 20 Hz .. 20 kHz
        let mut bad = Vec::new();
        for p in &BUILTIN {
            let params = EqParams { enabled: true, preamp_db: p.preamp_db, bands_db: p.bands_db, limiter: true };
            let peak = response_db(&params, &freqs, 48_000.0).into_iter().fold(f64::MIN, f64::max);
            println!("{:<13} peak {peak:+.2} dB", p.name);
            if peak > 0.75 {
                bad.push(format!("{} peaks at {peak:.2} dB", p.name));
            }
        }
        assert!(bad.is_empty(), "{bad:?}");
    }

    #[test]
    fn lookup_is_forgiving_and_names_are_validated() {
        assert_eq!(builtin("  bass BOOST ").unwrap().key, "bass_boost");
        assert_eq!(builtin("treble_boost").unwrap().name, "Treble boost");
        assert!(builtin("nope").is_none());
        assert_eq!(clean_name("  my   mix ").as_deref(), Some("my mix"));
        for bad in ["", "   ", "Flat", "custom", "bad\u{7}name", &"x".repeat(41)] {
            assert!(clean_name(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn resolve_and_identify_round_trip() {
        let mut user = BTreeMap::new();
        user.insert("Night".to_owned(), UserPreset { preamp_db: -2.0, bands_db: vec![3.0; 10] });
        let base = EqParams { enabled: true, ..Default::default() };
        let (name, p) = resolve("rock", &user, &base).unwrap();
        assert_eq!(name, "Rock");
        assert!(p.enabled && p.bands_db[0] == 5.0);
        assert_eq!(identify(&p, &user), "Rock");
        let (n2, p2) = resolve("night", &user, &base).unwrap();
        assert_eq!(n2, "Night");
        assert_eq!(identify(&p2, &user), "Night");
        let mut custom = p2.clone();
        custom.bands_db[3] += 1.0;
        assert_eq!(identify(&custom, &user), "Custom");
        assert!(resolve("ghost", &user, &base).is_none());
    }

    #[test]
    fn file_round_trips_and_survives_garbage() {
        let dir = std::env::temp_dir().join(format!("ytm-eq-test-{}", std::process::id()));
        let path = dir.join("eq.json");
        let mut f = EqFile {
            enabled: Some(true),
            preamp_db: Some(-3.0),
            bands_db: Some(vec![1.0; 10]),
            preset: Some("Custom".into()),
            ..Default::default()
        };
        f.user_presets.insert("A".into(), UserPreset { preamp_db: 0.0, bands_db: vec![2.0; 10] });
        save(&path, &f).unwrap();
        assert_eq!(load(&path), f);
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load(&path), EqFile::default());
        assert_eq!(load(&dir.join("missing.json")), EqFile::default());
        std::fs::remove_dir_all(&dir).ok();
    }
}
