//! The real source: the daemon's file jobs (`AppState.files`), its
//! actions, and the window's own player (`files::player`), mapped onto the
//! page's [`Source`]. The daemon cleans; the window plays the cleaned
//! sound mixed with the original, and reads the picture of the file it
//! shows from that job's cache.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use super::desktop::{self, Pick};
use super::intake::{self, Intake};
use super::model::{
    Cmd, Estimate, FileId, Kind, Pass, Place, Play, Reason, Row, Save, Source, Strength, View,
};
use crate::appstate::{Action, AppState, FileJob, JobState};
use crate::files::picture::Sheet;
use crate::files::player::{FilePlayer, FileSource};
use crate::files::worker::Problem;
use crate::miccheck::player::PlayerState;
use crate::miccheck::Repaint;

/// 48 kHz frames per second.
const RATE: f64 = 48_000.0;
/// The picture is read again at most this often while the pass fills it.
const PICTURE_EVERY: Duration = Duration::from_millis(200);
/// The pass's speed is judged over this much of its recent progress.
const SPEED_WINDOW: Duration = Duration::from_secs(8);
/// The time left shown follows a new reading this slowly (seconds): a
/// pass's speed wobbles, the estimate should not (F8).
const ETA_EASE: f64 = 20.0;
/// The page's strength waits this long for the daemon to report it.
const STRENGTH_WAIT: Duration = Duration::from_secs(3);

/// A player starting on its own thread (opening a file can take a while).
type Starting = mpsc::Receiver<Result<FilePlayer, String>>;

/// A strength on its way to the daemon: the file's (`id`), else the
/// page's.
#[derive(Clone, Debug)]
struct Choice {
    id: Option<FileId>,
    strength: Strength,
    /// The daemon's word before the first of these clicks.
    was: Option<f32>,
    /// Every cap sent since: the daemon's answers to the earlier clicks
    /// are on the way, not a move elsewhere.
    sent: Vec<f32>,
    /// The last click.
    at: Instant,
}

/// What the chooser was opened for.
#[derive(Clone, Debug)]
enum Choosing {
    Add,
    /// "Choose it again…": the file found again keeps its row.
    Again(FileId),
    /// "Save elsewhere…": a folder.
    Folder(FileId),
    /// In a sandbox the first Save asks once where cleaned files go; these
    /// files save there once it is chosen.
    Place(Vec<FileId>),
}

pub struct DaemonFiles {
    wake: Repaint,
    jobs: Vec<FileJob>,
    yielding: bool,
    /// Save can't go next to the originals (a sandbox) and no folder is
    /// chosen yet.
    ask_place: bool,
    /// Saves asked while another chooser was up: they ask where once it
    /// closes.
    place_next: Vec<FileId>,
    /// Saves sent into the chosen folder that the daemon has not started
    /// yet (with what the file had saved then): the daemon looks at the
    /// folder first, and if it is gone they ask where.
    sent: Vec<(FileId, Option<String>)>,
    /// Saves whose folder went while they were written (a sandbox): asked
    /// where once each.
    asked_gone: Vec<FileId>,
    /// The daemon's strength (a dB cap), and the page's own choice until
    /// the daemon reports it.
    daemon_db: Option<f32>,
    strength: Option<Choice>,
    /// The strength's share of the original the player was last given.
    alpha_sent: Option<f32>,
    selected: Option<FileId>,
    /// The playhead while nothing plays (seconds).
    pos: f64,
    player: Option<FilePlayer>,
    starting: Option<Starting>,
    /// The frame the player was started from.
    started_at: u64,
    hold: bool,
    /// Playing only while the original is held.
    peek: bool,
    /// The selected file's picture (no other file's is held), when it was
    /// last read, and the job's state then: it is read again while the
    /// pass fills it, and once whenever the state moves.
    sheet: Option<Arc<Sheet>>,
    sheet_at: Option<Instant>,
    sheet_state: Option<JobState>,
    /// Jobs seen so far: one that appears later was just added (it pours).
    seen: HashSet<FileId>,
    /// Files that need attention and are no longer where they were.
    gone: HashSet<FileId>,
    first_state: bool,
    /// Each cleaning job's recent progress: (when on the page's clock,
    /// frames cleaned).
    progress: HashMap<FileId, Vec<(f64, u64)>>,
    /// Each cleaning job's time left as last worked out: (seconds, when on
    /// the page's clock).
    eta: HashMap<FileId, (f64, f64)>,
    out: Vec<Action>,
    choosing: Option<(mpsc::Receiver<Vec<PathBuf>>, Choosing)>,
    /// The output being prepared for a Save: since when, and how far then.
    preparing: Option<(FileId, Instant, f32)>,
    /// Clear all took this many files a moment ago (the daemon's word),
    /// and the file that was selected then: Undo selects it again; or a
    /// row's × took the one named.
    cleared: usize,
    cleared_name: Option<String>,
    /// The app whose call cleaning waits for (the first using HushMic).
    yield_app: Option<String>,
    next_up: Option<FileId>,
    quitting: Option<usize>,
    cleared_sel: Option<FileId>,
    /// Drops and choices being sorted ([`super::intake`]), off the frame.
    intake: (mpsc::Sender<Intake>, mpsc::Receiver<Intake>),
    note: Option<(String, u64)>,
    serial: u64,
    now: f64,
}

impl DaemonFiles {
    pub fn new(wake: Repaint) -> DaemonFiles {
        DaemonFiles {
            wake,
            jobs: Vec::new(),
            yielding: false,
            ask_place: false,
            place_next: Vec::new(),
            sent: Vec::new(),
            asked_gone: Vec::new(),
            daemon_db: None,
            strength: None,
            alpha_sent: None,
            selected: None,
            pos: 0.0,
            player: None,
            starting: None,
            started_at: 0,
            hold: false,
            peek: false,
            sheet: None,
            sheet_at: None,
            sheet_state: None,
            seen: HashSet::new(),
            gone: HashSet::new(),
            first_state: true,
            progress: HashMap::new(),
            eta: HashMap::new(),
            out: Vec::new(),
            choosing: None,
            preparing: None,
            cleared: 0,
            cleared_name: None,
            yield_app: None,
            next_up: None,
            quitting: None,
            cleared_sel: None,
            intake: mpsc::channel(),
            note: None,
            serial: 0,
            now: 0.0,
        }
    }

    /// Sort what was dropped or chosen (off the frame): the recordings
    /// are added, the rest noted.
    fn take_in(&mut self, paths: Vec<PathBuf>) {
        let have: Vec<PathBuf> = self.jobs.iter().map(|j| PathBuf::from(&j.path)).collect();
        let (tx, wake) = (self.intake.0.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let _ = tx.send(intake::sort(paths, &have));
            wake(Duration::ZERO);
        });
    }

    /// Sorted drops: added, noted, and one already on the page selected.
    fn took_in(&mut self) {
        while let Ok(i) = self.intake.1.try_recv() {
            if let Some(w) = i.words() {
                let n = self.note.as_ref().map_or(1, |n| n.1 + 1);
                self.note = Some((w, n));
            }
            if i.add.is_empty() {
                let found = i.already.first().and_then(|p| {
                    let p = p.to_string_lossy();
                    self.jobs.iter().find(|j| j.path == p).map(|j| j.id)
                });
                if let Some(id) = found.filter(|id| self.selected != Some(*id)) {
                    self.select(Some(id));
                }
            }
            self.out.extend(i.add.into_iter().map(Action::FilesAdd));
        }
    }

    fn job(&self, id: FileId) -> Option<&FileJob> {
        self.jobs.iter().find(|j| j.id == id)
    }

    fn selected_job(&self) -> Option<&FileJob> {
        self.job(self.selected?)
    }

    /// The page's strength: the selected file's, else what files added
    /// next get (the page's own choice until the daemon has it, else the
    /// daemon's word).
    fn strength(&self) -> Strength {
        step_of(self.strength_db(self.selected_job()))
    }

    /// The dB cap of file `j` (None: the page's for files added next).
    fn strength_db(&self, j: Option<&FileJob>) -> f32 {
        let id = j.map(|j| j.id);
        if let Some(c) = self.strength.as_ref().filter(|c| c.id == id) {
            return db_of(c.strength);
        }
        match j {
            Some(j) if j.db > 0.0 => j.db,
            _ => self.daemon_db.unwrap_or(crate::files::jobs::DEFAULT_DB),
        }
    }

    /// The daemon's word on the strength `c` is for.
    fn daemon_word(&self, c: &Choice) -> Option<f32> {
        match c.id {
            Some(id) => self.job(id).map(|j| j.db).filter(|&d| d > 0.0),
            None => self.daemon_db,
        }
    }

    /// Saved, at another strength than the file's own now: that one.
    fn saved_at_another(&self, j: &FileJob) -> Option<Strength> {
        j.saved.as_ref()?;
        let at = step_of(j.saved_db?);
        (at != step_of(self.strength_db(Some(j)))).then_some(at)
    }

    /// The original's share the player mixes in (the plugin's own blend).
    fn alpha(&self) -> f32 {
        crate::files::mix::alpha(self.strength_db(self.selected_job()))
    }

    /// Give the player the strength's mix when it changed.
    fn mix_strength(&mut self) {
        let a = self.alpha();
        if let Some(p) = &self.player {
            if self.alpha_sent != Some(a) {
                p.set_strength(a);
                self.alpha_sent = Some(a);
            }
        }
    }

    fn position(&self) -> f64 {
        match &self.player {
            Some(p) => p.position() as f64 / RATE,
            None => self.pos,
        }
    }

    /// Save these files: in a sandbox with no folder yet, once one is
    /// chosen (asked once; a Save meanwhile joins the question).
    fn save(&mut self, ids: Vec<FileId>) {
        if !self.ask_place {
            for &id in &ids {
                let saved = self.job(id).and_then(|j| j.saved.clone());
                self.sent.retain(|(s, _)| *s != id);
                self.sent.push((id, saved));
            }
            self.out.extend(ids.into_iter().map(Action::FilesSave));
            return;
        }
        match &mut self.choosing {
            Some((_, Choosing::Place(waiting))) => waiting.extend(ids),
            // another chooser is up (the portal's dialog is not modal to
            // the window): the question comes when it closes
            Some(_) => self.place_next.extend(ids),
            None => self.choose(Choosing::Place(ids)),
        }
    }

    /// Open the chooser (one at a time).
    fn choose(&mut self, what: Choosing) {
        if self.choosing.is_some() {
            return;
        }
        let (title, pick) = match what {
            Choosing::Add => (crate::tr!("files-choose"), Pick::Recordings),
            Choosing::Again(_) => (crate::tr!("files-choose-again"), Pick::Recording),
            Choosing::Folder(_) => (crate::tr!("files-save-elsewhere"), Pick::Folder),
            Choosing::Place(_) => (crate::tr!("files-choose-folder"), Pick::Folder),
        };
        let rx = desktop::choose(title, pick, self.wake.clone());
        self.choosing = Some((rx, what));
    }

    fn select(&mut self, id: Option<FileId>) {
        self.stop();
        self.selected = id;
        self.pos = 0.0;
        self.hold = false;
        self.sheet = None;
        self.sheet_at = None;
        self.sheet_state = None;
        self.out.push(Action::UiFile(id.map(|i| (i, 0))));
    }

    fn stop(&mut self) {
        if let Some(mut p) = self.player.take() {
            self.pos = p.position() as f64 / RATE;
            p.stop();
        }
        self.starting = None;
        self.peek = false;
    }

    /// Start playing the selected file from the playhead (a peek: only
    /// while the original is held).
    fn play(&mut self, peek: bool) {
        let Some(job) = self.selected_job().cloned() else {
            return;
        };
        if self.player.is_some() || self.starting.is_some() {
            self.peek = peek && self.peek;
            return;
        }
        // at the end: from the start again
        let len = job.frames.unwrap_or(0) as f64 / RATE;
        if len > 0.0 && self.pos >= len - 1e-3 {
            self.pos = 0.0;
        }
        let at = (self.pos * RATE) as u64;
        self.started_at = at;
        let (alpha, hold) = (self.alpha(), self.hold);
        let (id, wake) = (job.id, self.wake.clone());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let ffmpeg = job
                .ffmpeg
                .then(crate::files::ffmpeg::Ffmpeg::find)
                .flatten()
                .map(|f| f.ffmpeg);
            let started = FileSource::open(&job, ffmpeg.as_deref(), at, alpha).map(|s| {
                let p = FilePlayer::start(s);
                p.hold(hold);
                p
            });
            let _ = tx.send(started);
            wake(Duration::ZERO);
        });
        self.starting = Some(rx);
        self.peek = peek;
        self.out.push(Action::FilesPlay(id, at));
    }

    /// The player that was starting, once it has; the end of the file.
    fn tend_player(&mut self) {
        if let Some(rx) = &self.starting {
            match rx.try_recv() {
                Ok(Ok(p)) => {
                    if let Some(j) = self.selected_job() {
                        p.set_lit(j);
                    }
                    // what changed while it opened: the hold, the strength,
                    // a seek
                    p.hold(self.hold);
                    let a = self.alpha();
                    p.set_strength(a);
                    self.alpha_sent = Some(a);
                    let at = (self.pos * RATE) as u64;
                    if at != self.started_at {
                        p.seek(at);
                    }
                    self.player = Some(p);
                    self.starting = None;
                }
                Ok(Err(e)) => {
                    eprintln!("[hushmic] file playback: {e}");
                    self.starting = None;
                    self.peek = false;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.starting = None,
            }
        }
        let ended = self
            .player
            .as_ref()
            .map(|p| p.poll())
            .filter(|s| *s != PlayerState::Playing);
        if let Some(state) = ended {
            if let PlayerState::Failed { .. } = state {
                eprintln!("[hushmic] file playback stopped: no sound output");
            }
            self.stop();
        }
    }

    /// The selected file's picture, from its cache folder. A file that
    /// is not shown holds none, however many are in the list.
    fn read_picture(&mut self) {
        let Some(job) = self.selected_job() else {
            return;
        };
        if job.cache.is_empty() {
            return;
        }
        let state = job.state;
        let due = self.sheet_at.is_none_or(|t| t.elapsed() >= PICTURE_EVERY);
        let filling = state == JobState::Cleaning && due;
        if self.sheet_state == Some(state) && !filling {
            return;
        }
        let dir = PathBuf::from(&job.cache);
        self.sheet_at = Some(Instant::now());
        self.sheet_state = Some(state);
        if let Ok(Some(sheet)) = Sheet::read(&dir) {
            if self.sheet.as_deref() != Some(&sheet) {
                self.sheet = Some(Arc::new(sheet));
            }
        }
    }

    /// The pass has cleaned nothing for a whole speed window.
    fn stalled(&self, job: &FileJob) -> bool {
        job.state == JobState::Cleaning
            && !self.yielding
            && self
                .progress
                .get(&job.id)
                .and_then(|p| p.last())
                .is_some_and(|&(t1, _)| self.now - t1 > SPEED_WINDOW.as_secs_f64())
    }

    /// Seconds until the pass is done: at its speed up to now, eased from
    /// the last estimate (counted down since) so that it does not jump
    /// with every wobble of the speed. A pass that moved none for a whole
    /// window has no estimate: its worker runs at idle priority and gets
    /// no CPU while the machine is busy, and its last speed says nothing
    /// about when it ends.
    fn time_left(&self, job: &FileJob) -> Option<f64> {
        let raw = self.raw_left(job)?;
        Some(match self.eta.get(&job.id) {
            Some(&(was, at)) => {
                let dt = (self.now - at).max(0.0);
                let counted = (was - dt).max(0.0);
                counted + (raw - counted) * (1.0 - (-dt / ETA_EASE).exp())
            }
            None => raw,
        })
    }

    /// Seconds until the pass is done at its speed up to now.
    fn raw_left(&self, job: &FileJob) -> Option<f64> {
        if job.state != JobState::Cleaning || self.yielding {
            return None;
        }
        let speed = self.speed(job)?;
        let left = job.frames?.saturating_sub(cleaned(job)) as f64;
        Some(left / speed)
    }

    /// The pass's speed up to now, in frames a second: none before 3 s of
    /// it, nor while it stalls.
    fn speed(&self, job: &FileJob) -> Option<f64> {
        if job.state != JobState::Cleaning || self.yielding || self.stalled(job) {
            return None;
        }
        let p = self.progress.get(&job.id)?;
        let (&(t0, a), &(_, b)) = (p.first()?, p.last()?);
        let secs = self.now - t0;
        if secs < 3.0 || b <= a {
            return None;
        }
        Some((b - a) as f64 / secs)
    }

    /// Seconds until a Save's output is prepared: judged from how far it
    /// came since it began, else a re-encode's usual 40 times real time.
    fn save_left(&self, j: &FileJob, length: f64) -> f64 {
        let Some(f) = j.preparing else {
            return 0.0;
        };
        match self.preparing {
            Some((id, t0, f0)) if id == j.id && f > f0 + 0.02 => {
                t0.elapsed().as_secs_f64() / f64::from(f - f0) * f64::from(1.0 - f)
            }
            _ => (length / 40.0 * f64::from(1.0 - f)).max(4.0),
        }
    }

    fn row(&self, j: &FileJob) -> Row {
        let path = Path::new(&j.path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| j.path.clone());
        let length = j.frames.unwrap_or(0) as f64 / RATE;
        let save = match (&j.saved, j.saving, j.state) {
            // the output is prepared at the page's strength, then linked or
            // copied into place (prepared already: about done); also a
            // Save again at another strength
            (_, true, JobState::Done) => Save::Writing {
                fraction: j.preparing.unwrap_or(1.0),
                secs_left: self.save_left(j, length),
            },
            (_, true, _) => Save::WhenCleaned,
            (Some(_), _, _) => match self.saved_at_another(j) {
                Some(was) => Save::Stale {
                    was,
                    now: step_of(self.strength_db(Some(j))),
                },
                None => Save::Saved,
            },
            _ => Save::No,
        };
        let pass = match j.state {
            JobState::NeedsAttention => Pass::Attention(reason(j, self.gone.contains(&j.id))),
            // the pass is done; its Save failed: never silent
            _ if j.save_problem.is_some_and(|p| p != Problem::FolderGone) => {
                Pass::Attention(save_reason(j))
            }
            // the file stops early: its first part, once the user says so
            JobState::Done if j.damaged.is_some() && !j.damage_ok => {
                Pass::Attention(Reason::Damaged)
            }
            JobState::Done => Pass::Cleaned,
            JobState::Cleaning if self.yielding => Pass::Yielded,
            JobState::Cleaning => Pass::Cleaning,
            JobState::Waiting => Pass::Waiting,
        };
        Row {
            id: j.id,
            name,
            length,
            estimate: if j.exact || j.frames.is_none() {
                Estimate::Exact
            } else {
                Estimate::Close
            },
            damaged: j.damaged.map(|d| d as f64 / RATE),
            stated: j.stated.map(|d| d as f64 / RATE),
            // by its extension until it opened (its icon at once)
            kind: match (j.rate, crate::files::kinds::named(path)) {
                (0, crate::files::kinds::Named::Video) => Kind::Video,
                _ if j.video => Kind::Video,
                (0, crate::files::kinds::Named::Other) => Kind::Other,
                _ => Kind::Audio,
            },
            pass,
            segments: if j.state == JobState::Done {
                vec![(0.0, length)]
            } else {
                j.lit
                    .iter()
                    .map(|&(a, b)| (a as f64 / RATE, b as f64 / RATE))
                    .collect()
            },
            output: output_name(
                path,
                &j.out_ext,
                j.saved
                    .as_ref()
                    .and_then(|_| {
                        crate::files::save::resave_mark(j.saved_db, self.strength_db(Some(j)))
                    })
                    .as_deref(),
            ),
            saved_name: j
                .saved
                .as_deref()
                .and_then(|s| Path::new(s).file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            save,
            time_left: self.time_left(j),
            stalled: self.stalled(j),
            reduced: j.reduced,
            tracks: j.tracks.max(1),
            dropped: j.dropped,
            track: j.track,
            titles: j.titles.clone(),
            strength: step_of(self.strength_db(Some(j))),
        }
    }
}

/// The four strengths as the daemon's dB caps.
const CAPS: [f32; 4] = [6.0, 12.0, 24.0, 100.0];

fn db_of(s: Strength) -> f32 {
    CAPS[s.index()]
}

/// The step nearest a dB cap (a custom cap from the Settings or the
/// command line reads as the step closest to it).
fn step_of(db: f32) -> Strength {
    let i = CAPS
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 - db).abs().total_cmp(&(b.1 - db).abs()))
        .map_or(3, |(i, _)| i);
    Strength::ALL[i]
}

/// Frames cleaned so far.
fn cleaned(j: &FileJob) -> u64 {
    j.lit.iter().map(|(a, b)| b - a).sum()
}

/// Why a job's Save failed, in the page's words (a gone folder is not
/// one: the Save falls back, or the page asks where).
fn save_reason(j: &FileJob) -> Reason {
    match j.save_problem {
        Some(Problem::NoSpace) => Reason::NoSpace,
        Some(Problem::Moved) => Reason::Moved { at_save: true },
        Some(Problem::NoWrite) => Reason::NoWrite {
            folder: j
                .save_detail
                .as_deref()
                .map(|d| {
                    Path::new(d)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| d.to_string())
                })
                .unwrap_or_default(),
        },
        Some(Problem::Encode) => Reason::SaveEncode {
            format: j.out_ext.to_uppercase(),
            why: j.save_detail.clone().filter(|w| !w.is_empty()),
        },
        Some(Problem::Unreadable) => Reason::SaveRead,
        _ => Reason::SaveFailed {
            why: j.save_detail.clone().filter(|w| !w.is_empty()),
        },
    }
}

/// Why a job needs attention, in the page's words. `gone`: its file is no
/// longer where it was (moved or deleted while it was cleaned).
fn reason(j: &FileJob, gone: bool) -> Reason {
    let path = Path::new(&j.path);
    if gone {
        return Reason::Moved { at_save: false };
    }
    match j.problem {
        Some(Problem::NoSound) => Reason::NoSoundTrack,
        Some(Problem::Unsupported) => Reason::Unsupported {
            ext: path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
        },
        Some(Problem::Unreadable) => Reason::Unreadable,
        Some(Problem::Incomplete) => Reason::Incomplete,
        Some(Problem::NotMedia) => Reason::NotMedia,
        Some(Problem::Empty) => Reason::Empty,
        Some(Problem::Engine) => Reason::Engine,
        Some(Problem::Cache) => Reason::Cache { bytes: None },
        // the daemon's check of the cache's free space
        Some(Problem::NoSpace) => Reason::Cache {
            bytes: j
                .frames
                .map(|f| crate::files::cache::bytes_needed(f, j.channels.max(1))),
        },
        Some(Problem::Failed) => Reason::Failed {
            again: j.stops >= 2,
        },
        Some(Problem::Crashed) => Reason::Crashed {
            again: j.stops >= 2,
        },
        Some(Problem::Moved) => Reason::Moved { at_save: false },
        // a Save's alone, never a pass's
        Some(Problem::FolderGone | Problem::NoWrite | Problem::Encode) => {
            Reason::Failed { again: false }
        }
        None if j.damaged.is_some() => Reason::Damaged,
        None => Reason::Failed {
            again: j.stops >= 2,
        },
    }
}

/// The folder a folder chooser answered: a chooser too old for folders
/// gives a file in it.
pub(crate) fn folder_of(picked: &Path) -> PathBuf {
    match picked.parent() {
        Some(p) if picked.is_file() => p.to_path_buf(),
        _ => picked.to_path_buf(),
    }
}

/// "interview (cleaned).m4a": the extension Save writes.
fn output_name(path: &Path, ext: &str, mark: Option<&str>) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = if ext.is_empty() {
        path.extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        ext.to_owned()
    };
    crate::files::save::cleaned_name(&stem, &ext, mark, 1)
}

/// Where a saved file went: next to its original, or a folder by its last
/// two names ("Recordings/Cleaned").
fn place(j: Option<&FileJob>) -> Place {
    let Some((orig, saved)) = j.and_then(|j| Some((Path::new(&j.path), j.saved.as_deref()?)))
    else {
        return Place::NextToOriginal;
    };
    let dir = Path::new(saved).parent();
    if dir == orig.parent() {
        return match j.and_then(|j| j.folder_miss) {
            Some(miss) => Place::Instead(miss),
            None => Place::NextToOriginal,
        };
    }
    Place::Folder(dir.map(folder_shown).unwrap_or_default())
}

/// A folder as the page and Settings › Files show it: by its last two
/// names; one the document portal handed over (a sandbox) by where it is
/// on the host, or by its own name until the portal says or if it cannot
/// (never by the portal's ID).
pub(crate) fn folder_shown(dir: &Path) -> String {
    use crate::files::docpath;
    if docpath::doc_id(dir).is_none() {
        return folder_words(dir);
    }
    match docpath::host_path(dir) {
        Some(host) => folder_words(&host),
        None => dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| folder_words(dir)),
    }
}

/// A folder by its last two names ("Recordings/Cleaned"), as the saved
/// state and Settings › Files show it.
pub(crate) fn folder_words(dir: &Path) -> String {
    let names: Vec<String> = dir
        .components()
        .rev()
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .take(2)
        .collect();
    if names.is_empty() {
        return dir.display().to_string();
    }
    names.into_iter().rev().collect::<Vec<_>>().join("/")
}

impl Source for DaemonFiles {
    fn state(&mut self, s: &AppState, now: f64) {
        self.now = now;
        let jobs = &s.files.jobs;
        let first = std::mem::take(&mut self.first_state);
        let fresh: Vec<FileId> = jobs
            .iter()
            .map(|j| j.id)
            .filter(|id| !self.seen.contains(id))
            .collect();
        for id in &fresh {
            self.seen.insert(*id);
        }
        if *jobs != self.jobs {
            // which files that need attention are gone, looked at when the
            // jobs change (not on every frame: a stale mount would hang it)
            self.gone = jobs
                .iter()
                .filter(|j| j.state == JobState::NeedsAttention && !Path::new(&j.path).exists())
                .map(|j| j.id)
                .collect();
            self.jobs = jobs.clone();
        }
        self.yielding = s.files.yielding;
        self.yield_app = s.apps.using.first().and_then(|a| a.name.clone());
        self.cleared = s.files.cleared;
        self.cleared_name = s.files.cleared_name.clone();
        self.next_up = s.files.next_up;
        self.quitting = s.files.quitting;
        self.ask_place =
            s.files.needs_folder && (s.files.folder.is_none() || s.files.folder_unreachable);
        // the daemon started them (or they went): no longer waiting
        self.sent.retain(|(id, saved)| {
            jobs.iter()
                .any(|j| j.id == *id && !j.saving && j.saved == *saved)
        });
        // the chosen folder was gone when the daemon looked: they ask where
        if self.ask_place && !self.sent.is_empty() {
            let ids = std::mem::take(&mut self.sent)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            self.save(ids);
        }
        // the folder went while a Save was written into it (in a sandbox):
        // it asks where, once
        let gone = |j: &FileJob| j.save_problem == Some(Problem::FolderGone) && !j.saving;
        self.asked_gone
            .retain(|id| jobs.iter().any(|j| j.id == *id && gone(j)));
        let ask: Vec<FileId> = jobs
            .iter()
            .filter(|j| gone(j) && !self.asked_gone.contains(&j.id))
            .map(|j| j.id)
            .collect();
        if self.ask_place && !ask.is_empty() {
            self.asked_gone.extend(&ask);
            self.save(ask);
        }
        self.daemon_db = (s.files.strength > 0.0).then_some(s.files.strength);
        if let Some(c) = &self.strength {
            // the daemon took it, or its word moved elsewhere (the
            // Settings), or it never answered: from here its word is the
            // page's, so what plays is what Save writes
            let d = self.daemon_word(c);
            let taken = d == Some(db_of(c.strength));
            let on_its_way = d == c.was || d.is_some_and(|d| c.sent.contains(&d));
            if taken || !on_its_way || c.at.elapsed() >= STRENGTH_WAIT {
                self.strength = None;
            }
        }
        self.mix_strength();
        // the first word of an output being prepared, to judge its speed
        let prep = jobs
            .iter()
            .find(|j| j.saving && j.preparing.is_some())
            .map(|j| (j.id, j.preparing.unwrap_or(0.0)));
        self.preparing = match (self.preparing, prep) {
            (Some(p), Some((id, f))) if p.0 == id && f >= p.2 => Some(p),
            (_, Some((id, f))) => Some((id, Instant::now(), f)),
            (_, None) => None,
        };
        // the selection: the one the window had (a relaunch), else a file
        // just added, else the first; none when it went
        let gone = self.selected.is_some_and(|id| self.job(id).is_none());
        if first && self.selected.is_none() {
            let had =
                s.ui.file
                    .map(|(id, _)| id)
                    .filter(|id| self.job(*id).is_some());
            if let Some(id) = had.or_else(|| self.jobs.first().map(|j| j.id)) {
                self.select(Some(id));
            }
        } else if let Some(&id) = fresh.first().filter(|_| !first) {
            // the first of those just added: the one that cleans first
            self.select(Some(id));
        } else if gone {
            let next = self.jobs.first().map(|j| j.id);
            self.select(next);
        } else if self.selected.is_none() && !self.jobs.is_empty() {
            // Undo after Clear all: the one selected then
            let back = self.cleared_sel.take().filter(|id| self.job(*id).is_some());
            let next = back.or_else(|| self.jobs.first().map(|j| j.id));
            self.select(next);
        }
        // every cleaning job's progress, and its time left eased
        let cut = now - SPEED_WINDOW.as_secs_f64();
        let ids: HashSet<FileId> = jobs.iter().map(|j| j.id).collect();
        self.progress.retain(|id, _| ids.contains(id));
        for j in jobs.iter().filter(|j| j.state == JobState::Cleaning) {
            let c = cleaned(j);
            let p = self.progress.entry(j.id).or_default();
            if p.last().is_none_or(|&(_, t)| t != c) {
                if p.last().is_some_and(|&(_, t)| t > c) {
                    p.clear();
                }
                // Only the window's own samples: one from before a stall
                // would stretch the speed over the stall on its resume.
                p.retain(|&(t, _)| t >= cut);
                p.push((now, c));
            }
        }
        let etas: Vec<(FileId, Option<f64>)> =
            jobs.iter().map(|j| (j.id, self.time_left(j))).collect();
        for (id, left) in etas {
            match left {
                Some(l) => {
                    self.eta.insert(id, (l, now));
                }
                None => {
                    self.eta.remove(&id);
                }
            }
        }
        if let Some(j) = self.selected_job().cloned() {
            if let Some(p) = &self.player {
                p.set_lit(&j);
            }
        }
    }

    fn actions(&mut self) -> Vec<Action> {
        if let Some((rx, what)) = &self.choosing {
            if let Ok(files) = rx.try_recv() {
                let what = what.clone();
                self.choosing = None;
                match (what, files.first()) {
                    (Choosing::Add, _) => self.take_in(files),
                    (Choosing::Again(id), Some(path)) => {
                        self.out.push(Action::FilesReplace(id, path.clone()));
                    }
                    (Choosing::Folder(id), Some(dir)) => {
                        self.out.push(Action::FilesSaveTo(id, folder_of(dir)));
                    }
                    (Choosing::Place(ids), Some(dir)) => {
                        // the daemon keeps it; every Save goes there now
                        self.out.push(Action::FilesFolder(folder_of(dir)));
                        self.out.extend(ids.into_iter().map(Action::FilesSave));
                    }
                    // cancelled
                    (_, None) => {}
                }
                let next = std::mem::take(&mut self.place_next);
                if !next.is_empty() {
                    self.save(next);
                }
            }
        }
        self.took_in();
        std::mem::take(&mut self.out)
    }

    fn send(&mut self, c: Cmd) {
        match c {
            Cmd::Choose => self.choose(Choosing::Add),
            Cmd::ChooseAgain(id) => self.choose(Choosing::Again(id)),
            Cmd::SaveElsewhere(id) => self.choose(Choosing::Folder(id)),
            Cmd::Add(paths) => self.take_in(paths),
            Cmd::Select(id) => {
                if self.selected != Some(id) {
                    self.select(Some(id));
                }
            }
            Cmd::Remove(id) => {
                if self.selected == Some(id) {
                    self.stop();
                    // Undo selects it again
                    self.cleared_sel = Some(id);
                }
                self.out.push(Action::FilesRemove(id));
            }
            Cmd::Play => self.play(false),
            Cmd::Pause => self.stop(),
            Cmd::Seek { to, drag } => {
                let len = self.selected_job().and_then(|j| j.frames).unwrap_or(0);
                let at = ((to * RATE) as u64).min(len);
                self.pos = at as f64 / RATE;
                if let Some(p) = &self.player {
                    p.seek(at);
                }
                // the pass moves its front only for a seek that lands
                if !drag && (self.player.is_some() || self.starting.is_some()) {
                    if let Some(id) = self.selected {
                        self.out.push(Action::FilesPlay(id, at));
                    }
                }
                if let Some(id) = self.selected.filter(|_| !drag) {
                    self.out.push(Action::UiFile(Some((id, at))));
                }
            }
            Cmd::Original(on) => {
                self.hold = on;
                if let Some(p) = &self.player {
                    p.hold(on);
                }
                if on && self.player.is_none() && self.starting.is_none() {
                    self.play(true);
                } else if !on && self.peek {
                    self.stop();
                }
            }
            // the selected file's, and the next ones'
            Cmd::Strength(s) => {
                let db = db_of(s);
                let id = self.selected.filter(|&id| self.job(id).is_some());
                if self.strength.as_ref().is_some_and(|c| c.id != id) {
                    self.strength = None;
                }
                let was = self.daemon_word(&Choice {
                    id,
                    strength: s,
                    was: None,
                    sent: Vec::new(),
                    at: Instant::now(),
                });
                let c = self.strength.get_or_insert_with(|| Choice {
                    id,
                    strength: s,
                    was,
                    sent: Vec::new(),
                    at: Instant::now(),
                });
                c.strength = s;
                c.sent.push(db);
                c.at = Instant::now();
                self.mix_strength();
                self.out.push(Action::FilesStrength(db, id));
            }
            Cmd::Track(id, t) => {
                if self.selected == Some(id) {
                    self.stop();
                }
                self.out.push(Action::FilesTrack(id, t));
            }
            Cmd::CleanAnyway => self.out.push(Action::FilesCleanAnyway),
            Cmd::CleanNext(id) => self.out.push(Action::FilesCleanNext(id)),
            Cmd::CancelQuit => self.out.push(Action::FilesCancelQuit),
            Cmd::Retry(id) => self.out.push(Action::FilesRetry(id)),
            // the saved copy, else the original (a file that didn't open)
            Cmd::ShowInFolder(id) => {
                let at = self
                    .job(id)
                    .and_then(|j| j.saved.clone().or_else(|| Some(j.path.clone())));
                if let Some(p) = at.filter(|p| Path::new(p).exists()) {
                    desktop::show_in_folder(Path::new(&p));
                }
            }
            Cmd::Save(id) => self.save(vec![id]),
            // those cleaned now, the rest when cleaned
            // (not those that need attention: a damaged file waits for the
            // user's word, a moved or full one for a new place)
            Cmd::SaveAll => {
                let all: Vec<FileId> = self
                    .jobs
                    .iter()
                    .filter(|j| {
                        (j.saved.is_none() || self.saved_at_another(j).is_some()) && !j.saving
                    })
                    .filter(|j| self.row(j).attention().is_none())
                    .map(|j| j.id)
                    .collect();
                self.save(all);
            }
            Cmd::CancelSave(id) => self.out.push(Action::FilesCancelSave(id)),
            Cmd::CleanFirst(id) => self.out.push(Action::FilesCleanFirst(id)),
            Cmd::ClearAll => {
                self.stop();
                self.cleared_sel = self.selected;
                self.out.push(Action::FilesClear);
            }
            Cmd::UndoClear => self.out.push(Action::FilesUndoClear),
        }
        self.serial += 1;
    }

    fn view(&mut self, now: f64) -> View {
        self.now = now;
        self.tend_player();
        self.read_picture();
        let play = match (&self.player, &self.starting) {
            (Some(p), _) if p.waiting() => Play::CaughtUp,
            (Some(_), _) | (None, Some(_)) => Play::Playing,
            _ => Play::Paused,
        };
        self.serial += 1;
        View {
            files: self.jobs.iter().map(|j| self.row(j)).collect(),
            selected: self.selected,
            position: self.position(),
            play,
            original: self.hold && play != Play::Paused,
            peek: self.peek,
            strength: self.strength(),
            place: place(self.selected_job()),
            cleared: self.cleared,
            cleared_name: self.cleared_name.clone(),
            yield_app: self.yield_app.clone(),
            next_up: self.next_up,
            quitting: self.quitting,
            note: self.note.clone(),
            serial: self.serial,
        }
    }

    fn picture(&self) -> Option<Arc<Sheet>> {
        self.sheet.clone()
    }

    fn busy(&self) -> bool {
        self.player.is_some()
            || self.starting.is_some()
            || self.jobs.iter().any(|j| j.state == JobState::Cleaning)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::FilesState;

    fn job(id: u64, path: &str) -> FileJob {
        FileJob {
            id,
            path: path.into(),
            frames: Some(760 * 48_000),
            exact: true,
            ..FileJob::default()
        }
    }

    fn state(jobs: Vec<FileJob>) -> AppState {
        AppState {
            files: FilesState {
                jobs,
                ..FilesState::default()
            },
            ..AppState::default()
        }
    }

    fn source() -> DaemonFiles {
        DaemonFiles::new(std::sync::Arc::new(|_| {}))
    }

    #[test]
    fn the_jobs_become_rows_and_the_first_is_selected() {
        crate::i18n::pin_english();
        let mut d = source();
        let mut a = job(3, "/nowhere/interview.m4a");
        a.state = JobState::Cleaning;
        a.lit = vec![(0, 48_000 * 10)];
        a.out_ext = "m4a".into();
        let mut b = job(4, "/nowhere/meeting.wma");
        b.state = JobState::NeedsAttention;
        b.problem = Some(Problem::Unsupported);
        d.state(&state(vec![a, b]), 1.0);
        let v = d.view(1.0);
        assert_eq!(v.selected, Some(3));
        let r = &v.files[0];
        assert_eq!(r.name, "interview.m4a");
        assert_eq!(r.length, 760.0);
        assert_eq!(r.segments, vec![(0.0, 10.0)]);
        assert_eq!(r.output, "interview (cleaned).m4a");
        assert_eq!(r.pass, Pass::Cleaning);
        // a file that isn't where it was reads as moved
        assert_eq!(
            v.files[1].pass,
            Pass::Attention(Reason::Moved { at_save: false })
        );
        // the window tells the daemon what it shows
        assert!(d.actions().contains(&Action::UiFile(Some((3, 0)))));
    }

    #[test]
    fn the_time_left_is_the_speed_up_to_now_and_none_once_it_stalls() {
        let mut d = source();
        let mut j = job(3, "/nowhere/talk.flac");
        j.state = JobState::Cleaning;
        let left = |d: &mut DaemonFiles, now: f64| d.view(now).files[0].time_left;
        // twice real time for 8 s: 744 s of the file left, at 96000 frames/s
        for t in 0..=8 {
            j.lit = vec![(0, 96_000 * t)];
            d.state(&state(vec![j.clone()]), t as f64);
        }
        assert_eq!(left(&mut d, 8.0), Some(372.0));
        // the worker gets no CPU (a busy machine): the estimate grows as
        // the time passes (eased towards the slower speed's 558 s from the
        // 368 s counted down), not frozen at the last speed
        let later = left(&mut d, 12.0).unwrap();
        assert!(later > 380.0 && later < 558.0, "{later}");
        assert!(!d.view(12.0).files[0].stalled);
        // a whole window without a frame: no estimate at all, and stalled
        assert_eq!(left(&mut d, 16.5), None);
        assert!(d.view(16.5).files[0].stalled);
        // it moves again: an estimate once the new speed is 3 s old
        for t in 17..=19 {
            j.lit = vec![(0, 96_000 * 8 + 48_000 * (t - 16))];
            d.state(&state(vec![j.clone()]), t as f64);
            assert_eq!(left(&mut d, t as f64), None);
        }
        d.state(&state(vec![j.clone()]), 20.0);
        let back = left(&mut d, 20.0).unwrap();
        // 741 s of the file left at two thirds of real time
        assert!((back - 1111.5).abs() < 1e-6, "{back}");
        assert!(!d.view(20.0).files[0].stalled);
    }

    /// F8: a speed that wobbles (a busy machine) moves the time left a
    /// little, counting down, not by minutes at each reading; two files
    /// cleaning side by side each have their own.
    #[test]
    fn the_time_left_is_steady_and_each_cleaning_file_has_its_own() {
        let mut d = source();
        let mut a = job(3, "/nowhere/talk.flac");
        let mut b = job(4, "/nowhere/other.flac");
        a.state = JobState::Cleaning;
        b.state = JobState::Cleaning;
        // a at 1.25 or 0.75 times real time, 10 s each; b steady at 2
        let (mut fa, mut shown) = (0u64, Vec::new());
        for t in 0..=60u64 {
            fa += if t % 20 < 10 { 60_000 } else { 36_000 };
            a.lit = vec![(0, fa)];
            b.lit = vec![(0, 96_000 * t)];
            d.state(&state(vec![a.clone(), b.clone()]), t as f64);
            let v = d.view(t as f64);
            if t >= 20 {
                shown.push(v.files[0].time_left.unwrap());
                assert!(v.files[1].time_left.is_some(), "{t}");
            }
        }
        let steps: Vec<f64> = shown.windows(2).map(|w| w[1] - w[0]).collect();
        let worst = steps.iter().fold(0.0f64, |m, s| m.max(s.abs()));
        assert!(worst < 30.0, "a jump of {worst:.0} s: {shown:?}");
        // its speed by turns says 560 or 930 s; the page stays near the
        // 700-740 s the average speed gives
        let (lo, hi) = shown
            .iter()
            .fold((f64::MAX, 0.0f64), |(l, h), &v| (l.min(v), h.max(v)));
        assert!(lo > 640.0 && hi < 780.0, "{shown:?}");
    }

    #[test]
    fn a_pass_starved_from_its_first_frames_resumes_without_a_wild_estimate() {
        let mut d = source();
        let mut j = job(3, "/nowhere/talk.flac");
        j.state = JobState::Cleaning;
        // one second cleaned, then no CPU for a minute
        j.lit = vec![(0, 48_000)];
        d.state(&state(vec![j.clone()]), 0.0);
        assert_eq!(d.view(30.0).files[0].time_left, None);
        assert!(d.view(30.0).files[0].stalled);
        // twice real time again: the sample from before the stall is not
        // part of the speed (it would read as hours left)
        for t in 61..=64 {
            j.lit = vec![(0, 48_000 + 96_000 * (t - 60))];
            d.state(&state(vec![j.clone()]), t as f64);
            let row = &d.view(t as f64).files[0];
            assert!(!row.stalled, "{t}");
            if t < 64 {
                assert_eq!(row.time_left, None, "{t}");
            } else {
                // 760 - 9 s of the file left at twice real time
                let got = row.time_left.unwrap();
                assert!((got - 375.5).abs() < 1e-6, "{got}");
            }
        }
    }

    #[test]
    fn problems_map_to_reasons() {
        let dir = std::env::temp_dir();
        let here = dir.join("hushmic-files-reason.wma");
        std::fs::write(&here, b"").unwrap();
        let mut j = job(1, here.to_str().unwrap());
        for (p, r) in [
            (Problem::NoSound, Reason::NoSoundTrack),
            (
                Problem::Unsupported,
                Reason::Unsupported { ext: "wma".into() },
            ),
            (Problem::Unreadable, Reason::Unreadable),
            (Problem::Engine, Reason::Engine),
            (Problem::Cache, Reason::Cache { bytes: None }),
            (Problem::Failed, Reason::Failed { again: false }),
            (Problem::Crashed, Reason::Crashed { again: false }),
        ] {
            j.problem = Some(p);
            assert_eq!(reason(&j, false), r, "{p:?}");
        }
        // the second stop in a row says so
        j.stops = 2;
        j.problem = Some(Problem::Crashed);
        assert_eq!(reason(&j, false), Reason::Crashed { again: true });
        j.stops = 0;
        j.problem = Some(Problem::NoSpace);
        assert!(matches!(
            reason(&j, false),
            Reason::Cache { bytes: Some(_) }
        ));
        j.problem = None;
        j.damaged = Some(48_000);
        assert_eq!(reason(&j, false), Reason::Damaged);
        // a cleaned file that stops early waits for the user's word; then
        // it is its first part, cleaned
        let mut d = source();
        j.state = JobState::Done;
        d.state(&state(vec![j.clone()]), 1.0);
        assert_eq!(d.view(1.0).files[0].pass, Pass::Attention(Reason::Damaged));
        j.damage_ok = true;
        d.state(&state(vec![j]), 2.0);
        assert_eq!(d.view(2.0).files[0].pass, Pass::Cleaned);
        let _ = std::fs::remove_file(here);
    }

    #[test]
    fn commands_become_actions() {
        let mut d = source();
        d.state(
            &state(vec![job(3, "/nowhere/a.wav"), job(4, "/nowhere/b.wav")]),
            1.0,
        );
        d.view(1.0);
        d.actions();
        // (Add: sorted off the frame first, a_drop_adds_its_recordings…)
        d.send(Cmd::Select(4));
        d.send(Cmd::Seek {
            to: 2.0,
            drag: false,
        });
        d.send(Cmd::Retry(4));
        d.send(Cmd::Strength(Strength::Medium));
        d.send(Cmd::Save(4));
        d.send(Cmd::CancelSave(4));
        d.send(Cmd::CleanFirst(4));
        d.send(Cmd::SaveAll);
        d.send(Cmd::Remove(4));
        assert_eq!(
            d.actions(),
            vec![
                Action::UiFile(Some((4, 0))),
                Action::UiFile(Some((4, 96_000))),
                Action::FilesRetry(4),
                Action::FilesStrength(12.0, Some(4)),
                Action::FilesSave(4),
                Action::FilesCancelSave(4),
                Action::FilesCleanFirst(4),
                Action::FilesSave(3),
                Action::FilesSave(4),
                Action::FilesRemove(4),
            ]
        );
        // the file's strength holds until the daemon reports it, then the
        // daemon's word is the page's
        assert_eq!(d.view(1.5).strength, Strength::Medium);
        let mut s = state(vec![job(3, "/nowhere/a.wav"), job(4, "/nowhere/b.wav")]);
        s.files.jobs[1].db = 12.0;
        d.state(&s, 1.6);
        s.files.jobs[1].db = 24.0;
        d.state(&s, 1.7);
        assert_eq!(d.view(1.7).strength, Strength::Strong);
        // the other file keeps its own
        s.files.jobs[0].db = 6.0;
        d.state(&s, 1.8);
        d.send(Cmd::Select(3));
        assert_eq!(d.view(1.8).strength, Strength::Gentle);
        d.send(Cmd::Select(4));
        // a file just added is selected (it pours in)
        let mut c = job(5, "/tmp/c.wav");
        c.state = JobState::Waiting;
        d.state(&state(vec![job(3, "/nowhere/a.wav"), c]), 2.0);
        let v = d.view(2.0);
        assert_eq!(v.selected, Some(5));
    }

    #[test]
    fn no_failed_save_is_silent() {
        let mut d = source();
        let cases: [(Problem, Option<&str>, Reason); 7] = [
            (Problem::NoSpace, None, Reason::NoSpace),
            (Problem::Moved, None, Reason::Moved { at_save: true }),
            (
                Problem::NoWrite,
                Some("/media/usb/Cleaned"),
                Reason::NoWrite {
                    folder: "Cleaned".into(),
                },
            ),
            (
                Problem::Encode,
                None,
                Reason::SaveEncode {
                    format: "M4A".into(),
                    why: None,
                },
            ),
            (Problem::Unreadable, None, Reason::SaveRead),
            (
                Problem::Failed,
                Some("Input/output error"),
                Reason::SaveFailed {
                    why: Some("Input/output error".into()),
                },
            ),
            (Problem::Crashed, None, Reason::SaveFailed { why: None }),
        ];
        for (k, (problem, detail, want)) in cases.into_iter().enumerate() {
            let mut j = job(1, "/nowhere/a.m4a");
            j.state = JobState::Done;
            j.out_ext = "m4a".into();
            j.save_problem = Some(problem);
            j.save_detail = detail.map(String::from);
            d.state(&state(vec![j]), 1.0 + k as f64);
            let v = d.view(1.0 + k as f64);
            assert_eq!(v.files[0].pass, Pass::Attention(want), "{problem:?}");
        }
        // a gone folder is not one: the Save falls back, or the page asks
        let mut j = job(1, "/nowhere/a.m4a");
        j.state = JobState::Done;
        j.save_problem = Some(Problem::FolderGone);
        d.state(&state(vec![j]), 10.0);
        assert_eq!(d.view(10.0).files[0].pass, Pass::Cleaned);
    }

    #[test]
    fn save_all_leaves_the_files_that_need_attention() {
        let mut d = source();
        let mut jobs: Vec<FileJob> = (1..=4).map(|i| job(i, "/nowhere/a.wav")).collect();
        for j in &mut jobs {
            j.state = JobState::Done;
        }
        jobs[0].damaged = Some(48_000);
        jobs[1].save_problem = Some(Problem::Moved);
        jobs[2].save_problem = Some(Problem::NoSpace);
        d.state(&state(jobs.clone()), 1.0);
        d.actions();
        d.send(Cmd::SaveAll);
        assert_eq!(d.actions(), vec![Action::FilesSave(4)]);
        // the damaged one, once the user said yes
        jobs[0].damage_ok = true;
        d.state(&state(jobs), 2.0);
        d.send(Cmd::SaveAll);
        assert_eq!(
            d.actions(),
            vec![Action::FilesSave(1), Action::FilesSave(4)]
        );
    }

    #[test]
    fn the_daemons_strength_wins_once_it_moves_or_never_answers() {
        let mut d = source();
        let mut s = state(vec![job(1, "/nowhere/a.wav")]);
        s.files.jobs[0].db = 100.0;
        d.state(&s, 1.0);
        d.send(Cmd::Strength(Strength::Medium));
        d.state(&s, 1.1);
        assert_eq!(d.view(1.1).strength, Strength::Medium, "waits for its word");
        // the command line moved it elsewhere meanwhile
        s.files.jobs[0].db = 24.0;
        d.state(&s, 1.2);
        assert_eq!(d.view(1.2).strength, Strength::Strong);
        // a choice the daemon never answers goes after a while
        d.send(Cmd::Strength(Strength::Gentle));
        d.state(&s, 1.3);
        assert_eq!(d.view(1.3).strength, Strength::Gentle);
        if let Some(c) = d.strength.as_mut() {
            c.at -= STRENGTH_WAIT;
        }
        d.state(&s, 1.4);
        assert_eq!(d.view(1.4).strength, Strength::Strong);
    }

    #[test]
    fn a_save_into_a_folder_that_is_gone_asks_where() {
        let mut d = source();
        let mut s = state(vec![job(1, "/nowhere/a.wav"), job(2, "/nowhere/b.wav")]);
        s.files.needs_folder = true;
        s.files.folder = Some("/run/user/1000/doc/1a2b/Musik".into());
        d.state(&s, 1.0);
        d.actions();
        d.send(Cmd::Save(1));
        assert_eq!(d.actions(), vec![Action::FilesSave(1)]);
        // the daemon looks at the folder first: not saving yet
        d.state(&s, 1.1);
        assert!(d.choosing.is_none());
        // it was gone: the daemon dropped it, and the first Save asks
        s.files.folder = None;
        d.state(&s, 1.2);
        assert!(matches!(&d.choosing, Some((_, Choosing::Place(ids))) if *ids == [1]));
        assert!(d.actions().is_empty());
        // the folder went while the Save was written into it: asks, once
        let mut d = source();
        s.files.folder = None;
        s.files.jobs[0].save_problem = Some(Problem::FolderGone);
        d.state(&s, 1.0);
        assert!(matches!(&d.choosing, Some((_, Choosing::Place(ids))) if *ids == [1]));
        d.choosing = None;
        d.state(&s, 1.1);
        assert!(d.choosing.is_none(), "asked once");
        s.files.jobs[0].save_problem = None;
        // a folder that did not answer is kept, but a Save asks where
        let mut d = source();
        s.files.folder = Some("/run/user/1000/doc/1a2b/Musik".into());
        s.files.folder_unreachable = true;
        d.state(&s, 1.0);
        d.actions();
        d.send(Cmd::Save(1));
        assert!(d.actions().is_empty(), "the folder first");
        assert!(matches!(&d.choosing, Some((_, Choosing::Place(ids))) if *ids == [1]));
        s.files.folder_unreachable = false;
        // a Save the daemon started does not ask again
        let mut d = source();
        s.files.folder = Some("/home/a/Cleaned".into());
        d.state(&s, 1.0);
        d.send(Cmd::Save(2));
        s.files.jobs[1].saving = true;
        d.state(&s, 1.1);
        s.files.jobs[1].saving = false;
        s.files.folder = None;
        d.state(&s, 1.2);
        assert!(d.choosing.is_none());
    }

    #[test]
    fn in_a_sandbox_the_first_save_asks_where_once() {
        let mut d = source();
        let mut s = state(vec![job(1, "/nowhere/a.wav"), job(2, "/nowhere/b.wav")]);
        s.files.needs_folder = true;
        d.state(&s, 1.0);
        d.actions();
        d.send(Cmd::Save(1));
        d.send(Cmd::Save(2));
        assert!(d.actions().is_empty(), "the folder first");
        assert!(matches!(&d.choosing, Some((_, Choosing::Place(ids))) if *ids == [1, 2]));
        // the chooser answers
        let (tx, rx) = mpsc::channel();
        tx.send(vec![PathBuf::from("/home/a/Cleaned")]).unwrap();
        d.choosing.as_mut().unwrap().0 = rx;
        assert_eq!(
            d.actions(),
            vec![
                Action::FilesFolder("/home/a/Cleaned".into()),
                Action::FilesSave(1),
                Action::FilesSave(2),
            ]
        );
        // a Save while another chooser is up asks once that one closes
        d.send(Cmd::Choose);
        d.send(Cmd::Save(1));
        assert!(matches!(&d.choosing, Some((_, Choosing::Add))));
        let (tx, rx) = mpsc::channel();
        tx.send(vec![]).unwrap();
        d.choosing.as_mut().unwrap().0 = rx;
        assert!(d.actions().is_empty());
        assert!(matches!(&d.choosing, Some((_, Choosing::Place(ids))) if *ids == [1]));
        // an old chooser answers a file in the folder
        let file = std::env::temp_dir().join("hushmic-files-place.wav");
        std::fs::write(&file, b"").unwrap();
        let (tx, rx) = mpsc::channel();
        tx.send(vec![file.clone()]).unwrap();
        d.choosing.as_mut().unwrap().0 = rx;
        assert_eq!(
            d.actions(),
            vec![
                Action::FilesFolder(std::env::temp_dir()),
                Action::FilesSave(1)
            ]
        );
        let _ = std::fs::remove_file(file);
        // once the daemon has it, Save is Save
        s.files.folder = Some("/home/a/Cleaned".into());
        d.state(&s, 2.0);
        d.send(Cmd::Save(2));
        assert_eq!(d.actions(), vec![Action::FilesSave(2)]);
    }

    /// A2: saved at Maximum, then the page moved to Strong: the row says
    /// it and offers Save at Strong, and Save all takes it again. The
    /// headline names the file Save really wrote.
    #[test]
    fn a_copy_saved_at_another_strength_offers_save_again() {
        crate::i18n::pin_english();
        let mut d = source();
        let mut a = job(1, "/nowhere/talk.flac");
        a.state = JobState::Done;
        a.saved = Some("/nowhere/talk (cleaned 2).flac".into());
        a.saved_db = Some(100.0);
        let mut b = job(2, "/nowhere/b.flac");
        b.state = JobState::Done;
        let mut s = state(vec![a, b]);
        s.files.strength = 100.0;
        d.state(&s, 1.0);
        let v = d.view(1.0);
        assert_eq!(v.files[0].save, Save::Saved);
        assert_eq!(v.files[0].saved_name, "talk (cleaned 2).flac");
        assert_eq!(
            super::super::copy(&v, v.row(), false, false),
            "Saved talk (cleaned 2).flac"
        );
        s.files.strength = 24.0;
        d.state(&s, 2.0);
        let v = d.view(2.0);
        assert_eq!(
            v.files[0].save,
            Save::Stale {
                was: Strength::Maximum,
                now: Strength::Strong
            }
        );
        assert_eq!(
            super::super::copy(&v, v.row(), false, false),
            "talk (cleaned 2).flac was saved at Maximum."
        );
        d.actions();
        d.send(Cmd::SaveAll);
        assert_eq!(
            d.actions(),
            vec![Action::FilesSave(1), Action::FilesSave(2)]
        );
        // Save again writes: the row says so, not Saved
        s.files.jobs[0].saving = true;
        s.files.jobs[0].preparing = Some(0.5);
        d.state(&s, 3.0);
        assert!(matches!(d.view(3.0).files[0].save, Save::Writing { .. }));
    }

    /// Clear all: the player stops and the daemon is told; on Undo the
    /// file selected then is selected again.
    #[test]
    fn undo_after_clear_all_selects_the_file_again() {
        let mut d = source();
        let jobs = vec![job(1, "/nowhere/a.wav"), job(2, "/nowhere/b.wav")];
        let mut s = state(jobs.clone());
        d.state(&s, 1.0);
        d.send(Cmd::Select(2));
        d.actions();
        d.send(Cmd::ClearAll);
        assert_eq!(d.actions(), vec![Action::FilesClear]);
        s.files.jobs.clear();
        s.files.cleared = 2;
        d.state(&s, 2.0);
        d.actions();
        let v = d.view(2.0);
        assert_eq!((v.selected, v.cleared), (None, 2));
        d.send(Cmd::UndoClear);
        assert_eq!(d.actions(), vec![Action::FilesUndoClear]);
        s.files.jobs = jobs;
        s.files.cleared = 0;
        d.state(&s, 3.0);
        let v = d.view(3.0);
        assert_eq!((v.selected, v.cleared), (Some(2), 0));
    }

    /// B10: files added together select the first of them (the one that
    /// cleans first), not the last.
    #[test]
    fn files_added_together_select_the_first_of_them() {
        let mut d = source();
        let mut s = state(vec![job(1, "/nowhere/a.wav")]);
        d.state(&s, 1.0);
        assert_eq!(d.view(1.0).selected, Some(1));
        s.files.jobs.push(job(2, "/nowhere/b.wav"));
        s.files.jobs.push(job(3, "/nowhere/c.wav"));
        d.state(&s, 2.0);
        assert_eq!(d.view(2.0).selected, Some(2));
    }

    /// B13: a video's icon from its name at once, before it opened; once
    /// open, what it is.
    #[test]
    fn a_videos_icon_comes_from_its_name_at_once() {
        let mut d = source();
        let mut a = job(1, "/nowhere/VID_0001.MP4");
        a.frames = None;
        d.state(&state(vec![a.clone()]), 1.0);
        assert_eq!(d.view(1.0).files[0].kind, Kind::Video);
        // an MP4 with sound only, once open
        a.rate = 48_000;
        d.state(&state(vec![a]), 2.0);
        assert_eq!(d.view(2.0).files[0].kind, Kind::Audio);
    }

    /// B9: a drop is sorted off the frame: recordings are added, a text
    /// file is noted and left out, and one already on the page is
    /// selected, not added again.
    #[test]
    fn a_drop_adds_its_recordings_and_notes_the_rest() {
        crate::i18n::pin_english();
        let dir = std::env::temp_dir().join(format!("hushmic-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = |n: &str| {
            let p = dir.join(n);
            let mut b = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
            b.resize(64, 0);
            std::fs::write(&p, b).unwrap();
            p
        };
        let (a, b) = (wav("a.wav"), wav("b.wav"));
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, "x\n").unwrap();
        let mut d = source();
        let s = state(vec![
            job(1, &a.display().to_string()),
            job(2, "/nowhere/other.wav"),
        ]);
        d.state(&s, 1.0);
        d.send(Cmd::Select(2));
        d.actions();
        let wait = |d: &mut DaemonFiles| {
            for _ in 0..200 {
                let got = d.actions();
                if d.note.is_some() {
                    return got;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("no note");
        };
        d.send(Cmd::Add(vec![b.clone(), txt]));
        assert_eq!(wait(&mut d), vec![Action::FilesAdd(b)]);
        assert_eq!(
            d.view(2.0).note.map(|n| n.0).as_deref(),
            Some("notes.txt isn’t an audio or video file.")
        );
        // a file already there: selected, and said
        d.note = None;
        d.send(Cmd::Add(vec![a]));
        let got = wait(&mut d);
        assert!(
            !got.iter().any(|a| matches!(a, Action::FilesAdd(_))),
            "{got:?}"
        );
        let v = d.view(3.0);
        assert_eq!(v.selected, Some(1));
        assert_eq!(
            v.note.map(|n| n.0).as_deref(),
            Some("a.wav is already in the list.")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn two_quick_clicks_wait_for_the_last() {
        let mut d = source();
        let mut s = state(vec![job(1, "/nowhere/a.wav")]);
        s.files.jobs[0].db = 100.0;
        d.state(&s, 1.0);
        d.send(Cmd::Strength(Strength::Medium));
        d.send(Cmd::Strength(Strength::Strong));
        // the daemon's answer to the first is on its way, not a move
        s.files.jobs[0].db = 12.0;
        d.state(&s, 1.1);
        assert_eq!(d.view(1.1).strength, Strength::Strong);
        s.files.jobs[0].db = 24.0;
        d.state(&s, 1.2);
        assert_eq!(d.view(1.2).strength, Strength::Strong);
        assert!(d.strength.is_none(), "taken");
    }

    #[test]
    fn play_at_the_end_plays_from_the_start() {
        let mut d = source();
        d.state(&state(vec![job(1, "/nowhere/a.wav")]), 1.0);
        d.actions();
        d.pos = 760.0;
        d.send(Cmd::Play);
        assert_eq!(d.actions(), vec![Action::FilesPlay(1, 0)]);
    }

    #[test]
    fn a_save_waits_for_the_pass_then_prepares_its_output() {
        let mut d = source();
        let mut j = job(1, "/nowhere/talk.flac");
        j.saving = true;
        j.state = JobState::Cleaning;
        d.state(&state(vec![j.clone()]), 1.0);
        assert_eq!(d.view(1.0).files[0].save, Save::WhenCleaned);
        j.state = JobState::Done;
        j.preparing = Some(0.25);
        d.state(&state(vec![j.clone()]), 2.0);
        let Save::Writing {
            fraction,
            secs_left,
        } = d.view(2.0).files[0].save
        else {
            panic!("writing");
        };
        assert_eq!(fraction, 0.25);
        // 760 s at 40 times real time, three quarters to go
        assert!((secs_left - 14.25).abs() < 1e-9, "{secs_left}");
        j.preparing = None;
        j.saving = false;
        j.saved = Some("/nowhere/talk (cleaned).flac".into());
        d.state(&state(vec![j]), 3.0);
        assert_eq!(d.view(3.0).files[0].save, Save::Saved);
    }

    #[test]
    fn saved_files_say_where() {
        let mut j = job(1, "/home/a/Recordings/talk.flac");
        assert_eq!(place(Some(&j)), Place::NextToOriginal);
        j.saved = Some("/home/a/Recordings/talk (cleaned).flac".into());
        assert_eq!(place(Some(&j)), Place::NextToOriginal);
        // next to it instead of the chosen folder: the page says why
        j.folder_miss = Some(crate::appstate::FolderMiss::NoAnswer);
        assert_eq!(
            place(Some(&j)),
            Place::Instead(crate::appstate::FolderMiss::NoAnswer)
        );
        j.folder_miss = None;
        j.saved = Some("/home/a/Music/Cleaned/talk (cleaned).flac".into());
        assert_eq!(place(Some(&j)), Place::Folder("Music/Cleaned".into()));
        // a folder the document portal handed over: never by its ID, by
        // the host path the portal gives, else by its own name
        j.saved = Some("/run/user/1000/doc/9f3e01aa/Musik/talk (cleaned).flac".into());
        assert_eq!(place(Some(&j)), Place::Folder("Musik".into()));
        crate::files::docpath::answered("9f3e01aa", Some("/home/a/Musik"));
        assert_eq!(place(Some(&j)), Place::Folder("a/Musik".into()));
        assert_eq!(
            output_name(Path::new("/a/talk.mp4"), "m4a", None),
            "talk (cleaned).m4a"
        );
        assert_eq!(
            output_name(Path::new("/a/talk.flac"), "", None),
            "talk (cleaned).flac"
        );
        // F5: a Save again at another strength says which
        assert_eq!(
            output_name(Path::new("/a/talk.flac"), "", Some("Gentle")),
            "talk (cleaned, Gentle).flac"
        );
    }
}
