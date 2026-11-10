// SPDX-License-Identifier: GPL-3.0-only
//! Pure settings logic (no Qt, no globals): typed access by dotted path, validation against the
//! metadata table, and comment-preserving edits of the config file text.
use super::meta::{self, Kind, Meta};
use crate::config::Config;
use std::path::PathBuf;
use toml_edit::{DocumentMut, Item, Table, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Texts(Vec<String>),
    Floats(Vec<f64>),
}

impl Val {
    /// Plain JSON, for `describe`-style payloads.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Val::Bool(b) => (*b).into(),
            Val::Int(i) => (*i).into(),
            Val::Float(f) => serde_json::Number::from_f64(*f).map_or(serde_json::Value::Null, Into::into),
            Val::Text(s) => s.clone().into(),
            Val::Texts(v) => v.clone().into(),
            Val::Floats(v) => v.iter().filter_map(|f| serde_json::Number::from_f64(*f)).collect::<Vec<_>>().into(),
        }
    }
}

fn from_toml(v: &toml::Value) -> Option<Val> {
    Some(match v {
        toml::Value::Boolean(b) => Val::Bool(*b),
        toml::Value::Integer(i) => Val::Int(*i),
        toml::Value::Float(f) => Val::Float(*f),
        toml::Value::String(s) => Val::Text(s.clone()),
        toml::Value::Array(a) => {
            if a.iter().all(|x| x.is_str()) {
                Val::Texts(a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
            } else {
                Val::Floats(a.iter().filter_map(|x| x.as_float().or_else(|| x.as_integer().map(|i| i as f64))).collect())
            }
        }
        _ => return None,
    })
}

/// The effective value of `path` in `cfg`.
pub fn lookup(cfg: &Config, path: &str) -> Option<Val> {
    let table = toml::Table::try_from(cfg).ok()?;
    let (section, key) = path.split_once('.')?;
    from_toml(table.get(section)?.as_table()?.get(key)?)
}

pub fn default_of(path: &str) -> Option<Val> {
    lookup(&Config::default(), path)
}

// ---- validation -----------------------------------------------------------------------------

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

fn as_f64(v: &Val) -> Option<f64> {
    match v {
        Val::Int(i) => Some(*i as f64),
        Val::Float(f) => Some(*f),
        _ => None,
    }
}

/// Check and normalise `val` for `m`: wrong types and bad values are errors with a human message;
/// numbers outside the documented range are clamped.
pub fn coerce(m: &Meta, val: Val, cfg: &Config) -> Result<Val, String> {
    let wrong = |what: &str| Err(format!("{} needs {what}.", m.label));
    match m.kind {
        Kind::Bool => match val {
            Val::Bool(_) => Ok(val),
            _ => wrong("on or off"),
        },
        Kind::Int | Kind::Float => {
            let Some(x) = as_f64(&val) else { return wrong("a number") };
            if !x.is_finite() {
                return wrong("a finite number");
            }
            let x = x.clamp(m.min, m.max);
            Ok(if m.kind == Kind::Int { Val::Int(x.round() as i64) } else { Val::Float(round6(x)) })
        }
        Kind::Text => match val {
            Val::Text(s) => {
                let s = s.trim().to_owned();
                if s.chars().any(char::is_control) {
                    return Err(format!("{} must not contain control characters.", m.label));
                }
                if s.len() > 2048 {
                    return Err(format!("{} is too long.", m.label));
                }
                Ok(Val::Text(s))
            }
            _ => wrong("text"),
        },
        Kind::Choice => match val {
            Val::Text(s) if m.choices.iter().any(|(v, _)| *v == s) => Ok(Val::Text(s)),
            Val::Text(s) => Err(format!("`{s}` is not one of: {}.", m.choices.iter().map(|(v, _)| *v).collect::<Vec<_>>().join(", "))),
            _ => wrong("one of the listed choices"),
        },
        Kind::Url => match val {
            Val::Text(s) => {
                crate::security::validate_endpoint(&s, cfg.security.allow_insecure_urls)?;
                Ok(Val::Text(s.trim().trim_end_matches('/').to_owned()))
            }
            _ => wrong("an address"),
        },
        Kind::Dir => match val {
            Val::Text(s) => check_dir(&s).map(Val::Text),
            _ => wrong("a folder"),
        },
        Kind::Shortcut => match val {
            Val::Text(s) => {
                let norm = normalize_shortcut(&s)?;
                if !norm.is_empty() {
                    for other in meta::in_group("keys").filter(|o| o.path != m.path) {
                        if let Some(Val::Text(t)) = lookup(cfg, other.path) {
                            if t.eq_ignore_ascii_case(&norm) {
                                return Err(format!("{norm} is already used by \"{}\".", other.label));
                            }
                        }
                    }
                }
                Ok(Val::Text(norm))
            }
            _ => wrong("a key combination"),
        },
        Kind::TextList => match val {
            Val::Texts(list) => {
                let mut out: Vec<String> = Vec::new();
                for s in list {
                    if !m.choices.iter().any(|(v, _)| *v == s) {
                        return Err(format!("`{s}` is not one of: {}.", m.choices.iter().map(|(v, _)| *v).collect::<Vec<_>>().join(", ")));
                    }
                    if !out.contains(&s) {
                        out.push(s);
                    }
                }
                Ok(Val::Texts(out))
            }
            _ => wrong("a list"),
        },
        Kind::FloatList => match val {
            Val::Floats(list) => {
                let want = match default_of(m.path) {
                    Some(Val::Floats(d)) => d.len(),
                    _ => list.len(),
                };
                if list.len() != want {
                    return Err(format!("{} needs exactly {want} numbers.", m.label));
                }
                if list.iter().any(|x| !x.is_finite()) {
                    return wrong("finite numbers");
                }
                Ok(Val::Floats(list.into_iter().map(|x| round6(x.clamp(m.min, m.max))).collect()))
            }
            _ => wrong("a list of numbers"),
        },
    }
}

/// Resolve `~/…`, require an absolute path, create the folder if missing and prove it is writable.
/// Empty means "use the default location".
pub fn check_dir(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Ok(String::new());
    }
    if s.chars().any(char::is_control) {
        return Err("The folder name contains control characters.".into());
    }
    let path: PathBuf = match s.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().ok_or("Cannot find your home folder.")?.join(rest),
        None => PathBuf::from(s),
    };
    if !path.is_absolute() {
        return Err("Use a full folder path (starting with / or a drive letter).".into());
    }
    if path.exists() && !path.is_dir() {
        return Err("That path is a file, not a folder.".into());
    }
    std::fs::create_dir_all(&path).map_err(|e| format!("Cannot create the folder: {e}"))?;
    let probe = path.join(format!(".yqt-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"").map_err(|e| format!("The folder is not writable: {e}"))?;
    let _ = std::fs::remove_file(&probe);
    Ok(path.to_string_lossy().into_owned())
}

// ---- shortcuts ------------------------------------------------------------------------------

const NAMED_KEYS: &[&str] = &[
    "Space",
    "Return",
    "Enter",
    "Escape",
    "Tab",
    "Backspace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "PgUp",
    "PgDown",
    "Left",
    "Right",
    "Up",
    "Down",
    "Print",
    "Pause",
    "Menu",
    "MediaPlay",
    "MediaPause",
    "MediaTogglePlayPause",
    "MediaNext",
    "MediaPrevious",
    "MediaStop",
    "VolumeUp",
    "VolumeDown",
    "VolumeMute",
];

fn canonical_key(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let alias = match lower.as_str() {
        "esc" => "Escape",
        "pageup" => "PgUp",
        "pagedown" => "PgDown",
        "del" => "Delete",
        "ins" => "Insert",
        "backtab" => "Tab",
        _ => "",
    };
    if !alias.is_empty() {
        return Some(alias.to_owned());
    }
    if let Some(k) = NAMED_KEYS.iter().find(|k| k.eq_ignore_ascii_case(name)) {
        return Some((*k).to_owned());
    }
    if let Some(num) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        if (1..=24).contains(&num) {
            return Some(format!("F{num}"));
        }
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase().to_string()),
        (Some(c), None) if c.is_ascii_graphic() && c != '+' => Some(c.to_string()),
        (Some('+'), None) => Some("+".into()),
        _ => None,
    }
}

/// Validate a Qt key sequence (one chord) and return it in canonical form, e.g. `ctrl+shift+p` -> `Ctrl+Shift+P`.
/// The empty string is allowed and means "unbound".
pub fn normalize_shortcut(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Ok(String::new());
    }
    let (mods_part, key_part) = if s == "+" {
        ("", "+")
    } else if let Some(rest) = s.strip_suffix("++") {
        (rest, "+")
    } else if let Some((m, k)) = s.rsplit_once('+') {
        (m, k)
    } else {
        ("", s)
    };
    let key = canonical_key(key_part.trim())
        .ok_or_else(|| format!("`{}` is not a key Qt knows. Try something like Ctrl+Shift+P.", key_part.trim()))?;
    let (mut ctrl, mut shift, mut alt, mut meta_k) = (false, false, false, false);
    for part in mods_part.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let slot = match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => &mut ctrl,
            "shift" => &mut shift,
            "alt" => &mut alt,
            "meta" | "cmd" | "super" | "win" => &mut meta_k,
            other => return Err(format!("`{other}` is not a modifier. Use Ctrl, Shift, Alt or Meta.")),
        };
        if *slot {
            return Err(format!("`{part}` appears twice."));
        }
        *slot = true;
    }
    let mut out = String::new();
    for (on, name) in [(ctrl, "Ctrl"), (shift, "Shift"), (alt, "Alt"), (meta_k, "Meta")] {
        if on {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(&key);
    Ok(out)
}

/// Turn a Qt key event (`Qt.Key_*` value and `Qt.*Modifier` flags) into a key-sequence string.
/// Empty for a lone modifier press or an unknown key.
pub fn key_name(key: i32, modifiers: i32) -> String {
    let name: String = match key {
        0x20 => "Space".into(),
        0x30..=0x39 | 0x41..=0x5a => char::from_u32(key as u32).unwrap_or(' ').to_string(),
        0x01000000 => "Escape".into(),
        0x01000001 | 0x01000002 => "Tab".into(),
        0x01000003 => "Backspace".into(),
        0x01000004 => "Return".into(),
        0x01000005 => "Enter".into(),
        0x01000006 => "Insert".into(),
        0x01000007 => "Delete".into(),
        0x01000008 => "Pause".into(),
        0x01000009 => "Print".into(),
        0x01000010 => "Home".into(),
        0x01000011 => "End".into(),
        0x01000012 => "Left".into(),
        0x01000013 => "Up".into(),
        0x01000014 => "Right".into(),
        0x01000015 => "Down".into(),
        0x01000016 => "PgUp".into(),
        0x01000017 => "PgDown".into(),
        0x01000030..=0x01000047 => format!("F{}", key - 0x01000030 + 1),
        0x01000055 => "Menu".into(),
        0x01000080..=0x01000083 | 0x01000020..=0x01000023 | 0x01001103 => return String::new(), // lone modifiers
        0x21..=0x7e => char::from_u32(key as u32).unwrap_or(' ').to_string(),
        _ => return String::new(),
    };
    let mut out = String::new();
    for (flag, label) in [(0x04000000, "Ctrl"), (0x02000000, "Shift"), (0x08000000, "Alt"), (0x10000000, "Meta")] {
        if modifiers & flag != 0 {
            out.push_str(label);
            out.push('+');
        }
    }
    out + &name
}

// ---- editing the file text ------------------------------------------------------------------

#[derive(Debug)]
pub struct Outcome {
    pub text: String,
    pub config: Config,
}

fn open(text: &str) -> Result<DocumentMut, String> {
    text.parse::<DocumentMut>().map_err(|e| format!("config.toml has a syntax error and was left untouched: {}", e.message()))
}

fn to_value(v: &Val) -> Value {
    match v {
        Val::Bool(b) => Value::from(*b),
        Val::Int(i) => Value::from(*i),
        Val::Float(f) => Value::from(*f),
        Val::Text(s) => Value::from(s.as_str()),
        Val::Texts(l) => l.iter().map(String::as_str).collect(),
        Val::Floats(l) => l.iter().copied().collect(),
    }
}

fn section_mut<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut dyn toml_edit::TableLike, String> {
    let fresh = doc.get(name).is_none();
    if fresh {
        let mut t = Table::new();
        if !doc.as_table().is_empty() {
            t.decor_mut().set_prefix("\n");
        }
        doc.insert(name, Item::Table(t));
    }
    doc.get_mut(name).and_then(Item::as_table_like_mut).ok_or_else(|| format!("[{name}] in config.toml is not a table."))
}

fn put(doc: &mut DocumentMut, m: &Meta, val: &Val) -> Result<(), String> {
    let table = section_mut(doc, m.section())?;
    let mut new = to_value(val);
    match table.get_mut(m.key()) {
        Some(slot) => {
            if let Some(old) = slot.as_value() {
                *new.decor_mut() = old.decor().clone(); // keeps a trailing `# comment`
            }
            *slot = Item::Value(new);
        }
        None => {
            table.insert(m.key(), Item::Value(new));
        }
    }
    Ok(())
}

/// Warnings that exist after an edit but did not before it.
fn new_warnings(before: &[String], after: Vec<String>) -> Vec<String> {
    after.into_iter().filter(|w| !before.contains(w)).collect()
}

fn finish(text: String, before_warnings: &[String]) -> Result<Outcome, String> {
    let (config, after) = Config::parse(&text);
    if let Some(w) = new_warnings(before_warnings, after).into_iter().next() {
        return Err(format!("That value was rejected: {w}"));
    }
    Ok(Outcome { text, config })
}

/// Set `path` to `val` in the config text. Validates, edits in place (comments and unknown keys survive) and
/// returns the new text with its effective configuration.
pub fn set(text: &str, path: &str, val: Val) -> Result<Outcome, String> {
    let m = meta::find(path).ok_or_else(|| format!("Unknown setting `{path}`."))?;
    let doc_check = open(text)?;
    drop(doc_check);
    let (cfg, before) = Config::parse(text);
    let val = coerce(m, val, &cfg)?;
    let mut doc = open(text)?;
    put(&mut doc, m, &val)?;
    let out = finish(doc.to_string(), &before)?;
    if lookup(&out.config, path).as_ref() != Some(&val) {
        return Err("That value was not accepted by the configuration.".into());
    }
    Ok(out)
}

/// Put the listed paths back to their defaults. Keys that are not in the file already are at their default.
pub fn reset(text: &str, paths: &[&str]) -> Result<Outcome, String> {
    let mut doc = open(text)?;
    let (_, before) = Config::parse(text);
    for path in paths {
        let m = meta::find(path).ok_or_else(|| format!("Unknown setting `{path}`."))?;
        let present = doc.get(m.section()).and_then(Item::as_table_like).is_some_and(|t| t.contains_key(m.key()));
        if present {
            let d = default_of(path).ok_or("No default for that setting.")?;
            put(&mut doc, m, &d)?;
        }
    }
    finish(doc.to_string(), &before)
}

// ---- privacy ----------------------------------------------------------------------------------

/// What leaves the computer, to whom, and whether it is happening right now with this configuration.
pub fn privacy_flows(cfg: &Config, signed_in: bool) -> Vec<serde_json::Value> {
    let host = |u: &str| reqwest::Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| u.to_owned());
    let yt = host(&cfg.youtube.music_origin);
    let lyr = &cfg.lyrics;
    let has = |p: &str| lyr.providers.iter().any(|x| x == p);
    let mut flows = vec![
        serde_json::json!({
            "id": "browse", "title": "Browsing and search", "to": yt,
            "what": "Your search words, the pages you open and, when signed in, your session cookie so YouTube can show your library.",
            "active": true,
        }),
        serde_json::json!({
            "id": "media", "title": "Audio and covers", "to": "googlevideo.com, ytimg.com, googleusercontent.com (Google)",
            "what": "Your IP address and which song or picture you ask for. Audio is fetched through a local proxy that only this app can use.",
            "active": true,
        }),
    ];
    for (id, label, enabled, to) in [
        ("lrclib", "Lyrics from LRCLIB", has("lrclib"), host(&lyr.lrclib_url)),
        ("netease", "Lyrics from NetEase Cloud Music", has("netease"), host(&lyr.netease_url)),
    ] {
        let why_not = if !lyr.enabled {
            "Switched off: lyric lookup on other services is disabled."
        } else if !enabled {
            "Switched off: this provider is not in your list."
        } else {
            ""
        };
        flows.push(serde_json::json!({
            "id": id, "title": label, "to": to,
            "what": "The song's title, artist and duration. Nothing that identifies you except your IP address.",
            "active": why_not.is_empty(), "whyNot": why_not,
        }));
    }
    let why_not = if !cfg.reporting.enabled {
        "Switched off in Play reporting."
    } else if !signed_in {
        "Not sending anything because you are signed out."
    } else {
        ""
    };
    flows.push(serde_json::json!({
        "id": "play-reporting", "title": "Play reporting", "to": yt,
        "what": "Which songs you played and for how long, so they appear in your YouTube Music history and recommendations.",
        "active": why_not.is_empty(), "whyNot": why_not,
    }));
    flows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpl() -> String {
        Config::template()
    }

    fn get(text: &str, path: &str) -> Val {
        lookup(&Config::parse(text).0, path).unwrap()
    }

    #[test]
    fn round_trips_every_type() {
        let mut t = tmpl();
        for (path, v) in [
            ("audio.gapless", Val::Bool(false)),
            ("audio.crossfade_ms", Val::Int(3000)),
            ("downloads.cap_gb", Val::Float(12.5)),
            ("appearance.theme", Val::Text("dark".into())),
            ("lyrics.providers", Val::Texts(vec!["netease".into(), "lrclib".into()])),
            ("audio.eq_bands_db", Val::Floats(vec![1.0, 2.5, 0.0, -3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 4.0])),
            ("keys.search", Val::Text("Ctrl+K".into())),
            ("lyrics.lrclib_url", Val::Text("https://lrc.example.org".into())),
        ] {
            let out = set(&t, path, v.clone()).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!(get(&out.text, path), v, "{path}");
            t = out.text;
        }
    }

    #[test]
    fn comments_and_unknown_keys_survive() {
        let src = "# my notes\nfuture = 1\n\n[audio]\n# keep this\ncrossfade_ms = 100 # fade\ngapless = true\n\n[mystuff]\nx = 1\n";
        let out = set(src, "audio.crossfade_ms", Val::Int(2500)).unwrap();
        assert!(out.text.contains("# my notes"));
        assert!(out.text.contains("# keep this"));
        assert!(out.text.contains("crossfade_ms = 2500 # fade"), "{}", out.text);
        assert!(out.text.contains("future = 1"));
        assert!(out.text.contains("[mystuff]\nx = 1"));
        // a section that is not in the file yet is added
        let out = set(&out.text, "reporting.enabled", Val::Bool(false)).unwrap();
        assert!(out.text.contains("[reporting]"));
        assert!(out.text.contains("[mystuff]"));
    }

    #[test]
    fn invalid_values_are_rejected_with_a_message() {
        let t = tmpl();
        let e = set(&t, "appearance.theme", Val::Text("neon".into())).unwrap_err();
        assert!(e.contains("not one of"), "{e}");
        let e = set(&t, "audio.gapless", Val::Text("yes".into())).unwrap_err();
        assert!(e.contains("on or off"), "{e}");
        let e = set(&t, "audio.crossfade_ms", Val::Text("long".into())).unwrap_err();
        assert!(e.contains("number"), "{e}");
        assert!(set(&t, "nope.nothing", Val::Bool(true)).unwrap_err().contains("Unknown"));
        assert!(set(&t, "lyrics.providers", Val::Texts(vec!["spotify".into()])).is_err());
        assert!(set(&t, "audio.eq_bands_db", Val::Floats(vec![0.0; 3])).unwrap_err().contains("exactly 10"));
        assert!(set("[audio\nbroken", "audio.gapless", Val::Bool(true)).unwrap_err().contains("syntax error"));
    }

    #[test]
    fn ranges_are_clamped() {
        let t = tmpl();
        let out = set(&t, "audio.crossfade_ms", Val::Int(999_999)).unwrap();
        assert_eq!(get(&out.text, "audio.crossfade_ms"), Val::Int(12000));
        let out = set(&t, "lyrics.size_scale", Val::Float(0.01)).unwrap();
        assert_eq!(get(&out.text, "lyrics.size_scale"), Val::Float(0.5));
        let out = set(&t, "audio.eq_bands_db", Val::Floats(vec![99.0; 10])).unwrap();
        assert_eq!(get(&out.text, "audio.eq_bands_db"), Val::Floats(vec![24.0; 10]));
        // floats given for ints round
        let out = set(&t, "network.retries", Val::Float(2.6)).unwrap();
        assert_eq!(get(&out.text, "network.retries"), Val::Int(3));
        assert!(set(&t, "downloads.cap_gb", Val::Float(f64::NAN)).is_err());
        // the download manager runs at most 8 at once: the page must not offer more than takes effect
        let out = set(&t, "downloads.max_parallel", Val::Int(16)).unwrap();
        assert_eq!(get(&out.text, "downloads.max_parallel"), Val::Int(8));
    }

    #[test]
    fn a_bad_value_elsewhere_in_the_file_does_not_block_settings() {
        // this used to reset everything to the defaults, after which no change was accepted
        let src = "[network]\nconnect_timeout_secs = 1.5\n[reporting]\nenabled = false\n";
        let out = set(src, "audio.gapless", Val::Bool(false)).unwrap();
        assert_eq!(get(&out.text, "audio.gapless"), Val::Bool(false));
        assert!(!out.config.reporting.enabled);
        // and the bad value itself can be repaired from the page
        let out = set(src, "network.connect_timeout_secs", Val::Int(15)).unwrap();
        assert_eq!(out.config.network.connect_timeout_secs, 15);
    }

    #[test]
    fn storage_cap_has_no_upper_limit() {
        let out = set(&tmpl(), "downloads.cap_gb", Val::Float(5000.0)).unwrap();
        assert_eq!(get(&out.text, "downloads.cap_gb"), Val::Float(5000.0));
        let out = set(&tmpl(), "downloads.cap_gb", Val::Int(0)).unwrap(); // below the floor: clamped
        assert_eq!(get(&out.text, "downloads.cap_gb"), Val::Float(0.1));
    }

    #[test]
    fn reset_one_and_many() {
        let t = set(&tmpl(), "audio.crossfade_ms", Val::Int(4000)).unwrap().text;
        let t = set(&t, "audio.gapless", Val::Bool(false)).unwrap().text;
        let r = reset(&t, &["audio.crossfade_ms"]).unwrap();
        assert_eq!(get(&r.text, "audio.crossfade_ms"), Val::Int(0));
        assert_eq!(get(&r.text, "audio.gapless"), Val::Bool(false));
        let all: Vec<&str> = meta::in_section("audio").map(|m| m.path).collect();
        let r = reset(&r.text, &all).unwrap();
        assert_eq!(get(&r.text, "audio.gapless"), Val::Bool(true));
        // resetting something absent from the file is fine
        assert!(reset("", &["audio.gapless"]).is_ok());
    }

    #[test]
    fn reset_keeps_comments_and_unknown_keys() {
        let src = "# top\nx = 1\n[audio]\ncrossfade_ms = 500 # fade\n";
        let r = reset(src, &["audio.crossfade_ms"]).unwrap();
        assert!(r.text.contains("# top") && r.text.contains("x = 1") && r.text.contains("# fade"), "{}", r.text);
    }

    #[test]
    fn a_pre_existing_warning_does_not_block_other_edits() {
        let src = "[audio]\nbogus = 1\n";
        assert!(set(src, "audio.gapless", Val::Bool(false)).is_ok());
    }

    #[test]
    fn urls_are_validated() {
        let t = tmpl();
        for bad in ["http://example.org", "ftp://x.org", "https://user:pw@x.org", "not a url", "", "javascript:alert(1)"] {
            assert!(set(&t, "lyrics.lrclib_url", Val::Text(bad.into())).is_err(), "{bad}");
        }
        assert!(set(&t, "lyrics.netease_url", Val::Text("http://localhost:3000".into())).is_ok());
        assert!(set(&t, "youtube.music_origin", Val::Text("http://evil.example".into())).is_err());
        // trailing slash is dropped, and the insecure switch is honoured (the switch must be set first)
        let ok = set(&t, "lyrics.lrclib_url", Val::Text(" https://a.example/ ".into())).unwrap();
        assert_eq!(get(&ok.text, "lyrics.lrclib_url"), Val::Text("https://a.example".into()));
        let t2 = set(&t, "security.allow_insecure_urls", Val::Bool(true)).unwrap().text;
        assert!(set(&t2, "lyrics.lrclib_url", Val::Text("http://a.example".into())).is_ok());
    }

    #[test]
    fn directories_must_be_writable_folders() {
        let base = std::env::temp_dir().join(format!("ytm-settings-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let nested = base.join("a").join("b");
        let out = set(&tmpl(), "downloads.dir", Val::Text(nested.to_string_lossy().into())).unwrap();
        assert!(nested.is_dir());
        assert_eq!(get(&out.text, "downloads.dir"), Val::Text(nested.to_string_lossy().into()));
        assert!(std::fs::read_dir(&nested).unwrap().next().is_none(), "probe file removed");
        assert!(set(&tmpl(), "downloads.dir", Val::Text("relative/dir".into())).is_err());
        let file = base.join("file.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(set(&tmpl(), "downloads.dir", Val::Text(file.to_string_lossy().into())).unwrap_err().contains("file"));
        assert_eq!(get(&set(&tmpl(), "downloads.dir", Val::Text("".into())).unwrap().text, "downloads.dir"), Val::Text(String::new()));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn shortcuts_are_normalised_and_unique() {
        assert_eq!(normalize_shortcut("ctrl+shift+p").unwrap(), "Ctrl+Shift+P");
        assert_eq!(normalize_shortcut("Ctrl++").unwrap(), "Ctrl++");
        assert_eq!(normalize_shortcut("esc").unwrap(), "Escape");
        assert_eq!(normalize_shortcut("alt+f4").unwrap(), "Alt+F4");
        assert_eq!(normalize_shortcut("").unwrap(), "");
        assert!(normalize_shortcut("Ctrl+Banana").is_err());
        assert!(normalize_shortcut("Hyper+A").is_err());
        assert!(normalize_shortcut("Ctrl+Ctrl+A").is_err());
        let t = tmpl();
        let e = set(&t, "keys.search", Val::Text("ctrl+p".into())).unwrap_err();
        assert!(e.contains("Now playing"), "{e}");
        assert!(set(&t, "keys.search", Val::Text("Ctrl+F".into())).is_ok()); // its own value is not a duplicate
                                                                             // unbinding twice is not a duplicate
        let t = set(&t, "keys.search", Val::Text(String::new())).unwrap().text;
        assert!(set(&t, "keys.lyrics", Val::Text(String::new())).is_ok());
    }

    #[test]
    fn key_events_become_sequences() {
        assert_eq!(key_name(0x50, 0x04000000 | 0x02000000), "Ctrl+Shift+P");
        assert_eq!(key_name(0x01000014, 0x04000000), "Ctrl+Right");
        assert_eq!(key_name(0x20, 0), "Space");
        assert_eq!(key_name(0x01000030, 0), "F1");
        assert_eq!(key_name(0x01000021, 0x04000000), ""); // Control alone
        assert_eq!(normalize_shortcut(&key_name(0x01000016, 0x08000000)).unwrap(), "Alt+PgUp");
    }

    #[test]
    fn every_config_path_has_metadata_and_back() {
        let table = toml::Table::try_from(Config::default()).unwrap();
        let mut seen = 0;
        for (section, v) in &table {
            for (key, _) in v.as_table().unwrap() {
                let path = format!("{section}.{key}");
                let m = meta::find(&path).unwrap_or_else(|| panic!("no metadata for `{path}`"));
                assert!(!m.label.is_empty() && !m.description.is_empty(), "{path}");
                assert!(m.min <= m.max && m.soft_max >= m.min, "{path} range");
                seen += 1;
            }
        }
        assert_eq!(seen, meta::META.len(), "metadata for a path that is not in Config");
        // paths are unique
        let mut paths: Vec<_> = meta::META.iter().map(|m| m.path).collect();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), meta::META.len());
    }

    #[test]
    fn defaults_are_valid_under_their_own_rules() {
        let cfg = Config::default();
        for m in meta::META {
            if m.kind == Kind::Dir {
                continue; // default "" only; real folders are covered above
            }
            let d = default_of(m.path).unwrap();
            let c = coerce(m, d.clone(), &cfg).unwrap_or_else(|e| panic!("{}: {e}", m.path));
            assert_eq!(c, d, "{} default would be changed by validation", m.path);
        }
    }

    #[test]
    fn restart_flags_are_present_where_modules_cache() {
        for p in [
            "audio.engine",
            "ui.style",
            "security.proxy_token",
            "security.redact_logs",
            "network.connect_timeout_secs",
            "integration.mpris",
        ] {
            assert!(meta::find(p).unwrap().restart, "{p}");
        }
        for p in ["appearance.theme", "lyrics.karaoke", "downloads.cap_gb", "keys.search"] {
            assert!(!meta::find(p).unwrap().restart, "{p}");
        }
    }

    #[test]
    fn dangerous_switches_carry_a_warning() {
        for p in ["security.redact_logs", "security.proxy_token", "security.allow_insecure_urls"] {
            assert!(!meta::find(p).unwrap().warn.is_empty(), "{p}");
        }
    }

    #[test]
    fn privacy_list_reflects_the_configuration() {
        let mut cfg = Config::default();
        let f = privacy_flows(&cfg, true);
        let active = |f: &[serde_json::Value], id: &str| f.iter().find(|x| x["id"] == id).unwrap()["active"].as_bool().unwrap();
        assert!(active(&f, "lrclib") && active(&f, "play-reporting"));
        assert_eq!(f.iter().find(|x| x["id"] == "lrclib").unwrap()["to"], "lrclib.net");
        assert!(!active(&privacy_flows(&cfg, false), "play-reporting"));
        cfg.lyrics.enabled = false;
        assert!(!active(&privacy_flows(&cfg, true), "netease"));
    }
}
