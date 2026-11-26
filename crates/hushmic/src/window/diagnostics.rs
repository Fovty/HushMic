//! "Copy diagnostics" for the window (Settings › About), carried over from
//! the old About window: the report is collected off-thread (its probes shell
//! out to pw-dump — never block a frame), copied to the clipboard when it
//! arrives, and the button says so for a moment. The report is the
//! doctor's, in English on purpose (bug reports must be readable upstream).

use eframe::egui;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// How long the button says "Copied" before it is a button again.
pub const COPIED_FOR: Duration = Duration::from_secs(2);

/// What the button shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyView {
    /// "Copy diagnostics", clickable.
    Idle,
    /// "Collecting…", not clickable.
    Collecting,
    /// "Copied".
    Copied,
}

enum Phase {
    Idle,
    Collecting(Receiver<String>),
    Copied(Instant),
}

pub struct Copier {
    phase: Phase,
    /// Produces the report; a stub in tests.
    collect: fn() -> String,
}

impl Default for Copier {
    fn default() -> Self {
        Copier::with_collector(report)
    }
}

/// The doctor's report, as `hushmic --doctor` prints it.
pub fn report() -> String {
    crate::diagnostics::render(&crate::diagnostics::collect()).0
}

impl Copier {
    pub fn with_collector(collect: fn() -> String) -> Self {
        Copier {
            phase: Phase::Idle,
            collect,
        }
    }

    /// The button was clicked: start collecting (ignored while busy).
    pub fn start(&mut self) {
        if matches!(self.phase, Phase::Collecting(_)) {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let collect = self.collect;
        std::thread::spawn(move || {
            let _ = tx.send(collect());
        });
        self.phase = Phase::Collecting(rx);
    }

    /// Call every frame the button is shown: copies the report when it
    /// arrives, asks for the frames the lifecycle needs, and says what the
    /// button shows.
    pub fn poll(&mut self, ctx: &egui::Context) -> CopyView {
        self.poll_at(
            Instant::now(),
            |text| ctx.copy_text(text),
            |after| ctx.request_repaint_after(after),
        )
    }

    fn poll_at(
        &mut self,
        now: Instant,
        copy: impl FnOnce(String),
        repaint_after: impl FnOnce(Duration),
    ) -> CopyView {
        self.phase = match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Collecting(rx) => match rx.try_recv() {
                Ok(text) => {
                    copy(text);
                    Phase::Copied(now)
                }
                Err(TryRecvError::Empty) => Phase::Collecting(rx),
                // The collector died: give the button back rather than
                // wedge it on "Collecting…".
                Err(TryRecvError::Disconnected) => Phase::Idle,
            },
            Phase::Copied(at) if now.duration_since(at) >= COPIED_FOR => Phase::Idle,
            other => other,
        };
        match &self.phase {
            Phase::Idle => CopyView::Idle,
            Phase::Collecting(_) => {
                repaint_after(Duration::from_millis(50));
                CopyView::Collecting
            }
            Phase::Copied(at) => {
                repaint_after(COPIED_FOR.saturating_sub(now.duration_since(*at)));
                CopyView::Copied
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_copied(c: &mut Copier, t0: Instant) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let mut got = None;
            let view = c.poll_at(t0, |t| got = Some(t), |_| {});
            if let Some(t) = got {
                assert_eq!(view, CopyView::Copied);
                return Some(t);
            }
            assert_eq!(view, CopyView::Collecting);
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn a_click_collects_off_thread_copies_and_confirms_for_a_moment() {
        let mut c = Copier::with_collector(|| "stub diagnostics report".into());
        let t0 = Instant::now();
        assert_eq!(
            c.poll_at(t0, |_| panic!("nothing to copy"), |_| {}),
            CopyView::Idle
        );
        c.start();
        assert_eq!(
            wait_copied(&mut c, t0).as_deref(),
            Some("stub diagnostics report")
        );
        assert_eq!(
            c.poll_at(t0 + Duration::from_secs(1), |_| {}, |_| {}),
            CopyView::Copied
        );
        assert_eq!(c.poll_at(t0 + COPIED_FOR, |_| {}, |_| {}), CopyView::Idle);
    }

    #[test]
    fn a_collector_that_dies_gives_the_button_back() {
        let mut c = Copier::with_collector(|| panic!("probe crashed"));
        c.start();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match c.poll_at(Instant::now(), |_| panic!("no report"), |_| {}) {
                CopyView::Idle => break,
                CopyView::Collecting => {}
                CopyView::Copied => panic!("nothing was copied"),
            }
            assert!(Instant::now() < deadline, "stuck on Collecting");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_second_click_while_collecting_is_ignored() {
        let mut c = Copier::with_collector(|| {
            std::thread::sleep(Duration::from_millis(50));
            "r".into()
        });
        c.start();
        c.start();
        assert_eq!(wait_copied(&mut c, Instant::now()).as_deref(), Some("r"));
    }
}
