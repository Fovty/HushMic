//! The playhead of a player that feeds `pw-cat` through a pipe (Mic check's
//! replay, the Files page): what is heard now, on the audio's own clock,
//! for every frame the window paints.
//!
//! The feeder sees only how far `pw-cat` has read (the take position at the
//! pipe's read end). That moves in steps (pw-cat reads through a 4 KiB
//! stdio buffer, and only once per graph cycle), so it is followed by a
//! line at the stream's rate, nudged towards each reading: the playhead
//! moves smoothly between readings and never jumps by a read. A reading
//! that is off the line by more than a cycle and its lead (at least
//! [`REANCHOR`]: a stall, the graph changing its quantum) starts the line
//! over from it; a seek starts it over explicitly ([`PlayClock::reset`]).
//! What is shown never steps back between those starts: a line that runs
//! behind for a moment, or a latency figure that grows, holds the playhead
//! instead until the audio catches up.
//!
//! The reading leads what the stream hands on by half a cycle's worth on
//! average (each cycle is taken whole at its start) and what pw-cat's stdio
//! buffer holds ([`read_lead`]). From there the stream's port reports its
//! latency: a quantum (pw-cat fills its buffer outside the graph's cycle,
//! for the next one) plus the device's buffer. What is heard is the
//! line less that lead, less the stream's output latency: the graph's
//! quantum and the device's buffer behind it, as PipeWire reports it for
//! the stream's port ([`super::outlat`], measured while it plays). Until
//! the first measurement the latency is [`FALLBACK_LATENCY`], the 20 ms the
//! playhead used to subtract, and the quantum 1024: close at the usual
//! quanta. At a large one (4096 and up: the reading stands still for more
//! than [`GAP`] between cycles) that guess ran up to half a second ahead,
//! so a new stream's playhead holds at its start until the first figure
//! comes (at most [`HOLD_FOR`]); nothing is heard before then anyway. It
//! takes the reading moving for twice [`GAP`] without such a gap to tell a
//! usual quantum, so the playhead holds that long at least.
//!
//! Everything pw-cat has read is heard in the end: when it stops reading
//! (the end of a take, a stall), the line runs on until what was read has
//! come out of the device (read + latency), so the playhead reaches the
//! last frame when it is heard.

use std::sync::Mutex;
use std::time::Instant;

/// 48 kHz.
const RATE: f64 = 48_000.0;
/// A reading this far off the line starts it over, at least (50 ms; more
/// at a large quantum, whose reads swing half a cycle either way).
const REANCHOR: f64 = 2_400.0;
/// How far each reading pulls the line (the feeder reads every ~5 ms: a
/// time constant of about a quarter second).
const GAIN: f64 = 0.02;
/// The output latency until it is measured: pw-cat's `--latency 20ms`.
pub const FALLBACK_LATENCY: f64 = 960.0;
/// pw-cat's stdio buffer, in bytes.
const STDIO_BYTES: f64 = 4_096.0;
/// The reading standing still this long between moves (seconds) means a
/// quantum of about 4096 or more (2048: about 40 ms a cycle, less the
/// moves themselves).
const GAP: f64 = 0.05;
/// A new stream at a large quantum holds this long at most for its first
/// latency figure (pw-top took 0.3-0.5 s on a VM).
const HOLD_FOR: f64 = 1.5;

#[derive(Debug)]
struct State {
    /// The line: the read position (frames) at `at`.
    line: Option<(f64, Instant)>,
    /// The last reading: how far pw-cat has read.
    read: f64,
    /// Output latency behind the stream's port (frames).
    latency: f64,
    /// How far the reading leads the audio (frames): [`read_lead`].
    lead: f64,
    /// When the reading last moved.
    moved: Option<Instant>,
    /// The graph's quantum (frames), as last measured.
    quantum: f64,
    /// The playhead last shown since the line last started over (it
    /// never steps back in between).
    shown: Option<f64>,
    /// How often the line started over on its own (not a seek).
    reanchors: u64,
    /// The latency watch has handed over a figure.
    measured: bool,
    /// The reading stood still for more than [`GAP`] between moves.
    gappy: bool,
    /// Where the stream started (or was sought to), when it was first
    /// read, and when the reading first moved from there.
    start: f64,
    began: Option<Instant>,
    first_move: Option<Instant>,
}

impl State {
    /// How long the reading has stood still at `now` (seconds).
    fn gap(&self, now: Instant) -> f64 {
        self.moved
            .map_or(0.0, |m| now.saturating_duration_since(m).as_secs_f64())
    }
}

/// See the module docs. Shared by the feeder (writer) and the page.
#[derive(Debug)]
pub struct PlayClock {
    channels: usize,
    s: Mutex<State>,
}

impl PlayClock {
    pub fn new(channels: usize, read: u64) -> PlayClock {
        PlayClock {
            channels: channels.max(1),
            s: Mutex::new(State {
                line: None,
                read: read as f64,
                latency: FALLBACK_LATENCY,
                lead: read_lead(FALLBACK_QUANTUM, stdio_frames(channels)),
                moved: None,
                quantum: FALLBACK_QUANTUM,
                shown: None,
                reanchors: 0,
                measured: false,
                gappy: false,
                start: read as f64,
                began: None,
                first_move: None,
            }),
        }
    }

    /// The feeder's reading at `now`: pw-cat has read up to `read`.
    pub fn observe(&self, read: u64, now: Instant) {
        let mut s = self.s.lock().unwrap();
        let read = read as f64;
        s.began.get_or_insert(now);
        if read != s.read || s.moved.is_none() {
            if read != s.read && s.read > s.start && s.gap(now) > GAP {
                s.gappy = true;
            }
            if read > s.start && s.first_move.is_none() {
                s.first_move = Some(now);
            }
            s.moved = Some(now);
        }
        s.read = read;
        let obs = read - s.lead;
        // pw-cat reads once a cycle: a stall is a few cycles without one
        let stall = (4.0 * s.lead / RATE).max(0.1);
        let stalled = s
            .moved
            .is_some_and(|m| now.saturating_duration_since(m).as_secs_f64() > stall);
        // pw-cat's reads swing half a cycle either side of the line (a
        // whole quantum at the start of each cycle), plus the stdio buffer
        let reanchor = REANCHOR.max(s.quantum + s.lead);
        let latency = s.latency;
        s.line = match s.line {
            Some((pos, at)) => {
                let pred = pos + now.saturating_duration_since(at).as_secs_f64() * RATE;
                let err = obs - pred;
                if err.abs() > reanchor && !(stalled && err < 0.0) {
                    // (what is shown holds if the new line is behind it: a
                    // stream that resumes after a stall is heard from
                    // where it stopped once its first new cycle is through)
                    s.reanchors += 1;
                    Some((obs, now))
                } else if stalled {
                    // nothing read for a while (a stall, the end): what was
                    // read still comes out of the device, then the line
                    // holds
                    Some((pred.min(read + latency), now))
                } else {
                    // Every reading pulls, ahead or behind: over a cycle
                    // the line settles on the reading's average.
                    Some((pred + GAIN * err, now))
                }
            }
            None if read > 0.0 => Some((obs, now)),
            None => None,
        };
    }

    /// Start over at `read`: a new stream, or a seek (the feeder knows
    /// it seeks; the reading may lag behind the jump for a while).
    pub fn reset(&self, read: u64) {
        let mut s = self.s.lock().unwrap();
        s.line = None;
        s.moved = None;
        s.read = read as f64;
        s.shown = None;
        s.start = read as f64;
        s.first_move = None;
        s.gappy = false;
    }

    /// How often the line started over on its own (a stall, the quantum
    /// changing): the latency watch measures again then.
    pub fn reanchors(&self) -> u64 {
        self.s.lock().unwrap().reanchors
    }

    /// The measured output latency behind the stream's port and the
    /// graph's quantum, both in frames.
    pub fn set_latency(&self, latency: f64, quantum: Option<f64>) {
        let q = quantum.unwrap_or(FALLBACK_QUANTUM);
        let lead = read_lead(q, stdio_frames(self.channels));
        let mut s = self.s.lock().unwrap();
        s.quantum = q;
        // (a figure that moves what is heard back holds the playhead until
        // the audio catches up, rather than step it back: the first one at
        // a large quantum, a quantum change)
        s.latency = latency.max(0.0);
        s.measured = true;
        let shift = lead - s.lead;
        if shift.abs() > 0.5 {
            // the line follows the reading less the lead: move it along
            if let Some((pos, _)) = s.line.as_mut() {
                *pos -= shift;
            }
            s.lead = lead;
        }
    }

    /// What is heard at `now` (frames), never beyond what pw-cat has read
    /// and never back from what was shown since the line last started over.
    pub fn heard(&self, now: Instant) -> u64 {
        let mut s = self.s.lock().unwrap();
        let young = s
            .began
            .is_none_or(|b| now.saturating_duration_since(b).as_secs_f64() < HOLD_FOR);
        let large = s.gappy || (s.read > s.start && s.gap(now) > GAP);
        let usual = s
            .first_move
            .is_some_and(|f| now.saturating_duration_since(f).as_secs_f64() > 2.0 * GAP);
        if !s.measured && young && (large || !usual) {
            // no figure yet, and a large quantum or not yet known not to
            // be: hold (see the module docs)
            let held = s.shown.unwrap_or(s.start);
            s.shown = Some(held);
            return held as u64;
        }
        let played = match s.line {
            Some((pos, at)) => pos + now.saturating_duration_since(at).as_secs_f64() * RATE,
            None => s.read - s.lead,
        };
        // never past what pw-cat has read (it may have stopped reading)
        let heard = (played - s.latency).min(s.read).max(0.0);
        let heard = s.shown.map_or(heard, |shown| heard.max(shown));
        s.shown = Some(heard);
        heard as u64
    }

    /// The output latency now used (frames).
    pub fn latency(&self) -> f64 {
        self.s.lock().unwrap().latency
    }
}

/// The quantum until it is measured.
const FALLBACK_QUANTUM: f64 = 1024.0;

/// pw-cat's stdio buffer in frames of `channels` f32 samples.
fn stdio_frames(channels: usize) -> f64 {
    STDIO_BYTES / (4.0 * channels.max(1) as f64)
}

/// How far, on average, pw-cat's reading leads what its stream hands on:
/// it takes a whole `quantum` at the start of each cycle (half a quantum
/// ahead on average over the cycle), through a stdio buffer of `buf`
/// frames that is refilled a buffer at a time and bypassed for a request
/// of a buffer or more (glibc's fread). Worked out by running the reads
/// for a while: the pattern repeats, and an exact formula would have to
/// spell out every ratio of the two.
pub fn read_lead(quantum: f64, buf: f64) -> f64 {
    let (q, b) = (quantum.max(1.0).round() as u64, buf.max(1.0).round() as u64);
    let (mut held, mut read, mut taken) = (0u64, 0u64, 0u64);
    let mut sum = 0.0;
    const CYCLES: u64 = 4_096;
    for _ in 0..CYCLES {
        let mut need = q;
        let from_buf = held.min(need);
        held -= from_buf;
        need -= from_buf;
        if need >= b {
            read += need;
            need = 0;
        }
        if need > 0 {
            read += b;
            held = b - need;
        }
        taken += q;
        sum += (read - taken) as f64 + q as f64 / 2.0;
    }
    sum / CYCLES as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// pw-cat taking `q` frames at the start of each cycle through a
    /// `buf`-frame stdio buffer, polled every 5 ms for `secs` with the
    /// stream at `t·RATE`: the heard positions at each poll.
    fn run(clock: &PlayClock, start: Instant, secs: f64, q: u64, buf: u64) -> Vec<(f64, u64)> {
        run_with(clock, start, secs, q, buf, |_| {})
    }

    /// [`run`], with `at(t)` before each poll (the latency watch).
    fn run_with(
        clock: &PlayClock,
        start: Instant,
        secs: f64,
        q: u64,
        buf: u64,
        mut at: impl FnMut(f64),
    ) -> Vec<(f64, u64)> {
        let mut out = Vec::new();
        let (mut held, mut read, mut taken) = (0u64, 0u64, 0u64);
        let mut t = 0.0;
        while t < secs {
            // cycles started by now
            while (taken as f64) <= t * RATE {
                let mut need = q;
                let from_buf = held.min(need);
                held -= from_buf;
                need -= from_buf;
                if need >= buf {
                    read += need;
                    need = 0;
                }
                if need > 0 {
                    read += buf;
                    held = buf - need;
                }
                taken += q;
            }
            let now = start + Duration::from_secs_f64(t);
            at(t);
            clock.observe(read, now);
            out.push((t, clock.heard(now)));
            t += 0.005;
        }
        out
    }

    #[test]
    fn the_playhead_follows_the_audio_clock_without_the_reads_steps() {
        // 4096 and 8192: a forced quantum (crackling advice); the reads
        // swing half a cycle either side of the line, which must not start
        // it over each cycle
        for (ch, q) in [
            (1, 480),
            (1, 1024),
            (2, 2048),
            (1, 4096),
            (1, 8192),
            (2, 8192),
        ] {
            let clock = PlayClock::new(ch, 0);
            clock.set_latency(2_528.0, Some(q as f64));
            let start = Instant::now();
            let buf = stdio_frames(ch) as u64;
            let h = run(&clock, start, 6.0, q, buf);
            // once the line settles (a few cycles: longer at a large
            // quantum): within 5 ms of the audio clock less the latency,
            // and each 5 ms poll moves it by about 5 ms (240 frames), never
            // by a read
            let settled = 1.5_f64.max(16.0 * q as f64 / RATE);
            // no bias: the line settles on the audio clock on average
            let offs: Vec<f64> = h
                .iter()
                .filter(|x| x.0 > settled)
                .map(|&(t, v)| v as f64 - (t * RATE - 2_528.0))
                .collect();
            let mean = offs.iter().sum::<f64>() / offs.len() as f64;
            assert!(mean.abs() < 150.0, "{ch} ch, q {q}: mean {mean:.0}");
            assert_eq!(clock.reanchors(), 0, "{ch} ch, q {q}: never started over");
            for w in h.windows(2).filter(|w| w[0].0 > settled) {
                let (t, heard) = w[1];
                let want = t * RATE - 2_528.0;
                // (each cycle pulls the line half a quantum either way: at
                // 8192, a 170 ms cycle, that ripples it by up to ~11 ms,
                // under a pixel of the 10 s replay rule)
                let within = 240.0_f64.max(q as f64 / 12.0);
                assert!(
                    (heard as f64 - want).abs() < within,
                    "{ch} ch, q {q}, {t:.3}: {heard} vs {want}"
                );
                let step = w[1].1 as i64 - w[0].1 as i64;
                assert!(
                    (150..=330).contains(&step),
                    "{ch} ch, q {q}, {t:.3}: a step of {step}"
                );
            }
        }
    }

    #[test]
    fn a_jump_in_what_pw_cat_reads_starts_the_line_over() {
        let clock = PlayClock::new(1, 0);
        clock.set_latency(FALLBACK_LATENCY, None);
        let start = Instant::now();
        let t0 = start + Duration::from_secs(1);
        clock.observe(48_000, t0);
        clock.observe(48_240, t0 + Duration::from_millis(5));
        // a seek: the reading jumps 2 s ahead
        let t1 = t0 + Duration::from_millis(10);
        clock.observe(144_000, t1);
        let heard = clock.heard(t1) as f64;
        let want = 144_000.0 - read_lead(FALLBACK_QUANTUM, 1024.0) - FALLBACK_LATENCY;
        assert!((heard - want).abs() < 1.0, "{heard} vs {want}");
    }

    #[test]
    fn a_seek_starts_the_line_over_and_the_playhead_never_steps_back_otherwise() {
        let clock = PlayClock::new(1, 0);
        clock.set_latency(2_528.0, Some(1024.0));
        let start = Instant::now();
        let buf = stdio_frames(1) as u64;
        let h = run(&clock, start, 2.0, 1024, buf);
        for w in h.windows(2) {
            assert!(w[1].1 >= w[0].1, "{w:?}");
        }
        // a seek back to 0.5 s: the feeder says so, the reading lags
        let t = start + Duration::from_secs(2);
        clock.reset(24_000);
        clock.observe(24_000, t);
        let heard = clock.heard(t);
        assert!(heard <= 24_000, "{heard}");
        assert!(heard > 24_000 - 2_528 - 1_100, "{heard}");
        assert_eq!(clock.reanchors(), 0, "a seek is no start-over of its own");
    }

    #[test]
    fn the_end_is_heard_when_what_was_read_has_come_out() {
        let clock = PlayClock::new(1, 0);
        clock.set_latency(2_528.0, Some(1024.0));
        let start = Instant::now();
        let buf = stdio_frames(1) as u64;
        let h = run(&clock, start, 1.0, 1024, buf);
        let read = 48_000 + 1024;
        let last = h.last().unwrap().1;
        // pw-cat read everything: the line runs on until it is heard
        let mut t = start + Duration::from_secs(1);
        let mut prev = last;
        for _ in 0..100 {
            t += Duration::from_millis(5);
            clock.observe(read, t);
            let heard = clock.heard(t);
            assert!(heard >= prev && heard <= read, "{heard}");
            prev = heard;
        }
        assert_eq!(prev, read, "the playhead reaches the last frame");
    }

    #[test]
    fn a_stalled_stream_never_runs_past_what_was_read() {
        let clock = PlayClock::new(2, 0);
        clock.set_latency(0.0, Some(2048.0));
        let start = Instant::now();
        clock.observe(4_800, start);
        clock.observe(4_800, start + Duration::from_millis(5));
        let later = start + Duration::from_secs(2);
        assert_eq!(clock.heard(later), 4_800);
        // and the line was not dragged along: reading again resumes from it
        clock.observe(4_800 + 2048, later);
        assert!(clock.heard(later) <= 4_800 + 2048);
    }

    #[test]
    fn a_new_stream_holds_still_until_its_latency_is_known() {
        // the VM run at a forced 4096 and 8192: the playhead ran up to
        // 466 ms ahead for the first half second, then stepped back
        for (ch, q, latency) in [
            (1, 8192u64, 18_000.0),
            (2, 4096, 9_500.0),
            (1, 1024, 2_528.0),
            (1, 480, 2_450.0),
        ] {
            let clock = PlayClock::new(ch, 0);
            let start = Instant::now();
            let buf = stdio_frames(ch) as u64;
            let mut told = false;
            let h = run_with(&clock, start, 4.0, q, buf, |t| {
                if t >= 0.4 && !told {
                    told = true;
                    clock.set_latency(latency, Some(q as f64));
                }
            });
            // at a large quantum nothing moves before the figure; at a usual
            // one, the first 0.1 s (it takes that to tell)
            let until = if q >= 4096 { 0.4 } else { 2.0 * GAP };
            for &(t, heard) in h.iter().filter(|x| x.0 < until - 0.001) {
                assert_eq!(heard, 0, "{ch} ch, q {q}, {t:.3}");
            }
            for w in h.windows(2) {
                assert!(w[1].1 >= w[0].1, "{ch} ch, q {q}: back at {:?}", w[1]);
            }
            // never far ahead of the audio: at the start, by the guessed
            // 20 ms and a little at a usual quantum (50 ms), or by half a
            // cycle at a large one (the line, started over on each cycle
            // while the quantum was not known, sits at the top of the
            // reads' swing for a moment)
            let within = 240.0_f64.max(q as f64 / 12.0);
            for &(t, heard) in &h {
                let want = (t * RATE - latency).max(0.0);
                let ahead = if q >= 4096 { q as f64 / 2.0 } else { 2_400.0 };
                assert!(
                    heard as f64 - want < within + ahead,
                    "{ch} ch, q {q}, {t:.3}: {heard} vs {want}"
                );
            }
            let settled = 1.5_f64.max(16.0 * q as f64 / RATE);
            for &(t, heard) in h.iter().filter(|x| x.0 > settled) {
                let want = t * RATE - latency;
                assert!(
                    (heard as f64 - want).abs() < within,
                    "{ch} ch, q {q}, {t:.3}: {heard} vs {want}"
                );
            }
        }
    }

    #[test]
    fn without_a_figure_the_playhead_moves_after_a_while() {
        // pw-top missing or failing: the fallback, at a usual quantum soon,
        // at a large one after HOLD_FOR
        for (q, from) in [(1024u64, 2.0 * GAP), (8192, HOLD_FOR)] {
            let clock = PlayClock::new(1, 0);
            let start = Instant::now();
            let h = run(&clock, start, 2.5, q, 1024);
            assert!(
                h.iter().filter(|x| x.0 < from - 0.001).all(|x| x.1 == 0),
                "q {q}"
            );
            assert!(h.last().unwrap().1 > 48_000, "q {q}: {:?}", h.last());
        }
    }

    #[test]
    fn a_seek_after_the_figure_does_not_hold() {
        let clock = PlayClock::new(1, 0);
        let start = Instant::now();
        run(&clock, start, 1.0, 1024, 1024);
        clock.set_latency(2_528.0, Some(1024.0));
        let t = start + Duration::from_secs(1);
        clock.reset(240_000);
        clock.observe(240_000 + 1024, t);
        let heard = clock.heard(t + Duration::from_millis(5));
        assert!(
            heard > 240_000 - 2_528 - 1_100 && heard <= 240_000 + 1024,
            "{heard}"
        );
    }

    #[test]
    fn the_lead_is_half_a_cycle_plus_what_stdio_holds() {
        assert_eq!(read_lead(1024.0, 1024.0), 512.0);
        assert_eq!(read_lead(2048.0, 1024.0), 1024.0);
        assert_eq!(read_lead(256.0, 1024.0), 512.0);
        assert_eq!(read_lead(1024.0, 512.0), 512.0);
        // 480 does not divide 1024: more than half a buffer on average
        let l = read_lead(480.0, 1024.0);
        assert!((700.0..770.0).contains(&l), "{l}");
    }

    #[test]
    fn the_latency_is_taken_off_what_is_heard() {
        let start = Instant::now();
        let at = |latency: f64| {
            let clock = PlayClock::new(1, 0);
            clock.set_latency(latency, None);
            clock.observe(96_000, start);
            (clock.heard(start), clock)
        };
        let (before, clock) = at(FALLBACK_LATENCY);
        assert_eq!(before - at(FALLBACK_LATENCY + 4_800.0).0, 4_800);
        // a figure that grows holds the playhead until the audio catches
        // up, rather than step it back
        clock.set_latency(FALLBACK_LATENCY + 4_800.0, None);
        assert_eq!(clock.latency(), FALLBACK_LATENCY + 4_800.0);
        assert_eq!(clock.heard(start), before);
        let later = start + Duration::from_millis(200);
        clock.observe(96_000 + 9_600, later);
        assert_eq!(clock.heard(later), before + 9_600 - 4_800);
    }
}
