// SPDX-License-Identifier: GPL-3.0-only
//! `Accent`: the dominant colour of a cover image, for tinting pages, lyrics and highlights.
//!
//! The cover (from the cover cache, fetched if needed) is decoded, shrunk to ~32 px and run through
//! [`dominant`], which prefers a *saturated* colour, ignores near-black / near-white / grey pixels, and
//! finally keeps the lightness in a readable band. Work happens off the Qt thread; results are cached per URL.
//!
//! QML usage:
//! ```qml
//! Accent { id: accent; source: Player.cover }
//! color: accent.ready ? accent.color : Kirigami.Theme.highlightColor
//! ```
use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;
use std::collections::HashMap;
use std::sync::Mutex;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        /// Image URL to take the colour from (set by QML).
        #[qproperty(QString, source)]
        /// `#rrggbb` once known, empty before.
        #[qproperty(QString, color)]
        /// True when `color` is the real accent of `source`.
        #[qproperty(bool, ready)]
        type Accent = super::AccentRust;
    }

    impl cxx_qt::Threading for Accent {}
    impl cxx_qt::Initialize for Accent {}
}

#[derive(Default)]
pub struct AccentRust {
    source: QString,
    color: QString,
    ready: bool,
}

impl cxx_qt::Initialize for qobject::Accent {
    fn initialize(mut self: Pin<&mut Self>) {
        self.as_mut().on_source_changed(|this| this.refresh()).release();
    }
}

impl qobject::Accent {
    fn reset(mut self: Pin<&mut Self>) {
        if *self.ready() {
            self.as_mut().set_ready(false);
        }
        if !self.color().is_empty() {
            self.as_mut().set_color(QString::default());
        }
    }

    /// `source` changed: answer from the cache or compute in the background.
    fn refresh(mut self: Pin<&mut Self>) {
        let source = self.source().to_string();
        if source.is_empty() {
            return self.reset();
        }
        if let Some(hex) = cached(&source) {
            self.as_mut().set_color(QString::from(&hex));
            self.as_mut().set_ready(true);
            return;
        }
        self.as_mut().reset();
        let th = self.qt_thread();
        crate::state::st().rt.spawn(async move {
            let hex = compute_for_source(&source).await;
            let _ = th.queue(move |mut a| {
                // the source may have changed again while we were busy
                if a.source().to_string() != source {
                    return;
                }
                match hex {
                    Some(h) => {
                        a.as_mut().set_color(QString::from(&h));
                        a.as_mut().set_ready(true);
                    }
                    None => a.reset(),
                }
            });
        });
    }
}

// ---- cache + pipeline --------------------------------------------------------------------------

static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
const CACHE_MAX: usize = 512;

fn cached(source: &str) -> Option<String> {
    CACHE.lock().unwrap().as_ref()?.get(source).cloned()
}

fn remember(source: &str, hex: &str) {
    let mut g = CACHE.lock().unwrap();
    let m = g.get_or_insert_default();
    if m.len() >= CACHE_MAX {
        m.clear();
    }
    m.insert(source.to_owned(), hex.to_owned());
}

async fn compute_for_source(source: &str) -> Option<String> {
    let bytes = crate::covercache::load(source).await?;
    let hex = tokio::task::spawn_blocking(move || accent_from_bytes(&bytes)).await.ok().flatten()?;
    remember(source, &hex);
    Some(hex)
}

/// Encoded image bytes → `#rrggbb`.
pub fn accent_from_bytes(bytes: &[u8]) -> Option<String> {
    let img = crate::covercache::decode_image(bytes).ok()?;
    let small = img.thumbnail(32, 32).to_rgb8();
    let pixels: Vec<[u8; 3]> = small.pixels().map(|p| p.0).collect();
    Some(to_hex(dominant(&pixels)))
}

// ---- colour maths (pure) -----------------------------------------------------------------------

/// Pixels with HSV saturation below this are "grey" and ignored when anything colourful exists.
const MIN_SATURATION: f32 = 0.22;
/// Darker than this is "black", ignored.
const MIN_VALUE: f32 = 0.20;
/// Lightness (HSL) band the result is clamped to so white / dark text stays readable on it.
pub const LIGHTNESS_MIN: f32 = 0.38;
pub const LIGHTNESS_MAX: f32 = 0.62;
const HUE_BINS: usize = 36;

fn rgb_to_hsv([r, g, b]: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

fn rgb_to_hsl([r, g, b]: [u8; 3]) -> (f32, f32, f32) {
    let (rf, gf, bf) = (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let l = (max + min) / 2.0;
    let d = max - min;
    let s = if d == 0.0 { 0.0 } else { d / (1.0 - (2.0 * l - 1.0).abs()) };
    (rgb_to_hsv([r, g, b]).0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let q = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [q(r), q(g), q(b)]
}

/// Move the colour's lightness into [`LIGHTNESS_MIN`, `LIGHTNESS_MAX`] keeping hue and saturation.
pub fn clamp_lightness(rgb: [u8; 3]) -> [u8; 3] {
    let (h, s, l) = rgb_to_hsl(rgb);
    let nl = l.clamp(LIGHTNESS_MIN, LIGHTNESS_MAX);
    if (nl - l).abs() < f32::EPSILON {
        rgb
    } else {
        hsl_to_rgb(h, s, nl)
    }
}

/// A pleasant, saturated dominant colour of `pixels`; the plain average when there is no colour at all
/// (greyscale art); mid-grey for an empty slice. Always lightness-clamped.
pub fn dominant(pixels: &[[u8; 3]]) -> (u8, u8, u8) {
    if pixels.is_empty() {
        let [r, g, b] = clamp_lightness([128, 128, 128]);
        return (r, g, b);
    }
    // weighted histogram over hue
    let mut weight = [0f32; HUE_BINS];
    let mut sum = [[0f32; 3]; HUE_BINS];
    for &p in pixels {
        let (h, s, v) = rgb_to_hsv(p);
        if s < MIN_SATURATION || v < MIN_VALUE {
            continue;
        }
        let w = s * s * v.max(0.3); // vivid beats dull, bright beats dim
        let bin = ((h / 360.0 * HUE_BINS as f32) as usize).min(HUE_BINS - 1);
        weight[bin] += w;
        for (acc, c) in sum[bin].iter_mut().zip(p) {
            *acc += w * f32::from(c);
        }
    }
    // smooth over neighbouring bins so a hue split across a bin edge still wins
    let smoothed = |i: usize| weight[(i + HUE_BINS - 1) % HUE_BINS] * 0.5 + weight[i] + weight[(i + 1) % HUE_BINS] * 0.5;
    let best = (0..HUE_BINS).max_by(|&a, &b| smoothed(a).total_cmp(&smoothed(b)).then(b.cmp(&a)));
    let rgb = match best {
        Some(b) if weight[b] > 0.0 => {
            // average the winning bin only (not the neighbours: keeps the colour pure)
            let w = weight[b];
            [(sum[b][0] / w).round() as u8, (sum[b][1] / w).round() as u8, (sum[b][2] / w).round() as u8]
        }
        _ => {
            let n = pixels.len() as u64;
            let avg = |i: usize| (pixels.iter().map(|p| u64::from(p[i])).sum::<u64>() / n) as u8;
            [avg(0), avg(1), avg(2)]
        }
    };
    let [r, g, b] = clamp_lightness(rgb);
    (r, g, b)
}

pub fn to_hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Parse `#rrggbb` into 0–255 channels. Shared by the colour code and its tests.
#[allow(dead_code)]
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((c(0)?, c(2)?, c(4)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(n: usize, c: [u8; 3]) -> Vec<[u8; 3]> {
        vec![c; n]
    }
    fn hue_of((r, g, b): (u8, u8, u8)) -> f32 {
        rgb_to_hsv([r, g, b]).0
    }
    fn lightness_of((r, g, b): (u8, u8, u8)) -> f32 {
        rgb_to_hsl([r, g, b]).2
    }
    fn near_hue(c: (u8, u8, u8), target: f32) -> bool {
        let d = (hue_of(c) - target).abs();
        d.min(360.0 - d) < 12.0
    }

    #[test]
    fn hex_colours_parse() {
        assert_eq!(parse_hex("#a1b2c3"), Some((0xa1, 0xb2, 0xc3)));
        assert_eq!(parse_hex("a1b2c3"), None);
        assert_eq!(parse_hex("#abc"), None);
        assert_eq!(parse_hex("#zzzzzz"), None);
        assert_eq!(parse_hex("#ééé"), None);
        assert_eq!(parse_hex(&to_hex((1, 2, 255))), Some((1, 2, 255)));
    }

    #[test]
    fn a_red_image_gives_red_and_blue_gives_blue() {
        let red = dominant(&fill(100, [220, 20, 20]));
        assert!(near_hue(red, 0.0), "{red:?}");
        let blue = dominant(&fill(100, [20, 40, 220]));
        assert!(near_hue(blue, 230.0), "{blue:?}");
        let green = dominant(&fill(100, [30, 200, 60]));
        assert!(near_hue(green, 130.0), "{green:?}");
    }

    #[test]
    fn grey_black_and_white_do_not_beat_a_small_colourful_area() {
        let mut px = fill(700, [128, 128, 128]);
        px.extend(fill(200, [0, 0, 0]));
        px.extend(fill(90, [255, 255, 255]));
        px.extend(fill(10, [210, 30, 40])); // only 1 % of the pixels
        assert!(near_hue(dominant(&px), 355.0), "{:?}", dominant(&px));
    }

    #[test]
    fn the_larger_vivid_area_wins_a_gradient() {
        // 60 % blue, 40 % red
        let mut px = fill(60, [20, 30, 230]);
        px.extend(fill(40, [230, 20, 20]));
        assert!(near_hue(dominant(&px), 235.0));
        // a linear red→blue ramp lands on a hue inside the ramp's ends, not on grey
        let ramp: Vec<[u8; 3]> = (0..=255u16).map(|i| [(255 - i) as u8, 30, i as u8]).collect();
        let c = dominant(&ramp);
        assert!(rgb_to_hsv([c.0, c.1, c.2]).1 > 0.3, "stays saturated: {c:?}");
    }

    #[test]
    fn a_vivid_minority_beats_a_dull_majority() {
        let mut px = fill(80, [120, 100, 100]); // dull pinkish grey (s ≈ 0.17, ignored)
        px.extend(fill(20, [20, 190, 60]));
        assert!(near_hue(dominant(&px), 135.0));
    }

    #[test]
    fn pure_greyscale_falls_back_to_the_average() {
        let px = [fill(50, [100, 100, 100]), fill(50, [140, 140, 140])].concat();
        let (r, g, b) = dominant(&px);
        assert!(r == g && g == b, "still grey: {r},{g},{b}");
        assert!((i32::from(r) - 120).abs() <= 25, "near the average, got {r}");
    }

    #[test]
    fn white_and_black_art_are_clamped_to_readable_lightness() {
        for c in [[255, 255, 255], [0, 0, 0], [250, 250, 250], [3, 3, 3]] {
            let d = dominant(&fill(64, c));
            let l = lightness_of(d);
            assert!((LIGHTNESS_MIN - 0.01..=LIGHTNESS_MAX + 0.01).contains(&l), "{c:?} → {d:?} l={l}");
        }
        assert_eq!(dominant(&[]), dominant(&fill(4, [128, 128, 128])), "empty = neutral grey");
    }

    #[test]
    fn very_light_and_very_dark_colours_keep_their_hue() {
        let light = dominant(&fill(64, [255, 200, 200])); // pale pink: s too low? it is light red
        let dark = dominant(&fill(64, [60, 0, 0]));
        for c in [light, dark] {
            let l = lightness_of(c);
            assert!((LIGHTNESS_MIN - 0.02..=LIGHTNESS_MAX + 0.02).contains(&l), "{c:?} l={l}");
        }
        assert!(near_hue(dark, 0.0), "{dark:?}");
        assert!(near_hue(light, 0.0), "{light:?}");
    }

    #[test]
    fn hue_wrap_around_red_is_one_bucket() {
        // reds just below and just above 0° / 360° together beat a single green bucket
        let mut px = fill(30, [230, 20, 30]); // ~356°
        px.extend(fill(30, [230, 30, 10])); // ~4°
        px.extend(fill(45, [20, 220, 40]));
        assert!(near_hue(dominant(&px), 0.0), "{:?}", dominant(&px));
    }

    #[test]
    fn decoding_a_real_image_end_to_end() {
        use image::{ImageFormat, Rgb, RgbImage};
        let mut img = RgbImage::from_pixel(120, 120, Rgb([20, 200, 80]));
        for x in 0..120 {
            for y in 0..20 {
                img.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img).write_to(&mut out, ImageFormat::Png).unwrap();
        let hex = accent_from_bytes(out.get_ref()).unwrap();
        assert!(near_hue(parse_hex(&hex).unwrap(), 140.0), "{hex}");
        assert_eq!(accent_from_bytes(b"not an image"), None);
    }

    #[test]
    fn results_are_cached_per_url() {
        remember("https://x.example/a.jpg", "#112233");
        assert_eq!(cached("https://x.example/a.jpg").as_deref(), Some("#112233"));
        assert_eq!(cached("https://x.example/b.jpg"), None);
    }
}
