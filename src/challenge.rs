// SPDX-License-Identifier: GPL-3.0-only
//! Streams of songs YouTube only serves to a signed-in account (age-restricted ones).
//!
//! The anonymous visionOS route (`vision.rs`) is refused for those, and the app clients refuse cookie sign-in, so
//! they are fetched like the web player does it: a signed-in web client's `/player` answer, whose stream URLs only
//! work after the player script's own functions have rewritten the `n` parameter (and decoded the signature, when
//! the URL comes as a `signatureCipher`).
//!
//! The player script is heavily obfuscated and renamed with every release, so nothing here tries to read those
//! functions out of it. The whole script is run in an embedded QuickJS with stubbed browser globals, and the player's
//! own URL class and signature helper are then found by what they do: every candidate is called and its output is
//! checked, so a rename changes nothing.

use regex::Regex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::auth::AUTH_MSG;

/// The user agent the signed-in `/player` call is made with; the stream URL is fetched with the same one.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

/// How long the player script found on YouTube's embed page is trusted before asking again which one is current.
/// YouTube publishes a new one every few days and keeps accepting the previous ones for much longer.
const PLAYER_TTL: Duration = Duration::from_secs(6 * 3600);

/// The player script is ~3 MB today.
const MAX_SCRIPT_BODY: usize = 12 * 1024 * 1024;

/// Everything the script does in the JS engine (loading it and solving) must finish within this.
const JS_BUDGET: Duration = Duration::from_secs(20);
/// Memory the JS engine may use: loading the script takes a few tens of MB.
const JS_MEMORY_LIMIT: usize = 384 * 1024 * 1024;
/// The script's start-up nests deeply; QuickJS's default 256 KB stack is not enough for it.
const JS_STACK: usize = 4 * 1024 * 1024;

/// A signed-in web client whose `/player` answer carries stream URLs for age-restricted videos.
struct WebClient {
    name: &'static str,
    /// `X-YouTube-Client-Name`.
    id: &'static str,
    /// `None` = the YouTube Music web version RustyPipe keeps fresh (`ytapi::client_version`).
    version: Option<&'static str>,
    origin: &'static str,
}

/// Tried in this order. Client constants are hard-coded: they only make sense together with this module's solving.
const CLIENTS: [WebClient; 3] = [
    WebClient { name: "WEB_REMIX", id: "67", version: None, origin: "https://music.youtube.com" },
    WebClient { name: "MWEB", id: "2", version: Some("2.20261105.01.00"), origin: "https://www.youtube.com" },
    WebClient { name: "WEB_CREATOR", id: "62", version: Some("1.20261105.01.00"), origin: "https://www.youtube.com" },
];

// ---------------------------------------------------------------------------------------------
// the player script
// ---------------------------------------------------------------------------------------------

/// The player script currently in use: its 8-hex id and when YouTube was last asked.
static CURRENT_PLAYER: Mutex<Option<(String, Instant)>> = Mutex::new(None);

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("regex"))
}

/// The player id named by YouTube's embed loader (`…/s/player/a5c2aafe/…`, slashes possibly escaped).
fn extract_player_id(iframe_api: &str) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"/s\\?/player\\?/([0-9a-f]{8})\\?/").captures(iframe_api).map(|c| c[1].to_owned())
}

fn is_player_id(id: &str) -> bool {
    id.len() == 8 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The `signatureTimestamp` a `/player` request must name so the answer's URLs match this script's functions.
fn extract_sts(script: &str) -> Option<u32> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?:signatureTimestamp|sts):(\d{5})\b").captures(script).and_then(|c| c[1].parse().ok())
}

fn script_dir() -> PathBuf {
    crate::state::data_dir().join("player_script")
}

fn script_path(id: &str) -> PathBuf {
    script_dir().join(format!("{id}.js"))
}

/// Keep the script for the next start (a restart then only asks for the id), and drop the ones it replaces.
fn store_script(id: &str, script: &str) {
    let dir = script_dir();
    if crate::state::ensure_private_dir(&dir).is_err() {
        return;
    }
    if crate::state::write_private(&script_path(id), script.as_bytes()).is_err() {
        return;
    }
    let keep = format!("{id}.js");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".js") && name != keep && e.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// A stored script, if it is one: readable, of a sane size and with a timestamp in it.
fn load_script(id: &str) -> Option<String> {
    let path = script_path(id);
    let len = std::fs::metadata(&path).ok()?.len();
    if len == 0 || len > MAX_SCRIPT_BODY as u64 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    extract_sts(&text).map(|_| text)
}

async fn fetch_text(url: &str, max: usize) -> Result<String, String> {
    let resp = crate::state::http().get(url).header("User-Agent", USER_AGENT).send().await.map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("YouTube returned HTTP {}", resp.status().as_u16()));
    }
    Ok(String::from_utf8_lossy(&crate::state::read_limited(resp, max).await?).into_owned())
}

/// The current player script: (id, source). The id is asked for at most every [`PLAYER_TTL`]; the source comes from
/// disk when this id was fetched before.
async fn player_script() -> Result<(String, String), String> {
    let known = CURRENT_PLAYER.lock().unwrap().as_ref().filter(|(_, at)| at.elapsed() < PLAYER_TTL).map(|(id, _)| id.clone());
    let id = match known {
        Some(id) => id,
        None => {
            let page = fetch_text("https://www.youtube.com/iframe_api", 512 * 1024).await.map_err(|e| format!("player script: {e}"))?;
            extract_player_id(&page).ok_or("player script: YouTube's embed page names no player")?
        }
    };
    debug_assert!(is_player_id(&id));
    let cached = {
        let id = id.clone();
        tokio::task::spawn_blocking(move || load_script(&id)).await.ok().flatten()
    };
    let script = match cached {
        Some(s) => s,
        None => {
            let url = format!("https://www.youtube.com/s/player/{id}/player_ias.vflset/en_US/base.js");
            let s = fetch_text(&url, MAX_SCRIPT_BODY).await.map_err(|e| format!("player script: {e}"))?;
            if extract_sts(&s).is_none() {
                return Err("player script: no signature timestamp in it".to_owned());
            }
            let (id, copy) = (id.clone(), s.clone());
            let _ = tokio::task::spawn_blocking(move || store_script(&id, &copy)).await;
            s
        }
    };
    *CURRENT_PLAYER.lock().unwrap() = Some((id.clone(), Instant::now()));
    Ok((id, script))
}

// ---------------------------------------------------------------------------------------------
// URLs and ciphers (pure)
// ---------------------------------------------------------------------------------------------

/// What a format of a `/player` answer says about where its stream is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stream {
    /// The URL as YouTube sent it (its `n` not yet rewritten).
    url: String,
    /// `(s, sp)` of a `signatureCipher`: the scrambled signature and the name of the parameter it goes into.
    cipher: Option<(String, String)>,
}

/// Split a `signatureCipher` (`s=…&sp=sig&url=…`, each value percent-encoded once) into its parts.
fn parse_cipher(cipher: &str) -> Option<Stream> {
    // a query string is parsed most reliably as part of a URL
    let u = reqwest::Url::parse(&format!("https://cipher.invalid/?{cipher}")).ok()?;
    let (mut s, mut sp, mut url) = (None, None, None);
    for (k, v) in u.query_pairs() {
        match k.as_ref() {
            "s" => s = Some(v.into_owned()),
            "sp" => sp = Some(v.into_owned()),
            "url" => url = Some(v.into_owned()),
            _ => {}
        }
    }
    let (s, url) = (s.filter(|s| !s.is_empty())?, url.filter(|u| !u.is_empty())?);
    let sp = sp.filter(|p| is_param_name(p)).unwrap_or_else(|| "signature".to_owned());
    Some(Stream { url, cipher: Some((s, sp)) })
}

fn is_param_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The raw (still percent-encoded) value of a query parameter.
fn query_param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let query = url.split_once('?')?.1;
    let query = query.split('#').next().unwrap_or(query);
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then_some(v)
    })
}

/// `url` with query parameter `key` set to the raw `value`: replaced where it stands, appended when absent.
fn set_query_param(url: &str, key: &str, value: &str) -> String {
    let (base, fragment) = match url.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (url, None),
    };
    let (path, query) = base.split_once('?').unwrap_or((base, ""));
    let mut found = false;
    let mut pairs: Vec<String> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            if pair.split_once('=').map_or(pair, |(k, _)| k) == key {
                found = true;
                format!("{key}={value}")
            } else {
                pair.to_owned()
            }
        })
        .collect();
    if !found {
        pairs.push(format!("{key}={value}"));
    }
    let mut out = format!("{path}?{}", pairs.join("&"));
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// Percent-encode everything but the unreserved characters (what `encodeURIComponent` leaves alone, minus `!*'()`).
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode_component(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%').then(|| s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())).flatten();
        match hex {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A rewritten `n` the server will accept: changed, and not the script's "it threw" answer (a marker followed by the
/// original value).
fn n_is_solved(before: &str, after: &str) -> bool {
    !after.is_empty()
        && after.len() <= 128
        && after != before
        && !after.ends_with(before)
        && after.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// A decoded signature: the scrambling only reorders and drops characters, so the result is new, not longer, and
/// made of characters the input had.
fn sig_is_solved(s: &str, raw_out: &str) -> bool {
    let (s, out) = (decode_component(s), decode_component(raw_out));
    !out.is_empty() && out != s && out.len() <= s.len() && out.chars().all(|c| s.contains(c))
}

/// The audio-only format to play out of an `adaptiveFormats` array: Opus 251, else AAC 140, else the best other Opus
/// or AAC (the engine decodes nothing else). Dubbed and loudness-compressed variants are left out.
fn pick_stream(formats: &[Value]) -> Option<(u32, Stream)> {
    let mut best: Option<(u64, u32, Stream)> = None;
    for f in formats {
        let Some(mime) = f["mimeType"].as_str().filter(|m| m.starts_with("audio/")) else { continue };
        let mime = mime.to_ascii_lowercase();
        if f["isDrc"].as_bool() == Some(true) || f["audioTrack"]["audioIsDefault"].as_bool() == Some(false) {
            continue;
        }
        let itag = f["itag"].as_u64().unwrap_or(0) as u32;
        let codec_rank: u64 = if mime.contains("opus") {
            2
        } else if mime.contains("mp4a") {
            1
        } else {
            continue;
        };
        let stream = match (f["url"].as_str(), f["signatureCipher"].as_str().or_else(|| f["cipher"].as_str())) {
            (Some(u), _) if !u.is_empty() => Stream { url: u.to_owned(), cipher: None },
            (_, Some(c)) => match parse_cipher(c) {
                Some(s) => s,
                None => continue,
            },
            _ => continue,
        };
        // the two wanted itags first, then codec, then bitrate
        let rank = match itag {
            251 => 4u64 << 40,
            140 => 3 << 40,
            _ => codec_rank << 32,
        } + f["bitrate"].as_u64().unwrap_or(0).min(u32::MAX as u64);
        if best.as_ref().is_none_or(|(r, _, _)| rank > *r) {
            best = Some((rank, itag, stream));
        }
    }
    best.map(|(_, itag, s)| (itag, s))
}

// ---------------------------------------------------------------------------------------------
// running the player script
// ---------------------------------------------------------------------------------------------

/// Browser globals the script touches while it starts up. Nothing in here does anything: the script only has to get
/// through its top level so its functions exist.
const JS_PRELUDE: &str = r#"
var window = globalThis, self = globalThis;
var __yqt_dummy = function () {
  return new Proxy(function () {}, {
    get: function (t, k) {
      if (k === Symbol.toPrimitive || k === "toString" || k === "valueOf") return function () { return ""; };
      if (k === "length") return 0;
      if (k === "prototype") return {};
      if (typeof k === "symbol") return undefined;
      return __yqt_dummy();
    },
    apply: function () { return __yqt_dummy(); },
    construct: function () { return __yqt_dummy(); },
    set: function () { return true; },
    has: function () { return true; }
  });
};
var document = __yqt_dummy();
var navigator = { userAgent: "", platform: "", language: "en" };
var location = { hostname: "www.youtube.com", host: "www.youtube.com", href: "https://www.youtube.com/",
  protocol: "https:", origin: "https://www.youtube.com", pathname: "/", search: "", hash: "" };
var XMLHttpRequest = function () {};
XMLHttpRequest.prototype = { open: function () {}, send: function () {}, setRequestHeader: function () {} };
var setTimeout = function () { return 0; }, clearTimeout = function () {};
var setInterval = function () { return 0; }, clearInterval = function () {};
var requestAnimationFrame = function () { return 0; }, cancelAnimationFrame = function () {};
"#;

/// Finds the player's URL class and signature helper by behaviour and returns what they make of the input.
///
/// The URL class is the one with `get`, `set` and `clone` that, built as `new C(url, true)`, has a no-argument method
/// answering an https URL: building it that way is what rewrites `n`. The signature helper is a three-argument
/// function `(url, sp, s)` returning such an object. Both lists are only candidates: Rust checks every answer.
const JS_DRIVER: &str = r#"
(function () {
  var input = JSON.parse(__yqt_in), g = _yt_player, ev = g.__yqt_eval;
  var out = { n: [], sig: [] };
  function text(o) {
    if (o === null || typeof o !== "object") return null;
    var p = Object.getPrototypeOf(o);
    if (!p || p === Object.prototype) return null;
    var keys = Object.getOwnPropertyNames(p);
    for (var i = 0; i < keys.length; i++) {
      var k = keys[i];
      if (k === "constructor" || k === "get" || k === "set" || k === "clone") continue;
      try {
        if (typeof p[k] !== "function" || p[k].length !== 0) continue;
        var r = o[k]();
        if (typeof r === "string" && r.slice(0, 8) === "https://") return r;
      } catch (e) {}
    }
    return null;
  }
  function isUrlClass(f) {
    try {
      var p = typeof f === "function" && f.prototype;
      return !!p && typeof p.get === "function" && typeof p.set === "function" && typeof p.clone === "function";
    } catch (e) { return false; }
  }
  var classes = [];
  function add(f) { if (isUrlClass(f) && classes.indexOf(f) < 0) classes.push(f); }
  var names = Object.keys(g);
  for (var i = 0; i < names.length; i++) { try { add(g[names[i]]); } catch (e) {} }
  if (ev) input.classes.forEach(function (name) { try { add(ev(name)); } catch (e) {} });
  classes.forEach(function (C) {
    try { var r = text(new C(input.url, true)); if (r) out.n.push(r); } catch (e) {}
  });
  if (input.s && ev) input.fns.forEach(function (name) {
    try {
      var f = ev(name);
      if (typeof f !== "function") return;
      var r = text(f(input.url, input.sp, input.s));
      if (r) out.sig.push(r);
    } catch (e) {}
  });
  return JSON.stringify(out);
})()
"#;

/// Names worth trying, read off the script's text: functions of three arguments that start by building
/// `new X(arg, !0)` (the signature helper does), and the `X`s. Only hints: the driver checks what each one does.
fn candidates(script: &str) -> (Vec<String>, Vec<String>) {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = re(&R, r"(?:^|[;,\n{}]|var )([\w$]{1,8})=function\(\w+,\w+,\w+\)\{[^{}]{0,240}?=new ([\w$]{1,8}(?:\.[\w$]{1,8})?)\(\w+,!0\)");
    let (mut fns, mut classes) = (Vec::new(), Vec::new());
    for c in r.captures_iter(script).take(64) {
        for (list, name) in [(&mut fns, &c[1]), (&mut classes, &c[2])] {
            if !list.iter().any(|n: &String| n == name) {
                list.push(name.to_owned());
            }
        }
    }
    (fns, classes)
}

/// The script with a way in: closure-local functions are reached by name through an `eval` planted just before the
/// closing of its outer function. A script of another shape is run as it is (the URL class is usually exported).
fn with_hook(script: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let head = script.get(..script.len().min(400)).unwrap_or("");
    let param = re(&R, r"\(function\(([\w$]{1,8})\)\{").captures(head).map(|c| c[1].to_owned());
    match (param, script.rfind("})(_yt_player)")) {
        (Some(param), Some(at)) => {
            let mut s = String::with_capacity(script.len() + 64);
            s.push_str(&script[..at]);
            s.push_str(&format!(";{param}.__yqt_eval=function(__s){{return eval(__s)}};"));
            s.push_str(&script[at..]);
            s
        }
        _ => script.to_owned(),
    }
}

/// Run the player script and let it rewrite `stream`: the final URL, or why not. Blocks for up to `budget`.
fn solve_blocking(script: &str, stream: &Stream, budget: Duration) -> Result<String, String> {
    use rquickjs::{context::EvalOptions, Context, Runtime};
    let before_n = query_param(&stream.url, "n").map(str::to_owned);
    if before_n.is_none() && stream.cipher.is_none() {
        return Ok(stream.url.clone());
    }
    let (fns, classes) = candidates(script);
    let (s, sp) = stream.cipher.clone().unzip();
    let input = json!({"url": stream.url, "s": s, "sp": sp, "fns": fns, "classes": classes}).to_string();

    let rt = Runtime::new().map_err(|_| "player script: no JS engine")?;
    rt.set_memory_limit(JS_MEMORY_LIMIT);
    rt.set_max_stack_size(JS_STACK);
    // a script (or a wrong candidate) that never returns is stopped here instead of hanging the app
    let deadline = Instant::now() + budget;
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let ctx = Context::full(&rt).map_err(|_| "player script: no JS engine")?;
    let answer: Result<String, String> = ctx.with(|ctx| {
        // the script is not strict-mode code (it takes `this` for the window)
        let sloppy = || {
            let mut o = EvalOptions::default();
            o.strict = false;
            o
        };
        ctx.eval_with_options::<(), _>(JS_PRELUDE, sloppy()).map_err(|_| "player script: the browser stand-ins failed")?;
        ctx.eval_with_options::<(), _>(with_hook(script), sloppy()).map_err(|_| {
            if Instant::now() > deadline {
                "player script: it took too long to start"
            } else {
                "player script: it did not run"
            }
        })?;
        ctx.globals().set("__yqt_in", input).map_err(|_| "player script: no JS engine")?;
        ctx.eval_with_options::<String, _>(JS_DRIVER, sloppy()).map_err(|_| {
            if Instant::now() > deadline {
                "player script: solving took too long".to_owned()
            } else {
                "player script: solving failed".to_owned()
            }
        })
    });
    let out: Value = serde_json::from_str(&answer?).map_err(|_| "player script: unreadable answer")?;
    let list = |key: &str| -> Vec<&str> { out[key].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default() };
    let (n_urls, sig_urls) = (list("n"), list("sig"));

    // Only the two solved values are taken from the script's answers and put into YouTube's own URL: whatever else
    // the script made of the URL (the web player adds `alr=yes`, which turns redirects into text bodies) is dropped.
    let mut url = stream.url.clone();
    if let Some((s, sp)) = &stream.cipher {
        let sig = sig_urls
            .iter()
            .filter_map(|u| query_param(u, sp))
            .find(|v| sig_is_solved(s, v))
            .ok_or("player script: the signature function was not found")?;
        url = set_query_param(&url, sp, &encode_component(&decode_component(sig)));
    }
    if let Some(before) = &before_n {
        let n = sig_urls
            .iter()
            .chain(n_urls.iter())
            .filter_map(|u| query_param(u, "n"))
            .find(|v| n_is_solved(before, v))
            .ok_or("player script: the n function was not found")?;
        url = set_query_param(&url, "n", n);
    }
    Ok(url)
}

/// [`solve_blocking`] off the async runtime, on a thread with a stack deep enough for the script's start-up.
async fn solve(script: String, stream: Stream) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        std::thread::Builder::new()
            .name("player-script".into())
            .stack_size(JS_STACK * 4)
            .spawn(move || solve_blocking(&script, &stream, JS_BUDGET))
            .map_err(|_| "player script: could not start a thread".to_owned())?
            .join()
            .map_err(|_| "player script: the JS engine crashed".to_owned())?
    })
    .await
    .map_err(|_| "player script: the solver was cancelled".to_owned())?
}

// ---------------------------------------------------------------------------------------------
// the signed-in request
// ---------------------------------------------------------------------------------------------

/// YouTube's own words for a refusal, cut to a sane length.
fn refusal(player: &Value) -> String {
    let status = &player["playabilityStatus"];
    let why = status["reason"].as_str().or_else(|| status["status"].as_str()).unwrap_or("no answer");
    why.chars().filter(|c| !c.is_control()).take(160).collect()
}

/// Ask one web client for a video's formats: with the stored session, or (`signed_in` false) as nobody, which is
/// enough for videos that are not restricted and lets the solving be checked without an account.
async fn player_response(client: &WebClient, video_id: &str, sts: u32, signed_in: bool) -> Result<Value, String> {
    let version = client.version.map(str::to_owned).unwrap_or_else(crate::ytapi::client_version);
    let origin = client.origin;
    debug_assert!(crate::ytapi::is_trusted_youtube_url(origin));
    let body = json!({
        "context": {"client": {
            "clientName": client.name, "clientVersion": version, "hl": crate::config::current().youtube.language,
            "userAgent": USER_AGENT
        }},
        "videoId": video_id,
        "racyCheckOk": true, "contentCheckOk": true,
        "playbackContext": {"contentPlaybackContext": {"html5Preference": "HTML5_PREF_WANTS", "signatureTimestamp": sts}}
    });
    let mut req = crate::state::http()
        .post(format!("{origin}/youtubei/v1/player?prettyPrint=false"))
        .header("Origin", origin)
        .header("X-Origin", origin)
        .header("X-YouTube-Client-Name", client.id)
        .header("X-YouTube-Client-Version", version.as_str())
        .header("User-Agent", USER_AGENT);
    if signed_in {
        let cookie = crate::auth::stored_cookie().ok_or(AUTH_MSG)?;
        let sapisid = crate::auth::sapisid(&cookie).ok_or(AUTH_MSG)?;
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
        req = req
            .header("Authorization", crate::ytapi::sapisid_hash(sapisid, ts, origin))
            .header("X-Goog-AuthUser", "0")
            .header("Cookie", cookie);
    }
    let resp = req.json(&body).send().await.map_err(|e| e.without_url().to_string())?;
    match resp.status().as_u16() {
        200 => {}
        401 => return Err(AUTH_MSG.to_owned()),
        s => return Err(format!("YouTube returned HTTP {s}")),
    }
    serde_json::from_slice(&crate::state::read_limited(resp, crate::state::MAX_API_BODY).await?).map_err(|e| e.to_string())
}

/// Does the server hand out the stream at this URL? A wrong `n` or signature is answered with 403, and so is a
/// stream that wants more than this module can give; finding out here lets the next client be tried.
async fn probe(url: &str) -> Result<(), String> {
    let resp = crate::state::http()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Range", "bytes=0-1")
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    match resp.status().as_u16() {
        200 | 206 => Ok(()),
        s => Err(format!("the stream server answered HTTP {s}")),
    }
}

/// A signed-in request answered "sign in": YouTube no longer accepts the stored session (a session copied from a
/// browser stops working when the browser rotates its cookies).
fn session_refused(player: &Value) -> bool {
    player["playabilityStatus"]["status"].as_str() == Some("LOGIN_REQUIRED")
}

async fn resolve_with(client: &WebClient, video_id: &str, script: &str, sts: u32, signed_in: bool) -> Result<(u32, String), String> {
    let player = player_response(client, video_id, sts, signed_in).await?;
    if signed_in && session_refused(&player) {
        return Err(AUTH_MSG.to_owned());
    }
    if player["playabilityStatus"]["status"].as_str() != Some("OK") {
        return Err(refusal(&player));
    }
    let formats = player["streamingData"]["adaptiveFormats"].as_array().ok_or("no streams in the answer")?;
    let (itag, stream) = pick_stream(formats).ok_or("no Opus or AAC stream with an address")?;
    let url = solve(script.to_owned(), stream).await?;
    if !crate::vision::is_media_url(&url) {
        return Err("the stream is not on a YouTube media host".to_owned());
    }
    probe(&url).await?;
    Ok((itag, url))
}

/// A playable audio stream of a video YouTube only serves to a signed-in account, fetched signed in.
/// Returns (stream url, the User-Agent the URL must be fetched with).
pub async fn signed_in_audio(video_id: &str) -> Result<(String, String), String> {
    resolve(video_id, true).await.map(|(_, _, url)| (url, USER_AGENT.to_owned()))
}

/// The playback-tracking URL of a video, from the signed-in YouTube Music web player answer (what the play reporter
/// pings so a listen reaches the account's history). The app clients refuse the cookie sign-in (HTTP 400), and the
/// web client only answers properly when told the player script's signature timestamp, which is known here.
pub async fn tracking_url(video_id: &str) -> Result<String, String> {
    if crate::auth::stored_cookie().is_none() {
        return Err(AUTH_MSG.to_owned());
    }
    let (_, script) = player_script().await?;
    let sts = extract_sts(&script).ok_or("player script: no signature timestamp in it")?;
    let player = player_response(&CLIENTS[0], video_id, sts, true).await?;
    if session_refused(&player) {
        return Err(AUTH_MSG.to_owned());
    }
    let url = crate::ytapi::extract_tracking_url(&player).ok_or_else(|| format!("no playback-tracking URL ({})", refusal(&player)))?;
    if crate::ytapi::is_trusted_youtube_url(&url) {
        Ok(url)
    } else {
        Err("YouTube returned an unexpected playback-tracking host".into())
    }
}

/// [`signed_in_audio`] with the client and itag it ended on. Not signed in, it only resolves unrestricted videos.
async fn resolve(video_id: &str, signed_in: bool) -> Result<(&'static str, u32, String), String> {
    if signed_in && crate::auth::stored_cookie().is_none() {
        return Err(AUTH_MSG.to_owned());
    }
    let (_, script) = player_script().await?;
    let sts = extract_sts(&script).ok_or("player script: no signature timestamp in it")?;
    let mut last = String::new();
    for client in &CLIENTS {
        match resolve_with(client, video_id, &script, sts, signed_in).await {
            Ok((itag, url)) => return Ok((client.name, itag, url)),
            Err(e) if e == AUTH_MSG => return Err(e),
            Err(e) => {
                tracing::debug!(client = client.name, error = %e, "signed-in stream refused");
                last = format!("{}: {e}", client.name);
            }
        }
    }
    Err(format!("signed-in playback failed ({last})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the player script with the same shape and other names: an exported URL class that rewrites
    /// `n` (reversed) when built with `true`, and a closure-local signature helper (drops the first character).
    fn fake_player(class: &str, text_method: &str, sig_fn: &str) -> String {
        r#"var _yt_player={};(function(g){var window=this;var SIGFN,unused;
CLASS=function(a,b){var i=a.indexOf("?");this.p=i<0?a:a.slice(0,i);this.q={};this.o=[];var me=this;
(i<0?"":a.slice(i+1)).split("&").forEach(function(kv){if(!kv)return;var j=kv.indexOf("=");var k=j<0?kv:kv.slice(0,j);me.o.push(k);me.q[k]=j<0?"":kv.slice(j+1)});
if(b&&this.q.n)this.q.n=this.q.n.split("").reverse().join("")};
CLASS.prototype.set=function(a,b){if(!(a in this.q))this.o.push(a);this.q[a]=b};
CLASS.prototype.get=function(a){return this.q[a]||null};
CLASS.prototype.TEXT=function(){var me=this;return this.p+"?"+this.o.map(function(k){return k+"="+me.q[k]}).join("&")};
CLASS.prototype.clone=function(){return new CLASS(this.TEXT(),!1)};
unused=function(a,b,c){b=b===void 0?"":b;a=new CLASS(a,!0);return c};
SIGFN=function(a,b,c){b=b===void 0?"":b;c=c===void 0?"":c;a=new CLASS(a,!0);a.set("alr","yes");c&&a.set(b,encodeURIComponent(decodeURIComponent(c).slice(1)));return a};
var cfg={signatureTimestamp:20760};
})(_yt_player);
"#
        .replace("CLASS", class)
        .replace("TEXT", text_method)
        .replace("SIGFN", sig_fn)
    }

    const BUDGET: Duration = Duration::from_secs(20);
    const PLAIN: &str = "https://rr1---sn-abc.googlevideo.com/videoplayback?expire=1&n=AbCdEf123&itag=251";

    #[test]
    fn player_id_and_timestamp_are_read() {
        let api = r#"var scriptUrl = 'https:\/\/www.youtube.com\/s\/player\/a5c2aafe\/www-widgetapi.vflset\/www-widgetapi.js';"#;
        assert_eq!(extract_player_id(api).as_deref(), Some("a5c2aafe"));
        assert_eq!(extract_player_id("https://www.youtube.com/s/player/0123abcd/x.js").as_deref(), Some("0123abcd"));
        assert_eq!(extract_player_id("/s/player/../etc/x"), None);
        assert!(is_player_id("a5c2aafe"));
        assert!(!is_player_id("A5C2AAFE") && !is_player_id("a5c2aaf") && !is_player_id("../../ab"));
        assert_eq!(extract_sts("a={signatureTimestamp:20760,x:1}"), Some(20760));
        assert_eq!(extract_sts("a={sts:20761}"), Some(20761));
        assert_eq!(extract_sts("nothing"), None);
    }

    #[test]
    fn a_cipher_is_split_and_decoded_once() {
        let c = "s=AB%253DCD%3D&sp=sig&url=https%3A%2F%2Frr1---sn-abc.googlevideo.com%2Fvideoplayback%3Fexpire%3D1%26n%3Dxyz";
        let s = parse_cipher(c).unwrap();
        assert_eq!(s.url, "https://rr1---sn-abc.googlevideo.com/videoplayback?expire=1&n=xyz");
        assert_eq!(s.cipher, Some(("AB%3DCD=".to_owned(), "sig".to_owned())));
        assert_eq!(parse_cipher("s=abc&url=https%3A%2F%2Fx").unwrap().cipher.unwrap().1, "signature", "sp defaults");
        assert_eq!(parse_cipher("s=abc&sp=a%26b&url=https%3A%2F%2Fx").unwrap().cipher.unwrap().1, "signature", "odd sp is not used");
        assert!(parse_cipher("sp=sig&url=https%3A%2F%2Fx").is_none(), "no signature");
        assert!(parse_cipher("s=abc&sp=sig").is_none(), "no url");
        assert!(parse_cipher("").is_none());
    }

    #[test]
    fn query_parameters_are_read_and_replaced_in_place() {
        assert_eq!(query_param(PLAIN, "n"), Some("AbCdEf123"));
        assert_eq!(query_param(PLAIN, "itag"), Some("251"));
        assert_eq!(query_param(PLAIN, "ita"), None);
        assert_eq!(query_param("https://x/y?mn=1&n=2#n=3", "n"), Some("2"));
        assert_eq!(query_param("https://x/y", "n"), None);
        assert_eq!(set_query_param(PLAIN, "n", "new"), "https://rr1---sn-abc.googlevideo.com/videoplayback?expire=1&n=new&itag=251");
        assert_eq!(set_query_param("https://x/y?mn=1&a=%2C", "n", "v"), "https://x/y?mn=1&a=%2C&n=v", "appended, the rest untouched");
        assert_eq!(set_query_param("https://x/y", "sig", "v"), "https://x/y?sig=v");
        assert_eq!(set_query_param("https://x/y?n=1#f", "n", "2"), "https://x/y?n=2#f");
        assert_eq!(encode_component("aZ09-_.~=/+ %"), "aZ09-_.~%3D%2F%2B%20%25");
        assert_eq!(decode_component("a%3Db%2f%zz%"), "a=b/%zz%");
    }

    #[test]
    fn solved_values_are_told_from_failures() {
        assert!(n_is_solved("AbCdEf", "x9-_Q"));
        assert!(!n_is_solved("AbCdEf", "AbCdEf"), "unchanged");
        assert!(!n_is_solved("AbCdEf", "enhanced_except_AbCdEf"), "the script's error answer");
        assert!(!n_is_solved("AbCdEf", ""));
        assert!(!n_is_solved("AbCdEf", "a&b=c"));
        assert!(sig_is_solved("ABCDEF", "FBCDE"));
        assert!(sig_is_solved("AB%3DCD", "DCB%3D"));
        assert!(!sig_is_solved("ABCDEF", "ABCDEF"));
        assert!(!sig_is_solved("ABCDEF", "ABCDEFG"));
        assert!(!sig_is_solved("ABCDEF", "ABCXEF"));
        assert!(!sig_is_solved("ABCDEF", ""));
    }

    #[test]
    fn the_wanted_format_is_picked() {
        let u = |tag: &str| format!("https://rr1---sn-abc.googlevideo.com/videoplayback?itag={tag}&n=abc");
        let formats = vec![
            json!({"itag":137,"mimeType":"video/mp4; codecs=\"avc1\"","bitrate":9_000_000,"url":u("137")}),
            json!({"itag":140,"mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":130_000,"url":u("140")}),
            json!({"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":134_000,"isDrc":true,"url":u("251drc")}),
            json!({"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":134_000,"audioTrack":{"audioIsDefault":false},"url":u("251dub")}),
            json!({"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":134_000,"audioTrack":{"audioIsDefault":true},"url":u("251")}),
            json!({"itag":250,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":70_000,"url":u("250")}),
            json!({"itag":380,"mimeType":"audio/mp4; codecs=\"ac-3\"","bitrate":900_000,"url":u("380")}),
        ];
        let (itag, s) = pick_stream(&formats).unwrap();
        assert_eq!((itag, s.url.as_str(), s.cipher.is_none()), (251, u("251").as_str(), true));
        let (itag, _) = pick_stream(&formats[..2]).unwrap();
        assert_eq!(itag, 140);
        // neither wanted itag: Opus before AAC, then bitrate
        let rest = vec![
            json!({"itag":139,"mimeType":"audio/mp4; codecs=\"mp4a.40.5\"","bitrate":300_000,"url":u("139")}),
            json!({"itag":249,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":50_000,"url":u("249")}),
            json!({"itag":250,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":70_000,"signatureCipher":"s=abc&sp=sig&url=https%3A%2F%2Fx%2Fy"}),
        ];
        let (itag, s) = pick_stream(&rest).unwrap();
        assert_eq!((itag, s.cipher), (250, Some(("abc".to_owned(), "sig".to_owned()))));
        // formats without an address (SABR-only answers) are no use
        assert!(pick_stream(&[json!({"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":1})]).is_none());
        assert!(pick_stream(&[]).is_none());
    }

    #[test]
    fn candidates_are_read_off_the_script_whatever_their_names() {
        let (fns, classes) = candidates(&fake_player("g.Uc", "Xx", "Zq"));
        assert_eq!(fns, ["unused", "Zq"]);
        assert_eq!(classes, ["g.Uc"]);
        let (fns, classes) = candidates(&fake_player("$a", "b", "c$"));
        assert_eq!((fns, classes), (vec!["unused".to_owned(), "c$".to_owned()], vec!["$a".to_owned()]));
        assert_eq!(candidates("var a=function(a,b){return 1};"), (vec![], vec![]));
    }

    #[test]
    fn the_hook_goes_before_the_closing_of_the_outer_function() {
        let hooked = with_hook("var _yt_player={};(function(q){var window=this;q.a=1;})(_yt_player);\n");
        assert!(hooked.ends_with(";q.__yqt_eval=function(__s){return eval(__s)};})(_yt_player);\n"), "{hooked}");
        assert_eq!(with_hook("something else"), "something else");
    }

    #[test]
    fn n_is_solved_by_the_scripts_own_url_class() {
        let plain = Stream { url: PLAIN.to_owned(), cipher: None };
        for (class, method) in [("g.Uc", "Xx"), ("g.zz9", "toUrl")] {
            let url = solve_blocking(&fake_player(class, method, "Zq"), &plain, BUDGET).unwrap();
            assert_eq!(url, "https://rr1---sn-abc.googlevideo.com/videoplayback?expire=1&n=321fEdCbA&itag=251");
        }
        // a class that is not exported is reached by the name found next to `new X(a,!0)`
        let local = fake_player("Kq", "Xx", "Zq").replace("var SIGFN,unused;", "var Kq,SIGFN,unused;");
        assert!(solve_blocking(&local, &plain, BUDGET).unwrap().contains("&n=321fEdCbA&"));
        // nothing to solve: the URL comes back as it is, without running anything
        let no_n = Stream { url: "https://rr1---sn-abc.googlevideo.com/videoplayback?itag=251".to_owned(), cipher: None };
        assert_eq!(solve_blocking("not even javascript {", &no_n, BUDGET).unwrap(), no_n.url);
    }

    #[test]
    fn a_cipher_gets_its_signature_and_n_without_the_players_extras() {
        let stream = Stream { url: PLAIN.to_owned(), cipher: Some(("XAB%3DCD".to_owned(), "sig".to_owned())) };
        let url = solve_blocking(&fake_player("g.Uc", "Xx", "Zq"), &stream, BUDGET).unwrap();
        assert_eq!(url, "https://rr1---sn-abc.googlevideo.com/videoplayback?expire=1&n=321fEdCbA&itag=251&sig=AB%3DCD");
        assert!(!url.contains("alr"));
    }

    #[test]
    fn a_script_that_cannot_solve_is_an_error_not_a_wrong_url() {
        let plain = Stream { url: PLAIN.to_owned(), cipher: None };
        let ciphered = Stream { url: PLAIN.to_owned(), cipher: Some(("XABCD".to_owned(), "sig".to_owned())) };
        // n comes back unchanged
        let lazy = fake_player("g.Uc", "Xx", "Zq").replace(r#".split("").reverse().join("")"#, "");
        assert_eq!(solve_blocking(&lazy, &plain, BUDGET).unwrap_err(), "player script: the n function was not found");
        // the signature comes back unchanged
        let same_sig = fake_player("g.Uc", "Xx", "Zq").replace(".slice(1)", "");
        assert_eq!(solve_blocking(&same_sig, &ciphered, BUDGET).unwrap_err(), "player script: the signature function was not found");
        assert_eq!(solve_blocking("this is not javascript {", &plain, BUDGET).unwrap_err(), "player script: it did not run");
        let e = solve_blocking("var _yt_player={};", &plain, BUDGET).unwrap_err();
        assert_eq!(e, "player script: the n function was not found");
        assert!(!e.contains("googlevideo"), "no URL in an error");
    }

    #[test]
    fn a_script_that_never_returns_is_stopped() {
        let plain = Stream { url: PLAIN.to_owned(), cipher: None };
        let started = Instant::now();
        let e = solve_blocking("var _yt_player={};for(;;){}", &plain, Duration::from_millis(300)).unwrap_err();
        assert_eq!(e, "player script: it took too long to start");
        assert!(started.elapsed() < Duration::from_secs(10));
        // a candidate that hangs when called
        let hang = fake_player("g.Uc", "Xx", "Zq").replace("if(b&&this.q.n)", "if(b)for(;;){}if(b&&this.q.n)");
        assert_eq!(solve_blocking(&hang, &plain, Duration::from_millis(1500)).unwrap_err(), "player script: solving took too long");
    }

    #[test]
    fn a_refusal_is_reported_in_youtubes_words() {
        let v = json!({"playabilityStatus": {"status": "LOGIN_REQUIRED", "reason": "Sign in to confirm\n your age"}});
        assert_eq!(refusal(&v), "Sign in to confirm your age");
        assert_eq!(refusal(&json!({"playabilityStatus": {"status": "ERROR"}})), "ERROR");
        assert_eq!(refusal(&json!({})), "no answer");
    }
}

/// Live checks against YouTube with the app's own session
/// (`cargo test --bin yqt live_challenge -- --ignored --nocapture`). Only statuses and sizes are printed.
#[cfg(test)]
mod live {
    use super::*;

    const GATED: &str = "n4fLhgP8l44";
    const CONTROL: &str = "dQw4w9WgXcQ";

    async fn range(url: &str, ua: &str, from: u64, to: u64) -> (u16, Vec<u8>) {
        let resp = reqwest::Client::new()
            .get(url)
            .header("User-Agent", ua)
            .header("Range", format!("bytes={from}-{to}"))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| e.without_url().to_string())
            .expect("stream request");
        let status = resp.status().as_u16();
        let body = crate::state::read_limited(resp, 1024 * 1024).await.unwrap_or_default();
        (status, body)
    }

    fn sniff(head: &[u8]) -> &'static str {
        if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
            "webm"
        } else if head.get(4..8) == Some(b"ftyp".as_slice()) {
            "mp4"
        } else {
            "unknown"
        }
    }

    async fn check(video_id: &str, signed_in: bool) {
        let (client, itag, url) = match resolve(video_id, signed_in).await {
            Ok(r) => r,
            Err(e) if e == AUTH_MSG => panic!("stored session not accepted: cannot verify signed-in route (sign in again, then re-run)"),
            Err(e) => panic!("resolve: {e}"),
        };
        let (url2, ua) = (url.clone(), USER_AGENT);
        let total: u64 = query_param(&url2, "clen").and_then(|c| c.parse().ok()).expect("clen in the stream URL");
        println!("{video_id}: resolved via {client}, itag {itag}, {total} bytes, pot in url: {}", query_param(&url, "pot").is_some());
        let (status, head) = range(&url, ua, 0, 65_535).await;
        println!("{video_id}: head  HTTP {status}, {} bytes, {}", head.len(), sniff(&head));
        assert!(matches!(status, 200 | 206), "head status {status}");
        assert_ne!(sniff(&head), "unknown");
        assert!(head.len() >= 65_536.min(total as usize));
        let from = total.saturating_sub(65_536);
        let (status, tail) = range(&url, ua, from, total - 1).await;
        println!("{video_id}: tail  HTTP {status}, {} bytes from offset {from}", tail.len());
        assert!(matches!(status, 200 | 206), "tail status {status}");
        assert_eq!(tail.len() as u64, total - from);
        // and the middle, well past the first megabyte
        let mid = total / 2;
        let (status, body) = range(&url, ua, mid, (mid + 65_535).min(total - 1)).await;
        println!("{video_id}: mid   HTTP {status}, {} bytes from offset {mid}", body.len());
        assert!(matches!(status, 200 | 206), "mid status {status}");
    }

    #[tokio::test]
    #[ignore]
    async fn live_challenge_age_restricted_song_plays_signed_in() {
        check(GATED, true).await;
    }

    #[tokio::test]
    #[ignore]
    async fn live_challenge_ciphered_song_plays_signed_in() {
        check(CONTROL, true).await;
    }

    /// Needs no account: an unrestricted song through the same web client, signature and `n` solved by the real
    /// player script, the stream fetched at its start, middle and end.
    #[tokio::test]
    #[ignore]
    async fn live_challenge_solving_works_without_an_account() {
        check(CONTROL, false).await;
    }

    /// What each client answers for both songs, and whether its stream is served.
    #[tokio::test]
    #[ignore]
    async fn live_challenge_survey_of_clients() {
        let (id, script) = player_script().await.expect("player script");
        let sts = extract_sts(&script).unwrap();
        let (fns, classes) = candidates(&script);
        println!("player {id}, {} bytes, sts {sts}, {} function and {} class candidates", script.len(), fns.len(), classes.len());
        let signed_in_too = crate::auth::stored_cookie().is_some();
        for (video, signed_in) in [(CONTROL, false), (GATED, false), (CONTROL, true), (GATED, true)] {
            if signed_in && !signed_in_too {
                println!("{video} signed in: no stored session");
                continue;
            }
            let video = if signed_in { format!("{video} signed in") } else { format!("{video} anonymous") };
            let id = &video[..11];
            for client in &CLIENTS {
                let player = match player_response(client, id, sts, signed_in).await {
                    Ok(p) => p,
                    Err(e) => {
                        println!("{video} {}: request failed: {e}", client.name);
                        continue;
                    }
                };
                if signed_in && session_refused(&player) {
                    println!("{video} {}: stored session not accepted ({})", client.name, refusal(&player));
                    continue;
                }
                let status = player["playabilityStatus"]["status"].as_str().unwrap_or("-").to_owned();
                let formats = player["streamingData"]["adaptiveFormats"].as_array().cloned().unwrap_or_default();
                let audio: Vec<&Value> =
                    formats.iter().filter(|f| f["mimeType"].as_str().is_some_and(|m| m.starts_with("audio/"))).collect();
                let plain = audio.iter().filter(|f| f["url"].is_string()).count();
                let ciphered = audio.iter().filter(|f| f["signatureCipher"].is_string()).count();
                print!("{video} {}: {status}, {} audio formats ({plain} plain, {ciphered} ciphered)", client.name, audio.len());
                let Some((itag, stream)) = pick_stream(&formats) else {
                    println!(", nothing to pick ({})", refusal(&player));
                    continue;
                };
                let started = Instant::now();
                match solve(script.clone(), stream).await {
                    Ok(url) => {
                        let served = probe(&url).await;
                        println!(
                            ", itag {itag}, solved in {:?}, probe: {}",
                            started.elapsed(),
                            served.err().unwrap_or_else(|| "served".into())
                        );
                    }
                    Err(e) => println!(", itag {itag}, not solved: {e}"),
                }
            }
        }
    }
}
