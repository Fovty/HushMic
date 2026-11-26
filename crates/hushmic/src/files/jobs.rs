//! The Files page's jobs, kept by the daemon so they outlive the window
//! (which is replaced on every launch). Each file is cleaned by a worker
//! process of its own (`hushmic clean --worker`, see [`super::worker`]).
//! One cleans at a time, in the order the files were added, except the
//! file the user plays, which goes first; a file it overtook waits stopped
//! (SIGSTOP) where it was and goes on after. While an app uses HushMic and
//! the microphone's engine has stepped down for want of CPU, cleaning
//! waits too: the call comes first (unless the user says "Clean anyway").
//!
//! A cleaned file's output for Save is prepared in its cache at the page's
//! strength (`clean --worker --prepare`, one file at a time, a file the
//! user saves first), so Save itself only puts it in place. A new strength
//! prepares again.
//!
//! Driven by the daemon's main loop (the spawn ties each worker to the
//! calling thread's life, so that must be the main thread).

use super::cache::{bytes_needed, free_bytes};
use super::save::Pick;
use super::worker::{save_problem, short_reason, Args, Command, Msg, Prepare, Problem};
use crate::appstate::{FileJob, FilesState, FolderMiss, JobState};
use hushmic_denoiser::Inference;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// A cache folder of the daemon's: `job-<id>` under the cache root.
const PREFIX: &str = "job-";

/// A word from a worker, for the main loop.
#[derive(Clone, Debug, PartialEq)]
pub struct JobEvent {
    pub id: u64,
    /// Which of the job's workers (a retry starts another).
    pub serial: u64,
    pub heard: Heard,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Heard {
    Said(Msg),
    /// Its output ended: it is exiting.
    Gone,
    /// A Save put the output in place (where), or could not.
    Saved(Result<String, (Problem, String)>),
}

/// A running worker, as the jobs drive it.
pub trait Worker {
    fn send(&mut self, c: Command);
    /// Stop (SIGSTOP) or continue it.
    fn hold(&mut self, on: bool);
    /// Kill and reap it.
    fn end(&mut self);
    /// Reap it after its output ended.
    fn reap(&mut self);
}

pub trait Spawner {
    fn spawn(&mut self, args: &Args, id: u64, serial: u64) -> io::Result<Box<dyn Worker>>;
    /// A first look at a file ([`Args::probe`]): a worker like the others.
    fn probe(&mut self, args: &Args, id: u64, serial: u64) -> io::Result<Box<dyn Worker>> {
        self.spawn(args, id, serial)
    }
    /// Put a prepared output in place (see `save::deliver`), off the
    /// calling thread; the outcome comes back as [`Heard::Saved`].
    fn deliver(&mut self, d: Delivery, id: u64, serial: u64);
    /// Wait (up to `within`) for the deliveries under way to end: at Quit,
    /// once they are cancelled, so none leaves a partial copy behind.
    fn finish(&mut self, _within: std::time::Duration) {}
}

/// How long Quit waits for cancelled Saves to clean up after themselves
/// (a copy checks its cancel every MiB).
const FINISH: std::time::Duration = std::time::Duration::from_secs(3);

/// A Save: the prepared file, its original, and the folder (else the
/// original's).
#[derive(Clone, Debug)]
pub struct Delivery {
    pub file: PathBuf,
    pub original: PathBuf,
    pub dir: Option<PathBuf>,
    /// A re-save at another strength: its name says it ("(cleaned,
    /// Gentle)").
    pub mark: Option<String>,
    pub cancel: Arc<AtomicBool>,
    /// Where a copy under way is noted (see `save::sweep_partials`).
    pub journal: PathBuf,
}

impl PartialEq for Delivery {
    fn eq(&self, o: &Delivery) -> bool {
        (&self.file, &self.original, &self.dir, &self.mark)
            == (&o.file, &o.original, &o.dir, &o.mark)
    }
}

/// Why the daemon would not Save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    NoSuchFile,
    NeedsAttention,
    /// Damaged, and cleaning the first part was not agreed to.
    Damaged,
    /// The original is not where it was.
    Moved,
    /// In a sandbox, no folder for cleaned files is chosen yet.
    NoFolder,
    /// The folder to save in is not an absolute path (the daemon's working
    /// folder is nobody's).
    Relative,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refused::NoSuchFile => "no such file",
            Refused::NeedsAttention => "the file needs attention",
            Refused::Damaged => "the file is damaged: agree to clean its first part first",
            Refused::Moved => "the original moved",
            Refused::NoFolder => "choose a folder for cleaned files first",
            Refused::Relative => "the folder must be an absolute path",
        })
    }
}

/// A file comes as an absolute path: a relative one would be read against
/// the daemon's working folder, not the caller's.
fn absolute(path: &str) -> Result<(), String> {
    if Path::new(path).is_absolute() {
        Ok(())
    } else {
        Err(format!("not an absolute path: {path}"))
    }
}

/// What the workers run: the Settings' model and inference.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineChoice {
    pub model: String,
    pub inference: Inference,
    pub ffmpeg: Option<PathBuf>,
    /// Workers without an engine (tests).
    pub fake: bool,
}

struct Live {
    serial: u64,
    w: Box<dyn Worker>,
    held: bool,
}

struct Job {
    wire: FileJob,
    worker: Option<Live>,
    /// Where playback wants cleaning from, for a worker not started yet.
    front: Option<u64>,
    /// The worker preparing the output for Save, and at what strength.
    saver: Option<(Live, f32)>,
    prepared_file: Option<PathBuf>,
    /// Save was asked for: at this strength, into this folder.
    save: Option<(f32, Option<PathBuf>)>,
    /// The Save under way (its serial), and what cancels it.
    delivering: Option<(u64, Arc<AtomicBool>)>,
    /// Preparing at this strength failed: not again unless Save asks.
    failed_db: Option<f32>,
    /// The first look at the file, while it runs ([`super::probe`]).
    probe: Option<Live>,
    /// The file was looked at (or opened by its pass): no first look.
    looked: bool,
}

impl Job {
    fn new(wire: FileJob) -> Job {
        Job {
            wire,
            probe: None,
            looked: false,
            worker: None,
            front: None,
            saver: None,
            prepared_file: None,
            save: None,
            delivering: None,
            failed_db: None,
        }
    }

    /// The strength its output should be ready at, once cleaned: its
    /// Save's, else its own.
    fn want(&self) -> Option<f32> {
        (self.wire.state == JobState::Done)
            .then(|| self.save.as_ref().map_or(self.wire.db, |s| s.0))
    }

    /// It needs preparing at `want`.
    fn stale(&self) -> Option<f32> {
        self.want()
            .filter(|&db| self.wire.prepared != Some(db) && self.failed_db != Some(db))
    }

    /// Its row as new, to clean again (`end` it first): what the user
    /// chose for it stays (its track, its strength) and its place.
    fn renewed(&self, path: String, stops: u32) -> Job {
        let w = &self.wire;
        Job::new(FileJob {
            id: w.id,
            path,
            cache: w.cache.clone(),
            stops,
            chosen: w.chosen,
            db: w.db,
            ..FileJob::default()
        })
    }
}

impl Job {
    fn open(&self) -> bool {
        matches!(self.wire.state, JobState::Waiting | JobState::Cleaning)
    }
}

type FreeBytes = Box<dyn Fn(&Path) -> io::Result<u64>>;

pub struct Jobs {
    root: PathBuf,
    engine: EngineChoice,
    list: Vec<Job>,
    next_id: u64,
    next_serial: u64,
    next_probe: u64,
    played: Option<u64>,
    /// "Clean next": this file's turn comes before the others that wait.
    next: Option<u64>,
    /// How many files clean at once while no app uses HushMic (F8): up to
    /// [`MOST_LANES`], a quarter of the cores; one while an app does.
    lanes: usize,
    /// Cleaning waits: an app uses HushMic while the microphone's engine
    /// has stepped down for want of CPU (the call comes first), and the
    /// user did not say "Clean anyway".
    yielding: bool,
    stepped_down: bool,
    in_use: bool,
    anyway: bool,
    /// The page's strength (a dB cap).
    strength: f32,
    /// The window asks whether to stop the files and quit.
    ask_quit: bool,
    /// "Save all and quit": these files are saved (each once cleaned),
    /// then HushMic quits ([`Jobs::quit_ready`]).
    quitting: Option<Vec<u64>>,
    /// "Clean anyway" before "Save all and quit" set it (Cancel puts it
    /// back).
    anyway_before: bool,
    /// Files cleaned since the last [`Jobs::take_cleaned`].
    cleaned: Vec<u64>,
    /// The note's Save all: the files it saves ([`Jobs::take_saved`]).
    noting: Vec<u64>,
    /// Clear all took these off the page, and when: [`Jobs::undo_clear`]
    /// puts them back until [`UNDO_FOR`] has passed ([`Jobs::tick`]).
    /// Each with its place on the page; one alone was a row's × (an
    /// unsaved or running file: also undoable).
    cleared: Option<(Instant, Vec<(usize, Job)>)>,
    /// What `cleared` holds came from a row's ×, not Clear all.
    cleared_one: bool,
    /// Save cannot write next to the original (a sandbox): into `folder`.
    needs_folder: bool,
    folder: Option<PathBuf>,
    /// A look at `folder` (is it still there?) on a thread of its own: a
    /// look into a hung network mount or document portal must not hold the
    /// daemon's loop. Its answer: the folder looked at, and whether it is.
    folder_look: Option<mpsc::Receiver<(PathBuf, bool)>>,
    /// When the last look started.
    looked_at: Option<Instant>,
    /// The Files page was shown at the last [`Jobs::check_folder`].
    files_shown: bool,
    /// Saves into the chosen folder that wait for a look at it.
    waiting_folder: Vec<u64>,
    /// How a folder is looked at (tests: one that hangs).
    looker: Looker,
    /// The last look did not answer in [`FOLDER_LOOK_LIMIT`]: the folder is
    /// kept (a share that mounts slowly is not gone), Saves meanwhile go
    /// as for a gone one, and the look under way (or the next) may still
    /// say it is there.
    folder_unreachable: bool,
    spawner: Box<dyn Spawner>,
    free: FreeBytes,
}

/// While the Files page shows, the chosen folder is looked at this often
/// (and when the page opens, and before each Save); never otherwise, so an
/// idle network or autofs mount can sleep.
pub const FOLDER_LOOK_EVERY: Duration = Duration::from_secs(30);

/// A look that has not answered in this long (a hung network mount or
/// portal) counts as a folder that is gone: a Save that waits for it never
/// waits silently for good.
pub const FOLDER_LOOK_LIMIT: Duration = Duration::from_secs(10);

/// At most this many files clean at once (F8).
pub const MOST_LANES: usize = 3;

/// How many files clean at once on a machine with `cores`: a quarter of
/// them, at least one and at most [`MOST_LANES`] (each pass takes a core,
/// and the microphone's own engine comes first).
pub fn lanes_for(cores: usize) -> usize {
    (cores / 4).clamp(1, MOST_LANES)
}

/// First looks at files that wait their turn, at most this many at once.
const PROBES: usize = 2;
/// The first looks' serials start here, apart from the others.
const PROBE_SERIALS: u64 = 1 << 40;

/// The strength Files starts at (a dB cap): Strong. Live starts at
/// Maximum; a recording keeps a little of its room, no dead air between
/// words.
pub const DEFAULT_DB: f32 = 24.0;

/// Clear all can be undone this long: a little longer than the page's
/// toast shows (about 8 s), so an Undo pressed as it goes still lands.
pub const UNDO_FOR: Duration = Duration::from_secs(10);

type Looker = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

impl Jobs {
    pub fn new(root: PathBuf, engine: EngineChoice, spawner: Box<dyn Spawner>) -> Jobs {
        Jobs {
            root,
            engine,
            list: Vec::new(),
            next_id: 1,
            next_serial: 1,
            next_probe: 0,
            played: None,
            next: None,
            lanes: lanes_for(
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1),
            ),
            yielding: false,
            stepped_down: false,
            in_use: false,
            anyway: false,
            strength: DEFAULT_DB,
            ask_quit: false,
            quitting: None,
            anyway_before: false,
            cleaned: Vec::new(),
            noting: Vec::new(),
            cleared: None,
            cleared_one: false,
            needs_folder: false,
            folder: None,
            folder_look: None,
            looked_at: None,
            files_shown: false,
            waiting_folder: Vec::new(),
            looker: Arc::new(|d: &Path| d.is_dir()),
            folder_unreachable: false,
            spawner,
            free: Box::new(free_bytes),
        }
    }

    /// Free space as `free` says (tests).
    /// Tests: this many files clean at once while no app uses HushMic.
    pub fn with_lanes(mut self, lanes: usize) -> Jobs {
        self.lanes = lanes.max(1);
        self
    }

    pub fn with_free(mut self, free: impl Fn(&Path) -> io::Result<u64> + 'static) -> Jobs {
        self.free = Box::new(free);
        self
    }

    /// The model or inference changed: files started from now on use it.
    pub fn set_engine(&mut self, engine: EngineChoice) {
        self.engine = engine;
    }

    pub fn state(&self) -> FilesState {
        FilesState {
            jobs: self.list.iter().map(|j| j.wire.clone()).collect(),
            yielding: self.yielding,
            strength: self.strength,
            ask_quit: self.ask_quit,
            needs_folder: self.needs_folder,
            folder: self.folder.as_ref().map(|f| f.display().to_string()),
            folder_unreachable: self.folder_unreachable,
            cleared: self.cleared.as_ref().map_or(0, |(_, l)| l.len()),
            cleared_name: match self.cleared.as_ref().map(|(_, l)| l.as_slice()) {
                Some([(_, j)]) if self.cleared_one => Path::new(&j.wire.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned()),
                _ => None,
            },
            next_up: self
                .queue()
                .get(self.lanes_now())
                .map(|&i| self.list[i].wire.id),
            quitting: self.quitting.as_ref().map(|ids| {
                ids.iter()
                    .filter(|&&id| {
                        self.index(id)
                            .is_some_and(|i| self.list[i].wire.saved.is_none())
                    })
                    .count()
            }),
        }
    }

    /// Whether Save can write next to the original (false in a sandbox),
    /// and the folder Settings › Files chose ("In a folder…"; in a sandbox
    /// the one asked for at the first Save). A folder that is gone
    /// (deleted, or a sandbox grant that did not last) counts as none: next
    /// to the original again, and in a sandbox the page asks again.
    /// It is looked at once (off the loop) at once, at the start and at
    /// each choice, so one already gone is not shown by its old name.
    pub fn set_folder(&mut self, needs: bool, folder: Option<PathBuf>) {
        (self.needs_folder, self.folder) = (needs, folder);
        (self.folder_look, self.folder_unreachable) = (None, false);
        self.look_at_folder();
    }

    /// How a folder is looked at (tests).
    #[cfg(test)]
    fn with_looker(mut self, looker: impl Fn(&Path) -> bool + Send + Sync + 'static) -> Jobs {
        self.looker = Arc::new(looker);
        self
    }

    /// The chosen folder is looked at when it matters: when the Files page
    /// opens (`shown` turns true), every [`FOLDER_LOOK_EVERY`] while it
    /// shows, and before each Save. One that is gone is dropped: the
    /// window's state says none, so in a sandbox the Save asks for a folder
    /// again; otherwise the file goes next to its original. Saves that
    /// waited for the look go ahead once it answers; a look that does not
    /// answer within [`FOLDER_LOOK_LIMIT`] counts as a folder that is gone.
    /// Never blocks.
    pub fn check_folder(&mut self, shown: bool) {
        let opened = shown && !self.files_shown;
        self.files_shown = shown;
        if let Some(rx) = &self.folder_look {
            let late = self
                .looked_at
                .is_some_and(|t| t.elapsed() >= FOLDER_LOOK_LIMIT);
            match rx.try_recv() {
                Ok((_, there)) => {
                    // a definite answer: there, or gone (only that drops it)
                    (self.folder_look, self.folder_unreachable) = (None, false);
                    let miss = (!there && self.folder.take().is_some()).then_some(FolderMiss::Gone);
                    if miss.is_some() {
                        eprintln!("[hushmic] the folder for cleaned files is gone");
                    }
                    for id in std::mem::take(&mut self.waiting_folder) {
                        self.start_save(id, self.folder.clone(), miss);
                    }
                }
                Err(mpsc::TryRecvError::Empty) if late && !self.folder_unreachable => {
                    eprintln!(
                        "[hushmic] the folder for cleaned files did not answer in {}s; \
                         kept, and looked at again",
                        FOLDER_LOOK_LIMIT.as_secs()
                    );
                    self.unreachable();
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.folder_look = None;
                    self.unreachable();
                }
            }
        }
        let due = self
            .looked_at
            .is_none_or(|t| t.elapsed() >= FOLDER_LOOK_EVERY);
        if opened || (shown && due) {
            self.look_at_folder();
        }
    }

    /// The folder a Save was written into went after the look said it was
    /// there (a definite "gone"): the chosen one is dropped, and the Save
    /// falls back as for a gone folder. In a sandbox the page asks where
    /// (the job says FolderGone); otherwise it goes next to the original,
    /// and the page says why.
    fn save_without(&mut self, i: usize, into: Option<PathBuf>) {
        if into.is_some() && into == self.folder {
            eprintln!("[hushmic] the folder for cleaned files is gone");
            (self.folder, self.folder_look, self.folder_unreachable) = (None, None, false);
        }
        if !self.needs_folder {
            let id = self.list[i].wire.id;
            self.list[i].wire.save_problem = None;
            self.start_save(id, None, Some(FolderMiss::Gone));
        }
    }

    /// The look did not answer in time: "not reachable right now". The
    /// folder stays; the Saves that waited go as for a gone folder (in a
    /// sandbox the page asks where; otherwise next to the original, which
    /// the page says).
    fn unreachable(&mut self) {
        self.folder_unreachable = true;
        for id in std::mem::take(&mut self.waiting_folder) {
            self.start_save(id, None, Some(FolderMiss::NoAnswer));
        }
    }

    /// A look at the chosen folder, unless one is under way.
    fn look_at_folder(&mut self) {
        let Some(dir) = self.folder.clone() else {
            return;
        };
        if self.folder_look.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let looker = Arc::clone(&self.looker);
        let spawned = std::thread::Builder::new()
            .name("hushmic-folder".into())
            .spawn(move || {
                let there = looker(&dir);
                let _ = tx.send((dir, there));
            });
        if spawned.is_ok() {
            (self.folder_look, self.looked_at) = (Some(rx), Some(Instant::now()));
        }
    }

    /// Files that quitting would stop: cleaning, waiting to, or saving.
    pub fn busy(&self) -> usize {
        self.list
            .iter()
            .filter(|j| j.open() || j.wire.saving)
            .count()
    }

    /// Files that quitting would lose: the busy ones, and those cleaned
    /// but not saved (a failed Save counts: nothing was written).
    pub fn at_stake(&self) -> usize {
        self.list
            .iter()
            .filter(|j| j.open() || j.wire.saving || j.wire.unsaved())
            .count()
    }

    /// Why `hushmic quit` (without `--force`) won't: "2 cleaned files
    /// aren't saved and 1 is cleaning; use --force to quit anyway". None:
    /// nothing would be lost.
    pub fn quit_refusal(&self) -> Option<String> {
        let unsaved = self.list.iter().filter(|j| j.wire.unsaved()).count();
        let cleaning = self.list.iter().filter(|j| j.open()).count();
        let saving = self
            .list
            .iter()
            .filter(|j| j.wire.saving && !j.open())
            .count();
        let mut said: Vec<String> = Vec::new();
        let noun = |n: usize, first: bool, what: &str| match (n, first) {
            (1, true) => format!("1 {what}file"),
            (n, true) => format!("{n} {what}files"),
            (n, false) => n.to_string(),
        };
        let verb = |n: usize, one: &str, many: &str| if n == 1 { one } else { many }.to_string();
        if unsaved > 0 {
            let n = noun(unsaved, said.is_empty(), "cleaned ");
            said.push(format!(
                "{n} {}",
                verb(unsaved, "isn't saved", "aren't saved")
            ));
        }
        if cleaning > 0 {
            let n = noun(cleaning, said.is_empty(), "");
            said.push(format!(
                "{n} {}",
                verb(cleaning, "is cleaning", "are cleaning")
            ));
        }
        if saving > 0 {
            let n = noun(saving, said.is_empty(), "");
            said.push(format!("{n} {}", verb(saving, "is saving", "are saving")));
        }
        let list = match said.as_slice() {
            [] => return None,
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        };
        Some(format!("{list}; use --force to quit anyway"))
    }

    /// "Save all and quit": every file quitting would lose is saved (one
    /// still cleaning once cleaned), at its own strength, next to its
    /// original or into the chosen folder; HushMic quits once they are
    /// ([`Jobs::quit_ready`]). Files that can't be saved as they are
    /// (they need attention, a damaged one not agreed to) stay as they
    /// are. Refused in a sandbox with no folder yet (the window asks
    /// where first).
    pub fn save_all_and_quit(&mut self) -> Result<usize, Refused> {
        let ids: Vec<u64> = self
            .list
            .iter()
            .filter(|j| j.open() || j.wire.saving || j.wire.unsaved())
            .filter(|j| j.wire.save_problem.is_none())
            .filter(|j| j.wire.damaged.is_none() || j.wire.damage_ok)
            .map(|j| j.wire.id)
            .collect();
        if self.needs_folder && self.folder.is_none() {
            return Err(Refused::NoFolder);
        }
        for &id in &ids {
            let saving = self.index(id).is_some_and(|i| self.list[i].wire.saving);
            if !saving {
                // one that moved says so on its row, and keeps HushMic up
                let _ = self.save(id, None);
            }
        }
        self.ask_quit = false;
        self.quitting = Some(ids);
        // quitting takes HushMic out of the call anyway: a file still
        // cleaning doesn't wait for the call's end
        self.anyway_before = self.anyway;
        self.clean_anyway();
        Ok(self.quitting.as_ref().map_or(0, Vec::len))
    }

    /// Cancel on "Quits once 2 files are saved": HushMic stays; the files
    /// it was saving go on saving.
    pub fn cancel_quit(&mut self) -> bool {
        if self.quitting.take().is_none() {
            return false;
        }
        if !self.anyway_before {
            self.anyway = false;
            self.yield_now();
        }
        true
    }

    /// "Save all and quit" is through: true once every file it saves is
    /// saved (quit now). One that could not be (its Save failed, it needs
    /// attention, it was taken off the page) keeps HushMic up: the page
    /// says why, and nothing more is waited for.
    pub fn quit_ready(&mut self) -> bool {
        let Some(ids) = &self.quitting else {
            return false;
        };
        let rows: Vec<Option<&FileJob>> = ids
            .iter()
            .map(|id| self.index(*id).map(|i| &self.list[i].wire))
            .collect();
        let pending = |w: &&FileJob| {
            matches!(w.state, JobState::Waiting | JobState::Cleaning)
                || w.saving
                || self.waiting_folder.contains(&w.id)
        };
        if rows.iter().flatten().any(pending) {
            return false;
        }
        let all = rows.iter().all(|w| w.is_some_and(|w| w.saved.is_some()));
        self.quitting = None;
        if !all {
            eprintln!("[hushmic] not quitting: a file could not be saved");
            return false;
        }
        // a file added meanwhile would be lost: the window asks again
        if self.at_stake() > 0 {
            self.ask_quit = true;
            return false;
        }
        true
    }

    /// Quit was chosen while files are busy: the window asks (true), or
    /// the user answered (false). The question stays until answered, also
    /// when the files finish meanwhile.
    pub fn set_ask_quit(&mut self, on: bool) {
        self.ask_quit = on;
    }

    /// The window asks whether to quit.
    pub fn asking_quit(&self) -> bool {
        self.ask_quit
    }

    /// Once nothing is busy any more: the files cleaned since the last
    /// time, if any (for "3 recordings cleaned.").
    pub fn take_cleaned(&mut self) -> Option<Cleaned> {
        if self.busy() > 0 || self.cleaned.is_empty() {
            return None;
        }
        let ids = std::mem::take(&mut self.cleaned);
        let saved = ids
            .iter()
            .map(|id| {
                let w = &self.list[self.index(*id)?].wire;
                Some((PathBuf::from(&w.path), PathBuf::from(w.saved.as_ref()?)))
            })
            .collect();
        Some(Cleaned {
            count: ids.len(),
            saved,
            failed: 0,
        })
    }

    /// The note's Save all: every cleaned file not saved yet, next to its
    /// original (or into the chosen folder) at the page's strength, as the
    /// page's Save all; not those that need attention or wait for the
    /// user's word. How many; refused (the window must ask where) in a
    /// sandbox with no folder yet.
    pub fn save_all(&mut self) -> Result<usize, Refused> {
        let ids: Vec<u64> = self
            .list
            .iter()
            .filter(|j| j.wire.unsaved() && j.wire.save_problem.is_none())
            .filter(|j| j.wire.damaged.is_none() || j.wire.damage_ok)
            .map(|j| j.wire.id)
            .collect();
        if !ids.is_empty() && self.needs_folder && self.folder.is_none() {
            return Err(Refused::NoFolder);
        }
        for &id in &ids {
            // one that moved says so on its row, and counts as not saved
            if let Err(Refused::NoFolder) = self.save(id, None) {
                return Err(Refused::NoFolder);
            }
        }
        self.noting = ids;
        Ok(self.noting.len())
    }

    /// Once the note's Save all is through: how many were saved and where,
    /// and how many weren't (for "Saved 2 recordings.").
    pub fn take_saved(&mut self) -> Option<Cleaned> {
        if self.noting.is_empty() {
            return None;
        }
        let jobs: Vec<FileJob> = self
            .noting
            .iter()
            .filter_map(|id| Some(self.list[self.index(*id)?].wire.clone()))
            .collect();
        let pending = |w: &FileJob| w.saving || self.waiting_folder.contains(&w.id);
        if jobs.iter().any(pending) {
            return None;
        }
        self.noting.clear();
        let saved: Vec<(PathBuf, PathBuf)> = jobs
            .iter()
            .filter_map(|w| Some((PathBuf::from(&w.path), PathBuf::from(w.saved.as_ref()?))))
            .collect();
        let failed = jobs.len() - saved.len();
        if jobs.is_empty() {
            return None;
        }
        Some(Cleaned {
            count: saved.len(),
            saved: Some(saved),
            failed,
        })
    }

    /// The page's strength: what files added from now on play and save
    /// at.
    pub fn set_strength(&mut self, db: f32) {
        self.strength = db;
    }

    /// The selected file's strength (the page's too, for files added
    /// next): what it plays and saves at from now on. The others keep
    /// theirs. False: no such file.
    pub fn set_file_strength(&mut self, id: u64, db: f32) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.strength = db;
        if self.list[i].wire.db != db {
            self.list[i].wire.db = db;
            self.reconcile();
        }
        true
    }

    /// Clean another of the file's sound tracks (`track`: 0 the first):
    /// it starts over on it, its strength kept. What it knows of the
    /// file's tracks stays until the worker opens it again (the picker
    /// stays, and a second change is checked against them). Err: why not,
    /// in words for `hushmic files track`.
    pub fn set_track(&mut self, id: u64, track: usize) -> Result<(), String> {
        let Some(i) = self.index(id) else {
            return Err(Refused::NoSuchFile.to_string());
        };
        let j = &mut self.list[i];
        let w = &j.wire;
        let opened = w.tracks > 0;
        let count = w.tracks + w.dropped;
        if opened && track >= count {
            return Err(format!(
                "there is no sound track {} (it has {count})",
                track + 1
            ));
        }
        if w.chosen.unwrap_or(w.track) == track && (opened || w.chosen.is_some()) {
            return Err(format!("it already cleans sound track {}", track + 1));
        }
        let (tracks, dropped, titles) = (w.tracks, w.dropped, w.titles.clone());
        end(j);
        let path = j.wire.path.clone();
        *j = j.renewed(path, 0);
        j.wire.chosen = Some(track);
        j.wire.track = track;
        (j.wire.tracks, j.wire.dropped, j.wire.titles) = (tracks, dropped, titles);
        self.cleaned.retain(|&c| c != id);
        self.noting.retain(|&c| c != id);
        self.waiting_folder.retain(|&c| c != id);
        self.reconcile();
        Ok(())
    }

    /// Save `id` at the strength now, into `dir` (else next to the
    /// original). A file still cleaning saves once cleaned. The daemon
    /// guards this itself (`hushmic files save` reaches it without the
    /// page): no Save for a file that needs attention, a damaged file
    /// whose first part was not agreed to, an original that moved (it
    /// says Moved), or, in a sandbox, before a folder is chosen. A Save
    /// that failed for space can be tried again, into another folder.
    pub fn save(&mut self, id: u64, dir: Option<PathBuf>) -> Result<(), Refused> {
        let i = self.index(id).ok_or(Refused::NoSuchFile)?;
        if dir.as_ref().is_some_and(|d| !d.is_absolute()) {
            return Err(Refused::Relative);
        }
        let w = &self.list[i].wire;
        if w.state == JobState::NeedsAttention {
            return Err(Refused::NeedsAttention);
        }
        if w.damaged.is_some() && !w.damage_ok {
            return Err(Refused::Damaged);
        }
        if std::fs::metadata(&w.path).is_err() {
            let j = &mut self.list[i];
            (j.wire.saving, j.wire.save_problem) = (false, Some(Problem::Moved));
            return Err(Refused::Moved);
        }
        if dir.is_some() {
            self.start_save(id, dir, None);
            return Ok(());
        }
        // In a sandbox, "next to the original" is the chosen folder: looked
        // at first (off the loop), and the Save goes ahead once it answers.
        if self.folder.is_some() {
            // not reachable a moment ago: as for a gone one now, and it is
            // looked at again
            if self.folder_unreachable {
                self.look_at_folder();
                self.start_save(id, None, Some(FolderMiss::NoAnswer));
                return Ok(());
            }
            if !self.waiting_folder.contains(&id) {
                self.waiting_folder.push(id);
            }
            self.look_at_folder();
            return Ok(());
        }
        if self.needs_folder {
            return Err(Refused::NoFolder);
        }
        self.start_save(id, None, None);
        Ok(())
    }

    /// Save `id` into `dir` (None: next to the original; in a sandbox,
    /// which has none, nothing: the window asks for a folder).
    /// `miss`: the chosen folder was meant and is gone or did not answer,
    /// so this goes next to the original instead (the page says why).
    fn start_save(&mut self, id: u64, dir: Option<PathBuf>, miss: Option<FolderMiss>) {
        if dir.is_none() && self.needs_folder {
            return;
        }
        let Some(i) = self.index(id) else { return };
        let j = &mut self.list[i];
        if j.save.is_none() {
            j.save = Some((j.wire.db, dir));
            j.failed_db = None;
            (j.wire.saving, j.wire.save_problem) = (true, None);
            j.wire.save_detail = None;
            j.wire.folder_miss = miss;
            self.reconcile();
        }
    }

    /// Add a file; a file already on the page keeps its job. Its id.
    pub fn add(&mut self, path: &str) -> Result<u64, String> {
        absolute(path)?;
        if let Some(j) = self.list.iter().find(|j| j.wire.path == path) {
            return Ok(j.wire.id);
        }
        match std::fs::metadata(path) {
            Ok(m) if m.is_file() => {}
            Ok(_) => return Err(format!("not a file: {path}")),
            Err(e) => return Err(format!("{path}: {e}")),
        }
        let id = self.next_id;
        self.next_id += 1;
        self.list.push(Job::new(FileJob {
            id,
            path: path.to_string(),
            cache: self
                .root
                .join(format!("{PREFIX}{id}"))
                .display()
                .to_string(),
            db: self.strength,
            ..FileJob::default()
        }));
        self.reconcile();
        Ok(id)
    }

    /// Take a file off the page: its worker ends and its cache goes. One
    /// that quitting would lose (cleaned and not saved, or cleaning) can
    /// be brought back for [`UNDO_FOR`], as after Clear all
    /// ([`Jobs::undo_clear`]).
    pub fn remove(&mut self, id: u64) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        if self.played == Some(id) {
            self.played = None;
        }
        if self.next == Some(id) {
            self.next = None;
        }
        let w = &self.list[i].wire;
        if self.list[i].open() || w.saving || w.unsaved() {
            self.expire_cleared();
            let j = self.list.remove(i);
            self.hold_for_undo(vec![(i, j)], true);
            return true;
        }
        let mut j = self.list.remove(i);
        end(&mut j);
        self.reconcile();
        true
    }

    /// The user plays `id` from `at` (48 kHz frames): it cleans first, from
    /// there.
    pub fn play(&mut self, id: u64, at: u64) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        self.played = Some(id);
        let j = &mut self.list[i];
        if j.open() {
            match j.worker.as_mut() {
                Some(l) => l.w.send(Command::Front(at)),
                None => j.front = Some(at),
            }
        }
        self.reconcile();
        true
    }

    /// "Clean next": a file that waits goes before the others that wait.
    pub fn clean_next(&mut self, id: u64) -> bool {
        if !self.index(id).is_some_and(|i| self.list[i].open()) {
            return false;
        }
        self.next = Some(id);
        self.reconcile();
        true
    }

    /// Start a file that needs attention over.
    pub fn retry(&mut self, id: u64) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        let j = &mut self.list[i];
        if j.wire.state != JobState::NeedsAttention {
            return false;
        }
        end(j);
        let (path, stops) = (j.wire.path.clone(), j.wire.stops);
        *j = j.renewed(path, stops);
        self.reconcile();
        true
    }

    /// The same file found again at `path` (moved, or unreadable where it
    /// was): it keeps its id and place on the page and starts over.
    pub fn replace(&mut self, id: u64, path: &str) -> Result<(), String> {
        let i = self.index(id).ok_or("no such file")?;
        absolute(path)?;
        if self
            .list
            .iter()
            .any(|j| j.wire.path == path && j.wire.id != id)
        {
            return Err(format!("already on the page: {path}"));
        }
        match std::fs::metadata(path) {
            Ok(m) if m.is_file() => {}
            Ok(_) => return Err(format!("not a file: {path}")),
            Err(e) => return Err(format!("{path}: {e}")),
        }
        let j = &mut self.list[i];
        end(j);
        // another file: the voice's track again
        *j = j.renewed(path.to_string(), 0);
        j.wire.chosen = None;
        self.reconcile();
        Ok(())
    }

    /// A damaged file: the user chose to clean the part before the damage.
    pub fn clean_first(&mut self, id: u64) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        let w = &mut self.list[i].wire;
        w.damage_ok = w.damaged.is_some();
        w.damage_ok
    }

    /// Cancel a Save that waits for its file or runs. A copy that has
    /// already landed stays (and says where).
    pub fn cancel_save(&mut self, id: u64) -> bool {
        let Some(i) = self.index(id) else {
            return false;
        };
        let j = &mut self.list[i];
        if j.save.take().is_none() {
            return false;
        }
        j.wire.saving = false;
        if let Some((_, cancel)) = &j.delivering {
            cancel.store(true, Ordering::Release);
        }
        self.reconcile();
        true
    }

    /// Clear all: every file goes off the page, for [`UNDO_FOR`] only
    /// as far as [`Jobs::undo_clear`] can bring it back. Workers end (one
    /// still cleaning starts over on Undo), cleaned sound stays in its
    /// cache, and a Save under way is cancelled (one that waits stays
    /// asked for). An earlier Clear all that could still be undone ends
    /// for good. How many went.
    pub fn clear(&mut self) -> usize {
        self.expire_cleared();
        if self.list.is_empty() {
            return 0;
        }
        let held = std::mem::take(&mut self.list)
            .into_iter()
            .enumerate()
            .collect();
        (self.played, self.next) = (None, None);
        self.hold_for_undo(held, false)
    }

    /// Off the page, for [`UNDO_FOR`] only as far as Undo can bring them
    /// back (`one`: a row's ×): workers end (one still cleaning starts
    /// over on Undo), cleaned sound stays in its cache, and a Save under
    /// way is cancelled (one that waits stays asked for). How many.
    fn hold_for_undo(&mut self, mut held: Vec<(usize, Job)>, one: bool) -> usize {
        for (_, j) in held.iter_mut() {
            if let Some(mut l) = j.worker.take() {
                l.w.end();
            }
            if let Some(mut l) = j.probe.take() {
                l.w.end();
                j.looked = false;
            }
            if let Some((mut l, _)) = j.saver.take() {
                l.w.end();
            }
            j.wire.preparing = None;
            if let Some((_, cancel)) = &j.delivering {
                // kept: a copy that lands all the same says where (heard)
                cancel.store(true, Ordering::Release);
                j.save = None;
                j.wire.saving = false;
            }
            if j.open() {
                // its pass starts over on Undo, from a clean cache
                let _ = std::fs::remove_dir_all(&j.wire.cache);
                let (path, stops) = (j.wire.path.clone(), j.wire.stops);
                let save = j.save.take();
                *j = j.renewed(path, stops);
                j.wire.saving = save.is_some();
                j.save = save;
            }
        }
        self.cleaned
            .retain(|id| !held.iter().any(|(_, j)| j.wire.id == *id));
        self.waiting_folder
            .retain(|id| !held.iter().any(|(_, j)| j.wire.id == *id));
        let n = held.len();
        self.cleared = Some((Instant::now(), held));
        self.cleared_one = one;
        self.reconcile();
        n
    }

    /// Undo Clear all: the files come back in their order, before any
    /// added since; a row's ×: the file comes back where it was. Each with
    /// what it had (one added again meanwhile keeps its new row). False
    /// when there is nothing to undo.
    pub fn undo_clear(&mut self) -> bool {
        let Some((_, held)) = self.cleared.take() else {
            return false;
        };
        let mut next = 0;
        for (at, mut j) in held {
            if self.list.iter().any(|k| k.wire.path == j.wire.path) {
                j.delivering = None;
                end(&mut j);
                continue;
            }
            let to = if self.cleared_one { at } else { next };
            self.list.insert(to.min(self.list.len()), j);
            next += 1;
        }
        self.reconcile();
        true
    }

    /// The loop's heartbeat: a Clear all past [`UNDO_FOR`] ends for good.
    pub fn tick(&mut self) {
        if self
            .cleared
            .as_ref()
            .is_some_and(|(at, _)| at.elapsed() >= UNDO_FOR)
        {
            self.expire_cleared();
        }
    }

    /// The cleared files end: their caches go.
    fn expire_cleared(&mut self) {
        if let Some((_, held)) = self.cleared.take() {
            for (_, mut j) in held {
                end(&mut j);
            }
        }
    }

    /// A cleared file's Save that landed after all (its cancel came too
    /// late): Undo brings it back saved.
    fn heard_cleared(&mut self, ev: JobEvent) {
        let Some((_, held)) = self.cleared.as_mut() else {
            return;
        };
        let Some((_, j)) = held.iter_mut().find(|(_, j)| j.wire.id == ev.id) else {
            return;
        };
        if let Heard::Saved(res) = ev.heard {
            if j.delivering.take_if(|d| d.0 == ev.serial).is_some() {
                if let Ok(path) = res {
                    j.wire.saved_db = j.wire.prepared;
                    j.wire.saved = Some(path);
                }
            }
        }
    }

    /// The microphone's engine stepped down for want of CPU (or
    /// recovered), and whether an app uses HushMic: cleaning waits only
    /// while both hold (no call, nothing to keep clean), unless the user
    /// said "Clean anyway" ([`Jobs::clean_anyway`]) since the app began.
    pub fn set_live(&mut self, stepped_down: bool, in_use: bool) {
        let lanes = self.lanes_now();
        (self.stepped_down, self.in_use) = (stepped_down, in_use);
        if !in_use {
            self.anyway = false;
        }
        self.yield_now();
        // an app began or ended: one file at a time, or several (F8)
        if self.lanes_now() != lanes {
            self.reconcile();
        }
    }

    /// "Clean anyway": files clean on while an app uses HushMic, until
    /// none does.
    pub fn clean_anyway(&mut self) {
        self.anyway = self.in_use;
        self.yield_now();
    }

    fn yield_now(&mut self) {
        let on = self.stepped_down && self.in_use && !self.anyway;
        if self.yielding != on {
            self.yielding = on;
            self.reconcile();
        }
    }

    /// A worker's word.
    pub fn heard(&mut self, ev: JobEvent) {
        let Some(i) = self.index(ev.id) else {
            self.heard_cleared(ev);
            return;
        };
        if self.list[i].probe.as_ref().map(|l| l.serial) == Some(ev.serial) {
            self.heard_probe(i, ev.heard);
            return;
        }
        let j = &mut self.list[i];
        if let Heard::Saved(res) = ev.heard {
            let Some((_, cancel)) = j.delivering.take_if(|d| d.0 == ev.serial) else {
                return;
            };
            let cancelled = cancel.load(Ordering::Acquire);
            let into = j.save.as_ref().and_then(|(_, dir)| dir.clone());
            let at = j.save.as_ref().map(|s| s.0).or(j.wire.prepared);
            if !cancelled {
                j.save = None;
                j.wire.saving = false;
            }
            let mut folder_gone = false;
            match res {
                // A cancel too late to stop it: it is there all the same.
                Ok(path) => (j.wire.saved, j.wire.saved_db) = (Some(path), at),
                Err(_) if cancelled => {}
                Err((kind, detail)) => {
                    eprintln!("[hushmic] {}: save: {detail}", j.wire.path);
                    folder_gone = kind == Problem::FolderGone;
                    j.wire.save_detail = save_note(kind, &detail, into.as_deref(), &j.wire.path);
                    j.wire.save_problem = Some(kind);
                }
            }
            if folder_gone {
                self.save_without(i, into);
            }
            self.reconcile();
            return;
        }
        if j.saver.as_ref().map(|(l, _)| l.serial) == Some(ev.serial) {
            saver_heard(j, ev.heard);
            self.reconcile();
            return;
        }
        if j.worker.as_ref().map(|l| l.serial) != Some(ev.serial) {
            return; // an ended worker's last words
        }
        let w = &mut j.wire;
        match ev.heard {
            Heard::Said(Msg::Opened {
                rate,
                channels,
                stored,
                frames,
                video,
                codec,
                ffmpeg,
                reduced,
                tracks,
                cleaning,
                dropped,
                track,
                titles,
            }) => {
                (w.rate, w.channels, w.stored, w.frames) = (rate, channels, stored, frames);
                (w.track, w.titles) = (track, titles);
                j.looked = true;
                w.stated = frames;
                (w.video, w.codec, w.ffmpeg, w.reduced) = (video, codec, ffmpeg, reduced);
                (w.tracks, w.cleaning, w.dropped) = (tracks, cleaning, dropped);
                (w.more, w.more_done) = (vec![None; tracks.saturating_sub(1)], 0.0);
                // What the cleaned sound takes on disk, against what is
                // free (a length the file does not state is not checked):
                // every track it cleans, about as long as the first.
                let need = frames.map_or(0, |f| bytes_needed(f, stored) * cleaning.max(1) as u64);
                let free = (self.free)(&self.root).unwrap_or(u64::MAX);
                if need > free {
                    eprintln!(
                        "[hushmic] {}: the cleaned sound needs {need} bytes, {free} are free",
                        w.path
                    );
                    w.state = JobState::NeedsAttention;
                    w.problem = Some(Problem::NoSpace);
                    end(j);
                }
            }
            Heard::Said(Msg::Output {
                ext,
                same_format,
                sound_only,
            }) => (w.out_ext, w.same_format, w.sound_only) = (ext, same_format, sound_only),
            Heard::Said(Msg::Stored { stored }) => w.stored = stored,
            Heard::Said(Msg::Lit { ranges }) => w.lit = ranges,
            Heard::Said(Msg::Length { frames }) => (w.frames, w.exact) = (Some(frames), true),
            Heard::Said(Msg::Damaged { frames }) => w.damaged = Some(frames),
            // the tracks after the first, once it is through
            Heard::Said(Msg::More {
                cleaned, frames, ..
            }) => {
                let len = frames.or(w.frames).unwrap_or(0).max(1);
                let through = w.more_done.floor();
                w.more_done = through + (cleaned as f64 / len as f64).min(0.99) as f32;
            }
            Heard::Said(Msg::MoreDone { track, cleaned }) => {
                // its slot among the other tracks
                let slot = if track > w.track { track - 1 } else { track };
                if let Some(m) = w.more.get_mut(slot).filter(|_| track != w.track) {
                    *m = cleaned;
                }
                w.more_done = w.more_done.floor() + 1.0;
            }
            Heard::Said(Msg::Done) => {
                w.state = JobState::Done;
                w.stops = 0;
                self.cleaned.push(w.id);
                if let Some(l) = j.worker.as_mut() {
                    l.w.send(Command::Stop);
                }
            }
            Heard::Said(Msg::Error { kind, detail }) => {
                eprintln!("[hushmic] {}: {detail}", w.path);
                w.state = JobState::NeedsAttention;
                w.problem = Some(kind);
                if kind == Problem::Failed {
                    w.stops += 1;
                }
            }
            Heard::Said(
                Msg::Preparing { .. }
                | Msg::Prepared { .. }
                | Msg::SaveFailed { .. }
                | Msg::Probed { .. },
            )
            | Heard::Saved(_) => {}
            Heard::Gone => {
                if let Some(mut l) = j.worker.take() {
                    l.w.reap();
                }
                if j.open() {
                    eprintln!("[hushmic] {}: the file worker ended early", j.wire.path);
                    j.wire.state = JobState::NeedsAttention;
                    j.wire.problem = Some(Problem::Crashed);
                    j.wire.stops += 1;
                }
            }
        }
        if j.wire.state == JobState::NeedsAttention && j.save.take().is_some() {
            j.wire.saving = false;
            j.wire.save_problem = j.wire.problem;
        }
        // Done or failed while stopped (a yield, another file played in
        // between): it goes on, to read its stop and end. The cleaning
        // turns only look at open files, so nothing else would.
        if !j.open() {
            if let Some(l) = j.worker.as_mut().filter(|l| l.held) {
                l.w.hold(false);
                l.held = false;
            }
        }
        self.reconcile();
    }

    /// Quit: every worker ends, every Save under way is cancelled (and
    /// waited for, so no partial copy stays), and every cache goes.
    pub fn shutdown(&mut self) {
        for j in &self.list {
            if let Some((_, cancel)) = &j.delivering {
                cancel.store(true, Ordering::Release);
            }
        }
        self.spawner.finish(FINISH);
        self.expire_cleared();
        for mut j in self.list.drain(..) {
            end(&mut j);
        }
    }

    fn index(&self, id: u64) -> Option<usize> {
        self.list.iter().position(|j| j.wire.id == id)
    }

    /// The open files in the order they clean: the played one, then the
    /// page's order, with the one asked for next right after those that
    /// clean now (it stops none of them, and takes the first lane free).
    fn queue(&self) -> Vec<usize> {
        let open = |i: &usize| self.list[*i].open();
        let at = |id: Option<u64>| id.and_then(|id| self.index(id)).filter(open);
        let next = at(self.next);
        let mut order: Vec<usize> = at(self.played).into_iter().collect();
        for i in (0..self.list.len()).filter(open) {
            if !order.contains(&i) && Some(i) != next {
                order.push(i);
            }
        }
        if let Some(n) = next.filter(|n| !order.contains(n)) {
            let now = order
                .iter()
                .take_while(|&&i| {
                    let j = &self.list[i];
                    j.wire.state == JobState::Cleaning && j.worker.is_some()
                })
                .count();
            order.insert(now.min(self.lanes_now()), n);
        }
        order
    }

    /// How many files clean at once now: one while an app uses HushMic.
    fn lanes_now(&self) -> usize {
        if self.in_use {
            1
        } else {
            self.lanes
        }
    }

    /// The files whose turn it is.
    fn turns(&self) -> Vec<usize> {
        let mut q = self.queue();
        q.truncate(self.lanes_now());
        q
    }

    fn reconcile(&mut self) {
        self.reconcile_cleaning();
        self.reconcile_saves();
        self.reconcile_probes();
    }

    /// A first look at each file that waits its turn, [`PROBES`] at a
    /// time in order: one that can't be cleaned says so at once, not when
    /// its turn comes. Not while the microphone needs the CPU.
    fn reconcile_probes(&mut self) {
        if self.yielding {
            return;
        }
        let mut running = self.list.iter().filter(|j| j.probe.is_some()).count();
        for i in 0..self.list.len() {
            if running >= PROBES {
                return;
            }
            let j = &self.list[i];
            let wants = j.wire.state == JobState::Waiting
                && j.worker.is_none()
                && j.probe.is_none()
                && !j.looked;
            if !wants {
                continue;
            }
            // their own numbers: a pass's or a Save's never change with them
            let serial = PROBE_SERIALS + self.next_probe;
            self.next_probe += 1;
            let j = &mut self.list[i];
            let args = Args {
                file: PathBuf::from(&j.wire.path),
                cache: PathBuf::from(&j.wire.cache),
                model: self.engine.model.clone(),
                inference: self.engine.inference,
                ffmpeg: self.engine.ffmpeg.clone(),
                track: Pick::Auto,
                fake: self.engine.fake,
                probe: true,
                prepare: None,
            };
            match self.spawner.probe(&args, j.wire.id, serial) {
                Ok(w) => {
                    j.probe = Some(Live {
                        serial,
                        w,
                        held: false,
                    });
                    running += 1;
                }
                // its pass will tell
                Err(_) => j.looked = true,
            }
        }
    }

    /// The first look's word.
    fn heard_probe(&mut self, i: usize, heard: Heard) {
        let j = &mut self.list[i];
        match heard {
            Heard::Said(Msg::Probed { problem, detail }) => {
                j.looked = true;
                // a pass that started meanwhile knows better
                if let Some(kind) = problem.filter(|_| j.worker.is_none() && j.open()) {
                    eprintln!("[hushmic] {}: {detail}", j.wire.path);
                    j.wire.state = JobState::NeedsAttention;
                    j.wire.problem = Some(kind);
                    if j.save.take().is_some() {
                        j.wire.saving = false;
                        j.wire.save_problem = Some(kind);
                    }
                }
            }
            Heard::Gone => {
                if let Some(mut l) = j.probe.take() {
                    l.w.reap();
                }
                j.looked = true;
            }
            _ => {}
        }
        self.reconcile();
    }

    /// Bring the workers in line: the active file's runs (unless the
    /// microphone needs the CPU), every other open file's waits.
    fn reconcile_cleaning(&mut self) {
        loop {
            let turns = self.turns();
            for (i, j) in self.list.iter_mut().enumerate() {
                if !j.open() {
                    continue;
                }
                let turn = turns.contains(&i);
                if let Some(l) = j.worker.as_mut() {
                    let run = turn && !self.yielding;
                    if l.held == run {
                        l.w.hold(!run);
                        l.held = !run;
                    }
                }
                j.wire.state = if turn && j.worker.is_some() {
                    JobState::Cleaning
                } else {
                    JobState::Waiting
                };
            }
            let Some(i) = turns.into_iter().find(|&i| self.list[i].worker.is_none()) else {
                return;
            };
            if self.yielding {
                return;
            }
            // Started, or it could not start (it needs attention): the
            // next lane's file, if any.
            self.start(i);
        }
    }

    /// One output prepared at a time: for a file the user saves first,
    /// then the played one, then in order. A file whose output is ready at
    /// its Save's strength goes in place.
    fn reconcile_saves(&mut self) {
        let order = (0..self.list.len())
            .filter(|&i| self.list[i].save.is_some())
            .chain(self.played.and_then(|id| self.index(id)))
            .chain(0..self.list.len());
        let mut target = None;
        for i in order {
            let j = &self.list[i];
            if j.delivering.is_some() {
                continue;
            }
            if let Some(db) = j.stale() {
                target = Some((i, db));
                break;
            }
        }
        // While the microphone needs the CPU, only a Save prepares.
        let urgent = target.is_some_and(|(i, _)| self.list[i].save.is_some());
        for (i, j) in self.list.iter_mut().enumerate() {
            let Some((l, db)) = j.saver.as_mut() else {
                continue;
            };
            if target.is_some_and(|t| t == (i, *db)) {
                let run = !self.yielding || urgent;
                if l.held == run {
                    l.w.hold(!run);
                    l.held = !run;
                }
            } else if let Some((mut l, _)) = j.saver.take() {
                l.w.end();
            }
        }
        if let Some((i, db)) = target {
            if self.list[i].saver.is_none() && (!self.yielding || urgent) {
                self.start_saver(i, db);
            }
        }
        for j in self.list.iter_mut() {
            // Found damaged while its Save waited: nothing is written
            // until the user agrees to its first part, then saves again.
            if j.save.is_some() && j.wire.damaged.is_some() && !j.wire.damage_ok {
                j.save = None;
                j.wire.saving = false;
            }
            let ready = match &j.save {
                Some((db, _)) => j.wire.prepared == Some(*db),
                None => false,
            };
            if !ready || j.delivering.is_some() || j.saver.is_some() {
                continue;
            }
            let (Some(file), Some((db, dir))) = (j.prepared_file.clone(), j.save.clone()) else {
                continue;
            };
            let mark = j
                .wire
                .saved
                .as_ref()
                .and_then(|_| super::save::resave_mark(j.wire.saved_db, db));
            let serial = self.next_serial;
            self.next_serial += 1;
            let cancel = Arc::new(AtomicBool::new(false));
            j.delivering = Some((serial, cancel.clone()));
            let d = Delivery {
                file,
                original: PathBuf::from(&j.wire.path),
                dir,
                mark,
                cancel,
                journal: self.root.join(super::save::PARTIALS),
            };
            self.spawner.deliver(d, j.wire.id, serial);
        }
        for j in self.list.iter_mut() {
            if j.saver.is_none() {
                j.wire.preparing = None;
            }
        }
    }

    fn start_saver(&mut self, i: usize, db: f32) {
        let serial = self.next_serial;
        self.next_serial += 1;
        let j = &mut self.list[i];
        let w = &j.wire;
        let Some(frames) = w.damaged.or(w.frames) else {
            j.failed_db = Some(db);
            return;
        };
        let args = Args {
            file: PathBuf::from(&w.path),
            cache: PathBuf::from(&w.cache),
            model: self.engine.model.clone(),
            inference: self.engine.inference,
            ffmpeg: self.engine.ffmpeg.clone(),
            // the track its pass cleaned
            track: Pick::One(w.track),
            fake: self.engine.fake,
            probe: false,
            prepare: Some(Prepare {
                db,
                frames,
                stored: w.stored,
                more: w.more.clone(),
            }),
        };
        match self.spawner.spawn(&args, w.id, serial) {
            Ok(w) => {
                let held = false;
                j.saver = Some((Live { serial, w, held }, db));
                j.wire.preparing = Some(0.0);
            }
            Err(e) => {
                eprintln!("[hushmic] could not start the file worker: {e}");
                saver_failed(j, db, Problem::Crashed, None);
            }
        }
    }

    fn start(&mut self, i: usize) -> bool {
        let serial = self.next_serial;
        self.next_serial += 1;
        let j = &mut self.list[i];
        let args = Args {
            file: PathBuf::from(&j.wire.path),
            cache: PathBuf::from(&j.wire.cache),
            model: self.engine.model.clone(),
            inference: self.engine.inference,
            ffmpeg: self.engine.ffmpeg.clone(),
            track: j.wire.chosen.map_or(Pick::Auto, Pick::One),
            fake: self.engine.fake,
            probe: false,
            prepare: None,
        };
        match self.spawner.spawn(&args, j.wire.id, serial) {
            Ok(mut w) => {
                if let Some(at) = j.front.take() {
                    w.send(Command::Front(at));
                }
                j.worker = Some(Live {
                    serial,
                    w,
                    held: false,
                });
                j.wire.state = JobState::Cleaning;
                true
            }
            Err(e) => {
                eprintln!("[hushmic] could not start the file worker: {e}");
                j.wire.state = JobState::NeedsAttention;
                j.wire.problem = Some(Problem::Crashed);
                false
            }
        }
    }
}

/// The preparing worker's word.
fn saver_heard(j: &mut Job, heard: Heard) {
    let Some(db) = j.saver.as_ref().map(|s| s.1) else {
        return;
    };
    match heard {
        Heard::Said(Msg::Preparing { done }) => j.wire.preparing = Some(done),
        // Its last word: it is on its way out.
        Heard::Said(Msg::Prepared { db: at, file }) if at == db => {
            j.wire.prepared = Some(db);
            j.prepared_file = Some(PathBuf::from(file));
            if let Some((mut l, _)) = j.saver.take() {
                l.w.end();
            }
        }
        Heard::Said(Msg::SaveFailed { kind, detail }) => {
            eprintln!("[hushmic] {}: preparing the save: {detail}", j.wire.path);
            let note = save_note(kind, &detail, None, &j.wire.path);
            saver_failed(j, db, kind, note);
            if let Some((mut l, _)) = j.saver.take() {
                l.w.end();
            }
        }
        Heard::Gone => {
            if let Some((mut l, _)) = j.saver.take() {
                l.w.reap();
            }
            if j.wire.prepared != Some(db) && j.failed_db != Some(db) {
                eprintln!("[hushmic] {}: the file worker ended early", j.wire.path);
                saver_failed(j, db, Problem::Crashed, None);
            }
        }
        _ => {}
    }
}

/// The files cleaned since the last note ([`Jobs::take_cleaned`]), or
/// saved by the note's Save all ([`Jobs::take_saved`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Cleaned {
    pub count: usize,
    /// Each one's original and where it was saved, when every one of them
    /// was saved (Save all, or a Save asked while it cleaned).
    pub saved: Option<Vec<(PathBuf, PathBuf)>>,
    /// Of the note's Save all: how many weren't saved.
    pub failed: usize,
}

/// Preparing at `db` failed: a Save that waits for it fails with it
/// (`note`: what the page names, see [`save_note`]).
fn saver_failed(j: &mut Job, db: f32, kind: Problem, note: Option<String>) {
    j.failed_db = Some(db);
    if j.save.as_ref().is_some_and(|s| s.0 == db) {
        j.save = None;
        j.wire.saving = false;
        j.wire.save_problem = Some(kind);
        j.wire.save_detail = note;
    }
}

/// What the page names with a failed Save: the folder that can't be
/// written (`into`, else the original's), the encoder's reason, or the
/// short reason of a Save that failed otherwise. The other kinds say it
/// all.
fn save_note(kind: Problem, detail: &str, into: Option<&Path>, original: &str) -> Option<String> {
    match kind {
        Problem::NoWrite => {
            let dir = into.or_else(|| Path::new(original).parent())?;
            Some(dir.display().to_string())
        }
        Problem::Failed => Some(short_reason(detail)),
        // the encoder's own words: what to look up, or to report
        Problem::Encode => Some(short_reason(detail.trim_start_matches("ffmpeg: "))),
        _ => None,
    }
}

/// End a job's workers and remove its cache.
fn end(j: &mut Job) {
    if let Some(mut l) = j.worker.take() {
        l.w.end();
    }
    if let Some(mut l) = j.probe.take() {
        l.w.end();
    }
    if let Some((mut l, _)) = j.saver.take() {
        l.w.end();
    }
    let _ = std::fs::remove_dir_all(&j.wire.cache);
}

/// The daemon's start: the cache root's job folders of an earlier run (a
/// crash, a power cut; none is anyone's now), and what
/// [`sweep_stale`] takes.
pub fn sweep(root: &Path) {
    if let Ok(dir) = std::fs::read_dir(root) {
        for e in dir.flatten() {
            if e.file_name().to_string_lossy().starts_with(PREFIX) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    sweep_stale(root);
}

/// What processes that died left in the cache root: a `hushmic clean`'s
/// scratch folder that no run holds, and partial Save copies in the user's
/// folders (see `save::sweep_partials`). A live process's stay. The
/// daemon and `hushmic clean` both run it at their start.
pub fn sweep_stale(root: &Path) {
    sweep_stale_pausing(root, &mut |_| {});
}

/// [`sweep_stale`], calling `pause` with each lock it holds just before it
/// removes what the lock guards (tests: a run starting right then).
fn sweep_stale_pausing(root: &Path, pause: &mut dyn FnMut(&Path)) {
    use super::held::take;
    let names: Vec<String> = std::fs::read_dir(root)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(super::cli::SCRATCH_PREFIX))
                .collect()
        })
        .unwrap_or_default();
    // The folders first: a run holds its lock before its folder exists,
    // and only removes it after the folder. Each removal is made while the
    // sweep holds the lock itself (see `held::take`).
    for name in names.iter().filter(|n| !n.ends_with(".lock")) {
        let lock = root.join(format!("{name}.lock"));
        if let Some(_taken) = take(&lock) {
            pause(&lock);
            let _ = std::fs::remove_dir_all(root.join(name));
            let _ = std::fs::remove_file(lock);
        } else if !lock.exists() {
            // its run ended between removing the folder and the lock
            let _ = std::fs::remove_dir_all(root.join(name));
        }
    }
    // a lock whose folder never came (a run that ended in between)
    for name in names.iter().filter(|n| n.ends_with(".lock")) {
        let lock = root.join(name);
        if let Some(_taken) = take(&lock) {
            if !root.join(name.trim_end_matches(".lock")).exists() {
                pause(&lock);
                let _ = std::fs::remove_file(lock);
            }
        }
    }
    super::save::sweep_partials(&root.join(super::save::PARTIALS));
}

/// Workers as processes: this binary, `clean --worker`.
pub struct Processes {
    /// None: the binary that runs (`selfexe`), also after an upgrade.
    exe: Option<PathBuf>,
    tell: Arc<dyn Fn(JobEvent) + Send + Sync>,
    /// The Saves' threads, to wait for at Quit.
    saving: Vec<std::thread::JoinHandle<()>>,
}

impl Processes {
    /// Workers that run `exe` (tests). `tell` gets every worker's words,
    /// from a reader thread per worker.
    pub fn new(exe: PathBuf, tell: impl Fn(JobEvent) + Send + Sync + 'static) -> Processes {
        Processes {
            exe: Some(exe),
            tell: Arc::new(tell),
            saving: Vec::new(),
        }
    }

    /// Workers that run the binary that runs now, as the daemon's do.
    pub fn this_binary(tell: impl Fn(JobEvent) + Send + Sync + 'static) -> Processes {
        Processes {
            exe: None,
            tell: Arc::new(tell),
            saving: Vec::new(),
        }
    }
}

impl Spawner for Processes {
    fn spawn(&mut self, args: &Args, id: u64, serial: u64) -> io::Result<Box<dyn Worker>> {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;
        let mut c = match &self.exe {
            Some(exe) => std::process::Command::new(exe),
            None => crate::selfexe::command(),
        };
        c.args(["clean", "--worker"])
            .args(args.to_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        // SAFETY: one plain syscall between fork and exec. SIGKILL, not
        // SIGTERM: a stopped worker would hold a SIGTERM until continued.
        unsafe {
            c.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = c.spawn()?;
        let out = child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?;
        let tell = self.tell.clone();
        std::thread::Builder::new()
            .name("hushmic-file-worker".into())
            .spawn(move || {
                for line in io::BufReader::new(out).lines().map_while(Result::ok) {
                    match serde_json::from_str::<Msg>(&line) {
                        Ok(m) => tell(JobEvent {
                            id,
                            serial,
                            heard: Heard::Said(m),
                        }),
                        Err(_) => eprintln!("[hushmic] file worker: {line}"),
                    }
                }
                tell(JobEvent {
                    id,
                    serial,
                    heard: Heard::Gone,
                });
            })?;
        let stdin = child.stdin.take();
        Ok(Box::new(Process { child, stdin }))
    }

    fn deliver(&mut self, d: Delivery, id: u64, serial: u64) {
        let tell = self.tell.clone();
        let failed = self.tell.clone();
        self.saving.retain(|t| !t.is_finished());
        let run = move || {
            let res = super::save::deliver_noted(
                &d.file,
                &d.original,
                d.dir.as_deref(),
                d.mark.as_deref(),
                &d.cancel,
                Some(&d.journal),
            )
            .map(|p| p.display().to_string())
            .map_err(|e| (save_problem(&e), e.to_string()));
            tell(JobEvent {
                id,
                serial,
                heard: Heard::Saved(res),
            });
        };
        match std::thread::Builder::new()
            .name("hushmic-file-save".into())
            .spawn(run)
        {
            Ok(t) => self.saving.push(t),
            Err(e) => failed(JobEvent {
                id,
                serial,
                heard: Heard::Saved(Err((Problem::Failed, e.to_string()))),
            }),
        }
    }

    fn finish(&mut self, within: std::time::Duration) {
        let deadline = std::time::Instant::now() + within;
        for t in self.saving.drain(..) {
            while !t.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
    }
}

struct Process {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
}

impl Worker for Process {
    fn send(&mut self, c: Command) {
        if let Some(s) = self.stdin.as_mut() {
            let _ = s.write_all(c.line().as_bytes()).and_then(|_| s.flush());
        }
    }

    fn hold(&mut self, on: bool) {
        let sig = if on { libc::SIGSTOP } else { libc::SIGCONT };
        // SAFETY: a signal to our own child, not yet reaped.
        unsafe { libc::kill(self.child.id() as libc::pid_t, sig) };
    }

    fn end(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn reap(&mut self) {
        self.stdin = None;
        // Its output ended, so it is on its way out; one that lingers goes.
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if !matches!(self.child.try_wait(), Ok(Some(_))) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// What the fake workers were told, in order: "spawn 1", "hold 1",
    /// "cont 1", "end 1", "reap 1" (by job id), and the commands as sent
    /// ("front 480", "stop"); a preparing worker is "prepare 1 at 12"
    /// and the like, a Save "deliver 1 /dir".
    type Log = Arc<Mutex<Vec<String>>>;

    struct FakeWorker(u64, Log);

    impl Worker for FakeWorker {
        fn send(&mut self, c: Command) {
            self.1.lock().unwrap().push(c.line().trim().to_string());
        }
        fn hold(&mut self, on: bool) {
            let w = if on { "hold" } else { "cont" };
            self.1.lock().unwrap().push(format!("{w} {}", self.0));
        }
        fn end(&mut self) {
            self.1.lock().unwrap().push(format!("end {}", self.0));
        }
        fn reap(&mut self) {
            self.1.lock().unwrap().push(format!("reap {}", self.0));
        }
    }

    struct FakeSpawner(Log, bool);

    /// First looks go to a log of their own ([`probes`]), so the other
    /// tests' logs stay as they were.
    static PROBES_SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// The first looks started so far by jobs over `dir` (their files).
    fn probes(dir: &Path) -> Vec<String> {
        let d = dir.display().to_string();
        PROBES_SEEN
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with(&d))
            .cloned()
            .collect()
    }

    impl Spawner for FakeSpawner {
        fn spawn(&mut self, a: &Args, id: u64, _: u64) -> io::Result<Box<dyn Worker>> {
            if self.1 {
                return Err(io::ErrorKind::NotFound.into());
            }
            let what = match &a.prepare {
                Some(p) if !p.more.is_empty() => {
                    format!("prepare {id} at {} with {:?}", p.db, p.more)
                }
                Some(p) => format!("prepare {id} at {}", p.db),
                None => format!("spawn {id}"),
            };
            self.0.lock().unwrap().push(what);
            Ok(Box::new(FakeWorker(id, self.0.clone())))
        }

        fn deliver(&mut self, d: Delivery, id: u64, _: u64) {
            let dir = d.dir.map(|p| p.display().to_string()).unwrap_or_default();
            let mark = d.mark.map(|m| format!(" as {m}")).unwrap_or_default();
            self.0
                .lock()
                .unwrap()
                .push(format!("deliver {id} {dir}{mark}"));
        }

        fn probe(&mut self, a: &Args, id: u64, _: u64) -> io::Result<Box<dyn Worker>> {
            assert!(a.probe);
            PROBES_SEEN
                .lock()
                .unwrap()
                .push(a.file.display().to_string());
            Ok(Box::new(FakeWorker(id, Log::default())))
        }
    }

    fn engine() -> EngineChoice {
        EngineChoice {
            model: "m".into(),
            inference: Inference::Auto,
            ffmpeg: None,
            fake: true,
        }
    }

    /// Jobs over three real (empty) files in a scratch folder.
    fn setup(name: &str) -> (Jobs, Log, Vec<String>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("hushmic-jobs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<String> = (1..=3)
            .map(|k| {
                let p = dir.join(format!("f{k}.wav"));
                std::fs::write(&p, b"").unwrap();
                p.display().to_string()
            })
            .collect();
        let log: Log = Arc::default();
        let jobs = Jobs::new(
            dir.join("cache"),
            engine(),
            Box::new(FakeSpawner(log.clone(), false)),
        )
        .with_lanes(1);
        (jobs, log, files, dir)
    }

    fn take(log: &Log) -> Vec<String> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    fn states(j: &Jobs) -> Vec<JobState> {
        j.state().jobs.iter().map(|j| j.state).collect()
    }

    fn said(id: u64, serial: u64, m: Msg) -> JobEvent {
        JobEvent {
            id,
            serial,
            heard: Heard::Said(m),
        }
    }

    use JobState::{Cleaning, Done, NeedsAttention, Waiting};

    /// Job `id` (worker `serial`) cleaned: 10 s, then done; its worker
    /// gone.
    fn cleaned(j: &mut Jobs, id: u64, serial: u64) {
        j.heard(said(id, serial, Msg::Length { frames: 480_000 }));
        j.heard(said(id, serial, Msg::Done));
        j.heard(JobEvent {
            id,
            serial,
            heard: Heard::Gone,
        });
    }

    fn prepared(id: u64, serial: u64, db: f32) -> JobEvent {
        said(
            id,
            serial,
            Msg::Prepared {
                db,
                file: format!("/c/{id}/prepared.wav"),
            },
        )
    }

    /// An OBS recording: the first track, then the second; how far it is
    /// counts both, and Save prepares with each track's cleaned sound.
    #[test]
    fn every_sound_track_is_cleaned_and_saved() {
        let (mut j, log, f, dir) = setup("tracks");
        let a = j.add(&f[0]).unwrap();
        take(&log);
        let opened = Msg::Opened {
            rate: 48_000,
            channels: 2,
            stored: 2,
            frames: Some(480_000),
            video: true,
            codec: "aac".into(),
            ffmpeg: true,
            reduced: false,
            tracks: 3,
            cleaning: 2,
            dropped: 0,
            track: 0,
            titles: Vec::new(),
        };
        j.heard(said(a, 1, opened));
        let w = &j.state().jobs[0];
        assert_eq!((w.tracks, w.cleaning, w.more.len()), (3, 2, 2));
        j.heard(said(
            a,
            1,
            Msg::Lit {
                ranges: vec![(0, 480_000)],
            },
        ));
        assert!((j.state().jobs[0].cleaned_fraction() - 0.5).abs() < 1e-6);
        // the second track a quarter through: 5/8 of the file's work
        j.heard(said(
            a,
            1,
            Msg::More {
                track: 1,
                cleaned: 120_000,
                frames: Some(480_000),
            },
        ));
        assert!((j.state().jobs[0].cleaned_fraction() - 0.625).abs() < 1e-6);
        assert_eq!(states(&j), [Cleaning], "not done before every track");
        j.heard(said(
            a,
            1,
            Msg::MoreDone {
                track: 1,
                cleaned: Some((479_000, 1)),
            },
        ));
        assert!((j.state().jobs[0].cleaned_fraction() - 1.0).abs() < 1e-6);
        // (the third has no encoder: copied as it is, never cleaned)
        j.set_strength(24.0);
        cleaned(&mut j, a, 1);
        assert!(j.save(a, None).is_ok());
        let said = take(&log);
        assert!(
            said.contains(&"prepare 1 at 24 with [Some((479000, 1)), None]".to_string()),
            "{said:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn files_clean_one_at_a_time_in_order() {
        let (mut j, log, f, dir) = setup("order");
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        assert_eq!(j.add(&f[0]), Ok(a), "a file on the page keeps its job");
        assert_eq!(take(&log), ["spawn 1"]);
        assert_eq!(states(&j), [Cleaning, Waiting]);
        j.heard(said(a, 1, Msg::Done));
        // Told to stop; the next file starts at once.
        assert_eq!(states(&j), [Done, Cleaning]);
        assert_eq!(take(&log), ["stop", "spawn 2"]);
        j.heard(JobEvent {
            id: a,
            serial: 1,
            heard: Heard::Gone,
        });
        assert_eq!(take(&log), ["reap 1"]);
        assert_eq!(states(&j), [Done, Cleaning]);
        j.heard(said(
            b,
            2,
            Msg::Lit {
                ranges: vec![(0, 9)],
            },
        ));
        assert_eq!(j.state().jobs[1].lit, [(0, 9)]);
        assert!(j.add(dir.to_str().unwrap()).is_err(), "a folder is no file");
        assert!(j.add("/nonexistent/x.wav").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// F8: files clean side by side, as many as the machine has lanes
    /// for, while no app uses HushMic; one at a time while one does (the
    /// others wait stopped). "Clean next" puts a file before the others
    /// that wait, never stopping one that cleans.
    #[test]
    fn several_files_clean_at_once_until_an_app_uses_hushmic() {
        assert_eq!(
            [1, 4, 8, 12, 16, 24, 64].map(lanes_for),
            [1, 1, 2, 3, 3, 3, 3]
        );
        let (j, log, f, dir) = setup("lanes");
        let mut j = j.with_lanes(2);
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        let c = j.add(&f[2]).unwrap();
        assert_eq!(take(&log), ["spawn 1", "spawn 2"]);
        assert_eq!(states(&j), [Cleaning, Cleaning, Waiting]);
        assert_eq!(j.state().next_up, Some(c));
        // a call: one file cleans, the other waits stopped
        j.set_live(false, true);
        assert_eq!(take(&log), ["hold 2"]);
        assert_eq!(states(&j), [Cleaning, Waiting, Waiting]);
        assert_eq!(j.state().next_up, Some(b));
        // "Clean next" on the last: before b, but a cleans on
        assert!(j.clean_next(c));
        assert_eq!(take(&log), Vec::<String>::new());
        assert_eq!(j.state().next_up, Some(c));
        // the call ends: c takes the lane free, b waits stopped
        j.set_live(false, false);
        assert_eq!(take(&log), ["spawn 3"]);
        assert_eq!(states(&j), [Cleaning, Waiting, Cleaning]);
        assert_eq!(j.state().next_up, Some(b));
        j.heard(said(a, 1, Msg::Done));
        assert_eq!(take(&log), ["stop", "cont 2"]);
        assert_eq!(states(&j), [Done, Cleaning, Cleaning]);
        assert_eq!(j.state().next_up, None);
        // one lane: c asked for next goes before b, after a
        let (j2, log2, f2, dir2) = setup("next");
        let mut j2 = j2;
        let a2 = j2.add(&f2[0]).unwrap();
        let b2 = j2.add(&f2[1]).unwrap();
        let c2 = j2.add(&f2[2]).unwrap();
        take(&log2);
        assert_eq!(j2.state().next_up, Some(b2));
        assert!(j2.clean_next(c2));
        assert_eq!(take(&log2), Vec::<String>::new(), "a cleans on");
        assert_eq!(j2.state().next_up, Some(c2));
        j2.heard(said(a2, 1, Msg::Done));
        assert_eq!(take(&log2), ["stop", "spawn 3"]);
        assert_eq!(states(&j2), [Done, Waiting, Cleaning]);
        let _ = std::fs::remove_dir_all(dir2);
        assert!(!j.clean_next(a), "a cleaned file has no turn");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_played_file_goes_first_and_the_other_waits_stopped() {
        let (mut j, log, f, dir) = setup("play");
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        take(&log);
        assert!(j.play(b, 48_000));
        assert_eq!(take(&log), ["hold 1", "spawn 2", "front 48000"]);
        assert_eq!(states(&j), [Waiting, Cleaning]);
        // A seek in the playing file goes straight to its worker.
        j.play(b, 96_000);
        assert_eq!(take(&log), ["front 96000"]);
        j.heard(said(b, 2, Msg::Done));
        assert_eq!(take(&log), ["stop", "cont 1"]);
        assert_eq!(states(&j), [Cleaning, Done]);
        // Playing a done file changes nothing for the others.
        j.play(b, 0);
        assert_eq!(take(&log), Vec::<String>::new());
        assert!(!j.play(99, 0));
        let _ = a;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// What the page tells a file stopped partway ("play it to continue"):
    /// playing it cleans it on at once, from where it is played.
    #[test]
    fn playing_a_file_stopped_partway_cleans_it_on() {
        let (mut j, log, f, dir) = setup("replay");
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        assert!(j.play(b, 0));
        take(&log);
        assert_eq!(states(&j), [Waiting, Cleaning], "a waits stopped");
        assert!(j.play(a, 48_000));
        let heard = take(&log);
        assert!(heard.contains(&"cont 1".to_string()), "{heard:?}");
        assert!(heard.contains(&"hold 2".to_string()), "{heard:?}");
        assert_eq!(states(&j), [Cleaning, Waiting]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cleaning_waits_while_the_microphone_needs_the_cpu() {
        let (mut j, log, f, dir) = setup("yield");
        j.set_live(true, true);
        let a = j.add(&f[0]).unwrap();
        assert_eq!(take(&log), Vec::<String>::new(), "nothing starts");
        assert!(j.state().yielding);
        j.set_live(false, true);
        assert_eq!(take(&log), ["spawn 1"]);
        j.set_live(true, true);
        assert_eq!(take(&log), ["hold 1"]);
        assert_eq!(states(&j), [Cleaning], "still its turn, held");
        j.set_live(true, true);
        assert_eq!(take(&log), Vec::<String>::new());
        j.set_live(false, true);
        assert_eq!(take(&log), ["cont 1"]);
        let _ = a;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// F2: only a call is kept clean at a file's cost. A busy computer
    /// with no app using HushMic cleans on; "Clean anyway" cleans on
    /// during a call, until it ends (the next call waits again).
    #[test]
    fn cleaning_waits_only_while_an_app_uses_hushmic() {
        let (mut j, log, f, dir) = setup("yield-app");
        j.set_live(true, false);
        let a = j.add(&f[0]).unwrap();
        assert_eq!(take(&log), ["spawn 1"], "no call: it cleans");
        assert!(!j.state().yielding);
        j.set_live(true, true);
        assert_eq!(take(&log), ["hold 1"]);
        assert!(j.state().yielding);
        j.clean_anyway();
        assert_eq!(take(&log), ["cont 1"]);
        assert!(!j.state().yielding);
        // still the same call: it stays so
        j.set_live(true, true);
        assert_eq!(take(&log), Vec::<String>::new());
        // the call ends, a new one starts: it waits again
        j.set_live(true, false);
        j.set_live(true, true);
        assert_eq!(take(&log), ["hold 1"]);
        // no call: "Clean anyway" is nothing to keep
        j.set_live(false, false);
        j.clean_anyway();
        j.set_live(true, true);
        assert!(j.state().yielding);
        let _ = (a, std::fs::remove_dir_all(dir));
    }

    #[test]
    fn a_stopped_worker_that_finishes_goes_on_to_end() {
        let (mut j, log, f, dir) = setup("heldend");
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        j.set_live(true, true);
        assert_eq!(take(&log), ["spawn 1", "hold 1"]);
        // Its last word was on its way as it was stopped.
        j.heard(said(a, 1, Msg::Done));
        assert_eq!(take(&log), ["stop", "cont 1"]);
        // The same for one that fails while another file is played.
        j.set_live(false, true);
        assert_eq!(take(&log), ["spawn 2"]);
        j.play(a, 0);
        let c = j.add(&f[2]).unwrap();
        j.play(c, 0);
        assert_eq!(take(&log), ["hold 2", "spawn 3", "front 0"]);
        j.heard(said(
            b,
            2,
            Msg::Error {
                kind: Problem::Failed,
                detail: "x".into(),
            },
        ));
        assert_eq!(take(&log), ["cont 2"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_ends_the_worker_and_the_cache() {
        let (mut j, log, f, dir) = setup("remove");
        let a = j.add(&f[0]).unwrap();
        j.add(&f[1]).unwrap();
        let cache = PathBuf::from(&j.state().jobs[0].cache);
        std::fs::create_dir_all(&cache).unwrap();
        take(&log);
        assert!(j.remove(a));
        assert!(!cache.exists());
        assert_eq!(take(&log), ["end 1", "spawn 2"]);
        // The removed worker's late words change nothing.
        j.heard(said(a, 1, Msg::Done));
        assert_eq!(states(&j), [Cleaning]);
        assert!(!j.remove(a));
        j.shutdown();
        assert_eq!(take(&log), ["end 2"]);
        assert!(j.state().jobs.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn errors_crashes_and_full_disks_need_attention_and_retry_starts_over() {
        let (j, log, f, dir) = setup("problems");
        let mut j = j.with_free(|_| Ok(1_000_000));
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        let c = j.add(&f[2]).unwrap();
        take(&log);
        j.heard(said(
            a,
            1,
            Msg::Error {
                kind: Problem::Unsupported,
                detail: "x".into(),
            },
        ));
        assert_eq!(states(&j), [NeedsAttention, Cleaning, Waiting]);
        assert_eq!(j.state().jobs[0].problem, Some(Problem::Unsupported));
        // An hour of mono needs far more than 1 MB.
        let opened = Msg::Opened {
            rate: 48_000,
            channels: 1,
            stored: 1,
            frames: Some(48_000 * 3600),
            video: false,
            codec: "flac".into(),
            ffmpeg: false,
            reduced: false,
            tracks: 1,
            cleaning: 1,
            dropped: 0,
            track: 0,
            titles: Vec::new(),
        };
        j.heard(said(b, 2, opened));
        assert_eq!(j.state().jobs[1].problem, Some(Problem::NoSpace));
        j.heard(JobEvent {
            id: c,
            serial: 3,
            heard: Heard::Gone,
        });
        assert_eq!(states(&j), [NeedsAttention; 3]);
        assert_eq!(j.state().jobs[2].problem, Some(Problem::Crashed));
        assert_eq!(take(&log), ["spawn 2", "end 2", "spawn 3", "reap 3"]);
        assert!(j.retry(a));
        assert_eq!(states(&j), [Cleaning, NeedsAttention, NeedsAttention]);
        assert_eq!(j.state().jobs[0].problem, None);
        assert!(!j.retry(a), "only a file that needs attention");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_worker_that_cannot_start_hands_the_turn_on() {
        let dir = std::env::temp_dir().join(format!("hushmic-jobs-{}-nostart", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.wav");
        std::fs::write(&p, b"").unwrap();
        let log: Log = Arc::default();
        let mut j = Jobs::new(dir.join("c"), engine(), Box::new(FakeSpawner(log, true)));
        j.add(p.to_str().unwrap()).unwrap();
        assert_eq!(states(&j), [NeedsAttention]);
        assert_eq!(j.state().jobs[0].problem, Some(Problem::Crashed));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_cleaned_file_is_prepared_at_the_strength_and_again_when_it_changes() {
        let (mut j, log, f, dir) = setup("prepare");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        cleaned(&mut j, a, 1);
        // Serials: 1 A's pass, 2 B's pass, 3 A's preparation.
        assert_eq!(
            take(&log),
            ["spawn 1", "stop", "spawn 2", "prepare 1 at 12", "reap 1"]
        );
        j.heard(prepared(a, 3, 12.0));
        assert_eq!(j.state().jobs[0].prepared, Some(12.0));
        assert_eq!(take(&log), ["end 1"]);
        // A new strength: prepared again; one changed midway ends that.
        assert!(j.set_file_strength(a, 20.0));
        assert_eq!(take(&log), ["prepare 1 at 20"]);
        j.set_file_strength(a, 6.0);
        assert_eq!(take(&log), ["end 1", "prepare 1 at 6"]);
        assert_eq!(j.state().strength, 6.0);
        // Its words come too late: nothing changes.
        j.heard(prepared(a, 4, 20.0));
        assert_eq!(j.state().jobs[0].prepared, Some(12.0));
        // Back to 12, ready already: the preparation at 6 ends.
        j.set_file_strength(a, 12.0);
        assert_eq!(take(&log), ["end 1"]);
        let _ = (b, std::fs::remove_dir_all(dir));
    }

    #[test]
    fn save_waits_for_the_cleaning_and_the_output_then_puts_it_in_place() {
        let (mut j, log, f, dir) = setup("save");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        take(&log);
        // Asked while cleaning: nothing yet.
        assert!(j.save(a, Some("/out".into())).is_ok());
        assert!(j.state().jobs[0].saving);
        assert_eq!(take(&log), Vec::<String>::new());
        // A strength changed after Save: Save keeps the one it was asked at.
        j.set_file_strength(a, 30.0);
        cleaned(&mut j, a, 1);
        assert_eq!(take(&log), ["stop", "prepare 1 at 12", "reap 1"]);
        // The next preparation (at 30) waits for the Save.
        j.heard(prepared(a, 2, 12.0));
        assert_eq!(take(&log), ["end 1", "deliver 1 /out"]);
        j.heard(JobEvent {
            id: a,
            serial: 3,
            heard: Heard::Saved(Ok("/out/f1 (cleaned).wav".into())),
        });
        let s = &j.state().jobs[0];
        assert!(!s.saving);
        assert_eq!(s.saved.as_deref(), Some("/out/f1 (cleaned).wav"));
        assert_eq!(take(&log), ["prepare 1 at 30"]);
        // Save at a strength that is ready: in place at once.
        j.heard(prepared(a, 4, 30.0));
        assert!(j.save(a, None).is_ok());
        // saved at 12 before: this one's name says 30 dB
        assert_eq!(take(&log), ["end 1", "deliver 1  as 30 dB"]);
        j.heard(JobEvent {
            id: a,
            serial: 5,
            heard: Heard::Saved(Err((Problem::Moved, "gone".into()))),
        });
        let s = &j.state().jobs[0];
        assert_eq!((s.saving, s.save_problem), (false, Some(Problem::Moved)));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Clear all takes every file off the page for a moment: Undo brings
    /// them back in order with what they had (the cleaned one cleaned,
    /// the one cleaning queued again), and after [`UNDO_FOR`] they end
    /// for good, caches and all (A1).
    #[test]
    fn clear_all_can_be_undone_for_a_moment() {
        let (mut j, log, f, dir) = setup("clear");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        let c = j.add(&f[2]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 3, 12.0));
        assert_eq!(states(&j), [Done, Cleaning, Waiting]);
        let cache = PathBuf::from(&j.state().jobs[0].cache);
        std::fs::create_dir_all(&cache).unwrap();
        // cleaned and not saved: quitting would lose it
        assert_eq!((j.busy(), j.at_stake()), (2, 3));
        take(&log);
        assert_eq!(j.clear(), 3);
        assert!(take(&log).contains(&"end 2".to_string()), "b's pass ends");
        let s = j.state();
        assert_eq!((s.jobs.len(), s.cleared), (0, 3));
        assert_eq!((j.busy(), j.at_stake()), (0, 0));
        assert!(cache.is_dir(), "the cleaned sound stays for Undo");
        assert!(j.undo_clear());
        let s = j.state();
        assert_eq!(s.cleared, 0);
        let ids: Vec<u64> = s.jobs.iter().map(|w| w.id).collect();
        assert_eq!(ids, [a, b, c]);
        assert_eq!(states(&j), [Done, Cleaning, Waiting]);
        assert_eq!(s.jobs[0].prepared, Some(12.0));
        assert_eq!(take(&log), ["spawn 2"], "b cleans again");
        assert!(!j.undo_clear(), "once");
        // A file added again meanwhile keeps its new row; Undo after the
        // moment has passed brings nothing back, and the caches go.
        j.clear();
        let again = j.add(&f[1]).unwrap();
        assert_ne!(again, b);
        assert!(j.undo_clear());
        let ids: Vec<u64> = j.state().jobs.iter().map(|w| w.id).collect();
        assert_eq!(ids, [a, c, again]);
        j.clear();
        j.tick();
        assert_eq!(j.state().cleared, 3, "not yet");
        j.cleared.as_mut().unwrap().0 -= UNDO_FOR;
        j.tick();
        assert_eq!(j.state().cleared, 0);
        assert!(!j.undo_clear());
        assert!(j.state().jobs.is_empty());
        assert!(!cache.exists(), "gone for good");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn quit_from_a_terminal_says_what_it_would_lose() {
        let (mut j, _log, f, dir) = setup("quit-refusal");
        assert_eq!(j.quit_refusal(), None);
        let a = j.add(&f[0]).unwrap();
        assert_eq!(
            j.quit_refusal().as_deref(),
            Some("1 file is cleaning; use --force to quit anyway")
        );
        let _b = j.add(&f[1]).unwrap();
        cleaned(&mut j, a, 1);
        assert_eq!(
            j.quit_refusal().as_deref(),
            Some("1 cleaned file isn't saved and 1 is cleaning; use --force to quit anyway")
        );
        let _c = j.add(&f[2]).unwrap();
        assert_eq!(
            j.quit_refusal().as_deref(),
            Some("1 cleaned file isn't saved and 2 are cleaning; use --force to quit anyway")
        );
        j.list[0].wire.saved = Some("/x".into());
        j.clear();
        assert_eq!(
            j.quit_refusal(),
            None,
            "what Clear all took is not at stake"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// F3: "Save all and quit" saves what quitting would lose, a file
    /// still cleaning once cleaned, then says quit; a Save that fails keeps
    /// HushMic up.
    #[test]
    fn save_all_and_quit_quits_once_everything_is_saved() {
        let (mut j, log, f, dir) = setup("save-quit");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 3, 12.0));
        take(&log);
        assert_eq!(j.save_all_and_quit(), Ok(2));
        assert!(!j.quit_ready(), "b still cleans");
        j.heard(JobEvent {
            id: a,
            serial: 4,
            heard: Heard::Saved(Ok("/x/f1 (cleaned).wav".into())),
        });
        assert!(!j.quit_ready());
        cleaned(&mut j, b, 2);
        let s = j.state().jobs[1].clone();
        assert!(s.saving, "saves once cleaned");
        assert!(!j.quit_ready());
        j.heard(prepared(b, 5, 12.0));
        j.heard(JobEvent {
            id: b,
            serial: 6,
            heard: Heard::Saved(Ok("/x/f2 (cleaned).wav".into())),
        });
        assert!(j.quit_ready());
        assert!(!j.quit_ready(), "said once");
        // a failed Save: no quit, and it isn't waited for again
        let c = j.add(&f[2]).unwrap();
        j.play(c, 0);
        let _ = (c, j.save_all_and_quit());
        j.heard(said(
            c,
            7,
            Msg::Error {
                kind: Problem::Unreadable,
                detail: "gone".into(),
            },
        ));
        assert!(!j.quit_ready());
        assert!(!j.quit_ready());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A104 SF-Q1: a file that came to be at stake after "Save all and
    /// quit" was chosen keeps HushMic up: the window asks again. The page
    /// says how many files quitting waits for; Cancel stops the quit.
    #[test]
    fn a_file_added_while_quitting_is_asked_about() {
        let (mut j, _log, f, dir) = setup("save-quit-late");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 3, 12.0));
        assert_eq!(j.save_all_and_quit(), Ok(2));
        assert_eq!(j.state().quitting, Some(2));
        let saved = |id, serial, name: &str| JobEvent {
            id,
            serial,
            heard: Heard::Saved(Ok(name.into())),
        };
        j.heard(saved(a, 4, "/x/f1 (cleaned).wav"));
        assert_eq!(j.state().quitting, Some(1), "b is left");
        cleaned(&mut j, b, 2);
        j.heard(prepared(b, 5, 12.0));
        let _c = j.add(&f[2]).unwrap();
        j.heard(saved(b, 6, "/x/f2 (cleaned).wav"));
        assert_eq!(j.at_stake(), 1);
        assert!(!j.quit_ready(), "c would be lost");
        let s = j.state();
        assert!(s.ask_quit, "the window asks again");
        assert_eq!(s.quitting, None);
        assert!(!j.quit_ready());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A104 SF-Q1: quitting takes HushMic out of the call, so a file
    /// still cleaning doesn't wait for the call to end; Cancel puts the
    /// wait back, and keeps HushMic up.
    #[test]
    fn save_all_and_quit_cleans_through_a_call_until_cancelled() {
        let (mut j, _log, f, dir) = setup("save-quit-call");
        let a = j.add(&f[0]).unwrap();
        j.set_live(true, true);
        assert!(j.state().yielding);
        assert_eq!(j.save_all_and_quit(), Ok(1));
        assert!(!j.state().yielding, "cleans on through the call");
        assert!(j.cancel_quit());
        assert!(j.state().yielding, "waits for the call again");
        assert_eq!(j.state().quitting, None);
        assert!(!j.cancel_quit(), "nothing to cancel");
        cleaned(&mut j, a, 1);
        assert!(!j.quit_ready(), "cancelled: no quit");
        // said "Clean anyway" before: Cancel leaves it so
        let (mut j, _log, f, dir2) = setup("save-quit-call-anyway");
        j.add(&f[0]).unwrap();
        j.set_live(true, true);
        j.clean_anyway();
        j.save_all_and_quit().unwrap();
        j.cancel_quit();
        assert!(!j.state().yielding);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    /// A104 SF-Q1 (c): a Save that waits for the chosen folder's look is
    /// waited for too, then HushMic quits.
    #[test]
    fn save_all_and_quit_waits_for_the_folder() {
        let (mut j, log, f, dir) = setup("save-quit-folder");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        j.set_folder(true, Some(out.clone()));
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, j.next_serial - 1, j.strength));
        take(&log);
        assert_eq!(j.save_all_and_quit(), Ok(1));
        assert!(
            j.waiting_folder.contains(&a),
            "the folder is looked at first"
        );
        assert!(!j.quit_ready(), "its Save waits for the look");
        looked(&mut j);
        let spawned = take(&log);
        assert!(
            spawned.iter().any(|l| l.starts_with("deliver 1 ")),
            "the Save went ahead: {spawned:?}"
        );
        let serial = j.next_serial - 1;
        assert!(!j.quit_ready());
        j.heard(JobEvent {
            id: a,
            serial,
            heard: Heard::Saved(Ok(out.join("f1 (cleaned).wav").display().to_string())),
        });
        assert!(j.quit_ready());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A104 N-T1, N-T2: another track starts over on it and keeps what
    /// is known of the file's tracks (the picker stays; a second change
    /// is checked against them); the track already cleaned, or one the
    /// file hasn't, is said so.
    #[test]
    fn another_track_keeps_the_files_tracks() {
        let (mut j, _log, f, dir) = setup("track-change");
        let a = j.add(&f[0]).unwrap();
        j.heard(said(
            a,
            1,
            Msg::Opened {
                rate: 48_000,
                channels: 2,
                stored: 2,
                frames: Some(480_000),
                video: true,
                codec: "aac".into(),
                ffmpeg: true,
                reduced: false,
                tracks: 2,
                cleaning: 1,
                dropped: 0,
                track: 0,
                titles: vec!["Desktop".into(), "Mic".into()],
            },
        ));
        assert_eq!(
            j.set_track(a, 0),
            Err("it already cleans sound track 1".into())
        );
        assert_eq!(
            j.set_track(a, 2),
            Err("there is no sound track 3 (it has 2)".into())
        );
        assert_eq!(j.set_track(9, 0), Err("no such file".into()));
        assert_eq!(j.set_track(a, 1), Ok(()));
        let w = j.state().jobs[0].clone();
        assert_eq!((w.chosen, w.track, w.tracks), (Some(1), 1, 2));
        assert_eq!(w.titles, ["Desktop", "Mic"]);
        assert_eq!(
            j.set_track(a, 2),
            Err("there is no sound track 3 (it has 2)".into()),
            "checked before the worker opens it again"
        );
        assert_eq!(j.set_track(a, 0), Ok(()));
        assert_eq!(j.state().jobs[0].chosen, Some(0));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// F3: a row's × on a file quitting would lose (cleaned and not
    /// saved, or cleaning) can be undone as Clear all can: it comes back
    /// where it was, cleaned; a saved one goes at once.
    #[test]
    fn a_removed_unsaved_file_can_be_undone() {
        let (mut j, log, f, dir) = setup("remove-undo");
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        let c = j.add(&f[2]).unwrap();
        // played first: it cleans first
        j.play(b, 0);
        cleaned(&mut j, b, 2);
        assert_eq!(states(&j), [Cleaning, Done, Waiting]);
        take(&log);
        assert!(j.remove(b));
        let s = j.state();
        assert_eq!((s.cleared, s.cleared_name.as_deref()), (1, Some("f2.wav")));
        assert!(j.undo_clear());
        let ids: Vec<u64> = j.state().jobs.iter().map(|w| w.id).collect();
        assert_eq!(ids, [a, b, c], "back in its place");
        assert_eq!(states(&j), [Cleaning, Done, Waiting]);
        // nothing at stake: off the page for good, no toast
        j.list[1].wire.saved = Some("/x/f2 (cleaned).wav".into());
        assert!(j.remove(b));
        let s = j.state();
        assert_eq!((s.cleared, s.cleared_name), (0, None));
        assert!(!j.undo_clear());
        // Clear all says no name
        j.clear();
        assert_eq!((j.state().cleared, j.state().cleared_name), (2, None));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A Save under way when Clear all comes is cancelled; one that lands
    /// all the same is saved after Undo. A saved copy says its strength
    /// (A2), so the row can offer Save at another.
    #[test]
    fn a_saved_copy_says_its_strength_and_clear_all_cancels_a_save() {
        let (mut j, log, f, dir) = setup("clear-save");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 2, 12.0));
        assert!(j.save(a, None).is_ok());
        take(&log);
        j.heard(JobEvent {
            id: a,
            serial: 3,
            heard: Heard::Saved(Ok("/x/f1 (cleaned).wav".into())),
        });
        let s = &j.state().jobs[0];
        assert_eq!(s.saved_db, Some(12.0));
        assert!(!s.unsaved());
        assert_eq!(j.at_stake(), 0, "saved: nothing to lose");
        // at 24 now; Save writes at 24
        j.set_file_strength(a, 24.0);
        j.heard(prepared(a, 4, 24.0));
        assert!(j.save(a, None).is_ok());
        assert!(j.state().jobs[0].saving);
        // F5: its name says the other strength ("(cleaned, Strong)")
        assert!(take(&log).contains(&format!("deliver {a}  as Strong")));
        assert_eq!(j.at_stake(), 1);
        j.clear();
        assert_eq!(j.at_stake(), 0);
        // the cancel came too late: it landed
        j.heard(JobEvent {
            id: a,
            serial: 5,
            heard: Heard::Saved(Ok("/x/f1 (cleaned) (2).wav".into())),
        });
        assert!(j.undo_clear());
        let s = &j.state().jobs[0];
        assert!(!s.saving);
        assert_eq!(s.saved.as_deref(), Some("/x/f1 (cleaned) (2).wav"));
        assert_eq!(s.saved_db, Some(24.0));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// B11: the note's Save all saves every cleaned file not saved (not
    /// one that needs attention), and says once all are through: how many
    /// were saved, and how many weren't.
    #[test]
    fn the_notes_save_all_saves_the_cleaned_and_reports_once() {
        let (mut j, log, f, dir) = setup("save-all");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        let c = j.add(&f[2]).unwrap();
        cleaned(&mut j, a, 1);
        cleaned(&mut j, b, 2);
        // c's pass stopped: nothing to save
        j.heard(said(
            c,
            4,
            Msg::Error {
                kind: Problem::Failed,
                detail: "x".into(),
            },
        ));
        assert_eq!(j.take_cleaned().map(|c| c.count), Some(2));
        take(&log);
        assert_eq!(j.save_all(), Ok(2));
        assert!(j.state().jobs[0].saving && j.state().jobs[1].saving);
        assert!(!j.state().jobs[2].saving);
        assert_eq!(j.take_saved(), None, "not through yet");
        // the two Saves end (by hand: the fake spawner prepares and
        // delivers nothing): a in place, b's folder can't be written
        for (id, k) in [(a, 0usize), (b, 1)] {
            let jj = &mut j.list[k];
            jj.save = None;
            jj.wire.saving = false;
            if id == a {
                jj.wire.saved = Some("/x/f1 (cleaned).wav".into());
            } else {
                jj.wire.save_problem = Some(Problem::NoWrite);
            }
        }
        let n = j.take_saved().expect("through");
        assert_eq!((n.count, n.failed), (1, 1));
        assert_eq!(
            n.saved,
            Some(vec![(
                PathBuf::from(&f[0]),
                PathBuf::from("/x/f1 (cleaned).wav")
            )])
        );
        assert_eq!(j.take_saved(), None, "once");
        // in a sandbox with no folder yet: the window must ask
        j.set_folder(true, None);
        j.list[1].wire.save_problem = None;
        assert_eq!(j.save_all(), Err(Refused::NoFolder));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// B9: each file that waits gets a first look, two at a time; one
    /// that can't be cleaned is marked at once, with what is wrong, and
    /// the queue goes on without it.
    #[test]
    fn a_file_that_waits_is_looked_at_and_marked_at_once() {
        let (mut j, log, f, dir) = setup("probe");
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        let c = j.add(&f[2]).unwrap();
        // a cleans (its pass looks for itself); b and c are looked at
        assert_eq!(probes(&dir), [f[1].clone(), f[2].clone()]);
        assert_eq!(take(&log), ["spawn 1"]);
        let probe_serial = |j: &Jobs, id: u64| {
            j.list[j.index(id).unwrap()]
                .probe
                .as_ref()
                .map(|l| l.serial)
                .unwrap()
        };
        let (sb, sc) = (probe_serial(&j, b), probe_serial(&j, c));
        j.heard(said(
            b,
            sb,
            Msg::Probed {
                problem: Some(Problem::Incomplete),
                detail: "cut".into(),
            },
        ));
        j.heard(said(
            c,
            sc,
            Msg::Probed {
                problem: None,
                detail: String::new(),
            },
        ));
        assert_eq!(states(&j), [Cleaning, NeedsAttention, Waiting]);
        assert_eq!(j.state().jobs[1].problem, Some(Problem::Incomplete));
        // a is done: c's turn, not b's
        cleaned(&mut j, a, 1);
        assert_eq!(states(&j), [Done, NeedsAttention, Cleaning]);
        // c's look ends after its word: reaped, and not looked at again
        j.heard(JobEvent {
            id: c,
            serial: sc,
            heard: Heard::Gone,
        });
        assert!(j.list[2].looked && j.list[2].probe.is_none());
        // b tried again goes first (its pass looks for itself)
        j.retry(b);
        assert!(j.list[1].probe.is_none() && j.list[1].worker.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The guards review's mutations got past (N-F4).
    #[test]
    fn clear_all_starts_a_cleaning_file_over_on_undo() {
        let (mut j, log, f, dir) = setup("clear-reset");
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        // a is half cleaned, with sound in its cache; b waits
        j.heard(said(a, 1, Msg::Length { frames: 480_000 }));
        j.heard(said(
            a,
            1,
            Msg::Lit {
                ranges: vec![(0, 240_000)],
            },
        ));
        let cache = PathBuf::from(&j.state().jobs[0].cache);
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("cleaned.f32"), b"x").unwrap();
        assert_eq!(states(&j), [Cleaning, Waiting]);
        take(&log);
        j.clear();
        assert_eq!(take(&log), ["end 1"]);
        assert!(!cache.exists(), "a pass that was cut short leaves no sound");
        assert!(j.undo_clear());
        let s = j.state();
        assert_eq!(s.jobs[0].id, a);
        assert!(s.jobs[0].lit.is_empty(), "{:?}", s.jobs[0].lit);
        assert_eq!(s.jobs[0].frames, None, "opened afresh");
        assert_eq!((s.jobs[1].id, s.jobs[1].state), (b, Waiting));
        assert_eq!(take(&log), ["spawn 1"], "a cleans again from the start");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn save_all_leaves_a_damaged_file_to_its_own_question() {
        let (mut j, log, f, dir) = setup("save-all-damaged");
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        cleaned(&mut j, a, 1);
        cleaned(&mut j, b, 2);
        // b stops early, and nobody agreed to keep its first part only
        let ib = j.index(b).unwrap();
        j.list[ib].wire.damaged = Some(96_000);
        take(&log);
        assert_eq!(j.save_all(), Ok(1));
        assert_eq!(j.noting, [a]);
        j.list[ib].wire.damage_ok = true;
        // agreed: Save all takes it too (a is saving already)
        assert_eq!(j.save_all(), Ok(1));
        assert_eq!(j.noting, [b]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_first_look_that_comes_late_does_not_overrule_the_pass() {
        let (mut j, _log, f, dir) = setup("late-look");
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        let sb = j.list[1].probe.as_ref().map(|l| l.serial).unwrap();
        // a is done before b's look answers: b's pass runs, and knows
        cleaned(&mut j, a, 1);
        assert_eq!(states(&j), [Done, Cleaning]);
        j.heard(said(
            b,
            sb,
            Msg::Probed {
                problem: Some(Problem::NotMedia),
                detail: "late".into(),
            },
        ));
        assert_eq!(states(&j), [Done, Cleaning]);
        assert_eq!(j.state().jobs[1].problem, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn first_looks_wait_for_the_microphone_and_go_two_at_a_time() {
        let (mut j, _log, mut f, dir) = setup("look-limit");
        for k in 4..=5 {
            let p = dir.join(format!("f{k}.wav"));
            std::fs::write(&p, b"").unwrap();
            f.push(p.display().to_string());
        }
        // the microphone needs the CPU: no looks
        j.set_live(true, true);
        for p in &f {
            j.add(p).unwrap();
        }
        assert!(probes(&dir).is_empty(), "{:?}", probes(&dir));
        assert!(j.list.iter().all(|x| x.probe.is_none()));
        // it recovered: two of the four waiting files are looked at
        j.set_live(false, true);
        assert_eq!(probes(&dir), [f[1].clone(), f[2].clone()]);
        assert_eq!(j.list.iter().filter(|x| x.probe.is_some()).count(), PROBES);
        // one look ends: the next one starts
        let (id, serial) = (j.list[1].wire.id, j.list[1].probe.as_ref().unwrap().serial);
        j.heard(JobEvent {
            id,
            serial,
            heard: Heard::Gone,
        });
        assert_eq!(probes(&dir), [f[1].clone(), f[2].clone(), f[3].clone()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_save_keeps_what_the_page_names() {
        let (mut j, log, f, dir) = setup("save-note");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 2, 12.0));
        take(&log);
        let ro = dir.join("read-only");
        // a folder that can't be written: the folder itself
        assert!(j.save(a, Some(ro.clone())).is_ok());
        assert_eq!(take(&log), [format!("deliver 1 {}", ro.display())]);
        j.heard(JobEvent {
            id: a,
            serial: 3,
            heard: Heard::Saved(Err((
                Problem::NoWrite,
                format!("can't write to {}", ro.display()),
            ))),
        });
        let s = &j.state().jobs[0];
        assert_eq!((s.saving, s.save_problem), (false, Some(Problem::NoWrite)));
        assert_eq!(s.save_detail.as_deref(), Some(ro.to_str().unwrap()));
        // next to the original: the original's folder
        assert!(j.save(a, None).is_ok());
        assert_eq!(
            j.state().jobs[0].save_detail,
            None,
            "a new Save starts clean"
        );
        take(&log);
        j.heard(JobEvent {
            id: a,
            serial: 4,
            heard: Heard::Saved(Err((Problem::NoWrite, "can't write".into()))),
        });
        assert_eq!(
            j.state().jobs[0].save_detail.as_deref(),
            Some(dir.to_str().unwrap())
        );
        // any other failure: its short reason
        assert!(j.save(a, None).is_ok());
        take(&log);
        j.heard(JobEvent {
            id: a,
            serial: 5,
            heard: Heard::Saved(Err((
                Problem::Failed,
                "Input/output error (os error 5)".into(),
            ))),
        });
        let s = &j.state().jobs[0];
        assert_eq!(s.save_problem, Some(Problem::Failed));
        assert_eq!(s.save_detail.as_deref(), Some("Input/output error"));
        // the encoder's failure, while it was prepared: its kind, and the
        // encoder's own reason (the true one, for the row's tooltip)
        j.set_file_strength(a, 24.0);
        assert!(j.save(a, None).is_ok());
        assert_eq!(take(&log), ["prepare 1 at 24"]);
        j.heard(said(
            a,
            6,
            Msg::SaveFailed {
                kind: Problem::Encode,
                detail: "ffmpeg: Invalid argument".into(),
            },
        ));
        let s = &j.state().jobs[0];
        assert_eq!((s.saving, s.save_problem), (false, Some(Problem::Encode)));
        assert_eq!(s.save_detail.as_deref(), Some("Invalid argument"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_preparation_fails_its_save_and_is_not_retried_alone() {
        let (mut j, log, f, dir) = setup("savefail");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        take(&log);
        assert!(j.save(a, None).is_ok());
        j.heard(JobEvent {
            id: a,
            serial: 2,
            heard: Heard::Gone,
        });
        let s = &j.state().jobs[0];
        assert_eq!((s.saving, s.save_problem), (false, Some(Problem::Crashed)));
        assert_eq!(take(&log), ["reap 1"], "not again on its own");
        // Save again: it tries again.
        assert!(j.save(a, None).is_ok());
        assert_eq!(take(&log), ["prepare 1 at 12"]);
        j.heard(said(
            a,
            3,
            Msg::SaveFailed {
                kind: Problem::NoSpace,
                detail: "full".into(),
            },
        ));
        assert_eq!(j.state().jobs[0].save_problem, Some(Problem::NoSpace));
        // A file that needs attention cannot be saved.
        let b = j.add(&f[1]).unwrap();
        j.heard(said(
            b,
            4,
            Msg::Error {
                kind: Problem::Unreadable,
                detail: "x".into(),
            },
        ));
        assert!(j.save(b, None).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preparing_waits_while_the_microphone_needs_the_cpu_unless_saving() {
        let (mut j, log, f, dir) = setup("saveyield");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        assert_eq!(take(&log), ["spawn 1", "stop", "prepare 1 at 12", "reap 1"]);
        j.set_live(true, true);
        assert_eq!(take(&log), ["hold 1"]);
        assert!(j.save(a, None).is_ok());
        assert_eq!(take(&log), ["cont 1"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_can_be_cancelled_waiting_or_running() {
        let (mut j, log, f, dir) = setup("cancel");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        assert!(!j.cancel_save(a), "nothing to cancel");
        // Waiting for the cleaning.
        assert!(j.save(a, None).is_ok());
        assert!(j.cancel_save(a));
        assert!(!j.state().jobs[0].saving);
        cleaned(&mut j, a, 1);
        assert_eq!(take(&log), ["spawn 1", "stop", "prepare 1 at 12", "reap 1"]);
        // Running: it says how far; once in place, a cancel comes too late.
        assert_eq!(j.state().jobs[0].preparing, Some(0.0));
        j.heard(said(a, 2, Msg::Preparing { done: 0.5 }));
        assert_eq!(j.state().jobs[0].preparing, Some(0.5));
        j.heard(prepared(a, 2, 12.0));
        assert_eq!(j.state().jobs[0].preparing, None);
        assert!(j.save(a, None).is_ok());
        assert!(j.cancel_save(a));
        j.heard(JobEvent {
            id: a,
            serial: 3,
            heard: Heard::Saved(Err((Problem::Failed, "cancelled".into()))),
        });
        let s = &j.state().jobs[0];
        assert_eq!(
            (s.saving, s.saved.clone(), s.save_problem),
            (false, None, None)
        );
        assert!(j.save(a, None).is_ok());
        assert!(j.cancel_save(a));
        j.heard(JobEvent {
            id: a,
            serial: 4,
            heard: Heard::Saved(Ok("/x/f1 (cleaned).wav".into())),
        });
        assert_eq!(
            j.state().jobs[0].saved.as_deref(),
            Some("/x/f1 (cleaned).wav")
        );
        assert_eq!(take(&log), ["end 1", "deliver 1 ", "deliver 1 "]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_found_again_keeps_its_place_and_starts_over() {
        let (mut j, log, f, dir) = setup("replace");
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        j.heard(said(
            a,
            1,
            Msg::Error {
                kind: Problem::Unreadable,
                detail: "x".into(),
            },
        ));
        take(&log);
        assert!(j.replace(a, &f[1]).is_err(), "already on the page");
        assert!(j.replace(a, "/nonexistent/x.wav").is_err());
        assert!(j.replace(99, &f[2]).is_err());
        assert_eq!(j.replace(a, &f[2]), Ok(()));
        let s = j.state();
        assert_eq!((s.jobs[0].id, s.jobs[0].path.as_str()), (a, f[2].as_str()));
        assert_eq!(s.jobs[0].problem, None);
        assert_eq!(states(&j), [Cleaning, Waiting]);
        assert_eq!(take(&log), ["end 1", "hold 2", "spawn 1"]);
        let _ = (b, std::fs::remove_dir_all(dir));
    }

    #[test]
    fn stops_in_a_row_are_counted_and_damage_is_confirmed() {
        let (mut j, _log, f, dir) = setup("stops");
        let a = j.add(&f[0]).unwrap();
        let fail = |j: &mut Jobs, serial| {
            j.heard(said(
                a,
                serial,
                Msg::Error {
                    kind: Problem::Failed,
                    detail: "x".into(),
                },
            ))
        };
        fail(&mut j, 1);
        assert!(j.retry(a));
        fail(&mut j, 2);
        assert_eq!(j.state().jobs[0].stops, 2, "the second stop in a row");
        assert!(j.retry(a));
        assert!(!j.clean_first(a), "not damaged");
        j.heard(said(a, 3, Msg::Damaged { frames: 96_000 }));
        assert!(j.clean_first(a));
        assert!(j.state().jobs[0].damage_ok);
        cleaned(&mut j, a, 3);
        assert_eq!(j.state().jobs[0].stops, 0, "cleaned: counted afresh");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_stated_length_outlives_what_was_read() {
        // the file says 12:30; it is damaged after 3:12, and what was read
        // becomes the length: the page names both ("of 12:30")
        let (mut j, _log, f, dir) = setup("stated");
        let a = j.add(&f[0]).unwrap();
        let opened = Msg::Opened {
            rate: 48_000,
            channels: 1,
            stored: 1,
            frames: Some(48_000 * 750),
            video: false,
            codec: "flac".into(),
            ffmpeg: false,
            reduced: false,
            tracks: 1,
            cleaning: 1,
            dropped: 0,
            track: 0,
            titles: Vec::new(),
        };
        j.heard(said(a, 1, opened));
        j.heard(said(
            a,
            1,
            Msg::Damaged {
                frames: 48_000 * 192,
            },
        ));
        j.heard(said(
            a,
            1,
            Msg::Length {
                frames: 48_000 * 192,
            },
        ));
        let job = &j.state().jobs[0];
        assert_eq!(job.stated, Some(48_000 * 750));
        assert_eq!(job.frames, Some(48_000 * 192));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn quitting_asks_while_files_are_busy_and_cleaned_files_are_counted_once() {
        let (mut j, _log, f, dir) = setup("quit");
        assert_eq!((j.busy(), j.take_cleaned()), (0, None));
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        assert_eq!(j.busy(), 2);
        j.set_ask_quit(true);
        assert!(j.state().ask_quit);
        cleaned(&mut j, a, 1);
        assert_eq!(j.take_cleaned(), None, "one still cleaning");
        assert!(j.state().ask_quit);
        // A Save under way keeps it busy.
        assert!(j.save(a, None).is_ok());
        cleaned(&mut j, b, 2);
        assert_eq!(j.busy(), 1);
        j.set_ask_quit(false);
        assert!(!j.state().ask_quit);
        j.set_ask_quit(true);
        j.cancel_save(a);
        // Nothing left to stop: the question stays until it is answered,
        // and the note counts both.
        assert!(j.state().ask_quit && j.asking_quit());
        let c = j.take_cleaned().expect("both cleaned");
        assert_eq!((c.count, c.saved), (2, None), "not every one saved");
        assert_eq!(j.take_cleaned(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_note_knows_where_files_went_when_every_one_was_saved() {
        let (mut j, log, f, dir) = setup("note-saved");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        // Save all while they clean: each saves once cleaned
        assert!(j.save(a, None).is_ok());
        assert!(j.save(b, None).is_ok());
        cleaned(&mut j, a, 1);
        cleaned(&mut j, b, 2);
        assert_eq!(j.take_cleaned(), None, "still saving");
        let out = |p: &str| p.replace(".wav", " (cleaned).wav");
        for (id, serial) in [(a, 3), (b, 4)] {
            j.heard(prepared(id, serial, 12.0));
        }
        let delivered: Vec<String> = take(&log)
            .into_iter()
            .filter(|l| l.starts_with("deliver"))
            .collect();
        assert_eq!(delivered.len(), 2, "{delivered:?}");
        for (id, serial, file) in [(a, 5, &f[0]), (b, 6, &f[1])] {
            j.heard(JobEvent {
                id,
                serial,
                heard: Heard::Saved(Ok(out(file))),
            });
        }
        let c = j.take_cleaned().expect("both cleaned and saved");
        assert_eq!(c.count, 2);
        assert_eq!(
            c.saved,
            Some(
                [&f[0], &f[1]]
                    .iter()
                    .map(|p| (PathBuf::from(p), PathBuf::from(out(p))))
                    .collect()
            )
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_chosen_in_settings_takes_every_save_until_it_is_cleared() {
        let (mut j, log, f, dir) = setup("settings-folder");
        let chosen = dir.join("Cleaned");
        std::fs::create_dir_all(&chosen).unwrap();
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 2, 12.0));
        take(&log);
        // Outside a sandbox, "In a folder…" (looked at first).
        j.set_folder(false, Some(chosen.clone()));
        assert!(j.save(a, None).is_ok());
        looked(&mut j);
        assert_eq!(take(&log), [format!("deliver 1 {}", chosen.display())]);
        j.heard(JobEvent {
            id: a,
            serial: 3,
            heard: Heard::Saved(Ok("x".into())),
        });
        // "Next to the original" again.
        j.set_folder(false, None);
        assert_eq!(j.state().folder, None);
        assert!(j.save(a, None).is_ok());
        assert_eq!(take(&log), ["deliver 1 "]);
        j.heard(JobEvent {
            id: a,
            serial: 4,
            heard: Heard::Saved(Ok("y".into())),
        });
        // A folder that went away: next to the original, and it is unset.
        // (The choice's own look first, or the Save could ride on it.)
        j.set_folder(false, Some(chosen.clone()));
        looked(&mut j);
        std::fs::remove_dir(&chosen).unwrap();
        assert!(j.save(a, None).is_ok());
        looked(&mut j);
        assert_eq!(take(&log), ["deliver 1 "]);
        assert_eq!(j.state().folder, None);
        assert_eq!(j.state().jobs[0].folder_miss, Some(FolderMiss::Gone));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Until the look at the chosen folder under way has answered.
    fn looked(j: &mut Jobs) {
        let t = std::time::Instant::now();
        while j.folder_look.is_some() {
            assert!(
                t.elapsed() < std::time::Duration::from_secs(60),
                "no answer"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
            j.check_folder(j.files_shown);
        }
    }

    #[test]
    fn a_chosen_folder_is_looked_at_when_it_matters_and_dropped_once_gone() {
        let (mut j, log, f, dir) = setup("folder-gone");
        let chosen = dir.join("chosen");
        std::fs::create_dir_all(&chosen).unwrap();
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 2, 12.0));
        take(&log);
        j.set_folder(true, Some(chosen.clone()));
        // one look at the choice, then not while the window is closed or
        // Files is not shown: an idle mount sleeps
        looked(&mut j);
        assert!(j.state().folder.is_some());
        std::fs::remove_dir(&chosen).unwrap();
        for _ in 0..3 {
            j.check_folder(false);
        }
        assert!(j.folder_look.is_none(), "no look while Files is not shown");
        assert!(j.state().folder.is_some());
        // The Files page opens: a look, and the gone folder is dropped, so
        // the window's Save asks (a sandbox has no "next to the original").
        j.check_folder(true);
        assert!(j.folder_look.is_some(), "the page opened: a look");
        looked(&mut j);
        assert_eq!(j.state().folder, None);
        assert!(j.state().needs_folder);
        // While Files shows: again only after FOLDER_LOOK_EVERY.
        std::fs::create_dir_all(&chosen).unwrap();
        j.set_folder(true, Some(chosen.clone()));
        looked(&mut j);
        j.check_folder(true);
        assert!(j.folder_look.is_none(), "looked at {:?} ago", j.looked_at);
        j.looked_at = Some(std::time::Instant::now() - FOLDER_LOOK_EVERY);
        j.check_folder(true);
        assert!(j.folder_look.is_some(), "due: a look");
        looked(&mut j);
        assert!(j.state().folder.is_some(), "still there");
        // Before each Save: the folder went meanwhile, the Save waits for
        // the look, and it does not go out.
        std::fs::remove_dir(&chosen).unwrap();
        assert!(j.save(a, None).is_ok());
        assert!(!j.state().jobs[0].saving, "waits for the look");
        looked(&mut j);
        assert_eq!(j.state().folder, None);
        assert!(!j.state().jobs[0].saving, "the page asks for a folder");
        assert!(take(&log).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_gone_at_the_start_goes_and_one_that_does_not_answer_is_kept() {
        // gone before the start (or the choice): one look, and it goes
        let (mut j, log, f, dir) = setup("folder-start");
        j.set_folder(true, Some(dir.join("never-made")));
        looked(&mut j);
        assert_eq!(j.state().folder, None);
        // a look that does not answer (a share that mounts slowly): a Save
        // waits for it only FOLDER_LOOK_LIMIT; then the folder is kept, but
        // not reachable, and the page asks where (in a sandbox)
        let (release, held) = mpsc::channel::<()>();
        let held = std::sync::Mutex::new(held);
        let mut j = j.with_looker(move |_| {
            let _ = held.lock().unwrap().recv();
            true
        });
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 2, 12.0));
        take(&log);
        let chosen = dir.join("slow");
        std::fs::create_dir_all(&chosen).unwrap();
        j.set_folder(true, Some(chosen.clone()));
        assert!(j.save(a, None).is_ok());
        j.check_folder(true);
        assert!(j.state().folder.is_some() && !j.state().folder_unreachable);
        assert!(!j.state().jobs[0].saving, "waits for the look");
        j.looked_at = Some(std::time::Instant::now() - FOLDER_LOOK_LIMIT);
        j.check_folder(true);
        let s = j.state();
        assert!(s.folder.is_some(), "a timeout keeps the folder");
        assert!(s.folder_unreachable, "the page asks where meanwhile");
        assert!(!s.jobs[0].saving);
        assert!(take(&log).is_empty());
        // a Save meanwhile: as for a gone one, at once
        assert!(j.save(a, None).is_ok());
        assert!(!j.state().jobs[0].saving);
        // the share answers: kept and reachable, and the next Save goes there
        release.send(()).unwrap();
        looked(&mut j);
        let s = j.state();
        assert!(s.folder.is_some() && !s.folder_unreachable);
        assert!(j.save(a, None).is_ok());
        release.send(()).unwrap();
        looked(&mut j);
        assert_eq!(take(&log), [format!("deliver 1 {}", chosen.display())]);
        assert_eq!(j.state().jobs[0].folder_miss, None);
        j.heard(JobEvent {
            id: a,
            serial: 3,
            heard: Heard::Saved(Ok("x".into())),
        });
        // outside a sandbox, a Save while it does not answer goes next to
        // the original (and says why); the folder stays
        j.set_folder(false, Some(chosen.clone()));
        assert!(j.save(a, None).is_ok());
        j.looked_at = Some(std::time::Instant::now() - FOLDER_LOOK_LIMIT);
        j.check_folder(false);
        assert_eq!(take(&log), ["deliver 1 "]);
        assert_eq!(j.state().jobs[0].folder_miss, Some(FolderMiss::NoAnswer));
        assert!(j.state().folder.is_some());
        release.send(()).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_gone_between_the_look_and_the_write_falls_back() {
        // outside a sandbox: next to the original, saying why; the folder
        // is dropped (a failed write into it is a definite "gone")
        let (mut j, log, f, dir) = setup("folder-went");
        let chosen = dir.join("chosen");
        std::fs::create_dir_all(&chosen).unwrap();
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 2, 12.0));
        take(&log);
        j.set_folder(false, Some(chosen.clone()));
        looked(&mut j);
        assert!(j.save(a, None).is_ok());
        looked(&mut j);
        assert_eq!(take(&log), [format!("deliver 1 {}", chosen.display())]);
        // it went before the write: the delivery says so
        j.heard(JobEvent {
            id: a,
            serial: 3,
            heard: Heard::Saved(Err((Problem::FolderGone, "the folder is gone".into()))),
        });
        assert_eq!(take(&log), ["deliver 1 "]);
        let s = j.state();
        assert_eq!(s.folder, None);
        assert!(s.jobs[0].saving);
        assert_eq!(s.jobs[0].folder_miss, Some(FolderMiss::Gone));
        assert_eq!(s.jobs[0].save_problem, None);
        let _ = std::fs::remove_dir_all(dir);
        // in a sandbox: nothing goes out; the job says FolderGone, so the
        // page asks where
        let (mut j, log, f, dir) = setup("folder-went-sandbox");
        let chosen = dir.join("chosen");
        std::fs::create_dir_all(&chosen).unwrap();
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 2, 12.0));
        take(&log);
        j.set_folder(true, Some(chosen.clone()));
        looked(&mut j);
        assert!(j.save(a, None).is_ok());
        looked(&mut j);
        assert_eq!(take(&log), [format!("deliver 1 {}", chosen.display())]);
        j.heard(JobEvent {
            id: a,
            serial: 3,
            heard: Heard::Saved(Err((Problem::FolderGone, "the folder is gone".into()))),
        });
        assert!(take(&log).is_empty());
        let s = j.state();
        assert_eq!(s.folder, None);
        assert!(!s.jobs[0].saving);
        assert_eq!(s.jobs[0].save_problem, Some(Problem::FolderGone));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn in_a_sandbox_next_to_the_original_is_the_chosen_folder() {
        let (mut j, log, f, dir) = setup("folder");
        let chosen = dir.join("chosen");
        std::fs::create_dir_all(&chosen).unwrap();
        let shown = chosen.display().to_string();
        j.set_strength(12.0);
        j.set_folder(true, Some(chosen.clone()));
        let s = j.state();
        assert_eq!(
            (s.needs_folder, s.folder.as_deref()),
            (true, Some(shown.as_str()))
        );
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 2, 12.0));
        take(&log);
        assert!(j.save(a, None).is_ok());
        looked(&mut j);
        assert_eq!(take(&log), [format!("deliver 1 {shown}")]);
        // A folder of its own still wins.
        j.heard(JobEvent {
            id: a,
            serial: 3,
            heard: Heard::Saved(Ok("x".into())),
        });
        assert!(j.save(a, Some("/other".into())).is_ok());
        assert_eq!(take(&log), ["deliver 1 /other"]);
        j.heard(JobEvent {
            id: a,
            serial: 4,
            heard: Heard::Saved(Ok("y".into())),
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn in_a_sandbox_no_save_goes_out_before_a_folder_is_chosen() {
        let (mut j, log, f, dir) = setup("nofolder");
        j.set_strength(12.0);
        j.set_folder(true, None);
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 2, 12.0));
        take(&log);
        // Not next to the original (a document-portal folder): the page
        // asks first.
        assert_eq!(j.save(a, None), Err(Refused::NoFolder));
        assert!(!j.state().jobs[0].saving);
        // A chosen folder that is gone counts as none once looked at (a
        // Save looks first), at the start and later.
        j.set_folder(true, Some(dir.join("never-made")));
        assert!(j.save(a, None).is_ok());
        looked(&mut j);
        assert_eq!(j.state().folder, None);
        let gone = dir.join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        j.set_folder(true, Some(gone.clone()));
        looked(&mut j);
        std::fs::remove_dir(&gone).unwrap();
        assert!(j.save(a, None).is_ok());
        looked(&mut j);
        assert_eq!(j.state().folder, None, "the page asks again");
        assert!(!j.state().jobs[0].saving);
        assert_eq!(j.save(a, None), Err(Refused::NoFolder));
        // Asked with a folder: it goes there.
        assert!(j.save(a, Some(dir.clone())).is_ok());
        assert_eq!(take(&log), [format!("deliver 1 {}", dir.display())]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn paths_from_elsewhere_must_be_absolute() {
        let (mut j, log, f, dir) = setup("relative");
        j.set_strength(12.0);
        // The daemon's working folder is not the caller's.
        let rel = "f1.wav";
        assert!(j.add(rel).unwrap_err().contains("not an absolute path"));
        assert!(j.state().jobs.is_empty());
        let a = j.add(&f[0]).unwrap();
        assert!(j
            .replace(a, rel)
            .unwrap_err()
            .contains("not an absolute path"));
        assert_eq!(j.state().jobs[0].path, f[0]);
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 2, 12.0));
        take(&log);
        assert_eq!(j.save(a, Some("out".into())), Err(Refused::Relative));
        assert!(take(&log).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_file_or_a_moved_original_is_not_saved() {
        let (mut j, log, f, dir) = setup("guard");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        let b = j.add(&f[1]).unwrap();
        // Save all while it cleans; then it turns out damaged: the Save
        // waits for the user's word and goes.
        assert!(j.save(a, None).is_ok());
        j.heard(said(a, 1, Msg::Damaged { frames: 96_000 }));
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 3, 12.0));
        assert!(!take(&log).iter().any(|l| l.starts_with("deliver")));
        assert!(!j.state().jobs[0].saving);
        assert_eq!(j.save(a, None), Err(Refused::Damaged));
        // Agreed to: its first part saves.
        assert!(j.clean_first(a));
        assert!(j.save(a, None).is_ok());
        assert_eq!(take(&log), ["deliver 1 "]);
        // The original moved after it was cleaned: Moved, nothing written.
        cleaned(&mut j, b, 2);
        std::fs::remove_file(&f[1]).unwrap();
        assert_eq!(j.save(b, None), Err(Refused::Moved));
        let s = &j.state().jobs[1];
        assert_eq!((s.saving, s.save_problem), (false, Some(Problem::Moved)));
        assert!(!take(&log).iter().any(|l| l.starts_with("deliver 2")));
        assert_eq!(j.save(99, None), Err(Refused::NoSuchFile));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_that_found_no_space_can_go_elsewhere() {
        let (mut j, log, f, dir) = setup("nospace");
        j.set_strength(12.0);
        let a = j.add(&f[0]).unwrap();
        cleaned(&mut j, a, 1);
        j.heard(prepared(a, 2, 12.0));
        assert!(j.save(a, None).is_ok());
        j.heard(JobEvent {
            id: a,
            serial: 3,
            heard: Heard::Saved(Err((Problem::NoSpace, "full".into()))),
        });
        assert_eq!(j.state().jobs[0].save_problem, Some(Problem::NoSpace));
        take(&log);
        assert!(j.save(a, Some("/elsewhere".into())).is_ok());
        assert_eq!(take(&log), ["deliver 1 /elsewhere"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_sweep_never_takes_a_clean_that_starts_beside_it() {
        // A run takes its lock at the very moment a sweep has found it
        // free and is about to remove it. Had the sweep only looked, the
        // run would hold a lock it then unlinks, and the next sweep would
        // remove the run's live folder. The sweep holds the lock instead:
        // the run waits, then makes a new one.
        use crate::files::held::{hold, is_held};
        use std::sync::mpsc;
        let dir = std::env::temp_dir().join(format!("hushmic-jobs-{}-race", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("clean-dead")).unwrap();
        std::fs::write(dir.join("clean-dead.lock"), b"").unwrap();
        std::fs::write(dir.join("clean-lone.lock"), b"").unwrap();
        type Run = (
            PathBuf,
            std::thread::JoinHandle<()>,
            mpsc::Receiver<()>,
            mpsc::Sender<()>,
        );
        let mut runs: Vec<Run> = Vec::new();
        sweep_stale_pausing(&dir, &mut |lock| {
            // (the run's new lock, if the sweep comes by again before the
            // run has it: the run waits on that one as well)
            if runs.iter().any(|(l, ..)| l == lock) {
                return;
            }
            let (lock, (said, heard)) = (lock.to_path_buf(), mpsc::channel());
            let (end, ended) = mpsc::channel::<()>();
            runs.push((
                lock.clone(),
                std::thread::spawn(move || {
                    let held = hold(&lock).unwrap();
                    let base = lock.with_extension("");
                    std::fs::create_dir_all(&base).unwrap();
                    std::fs::write(base.join("x"), b"1").unwrap();
                    said.send(()).unwrap();
                    let _ = ended.recv();
                    drop(held);
                }),
                heard,
                end,
            ));
            let (lock, _, heard, _) = runs.last().unwrap();
            assert!(
                heard
                    .recv_timeout(std::time::Duration::from_millis(300))
                    .is_err(),
                "the run took {} while the sweep held it",
                lock.display()
            );
        });
        assert_eq!(runs.len(), 2, "both stale locks were taken");
        for (lock, _, heard, _) in &runs {
            heard
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            assert!(is_held(lock), "the run holds a lock that is there");
        }
        sweep_stale(&dir);
        for (lock, run, _, end) in runs {
            assert!(
                lock.with_extension("").join("x").exists(),
                "a live folder stays"
            );
            end.send(()).unwrap();
            run.join().unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_takes_only_the_daemons_folders_and_unheld_cleans() {
        use crate::files::cli::scratch_folder;
        let dir = std::env::temp_dir().join(format!("hushmic-jobs-{}-sweep", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["job-1", "job-17", "other", "clean-999999999", "clean-x"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        // a lock left without its folder
        std::fs::write(dir.join("clean-y.lock"), b"").unwrap();
        // Two runs at once in one process, as two Flatpak runs that are
        // both pid 2: a folder each, both held.
        let (a, held_a) = scratch_folder(&dir).unwrap();
        let (b, held_b) = scratch_folder(&dir).unwrap();
        assert_ne!(a, b);
        sweep(&dir);
        let names = |dir: &Path| {
            let mut v: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        let mut want = vec![
            name(&a),
            format!("{}.lock", name(&a)),
            name(&b),
            format!("{}.lock", name(&b)),
            "other".into(),
        ];
        want.sort();
        assert_eq!(names(&dir), want, "the running cleans' folders stay");
        // One run ends without tidying (killed): its folder goes, the other's stays.
        drop(held_a);
        std::fs::create_dir_all(dir.join("job-2")).unwrap();
        sweep_stale(&dir);
        assert!(
            dir.join("job-2").exists(),
            "`hushmic clean` leaves the daemon's jobs"
        );
        assert!(!a.exists() && b.exists());
        drop(held_b);
        let _ = std::fs::remove_dir_all(dir);
    }
}
