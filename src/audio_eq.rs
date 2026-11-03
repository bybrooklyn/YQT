// SPDX-License-Identifier: GPL-3.0-only
//! `Equalizer`: the QML singleton behind the equalizer panel. It owns the settings (start values from `config.audio.*`,
//! the user's own state in `<data dir>/eq.json`), pushes them to the running engine and answers `response()` for the
//! curve in the UI.
use crate::audio::eq::{self, EqParams, BANDS, FREQS};
use crate::audio::presets::{self, EqFile, UserPreset, MAX_USER};
use crate::config;
use core::pin::Pin;
use cxx_qt::CxxQtType;
use cxx_qt_lib::{QList, QString, QStringList};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
        include!("cxx-qt-lib/qlist.h");
        type QList_f64 = cxx_qt_lib::QList<f64>;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(bool, enabled, READ, NOTIFY)]
        #[qproperty(f64, preamp_db, READ, NOTIFY)]
        #[qproperty(QList_f64, bands_db, READ, NOTIFY)]
        #[qproperty(QString, preset, READ, NOTIFY)]
        #[qproperty(bool, limiter, READ, NOTIFY)]
        type Equalizer = super::EqualizerRust;

        #[qinvokable]
        fn set_enabled(self: Pin<&mut Equalizer>, on: bool);
        #[qinvokable]
        fn set_preamp_db(self: Pin<&mut Equalizer>, db: f64);
        #[qinvokable]
        fn set_limiter(self: Pin<&mut Equalizer>, on: bool);
        /// Set one band (0..9) in dB; the preset becomes "Custom" unless it still matches one.
        #[qinvokable]
        fn set_band_db(self: Pin<&mut Equalizer>, index: i32, db: f64);
        #[qinvokable]
        fn set_bands_db(self: Pin<&mut Equalizer>, bands: &QList_f64);
        /// Built-in or user preset by name.
        #[qinvokable]
        fn apply_preset(self: Pin<&mut Equalizer>, name: &QString);
        /// Store the current gains as a user preset. Returns false for an invalid or built-in name.
        #[qinvokable]
        fn save_preset(self: Pin<&mut Equalizer>, name: &QString) -> bool;
        #[qinvokable]
        fn delete_preset(self: Pin<&mut Equalizer>, name: &QString) -> bool;
        /// Built-in presets first, then the user's.
        #[qinvokable]
        fn preset_names(self: &Equalizer) -> QStringList;
        #[qinvokable]
        fn is_user_preset(self: &Equalizer, name: &QString) -> bool;
        /// Centre frequencies of the ten bands (Hz).
        #[qinvokable]
        fn band_frequencies(self: &Equalizer) -> QList_f64;
        /// dB response of the current chain at each frequency (flat while the EQ is off): the curve for the UI.
        #[qinvokable]
        fn response(self: &Equalizer, freqs: &QList_f64) -> QList_f64;
        /// Write the current state to eq.json now (sliders call this on release).
        #[qinvokable]
        fn persist(self: &Equalizer);
    }
}

pub struct EqualizerRust {
    enabled: bool,
    preamp_db: f64,
    bands_db: QList<f64>,
    preset: QString,
    limiter: bool,
    user: BTreeMap<String, UserPreset>,
    path: std::path::PathBuf,
}

impl EqualizerRust {
    fn params(&self) -> EqParams {
        let bands: Vec<f64> = Vec::from(&self.bands_db);
        EqParams { enabled: self.enabled, preamp_db: self.preamp_db, bands_db: EqParams::bands_from_slice(&bands), limiter: self.limiter }
    }
}

/// Start values: `config.audio.*`, overridden by whatever the user saved in eq.json.
pub fn initial(cfg: &config::Audio, file: &EqFile) -> (EqParams, String) {
    let mut p = EqParams {
        enabled: cfg.eq_enabled,
        preamp_db: cfg.eq_preamp_db,
        bands_db: EqParams::bands_from_slice(&cfg.eq_bands_db),
        limiter: true,
    };
    let mut preset = presets::builtin(&cfg.eq_preset).map_or_else(|| "Custom".to_owned(), |b| b.name.to_owned());
    if let Some(v) = file.enabled {
        p.enabled = v;
    }
    if let Some(v) = file.preamp_db {
        p.preamp_db = v;
    }
    if let Some(b) = &file.bands_db {
        p.bands_db = EqParams::bands_from_slice(b);
    }
    if let Some(v) = file.limiter {
        p.limiter = v;
    }
    if let Some(n) = &file.preset {
        preset = n.clone();
    }
    let p = p.sanitized();
    // a stored name is only trusted while it still describes the gains
    let actual = presets::identify(&p, &file.user_presets);
    if preset != actual && !(preset.eq_ignore_ascii_case("custom") && actual == "Custom") {
        preset = actual;
    }
    (p, preset)
}

impl Default for EqualizerRust {
    fn default() -> Self {
        let path = presets::file_path();
        let file = presets::load(&path);
        let (p, preset) = initial(&config::get().audio, &file);
        eq::shared().set(p.clone());
        Self {
            enabled: p.enabled,
            preamp_db: p.preamp_db,
            bands_db: QList::from(p.bands_db.as_slice()),
            preset: QString::from(&preset),
            limiter: p.limiter,
            user: file.user_presets,
            path,
        }
    }
}

static SAVE_GEN: AtomicU64 = AtomicU64::new(0);

impl qobject::Equalizer {
    /// Push the current state to the engine and notify QML of what changed.
    fn commit(mut self: Pin<&mut Self>, preset: Option<String>) {
        let p = self.rust().params();
        eq::shared().set(p.clone());
        let name = preset.unwrap_or_else(|| presets::identify(&p, &self.rust().user));
        if self.rust().preset.to_string() != name {
            self.as_mut().rust_mut().get_mut().preset = QString::from(&name);
            self.as_mut().preset_changed();
        }
        self.as_mut().enabled_changed();
        self.as_mut().preamp_db_changed();
        self.as_mut().bands_db_changed();
        self.as_mut().limiter_changed();
        self.schedule_save();
    }

    /// Save shortly after the last change (a slider drag is many changes).
    fn schedule_save(&self) {
        let generation = SAVE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
        let snapshot = self.file_snapshot();
        let path = self.rust().path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            if SAVE_GEN.load(Ordering::SeqCst) == generation {
                if let Err(e) = presets::save(&path, &snapshot) {
                    eprintln!("could not save the equalizer settings: {e}");
                }
            }
        });
    }

    fn file_snapshot(&self) -> EqFile {
        let me = self.rust();
        let p = me.params();
        EqFile {
            enabled: Some(p.enabled),
            preamp_db: Some(p.preamp_db),
            bands_db: Some(p.bands_db.to_vec()),
            limiter: Some(p.limiter),
            preset: Some(me.preset.to_string()),
            user_presets: me.user.clone(),
        }
    }

    fn set_enabled(mut self: Pin<&mut Self>, on: bool) {
        self.as_mut().rust_mut().get_mut().enabled = on;
        self.commit(None);
    }

    fn set_limiter(mut self: Pin<&mut Self>, on: bool) {
        self.as_mut().rust_mut().get_mut().limiter = on;
        self.commit(None);
    }

    fn set_preamp_db(mut self: Pin<&mut Self>, db: f64) {
        self.as_mut().rust_mut().get_mut().preamp_db = presets::clamp_db(if db.is_finite() { db } else { 0.0 });
        self.commit(None);
    }

    fn set_band_db(mut self: Pin<&mut Self>, index: i32, db: f64) {
        let Ok(i) = usize::try_from(index) else { return };
        if i >= BANDS {
            return;
        }
        let mut bands = self.rust().params().bands_db;
        bands[i] = presets::clamp_db(if db.is_finite() { db } else { 0.0 });
        self.as_mut().rust_mut().get_mut().bands_db = QList::from(bands.as_slice());
        self.commit(None);
    }

    fn set_bands_db(mut self: Pin<&mut Self>, bands: &QList<f64>) {
        let v: Vec<f64> = Vec::from(bands);
        let b = EqParams::bands_from_slice(&v);
        self.as_mut().rust_mut().get_mut().bands_db = QList::from(b.as_slice());
        self.commit(None);
    }

    fn apply_preset(mut self: Pin<&mut Self>, name: &QString) {
        let base = self.rust().params();
        let Some((display, p)) = presets::resolve(&name.to_string(), &self.rust().user, &base) else { return };
        {
            let me = self.as_mut().rust_mut().get_mut();
            me.preamp_db = p.preamp_db;
            me.bands_db = QList::from(p.bands_db.as_slice());
        }
        self.commit(Some(display));
    }

    fn save_preset(mut self: Pin<&mut Self>, name: &QString) -> bool {
        let Some(n) = presets::clean_name(&name.to_string()) else { return false };
        let exists = self.rust().user.keys().any(|k| k.eq_ignore_ascii_case(&n));
        if !exists && self.rust().user.len() >= MAX_USER {
            return false;
        }
        let p = self.rust().params();
        let key = self.rust().user.keys().find(|k| k.eq_ignore_ascii_case(&n)).cloned().unwrap_or(n);
        self.as_mut().rust_mut().get_mut().user.insert(key.clone(), UserPreset { preamp_db: p.preamp_db, bands_db: p.bands_db.to_vec() });
        self.commit(Some(key));
        true
    }

    fn delete_preset(mut self: Pin<&mut Self>, name: &QString) -> bool {
        let n = name.to_string();
        let Some(key) = self.rust().user.keys().find(|k| k.eq_ignore_ascii_case(n.trim())).cloned() else { return false };
        self.as_mut().rust_mut().get_mut().user.remove(&key);
        self.commit(None);
        true
    }

    fn preset_names(&self) -> QStringList {
        presets::BUILTIN
            .iter()
            .map(|p| p.name.to_owned())
            .chain(self.rust().user.keys().cloned())
            .map(|n| QString::from(n.as_str()))
            .collect()
    }

    fn is_user_preset(&self, name: &QString) -> bool {
        let n = name.to_string();
        self.rust().user.keys().any(|k| k.eq_ignore_ascii_case(n.trim()))
    }

    fn band_frequencies(&self) -> QList<f64> {
        QList::from(FREQS.as_slice())
    }

    fn response(&self, freqs: &QList<f64>) -> QList<f64> {
        let f: Vec<f64> = Vec::from(freqs);
        QList::from(eq::response_db(&self.rust().params(), &f, 48_000.0).as_slice())
    }

    fn persist(&self) {
        SAVE_GEN.fetch_add(1, Ordering::SeqCst); // cancel the pending debounced save
        if let Err(e) = presets::save(&self.rust().path, &self.file_snapshot()) {
            eprintln!("could not save the equalizer settings: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_from_config_then_the_users_file_wins() {
        let cfg = config::Audio {
            eq_enabled: true,
            eq_preamp_db: -2.0,
            eq_bands_db: vec![1.0; 10],
            eq_preset: "custom".into(),
            ..Default::default()
        };
        let (p, name) = initial(&cfg, &EqFile::default());
        assert!(p.enabled && p.preamp_db == -2.0 && p.bands_db == [1.0; 10]);
        assert_eq!(name, "Custom");
        let file = EqFile {
            enabled: Some(false),
            bands_db: Some(vec![0.0; 10]),
            preamp_db: Some(0.0),
            preset: Some("Flat".into()),
            ..Default::default()
        };
        let (p, name) = initial(&cfg, &file);
        assert!(!p.enabled && p.bands_db == [0.0; 10]);
        assert_eq!(name, "Flat");
    }

    #[test]
    fn config_defaults_are_flat_and_off() {
        let (p, name) = initial(&config::Audio::default(), &EqFile::default());
        assert!(!p.enabled && p.is_identity());
        assert_eq!(name, "Flat");
    }

    #[test]
    fn a_stale_preset_name_is_corrected_and_garbage_is_clamped() {
        let file = EqFile { preset: Some("Rock".into()), bands_db: Some(vec![99.0, -99.0, f64::NAN]), ..Default::default() };
        let (p, name) = initial(&config::Audio::default(), &file);
        assert_eq!(p.bands_db[..4], [12.0, -12.0, 0.0, 0.0]);
        assert_eq!(name, "Custom", "the stored name no longer describes the gains");
    }
}
