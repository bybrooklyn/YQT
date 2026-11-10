// SPDX-License-Identifier: GPL-3.0-only
//! Where the bytes come from: a ranged HTTP reader (the localhost stream proxy), a local file, or memory.
//!
//! The demuxer wants `Read + Seek`; the proxy supports `Range`, so a seek is simply a new ranged request at the
//! byte offset the demuxer asks for. Everything here is blocking and runs on the engine's own threads.
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::OnceLock;
use std::time::Duration;
use symphonia::core::io::MediaSource;

/// The containers YouTube serves audio in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// Matroska / WebM (EBML header `1A 45 DF A3`): Opus.
    WebM,
    /// ISO base media (`ftyp` box): AAC.
    Mp4,
    Unknown,
}

/// Recognise the container from the first bytes of a stream.
pub fn sniff(head: &[u8]) -> Container {
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        Container::WebM
    } else if head.len() >= 8 && (&head[4..8] == b"ftyp" || &head[4..8] == b"styp") {
        Container::Mp4
    } else {
        Container::Unknown
    }
}

/// A small runtime used only to drive `reqwest` from the engine's blocking threads.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("ytm-audio-net")
            .enable_all()
            .build()
            .expect("audio net runtime")
    })
}

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy() // the stream proxy is on loopback; never route it through a system proxy
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .build()
            .expect("audio http client")
    })
}

fn io_err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

// ---------------------------------------------------------------------------------------------
// interrupting a blocked read
// ---------------------------------------------------------------------------------------------

/// Lets another thread break a network read that the engine's command thread is blocked in.
///
/// The command thread also decodes, so a stalled upstream (connection open, no bytes) would keep `stop`, `load` and
/// `seek` waiting for the read timeout. The caller of such a command `raise`s the interrupt *before* sending it; from
/// then until the command thread `ack`s that very command, every network wait on that thread gives up at once (and
/// new ones do not start). Nothing is lost by it: the response stays open and the decoder continues from the same
/// sample later (see `TrackDecoder::next`).
///
/// It is installed per thread (`install`), not per stream: streams are opened on helper threads and then handed to
/// the command thread, and it is the thread that must not be kept waiting. Helper threads are never interrupted.
#[derive(Default)]
pub struct Interrupt {
    raised: std::sync::atomic::AtomicU64,
    acked: std::sync::atomic::AtomicU64,
    closed: std::sync::atomic::AtomicBool,
}

/// How often a blocked read looks at the interrupt: the bound on how long a command waits for a stalled read.
const INTERRUPT_POLL: Duration = Duration::from_millis(15);

impl Interrupt {
    /// A command that must not wait is on its way.
    pub fn raise(&self) {
        self.raised.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    /// One raised command has been taken up by the command thread.
    pub fn ack(&self) {
        self.acked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    /// For good (shutdown): every read on the thread fails from now on.
    pub fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn pending(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.closed.load(SeqCst) || self.raised.load(SeqCst) != self.acked.load(SeqCst)
    }
}

thread_local! {
    static INTERRUPT: std::cell::RefCell<Option<std::sync::Arc<Interrupt>>> = const { std::cell::RefCell::new(None) };
}

/// Make network reads on the current thread interruptible through `i` (`None` removes it again).
pub fn install_interrupt(i: Option<std::sync::Arc<Interrupt>>) {
    INTERRUPT.with(|slot| *slot.borrow_mut() = i);
}

fn thread_interrupt() -> Option<std::sync::Arc<Interrupt>> {
    INTERRUPT.with(|slot| slot.borrow().clone())
}

/// A command is waiting for the current thread: an I/O error right now is (or may as well be) that interruption.
pub fn interrupt_pending() -> bool {
    INTERRUPT.with(|slot| slot.borrow().as_ref().is_some_and(|i| i.pending()))
}

fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "interrupted by a newer command")
}

/// Drive `fut` on the audio runtime from this (blocking) thread; give up as soon as the thread's interrupt is pending.
fn block_on_interruptible<T>(fut: impl std::future::Future<Output = T>) -> io::Result<T> {
    let Some(int) = thread_interrupt() else { return Ok(runtime().block_on(fut)) };
    if int.pending() {
        return Err(interrupted());
    }
    runtime().block_on(async {
        tokio::pin!(fut);
        loop {
            tokio::select! {
                biased;
                v = &mut fut => return Ok(v),
                () = tokio::time::sleep(INTERRUPT_POLL) => {
                    if int.pending() {
                        return Err(interrupted());
                    }
                }
            }
        }
    })
}

/// Forward seeks shorter than this are served by reading and discarding (keeps the connection).
const SKIP_LIMIT: u64 = 32 * 1024;

/// A seekable view of an HTTP resource using `Range` requests.
pub struct HttpRangeSource {
    url: String,
    len: u64,
    /// Current logical position (what the next `read` returns).
    pos: u64,
    /// The open response, if any; it delivers its next byte at `end`.
    resp: Option<reqwest::Response>,
    /// Bytes received but not yet consumed start at `buf_start`; `end == buf_start + buf.len()` unless drained.
    buf: bytes::Bytes,
    buf_start: u64,
    end: u64,
}

impl HttpRangeSource {
    /// Open `url` and learn its length. Fails with a readable message on HTTP errors (404 from the proxy, ...).
    pub fn open(url: &str) -> Result<Self, String> {
        let mut s = Self { url: url.to_owned(), len: 0, pos: 0, resp: None, buf: bytes::Bytes::new(), buf_start: 0, end: 0 };
        let resp = s.request(0).map_err(|e| e.to_string())?;
        let status = resp.status();
        s.len = if status.as_u16() == 206 {
            resp.headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next())
                .and_then(|v| v.parse().ok())
                .ok_or("server sent no Content-Range")?
        } else {
            resp.content_length().ok_or("server sent no length")?
        };
        s.resp = Some(resp);
        Ok(s)
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    fn request(&self, from: u64) -> io::Result<reqwest::Response> {
        // Built and polled inside the runtime: constructing the client outside of it has no reactor to register with.
        let resp = block_on_interruptible(async { client().get(&self.url).header("Range", format!("bytes={from}-")).send().await })?
            .map_err(|e| io_err(format!("request failed: {}", e.without_url())))?;
        let st = resp.status();
        if st.as_u16() == 206 || (st.as_u16() == 200 && from == 0) {
            Ok(resp)
        } else if st.as_u16() == 416 {
            Err(io::Error::new(io::ErrorKind::UnexpectedEof, "range not satisfiable"))
        } else {
            Err(io_err(format!("HTTP {} {}", st.as_u16(), st.canonical_reason().unwrap_or(""))))
        }
    }

    /// Next piece of the open response (or `None` at its end).
    fn next_chunk(&mut self) -> io::Result<Option<bytes::Bytes>> {
        let Some(resp) = self.resp.as_mut() else { return Ok(None) };
        // Giving up on the wait keeps the response: `chunk` holds no data between its polls, so the next call goes on
        // with the same body.
        match block_on_interruptible(resp.chunk())? {
            Ok(Some(b)) => Ok(Some(b)),
            Ok(None) => {
                self.resp = None;
                Ok(None)
            }
            Err(e) => {
                self.resp = None;
                Err(io_err(format!("stream interrupted: {}", e.without_url())))
            }
        }
    }
}

impl Read for HttpRangeSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let mut fresh = false; // a response opened during this call that has delivered nothing yet
        loop {
            if self.pos >= self.buf_start && self.pos < self.end {
                let off = (self.pos - self.buf_start) as usize;
                let n = out.len().min(self.buf.len() - off);
                out[..n].copy_from_slice(&self.buf[off..off + n]);
                self.pos += n as u64;
                return Ok(n);
            }
            let near = self.resp.is_some() && self.pos >= self.end && self.pos - self.end <= SKIP_LIMIT;
            if near {
                match self.next_chunk()? {
                    Some(b) => {
                        self.buf_start = self.end;
                        self.end += b.len() as u64;
                        self.buf = b;
                        fresh = false;
                    }
                    None if fresh => return Ok(0), // a new response with no data: truncated stream
                    None => {}
                }
            } else {
                self.resp = Some(self.request(self.pos)?);
                self.buf = bytes::Bytes::new();
                self.buf_start = self.pos;
                self.end = self.pos;
                fresh = true;
            }
        }
    }
}

impl Seek for HttpRangeSource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let new = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        self.pos = new;
        Ok(new)
    }
}

impl MediaSource for HttpRangeSource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

/// An in-memory source (tests, small files).
pub struct MemorySource(io::Cursor<Vec<u8>>);

impl MemorySource {
    pub fn new(data: Vec<u8>) -> Self {
        Self(io::Cursor::new(data))
    }
}

impl Read for MemorySource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}
impl Seek for MemorySource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }
}
impl MediaSource for MemorySource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.0.get_ref().len() as u64)
    }
}

/// Open whatever `url` names: `http(s)://` (the stream proxy), `file://…` or a plain path.
pub fn open(url: &str) -> Result<Box<dyn MediaSource>, String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(Box::new(HttpRangeSource::open(url)?))
    } else {
        let path = url.strip_prefix("file://").unwrap_or(url);
        let f = std::fs::File::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
        Ok(Box::new(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_containers() {
        assert_eq!(sniff(&[0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42]), Container::WebM);
        assert_eq!(sniff(b"\0\0\0\x1cftypiso5"), Container::Mp4);
        assert_eq!(sniff(b"\0\0\0\x18styp"), Container::Mp4);
        assert_eq!(sniff(b"OggS\0\x02"), Container::Unknown);
        assert_eq!(sniff(b""), Container::Unknown);
        assert_eq!(sniff(b"<html>not audio</html>"), Container::Unknown);
    }

    #[test]
    fn memory_source_reads_and_seeks() {
        let mut m = MemorySource::new((0..100u8).collect());
        let mut b = [0u8; 4];
        m.seek(SeekFrom::Start(10)).unwrap();
        m.read_exact(&mut b).unwrap();
        assert_eq!(b, [10, 11, 12, 13]);
        assert_eq!(m.byte_len(), Some(100));
    }

    #[test]
    fn missing_file_is_an_error_message_not_a_panic() {
        let e = open("/definitely/not/here.webm").err().unwrap();
        assert!(e.contains("cannot open"), "{e}");
    }
}
