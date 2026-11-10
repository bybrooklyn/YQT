// SPDX-License-Identifier: GPL-3.0-only
//! Threaded-engine tests against a tiny local HTTP server that behaves like the stream proxy (Range, 206, 404).
//! Everything plays into the null sink at volume 0.
use super::codec_tests::fixture;
use super::engine::{Engine, Settings};
use super::output::{Backend, Event};
use super::source::HttpRangeSource;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    /// Send only this many bytes of the body, then hang up (headers still promise the whole file).
    Truncate(usize),
    /// Send only this many bytes of each response, then keep the connection open without sending anything more.
    Stall(usize),
}

/// Serves `/ok/<name>` from the fixtures dir; anything else is a 404.
fn serve(mode: Mode) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let _ = handle(s, mode);
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn handle(mut s: TcpStream, mode: Mode) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read(&mut b)? == 0 {
            return Ok(());
        }
        head.push(b[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let path = head.split_whitespace().nth(1).unwrap_or("");
    let range = head.lines().find_map(|l| l.strip_prefix("Range: bytes=").or_else(|| l.strip_prefix("range: bytes=")));
    let Some(name) = path.strip_prefix("/ok/") else {
        return s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    };
    let Ok(body) = std::fs::read(fixture(name)) else {
        return s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    };
    let total = body.len();
    let start: usize = range.and_then(|r| r.split('-').next()).and_then(|a| a.trim().parse().ok()).unwrap_or(0);
    if start >= total {
        return s.write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
    let slice = &body[start..];
    write!(
        s,
        "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {start}-{}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        total - 1,
        slice.len()
    )?;
    let send = match mode {
        Mode::Normal => slice,
        Mode::Truncate(n) | Mode::Stall(n) => &slice[..n.min(slice.len())],
    };
    for piece in send.chunks(4096) {
        s.write_all(piece)?;
    }
    if matches!(mode, Mode::Stall(_)) && send.len() < slice.len() {
        s.flush()?;
        std::thread::sleep(Duration::from_secs(120)); // the upstream is alive but sends nothing
    }
    Ok(())
}

fn engine() -> (Engine, Receiver<Event>) {
    let (e, rx) = Engine::start(&Backend::Null { rate: 48_000 }, Arc::new(Settings::new(0, true, false))).unwrap();
    e.set_volume(0.0); // belt and braces: the null sink makes no sound anyway
    (e, rx)
}

/// Wait for an event matching `f`, up to `secs`; returns every event seen on the way.
fn wait_for(rx: &Receiver<Event>, secs: u64, f: impl Fn(&Event) -> bool) -> Result<Vec<Event>, Vec<Event>> {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut seen = Vec::new();
    while Instant::now() < end {
        if let Ok(e) = rx.recv_timeout(Duration::from_millis(50)) {
            let hit = f(&e);
            seen.push(e);
            if hit {
                return Ok(seen);
            }
        }
    }
    Err(seen)
}

#[test]
fn http_range_source_reads_seeks_and_reports_length() {
    let base = serve(Mode::Normal);
    let file = std::fs::read(fixture("sine440_opus.webm")).unwrap();
    let mut s = HttpRangeSource::open(&format!("{base}/ok/sine440_opus.webm")).unwrap();
    assert_eq!(s.len() as usize, file.len());
    let mut buf = vec![0u8; 1000];
    s.read_exact(&mut buf).unwrap();
    assert_eq!(buf, file[..1000]);
    // small forward skip keeps the connection; big jump and backward seek reopen it
    s.seek(SeekFrom::Start(1500)).unwrap();
    s.read_exact(&mut buf).unwrap();
    assert_eq!(buf, file[1500..2500]);
    s.seek(SeekFrom::Start(20_000)).unwrap();
    s.read_exact(&mut buf).unwrap();
    assert_eq!(buf, file[20_000..21_000]);
    s.seek(SeekFrom::Start(100)).unwrap();
    s.read_exact(&mut buf).unwrap();
    assert_eq!(buf, file[100..1100]);
    s.seek(SeekFrom::End(-10)).unwrap();
    let mut tail = Vec::new();
    s.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, file[file.len() - 10..]);
    assert!(HttpRangeSource::open(&format!("{base}/nope")).err().unwrap().contains("404"));
}

#[test]
fn plays_a_track_over_http_to_its_end_with_a_sample_accurate_clock() {
    let base = serve(Mode::Normal);
    let (e, rx) = engine();
    let url = format!("{base}/ok/sine440_opus.webm");
    e.load(&url, 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).expect("loaded");
    assert!(e.shared.duration_ms().is_some_and(|d| (1900..2100).contains(&d)));
    e.play();
    wait_for(&rx, 5, |ev| matches!(ev, Event::Started(_))).expect("started");
    std::thread::sleep(Duration::from_millis(600));
    let p = e.position_ms();
    assert!((300.0..1100.0).contains(&p), "after ~0.6 s of real time the clock reads {p} ms");
    let seen = wait_for(&rx, 6, |ev| matches!(ev, Event::Ended(_))).expect("ended");
    assert!(!seen.iter().any(|e| matches!(e, Event::Failed(_))));
    let end = e.position_ms();
    assert!((1900.0..2200.0).contains(&end), "ended at {end} ms");
}

#[test]
fn pause_freezes_the_clock_and_seek_jumps() {
    let base = serve(Mode::Normal);
    let (e, rx) = engine();
    e.load(&format!("{base}/ok/chirp_opus.webm"), 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).unwrap();
    e.play();
    std::thread::sleep(Duration::from_millis(300));
    e.pause();
    std::thread::sleep(Duration::from_millis(100));
    let frozen = e.position_ms();
    std::thread::sleep(Duration::from_millis(300));
    assert!((e.position_ms() - frozen).abs() < 1.0, "paused clock moved: {frozen} -> {}", e.position_ms());
    e.seek(4000);
    std::thread::sleep(Duration::from_millis(300));
    let at = e.position_ms();
    assert!((3900.0..4300.0).contains(&at), "after seek(4000) the clock reads {at}");
    e.play();
    std::thread::sleep(Duration::from_millis(300));
    let later = e.position_ms();
    assert!(later > at + 100.0 && later < 4800.0, "{at} -> {later}");
}

#[test]
fn seek_before_the_track_has_opened_is_applied_when_it_does() {
    let base = serve(Mode::Normal);
    let (e, rx) = engine();
    e.load(&format!("{base}/ok/chirp_opus.webm"), 2500);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let p = e.position_ms();
    assert!((2400.0..2700.0).contains(&p), "{p}");
}

#[test]
fn gapless_switch_in_the_engine_and_the_ui_catching_up_does_not_restart() {
    let base = serve(Mode::Normal);
    let (e, rx) = engine();
    let (a, b) = (format!("{base}/ok/sine440_opus.webm"), format!("{base}/ok/sine880_opus.webm"));
    e.load(&a, 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).unwrap();
    e.set_next(Some(&b));
    std::thread::sleep(Duration::from_millis(400)); // let the next track open
    e.play();
    let seen = wait_for(&rx, 8, |ev| matches!(ev, Event::Started(m) if m.url.ends_with("sine880_opus.webm"))).expect("switch to B");
    let ended_a = seen.iter().position(|ev| matches!(ev, Event::Ended(m) if m.url.ends_with("sine440_opus.webm")));
    assert!(ended_a.is_some(), "A reported ended before B started: {seen:?}");
    // the player reacts to ended() by loading B: the engine must recognise it is already playing it
    e.load(&b, 0);
    std::thread::sleep(Duration::from_millis(300));
    let reload = rx.try_iter().any(|ev| matches!(ev, Event::Loaded(_)));
    assert!(!reload, "load() of the track already playing must not reload it");
    assert!(e.position_ms() > 100.0, "B keeps playing from where it was: {}", e.position_ms());
    wait_for(&rx, 6, |ev| matches!(ev, Event::Ended(m) if m.url.ends_with("sine880_opus.webm"))).expect("B ends");
}

#[test]
fn failures_become_failed_events_not_panics() {
    let base = serve(Mode::Normal);
    let (e, rx) = engine();
    // 404 from the proxy
    e.load(&format!("{base}/missing"), 0);
    let seen = wait_for(&rx, 5, |ev| matches!(ev, Event::Failed(_))).expect("404 -> failed");
    assert!(matches!(seen.last(), Some(Event::Failed(m)) if m.contains("404")), "{seen:?}");
    assert!(!e.shared.loading.load(std::sync::atomic::Ordering::Relaxed));
    // nothing listening
    e.load("http://127.0.0.1:1/t/x/y", 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Failed(_))).expect("connection refused -> failed");
    // wrong container (the server hands out a file that is neither WebM nor MP4)
    e.load(&format!("{base}/ok/not_audio.bin"), 0);
    let seen = wait_for(&rx, 5, |ev| matches!(ev, Event::Failed(_))).expect("not audio -> failed");
    assert!(matches!(seen.last(), Some(Event::Failed(m)) if m.contains("unsupported container")), "{seen:?}");
    // and the engine is still usable afterwards
    e.load(&format!("{base}/ok/sine440_opus.webm"), 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).expect("recovers");
}

#[test]
fn a_truncated_stream_fails_cleanly() {
    let base = serve(Mode::Truncate(20_000)); // fixture is ~39 KB
    let (e, rx) = engine();
    e.load(&format!("{base}/ok/sine440_opus.webm"), 0);
    e.play();
    let seen = wait_for(&rx, 8, |ev| matches!(ev, Event::Failed(_))).expect("truncated -> failed");
    assert!(matches!(seen.last(), Some(Event::Failed(m)) if !m.is_empty()), "{seen:?}");
}

#[test]
fn seeking_after_the_track_ended_replays_it_like_repeat_one() {
    let base = serve(Mode::Normal);
    let (e, rx) = engine();
    e.load(&format!("{base}/ok/chirp_opus.webm"), 0);
    e.play();
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).unwrap();
    e.seek(5700); // near the end of the 6 s clip
    wait_for(&rx, 6, |ev| matches!(ev, Event::Ended(_))).expect("ended");
    e.seek(0);
    e.play();
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).expect("restarted from the top");
    std::thread::sleep(Duration::from_millis(400));
    let p = e.position_ms();
    assert!((100.0..900.0).contains(&p), "playing again from the start: {p}");
    // after an explicit stop, seeking must not resurrect the old track
    e.stop();
    std::thread::sleep(Duration::from_millis(100));
    e.seek(0);
    std::thread::sleep(Duration::from_millis(300));
    assert!(rx.try_iter().all(|ev| !matches!(ev, Event::Loaded(_))));
}

/// Dev check of the real device path (cpal): the stream opens and its callback pulls audio. Volume is set to 0 and the
/// engine is only told to play after the volume ramp finished, so nothing audible is ever produced.
/// `cargo test -- --ignored --nocapture dev_real_device`
/// Set `YTM_DEV_DEVICE=null` to use ALSA's silent `null` device (it discards the samples) instead of the default one.
#[test]
#[ignore]
fn dev_real_device_opens_and_consumes_at_volume_zero() {
    println!("output devices: {:?}", super::output::output_devices());
    let name = std::env::var("YTM_DEV_DEVICE").unwrap_or_default();
    let (e, rx) = Engine::start(&Backend::Device { name }, Arc::new(Settings::new(0, true, false))).expect("open the output device");
    e.set_volume(0.0);
    println!("device rate: {} Hz", e.rate());
    let base = serve(Mode::Normal);
    e.load(&format!("{base}/ok/chirp_opus.webm"), 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).unwrap();
    std::thread::sleep(Duration::from_millis(200)); // volume ramp to 0 completes while paused
    e.play();
    std::thread::sleep(Duration::from_millis(1000));
    let p = e.position_ms();
    println!(
        "position after ~1 s on the real device: {p:.0} ms, underrun frames: {}",
        e.underruns.load(std::sync::atomic::Ordering::Relaxed)
    );
    assert!(p > 500.0, "the device callback is not consuming: {p}");
}

#[test]
fn shutting_down_while_loading_does_not_hang() {
    let base = serve(Mode::Normal);
    let (e, _rx) = engine();
    e.load(&format!("{base}/ok/sine440_opus.webm"), 0);
    drop(e);
}

/// Poll until `f` holds (true) or `secs` have passed (false).
fn until(secs: u64, f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// The duration shown is the audible track's: it used to stay the first track's after every gapless switch, and to
/// survive a stop.
#[test]
fn duration_follows_the_audible_track_and_is_reset_by_stop() {
    let base = serve(Mode::Normal);
    let (e, rx) = engine();
    let st = e.status();
    let (a, b) = (format!("{base}/ok/sine440_opus.webm"), format!("{base}/ok/chirp_opus.webm"));
    e.load(&a, 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).unwrap();
    assert!((1900..2100).contains(&st.duration_ms()), "known before anything is heard: {}", st.duration_ms());
    e.set_next(Some(&b));
    std::thread::sleep(Duration::from_millis(400));
    e.play();
    // in the last half second the feeder is already decoding B, but A is what is heard
    assert!(until(5, || e.position_ms() > 1700.0));
    assert!((1900..2100).contains(&st.duration_ms()), "A is still audible: {}", st.duration_ms());
    wait_for(&rx, 5, |ev| matches!(ev, Event::Started(m) if m.url == b)).expect("switch to B");
    assert!((5900..6100).contains(&st.duration_ms()), "B is audible: {}", st.duration_ms());
    e.load(&b, 0); // the UI catching up changes nothing
    std::thread::sleep(Duration::from_millis(200));
    assert!((5900..6100).contains(&st.duration_ms()), "{}", st.duration_ms());
    e.stop();
    assert!(until(2, || st.duration_ms() == 0), "stop forgets the duration: {}", st.duration_ms());
}

/// Seeking while the previous track's tail is still playing (the feeder is already in the next one) used to seek
/// in the next track and swallow `Ended`, leaving the UI on the wrong track.
#[test]
fn a_seek_in_the_tail_of_a_track_stays_in_that_track_and_ended_is_not_lost() {
    let base = serve(Mode::Normal);
    let (e, rx) = engine();
    let (a, b) = (format!("{base}/ok/sine440_opus.webm"), format!("{base}/ok/sine880_opus.webm"));
    e.load(&a, 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).unwrap();
    e.set_next(Some(&b));
    std::thread::sleep(Duration::from_millis(400));
    e.play();
    assert!(until(5, || e.position_ms() > 1750.0), "A's last quarter second");
    e.seek(500);
    // A starts over at the position: nothing ended, B did not start
    let seen = wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(m) if m.url == a)).expect("A is reopened at the seek target");
    assert!(!seen.iter().any(|ev| matches!(ev, Event::Ended(_))), "A has not ended: {seen:?}");
    assert!(!seen.iter().any(|ev| matches!(ev, Event::Started(m) if m.url == b)), "B must not start: {seen:?}");
    std::thread::sleep(Duration::from_millis(300));
    let p = e.position_ms();
    assert!((500.0..1300.0).contains(&p), "playing A on from the seek target: {p}");
    assert!(e.status().playing_meta().is_some_and(|m| m.url == a));
    // and the queue is intact: A ends (once), then B follows gaplessly
    let seen = wait_for(&rx, 6, |ev| matches!(ev, Event::Started(m) if m.url == b)).expect("B follows");
    assert_eq!(seen.iter().filter(|ev| matches!(ev, Event::Ended(m) if m.url == a)).count(), 1, "{seen:?}");
    wait_for(&rx, 6, |ev| matches!(ev, Event::Ended(m) if m.url == b)).expect("B ends");
}

/// `play()` with nothing left to play used to report "playing" over silence for ever.
#[test]
fn play_on_an_idle_engine_restarts_the_last_track_and_never_a_stopped_or_failed_one() {
    let base = serve(Mode::Normal);
    let (e, rx) = engine();
    let a = format!("{base}/ok/sine440_opus.webm");
    e.load(&a, 0);
    e.play();
    wait_for(&rx, 8, |ev| matches!(ev, Event::Ended(_))).expect("ended");
    assert!(until(2, || e.status().idle()));
    e.play();
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(m) if m.url == a)).expect("the last track is loaded again");
    wait_for(&rx, 5, |ev| matches!(ev, Event::Started(_))).expect("and plays");
    std::thread::sleep(Duration::from_millis(300));
    assert!((100.0..900.0).contains(&e.position_ms()), "from the top: {}", e.position_ms());
    // play() while a track is playing or loading changes nothing
    e.play();
    std::thread::sleep(Duration::from_millis(200));
    assert!(rx.try_iter().all(|ev| !matches!(ev, Event::Loaded(_))));
    // after a stop there is nothing to restart
    e.stop();
    std::thread::sleep(Duration::from_millis(100));
    e.play();
    std::thread::sleep(Duration::from_millis(300));
    assert!(rx.try_iter().all(|ev| !matches!(ev, Event::Loaded(_))));
    // nor after a load that failed: the track before it must not come back
    e.load(&a, 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).unwrap();
    e.load(&format!("{base}/nope"), 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Failed(_))).expect("failed");
    e.play();
    std::thread::sleep(Duration::from_millis(300));
    assert!(rx.try_iter().all(|ev| !matches!(ev, Event::Loaded(_))));
}

/// How long it takes until `done` holds, in ms (`None` = not within 3 s).
fn ms_until(done: impl Fn() -> bool) -> Option<u128> {
    let t = Instant::now();
    until(3, done).then(|| t.elapsed().as_millis())
}

/// The position has stopped although the engine is playing: the feeder thread is waiting for the network.
fn stuck(e: &Engine) -> bool {
    let p = e.position_ms();
    std::thread::sleep(Duration::from_millis(250));
    e.position_ms() == p
}

/// An upstream that keeps the connection open but sends nothing blocks the feeder thread in a read. Commands used to
/// wait behind that read (up to the 30 s read timeout); now it is interrupted the moment one is on its way.
#[test]
fn a_stalled_stream_never_keeps_seek_load_stop_or_shutdown_waiting() {
    const PROMPT_MS: u128 = 500; // the bound asserted; in practice it is a few tens of ms
    let (stall, ok) = (serve(Mode::Stall(14_000)), serve(Mode::Normal));
    let (e, rx) = engine();
    let stalled = format!("{stall}/ok/chirp_opus.webm");
    e.load(&stalled, 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).expect("the first bytes are enough to open it");
    e.play();
    assert!(until(5, || stuck(&e)), "the stream runs dry and the feeder waits for more");
    assert!(e.position_ms() < 2000.0, "{}", e.position_ms());

    // seek: taken up at once (the new position shows), although the stream then stalls again at the new place
    e.seek(3000);
    let took = ms_until(|| e.position_ms() >= 3000.0).expect("seek acted on");
    assert!(took < PROMPT_MS, "seek waited {took} ms behind a stalled read");
    assert!(until(5, || stuck(&e)), "stalled again after the seek");
    let p = e.position_ms();
    assert!((3000.0..4500.0).contains(&p), "it played on from the seek target until the bytes ran out: {p}");

    // load: the other track opens and plays
    let other = format!("{ok}/ok/sine440_opus.webm");
    let t = Instant::now();
    e.load(&other, 0);
    wait_for(&rx, 3, |ev| matches!(ev, Event::Loaded(m) if m.url == other)).expect("load acted on");
    let took = t.elapsed().as_millis();
    assert!(took < PROMPT_MS, "load waited {took} ms behind a stalled read");
    wait_for(&rx, 5, |ev| matches!(ev, Event::Ended(m) if m.url == other)).expect("and it plays to its end");

    // stop
    e.load(&stalled, 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).unwrap();
    e.play();
    assert!(until(5, || stuck(&e)));
    e.stop();
    let took = ms_until(|| e.status().idle() && e.position_ms() == 0.0).expect("stop acted on");
    assert!(took < PROMPT_MS, "stop waited {took} ms behind a stalled read");

    // shutdown: dropping the engine joins the feeder thread
    e.load(&stalled, 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).unwrap();
    e.play();
    assert!(until(5, || stuck(&e)));
    let t = Instant::now();
    drop(e);
    let took = t.elapsed().as_millis();
    assert!(took < PROMPT_MS, "shutdown waited {took} ms behind a stalled read");
}

/// The next track is pre-decoded on the feeder thread too: if *it* stalls, commands must not wait either, and the
/// track that is playing is not harmed by the interruption.
#[test]
fn a_stalled_next_track_does_not_keep_commands_waiting() {
    let (stall, ok) = (serve(Mode::Stall(6_000)), serve(Mode::Normal));
    let (e, rx) = engine();
    let (a, next) = (format!("{ok}/ok/chirp_opus.webm"), format!("{stall}/ok/sine880_opus.webm"));
    e.load(&a, 0);
    wait_for(&rx, 5, |ev| matches!(ev, Event::Loaded(_))).unwrap();
    e.play();
    wait_for(&rx, 5, |ev| matches!(ev, Event::Started(_))).unwrap();
    e.set_next(Some(&next));
    // the pre-decode of the next track blocks the feeder: the playing track runs out of queued audio
    assert!(until(5, || stuck(&e)), "the feeder waits in the next track's pre-decode");
    let before = e.position_ms();
    assert!(before < 3000.0, "{before}");
    // a seek in the playing track is taken up at once and that track plays on (its stream is fine)
    e.seek(4000);
    let took = ms_until(|| e.position_ms() >= 4000.0).expect("seek acted on");
    assert!(took < 500, "seek waited {took} ms behind the stalled pre-decode");
    assert!(until(3, || e.position_ms() > 4300.0), "the playing track goes on after the interruption: {}", e.position_ms());
    assert!(rx.try_iter().all(|ev| !matches!(ev, Event::Failed(_))), "an interruption is not a failure");
    e.stop();
    let took = ms_until(|| e.status().idle()).expect("stop acted on");
    assert!(took < 500, "stop waited {took} ms");
}
