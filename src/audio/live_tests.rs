// SPDX-License-Identifier: GPL-3.0-only
//! Live checks against YouTube (`cargo test -- --ignored --nocapture live_audio`). They need network access and must be
//! run with temporary XDG_* directories. Nothing is played: the real stream goes through the app's own stream proxy
//! and the engine's range reader into the decoder, in memory.
use super::analysis::rms;
use super::track::TrackStream;
use crate::vision::{self, Codec, Quality};
use std::time::Instant;

/// Resolve the best stream of `codec` and register it with the app's proxy: the URL the engine is given in the app.
async fn proxied(video_id: &str, codec: Codec) -> Result<(String, u32), String> {
    let formats = vision::audio_formats(video_id).await?;
    let f = vision::pick_format(&formats, Quality::Best, Some(codec)).ok_or("no such format")?;
    println!("{video_id}: itag {} {} {} bps, loudnessDb {:?}", f.itag, f.mime, f.bitrate, f.loudness_db);
    let (url, itag) = (f.url.clone(), f.itag);
    let id = video_id.to_owned();
    // the proxy binds a socket and spawns on the app runtime: do that off the async test thread
    let local =
        tokio::task::spawn_blocking(move || crate::proxy::local_url(&id, &url, vision::user_agent())).await.map_err(|e| e.to_string())?;
    Ok((local, itag))
}

/// Play `secs` seconds of audio through the stream, then (optionally) seek and play a few more. Returns x real time.
fn run(url: &str, seek_to_ms: Option<u64>) -> Result<f64, String> {
    let t0 = Instant::now();
    let mut t = TrackStream::open_url(url, 48_000)?;
    println!("  opened in {:?} ({:?}, duration {:?} ms)", t0.elapsed(), t.codec, t.duration_ms);
    let take = |t: &mut TrackStream, secs: f64| -> Result<(f64, f64), String> {
        let mut out = Vec::new();
        let want = (secs * 48_000.0) as usize;
        let t0 = Instant::now();
        while out.len() / 2 < want {
            t.fill(4096);
            if t.take(4096, 1.0, &mut out) == 0 {
                if let Some(e) = t.take_error() {
                    return Err(e);
                }
                if t.finished() {
                    break;
                }
            }
        }
        let audio = out.len() as f64 / 2.0 / 48_000.0;
        println!("  {audio:.1} s decoded, rms {:.3}, {:.0}x real time", rms(&out), audio / t0.elapsed().as_secs_f64());
        assert!(rms(&out) > 0.005, "silence");
        assert!(out.iter().all(|s| s.is_finite() && s.abs() < 4.0));
        Ok((audio, audio / t0.elapsed().as_secs_f64()))
    };
    let (audio, x) = take(&mut t, 40.0)?;
    assert!(audio > 39.0);
    if let Some(ms) = seek_to_ms {
        let s0 = Instant::now();
        t.seek(ms)?;
        t.fill(4096);
        println!("  seek to {ms} ms: first audio after {:?}", s0.elapsed());
        let (a2, _) = take(&mut t, 10.0)?;
        assert!(a2 > 9.0);
        assert!((t.position_ms() - (ms as f64 + a2 * 1000.0)).abs() < 100.0, "position {} after seek to {ms}", t.position_ms());
    }
    Ok(x)
}

#[tokio::test]
#[ignore]
async fn live_audio_itag251_opus_through_the_proxy() {
    let (url, itag) = proxied("dQw4w9WgXcQ", Codec::Opus).await.expect("resolve");
    assert_eq!(itag, 251, "expected the 160 kbps Opus stream");
    let x = tokio::task::spawn_blocking(move || run(&url, Some(100_000))).await.unwrap().expect("play");
    assert!(x > 1.0);
}

#[tokio::test]
#[ignore]
async fn live_audio_aac_through_the_proxy() {
    let (url, itag) = proxied("dQw4w9WgXcQ", Codec::Aac).await.expect("resolve");
    println!("AAC itag {itag}");
    let x = tokio::task::spawn_blocking(move || run(&url, Some(100_000))).await.unwrap().expect("play");
    assert!(x > 1.0);
}
