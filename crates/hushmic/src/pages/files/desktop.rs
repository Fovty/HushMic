//! What the Files page asks the desktop itself: the file chooser (the
//! FileChooser portal, which works in the Flatpak too), showing a file in
//! its folder (`files::reveal`) and opening it (the OpenURI portal). None of
//! it blocks the frame: each runs on its own thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

pub use crate::files::chooser::Pick;

/// Ask for recordings, one recording or a folder (`files::chooser`). The
/// answer arrives on the receiver (empty when the chooser was cancelled
/// or isn't there); `wake` repaints the window.
pub fn choose(
    title: String,
    pick: Pick,
    wake: crate::miccheck::Repaint,
) -> mpsc::Receiver<Vec<PathBuf>> {
    let (tx, rx) = mpsc::channel();
    #[cfg(test)]
    {
        // no dialog from a test: the answer it gave (`answer_next`), else
        // the receiver waits and the test answers it
        match ANSWER.with(|a| a.borrow_mut().take()) {
            Some(files) => {
                let _ = tx.send(files);
            }
            None => std::mem::forget(tx),
        }
        let _ = (title, pick, wake);
        rx
    }
    #[cfg(not(test))]
    {
        // Over the window (`window::parent`), not as a window of its own.
        let parent = crate::window::parent::get();
        std::thread::spawn(move || {
            let files = match crate::files::chooser::choose(
                pick,
                &title,
                &parent,
                crate::files::chooser::WAIT,
            ) {
                Ok(f) => f.unwrap_or_default(),
                Err(e) => {
                    eprintln!("[hushmic] file chooser: {e}");
                    Vec::new()
                }
            };
            let _ = tx.send(files);
            wake(std::time::Duration::ZERO);
        });
        rx
    }
}

#[cfg(test)]
thread_local! {
    static ANSWER: std::cell::RefCell<Option<Vec<PathBuf>>> = const { std::cell::RefCell::new(None) };
}

/// Tests: the next chooser on this thread answers `files` (empty: it was
/// cancelled).
#[cfg(test)]
pub fn answer_next(files: Vec<PathBuf>) {
    ANSWER.with(|a| *a.borrow_mut() = Some(files));
}

/// Show `path` selected in the file manager (`files::reveal`: FileManager1,
/// else the folder through the OpenURI portal), off the frame.
pub fn show_in_folder(path: &Path) {
    if cfg!(test) {
        return;
    }
    let path = path.to_owned();
    std::thread::spawn(move || {
        if let Err(e) = crate::files::reveal::show_in_folder(&path) {
            eprintln!("[hushmic] show in folder: {e}");
        }
    });
}
