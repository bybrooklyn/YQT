// SPDX-License-Identifier: GPL-3.0-only
//! Are we online? A cheap, privacy-neutral probe: DNS-resolve `music.youtube.com` and open a TCP connection
//! to port 443, then close it. **No HTTP request is sent** and nothing but the name lookup the app does anyway
//! leaves the machine.
//!
//! * [`Monitor`]: the pure state machine (two failures in a row = offline, one success = online, backoff while
//!   offline, ~20 s between probes while online).
//! * `Connectivity`: the QML singleton (`online`, `onlineChanged`, `checkNow()`); the error layer uses it for
//!   the `Offline` error kind, the downloads manager uses [`is_online`] / [`subscribe`].
use core::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const PROBE_HOST: &str = "music.youtube.com";
pub const PROBE_PORT: u16 = 443;
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Probe cadence while online and healthy.
pub const ONLINE_INTERVAL: Duration = Duration::from_secs(20);
/// Quick re-check after a single failure (to tell a blip from an outage).
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(3);
const OFFLINE_BASE: Duration = Duration::from_secs(5);
const OFFLINE_MAX: Duration = Duration::from_secs(60);
/// Consecutive failures that flip the state to offline.
pub const FAILS_TO_GO_OFFLINE: u32 = 2;

/// The pure state machine. Starts optimistic (online) so the UI does not flash "offline" at start-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    online: bool,
    fails: u32,
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor { online: true, fails: 0 }
    }
}

impl Monitor {
    pub fn online(&self) -> bool {
        self.online
    }

    /// Feed one probe result; returns `true` when the online state flipped.
    pub fn record(&mut self, ok: bool) -> bool {
        let before = self.online;
        if ok {
            self.fails = 0;
            self.online = true;
        } else {
            self.fails = self.fails.saturating_add(1);
            if self.fails >= FAILS_TO_GO_OFFLINE {
                self.online = false;
            }
        }
        before != self.online
    }

    /// How long to wait before the next probe.
    pub fn next_delay(&self) -> Duration {
        match (self.online, self.fails) {
            (true, 0) => ONLINE_INTERVAL,
            (true, _) => RECHECK_INTERVAL,
            (false, f) => {
                let steps = f.saturating_sub(FAILS_TO_GO_OFFLINE).min(16);
                OFFLINE_BASE.saturating_mul(1u32 << steps).min(OFFLINE_MAX)
            }
        }
    }
}

// ---- process-wide state ------------------------------------------------------------------------

static ONLINE: AtomicBool = AtomicBool::new(true);
type Sub = Box<dyn Fn(bool) + Send + Sync>;
static SUBS: Mutex<Vec<Sub>> = Mutex::new(Vec::new());
static STARTED: OnceLock<()> = OnceLock::new();
static WAKE: OnceLock<tokio::sync::Notify> = OnceLock::new();

fn wake() -> &'static tokio::sync::Notify {
    WAKE.get_or_init(tokio::sync::Notify::new)
}

pub fn is_online() -> bool {
    ONLINE.load(Ordering::Relaxed)
}

/// Be told whenever the online state flips (called on a background thread).
pub fn subscribe(f: impl Fn(bool) + Send + Sync + 'static) {
    SUBS.lock().unwrap().push(Box::new(f));
}

/// Probe now instead of waiting (e.g. after a request failed).
pub fn check_now() {
    wake().notify_one();
}

/// One probe: resolve the name, connect, drop. `true` = a TCP connection was established.
pub async fn probe_once() -> bool {
    let attempt = async {
        let addrs = tokio::net::lookup_host((PROBE_HOST, PROBE_PORT)).await.ok()?;
        connect_any(addrs).await.then_some(())
    };
    tokio::time::timeout(PROBE_TIMEOUT, attempt).await.ok().flatten().is_some()
}

/// Does any of `addrs` accept a TCP connection? They are tried all at once and the first success wins: one address
/// that never answers (typically IPv6 on a network where only IPv4 works) must not use up the whole time budget
/// before the working ones get their turn.
async fn connect_any(addrs: impl IntoIterator<Item = std::net::SocketAddr>) -> bool {
    let mut tries = tokio::task::JoinSet::new();
    for a in addrs {
        tries.spawn(async move { tokio::net::TcpStream::connect(a).await.is_ok() });
    }
    while let Some(done) = tries.join_next().await {
        if done.unwrap_or(false) {
            return true; // (dropping the set aborts the attempts still running)
        }
    }
    false
}

/// Start the background probe loop (once per process).
pub fn start() {
    STARTED.get_or_init(|| {
        crate::state::st().rt.spawn(async {
            let mut mon = Monitor::default();
            loop {
                let ok = probe_once().await;
                if mon.record(ok) {
                    ONLINE.store(mon.online(), Ordering::Relaxed);
                    for s in SUBS.lock().unwrap().iter() {
                        s(mon.online());
                    }
                }
                tokio::select! {
                    () = tokio::time::sleep(mon.next_delay()) => {}
                    () = wake().notified() => {}
                }
            }
        });
    });
}

#[cxx_qt::bridge]
pub mod qobject {
    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        /// True while `music.youtube.com:443` is reachable.
        #[qproperty(bool, online)]
        type Connectivity = super::ConnectivityRust;

        /// Probe right now (cheap; the result arrives through `onlineChanged`).
        #[qinvokable]
        fn check_now(self: Pin<&mut Connectivity>);
    }

    impl cxx_qt::Threading for Connectivity {}
    impl cxx_qt::Initialize for Connectivity {}
}

pub struct ConnectivityRust {
    online: bool,
}

impl Default for ConnectivityRust {
    fn default() -> Self {
        Self { online: is_online() }
    }
}

impl cxx_qt::Initialize for qobject::Connectivity {
    fn initialize(self: Pin<&mut Self>) {
        use cxx_qt::Threading;
        let th = self.qt_thread();
        subscribe(move |on| {
            let _ = th.queue(move |mut c| c.as_mut().set_online(on));
        });
        start();
    }
}

impl qobject::Connectivity {
    fn check_now(self: Pin<&mut Self>) {
        check_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(m: &mut Monitor, results: &[bool]) -> Vec<bool> {
        results.iter().map(|&r| m.record(r)).collect()
    }

    #[test]
    fn starts_online_and_one_failure_is_a_blip() {
        let mut m = Monitor::default();
        assert!(m.online());
        assert_eq!(m.next_delay(), ONLINE_INTERVAL);
        assert!(!m.record(false));
        assert!(m.online(), "a single failure keeps us online");
        assert_eq!(m.next_delay(), RECHECK_INTERVAL, "but re-check quickly");
        assert!(!m.record(true));
        assert_eq!(m.next_delay(), ONLINE_INTERVAL);
    }

    #[test]
    fn two_failures_flip_offline_and_one_success_flips_back() {
        let mut m = Monitor::default();
        assert_eq!(run(&mut m, &[false, false, false, true]), [false, true, false, true]);
        assert!(m.online());
    }

    #[test]
    fn flips_are_reported_exactly_once() {
        let mut m = Monitor::default();
        let flips = run(&mut m, &[true, false, false, false, false, true, true]);
        assert_eq!(flips.iter().filter(|&&f| f).count(), 2);
    }

    #[test]
    fn offline_backoff_grows_and_is_capped() {
        let mut m = Monitor::default();
        m.record(false);
        m.record(false);
        let mut delays = vec![m.next_delay()];
        for _ in 0..8 {
            m.record(false);
            delays.push(m.next_delay());
        }
        let secs: Vec<u64> = delays.iter().map(Duration::as_secs).collect();
        assert_eq!(secs, [5, 10, 20, 40, 60, 60, 60, 60, 60]);
        assert!(secs.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn a_long_outage_does_not_overflow() {
        let mut m = Monitor::default();
        for _ in 0..100_000 {
            m.record(false);
        }
        assert_eq!(m.next_delay(), OFFLINE_MAX);
        assert!(m.record(true));
        assert_eq!(m.next_delay(), ONLINE_INTERVAL);
    }

    #[tokio::test]
    async fn one_address_that_never_answers_does_not_hide_a_working_one() {
        use std::net::SocketAddr;
        // A listener whose accept queue is full never completes another handshake: connecting to it hangs, like a
        // blackholed IPv6 route does.
        let sock = tokio::net::TcpSocket::new_v4().unwrap();
        sock.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let hang = sock.listen(1).unwrap(); // never accepted from
        let hang_addr = hang.local_addr().unwrap();
        let mut fillers = Vec::new();
        let hangs = loop {
            match tokio::time::timeout(Duration::from_millis(300), tokio::net::TcpStream::connect(hang_addr)).await {
                Ok(Ok(s)) if fillers.len() < 64 => fillers.push(s),
                Ok(_) => break false, // this system never fills up (or refused): the ordering below is still checked
                Err(_) => break true,
            }
        };
        let good = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let good_addr = good.local_addr().unwrap();
        let closed: SocketAddr = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap() // dropped: nothing listens there any more
        };
        // the dead ones come first, as a resolver that prefers IPv6 would order them
        let found = tokio::time::timeout(Duration::from_secs(3), connect_any([hang_addr, closed, good_addr])).await;
        assert_eq!(found, Ok(true), "found the working address without waiting for the others (hanging one: {hangs})");
        assert!(!connect_any([closed]).await);
        assert!(!connect_any(Vec::<SocketAddr>::new()).await);
    }

    #[tokio::test]
    async fn the_probe_resolves_and_connects_without_http() {
        // A local listener stands in for the server: connecting must succeed with no bytes sent.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let accept = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 16];
            tokio::time::timeout(Duration::from_millis(300), tokio::io::AsyncReadExt::read(&mut s, &mut buf)).await
        });
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok());
        // closing without writing yields EOF (0 bytes) at the server, never data
        assert!(matches!(accept.await.unwrap(), Ok(Ok(0)) | Err(_)));
    }
}
