//! What a page holds: the state to draw ([`Link::state`]) and the way to
//! ask for a change ([`Link::send`]). Three kinds, one interface:
//! - [`Link::daemon`]: the running daemon (the watch feed, and one control
//!   connection per action on a worker thread, in order);
//! - [`Link::mock`]: a board scenario that answers actions the way the
//!   daemon would (`appstate::mock::apply`), for building and screenshots;
//! - [`Link::recording`]: a fixed state that only records actions (UI
//!   tests: click, then assert what was sent).
//!
//! `send` never blocks a frame. The page draws from the next snapshot, not
//! from what it sent: the daemon's answer is the truth (it may refuse, or
//! land somewhere else, as a strength edit on a profile does).

use crate::appstate::{mock, Action, AppState};
use crate::watch::Feed;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

type Wake = Arc<dyn Fn() + Send + Sync>;

enum Sink {
    Daemon(mpsc::Sender<Action>),
    Mock(Arc<Mutex<AppState>>, Wake),
    Record,
}

#[derive(Default)]
struct Log {
    /// What a mock or recording link was asked (a daemon link keeps none).
    sent: Vec<Action>,
    refused: Vec<(Action, String)>,
}

pub struct Link {
    feed: Feed,
    sink: Sink,
    log: Arc<Mutex<Log>>,
}

impl Link {
    /// The running daemon on the default control socket. `wake` runs after
    /// every change (pass the window's repaint request).
    pub fn daemon(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Link::daemon_at(crate::lock::default_control_socket_path(), wake)
    }

    /// A daemon on the control socket at `path`.
    pub fn daemon_at(path: std::path::PathBuf, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let wake: Wake = Arc::new(wake);
        let w = wake.clone();
        let feed = Feed::connect(path.clone(), move || w());
        let log = Arc::new(Mutex::new(Log::default()));
        let (tx, rx) = mpsc::channel::<Action>();
        let worker_log = log.clone();
        std::thread::spawn(move || {
            for action in rx {
                let words: Vec<String> = action
                    .request_line()
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                // The same ownership check the feed makes before connecting.
                let (code, out) = if crate::control::socket_is_ours(&path) {
                    crate::control::client_run_at(&path, &words)
                } else {
                    (2, "hushmic is not running".to_string())
                };
                if code != 0 {
                    eprintln!("[hushmic] the window's request was refused: {out}");
                    lock(&worker_log).refused.push((action, out));
                    wake();
                }
            }
        });
        Link {
            feed,
            sink: Sink::Daemon(tx),
            log,
        }
    }

    /// A mock daemon starting from `state` (see `appstate::mock`).
    pub fn mock(state: AppState, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Link {
            feed: Feed::fixed(state.clone()),
            sink: Sink::Mock(Arc::new(Mutex::new(state)), Arc::new(wake)),
            log: Arc::new(Mutex::new(Log::default())),
        }
    }

    /// A fixed state; actions are only recorded.
    pub fn recording(state: AppState) -> Self {
        Link {
            feed: Feed::fixed(state),
            sink: Sink::Record,
            log: Arc::new(Mutex::new(Log::default())),
        }
    }

    /// The newest snapshot; None until the first arrived (a daemon link
    /// that never connected).
    pub fn state(&self) -> Option<Arc<AppState>> {
        self.feed.latest()
    }

    /// Wait up to `timeout` for the first snapshot.
    pub fn wait_first(&self, timeout: Duration) -> Option<Arc<AppState>> {
        self.feed.wait_first(timeout)
    }

    /// Talking to a daemon right now (always true for mock and recording).
    pub fn connected(&self) -> bool {
        self.feed.connected()
    }

    /// Moves on every change: compare between frames.
    pub fn changes(&self) -> u64 {
        self.feed.changes()
    }

    /// Ask for a change. Never blocks.
    pub fn send(&self, action: Action) {
        if !matches!(self.sink, Sink::Daemon(_)) {
            lock(&self.log).sent.push(action.clone());
        }
        match &self.sink {
            Sink::Daemon(tx) => {
                let _ = tx.send(action);
            }
            Sink::Mock(state, wake) => {
                let starts = {
                    let mut s = lock(state);
                    let starts = mock::apply(&mut s, &action);
                    self.feed.set(s.clone());
                    starts
                };
                wake();
                if starts {
                    let (state, feed, wake) = (state.clone(), self.feed.clone(), wake.clone());
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs_f32(mock::START_SECS));
                        let mut s = lock(&state);
                        mock::settle(&mut s);
                        feed.set(s.clone());
                        drop(s);
                        wake();
                    });
                }
            }
            Sink::Record => {}
        }
    }

    /// Every action sent so far, in order (mock and recording links; a
    /// daemon link keeps no log), but for Live's word on what its Status
    /// card shows (it follows the state on its own: [`Link::told`]).
    pub fn sent(&self) -> Vec<Action> {
        lock(&self.log)
            .sent
            .iter()
            .filter(|a| !matches!(a, Action::UiShows(_)))
            .cloned()
            .collect()
    }

    /// What Live told the daemon its Status card shows, in order.
    pub fn told(&self) -> Vec<Option<crate::notify::Problem>> {
        lock(&self.log)
            .sent
            .iter()
            .filter_map(|a| match a {
                Action::UiShows(p) => Some(*p),
                _ => None,
            })
            .collect()
    }

    /// Actions the daemon refused since the last call, with its message
    /// (English; for a log line or a quiet notice).
    pub fn take_refused(&self) -> Vec<(Action, String)> {
        std::mem::take(&mut lock(&self.log).refused)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock::Scenario;
    use crate::appstate::{Mode, Phase};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_recording_link_records_and_changes_nothing() {
        let link = Link::recording(mock::state(Scenario::OnInCall));
        link.send(Action::ToggleMute);
        link.send(Action::SetMode(Mode::Bypass));
        assert_eq!(
            link.sent(),
            vec![Action::ToggleMute, Action::SetMode(Mode::Bypass)]
        );
        assert_eq!(link.state().unwrap().mode, Mode::Suppress);
        assert!(link.connected());
    }

    #[test]
    fn a_mock_link_answers_like_the_daemon_and_settles_a_start() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        let link = Link::mock(mock::state(Scenario::Off), move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        let before = link.changes();
        link.send(Action::SetMode(Mode::Suppress));
        let s = link.state().unwrap();
        assert_eq!(s.mode, Mode::Suppress);
        assert_eq!(s.engine.phase, Phase::Starting);
        assert!(link.changes() > before);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while link.state().unwrap().engine.phase != Phase::Ready {
            assert!(std::time::Instant::now() < deadline, "never settled");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(wakes.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn a_daemon_link_without_a_daemon_has_no_state_and_records_refusals() {
        // A socket path nobody serves (never the user's own daemon): nothing
        // arrives, and a send is refused (exit 2) without blocking.
        let path =
            std::env::temp_dir().join(format!("hushmic-link-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let link = Link::daemon_at(path, || {});
        assert!(link.wait_first(Duration::from_millis(200)).is_none());
        assert!(!link.connected());
        link.send(Action::Refresh);
        assert!(link.sent().is_empty(), "a daemon link keeps no log");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let refused = link.take_refused();
            if !refused.is_empty() {
                assert_eq!(refused[0].0, Action::Refresh);
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no refusal");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
