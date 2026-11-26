//! The `watch` channel: a long-lived connection on the control socket over
//! which the daemon pushes an [`AppState`] snapshot every time it changes.
//! The window never polls.
//!
//! Wire format, after the request line `watch`:
//!
//! ```text
//! ok
//! {"v":1,"seq":1,"state":{…}}    a snapshot; the current one comes at once
//! {"v":1,"seq":1,"beat":42}      a keep-alive, at most PING_EVERY apart
//! {"v":1,"seq":1,"event":"stopped"}  the last line, on a clean quit
//! ```
//!
//! One JSON object per line. `v` is [`PROTOCOL`]; `seq` counts snapshots
//! (a keep-alive repeats the last one); `beat` counts the main loop's turns
//! (it publishes on every one, at least once a tick), so a reader can tell
//! a daemon whose loop is stuck from one with nothing new. A reader skips lines it cannot
//! parse or whose `v` it does not speak. Slow readers are never waited
//! for: each watcher has a one-slot mailbox and gets the latest snapshot,
//! skipping any it was too slow for.
//!
//! Daemon side: [`Hub`] (the control listener hands it `watch`
//! connections; the main loop publishes). Window side: [`Feed`].

use crate::appstate::{AppState, PROTOCOL};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

/// A keep-alive goes out when nothing else did for this long. It is also
/// how a writer notices that its window has gone.
pub const PING_EVERY: Duration = Duration::from_secs(5);
/// A watcher that cannot take a line within this long is dropped (its
/// window reconnects if it is still alive).
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// A reader that hears nothing (not even a keep-alive) for this long
/// assumes a wedged daemon and reconnects.
const READ_TIMEOUT: Duration = Duration::from_secs(16);
/// Watchers at once (one window, plus status-bar scripts).
const MAX_WATCHERS: usize = 16;
/// Keep-alives whose `beat` has not moved for this long mean the daemon's
/// main loop is stuck (it turns at least once a tick, 5 s): the window
/// stops calling its state live.
const STALL_AFTER: Duration = Duration::from_secs(20);

/// One line as read.
#[derive(Debug, Deserialize)]
pub struct Line {
    pub v: u32,
    pub seq: u64,
    #[serde(default)]
    pub state: Option<AppState>,
    /// The daemon loop's turn count (keep-alives).
    #[serde(default)]
    pub beat: Option<u64>,
    /// `stopped`: the daemon quit cleanly; nothing follows.
    #[serde(default)]
    pub event: Option<String>,
}

#[derive(Serialize)]
struct LineOut<'a> {
    v: u32,
    seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<&'a AppState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    beat: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<&'a str>,
}

fn encode_line(seq: u64, state: Option<&AppState>, beat: Option<u64>) -> String {
    encode_out(LineOut {
        v: PROTOCOL,
        seq,
        state,
        beat,
        event: None,
    })
}

/// The last line on a clean quit: `{"v":1,"seq":…,"event":"stopped"}`.
pub fn encode_stopped(seq: u64) -> String {
    encode_out(LineOut {
        v: PROTOCOL,
        seq,
        state: None,
        beat: None,
        event: Some(STOPPED),
    })
}

/// The `event` of the last line on a clean quit.
pub const STOPPED: &str = "stopped";

fn encode_out(line: LineOut<'_>) -> String {
    let mut s = serde_json::to_string(&line).expect("AppState serializes");
    s.push('\n');
    s
}

/// A line to write, newline included.
pub fn encode(seq: u64, state: Option<&AppState>) -> String {
    encode_line(seq, state, None)
}

/// A keep-alive carrying the main loop's turn count.
pub fn encode_ping(seq: u64, beat: u64) -> String {
    encode_line(seq, None, Some(beat))
}

/// Parse one line; None for junk or another protocol version.
pub fn decode(line: &str) -> Option<Line> {
    serde_json::from_str::<Line>(line)
        .ok()
        .filter(|l| l.v == PROTOCOL)
}

// --- daemon side --------------------------------------------------------------

/// A watcher's one-slot mailbox: the writer thread takes the newest line,
/// the publisher replaces whatever it did not take yet.
struct Mailbox {
    slot: Mutex<Slot>,
    cv: Condvar,
    /// The watcher's socket, to notice a reader that went away while
    /// nothing is being written to it.
    peer: Option<UnixStream>,
}

#[derive(Default)]
struct Slot {
    pending: Option<(u64, Arc<str>)>,
    closed: bool,
    /// The pending line is the last: the writer ends after it.
    last: bool,
}

enum Next {
    /// A line, and whether it is the last.
    Line(u64, Arc<str>, bool),
    Idle,
    Closed,
}

impl Mailbox {
    fn new(first: Option<(u64, Arc<str>)>, peer: Option<UnixStream>) -> Self {
        Mailbox {
            slot: Mutex::new(Slot {
                pending: first,
                ..Slot::default()
            }),
            cv: Condvar::new(),
            peer,
        }
    }

    /// Leave `line` for the writer; false once the watcher is gone.
    fn offer(&self, seq: u64, line: Arc<str>) -> bool {
        let mut s = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        if s.closed || s.last {
            return false;
        }
        s.pending = Some((seq, line));
        self.cv.notify_one();
        true
    }

    /// Leave `line` as the last one: the writer sends it and ends.
    fn finish(&self, seq: u64, line: Arc<str>) {
        let mut s = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        if !s.closed {
            s.pending = Some((seq, line));
            s.last = true;
            self.cv.notify_one();
        }
    }

    fn take(&self, wait: Duration) -> Next {
        let s = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        let (mut s, _) = self
            .cv
            .wait_timeout_while(s, wait, |s| s.pending.is_none() && !s.closed)
            .unwrap_or_else(|e| e.into_inner());
        if s.closed {
            return Next::Closed;
        }
        match s.pending.take() {
            Some((seq, line)) => Next::Line(seq, line, s.last),
            None => Next::Idle,
        }
    }

    fn close(&self) {
        let mut s = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        s.closed = true;
        self.cv.notify_one();
    }

    fn is_closed(&self) -> bool {
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).closed
    }

    /// Closed, or its reader hung up (a watcher only ever sends its one
    /// request line, so end-of-stream on a peek means gone). Closes the
    /// mailbox then, which ends the writer at once.
    fn is_gone(&self) -> bool {
        if self.is_closed() {
            return true;
        }
        if self.peer.as_ref().is_some_and(peer_hung_up) {
            self.close();
            return true;
        }
        false
    }
}

/// The other end of `s` closed (a non-blocking peek reads end-of-stream).
fn peer_hung_up(s: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut b = [0u8; 1];
    let n = unsafe {
        libc::recv(
            s.as_raw_fd(),
            b.as_mut_ptr().cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    n == 0
}

#[derive(Default)]
struct Published {
    seq: u64,
    last: Option<AppState>,
    line: Option<Arc<str>>,
    watchers: Vec<Arc<Mailbox>>,
    /// [`Hub::stop`] ran: no new watchers.
    stopped: bool,
}

/// The daemon's end: the main loop publishes, the control listener
/// subscribes. Cheap to clone (shared).
#[derive(Clone)]
pub struct Hub {
    p: Arc<Mutex<Published>>,
    /// Publish calls so far: the main loop's heartbeat.
    beat: Arc<AtomicU64>,
    /// How long a watcher may take to take a line ([`WRITE_TIMEOUT`]).
    write_timeout: Duration,
}

impl Default for Hub {
    fn default() -> Self {
        Hub {
            p: Arc::default(),
            beat: Arc::default(),
            write_timeout: WRITE_TIMEOUT,
        }
    }
}

impl Hub {
    pub fn new() -> Self {
        Hub::default()
    }

    /// A hub whose watchers may take `d` to take a line: for tests whose
    /// reader is held back on purpose, on a runner of any speed.
    #[cfg(test)]
    fn with_write_timeout(d: Duration) -> Self {
        Hub {
            write_timeout: d,
            ..Hub::default()
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Published> {
        self.p.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Send `state` to every watcher, unless it equals the last one sent.
    /// Never blocks on a watcher. True when it was new. Every call counts
    /// as a turn of the main loop (the keep-alives carry the count).
    pub fn publish(&self, state: &AppState) -> bool {
        self.beat.fetch_add(1, Ordering::Relaxed);
        let mut p = self.lock();
        if p.last.as_ref() == Some(state) {
            return false;
        }
        p.seq += 1;
        let seq = p.seq;
        let line: Arc<str> = encode(seq, Some(state)).into();
        p.last = Some(state.clone());
        p.line = Some(line.clone());
        p.watchers.retain(|m| m.offer(seq, line.clone()));
        true
    }

    /// The last snapshot published, if any.
    pub fn last(&self) -> Option<AppState> {
        self.lock().last.clone()
    }

    /// A clean quit: every watcher gets `{"event":"stopped"}` as its last
    /// line, and new ones are turned away. Waits up to `wait` for the
    /// lines to go out (UX panel round 2, A10: a script tells a quit from
    /// a crash, whose stream just ends).
    pub fn stop(&self, wait: Duration) {
        let watchers = {
            let mut p = self.lock();
            p.stopped = true;
            let line: Arc<str> = encode_stopped(p.seq).into();
            let seq = p.seq;
            for m in &p.watchers {
                m.finish(seq, line.clone());
            }
            std::mem::take(&mut p.watchers)
        };
        let until = Instant::now() + wait;
        while Instant::now() < until && !watchers.iter().all(|m| m.is_closed()) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Live watchers.
    pub fn watchers(&self) -> usize {
        let mut p = self.lock();
        p.watchers.retain(|m| !m.is_gone());
        p.watchers.len()
    }

    /// Take over a connection that asked to `watch`: answer `ok`, send the
    /// current snapshot (or the first one once there is one), then every
    /// change, on a thread of its own.
    pub fn subscribe(&self, stream: UnixStream) {
        let mailbox = {
            let mut p = self.lock();
            if p.stopped {
                return; // quitting: the connection just closes
            }
            p.watchers.retain(|m| !m.is_gone());
            if p.watchers.len() >= MAX_WATCHERS {
                drop(p);
                let mut stream = stream;
                let _ = stream.set_write_timeout(Some(self.write_timeout));
                let _ =
                    stream.write_all(crate::control::encode_err("too many watchers").as_bytes());
                return;
            }
            let first = p.line.clone().map(|l| (p.seq, l));
            let m = Arc::new(Mailbox::new(first, stream.try_clone().ok()));
            p.watchers.push(m.clone());
            m
        };
        let (beat, timeout) = (self.beat.clone(), self.write_timeout);
        std::thread::spawn(move || serve(stream, mailbox, beat, timeout));
    }
}

/// One watcher's writer: the mailbox's lines, keep-alives in between.
fn serve(mut stream: UnixStream, mailbox: Arc<Mailbox>, beat: Arc<AtomicU64>, timeout: Duration) {
    let _ = stream.set_write_timeout(Some(timeout));
    let mut seq = 0;
    let mut ok = stream.write_all(b"ok\n").is_ok();
    while ok {
        ok = match mailbox.take(PING_EVERY) {
            Next::Line(s, line, last) => {
                seq = s;
                stream.write_all(line.as_bytes()).is_ok() && !last
            }
            Next::Idle => stream
                .write_all(encode_ping(seq, beat.load(Ordering::Relaxed)).as_bytes())
                .is_ok(),
            Next::Closed => false,
        };
    }
    mailbox.close();
}

/// `hushmic watch`: print every snapshot line (keep-alives left out) until
/// the daemon goes away. A clean quit ends with `{"event":"stopped"}` and
/// exit 0; a stream that just ends (a crash, a kill) with
/// `{"event":"lost"}` and exit 1 (UX panel round 2, A10). Exit 2 when it
/// is not running, 1 when it refused.
pub fn cli_follow(path: &Path, out: &mut impl Write) -> i32 {
    use crate::control::NOT_RUNNING;
    let stream = match crate::control::socket_is_ours(path)
        .then(|| UnixStream::connect(path).ok())
        .flatten()
    {
        Some(s) => s,
        None => {
            eprintln!("{NOT_RUNNING}");
            return 2;
        }
    };
    let mut writer = &stream;
    if writer.write_all(b"watch\n").is_err() {
        eprintln!("{NOT_RUNNING}");
        return 2;
    }
    let mut lines = BufReader::new(&stream).lines();
    match lines.next() {
        Some(Ok(head)) if head == "ok" => {}
        Some(Ok(head)) => {
            eprintln!("{}", crate::control::decode_response(&head).1);
            return 1;
        }
        _ => {
            eprintln!("{}", crate::control::NO_REPLY);
            return 1;
        }
    }
    for line in lines.map_while(Result::ok) {
        let Some(l) = decode(&line) else { continue };
        if l.event.as_deref() == Some(STOPPED) {
            let _ = writeln!(out, r#"{{"event":"stopped"}}"#).and_then(|()| out.flush());
            return 0;
        }
        if l.state.is_some() {
            // A closed pipe (`| head`) ends the follow quietly.
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                return 0;
            }
        }
    }
    let _ = writeln!(out, r#"{{"event":"lost"}}"#).and_then(|()| out.flush());
    1
}

// --- window side --------------------------------------------------------------

#[derive(Default)]
struct FeedState {
    state: Option<Arc<AppState>>,
    seq: u64,
    connected: bool,
    /// Connected, but the daemon's loop has not turned for STALL_AFTER.
    stalled: bool,
    /// Bumps on every change the feed sees (new snapshot, connect, loss).
    changes: u64,
}

struct FeedInner {
    m: Mutex<FeedState>,
    cv: Condvar,
    /// The live connection, so dropping the feed hangs up at once (the
    /// reader and the daemon's watcher slot go with it).
    sock: Mutex<Option<UnixStream>>,
}

impl Drop for FeedInner {
    fn drop(&mut self) {
        let sock = self.sock.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = sock.take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// The window's end: the latest snapshot, kept up to date by a background
/// reader that reconnects on its own. A fixed feed (mock, tests) holds what
/// it is given.
#[derive(Clone)]
pub struct Feed(Arc<FeedInner>);

impl Feed {
    fn empty() -> Self {
        Feed(Arc::new(FeedInner {
            m: Mutex::new(FeedState::default()),
            cv: Condvar::new(),
            sock: Mutex::new(None),
        }))
    }

    /// Watch the daemon at `path`. `wake` runs after every change (the
    /// window passes its repaint request); it must not block.
    pub fn connect(path: PathBuf, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Feed::connect_stalling(path, STALL_AFTER, wake)
    }

    fn connect_stalling(
        path: PathBuf,
        stall_after: Duration,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let feed = Feed::empty();
        let weak = Arc::downgrade(&feed.0);
        std::thread::spawn(move || reader(path, weak, stall_after, wake));
        feed
    }

    /// Watch the running daemon on the default socket.
    pub fn connect_default(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Feed::connect(crate::lock::default_control_socket_path(), wake)
    }

    /// A feed that only changes through [`Feed::set`].
    pub fn fixed(state: AppState) -> Self {
        let feed = Feed::empty();
        feed.set(state);
        feed
    }

    /// Replace the snapshot (mock feeds).
    pub fn set(&self, state: AppState) {
        let mut s = self.lock();
        s.seq += 1;
        s.state = Some(Arc::new(state));
        s.connected = true;
        s.changes += 1;
        self.0.cv.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FeedState> {
        self.0.m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The newest snapshot; None until the first one arrived.
    pub fn latest(&self) -> Option<Arc<AppState>> {
        self.lock().state.clone()
    }

    /// Connected to a daemon that answers right now (its loop turns). The
    /// snapshot stays readable after a loss (the window shows its last state
    /// while it reconnects).
    pub fn connected(&self) -> bool {
        let s = self.lock();
        s.connected && !s.stalled
    }

    /// A counter that moves on every change: compare it between frames.
    pub fn changes(&self) -> u64 {
        self.lock().changes
    }

    /// Wait up to `timeout` for the first snapshot (the window holds its
    /// first frame this long, so it never shows a wrong state).
    pub fn wait_first(&self, timeout: Duration) -> Option<Arc<AppState>> {
        let s = self.lock();
        let (s, _) = self
            .0
            .cv
            .wait_timeout_while(s, timeout, |s| s.state.is_none())
            .unwrap_or_else(|e| e.into_inner());
        s.state.clone()
    }
}

/// The reader thread: connect, follow, reconnect with a growing pause.
/// Ends when the feed is dropped.
fn reader(
    path: PathBuf,
    feed: Weak<FeedInner>,
    stall_after: Duration,
    wake: impl Fn() + Send + Sync + 'static,
) {
    let mut pause = Duration::from_millis(100);
    loop {
        if feed.strong_count() == 0 {
            return;
        }
        let got_any = follow(&path, &feed, stall_after, &wake);
        let Some(inner) = feed.upgrade() else { return };
        *inner.sock.lock().unwrap_or_else(|e| e.into_inner()) = None;
        {
            let mut s = inner.m.lock().unwrap_or_else(|e| e.into_inner());
            if s.connected {
                s.connected = false;
                s.stalled = false;
                s.changes += 1;
                drop(s);
                wake();
            }
        }
        drop(inner);
        if got_any {
            pause = Duration::from_millis(100);
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_secs(2));
    }
}

/// One connection's life; true when it delivered at least one snapshot.
fn follow(
    path: &Path,
    feed: &Weak<FeedInner>,
    stall_after: Duration,
    wake: &(impl Fn() + Send + Sync),
) -> bool {
    if !crate::control::socket_is_ours(path) {
        return false;
    }
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    {
        let Some(inner) = feed.upgrade() else {
            return false;
        };
        *inner.sock.lock().unwrap_or_else(|e| e.into_inner()) = stream.try_clone().ok();
    }
    if stream.write_all(b"watch\n").is_err() {
        return false;
    }
    let mut lines = BufReader::new(stream).lines();
    match lines.next() {
        Some(Ok(head)) if head == "ok" => {}
        _ => return false,
    }
    let mut got_any = false;
    // The loop's heartbeat as last seen, and when it last moved.
    let mut beat: Option<u64> = None;
    let mut moved = Instant::now();
    for line in lines {
        let Ok(line) = line else { break };
        // A dropped feed stops here, keep-alive or not.
        let Some(inner) = feed.upgrade() else {
            return got_any;
        };
        let Some(l) = decode(&line) else {
            continue; // junk, or a version we do not speak
        };
        let mut s = inner.m.lock().unwrap_or_else(|e| e.into_inner());
        let was = (s.connected, s.stalled);
        let snapshot = l.state.is_some();
        match l.state {
            Some(state) => {
                moved = Instant::now();
                s.seq = l.seq;
                s.state = Some(Arc::new(state));
                s.connected = true;
                s.stalled = false;
                s.changes += 1;
                got_any = true;
                inner.cv.notify_all();
            }
            None => {
                if l.beat.is_some() && l.beat != beat {
                    beat = l.beat;
                    moved = Instant::now();
                    s.stalled = false;
                } else if l.beat.is_some() && moved.elapsed() >= stall_after {
                    s.stalled = true;
                }
                if was != (s.connected, s.stalled) {
                    s.changes += 1;
                }
            }
        }
        let changed = snapshot || was != (s.connected, s.stalled);
        drop(s);
        drop(inner);
        if changed {
            wake();
        }
    }
    got_any
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::{mock, Mode};
    use std::sync::atomic::AtomicUsize;

    fn read_line(r: &mut impl BufRead) -> String {
        let mut s = String::new();
        r.read_line(&mut s).expect("read");
        s
    }

    #[test]
    fn lines_round_trip_and_foreign_versions_are_skipped() {
        let s = mock::state(mock::Scenario::Muted);
        let line = encode(3, Some(&s));
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        let back = decode(line.trim_end()).unwrap();
        assert_eq!((back.v, back.seq), (PROTOCOL, 3));
        assert_eq!(back.state.unwrap(), s);
        let ping = decode(encode(3, None).trim_end()).unwrap();
        assert!(ping.state.is_none());
        assert!(decode(r#"{"v":99,"seq":1,"state":{}}"#).is_none());
        assert!(decode("garbage").is_none());
    }

    #[test]
    fn a_watcher_gets_ok_the_current_state_then_changes_only() {
        let hub = Hub::new();
        let on = mock::state(mock::Scenario::OnInCall);
        assert!(hub.publish(&on));
        assert!(!hub.publish(&on), "an unchanged state is not resent");
        let (ours, theirs) = UnixStream::pair().unwrap();
        hub.subscribe(ours);
        let mut r = BufReader::new(theirs);
        assert_eq!(read_line(&mut r), "ok\n");
        let first = decode(read_line(&mut r).trim_end()).unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(first.state.unwrap(), on);
        let muted = mock::state(mock::Scenario::Muted);
        assert!(hub.publish(&muted));
        let next = decode(read_line(&mut r).trim_end()).unwrap();
        assert_eq!(next.seq, 2);
        assert_eq!(next.state.unwrap().mode, Mode::Mute);
        assert_eq!(hub.watchers(), 1);
    }

    /// UX panel round 2, A10: a clean quit ends every watch with
    /// `stopped`; a watch asked for while quitting just closes.
    #[test]
    fn a_clean_quit_ends_every_watch_with_stopped() {
        let hub = Hub::new();
        hub.publish(&mock::state(mock::Scenario::OnInCall));
        let mut readers = Vec::new();
        for _ in 0..2 {
            let (ours, theirs) = UnixStream::pair().unwrap();
            hub.subscribe(ours);
            let mut r = BufReader::new(theirs);
            assert_eq!(read_line(&mut r), "ok\n");
            assert!(decode(read_line(&mut r).trim_end())
                .unwrap()
                .state
                .is_some());
            readers.push(r);
        }
        hub.stop(Duration::from_secs(5));
        for mut r in readers {
            let last = decode(read_line(&mut r).trim_end()).unwrap();
            assert_eq!(last.event.as_deref(), Some(STOPPED));
            assert!(last.state.is_none() && last.beat.is_none());
            assert_eq!(read_line(&mut r), "", "nothing after it");
        }
        let (ours, theirs) = UnixStream::pair().unwrap();
        hub.subscribe(ours);
        assert_eq!(read_line(&mut BufReader::new(theirs)), "");
        assert_eq!(hub.watchers(), 0);
    }

    /// UX panel round 2, A10: `hushmic watch` ends with `stopped` and exit
    /// 0 on a clean quit, and with `lost` and exit 1 when the stream just
    /// ends (a crash).
    #[test]
    fn the_watch_command_tells_a_quit_from_a_crash() {
        let dir = std::env::temp_dir().join(format!("hushmic-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (clean, code, last) in [
            (true, 0, r#"{"event":"stopped"}"#),
            (false, 1, r#"{"event":"lost"}"#),
        ] {
            let path = dir.join(format!("control-{clean}.sock"));
            let _ = std::fs::remove_file(&path);
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            let daemon = std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let mut req = String::new();
                BufReader::new(&s).read_line(&mut req).unwrap();
                assert_eq!(req, "watch\n");
                let on = mock::state(mock::Scenario::OnInCall);
                s.write_all(b"ok\n").unwrap();
                s.write_all(encode(1, Some(&on)).as_bytes()).unwrap();
                s.write_all(encode_ping(1, 7).as_bytes()).unwrap();
                if clean {
                    s.write_all(encode_stopped(1).as_bytes()).unwrap();
                }
            });
            let mut out = Vec::new();
            assert_eq!(cli_follow(&path, &mut out), code);
            daemon.join().unwrap();
            let text = String::from_utf8(out).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 2, "{text}");
            assert!(decode(lines[0]).unwrap().state.is_some());
            assert_eq!(lines[1], last);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_watcher_that_subscribes_early_gets_the_first_snapshot() {
        let hub = Hub::new();
        let (ours, theirs) = UnixStream::pair().unwrap();
        hub.subscribe(ours);
        let mut r = BufReader::new(theirs);
        assert_eq!(read_line(&mut r), "ok\n");
        hub.publish(&mock::state(mock::Scenario::Starting));
        let first = decode(read_line(&mut r).trim_end()).unwrap();
        assert_eq!(
            first.state.unwrap().engine.phase,
            crate::appstate::Phase::Starting
        );
    }

    #[test]
    fn a_slow_watcher_gets_the_latest_never_blocks_the_publisher() {
        // The reader is held back until every publish is done, however
        // long that takes here: the writer may wait as long for it (the
        // daemon's 2 s drop of a stalled watcher is not this test's).
        let hub = Hub::with_write_timeout(Duration::from_secs(3600));
        let (ours, theirs) = UnixStream::pair().unwrap();
        hub.subscribe(ours);
        // Nobody reads: publish far more than any socket buffer holds. A
        // publisher that waited for the reader would never finish (nobody
        // reads until it has); the cap only bounds that hang.
        let (done, finished) = std::sync::mpsc::channel();
        let publisher = hub.clone();
        std::thread::spawn(move || {
            let mut s = mock::state(mock::Scenario::OnInCall);
            for g in 0..5000 {
                s.engine.generation = g;
                publisher.publish(&s);
            }
            let _ = done.send(());
        });
        assert!(
            finished.recv_timeout(Duration::from_secs(120)).is_ok(),
            "publish waited for a reader"
        );
        // When the reader catches up it gets what the socket buffer held,
        // then the newest: the rest was skipped, not queued.
        let mut r = BufReader::new(theirs);
        assert_eq!(read_line(&mut r), "ok\n");
        let mut read = 0;
        loop {
            let l = decode(read_line(&mut r).trim_end()).unwrap();
            read += 1;
            if l.state.map(|s| s.engine.generation) == Some(4999) {
                break;
            }
        }
        assert!(read < 1000, "{read} lines: the backlog was queued");
    }

    #[test]
    fn a_closed_watcher_is_dropped() {
        let hub = Hub::new();
        hub.publish(&mock::state(mock::Scenario::OnInCall));
        let (ours, theirs) = UnixStream::pair().unwrap();
        hub.subscribe(ours);
        drop(theirs);
        // The writer notices on its next write: a change or a keep-alive.
        let mut s = mock::state(mock::Scenario::OnInCall);
        for g in 0..50 {
            s.engine.generation = g;
            hub.publish(&s);
            if hub.watchers() == 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the closed watcher is still counted");
    }

    #[test]
    fn the_watcher_limit_answers_err() {
        let hub = Hub::new();
        let mut keep = Vec::new();
        for _ in 0..MAX_WATCHERS {
            let (ours, theirs) = UnixStream::pair().unwrap();
            hub.subscribe(ours);
            keep.push(theirs);
        }
        let (ours, theirs) = UnixStream::pair().unwrap();
        hub.subscribe(ours);
        let mut r = BufReader::new(theirs);
        assert!(read_line(&mut r).starts_with("err "));
    }

    fn temp_sock(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hushmic-watch-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    /// The whole path: the control listener hands `watch` to the hub and
    /// the feed follows its changes (losing and finding the daemon again is
    /// the next test).
    #[test]
    fn the_feed_follows_the_daemon_over_a_real_socket() {
        let path = temp_sock("feed.sock");
        let hub = Hub::new();
        hub.publish(&mock::state(mock::Scenario::Starting));
        let listener = crate::lock::bind_control_socket(&path).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        crate::control::spawn_listener_with(listener, tx, Some(hub.clone()));
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        let feed = Feed::connect(path.clone(), move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        let first = feed
            .wait_first(Duration::from_secs(5))
            .expect("first snapshot");
        assert_eq!(first.engine.phase, crate::appstate::Phase::Starting);
        assert!(feed.connected());
        hub.publish(&mock::state(mock::Scenario::OnInCall));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while feed.latest().unwrap().engine.phase != crate::appstate::Phase::Ready {
            assert!(std::time::Instant::now() < deadline, "no update");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(wakes.load(Ordering::SeqCst) >= 2);
        let _ = std::fs::remove_file(&path);
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The review's leak: feeds that are dropped must give their watcher
    /// slots back at once, keep-alives or not, so a later window is served.
    #[test]
    fn dropped_feeds_free_their_watcher_slots() {
        let path = temp_sock("slots.sock");
        let hub = Hub::new();
        hub.publish(&mock::state(mock::Scenario::OnInCall));
        let listener = crate::lock::bind_control_socket(&path).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        crate::control::spawn_listener_with(listener, tx, Some(hub.clone()));
        for _ in 0..MAX_WATCHERS {
            let feed = Feed::connect(path.clone(), || {});
            assert!(feed.wait_first(Duration::from_secs(5)).is_some());
            drop(feed);
        }
        let feed = Feed::connect(path.clone(), || {});
        assert!(
            feed.wait_first(Duration::from_secs(3)).is_some(),
            "the 17th window got no state"
        );
        wait_for("the dropped feeds' slots", || hub.watchers() == 1);
        let _ = std::fs::remove_file(&path);
    }

    /// A stand-in daemon on a real socket, one connection at a time.
    fn fake_daemon(name: &str) -> (PathBuf, std::os::unix::net::UnixListener) {
        let path = temp_sock(name);
        let _ = std::fs::remove_file(&path);
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        (path, l)
    }

    fn accept_watch(l: &std::os::unix::net::UnixListener) -> UnixStream {
        let (s, _) = l.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        assert_eq!(read_line(&mut r), "watch\n");
        s
    }

    #[test]
    fn the_feed_says_when_the_daemon_goes_and_follows_it_back() {
        let (path, l) = fake_daemon("back.sock");
        let feed = Feed::connect(path.clone(), || {});
        let mut s = accept_watch(&l);
        s.write_all(b"ok\n").unwrap();
        s.write_all(encode(1, Some(&mock::state(mock::Scenario::Starting))).as_bytes())
            .unwrap();
        assert!(feed.wait_first(Duration::from_secs(5)).is_some());
        assert!(feed.connected());
        drop(s); // the daemon goes away
        wait_for("the loss", || !feed.connected());
        assert_eq!(
            feed.latest().unwrap().engine.phase,
            crate::appstate::Phase::Starting,
            "the last state stays readable"
        );
        // Back again: a new connection, a new state.
        let mut s = accept_watch(&l);
        s.write_all(b"ok\n").unwrap();
        s.write_all(encode(1, Some(&mock::state(mock::Scenario::OnInCall))).as_bytes())
            .unwrap();
        wait_for("the new state", || {
            feed.connected() && feed.latest().unwrap().engine.phase == crate::appstate::Phase::Ready
        });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_full_daemon_is_asked_again() {
        let (path, l) = fake_daemon("full.sock");
        let feed = Feed::connect(path.clone(), || {});
        let mut s = accept_watch(&l);
        s.write_all(crate::control::encode_err("too many watchers").as_bytes())
            .unwrap();
        drop(s);
        let mut s = accept_watch(&l);
        s.write_all(b"ok\n").unwrap();
        s.write_all(encode(1, Some(&mock::state(mock::Scenario::Muted))).as_bytes())
            .unwrap();
        assert!(feed.wait_first(Duration::from_secs(5)).is_some());
        let _ = std::fs::remove_file(&path);
    }

    /// Keep-alives keep coming but the loop's heartbeat stands still: the
    /// window must stop calling its state live, and pick up again after.
    #[test]
    fn a_stuck_daemon_loop_is_not_called_live() {
        let (path, l) = fake_daemon("stuck.sock");
        let feed = Feed::connect_stalling(path.clone(), Duration::from_millis(300), || {});
        let mut s = accept_watch(&l);
        s.write_all(b"ok\n").unwrap();
        s.write_all(encode(1, Some(&mock::state(mock::Scenario::OnInCall))).as_bytes())
            .unwrap();
        assert!(feed.wait_first(Duration::from_secs(5)).is_some());
        let ping = |s: &mut UnixStream, beat: u64| {
            s.write_all(encode_ping(1, beat).as_bytes()).unwrap();
        };
        // The loop stays at beat 7: keep-alives go on, the beat does not
        // move. The feed judges it as it reads them, so a reader held up
        // (a busy runner) reads a burst at once and judges later: keep
        // pinging until it says so, never sooner than 300 ms after the
        // first beat 7 went out.
        let first = Instant::now();
        while feed.connected() {
            assert!(
                first.elapsed() < Duration::from_secs(30),
                "never called stuck"
            );
            ping(&mut s, 7);
            std::thread::sleep(Duration::from_millis(60));
        }
        assert!(
            first.elapsed() >= Duration::from_millis(300),
            "called stuck {:?} after the beat stopped",
            first.elapsed()
        );
        ping(&mut s, 8);
        wait_for("the loop turning again", || feed.connected());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn keep_alives_carry_the_loops_turns() {
        let hub = Hub::new();
        let on = mock::state(mock::Scenario::OnInCall);
        hub.publish(&on);
        hub.publish(&on); // unchanged, but the loop turned
        assert_eq!(hub.beat.load(Ordering::Relaxed), 2);
        let line = decode(encode_ping(4, 2).trim_end()).unwrap();
        assert_eq!(
            (line.seq, line.beat, line.state.is_none()),
            (4, Some(2), true)
        );
    }

    #[test]
    fn a_fixed_feed_holds_what_it_is_given() {
        let feed = Feed::fixed(mock::state(mock::Scenario::Off));
        let c = feed.changes();
        assert_eq!(feed.latest().unwrap().mode, Mode::Off);
        feed.set(mock::state(mock::Scenario::Bypass));
        assert_eq!(feed.latest().unwrap().mode, Mode::Bypass);
        assert!(feed.changes() > c);
        assert!(feed.wait_first(Duration::ZERO).is_some());
    }

    #[test]
    fn no_daemon_means_no_state_and_not_connected() {
        let path = temp_sock("nobody.sock");
        let _ = std::fs::remove_file(&path);
        let feed = Feed::connect(path, || {});
        assert!(feed.wait_first(Duration::from_millis(300)).is_none());
        assert!(!feed.connected());
    }
}
