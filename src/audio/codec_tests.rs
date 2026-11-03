// SPDX-License-Identifier: GPL-3.0-only
//! Decoder tests on real fixtures (WebM/Opus, MP4/AAC) by tone detection and sample counts.
use super::analysis::{channel, tone_fit};
use super::decode::*;
use super::source::{open, Container, MemorySource};

/// ffmpeg's `sine` is 1/8 full scale; upmixing mono to stereo (`-ac 2`) scales it by 1/sqrt(2).
const AMP: f64 = 0.125 / std::f64::consts::SQRT_2;

pub fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/audio/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn decode_fixture(name: &str) -> (TrackInfo, Vec<f32>) {
    decode_all(open(&fixture(name)).unwrap()).unwrap()
}

/// Tone-detection check on the middle of a decoded clip (skips codec edge effects).
fn assert_tone(name: &str, freq: f64, amp: f64, min_snr: f64) {
    let (info, pcm) = decode_fixture(name);
    let ch0 = channel(&pcm, info.channels, 0);
    let rate = f64::from(info.sample_rate);
    let mid = &ch0[(rate * 0.2) as usize..ch0.len() - (rate * 0.2) as usize];
    let (a, snr) = tone_fit(mid, rate, freq);
    println!("{name}: {} Hz x{} ch, {} frames, amp {a:.4}, snr {snr:.1} dB", info.sample_rate, info.channels, ch0.len());
    assert!((a - amp).abs() / amp < 0.06, "{name}: amplitude {a} vs {amp}");
    assert!(snr > min_snr, "{name}: SNR {snr:.1} dB < {min_snr}");
}

#[test]
fn opus_webm_decodes_to_a_clean_440_hz_tone() {
    assert_tone("sine440_opus.webm", 440.0, AMP, 35.0);
    let (info, pcm) = decode_fixture("sine440_opus.webm");
    assert_eq!((info.codec, info.container, info.sample_rate, info.channels), (CodecKind::Opus, Container::WebM, 48_000, 2));
    let frames = pcm.len() / 2;
    assert!((frames as i64 - 96_000).abs() <= 1920, "expected ~2 s of audio, got {frames} frames");
    assert!(info.duration_ms.is_some_and(|d| (1900..=2100).contains(&d)), "{:?}", info.duration_ms);
}

#[test]
fn aac_mp4_decodes_to_a_clean_440_hz_tone() {
    assert_tone("sine440_aac.m4a", 440.0, AMP, 30.0);
    let (info, pcm) = decode_fixture("sine440_aac.m4a");
    assert_eq!((info.codec, info.container, info.sample_rate, info.channels), (CodecKind::Aac, Container::Mp4, 44_100, 2));
    let frames = pcm.len() / 2;
    assert!((frames as i64 - 88_200).abs() <= 4096, "expected ~2 s of audio, got {frames} frames");
}

#[test]
fn opus_and_aac_agree_on_the_tone() {
    let (io, po) = decode_fixture("sine440_opus.webm");
    let (ia, pa) = decode_fixture("sine440_aac.m4a");
    let o = channel(&po, io.channels, 0);
    let a = channel(&pa, ia.channels, 0);
    let (ao, _) = tone_fit(&o[9600..o.len() - 9600], 48_000.0, 440.0);
    let (aa, _) = tone_fit(&a[8820..a.len() - 8820], 44_100.0, 440.0);
    assert!((ao - aa).abs() / ao < 0.08, "opus {ao} vs aac {aa}");
}

#[test]
fn mono_opus_chirp_decodes_without_trouble() {
    let (info, pcm) = decode_fixture("chirp_opus.webm");
    assert_eq!(info.channels, 1);
    assert!((pcm.len() as i64 - 288_000).abs() <= 1920, "{}", pcm.len());
    // the 3 kHz companion tone is steady for the whole clip
    let mid = &pcm[24_000..pcm.len() - 24_000];
    let (a, _) = tone_fit(mid, 48_000.0, 3000.0);
    assert!((a - 0.1).abs() < 0.015, "3 kHz amplitude {a}");
}

#[test]
fn wrong_container_truncated_and_empty_streams_are_errors_not_panics() {
    let e = TrackDecoder::open(Box::new(MemorySource::new(b"RIFF....WAVEfmt this is not webm or mp4".to_vec()))).err().unwrap();
    assert!(e.contains("unsupported container"), "{e}");
    assert!(TrackDecoder::open(Box::new(MemorySource::new(vec![]))).is_err());
    assert!(TrackDecoder::open(Box::new(MemorySource::new(vec![0x1A, 0x45, 0xDF, 0xA3, 1, 0, 0, 0, 0, 0, 0, 0, 0]))).is_err());
    let full = std::fs::read(fixture("sine440_opus.webm")).unwrap();
    for cut in [30usize, 200] {
        let r = decode_all(Box::new(MemorySource::new(full[..cut].to_vec())));
        assert!(r.is_err(), "cut at {cut} must be an error");
    }
    let half = decode_all(Box::new(MemorySource::new(full[..full.len() / 2].to_vec())));
    assert!(half.is_err(), "a stream cut in half must be reported, not silently accepted as complete");
}

#[test]
fn seek_lands_within_one_packet_of_the_request() {
    let (_, full) = decode_fixture("sine440_opus.webm");
    let mut blk = Vec::new();
    let mut d = TrackDecoder::open(open(&fixture("sine440_opus.webm")).unwrap()).unwrap();
    let at = d.seek_ms(1000).unwrap();
    assert_eq!(at, 1000);
    let mut rest = Vec::new();
    while d.next_block(&mut blk).unwrap() {
        rest.extend_from_slice(&blk);
    }
    let total = full.len() / 2;
    let expected = total - 48_000;
    let got = rest.len() / 2;
    assert!((got as i64 - expected as i64).abs() <= 960, "after seek {got} frames, expected {expected} (+-20 ms)");
    let ch = channel(&rest, 2, 0);
    let (a, snr) = tone_fit(&ch[2400..ch.len() - 4800], 48_000.0, 440.0);
    assert!((a - AMP).abs() < 0.01 && snr > 30.0, "tone intact after a seek: amp {a} snr {snr}");
}

#[test]
fn seeking_past_the_last_cue_point_decodes_forward_from_an_earlier_one() {
    let (_, full) = decode_fixture("chirp_opus.webm");
    let total = full.len();
    for ms in [5500u64, 5700, 5990] {
        let mut d = TrackDecoder::open(open(&fixture("chirp_opus.webm")).unwrap()).unwrap();
        assert_eq!(d.seek_ms(ms).unwrap(), ms);
        let mut blk = Vec::new();
        let mut rest = 0;
        while d.next_block(&mut blk).unwrap() {
            rest += blk.len();
        }
        let expected = total - (ms as usize * 48);
        assert!((rest as i64 - expected as i64).abs() <= 1920, "seek {ms}: {rest} samples vs {expected}");
    }
}

#[test]
fn seek_works_for_aac_too() {
    let mut d = TrackDecoder::open(open(&fixture("sine440_aac.m4a")).unwrap()).unwrap();
    d.seek_ms(1000).unwrap();
    let mut blk = Vec::new();
    let mut rest = Vec::new();
    while d.next_block(&mut blk).unwrap() {
        rest.extend_from_slice(&blk);
    }
    let got = rest.len() / 2;
    assert!((got as i64 - 44_100).abs() <= 2048 + 1024, "{got}");
    let ch = channel(&rest, 2, 0);
    let (a, snr) = tone_fit(&ch[2205..ch.len() - 4410], 44_100.0, 440.0);
    assert!((a - AMP).abs() < 0.01 && snr > 25.0, "amp {a} snr {snr}");
}

/// Dev-only: CPU decode speed in multiples of real time (use `--release`; a debug build is ~10x slower).
/// `cargo test --release -- --ignored --nocapture dev_decode_speed`
#[test]
#[ignore]
fn dev_decode_speed() {
    for name in ["chirp_opus.webm", "sine440_opus.webm", "sine440_aac.m4a"] {
        let bytes = std::fs::read(fixture(name)).unwrap();
        let (mut audio_secs, t0) = (0.0, std::time::Instant::now());
        for _ in 0..40 {
            let (info, pcm) = decode_all(Box::new(MemorySource::new(bytes.clone()))).unwrap();
            audio_secs += (pcm.len() / info.channels) as f64 / f64::from(info.sample_rate);
        }
        let el = t0.elapsed().as_secs_f64();
        println!("{name}: {audio_secs:.0} s of audio in {el:.2} s = {:.0}x real time", audio_secs / el);
    }
}

/// Dev-only: compare with libopus output produced by the dev ffmpeg beforehand (never shipped, never run here by CI).
/// Generate the references with
/// `ffmpeg -i tests/fixtures/audio/sine440_opus.webm -f f32le -ac 2 -ar 48000 $YTM_FFREF/sine440_opus.webm.f32`
/// (and `-ac 1` for chirp_opus.webm), then `YTM_FFREF=<dir> cargo test -- --ignored dev_compare`.
#[test]
#[ignore]
fn dev_compare_with_ffmpeg_f32le() {
    let Some(dir) = std::env::var_os("YTM_FFREF") else { panic!("set YTM_FFREF to the directory with the .f32 references") };
    for name in ["sine440_opus.webm", "chirp_opus.webm"] {
        let raw = std::fs::read(std::path::Path::new(&dir).join(format!("{name}.f32"))).unwrap();
        let reff: Vec<f32> = raw.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        let (_, ours) = decode_fixture(name);
        let n = reff.len().min(ours.len());
        let (mut sig, mut err, mut maxd) = (0f64, 0f64, 0f32);
        for i in 0..n {
            let (a, b) = (f64::from(reff[i]), f64::from(ours[i]));
            sig += a * a;
            err += (a - b) * (a - b);
            maxd = maxd.max((reff[i] - ours[i]).abs());
        }
        let snr = 10.0 * (sig / err.max(1e-30)).log10();
        println!("{name}: ffmpeg {} vs ours {} samples, SNR vs libopus {snr:.1} dB, max diff {maxd:.2e}", reff.len(), ours.len());
        assert!(snr > 60.0 || maxd < 1e-3, "{name}: SNR {snr}, max diff {maxd}");
    }
}
