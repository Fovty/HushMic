//! Drops on the clipboard's own data device (patch): a compositor may
//! deliver drag-and-drop to one data device per client only, so the
//! clipboard's device takes drops too and hands them to a handler.

use std::sync::{Arc, Mutex};

/// What a drag over the application's surfaces does.
#[derive(Debug)]
pub enum Event {
    /// A drag entered; `accepted`: it offers one of the types asked for.
    Enter { mimes: Vec<String>, accepted: bool },
    /// The drag left without a drop.
    Leave,
    /// Dropped: the data of the first type asked for that it offers.
    Drop { mime: String, data: std::io::Result<Vec<u8>> },
}

type Handler = Arc<dyn Fn(Event) + Send + Sync>;

static HANDLER: Mutex<Option<(Vec<String>, Handler)>> = Mutex::new(None);

/// Take drops: `accept` names the MIME types in order of preference;
/// `handler` gets the events, on the clipboard's thread. Without a handler
/// no drop is accepted (as before).
pub fn set_handler(accept: Vec<String>, handler: impl Fn(Event) + Send + Sync + 'static) {
    *HANDLER.lock().unwrap_or_else(|p| p.into_inner()) = Some((accept, Arc::new(handler)));
}

pub(crate) fn pick(mimes: &[String]) -> Option<String> {
    let guard = HANDLER.lock().unwrap_or_else(|p| p.into_inner());
    let (accept, _) = guard.as_ref()?;
    accept.iter().find(|a| mimes.contains(a)).cloned()
}

pub(crate) fn emit(event: Event) {
    // Called unlocked: a handler may block (a drop's data looked up), and
    // the device's thread must not wait on it to pick a type.
    let guard = HANDLER.lock().unwrap_or_else(|p| p.into_inner());
    let handler = guard.as_ref().map(|(_, h)| h.clone());
    drop(guard);
    if let Some(handler) = handler {
        handler(event);
    }
}

/// The device's two calls, for HushMic's tests (not an API).
#[doc(hidden)]
pub mod testing {
    pub fn pick(mimes: &[String]) -> Option<String> {
        super::pick(mimes)
    }

    pub fn emit(event: super::Event) {
        super::emit(event)
    }
}
