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
        Mode::Truncate(n) => &slice[..n.min(slice.len())],
    };
    for piece in send.chunks(4096) {
        s.write_all(piece)?;
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

#[test]
fn shutting_down_while_loading_does_not_hang() {
    let base = serve(Mode::Normal);
    let (e, _rx) = engine();
    e.load(&format!("{base}/ok/sine440_opus.webm"), 0);
    drop(e);
}
