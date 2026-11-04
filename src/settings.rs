// SPDX-License-Identifier: GPL-3.0-only
//! Settings backend for the Settings screen: typed access to every `config.toml` value by dotted path
//! (`audio.crossfade_ms`), validation, comment-preserving persistence and live apply.
//!
//! * `core` – pure logic (validation, key-sequence rules, text edits); unit tested.
//! * `meta` – the static description table the page is generated from.
//! * [`Settings`] – the QML singleton gluing both to `config::apply` and the `AppConfig` singleton.
mod logic;
mod meta;

use self::logic::Val;
use crate::{appconfig, config};
use core::pin::Pin;
use cxx_qt_lib::{QList, QString, QStringList, QVariant};
use serde_json::json;
use std::sync::Mutex;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        // `Settings` would clash with QtCore's QSettings element used by Main.qml.
        #[qml_element = "AppSettings"]
        #[qml_singleton]
        /// Absolute path of `config.toml`.
        #[qproperty(QString, config_path)]
        /// Bumps after every change; bind to it to re-read values.
        #[qproperty(i32, revision)]
        #[qproperty(QString, version)]
        type Settings = super::SettingsRust;

        /// Current value: bool, int, double, string, or a list of strings / numbers. Invalid for unknown paths.
        #[qinvokable]
        fn get(self: &Settings, path: &QString) -> QVariant;
        /// Validate, save and apply live. Returns "" on success, otherwise a message for the user.
        #[qinvokable]
        fn set(self: Pin<&mut Settings>, path: &QString, value: &QVariant) -> QString;
        #[qinvokable]
        fn reset(self: Pin<&mut Settings>, path: &QString) -> QString;
        /// Reset a whole TOML section (`audio`) or page group (`smart`).
        #[qinvokable]
        fn reset_section(self: Pin<&mut Settings>, name: &QString) -> QString;
        #[qinvokable]
        fn reset_all(self: Pin<&mut Settings>) -> QString;
        #[qinvokable]
        fn default_of(self: &Settings, path: &QString) -> QVariant;
        #[qinvokable]
        fn is_default(self: &Settings, path: &QString) -> bool;
        /// JSON: {path,label,description,kind,min,max,softMax,step,unit,choices,restart,group,hidden,advanced,slider,warn,default}.
        #[qinvokable]
        fn describe(self: &Settings, path: &QString) -> QString;
        /// Paths shown in a page group, in display order.
        #[qinvokable]
        fn paths(self: &Settings, group: &QString) -> QStringList;
        /// True when the setting needs a restart and differs from what the running app started with.
        #[qinvokable]
        fn needs_restart(self: &Settings, path: &QString) -> bool;
        /// Qt key event to key-sequence text (`Ctrl+Shift+P`); empty for a lone modifier.
        #[qinvokable]
        fn key_name(self: &Settings, key: i32, modifiers: i32) -> QString;
        /// `file://` URL of the folder holding config.toml, for `Qt.openUrlExternally`.
        #[qinvokable]
        fn config_folder_url(self: &Settings) -> QString;
        #[qinvokable]
        fn logs_folder_url(self: &Settings) -> QString;
        /// JSON list of what each feature sends where: [{id,title,to,what,active}].
        #[qinvokable]
        fn privacy_flows(self: &Settings, signed_in: bool) -> QString;
        /// Version / platform report with secrets removed.
        #[qinvokable]
        fn diagnostics(self: &Settings, signed_in: bool) -> QString;
        /// Strip cookies, tokens and signed URLs from text.
        #[qinvokable]
        fn redact(self: &Settings, text: &QString) -> QString;

        #[qsignal]
        fn changed(self: Pin<&mut Settings>, path: QString);
    }
}

pub struct SettingsRust {
    config_path: QString,
    revision: i32,
    version: QString,
}

impl Default for SettingsRust {
    fn default() -> Self {
        Self {
            config_path: QString::from(&config::path().to_string_lossy().into_owned()),
            revision: 0,
            version: QString::from(env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Serialises read-modify-write cycles of the file.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// The settings file as text. A missing file is a fresh start (the template); any other failure is an error, so a
/// momentary read problem can never turn into "save the template over the user's settings".
fn read_text() -> Result<String, String> {
    match std::fs::read_to_string(config::path()) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(config::Config::template()),
        Err(e) => Err(format!("Cannot read your settings file, so nothing was changed: {e}")),
    }
}

fn write_text(text: &str) -> Result<(), String> {
    let path = config::path();
    if let Some(dir) = path.parent() {
        crate::state::ensure_private_dir(dir).map_err(|e| format!("Cannot create the settings folder: {e}"))?;
    }
    crate::state::write_private(&path, text.as_bytes()).map_err(|e| format!("Cannot save settings: {e}"))
}

fn val_to_variant(v: &Val) -> QVariant {
    match v {
        Val::Bool(b) => QVariant::from(b),
        Val::Int(i) => match i32::try_from(*i) {
            Ok(small) => QVariant::from(&small),
            Err(_) => QVariant::from(&(*i as f64)),
        },
        Val::Float(f) => QVariant::from(f),
        Val::Text(s) => QVariant::from(&QString::from(s)),
        Val::Texts(l) => QVariant::from(&l.iter().map(String::as_str).map(QString::from).collect::<QStringList>()),
        Val::Floats(l) => QVariant::from(&l.iter().copied().collect::<QList<f64>>()),
    }
}

fn variant_to_val(m: &meta::Meta, v: &QVariant) -> Result<Val, String> {
    use meta::Kind::*;
    let bad = || format!("{} got a value of the wrong type.", m.label);
    match m.kind {
        Bool => v.value::<bool>().map(Val::Bool).ok_or_else(bad),
        Int | Float => v.value::<f64>().map(Val::Float).ok_or_else(bad),
        Text | Choice | Url | Dir | Shortcut => v.value::<QString>().map(|s| Val::Text(s.to_string())).ok_or_else(bad),
        TextList => {
            v.value::<QStringList>().map(|l| Val::Texts(QList::<QString>::from(&l).iter().map(|s| s.to_string()).collect())).ok_or_else(bad)
        }
        FloatList => {
            let list = v.value::<QList<QVariant>>().ok_or_else(bad)?;
            list.iter().map(|x| x.value::<f64>()).collect::<Option<Vec<_>>>().map(Val::Floats).ok_or_else(bad)
        }
    }
}

fn describe_json(m: &meta::Meta) -> serde_json::Value {
    let finite = |x: f64| if x.is_finite() { json!(x) } else { serde_json::Value::Null };
    json!({
        "path": m.path, "label": m.label, "description": m.description, "kind": m.kind.name(),
        "min": finite(m.min), "max": finite(m.max), "softMax": finite(m.soft_max), "step": m.step, "unit": m.unit,
        "choices": m.choices.iter().map(|(v, l)| json!({"value": v, "label": l})).collect::<Vec<_>>(),
        "restart": m.restart, "group": m.group, "hidden": m.hidden, "advanced": m.advanced, "slider": m.slider,
        "warn": m.warn,
        "default": logic::default_of(m.path).map_or(serde_json::Value::Null, |d| d.to_json()),
    })
}

fn folder_url(dir: &std::path::Path) -> String {
    reqwest::Url::from_directory_path(dir).map(|u| u.to_string()).unwrap_or_default()
}

/// Paths that a reset of `name` covers: a TOML section, else a page group.
fn paths_of(name: &str) -> Vec<&'static str> {
    let by_section: Vec<_> = meta::in_section(name).map(|m| m.path).collect();
    if by_section.is_empty() {
        meta::in_group(name).map(|m| m.path).collect()
    } else {
        by_section
    }
}

impl qobject::Settings {
    /// Run an edit on the file text; on success save it, apply live and tell QML.
    fn commit(mut self: Pin<&mut Self>, changed: &[String], edit: impl FnOnce(&str) -> Result<logic::Outcome, String>) -> QString {
        let result = {
            let _guard = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            read_text().and_then(|text| edit(&text)).and_then(|out| {
                write_text(&out.text)?;
                Ok(out.config)
            })
        };
        match result {
            Ok(cfg) => {
                config::apply(cfg.clone());
                appconfig::push(cfg);
                let rev = *self.revision() + 1;
                self.as_mut().set_revision(rev);
                for p in changed {
                    self.as_mut().changed(QString::from(p));
                }
                QString::default()
            }
            Err(e) => QString::from(&e),
        }
    }

    fn get(&self, path: &QString) -> QVariant {
        logic::lookup(&config::current(), &path.to_string()).map_or_else(QVariant::default, |v| val_to_variant(&v))
    }

    fn set(self: Pin<&mut Self>, path: &QString, value: &QVariant) -> QString {
        let path = path.to_string();
        let Some(m) = meta::find(&path) else { return QString::from(&format!("Unknown setting `{path}`.")) };
        let val = match variant_to_val(m, value) {
            Ok(v) => v,
            Err(e) => return QString::from(&e),
        };
        let p = path.clone();
        self.commit(&[path], move |text| logic::set(text, &p, val))
    }

    fn reset(self: Pin<&mut Self>, path: &QString) -> QString {
        let path = path.to_string();
        let p = path.clone();
        self.commit(&[path], move |text| logic::reset(text, &[&p]))
    }

    fn reset_section(self: Pin<&mut Self>, name: &QString) -> QString {
        let paths = paths_of(&name.to_string());
        let changed: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
        self.commit(&changed, move |text| logic::reset(text, &paths))
    }

    fn reset_all(self: Pin<&mut Self>) -> QString {
        let paths: Vec<&'static str> = meta::META.iter().map(|m| m.path).collect();
        let changed: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
        self.commit(&changed, move |text| logic::reset(text, &paths))
    }

    fn default_of(&self, path: &QString) -> QVariant {
        logic::default_of(&path.to_string()).map_or_else(QVariant::default, |v| val_to_variant(&v))
    }

    fn is_default(&self, path: &QString) -> bool {
        let path = path.to_string();
        logic::lookup(&config::current(), &path) == logic::default_of(&path)
    }

    fn describe(&self, path: &QString) -> QString {
        meta::find(&path.to_string()).map_or_else(|| QString::from("{}"), |m| QString::from(&describe_json(m).to_string()))
    }

    fn paths(&self, group: &QString) -> QStringList {
        let group = group.to_string();
        meta::in_group(&group).filter(|m| !m.hidden).map(|m| QString::from(m.path)).collect()
    }

    fn needs_restart(&self, path: &QString) -> bool {
        let path = path.to_string();
        meta::find(&path).is_some_and(|m| m.restart && logic::lookup(&config::current(), &path) != logic::lookup(config::get(), &path))
    }

    fn key_name(&self, key: i32, modifiers: i32) -> QString {
        QString::from(&logic::key_name(key, modifiers))
    }

    fn config_folder_url(&self) -> QString {
        QString::from(&config::path().parent().map(folder_url).unwrap_or_default())
    }

    fn logs_folder_url(&self) -> QString {
        QString::from(&folder_url(&crate::state::data_dir().join("logs")))
    }

    fn privacy_flows(&self, signed_in: bool) -> QString {
        QString::from(&serde_json::Value::Array(logic::privacy_flows(&config::current(), signed_in)).to_string())
    }

    fn diagnostics(&self, signed_in: bool) -> QString {
        let cfg = config::current();
        let changed = meta::META.iter().filter(|m| logic::lookup(&cfg, m.path) != logic::default_of(m.path)).count();
        let text = format!(
            "ytm {}\nOS: {} {}\nPlasma desktop: {}\nSecret stores available: {}\nAudio engine: {}\nMPRIS: {}\nQt Quick Controls style: {}\nSigned in: {}\nLog redaction: {}\nSettings changed from default: {}\nConfig: {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            if crate::platform::Desktop::detect().is_plasma() { "yes" } else { "no" },
            crate::platform::native_secret_stores().len(),
            cfg.audio.engine,
            if cfg.integration.mpris { "on" } else { "off" },
            std::env::var("QT_QUICK_CONTROLS_STYLE").unwrap_or_default(),
            if signed_in { "yes" } else { "no" },
            if cfg.security.redact_logs { "on" } else { "OFF" },
            changed,
            config::path().display(),
        );
        QString::from(&crate::logging::redact(&text))
    }

    fn redact(&self, text: &QString) -> QString {
        QString::from(&crate::logging::redact(&text.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_has_the_fields_the_page_needs() {
        let v = describe_json(meta::find("downloads.cap_gb").unwrap());
        assert_eq!(v["kind"], "float");
        assert_eq!(v["max"], serde_json::Value::Null); // no hard maximum
        assert_eq!(v["default"], 4.0);
        assert_eq!(v["unit"], "GB");
        let v = describe_json(meta::find("appearance.theme").unwrap());
        assert_eq!(v["choices"].as_array().unwrap().len(), 5);
        assert_eq!(describe_json(meta::find("audio.engine").unwrap())["restart"], true);
    }

    #[test]
    fn reset_scopes_resolve_to_sections_and_groups() {
        assert!(paths_of("audio").contains(&"audio.gapless"));
        assert!(paths_of("smart").contains(&"downloads.keep_liked"));
        assert!(paths_of("nonsense").is_empty());
        // every setting is shown in some group except the equalizer's, so the page can reach it
        for m in meta::META {
            assert!(!m.group.is_empty(), "{}", m.path);
        }
    }

    #[test]
    fn folder_urls_are_file_urls() {
        let u = folder_url(&std::env::temp_dir());
        assert!(u.starts_with("file:///"), "{u}");
        assert!(u.ends_with('/'));
    }
}
