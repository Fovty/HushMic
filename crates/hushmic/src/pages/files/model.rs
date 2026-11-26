//! What the Files page shows and asks for: the boundary between the page
//! and whatever cleans the files. The page talks only to a [`Source`]: the
//! demo (`demo.rs`, the boards' file model) until the daemon's jobs and the
//! window's player land, then a source that adapts them.
//!
//! The page keeps its own state: the latch of the hold, the drag-over
//! look, the motion. A source keeps the files, their passes, the playhead
//! and the picture of the selected file.

/// A file on the page, stable while it is there.
pub type FileId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Audio,
    /// The picture is kept; only the sound is cleaned.
    Video,
    /// Neither by its name, and it never opened ("notes.txt"): no sound
    /// icon (F15).
    Other,
}

/// The page's one strength (every file here): a mix of the pass and the
/// original, so a change needs no new pass. Files starts at Strong (Live
/// at Maximum): a recording keeps a little of its room, no dead air.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strength {
    Gentle,
    Medium,
    #[default]
    Strong,
    Maximum,
}

impl Strength {
    pub const ALL: [Strength; 4] = [
        Strength::Gentle,
        Strength::Medium,
        Strength::Strong,
        Strength::Maximum,
    ];

    /// How much of the original stays: 10^(−dB/20) for 6, 12 and 24 dB,
    /// none at Maximum.
    pub fn alpha(self) -> f32 {
        match self {
            Strength::Gentle => 10f32.powf(-6.0 / 20.0),
            Strength::Medium => 10f32.powf(-12.0 / 20.0),
            Strength::Strong => 10f32.powf(-24.0 / 20.0),
            Strength::Maximum => 0.0,
        }
    }

    pub fn index(self) -> usize {
        Strength::ALL.iter().position(|s| *s == self).unwrap_or(3)
    }

    /// Its word ("Strong"), as Live's strength says it.
    pub fn word(self) -> String {
        match self {
            Strength::Gentle => crate::tr!("strength-gentle"),
            Strength::Medium => crate::tr!("strength-medium"),
            Strength::Strong => crate::tr!("strength-strong"),
            Strength::Maximum => crate::tr!("strength-maximum"),
        }
    }

    /// What it does to a recording, in one line: Live's own words (its
    /// Maximum says that the pauses between words go silent, F9), but
    /// Strong says why Files starts there.
    pub fn files_line(self) -> String {
        match self {
            // why Files starts here, not at Live's Maximum (F17)
            Strength::Strong => crate::tr!("files-strength-strong"),
            s => s.line(),
        }
    }

    /// What it does, in one line: Live's own words.
    pub fn line(self) -> String {
        match self {
            Strength::Gentle => crate::tr!("live-strength-gentle"),
            Strength::Medium => crate::tr!("live-strength-medium"),
            Strength::Strong => crate::tr!("live-strength-strong"),
            Strength::Maximum => crate::tr!("live-strength-maximum"),
        }
    }
}

/// Why a file needs attention (PLAN §3; the daemon's
/// `files::worker::Problem`, plus what the window finds itself).
#[derive(Clone, Debug, PartialEq)]
pub enum Reason {
    /// HushMic (and ffmpeg) don't open files of this type (`ext` without
    /// the dot).
    Unsupported { ext: String },
    /// The file can't be read.
    Unreadable,
    /// An MP4 or MOV cut off before its index was written (a recording
    /// that stopped short).
    Incomplete,
    /// Not audio or video at all.
    NotMedia,
    /// Nothing in it (0 bytes).
    Empty,
    /// A video without a sound track.
    NoSoundTrack,
    /// There's no sound in the file.
    Silent,
    /// The model or the runtime wouldn't load.
    Engine,
    /// The cleaned sound can't be kept while the file is open: its folder
    /// can't be written, or there's not enough free space (`bytes`: about
    /// what it needs, when known).
    Cache { bytes: Option<u64> },
    /// The pass stopped on an error of its own (`again`: the second stop
    /// in a row).
    Failed { again: bool },
    /// The pass's process couldn't start, or ended without a word.
    Crashed { again: bool },
    /// The file stops early ([`Row::damaged`]): its first part can be
    /// cleaned, if asked ([`Cmd::CleanFirst`]).
    Damaged,
    /// The disk is full: the cleaned file wasn't saved.
    NoSpace,
    /// The original moved while it was being cleaned, or (`at_save`)
    /// before its Save; nothing was written.
    Moved { at_save: bool },
    /// Save: the folder can't be written (`folder`: its name).
    NoWrite { folder: String },
    /// Save: the encoder failed to write the output (`format`, e.g. "M4A";
    /// `why`: ffmpeg's own words, when it gave some).
    SaveEncode { format: String, why: Option<String> },
    /// Save: the original couldn't be read again.
    SaveRead,
    /// Save: failed otherwise (`why`: a short reason, when there is one).
    SaveFailed { why: Option<String> },
}

impl Reason {
    /// The file is cleaned; only its Save failed.
    pub fn save_only(&self) -> bool {
        matches!(
            self,
            Reason::NoSpace
                | Reason::NoWrite { .. }
                | Reason::SaveEncode { .. }
                | Reason::SaveRead
                | Reason::SaveFailed { .. }
        )
    }

    /// Saving the same way again can't work: the encoder refuses this
    /// output, or the original can't be read. No "Save again" for these.
    pub fn permanent(&self) -> bool {
        matches!(self, Reason::SaveEncode { .. } | Reason::SaveRead)
    }

    /// The file can't be played: it didn't open, has no sound, or is gone.
    pub fn unplayable(&self) -> bool {
        matches!(
            self,
            Reason::Unsupported { .. }
                | Reason::Unreadable
                | Reason::Incomplete
                | Reason::NotMedia
                | Reason::Empty
                | Reason::NoSoundTrack
                | Reason::Moved { .. }
        )
    }
}

/// A file's cleaning pass.
#[derive(Clone, Debug, PartialEq)]
pub enum Pass {
    /// In the queue, not started (the pass works one file at a time).
    Waiting,
    Cleaning,
    /// Paused while the live engine has stepped down (yield on overload).
    Yielded,
    Cleaned,
    Attention(Reason),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Save {
    No,
    /// Save was asked before the pass ended: it saves when cleaned.
    WhenCleaned,
    /// A long write (the strength changed just before Save).
    Writing {
        fraction: f32,
        secs_left: f64,
    },
    Saved,
    /// Saved at another strength (`was`) than the page's `now`: the row
    /// offers Save at the new one ("Saved at Maximum · Save Strong").
    Stale {
        was: Strength,
        now: Strength,
    },
}

/// Where cleaned files go (Settings › Files).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Place {
    #[default]
    NextToOriginal,
    /// A folder, by its short name ("Recordings/Cleaned").
    Folder(String),
    /// Next to the original instead of the chosen folder, and why.
    Instead(crate::appstate::FolderMiss),
}

/// One file on the page.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: FileId,
    pub name: String,
    /// Seconds; 0 when it couldn't be read.
    pub length: f64,
    /// How sure `length` is.
    pub estimate: Estimate,
    /// The file stops early: this many seconds can be cleaned.
    pub damaged: Option<f64>,
    /// The length the file states, in seconds (a damaged file is shorter).
    pub stated: Option<f64>,
    pub kind: Kind,
    pub pass: Pass,
    /// The cleaned parts, in seconds, in order (a seek past the front
    /// starts a new one).
    pub segments: Vec<(f64, f64)>,
    /// The cleaned file's name ("interview (cleaned).m4a"); empty until
    /// there is one to write.
    pub output: String,
    /// The name its last Save really wrote ("interview (cleaned 2).m4a":
    /// a Save never overwrites); empty while none.
    pub saved_name: String,
    pub save: Save,
    /// Seconds of pass left; None before the rate is known.
    pub time_left: Option<f64>,
    /// The pass has cleaned nothing for a whole speed window (its worker
    /// gets no CPU on a busy machine): no time left, and a player that
    /// waits for it says it waits.
    pub stalled: bool,
    /// Cleaned below the file's quality (HE-AAC without ffmpeg).
    pub reduced: bool,
    /// The sound tracks Save keeps (an OBS recording has one per source),
    /// and those it can't (without ffmpeg only the first is kept); 1 and 0
    /// until the file opened.
    pub tracks: usize,
    pub dropped: usize,
    /// The sound track it cleans (0: the first) and every track's title
    /// (empty without one; none before it opened, or without ffmpeg).
    pub track: usize,
    pub titles: Vec<String>,
    /// Its own strength: what it plays and saves at.
    pub strength: Strength,
}

impl Row {
    pub fn cleaned(&self) -> f64 {
        self.segments.iter().map(|(a, b)| b - a).sum()
    }

    pub fn attention(&self) -> Option<&Reason> {
        match &self.pass {
            Pass::Attention(r) => Some(r),
            _ => None,
        }
    }
}

/// A length before the decoder has counted it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Estimate {
    #[default]
    Exact,
    /// The file's own word: shown as is, and updated in place.
    Close,
    /// A guess that may be off by more than about 5% (a bitrate guess on
    /// VBR): not shown until it is counted.
    Rough,
}

/// The selected file's playback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Play {
    #[default]
    Paused,
    Playing,
    /// Playback waits for the cleaned front.
    CaughtUp,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    pub files: Vec<Row>,
    pub selected: Option<FileId>,
    /// The playhead, in seconds.
    pub position: f64,
    pub play: Play,
    /// The player plays the original now.
    pub original: bool,
    /// Playing only while the original is held (a hold while paused).
    pub peek: bool,
    pub strength: Strength,
    pub place: Place,
    /// Clear all took this many files a moment ago: Undo can bring them
    /// back (the page's toast).
    pub cleared: usize,
    /// A row's × took this one (its name), instead.
    pub cleared_name: Option<String>,
    /// Cleaning waits for this app's call (`Pass::Yielded`); None: an app
    /// with no name.
    pub yield_app: Option<String>,
    /// The file that starts cleaning next (its row offers no "Clean
    /// next").
    pub next_up: Option<FileId>,
    /// "Save all and quit" was chosen: HushMic quits once this many files
    /// are saved (the header says so, with Cancel).
    pub quitting: Option<usize>,
    /// A short note about the last drop or choice ("notes.txt isn't an
    /// audio or video file."), and its number: a new number is a new
    /// note (the page's toast).
    pub note: Option<(String, u64)>,
    /// Moves on every change (and with the playhead).
    pub serial: u64,
}

impl View {
    pub fn row(&self) -> Option<&Row> {
        let id = self.selected?;
        self.files.iter().find(|r| r.id == id)
    }
}

/// The page asks.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    /// "Choose files…": the source asks the desktop's file chooser.
    Choose,
    Add(Vec<std::path::PathBuf>),
    /// A new selected file (also after Add or Remove) plays HushMic: the
    /// source turns the original off (the page's hold starts afresh).
    Select(FileId),
    /// Also "Stop cleaning": nothing is written before Save.
    Remove(FileId),
    Play,
    Pause,
    /// Seconds. `drag`: part of a drag across the picture.
    Seek {
        to: f64,
        drag: bool,
    },
    /// The player plays the original (true) or HushMic. While paused, true
    /// plays for as long as it stays on (a peek); Play after it keeps
    /// playing.
    Original(bool),
    /// The selected file's strength (and the next files').
    Strength(Strength),
    /// Clean another of the file's sound tracks (0: the first).
    Track(FileId, usize),
    /// "Clean anyway": files clean on during the call.
    CleanAnyway,
    /// "Clean next": this waiting file before the others.
    CleanNext(FileId),
    /// Cancel on "Quits once 2 files are saved".
    CancelQuit,
    Save(FileId),
    /// Every file: those cleaned now, the rest when cleaned.
    SaveAll,
    CancelSave(FileId),
    SaveElsewhere(FileId),
    ShowInFolder(FileId),
    ChooseAgain(FileId),
    /// The pass again (after Engine, Cache, Failed, Crashed).
    Retry(FileId),
    /// A damaged file: clean the part before the damage.
    CleanFirst(FileId),
    /// Clear all: every file goes, for a moment undoable (no question).
    ClearAll,
    /// Undo Clear all: the files come back as they were.
    UndoClear,
}

pub trait Source {
    fn send(&mut self, c: Cmd);
    /// The newest view (the demo advances to `now`).
    fn view(&mut self, now: f64) -> View;
    /// The selected file's picture, as far as there is one: the same
    /// `Arc` until it changes. No other file's is held.
    fn picture(&self) -> Option<std::sync::Arc<crate::files::picture::Sheet>> {
        None
    }
    /// Whether something moves (the pass, the playhead): the page asks for
    /// frames at the source's rate then.
    fn busy(&self) -> bool;
    /// The daemon's newest word, each frame before [`Source::view`] (the
    /// demo has none).
    fn state(&mut self, _s: &crate::appstate::AppState, _now: f64) {}
    /// What to tell the daemon, once the frame is drawn.
    fn actions(&mut self) -> Vec<crate::appstate::Action> {
        Vec::new()
    }
}
