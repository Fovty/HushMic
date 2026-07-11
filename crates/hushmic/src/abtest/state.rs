//! Pure state machine of the A/B test window.
//!
//! Commands apply optimistically (the UI reacts instantly); frames from the
//! backend are authoritative and may override. Frames that do not match the
//! current mode (e.g. a stale `PlaybackProgress` after Stop) are ignored.

use crate::abtest::types::{Channel, Command, Frame, SampleMetrics, RECORD_SECS};

/// Toast auto-dismiss time.
pub const TOAST_SECS: f32 = 2.6;

/// Live monitoring starts on window open, so there is no Idle mode.
/// `Sample` = "not live": reviewing a recorded sample, or stopped with none.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Live,
    Recording,
    Playback(Channel),
    Sample,
}

/// Which transport controls are enabled right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controls {
    pub record: bool,
    pub play: bool,
    pub stop: bool,
    pub go_live: bool,
}

/// Status pill content (label drives the color/pulse in the UI).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Listening,
    Recording,
    Playback,
    Sample,
    Stopped,
    NoInput,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    pub label: String,
    pub pct: f32,
    pub time: String,
}

pub struct WindowState {
    pub mode: Mode,
    pub has_sample: bool,
    pub device_ok: bool,
    pub device_name: String,
    pub metrics: Option<SampleMetrics>,
    pub toast: Option<(String, f32)>, // message + seconds remaining
    /// Seconds of live monitoring since going live (local clock; the Live
    /// timeline shows it as text only — no bar fill, no 10 s wrap).
    pub elapsed: f32,
    /// Recording progress in seconds (authoritative, from `RecordProgress`).
    pub rec_secs_done: f32,
    /// Playback position in seconds (authoritative, from `PlaybackProgress`).
    pub play_pos: f32,
}

impl WindowState {
    pub fn new() -> Self {
        WindowState {
            mode: Mode::Sample,
            has_sample: false,
            device_ok: true,
            device_name: String::new(),
            metrics: None,
            toast: None,
            elapsed: 0.0,
            rec_secs_done: 0.0,
            play_pos: 0.0,
        }
    }

    /// Optimistic transition on a user command (backend confirms via frames).
    pub fn on_command(&mut self, c: Command) {
        match c {
            Command::StartMonitor => {
                if self.mode == Mode::Sample && self.device_ok {
                    self.mode = Mode::Live;
                    self.elapsed = 0.0;
                }
            }
            Command::Stop => match self.mode {
                // Cancel the take; the monitors never stopped → live view.
                Mode::Recording => self.mode = Mode::Live,
                Mode::Playback(_) => self.mode = Mode::Sample,
                // No Stop button in Live — script/backend path only.
                Mode::Live => self.mode = Mode::Sample,
                Mode::Sample => {}
            },
            Command::Record => {
                if matches!(self.mode, Mode::Live | Mode::Sample) && self.device_ok {
                    self.mode = Mode::Recording;
                    self.rec_secs_done = 0.0;
                }
            }
            Command::Play(ch) => {
                if self.has_sample
                    && matches!(self.mode, Mode::Live | Mode::Sample)
                    && self.device_ok
                {
                    self.mode = Mode::Playback(ch);
                    self.play_pos = 0.0;
                }
            }
            // Backend-only action; the mode does not change. The retry
            // outcome arrives as a Device frame.
            Command::RetryDevice => {}
        }
    }

    /// Authoritative updates from the backend stream.
    pub fn on_frame(&mut self, f: &Frame) {
        match f {
            Frame::RecordStarted => {
                // Backend-authoritative take start: (re)enter Recording even
                // if a stale MonitorStopped just knocked the optimistic
                // Recording down to the sample view — the mic IS hot again.
                // Playback is untouchable (the backend refuses Record there).
                if self.device_ok && !matches!(self.mode, Mode::Playback(_)) {
                    if self.mode != Mode::Recording {
                        self.rec_secs_done = 0.0;
                    }
                    self.mode = Mode::Recording;
                }
            }
            Frame::RecordCancelled => {
                // The backend dropped the armed take (monitors keep running).
                // Re-syncs a quick Record→Cancel whose RecordStarted raced
                // the optimistic cancel back into Recording.
                if self.mode == Mode::Recording {
                    self.mode = Mode::Live;
                }
            }
            Frame::RecordProgress { secs_done } => {
                if self.mode == Mode::Recording {
                    self.rec_secs_done = *secs_done;
                }
            }
            Frame::RecordDone => {
                // Always authoritative: the backend only emits this for a
                // take it stored, so even a cancel that raced the completion
                // ends with this sample playable (never a desynced one).
                self.has_sample = true;
                if self.mode == Mode::Recording {
                    self.mode = Mode::Live;
                    self.elapsed = 0.0;
                }
            }
            Frame::PlaybackProgress { secs } => {
                if matches!(self.mode, Mode::Playback(_)) {
                    self.play_pos = *secs;
                }
            }
            Frame::PlaybackDone => {
                if matches!(self.mode, Mode::Playback(_)) {
                    self.mode = Mode::Sample;
                    self.play_pos = RECORD_SECS;
                }
            }
            Frame::MonitorStopped => {
                if matches!(self.mode, Mode::Live | Mode::Recording) {
                    self.mode = Mode::Sample;
                }
            }
            Frame::Metrics(m) => self.metrics = Some(*m),
            Frame::Device { ok, name } => {
                self.device_ok = *ok;
                self.device_name = name.clone();
                // Recovery never changes the mode here: the UI decides when
                // to auto-send StartMonitor (only in the sample-less view).
                if !ok {
                    self.mode = Mode::Sample;
                }
            }
            Frame::Warn(msg) => self.toast = Some((msg.clone(), TOAST_SECS)),
            // Spectrum/level frames feed the panels directly, not the state.
            Frame::Spectrum { .. } | Frame::Level { .. } => {}
        }
    }

    /// Advance local clocks (live elapsed, toast expiry) by `dt` seconds.
    /// Recording/playback progress come only from frames.
    pub fn tick(&mut self, dt: f32) {
        if self.mode == Mode::Live {
            self.elapsed += dt;
        }
        if let Some((_, left)) = &mut self.toast {
            *left -= dt;
            if *left <= 0.0 {
                self.toast = None;
            }
        }
    }

    pub fn controls(&self) -> Controls {
        let live_or_sample = matches!(self.mode, Mode::Live | Mode::Sample);
        Controls {
            record: live_or_sample && self.device_ok,
            play: self.has_sample && live_or_sample && self.device_ok,
            stop: matches!(self.mode, Mode::Recording | Mode::Playback(_)),
            go_live: self.mode == Mode::Sample && self.device_ok,
        }
    }

    pub fn status(&self) -> Status {
        if !self.device_ok {
            return Status::NoInput;
        }
        match self.mode {
            Mode::Live => Status::Listening,
            Mode::Recording => Status::Recording,
            Mode::Playback(_) => Status::Playback,
            Mode::Sample => {
                if self.has_sample {
                    Status::Sample
                } else {
                    Status::Stopped
                }
            }
        }
    }

    pub fn timeline(&self) -> Timeline {
        if !self.device_ok {
            return Timeline {
                label: "Input unavailable".into(),
                pct: 0.0,
                time: "\u{2014}".into(),
            };
        }
        match self.mode {
            // Live shows no fill and no 10 s wrap — elapsed is text only.
            Mode::Live => Timeline {
                label: "Live monitoring".into(),
                pct: 0.0,
                time: fmt_time(self.elapsed),
            },
            Mode::Recording => {
                let left = (RECORD_SECS - self.rec_secs_done).max(0.0);
                Timeline {
                    label: format!("Recording sample \u{2014} {left:.1} s left"),
                    pct: (RECORD_SECS - left) * 10.0,
                    time: format!("{} / 0:10.0", fmt_time(self.rec_secs_done)),
                }
            }
            Mode::Playback(ch) => Timeline {
                label: match ch {
                    Channel::Raw => "Playing raw sample".into(),
                    Channel::Filtered => "Playing filtered sample".into(),
                },
                pct: self.play_pos * 10.0,
                time: format!("{} / 0:10.0", fmt_time(self.play_pos)),
            },
            Mode::Sample => {
                if self.has_sample {
                    Timeline {
                        label: "Sample ready".into(),
                        pct: 100.0,
                        time: "0:10.0".into(),
                    }
                } else {
                    Timeline {
                        label: "Not monitoring".into(),
                        pct: 0.0,
                        time: "\u{2014}".into(),
                    }
                }
            }
        }
    }

    /// The channel whose panel shows the pulsing PLAYING badge.
    pub fn playing(&self) -> Option<Channel> {
        match self.mode {
            Mode::Playback(ch) => Some(ch),
            _ => None,
        }
    }

    /// The stopped hint overlay ("Not monitoring / Go live…") visibility.
    pub fn stopped_hint(&self) -> bool {
        self.mode == Mode::Sample && !self.has_sample && self.device_ok
    }
}

impl Default for WindowState {
    fn default() -> Self {
        Self::new()
    }
}

/// "m:ss.d" — one decimal, zero-padded seconds (e.g. `0:03.4`). Rounding
/// happens on the decisecond so 59.96 s renders as 1:00.0, never 0:60.0.
fn fmt_time(t: f32) -> String {
    let ds = (t.max(0.0) * 10.0).round() as u64;
    format!("{}:{:02}.{}", ds / 600, ds % 600 / 10, ds % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(mode: Mode, has_sample: bool, device_ok: bool) -> WindowState {
        let mut s = WindowState::new();
        s.mode = mode;
        s.has_sample = has_sample;
        s.device_ok = device_ok;
        s
    }

    fn assert_pct(t: &Timeline, want: f32) {
        assert!(
            (t.pct - want).abs() < 1e-3,
            "pct {} != {} ({t:?})",
            t.pct,
            want
        );
    }

    // -- boot ----------------------------------------------------------------

    #[test]
    fn boot_state_is_sampleless_sample_view() {
        let s = WindowState::new();
        assert_eq!(s.mode, Mode::Sample);
        assert!(!s.has_sample);
        assert!(s.stopped_hint());
        assert_eq!(s.status(), Status::Stopped);
    }

    // -- controls: full enablement matrix -----------------------------------

    #[test]
    fn controls_matrix() {
        let modes = [
            Mode::Live,
            Mode::Recording,
            Mode::Playback(Channel::Raw),
            Mode::Playback(Channel::Filtered),
            Mode::Sample,
        ];
        for mode in modes {
            for has_sample in [false, true] {
                for device_ok in [false, true] {
                    let c = state(mode, has_sample, device_ok).controls();
                    let live_or_sample = matches!(mode, Mode::Live | Mode::Sample);
                    assert_eq!(c.record, live_or_sample && device_ok);
                    assert_eq!(c.play, has_sample && live_or_sample && device_ok);
                    assert_eq!(c.stop, matches!(mode, Mode::Recording | Mode::Playback(_)));
                    assert_eq!(c.go_live, mode == Mode::Sample && device_ok);
                }
            }
        }
    }

    #[test]
    fn go_live_only_in_sample_view_with_device() {
        assert!(state(Mode::Sample, false, true).controls().go_live);
        assert!(state(Mode::Sample, true, true).controls().go_live);
        assert!(!state(Mode::Sample, true, false).controls().go_live);
        for mode in [Mode::Live, Mode::Recording, Mode::Playback(Channel::Raw)] {
            assert!(!state(mode, true, true).controls().go_live);
        }
    }

    // -- command transitions -------------------------------------------------

    #[test]
    fn start_monitor_from_sample_view() {
        let mut s = state(Mode::Sample, false, true);
        s.elapsed = 7.0;
        s.on_command(Command::StartMonitor);
        assert_eq!(s.mode, Mode::Live);
        assert_eq!(s.elapsed, 0.0);
    }

    #[test]
    fn start_monitor_keeps_an_existing_sample() {
        let mut s = state(Mode::Sample, true, true);
        s.on_command(Command::StartMonitor);
        assert_eq!(s.mode, Mode::Live);
        assert!(s.has_sample);
    }

    #[test]
    fn start_monitor_blocked_without_device() {
        let mut s = state(Mode::Sample, false, false);
        s.on_command(Command::StartMonitor);
        assert_eq!(s.mode, Mode::Sample);
    }

    #[test]
    fn start_monitor_blocked_when_not_in_sample_view() {
        for mode in [Mode::Live, Mode::Recording, Mode::Playback(Channel::Raw)] {
            let mut s = state(mode, true, true);
            s.on_command(Command::StartMonitor);
            assert_eq!(s.mode, mode);
        }
    }

    #[test]
    fn stop_in_recording_cancels_back_to_live() {
        // The monitors never stopped, so a cancelled take keeps the live view.
        let mut s = state(Mode::Recording, false, true);
        s.on_command(Command::Stop);
        assert_eq!(s.mode, Mode::Live);
    }

    #[test]
    fn stop_in_playback_returns_to_sample_view() {
        for ch in [Channel::Raw, Channel::Filtered] {
            let mut s = state(Mode::Playback(ch), true, true);
            s.on_command(Command::Stop);
            assert_eq!(s.mode, Mode::Sample);
        }
    }

    #[test]
    fn stop_in_live_drops_to_sample_view() {
        // No Stop button in Live; this is the script/backend-only path.
        let mut s = state(Mode::Live, false, true);
        s.on_command(Command::Stop);
        assert_eq!(s.mode, Mode::Sample);
    }

    #[test]
    fn stop_in_sample_view_is_a_no_op() {
        let mut s = state(Mode::Sample, true, true);
        s.on_command(Command::Stop);
        assert_eq!(s.mode, Mode::Sample);
    }

    #[test]
    fn record_from_live_or_sample_view() {
        for mode in [Mode::Live, Mode::Sample] {
            let mut s = state(mode, false, true);
            s.rec_secs_done = 5.0;
            s.on_command(Command::Record);
            assert_eq!(s.mode, Mode::Recording);
            assert_eq!(s.rec_secs_done, 0.0);
        }
    }

    #[test]
    fn record_blocked_without_device_or_mid_activity() {
        for mode in [Mode::Live, Mode::Sample] {
            let mut s = state(mode, false, false);
            s.on_command(Command::Record);
            assert_eq!(s.mode, mode);
        }
        for mode in [Mode::Recording, Mode::Playback(Channel::Raw)] {
            let mut s = state(mode, true, true);
            s.on_command(Command::Record);
            assert_eq!(s.mode, mode);
        }
    }

    #[test]
    fn play_requires_sample_device_and_live_or_sample_view() {
        for (mode, ch) in [
            (Mode::Sample, Channel::Raw),
            (Mode::Live, Channel::Filtered),
        ] {
            let mut s = state(mode, true, true);
            s.play_pos = 9.0;
            s.on_command(Command::Play(ch));
            assert_eq!(s.mode, Mode::Playback(ch));
            assert_eq!(s.play_pos, 0.0);
        }
        // No sample.
        let mut s = state(Mode::Sample, false, true);
        s.on_command(Command::Play(Channel::Raw));
        assert_eq!(s.mode, Mode::Sample);
        // No device.
        let mut s = state(Mode::Live, true, false);
        s.on_command(Command::Play(Channel::Raw));
        assert_eq!(s.mode, Mode::Live);
        // Wrong modes.
        for mode in [Mode::Recording, Mode::Playback(Channel::Filtered)] {
            let mut s = state(mode, true, true);
            s.on_command(Command::Play(Channel::Raw));
            assert_eq!(s.mode, mode);
        }
    }

    #[test]
    fn retry_device_keeps_mode() {
        for mode in [
            Mode::Sample,
            Mode::Live,
            Mode::Recording,
            Mode::Playback(Channel::Raw),
        ] {
            let mut s = state(mode, true, true);
            s.on_command(Command::RetryDevice);
            assert_eq!(s.mode, mode);
        }
    }

    // -- frame handling ------------------------------------------------------

    #[test]
    fn record_progress_updates_while_recording() {
        let mut s = state(Mode::Recording, false, true);
        s.on_frame(&Frame::RecordProgress { secs_done: 3.5 });
        assert_eq!(s.rec_secs_done, 3.5);
    }

    #[test]
    fn record_done_sets_sample_and_returns_to_live() {
        let mut s = state(Mode::Recording, false, true);
        s.elapsed = 9.0;
        s.on_frame(&Frame::RecordDone);
        assert!(s.has_sample);
        assert_eq!(s.mode, Mode::Live);
        assert_eq!(s.elapsed, 0.0);
    }

    #[test]
    fn playback_progress_and_done() {
        let mut s = state(Mode::Playback(Channel::Filtered), true, true);
        s.on_frame(&Frame::PlaybackProgress { secs: 4.2 });
        assert_eq!(s.play_pos, 4.2);
        s.on_frame(&Frame::PlaybackDone);
        assert_eq!(s.mode, Mode::Sample);
        assert_eq!(s.play_pos, RECORD_SECS);
    }

    #[test]
    fn monitor_stopped_drops_live_and_recording_to_sample_view_only() {
        for mode in [Mode::Live, Mode::Recording] {
            let mut s = state(mode, false, true);
            s.on_frame(&Frame::MonitorStopped);
            assert_eq!(s.mode, Mode::Sample);
        }
        // Playback is unaffected (its monitors are already stopped).
        let mut s = state(Mode::Playback(Channel::Raw), true, true);
        s.on_frame(&Frame::MonitorStopped);
        assert_eq!(s.mode, Mode::Playback(Channel::Raw));
        // Sample view stays put too.
        let mut s = state(Mode::Sample, true, true);
        s.on_frame(&Frame::MonitorStopped);
        assert_eq!(s.mode, Mode::Sample);
    }

    #[test]
    fn metrics_frame_stored_in_any_mode() {
        let m = SampleMetrics {
            voice_measurable: true,
            background_reduction_db: 24.0,
            voice_retention_db: -0.4,
            ..SampleMetrics::default()
        };
        for mode in [Mode::Sample, Mode::Live, Mode::Recording] {
            let mut s = state(mode, true, true);
            s.on_frame(&Frame::Metrics(m));
            assert_eq!(s.metrics, Some(m));
        }
    }

    #[test]
    fn device_frame_sets_flag_and_name() {
        let mut s = state(Mode::Sample, false, false);
        s.on_frame(&Frame::Device {
            ok: true,
            name: "Blue Yeti".into(),
        });
        assert!(s.device_ok);
        assert_eq!(s.device_name, "Blue Yeti");
        // Recovery does not change the mode — the UI owns the auto-StartMonitor.
        assert_eq!(s.mode, Mode::Sample);
    }

    #[test]
    fn device_recovery_mid_review_keeps_sample_view() {
        let mut s = state(Mode::Sample, true, false);
        s.on_frame(&Frame::Device {
            ok: true,
            name: "Blue Yeti".into(),
        });
        assert_eq!(s.mode, Mode::Sample);
        assert!(s.has_sample);
    }

    #[test]
    fn device_lost_mid_recording_forces_sample_view() {
        let mut s = state(Mode::Recording, false, true);
        s.on_frame(&Frame::Device {
            ok: false,
            name: String::new(),
        });
        assert!(!s.device_ok);
        assert_eq!(s.mode, Mode::Sample);
        // Every transport control is disabled until the device retries back.
        let c = s.controls();
        assert!(!c.record && !c.play && !c.stop && !c.go_live);
    }

    #[test]
    fn device_lost_mid_playback_forces_sample_view() {
        let mut s = state(Mode::Playback(Channel::Raw), true, true);
        s.on_frame(&Frame::Device {
            ok: false,
            name: String::new(),
        });
        assert_eq!(s.mode, Mode::Sample);
        assert_eq!(s.playing(), None);
    }

    #[test]
    fn warn_frame_shows_verbatim_toast() {
        let mut s = WindowState::new();
        s.on_frame(&Frame::Warn("could not store the sample: disk full".into()));
        assert_eq!(
            s.toast,
            Some(("could not store the sample: disk full".into(), TOAST_SECS))
        );
    }

    #[test]
    fn spectrum_and_level_frames_do_not_touch_state() {
        let mut s = state(Mode::Live, true, true);
        s.on_frame(&Frame::Spectrum {
            ch: Channel::Raw,
            bins: vec![0.0; 64],
        });
        s.on_frame(&Frame::Level {
            raw_db: -20.0,
            filtered_db: -40.0,
        });
        assert_eq!(s.mode, Mode::Live);
        assert!(s.toast.is_none());
    }

    // -- stale-frame tolerance -----------------------------------------------

    #[test]
    fn stale_playback_progress_after_stop_ignored() {
        let mut s = state(Mode::Playback(Channel::Raw), true, true);
        s.on_command(Command::Stop);
        s.on_frame(&Frame::PlaybackProgress { secs: 7.7 });
        assert_eq!(s.mode, Mode::Sample);
        assert_eq!(s.play_pos, 0.0);
    }

    #[test]
    fn stale_playback_done_after_stop_ignored() {
        let mut s = state(Mode::Sample, true, true);
        s.play_pos = 3.0;
        s.on_frame(&Frame::PlaybackDone);
        assert_eq!(s.mode, Mode::Sample);
        assert_eq!(s.play_pos, 3.0);
    }

    #[test]
    fn stale_record_progress_outside_recording_ignored() {
        for mode in [Mode::Sample, Mode::Live, Mode::Playback(Channel::Raw)] {
            let mut s = state(mode, false, true);
            s.on_frame(&Frame::RecordProgress { secs_done: 5.0 });
            assert_eq!(s.rec_secs_done, 0.0);
            assert_eq!(s.mode, mode);
        }
    }

    #[test]
    fn record_done_after_cancel_still_lands_the_take() {
        // A cancel that races the completion: the backend already stored
        // the take (RecordDone is only emitted for stored takes), so the
        // sample MUST become playable — pretending it was cancelled would
        // desync the UI from the WAVs on disk.
        let mut s = state(Mode::Recording, false, true);
        s.on_command(Command::Stop);
        s.on_frame(&Frame::RecordDone);
        assert_eq!(s.mode, Mode::Live);
        assert!(s.has_sample);
    }

    #[test]
    fn record_started_reenters_recording_from_live_and_sample_view() {
        // Stale MonitorStopped knocked the optimistic Recording down; the
        // backend's authoritative RecordStarted must re-sync the view (the
        // mic is hot again).
        for mode in [Mode::Live, Mode::Sample] {
            let mut s = state(mode, true, true);
            s.rec_secs_done = 7.0;
            s.on_frame(&Frame::RecordStarted);
            assert_eq!(s.mode, Mode::Recording);
            assert_eq!(s.rec_secs_done, 0.0);
        }
        // Idempotent mid-recording: progress is not reset.
        let mut s = state(Mode::Recording, false, true);
        s.rec_secs_done = 3.0;
        s.on_frame(&Frame::RecordStarted);
        assert_eq!(s.mode, Mode::Recording);
        assert_eq!(s.rec_secs_done, 3.0);
        // Playback and a lost device are untouchable.
        let mut s = state(Mode::Playback(Channel::Raw), true, true);
        s.on_frame(&Frame::RecordStarted);
        assert_eq!(s.mode, Mode::Playback(Channel::Raw));
        let mut s = state(Mode::Sample, true, false);
        s.on_frame(&Frame::RecordStarted);
        assert_eq!(s.mode, Mode::Sample);
    }

    #[test]
    fn record_cancelled_returns_recording_to_live_only() {
        // Quick Record→Cancel: the optimistic cancel already went to Live,
        // then the raced RecordStarted re-entered Recording — the backend's
        // RecordCancelled resolves it back to Live (monitors still run).
        let mut s = state(Mode::Recording, false, true);
        s.on_frame(&Frame::RecordCancelled);
        assert_eq!(s.mode, Mode::Live);
        for mode in [Mode::Live, Mode::Sample, Mode::Playback(Channel::Raw)] {
            let mut s = state(mode, true, true);
            s.on_frame(&Frame::RecordCancelled);
            assert_eq!(s.mode, mode);
        }
    }

    // -- tick ----------------------------------------------------------------

    #[test]
    fn tick_advances_elapsed_only_in_live() {
        let mut s = state(Mode::Live, false, true);
        s.tick(0.5);
        s.tick(0.25);
        assert!((s.elapsed - 0.75).abs() < 1e-6);

        for mode in [Mode::Sample, Mode::Recording, Mode::Playback(Channel::Raw)] {
            let mut s = state(mode, true, true);
            s.tick(1.0);
            assert_eq!(s.elapsed, 0.0);
        }
    }

    #[test]
    fn toast_expires_via_tick() {
        let mut s = WindowState::new();
        s.on_frame(&Frame::Warn("hi".into()));
        s.tick(1.0);
        assert_eq!(s.toast, Some(("hi".into(), TOAST_SECS - 1.0)));
        s.tick(2.0);
        assert_eq!(s.toast, None);
    }

    #[test]
    fn toast_counts_down_in_any_mode() {
        let mut s = state(Mode::Recording, false, true);
        s.toast = Some(("t".into(), 0.5));
        s.tick(0.6);
        assert_eq!(s.toast, None);
    }

    // -- status --------------------------------------------------------------

    #[test]
    fn status_mapping() {
        assert_eq!(state(Mode::Live, false, true).status(), Status::Listening);
        assert_eq!(
            state(Mode::Recording, false, true).status(),
            Status::Recording
        );
        assert_eq!(
            state(Mode::Playback(Channel::Raw), true, true).status(),
            Status::Playback
        );
        // Sample view: Sample with a take on screen, Stopped without one.
        assert_eq!(state(Mode::Sample, true, true).status(), Status::Sample);
        assert_eq!(state(Mode::Sample, false, true).status(), Status::Stopped);
        // Device loss wins over every mode.
        for mode in [
            Mode::Sample,
            Mode::Live,
            Mode::Recording,
            Mode::Playback(Channel::Raw),
        ] {
            assert_eq!(state(mode, true, false).status(), Status::NoInput);
        }
    }

    // -- timeline strings ------------------------------------------------------

    #[test]
    fn timeline_input_unavailable() {
        let t = state(Mode::Live, true, false).timeline();
        assert_eq!(t.label, "Input unavailable");
        assert_pct(&t, 0.0);
        assert_eq!(t.time, "\u{2014}");
    }

    #[test]
    fn timeline_live_monitoring_shows_no_fill() {
        let mut s = state(Mode::Live, false, true);
        s.elapsed = 3.42;
        let t = s.timeline();
        assert_eq!(t.label, "Live monitoring");
        assert_pct(&t, 0.0);
        assert_eq!(t.time, "0:03.4");
    }

    #[test]
    fn timeline_live_never_wraps_past_ten_seconds() {
        let mut s = state(Mode::Live, false, true);
        s.elapsed = 12.34;
        let t = s.timeline();
        assert_pct(&t, 0.0);
        assert_eq!(t.time, "0:12.3");
    }

    #[test]
    fn timeline_live_shows_minutes() {
        let mut s = state(Mode::Live, false, true);
        s.elapsed = 63.4;
        let t = s.timeline();
        assert_eq!(t.time, "1:03.4");
    }

    #[test]
    fn timeline_recording() {
        let mut s = state(Mode::Recording, false, true);
        s.rec_secs_done = 6.0;
        let t = s.timeline();
        assert_eq!(t.label, "Recording sample \u{2014} 4.0 s left");
        assert_pct(&t, 60.0);
        assert_eq!(t.time, "0:06.0 / 0:10.0");
    }

    #[test]
    fn timeline_recording_overrun_clamps_to_zero_left() {
        let mut s = state(Mode::Recording, false, true);
        s.rec_secs_done = 10.4;
        let t = s.timeline();
        assert_eq!(t.label, "Recording sample \u{2014} 0.0 s left");
        assert_pct(&t, 100.0);
    }

    #[test]
    fn timeline_playback_raw() {
        let mut s = state(Mode::Playback(Channel::Raw), true, true);
        s.play_pos = 3.4;
        let t = s.timeline();
        assert_eq!(t.label, "Playing raw sample");
        assert_pct(&t, 34.0);
        assert_eq!(t.time, "0:03.4 / 0:10.0");
    }

    #[test]
    fn timeline_playback_filtered() {
        let mut s = state(Mode::Playback(Channel::Filtered), true, true);
        s.play_pos = 0.0;
        let t = s.timeline();
        assert_eq!(t.label, "Playing filtered sample");
        assert_pct(&t, 0.0);
        assert_eq!(t.time, "0:00.0 / 0:10.0");
    }

    #[test]
    fn timeline_sample_view_with_sample() {
        let t = state(Mode::Sample, true, true).timeline();
        assert_eq!(t.label, "Sample ready");
        assert_pct(&t, 100.0);
        assert_eq!(t.time, "0:10.0");
    }

    #[test]
    fn timeline_sample_view_without_sample() {
        let t = state(Mode::Sample, false, true).timeline();
        assert_eq!(t.label, "Not monitoring");
        assert_pct(&t, 0.0);
        assert_eq!(t.time, "\u{2014}");
    }

    #[test]
    fn fmt_time_rounds_on_the_decisecond() {
        assert_eq!(fmt_time(0.0), "0:00.0");
        assert_eq!(fmt_time(3.44), "0:03.4");
        assert_eq!(fmt_time(3.46), "0:03.5");
        assert_eq!(fmt_time(59.96), "1:00.0");
        assert_eq!(fmt_time(10.0), "0:10.0");
        assert_eq!(fmt_time(-1.0), "0:00.0");
    }

    // -- playing / stopped hint -------------------------------------------------

    #[test]
    fn playing_badge_channel() {
        assert_eq!(
            state(Mode::Playback(Channel::Raw), true, true).playing(),
            Some(Channel::Raw)
        );
        assert_eq!(
            state(Mode::Playback(Channel::Filtered), true, true).playing(),
            Some(Channel::Filtered)
        );
        for mode in [Mode::Sample, Mode::Live, Mode::Recording] {
            assert_eq!(state(mode, true, true).playing(), None);
        }
    }

    #[test]
    fn stopped_hint_only_in_sampleless_sample_view_with_device() {
        assert!(state(Mode::Sample, false, true).stopped_hint());
        assert!(!state(Mode::Sample, true, true).stopped_hint());
        assert!(!state(Mode::Sample, false, false).stopped_hint());
        assert!(!state(Mode::Live, false, true).stopped_hint());
    }

    // -- full user journey ------------------------------------------------------

    #[test]
    fn full_session_flow() {
        let mut s = WindowState::new();
        assert_eq!(s.mode, Mode::Sample);
        assert!(s.stopped_hint());
        // Window open → unconditional StartMonitor.
        s.on_command(Command::StartMonitor);
        assert_eq!(s.status(), Status::Listening);
        s.tick(2.0);
        s.on_command(Command::Record);
        s.on_frame(&Frame::RecordProgress { secs_done: 9.9 });
        s.on_frame(&Frame::RecordDone);
        s.on_frame(&Frame::Metrics(SampleMetrics::default()));
        assert_eq!(s.mode, Mode::Live);
        assert!(s.has_sample);
        assert!(s.metrics.is_some());
        s.on_command(Command::Play(Channel::Filtered));
        assert_eq!(s.playing(), Some(Channel::Filtered));
        s.on_frame(&Frame::PlaybackDone);
        // Playback ends in the sample view; the take stays reviewable.
        assert_eq!(s.mode, Mode::Sample);
        assert_eq!(s.status(), Status::Sample);
        assert!(!s.stopped_hint());
        assert_eq!(s.timeline().label, "Sample ready");
        // Replay straight from the sample view, stop early → back to it.
        s.on_command(Command::Play(Channel::Raw));
        assert_eq!(s.playing(), Some(Channel::Raw));
        s.on_command(Command::Stop);
        assert_eq!(s.mode, Mode::Sample);
        // Record a new take from the sample view, then cancel it: the take
        // is dropped, the monitors keep running → Live; the old sample
        // survives untouched.
        s.on_command(Command::Record);
        assert_eq!(s.mode, Mode::Recording);
        s.on_command(Command::Stop);
        assert_eq!(s.mode, Mode::Live);
        assert!(s.has_sample);
    }
}
