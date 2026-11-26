//! The daemon's file jobs with real worker processes (fake ones: no
//! engine, 1 s per file, see `files::worker::fake`): the played file goes
//! first while the other is stopped, and workers die with the thread that
//! started them, stopped or not.

use hushmic::appstate::JobState;
use hushmic::files::jobs::{EngineChoice, Heard, JobEvent, Jobs, Processes};
use hushmic::files::worker::{Msg, Problem};
use hushmic_denoiser::Inference;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

fn jobs(name: &str) -> (Jobs, Receiver<JobEvent>, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("hushmic-files-jobs-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (tx, rx) = channel();
    let workers = Processes::new(env!("CARGO_BIN_EXE_hushmic").into(), move |ev| {
        let _ = tx.send(ev);
    });
    let engine = EngineChoice {
        model: "none".into(),
        inference: Inference::Auto,
        ffmpeg: None,
        fake: true,
    };
    // one file at a time, as on a small machine or during a call (a
    // bigger one cleans several side by side: the unit tests)
    let jobs = Jobs::new(root.clone(), engine, Box::new(workers)).with_lanes(1);
    (jobs, rx, root)
}

/// Any file will do for a fake worker (in the scratch root, which goes
/// at the end).
fn file(root: &std::path::Path, k: u32) -> String {
    std::fs::create_dir_all(root).unwrap();
    let p = root.join(format!("in{k}.wav"));
    std::fs::write(&p, b"").unwrap();
    p.display().to_string()
}

fn lit_end(m: &Msg) -> Option<u64> {
    match m {
        Msg::Lit { ranges } => ranges.last().map(|r| r.1),
        _ => None,
    }
}

#[test]
fn the_played_file_goes_first_and_the_other_goes_on_after() {
    let (mut j, rx, root) = jobs("play");
    let a = j.add(&file(&root, 1)).unwrap();
    let b = j.add(&file(&root, 2)).unwrap();
    // A has started cleaning; then the user plays B.
    loop {
        let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        let first_lit = ev.id == a && matches!(&ev.heard, Heard::Said(m) if lit_end(m).is_some());
        j.heard(ev);
        if first_lit {
            break;
        }
    }
    assert!(j.play(b, 0));
    // Until both are done and, told so, both workers have exited.
    let (mut done, mut gone) = (Vec::new(), 0);
    while done.len() < 2 || gone < 2 {
        let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        if ev.heard == Heard::Said(Msg::Done) {
            done.push(ev.id);
        }
        gone += usize::from(ev.heard == Heard::Gone);
        j.heard(ev);
    }
    // A started first and both take a second: B finishes first only
    // because A was stopped meanwhile.
    assert_eq!(done, [b, a]);
    let s = j.state();
    assert!(s
        .jobs
        .iter()
        .all(|f| f.state == JobState::Done && f.lit == [(0, 480_000)]));
    j.shutdown();
    assert!(!root.join("job-1").exists());
    let _ = std::fs::remove_dir_all(root);
}

/// Feed the jobs every word until `id` has saved (or failed to).
fn until_saved(j: &mut Jobs, rx: &Receiver<JobEvent>, id: u64) -> Result<String, Problem> {
    loop {
        let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        j.heard(ev);
        let s = j.state();
        let f = s.jobs.iter().find(|f| f.id == id).unwrap();
        if !f.saving {
            return f
                .saved
                .clone()
                .ok_or_else(|| f.save_problem.unwrap_or_else(|| panic!("{f:?}")));
        }
    }
}

#[test]
fn save_prepares_the_output_and_puts_it_next_to_the_original() {
    let (mut j, rx, root) = jobs("save");
    j.set_strength(12.0);
    let a = j.add(&file(&root, 5)).unwrap();
    // Asked at once: it saves when cleaned and prepared.
    assert!(j.save(a, None).is_ok());
    let at = until_saved(&mut j, &rx, a).unwrap();
    assert_eq!(at, root.join("in5 (cleaned).wav").display().to_string());
    assert_eq!(std::fs::read_to_string(&at).unwrap(), "12");
    let s = j.state();
    assert_eq!(
        (s.jobs[0].prepared, s.jobs[0].out_ext.as_str()),
        (Some(12.0), "wav")
    );
    // Again, into a folder: a second name never over the first.
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("in5 (cleaned).wav"), b"mine").unwrap();
    assert!(j.save(a, Some(out.clone())).is_ok());
    let at = until_saved(&mut j, &rx, a).unwrap();
    assert_eq!(at, out.join("in5 (cleaned 2).wav").display().to_string());
    // A preparation that fails says so.
    let nosave = root.join("nosave.wav");
    std::fs::write(&nosave, b"").unwrap();
    let b = j.add(nosave.to_str().unwrap()).unwrap();
    assert!(j.save(b, None).is_ok());
    assert_eq!(until_saved(&mut j, &rx, b), Err(Problem::Failed));
    j.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

/// The cleaning workers of ours that run with `cache` in their arguments.
/// A waiting file's first look (`--probe`) is a worker process too, there
/// for a moment and gone by itself: not one of these.
fn workers_with(cache: &str) -> Vec<i32> {
    let me = std::process::id().to_string();
    let mut out = Vec::new();
    for e in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let stat = std::fs::read_to_string(e.path().join("stat")).unwrap_or_default();
        let parent = stat
            .rsplit(')')
            .next()
            .and_then(|r| r.split_whitespace().nth(1));
        let cmd = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
        let cmd = String::from_utf8_lossy(&cmd);
        let probe = cmd.split('\0').any(|a| a == "--probe");
        if parent == Some(me.as_str()) && cmd.contains(cache) && !probe {
            out.push(pid);
        }
    }
    out
}

fn state_of(pid: i32) -> char {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    stat.rsplit(')')
        .next()
        .and_then(|r| r.trim_start().chars().next())
        .unwrap_or('?')
}

#[test]
fn workers_die_with_the_thread_that_started_them_even_stopped() {
    let (root_tx, root_rx) = channel();
    let (seen_tx, seen_rx) = channel::<()>();
    let t = std::thread::spawn(move || {
        let (mut j, _rx, root) = jobs("death");
        let _a = j.add(&file(&root, 3)).unwrap();
        let b = j.add(&file(&root, 4)).unwrap();
        // B is played: its worker starts and A's is told to stop, both
        // before this returns.
        j.play(b, 0);
        root_tx.send(root.clone()).unwrap();
        // Hold the thread until the test has seen both workers (a clock
        // here lost the race on a loaded runner).
        let _ = seen_rx.recv_timeout(Duration::from_secs(120));
        // No shutdown: the thread just ends, as a crashed daemon would.
        std::mem::forget(j);
    });
    let root = root_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    let root_s = root.display().to_string();
    let pids = workers_with(&root_s);
    // A stop is a signal: the worker is stopped once it has taken it,
    // which is soon, not at once.
    let patience = Instant::now() + Duration::from_secs(60);
    let stopped = loop {
        let n = pids.iter().filter(|&&p| state_of(p) == 'T').count();
        if n > 0 || pids.len() != 2 || Instant::now() >= patience {
            break n;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let _ = seen_tx.send(());
    assert_eq!(pids.len(), 2, "both workers run");
    assert_eq!(stopped, 1, "one is stopped");
    t.join().unwrap();
    for &pid in &pids {
        let mut status = 0;
        // SAFETY: our own child; this reaps it.
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        assert_eq!(r, pid);
        assert!(libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL);
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn quit_cancels_a_copy_under_way_and_leaves_no_partial_file() {
    use std::os::unix::fs::MetadataExt;
    let (mut j, rx, root) = jobs("quitcopy");
    // Another disk than the cache, so Save copies.
    let out =
        std::path::Path::new("/dev/shm").join(format!("hushmic-quitcopy-{}", std::process::id()));
    if std::fs::create_dir_all(&out).is_err()
        || std::fs::metadata(&out).unwrap().dev()
            == std::fs::metadata(std::env::temp_dir()).unwrap().dev()
    {
        eprintln!("SKIP: no second disk");
        return;
    }
    j.set_strength(12.0);
    let a = j.add(&file(&root, 7)).unwrap();
    // Cleaned and prepared (the fake writes a tiny file).
    loop {
        let ev = rx.recv_timeout(Duration::from_secs(20)).unwrap();
        j.heard(ev);
        if j.state().jobs[0].prepared == Some(12.0) {
            break;
        }
    }
    // A large output, so the copy is still under way at Quit.
    let prepared = root.join("job-1").join("prepared.wav");
    std::fs::write(&prepared, vec![7u8; 256 << 20]).unwrap();
    assert!(j.save(a, Some(out.clone())).is_ok());
    std::thread::sleep(Duration::from_millis(20));
    j.shutdown();
    let left: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        left.iter().all(|n| !n.ends_with(".partial")),
        "a partial copy stayed: {left:?}"
    );
    let notes = std::fs::read_dir(root.join("partials"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(notes, 0, "and its note");
    let _ = std::fs::remove_dir_all(&out);
    let _ = std::fs::remove_dir_all(root);
}
