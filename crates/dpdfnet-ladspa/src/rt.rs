//! Realtime scheduling for the inference worker (issue #14).
//!
//! On a systemd desktop the cpu cgroup controller ranks HushMic's chain
//! against other cgroups as a group, so a nice level cannot protect the
//! worker from a busy browser; only a realtime policy can. This module asks
//! for `SCHED_RR` the way `libpipewire-module-rt` does for the data loop:
//! directly when the user's rlimits allow it (audio group, pam limits,
//! `realtime-privileges`), otherwise through rtkit on the system bus, or
//! the Realtime portal on the session bus inside a Flatpak. Every failure is
//! one log line
//! and the worker stays at timeshare; the adaptive engine does the rest.
//!
//! libdbus is loaded with `dlopen` on demand: the plugin links nothing new,
//! and a host without it just reports `no dbus`.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const DATA_LOOP_WINDOW: Duration = Duration::from_secs(3);
const DATA_LOOP_POLL: Duration = Duration::from_millis(20);
// Helper budget only: loop setup plus the existing two second broker wait.
// Activation is independent and Drop still waits at most 50 ms.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// A detached helper must return into mapped plugin code as well as libdbus.
/// Pin once for the process lifetime. Executables already have that lifetime.
pub(crate) fn keep_plugin_loaded() -> bool {
    static PINNED: OnceLock<bool> = OnceLock::new();
    *PINNED.get_or_init(|| {
        // SAFETY: dladdr writes a plain structure for an address in this module.
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        if unsafe { libc::dladdr(keep_plugin_loaded as *const () as *const c_void, &mut info) } == 0
            || info.dli_fname.is_null()
        {
            return false;
        }
        // SAFETY: dladdr supplied a valid filename for a live mapping.
        let name = unsafe { CStr::from_ptr(info.dli_fname) };
        use std::os::unix::ffi::OsStrExt;
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(name.to_bytes()));
        if std::fs::canonicalize(path)
            .ok()
            .zip(std::fs::read_link("/proc/self/exe").ok())
            .is_some_and(|(a, b)| a == b)
        {
            return true;
        }
        // SAFETY: retain this existing mapping permanently. Never dlclose it.
        !unsafe {
            libc::dlopen(
                name.as_ptr(),
                libc::RTLD_NOW | libc::RTLD_NOLOAD | libc::RTLD_NODELETE,
            )
        }
        .is_null()
    })
}

pub(crate) fn current_thread_is_realtime() -> bool {
    // SAFETY: a zero tid refers to the calling thread.
    let policy = unsafe { libc::sched_getscheduler(0) } & !libc::SCHED_RESET_ON_FORK;
    policy == libc::SCHED_RR || policy == libc::SCHED_FIFO
}

struct Deadline {
    until: Instant,
    cancelled: Arc<AtomicBool>,
    grant_in_flight: Arc<AtomicBool>,
}

impl Deadline {
    fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            Err("cancelled".into())
        } else if Instant::now() >= self.until {
            Err("timeout".into())
        } else {
            Ok(())
        }
    }

    fn remaining_ms(&self) -> Result<c_int, String> {
        self.check()?;
        Ok(self
            .until
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, REPLY_TIMEOUT_MS as u128) as c_int)
    }
}

struct Pending {
    pending: Arc<AtomicBool>,
    grant_in_flight: Arc<AtomicBool>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        // A timed out method can still run in the remote broker. Reserve
        // the target tid for the process lifetime if its outcome is unknown.
        if !self.grant_in_flight.load(Ordering::Acquire) {
            self.pending.store(false, Ordering::Release);
        }
    }
}

/// Bound the caller's entire wait, including connection and registration.
/// libdbus has no cancellation API for bus_get_private. A stalled call may
/// remain detached, with both libraries pinned and the target tid retained.
pub(crate) fn request_realtime_bounded(
    tid: libc::pid_t,
    cancelled: Arc<AtomicBool>,
    pending: Arc<AtomicBool>,
    timeout: Duration,
) -> RtOutcome {
    // Keep the fallback reason even if the caller times out while the
    // broker is still blocked and cannot return its own outcome yet.
    let no_data_loop = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&no_data_loop);
    let outcome = request_bounded(cancelled, pending, timeout, move |deadline| {
        request_realtime_inner(tid, deadline, &observed)
    });
    match outcome {
        RtOutcome::Unavailable(reason) if no_data_loop.load(Ordering::Acquire) => {
            RtOutcome::Unavailable(format!("{reason}; no data loop found"))
        }
        outcome => outcome,
    }
}

fn request_bounded(
    cancelled: Arc<AtomicBool>,
    pending: Arc<AtomicBool>,
    timeout: Duration,
    call: impl FnOnce(&Deadline) -> RtOutcome + Send + 'static,
) -> RtOutcome {
    let deadline = Deadline {
        until: Instant::now() + timeout,
        cancelled,
        grant_in_flight: Arc::new(AtomicBool::new(false)),
    };
    if !keep_plugin_loaded() {
        pending.store(false, Ordering::Release);
        return RtOutcome::Unavailable("cannot retain plugin".into());
    }
    let watcher = Deadline {
        until: deadline.until,
        cancelled: Arc::clone(&deadline.cancelled),
        grant_in_flight: Arc::clone(&deadline.grant_in_flight),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let lease = Pending {
        pending,
        grant_in_flight: Arc::clone(&deadline.grant_in_flight),
    };
    let task = std::thread::Builder::new()
        .name("hushmic-broker".into())
        .spawn(move || {
            let _lease = lease;
            let result = call(&deadline);
            let _ = tx.send(result);
        });
    let Ok(task) = task else {
        return RtOutcome::Unavailable("no broker thread".into());
    };
    loop {
        if let Err(reason) = watcher.check() {
            return RtOutcome::Unavailable(reason);
        }
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(outcome) => {
                // Receiving the outcome is enough. Joining can wait on TLS
                // destructors after a completed request, so detach here too.
                drop(task);
                return outcome;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return RtOutcome::Unavailable("broker thread stopped".into());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// Upper bound, further limited by the lowest data loop priority.
pub const RT_PRIORITY: i32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LoopSchedule {
    policy: i32,
    priority: i32,
}

/// Read comm as bytes, removing only the newline supplied by procfs.
/// Spaces, parentheses and even newlines within a name are literal bytes.
fn comm_name(comm: &[u8]) -> &[u8] {
    comm.strip_suffix(b"\n").unwrap_or(comm)
}

/// `data-loop.N` since PipeWire 1.2, `pw-data-loop` in 1.0 (seen on
/// Ubuntu 24.04 in CI).
fn is_data_loop(name: &[u8]) -> bool {
    name.starts_with(b"data-loop") || name.starts_with(b"pw-data-loop")
}

fn is_legacy_loop(name: &[u8], tid: libc::pid_t, pid: libc::pid_t) -> bool {
    // PipeWire 0.3 (0.3.65 in Debian 12) creates the loop unnamed, so it
    // inherits pipewire. Only use these candidates when no named loop exists.
    // Sources and the conservative legacy limitation are in the spec.
    name == b"pipewire" && tid != pid
}

fn thread_schedule(tid: libc::pid_t) -> Result<LoopSchedule, i32> {
    // SAFETY: scheduler queries accept a tid and a valid output pointer.
    let policy = unsafe { libc::sched_getscheduler(tid) };
    if policy < 0 {
        return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
    }
    let mut param = libc::sched_param { sched_priority: 0 };
    if unsafe { libc::sched_getparam(tid, &mut param) } < 0 {
        return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
    }
    Ok(LoopSchedule {
        policy: policy & !libc::SCHED_RESET_ON_FORK,
        priority: param.sched_priority,
    })
}

/// Only the helper scans procfs. Missing tasks exited during the scan;
/// other read failures prevent a grant because their policy is unknown.
fn data_loop_schedules() -> Vec<Option<LoopSchedule>> {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return vec![None];
    };
    let mut named = Vec::new();
    let mut legacy = Vec::new();
    let mut unreadable = false;
    for task in tasks {
        let Ok(task) = task else {
            unreadable = true;
            continue;
        };
        let Some(tid) = task.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        let comm = match std::fs::read(task.path().join("comm")) {
            Ok(comm) => comm,
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => continue,
            Err(_) => {
                unreadable = true;
                continue;
            }
        };
        let name = comm_name(&comm);
        let candidates = if is_data_loop(name) {
            &mut named
        } else if is_legacy_loop(name, tid, std::process::id() as libc::pid_t) {
            &mut legacy
        } else {
            continue;
        };
        match thread_schedule(tid) {
            Err(libc::ESRCH) => {}
            schedule => candidates.push(schedule.ok()),
        }
    }
    let mut result = select_loops(named, legacy);
    if unreadable {
        result.push(None);
    }
    result
}

fn is_realtime(s: &Option<LoopSchedule>) -> bool {
    s.is_some_and(|s| s.policy == libc::SCHED_FIFO || s.policy == libc::SCHED_RR)
}

/// Named loops win. In PipeWire 0.3 the data loop is unnamed and so are
/// other host threads, which stay timeshare; module-rt only raises the data
/// loop, so a realtime unnamed thread identifies it and a timeshare one says
/// nothing. With no realtime candidate the result is empty: the no data loop
/// fallback keeps the worker's usual request.
fn select_loops(
    named: Vec<Option<LoopSchedule>>,
    legacy: Vec<Option<LoopSchedule>>,
) -> Vec<Option<LoopSchedule>> {
    if !named.is_empty() {
        return named;
    }
    legacy.into_iter().filter(is_realtime).collect()
}

#[derive(Debug, PartialEq, Eq)]
enum LoopDecision {
    Request { priority: i32, detail: String },
    Wait,
    Skip(&'static str),
}

fn data_loop_decision(loops: &[Option<LoopSchedule>], window_elapsed: bool) -> LoopDecision {
    if loops.is_empty() {
        return LoopDecision::Request {
            priority: RT_PRIORITY,
            detail: "no data loop found".into(),
        };
    }
    if loops.iter().any(|s| !is_realtime(s)) {
        return if window_elapsed {
            LoopDecision::Skip("data loop not realtime")
        } else {
            LoopDecision::Wait
        };
    }
    let lowest = loops.iter().flatten().min_by_key(|s| s.priority).unwrap();
    if lowest.priority <= 1 {
        return LoopDecision::Skip("data loop priority too low");
    }
    let policy = if lowest.policy == libc::SCHED_FIFO {
        "FIFO"
    } else {
        "RR"
    };
    LoopDecision::Request {
        priority: RT_PRIORITY.min(lowest.priority - 1),
        detail: format!("data loop {policy} {}", lowest.priority),
    }
}

fn request_after_data_loops(
    deadline: &Deadline,
    window: Duration,
    mut scan: impl FnMut() -> Vec<Option<LoopSchedule>>,
    request: impl FnOnce(i32, &str) -> RtOutcome,
) -> RtOutcome {
    if std::env::var("HUSHMIC_WORKER_RT").as_deref() == Ok("0") {
        return RtOutcome::Unavailable("disabled".into());
    }
    let until = Instant::now() + window;
    loop {
        if let Err(reason) = deadline.check() {
            return RtOutcome::Unavailable(reason);
        }
        match data_loop_decision(&scan(), Instant::now() >= until) {
            LoopDecision::Request { priority, detail } => {
                if let Err(reason) = deadline.check() {
                    return RtOutcome::Unavailable(reason);
                }
                return request(priority, &detail);
            }
            LoopDecision::Skip(reason) => return RtOutcome::Unavailable(reason.into()),
            LoopDecision::Wait => std::thread::sleep(
                DATA_LOOP_POLL
                    .min(until.saturating_duration_since(Instant::now()))
                    .min(deadline.until.saturating_duration_since(Instant::now())),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtOutcome {
    Granted(i32, String),
    /// One of: `disabled`, `EPERM`, `no dbus`, `<errno name>`,
    /// `rtkit: <D-Bus error>`, `portal: <D-Bus error>` (inside a Flatpak),
    /// `<who>: not applied`, `no thread: <error>`.
    Unavailable(String),
}

/// The log contract line for an outcome.
pub fn contract(o: &RtOutcome) -> String {
    match o {
        RtOutcome::Granted(p, detail) => format!("worker: realtime priority {p} ({detail})"),
        RtOutcome::Unavailable(why) => format!("worker: realtime priority not available ({why})"),
    }
}

#[cfg(test)]
fn request_realtime(tid: libc::pid_t) -> RtOutcome {
    request_realtime_bounded(
        tid,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(true)),
        REQUEST_TIMEOUT,
    )
}

fn request_realtime_inner(
    tid: libc::pid_t,
    deadline: &Deadline,
    no_data_loop: &AtomicBool,
) -> RtOutcome {
    request_after_data_loops(
        deadline,
        DATA_LOOP_WINDOW,
        data_loop_schedules,
        |priority, detail| {
            no_data_loop.store(detail == "no data loop found", Ordering::Release);
            match direct(tid, priority) {
                Ok(()) => verify(tid, "direct", priority, detail),
                Err(libc::EPERM) => broker(tid, deadline, priority, detail),
                Err(e) => RtOutcome::Unavailable(errno_name(e)),
            }
        },
    )
}

fn errno_name(e: c_int) -> String {
    match e {
        libc::EPERM => "EPERM".into(),
        libc::ESRCH => "ESRCH".into(),
        libc::EINVAL => "EINVAL".into(),
        _ => format!("errno {e}"),
    }
}

fn direct(tid: libc::pid_t, priority: i32) -> Result<(), c_int> {
    let param = libc::sched_param {
        sched_priority: priority,
    };
    // SAFETY: plain syscall with a valid, fully initialised sched_param.
    let r = unsafe {
        libc::sched_setscheduler(tid, libc::SCHED_RR | libc::SCHED_RESET_ON_FORK, &param)
    };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

/// Re-read the policy after a grant: a broker may answer success and still
/// leave the thread where it was.
fn verify(tid: libc::pid_t, who: &str, priority: i32, detail: &str) -> RtOutcome {
    // SAFETY: plain syscalls.
    let policy = unsafe { libc::sched_getscheduler(tid) } & !libc::SCHED_RESET_ON_FORK;
    if policy == libc::SCHED_RR || policy == libc::SCHED_FIFO {
        let mut param = libc::sched_param { sched_priority: 0 };
        // SAFETY: valid out pointer.
        let ok = unsafe { libc::sched_getparam(tid, &mut param) } == 0;
        RtOutcome::Granted(
            if ok { param.sched_priority } else { priority },
            detail.into(),
        )
    } else {
        RtOutcome::Unavailable(format!("{who}: not applied"))
    }
}

const BUS_SESSION: c_int = 0;
const BUS_SYSTEM: c_int = 1;
const TYPE_UINT32: c_int = b'u' as c_int;
const TYPE_UINT64: c_int = b't' as c_int;
const REPLY_TIMEOUT_MS: c_int = 2000;

struct Broker {
    bus: c_int,
    service: &'static CStr,
    path: &'static CStr,
    iface: &'static CStr,
    who: &'static str,
}

const PORTAL: Broker = Broker {
    bus: BUS_SESSION,
    service: c"org.freedesktop.portal.Desktop",
    path: c"/org/freedesktop/portal/desktop",
    iface: c"org.freedesktop.portal.Realtime",
    who: "portal",
};

const RTKIT: Broker = Broker {
    bus: BUS_SYSTEM,
    service: c"org.freedesktop.RealtimeKit1",
    path: c"/org/freedesktop/RealtimeKit1",
    iface: c"org.freedesktop.RealtimeKit1",
    who: "rtkit",
};

fn broker(tid: libc::pid_t, deadline: &Deadline, priority: i32, detail: &str) -> RtOutcome {
    let Some(dbus) = DBus::load() else {
        return RtOutcome::Unavailable("no dbus".into());
    };
    // Inside a Flatpak the system bus is out of reach and the portal is the
    // only route (it forwards to rtkit); outside one, rtkit directly. The
    // portal is not tried on hosts: the call would D-Bus-activate
    // xdg-desktop-portal as a side effect of loading a LADSPA plugin and
    // wait out the reply timeout when it is not running.
    let broker = if std::path::Path::new("/.flatpak-info").exists() {
        &PORTAL
    } else {
        &RTKIT
    };
    match dbus.make_realtime(broker, tid, deadline, priority) {
        Ok(()) => verify(tid, broker.who, priority, detail),
        Err(e) => RtOutcome::Unavailable(format!("{}: {e}", broker.who)),
    }
}

/// Mirror of the public libdbus ABI (dbus-errors.h): two string pointers,
/// a bit-field word and a padding pointer.
#[repr(C)]
struct DBusError {
    name: *const c_char,
    message: *const c_char,
    dummy: u32,
    padding: *mut c_void,
}

/// The public struct is 72 bytes with pointer alignment; libdbus only
/// writes inside that, so an oversized aligned buffer is safe.
#[repr(C, align(8))]
struct DBusMessageIter([u8; 128]);

type FnBusGetPrivate = unsafe extern "C" fn(c_int, *mut DBusError) -> *mut c_void;
type FnConnVoid = unsafe extern "C" fn(*mut c_void);
type FnConnSetExit = unsafe extern "C" fn(*mut c_void, u32);
type FnNewMethodCall =
    unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *const c_char) -> *mut c_void;
type FnIterInitAppend = unsafe extern "C" fn(*mut c_void, *mut DBusMessageIter);
type FnIterAppendBasic = unsafe extern "C" fn(*mut DBusMessageIter, c_int, *const c_void) -> u32;
type FnSendBlock =
    unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, *mut DBusError) -> *mut c_void;
type FnErrorInit = unsafe extern "C" fn(*mut DBusError);
type FnErrorIsSet = unsafe extern "C" fn(*const DBusError) -> u32;

struct DBus {
    bus_get_private: FnBusGetPrivate,
    connection_set_exit_on_disconnect: FnConnSetExit,
    connection_close: FnConnVoid,
    connection_unref: FnConnVoid,
    message_new_method_call: FnNewMethodCall,
    message_iter_init_append: FnIterInitAppend,
    message_iter_append_basic: FnIterAppendBasic,
    connection_send_with_reply_and_block: FnSendBlock,
    message_unref: FnConnVoid,
    error_init: FnErrorInit,
    error_is_set: FnErrorIsSet,
    error_free: FnErrorInit,
}

impl DBus {
    /// `dlopen` libdbus and resolve the twelve functions used here. The
    /// handle is never closed: libdbus keeps process-wide state.
    fn load() -> Option<DBus> {
        // SAFETY: dlopen/dlsym with valid C strings; the returned function
        // pointers are only ever called with the signatures libdbus
        // documents for these names.
        unsafe {
            let lib = libc::dlopen(
                c"libdbus-1.so.3".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            );
            if lib.is_null() {
                return None;
            }
            macro_rules! sym {
                ($name:literal, $ty:ty) => {{
                    let p = libc::dlsym(lib, $name.as_ptr());
                    if p.is_null() {
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, $ty>(p)
                }};
            }
            Some(DBus {
                bus_get_private: sym!(c"dbus_bus_get_private", FnBusGetPrivate),
                connection_set_exit_on_disconnect: sym!(
                    c"dbus_connection_set_exit_on_disconnect",
                    FnConnSetExit
                ),
                connection_close: sym!(c"dbus_connection_close", FnConnVoid),
                connection_unref: sym!(c"dbus_connection_unref", FnConnVoid),
                message_new_method_call: sym!(c"dbus_message_new_method_call", FnNewMethodCall),
                message_iter_init_append: sym!(c"dbus_message_iter_init_append", FnIterInitAppend),
                message_iter_append_basic: sym!(
                    c"dbus_message_iter_append_basic",
                    FnIterAppendBasic
                ),
                connection_send_with_reply_and_block: sym!(
                    c"dbus_connection_send_with_reply_and_block",
                    FnSendBlock
                ),
                message_unref: sym!(c"dbus_message_unref", FnConnVoid),
                error_init: sym!(c"dbus_error_init", FnErrorInit),
                error_is_set: sym!(c"dbus_error_is_set", FnErrorIsSet),
                error_free: sym!(c"dbus_error_free", FnErrorInit),
            })
        }
    }

    fn error_text(&self, err: &DBusError) -> String {
        // SAFETY: `name` is a valid C string whenever the error is set.
        let name = unsafe {
            if err.name.is_null() {
                "unknown".to_string()
            } else {
                CStr::from_ptr(err.name).to_string_lossy().into_owned()
            }
        };
        name.trim_start_matches("org.freedesktop.DBus.Error.")
            .to_string()
    }

    /// `MakeThreadRealtimeWithPID(pid, tid, priority)` on `broker`.
    fn make_realtime(
        &self,
        broker: &Broker,
        tid: libc::pid_t,
        deadline: &Deadline,
        priority: i32,
    ) -> Result<(), String> {
        deadline.check()?;
        let method = CString::new("MakeThreadRealtimeWithPID").expect("static");
        // SAFETY: every pointer handed to libdbus is either a valid C string
        // that outlives the call, a struct initialised by libdbus itself
        // (`dbus_error_init`, `dbus_message_iter_init_append`), or a handle
        // libdbus returned and that is released exactly once below.
        unsafe {
            let mut err = DBusError {
                name: std::ptr::null(),
                message: std::ptr::null(),
                dummy: 0,
                padding: std::ptr::null_mut(),
            };
            (self.error_init)(&mut err);
            let conn = (self.bus_get_private)(broker.bus, &mut err);
            if conn.is_null() {
                let e = if (self.error_is_set)(&err) != 0 {
                    self.error_text(&err)
                } else {
                    "no bus".into()
                };
                (self.error_free)(&mut err);
                return Err(e);
            }
            // A shared bus connection would _exit() the whole host on a
            // disconnect; a private one with this off just fails the call.
            (self.connection_set_exit_on_disconnect)(conn, 0);
            if let Err(reason) = deadline.check() {
                (self.connection_close)(conn);
                (self.connection_unref)(conn);
                return Err(reason);
            }
            let msg = (self.message_new_method_call)(
                broker.service.as_ptr(),
                broker.path.as_ptr(),
                broker.iface.as_ptr(),
                method.as_ptr(),
            );
            let result = if msg.is_null() {
                Err("message allocation failed".to_string())
            } else {
                let pid: u64 = libc::getpid() as u64;
                let thread: u64 = tid as u64;
                let priority: u32 = priority as u32;
                let mut iter = DBusMessageIter([0; 128]);
                (self.message_iter_init_append)(msg, &mut iter);
                let appended = (self.message_iter_append_basic)(
                    &mut iter,
                    TYPE_UINT64,
                    &pid as *const u64 as *const c_void,
                ) != 0
                    && (self.message_iter_append_basic)(
                        &mut iter,
                        TYPE_UINT64,
                        &thread as *const u64 as *const c_void,
                    ) != 0
                    && (self.message_iter_append_basic)(
                        &mut iter,
                        TYPE_UINT32,
                        &priority as *const u32 as *const c_void,
                    ) != 0;
                let r = if !appended {
                    Err("message build failed".to_string())
                } else {
                    let timeout = match deadline.remaining_ms() {
                        Ok(timeout) => timeout,
                        Err(reason) => {
                            (self.message_unref)(msg);
                            (self.connection_close)(conn);
                            (self.connection_unref)(conn);
                            return Err(reason);
                        }
                    };
                    let reply =
                        (self.connection_send_with_reply_and_block)(conn, msg, timeout, &mut err);
                    if reply.is_null() {
                        let e = if (self.error_is_set)(&err) != 0 {
                            self.error_text(&err)
                        } else {
                            "no reply".into()
                        };
                        if !matches!(
                            e.as_str(),
                            "NoReply"
                                | "Disconnected"
                                | "Timeout"
                                | "TimedOut"
                                | "unknown"
                                | "no reply"
                        ) {
                            deadline.grant_in_flight.store(false, Ordering::Release);
                        }
                        (self.error_free)(&mut err);
                        Err(e)
                    } else {
                        deadline.grant_in_flight.store(false, Ordering::Release);
                        (self.message_unref)(reply);
                        Ok(())
                    }
                };
                (self.message_unref)(msg);
                r
            };
            (self.connection_close)(conn);
            (self.connection_unref)(conn);
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // The env var is process-global; serialise the tests that touch it.
    static ENV: Mutex<()> = Mutex::new(());

    fn my_tid() -> libc::pid_t {
        // SAFETY: gettid has no preconditions.
        unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t }
    }

    fn test_deadline() -> Deadline {
        Deadline {
            until: Instant::now() + Duration::from_secs(1),
            cancelled: Arc::new(AtomicBool::new(false)),
            grant_in_flight: Arc::new(AtomicBool::new(false)),
        }
    }

    fn schedule(policy: i32, priority: i32) -> Option<LoopSchedule> {
        Some(LoopSchedule { policy, priority })
    }

    #[test]
    fn legacy_hosts_trust_only_a_realtime_unnamed_thread() {
        let ts = schedule(libc::SCHED_OTHER, 0);
        let fifo = schedule(libc::SCHED_FIFO, 83);
        // Old PipeWire: the data loop is FIFO next to other unnamed
        // timeshare threads. The timeshare ones must not cost the grant.
        let loops = select_loops(Vec::new(), vec![ts, fifo, ts]);
        assert_eq!(
            data_loop_decision(&loops, true),
            LoopDecision::Request {
                priority: RT_PRIORITY,
                detail: "data loop FIFO 83".into(),
            }
        );
        // Only timeshare unnamed threads: nothing identifies the loop, so
        // the usual request stands.
        let loops = select_loops(Vec::new(), vec![ts, ts]);
        assert!(loops.is_empty());
        assert!(matches!(
            data_loop_decision(&loops, false),
            LoopDecision::Request {
                priority: RT_PRIORITY,
                ..
            }
        ));
        // A named loop wins over any unnamed candidate.
        let loops = select_loops(vec![ts], vec![fifo]);
        assert_eq!(
            data_loop_decision(&loops, true),
            LoopDecision::Skip("data loop not realtime")
        );
    }

    #[test]
    fn comm_preserves_awkward_names_and_matches_supported_loops() {
        for name in [
            "data-loop",
            "data-loop.0",
            "data-loop (x))",
            "data-loop\n(x)",
            "a (b)) c",
            " name with gap ",
            "pipewire",
            "pw-data-loop",
        ] {
            let record = format!("{name}\n");
            assert_eq!(comm_name(record.as_bytes()), name.as_bytes());
            assert_eq!(
                is_data_loop(comm_name(record.as_bytes())),
                name.starts_with("data-loop") || name.starts_with("pw-data-loop")
            );
        }
        assert_eq!(comm_name(b"data-loop.0"), b"data-loop.0");
        assert!(!is_data_loop(b" data-loop.0"));
        assert!(!is_data_loop(b"other-data-loop"));
        assert!(is_legacy_loop(b"pipewire", 11, 10));
        assert!(!is_legacy_loop(b"pipewire", 10, 10));
        assert!(!is_legacy_loop(b"pipewire-pulse", 11, 10));
    }

    #[test]
    fn priority_is_below_every_realtime_data_loop() {
        for (loops, priority, detail) in [
            (
                vec![schedule(libc::SCHED_FIFO, 83)],
                10,
                "data loop FIFO 83",
            ),
            (vec![schedule(libc::SCHED_RR, 5)], 4, "data loop RR 5"),
            (
                vec![schedule(libc::SCHED_FIFO, 83), schedule(libc::SCHED_RR, 5)],
                4,
                "data loop RR 5",
            ),
            (vec![], 10, "no data loop found"),
        ] {
            for elapsed in [false, true] {
                assert_eq!(
                    data_loop_decision(&loops, elapsed),
                    LoopDecision::Request {
                        priority,
                        detail: detail.into(),
                    }
                );
            }
        }
        for policy in [libc::SCHED_RR, libc::SCHED_FIFO] {
            assert_eq!(
                data_loop_decision(&[schedule(policy, 1)], false),
                LoopDecision::Skip("data loop priority too low")
            );
        }
    }

    #[test]
    fn timeshare_and_unreadable_loops_wait_then_skip() {
        for unknown in [
            None,
            schedule(libc::SCHED_OTHER, 0),
            schedule(libc::SCHED_BATCH, 0),
            schedule(libc::SCHED_IDLE, 0),
            schedule(libc::SCHED_DEADLINE, 0),
        ] {
            for loops in [vec![unknown], vec![schedule(libc::SCHED_FIFO, 83), unknown]] {
                assert_eq!(data_loop_decision(&loops, false), LoopDecision::Wait);
                assert_eq!(
                    data_loop_decision(&loops, true),
                    LoopDecision::Skip("data loop not realtime")
                );
            }
        }
    }

    #[test]
    fn late_data_loop_grant_proceeds_with_lower_worker_priority() {
        let _g = ENV.lock().unwrap();
        std::env::remove_var("HUSHMIC_WORKER_RT");
        let mut scans = 0;
        let outcome = request_after_data_loops(
            &test_deadline(),
            Duration::from_millis(200),
            || {
                scans += 1;
                vec![if scans == 1 {
                    schedule(libc::SCHED_OTHER, 0)
                } else {
                    schedule(libc::SCHED_FIFO, 83)
                }]
            },
            |priority, detail| RtOutcome::Granted(priority, detail.into()),
        );
        assert_eq!(scans, 2);
        assert_eq!(outcome, RtOutcome::Granted(10, "data loop FIFO 83".into()));
    }

    #[test]
    fn timeshare_data_loop_in_this_process_never_requests_a_policy_change() {
        let _g = ENV.lock().unwrap();
        std::env::remove_var("HUSHMIC_WORKER_RT");
        let (ready, started) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let data_loop = std::thread::Builder::new()
            .name("data-loop.0".into())
            .spawn(move || {
                ready.send(thread_schedule(my_tid()).unwrap()).unwrap();
                released.recv().unwrap();
            })
            .unwrap();
        assert_eq!(started.recv().unwrap().policy, libc::SCHED_OTHER);
        let started = Instant::now();
        let mut scans = 0;
        // This closure replaces the whole direct and broker request path.
        // Reaching it would permit a syscall that changes scheduling policy.
        let outcome = request_after_data_loops(
            &test_deadline(),
            Duration::from_millis(40),
            || {
                scans += 1;
                data_loop_schedules()
            },
            |_, _| panic!("timeshare data loop reached the policy change path"),
        );
        release.send(()).unwrap();
        data_loop.join().unwrap();
        assert_eq!(
            outcome,
            RtOutcome::Unavailable("data loop not realtime".into())
        );
        assert_eq!(
            contract(&outcome),
            "worker: realtime priority not available (data loop not realtime)"
        );
        assert!(scans >= 2);
        assert!(started.elapsed() >= Duration::from_millis(40));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn data_loop_wait_obeys_cancellation_and_disable() {
        let _g = ENV.lock().unwrap();
        std::env::remove_var("HUSHMIC_WORKER_RT");
        let deadline = test_deadline();
        let outcome = request_after_data_loops(
            &deadline,
            DATA_LOOP_WINDOW,
            || {
                deadline.cancelled.store(true, Ordering::Release);
                vec![None]
            },
            |_, _| panic!("cancelled wait requested scheduling"),
        );
        assert_eq!(outcome, RtOutcome::Unavailable("cancelled".into()));
        std::env::set_var("HUSHMIC_WORKER_RT", "0");
        let outcome = request_after_data_loops(
            &test_deadline(),
            DATA_LOOP_WINDOW,
            || panic!("disabled helper scanned procfs"),
            |_, _| panic!("disabled helper requested scheduling"),
        );
        std::env::remove_var("HUSHMIC_WORKER_RT");
        assert_eq!(outcome, RtOutcome::Unavailable("disabled".into()));
    }

    #[test]
    fn disabled_env_short_circuits() {
        let _g = ENV.lock().unwrap();
        std::env::set_var("HUSHMIC_WORKER_RT", "0");
        let o = request_realtime(my_tid());
        std::env::remove_var("HUSHMIC_WORKER_RT");
        assert_eq!(o, RtOutcome::Unavailable("disabled".into()));
        assert_eq!(
            contract(&o),
            "worker: realtime priority not available (disabled)"
        );
    }

    #[test]
    fn outcome_shape_on_this_machine() {
        let _g = ENV.lock().unwrap();
        std::env::remove_var("HUSHMIC_WORKER_RT");
        let tid = my_tid();
        let o = request_realtime(tid);
        match &o {
            RtOutcome::Granted(p, detail) => {
                assert!(*p >= 1, "{o:?}");
                assert_eq!(
                    contract(&o),
                    format!("worker: realtime priority {p} ({detail})")
                );
                // Put the test thread back.
                let param = libc::sched_param { sched_priority: 0 };
                // SAFETY: plain syscall.
                unsafe { libc::sched_setscheduler(tid, libc::SCHED_OTHER, &param) };
            }
            RtOutcome::Unavailable(why) => {
                let known = ["EPERM", "no dbus", "portal:", "rtkit:", "direct:", "errno"];
                assert!(known.iter().any(|k| why.starts_with(k)), "reason: {why}");
                assert!(contract(&o).starts_with("worker: realtime priority not available ("));
            }
        }
    }

    #[test]
    fn deadline_covers_a_stalled_bus_registration() {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixListener};
        let _g = ENV.lock().unwrap();
        if DBus::load().is_none() {
            return;
        }
        let name = format!("hushmic_deadline_test_{}", std::process::id());
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = UnixListener::bind_addr(&addr).unwrap();
        let (release, released) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (_connection, _) = listener.accept().unwrap();
            let _ = released.recv_timeout(Duration::from_secs(3));
        });
        let previous = std::env::var_os("DBUS_SYSTEM_BUS_ADDRESS");
        std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", format!("unix:abstract={name}"));
        let pending = Arc::new(AtomicBool::new(true));
        let started = Instant::now();
        let outcome = request_bounded(
            Arc::new(AtomicBool::new(false)),
            Arc::clone(&pending),
            Duration::from_millis(100),
            |deadline| {
                let dbus = DBus::load().unwrap();
                match dbus.make_realtime(&RTKIT, my_tid(), deadline, RT_PRIORITY) {
                    Ok(()) => panic!("silent bus unexpectedly granted realtime"),
                    Err(reason) => RtOutcome::Unavailable(reason),
                }
            },
        );
        let elapsed = started.elapsed();
        let was_pending = pending.load(Ordering::Acquire);
        let _ = release.send(());
        server.join().unwrap();
        let done = Instant::now() + Duration::from_secs(2);
        while pending.load(Ordering::Acquire) && Instant::now() < done {
            std::thread::sleep(Duration::from_millis(1));
        }
        if let Some(value) = previous {
            std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", value);
        } else {
            std::env::remove_var("DBUS_SYSTEM_BUS_ADDRESS");
        }
        assert_eq!(outcome, RtOutcome::Unavailable("timeout".into()));
        assert!(
            elapsed < Duration::from_secs(1),
            "deadline took {elapsed:?}"
        );
        assert!(
            was_pending,
            "target must stay alive while libdbus is blocked"
        );
        assert!(!pending.load(Ordering::Acquire));
    }

    #[test]
    fn cancelled_request_returns_before_the_blocked_call_and_rejects_late_grant() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(AtomicBool::new(true));
        let (entered, entry) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let cancel = Arc::clone(&cancelled);
        let lease = Arc::clone(&pending);
        let waiter = std::thread::spawn(move || {
            request_bounded(cancel, lease, Duration::from_secs(2), move |deadline| {
                entered.send(()).unwrap();
                released.recv().unwrap();
                assert_eq!(deadline.check(), Err("cancelled".into()));
                RtOutcome::Unavailable("cancelled".into())
            })
        });
        entry.recv_timeout(Duration::from_secs(1)).unwrap();
        cancelled.store(true, Ordering::Release);
        assert_eq!(
            waiter.join().unwrap(),
            RtOutcome::Unavailable("cancelled".into())
        );
        assert!(pending.load(Ordering::Acquire));
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while pending.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn an_unresolved_remote_grant_keeps_the_target_tid_reserved() {
        let pending = Arc::new(AtomicBool::new(true));
        let outcome = request_bounded(
            Arc::new(AtomicBool::new(false)),
            Arc::clone(&pending),
            Duration::from_secs(1),
            |deadline| {
                deadline.grant_in_flight.store(true, Ordering::Release);
                RtOutcome::Unavailable("NoReply".into())
            },
        );
        assert_eq!(outcome, RtOutcome::Unavailable("NoReply".into()));
        assert!(pending.load(Ordering::Acquire));
    }

    #[test]
    fn errno_names() {
        assert_eq!(errno_name(libc::EPERM), "EPERM");
        assert_eq!(errno_name(libc::ESRCH), "ESRCH");
        assert_eq!(errno_name(9999), "errno 9999");
    }
}
