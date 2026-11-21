use super::{deadline, file_hash, valid, Channel, Lease, Result, FIRST, WORDS};
use dpdfnet_xdna2::{pcm, FRAME, RESET};
use serde::Deserialize;
use std::{
    env,
    ffi::{c_char, c_void, CString, OsString},
    fs,
    io::{self, BufRead, BufReader},
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::{ffi::OsStringExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

const QUARANTINED: &[u8] =
    b"R296_QUARANTINED_ENTIRE_MATCHED_BF16_OWNER_WITH_PERSISTENT_BUFFERS_HELD";

#[derive(Deserialize)]
pub(super) struct Profile {
    library: PathBuf,
    library_sha256: String,
    manifest: PathBuf,
    manifest_sha256: String,
    model: PathBuf,
    pub(super) model_sha256: String,
    #[serde(default)]
    bind_model_in_worker: bool,
    #[serde(default)]
    compact_close: bool,
}

fn receipt_directory() -> Result<PathBuf> {
    let parent =
        env::var_os("HUSHMIC_XDNA2_PCM_RECEIPTS").ok_or("Missing private PCM receipt directory")?;
    let root = fs::canonicalize(parent)?;
    let mut pattern = CString::new(root.join("pcm-owner-XXXXXX").as_os_str().as_encoded_bytes())?
        .into_bytes_with_nul();
    // mkdtemp creates one 0700 directory and writes the final name in place.
    if unsafe { libc::mkdtemp(pattern.as_mut_ptr().cast()) }.is_null() {
        return Err(io::Error::last_os_error().into());
    }
    pattern.pop();
    Ok(PathBuf::from(OsString::from_vec(pattern)))
}

pub(super) fn start(p: &Profile, lease: Lease) -> Result<OwnedFd> {
    for (path, hash) in [
        (&p.library, &p.library_sha256),
        (&p.manifest, &p.manifest_sha256),
    ] {
        if !path.is_absolute() || file_hash(path)? != *hash {
            return Err("PCM artifact identity differs".into());
        }
    }
    if !p.model.is_absolute()
        || p.model_sha256.len() != 64
        || !p
            .model_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("PCM model identity is invalid".into());
    }
    if !p.bind_model_in_worker && file_hash(&p.model)? != p.model_sha256 {
        return Err("PCM model identity differs".into());
    }
    let (client, audio) = Channel::pair()?;
    let (observer, notifications) = Channel::pair()?;
    let parent = std::process::id() as libc::pid_t;
    let mut command = Command::new(env::current_exe()?);
    command
        .arg("--pcm-worker")
        .arg(&p.library)
        .arg(&p.manifest)
        .arg(&p.model);
    if p.bind_model_in_worker {
        command.arg(&p.model_sha256);
    }
    command
        .env("DPDF_R103_OUTPUT_DIRECTORY", receipt_directory()?)
        .env(
            "DPDF_PCM_CLOSE_WITHOUT_STATE",
            if p.compact_close { "1" } else { "0" },
        )
        .stdin(Stdio::from(audio.0))
        .stdout(Stdio::from(notifications.0))
        .stderr(Stdio::piped());
    // The child belongs to this supervisor for the life of its device context.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 || libc::getppid() != parent
            {
                return Err(io::Error::from_raw_os_error(libc::ECHILD));
            }
            Ok(())
        });
    }
    let (report, startup) = mpsc::channel();
    thread::Builder::new()
        .name("dpdfnet-xdna2-pcm".into())
        .spawn(move || {
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    let _ = report.send(Err(error));
                    return;
                }
            };
            drop(command);
            let readiness = observer
                .recv(&mut [0.0; WORDS], deadline(900))
                .and_then(|stamp| {
                    if stamp == FIRST {
                        Ok(())
                    } else {
                        Err(io::ErrorKind::InvalidData.into())
                    }
                });
            let _ = report.send(readiness);
            drop(observer);
            supervise(&mut child, lease);
        })?;
    startup.recv_timeout(Duration::from_secs(1))??;
    Ok(client.0)
}

fn supervise(child: &mut Child, lease: Lease) {
    if let Some(stderr) = child.stderr.take() {
        for record in BufReader::new(stderr).split(b'\n') {
            if record.is_ok_and(|line| line == QUARANTINED) {
                // This pinned facade has explicitly suspended itself with
                // acceptance unknown. Reap only the worker we just started.
                if let Err(error) = child.kill() {
                    eprintln!(
                        "Could not stop quarantined PCM worker {}: {error}",
                        child.id()
                    );
                }
                break;
            }
        }
    }
    if let Err(error) = child.wait() {
        eprintln!("Could not reap PCM worker {}: {error}", child.id());
        std::mem::forget(lease);
    }
}

type Fill = unsafe extern "C" fn(*mut c_void, *mut f32, usize, *mut f32, *mut f32) -> i32;
type Publish = unsafe extern "C" fn(*mut c_void, *const f32, usize) -> i32;
type Exchange = unsafe extern "C" fn(*mut c_void, Fill, Publish, *mut c_void) -> i32;
type Reset = unsafe extern "C" fn(*mut c_void) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void) -> i32;

struct Wire {
    audio: Channel,
    epoch: u32,
    number: u64,
    packet: Option<pcm::Packet>,
    error: Option<io::Error>,
}

unsafe extern "C" fn fill(
    opaque: *mut c_void,
    dest: *mut f32,
    count: usize,
    attenuation: *mut f32,
    bypass: *mut f32,
) -> i32 {
    if count != pcm::HOP {
        return -1;
    }
    // The native ABI lends one writable slot until this callback returns.
    let (wire, samples) = unsafe {
        (
            &mut *opaque.cast::<Wire>(),
            std::slice::from_raw_parts_mut(dest, count),
        )
    };
    match pcm::receive(&wire.audio, samples, deadline(300_000)) {
        Ok(packet) if valid(packet.stamp, wire.epoch, wire.number) => {
            wire.packet = Some(packet);
            if packet.stamp.kind == RESET {
                return -1;
            }
            unsafe {
                *attenuation = packet.attenuation;
                *bypass = packet.bypass;
            }
            0
        }
        Ok(_) => {
            wire.error = Some(io::ErrorKind::InvalidData.into());
            -1
        }
        Err(error) => {
            wire.error = Some(error);
            -1
        }
    }
}

unsafe extern "C" fn publish(opaque: *mut c_void, data: *const f32, count: usize) -> i32 {
    if count != pcm::HOP {
        return -1;
    }
    // The native ABI supplies a validated, read-only result for this callback.
    let (wire, samples) = unsafe {
        (
            &mut *opaque.cast::<Wire>(),
            std::slice::from_raw_parts(data, count),
        )
    };
    let Some(packet) = wire.packet else {
        return -1;
    };
    match pcm::send(&wire.audio, packet, samples, deadline(10)) {
        Ok(()) => 0,
        Err(error) => {
            wire.error = Some(error);
            -1
        }
    }
}

fn work(mut wire: Wire, session: *mut c_void, exchange: Exchange, reset: Reset) -> Result<()> {
    loop {
        wire.packet = None;
        wire.error = None;
        let result = unsafe { exchange(session, fill, publish, (&mut wire as *mut Wire).cast()) };
        if let Some(error) = wire.error.take() {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                return Ok(());
            }
            return Err(error.into());
        }
        match (result, wire.packet) {
            (0, Some(packet)) if packet.stamp.kind == FRAME => wire.number += 1,
            (-2, Some(packet)) if packet.stamp.kind == RESET => {
                if unsafe { reset(session) } != 0 {
                    return Err("Could not reset full PCM session".into());
                }
                wire.epoch = packet.stamp.epoch;
                pcm::send(&wire.audio, packet, &[], deadline(10))?;
            }
            _ => return Err("Full PCM exchange failed".into()),
        }
    }
}

pub(super) fn worker(
    library: &Path,
    manifest: &Path,
    model: &Path,
    expected: Option<&str>,
) -> Result<()> {
    type Abi = unsafe extern "C" fn() -> u32;
    type Open = unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void;
    let manifest = CString::new(manifest.as_os_str().as_encoded_bytes())?;
    let model = CString::new(model.as_os_str().as_encoded_bytes())?;
    // The broker supplies the data and startup sockets as two distinct inherited FDs.
    let audio = Channel(unsafe { OwnedFd::from_raw_fd(libc::STDIN_FILENO) });
    let notifications = Channel(unsafe { OwnedFd::from_raw_fd(libc::STDOUT_FILENO) });
    let loaded = unsafe { libloading::Library::new(library)? };
    let destroy_symbol: &[u8] =
        if env::var_os("DPDF_PCM_CLOSE_WITHOUT_STATE").as_deref() == Some("1".as_ref()) {
            b"r103_destroy_quiet_v1"
        } else {
            b"r103_destroy_v1"
        };
    let (abi, exchange, reset, destroy) = unsafe {
        (
            *loaded.get::<Abi>(b"r103_abi_version_v1")?,
            *loaded.get::<Exchange>(b"r103_pcm_exchange_v1")?,
            *loaded.get::<Reset>(b"audio_native_reset")?,
            *loaded.get::<Destroy>(destroy_symbol)?,
        )
    };
    if unsafe { abi() } != 1 {
        return Err("Unsupported full PCM native ABI".into());
    }
    let session = unsafe {
        if let Some(expected) = expected {
            type OpenBound =
                unsafe extern "C" fn(*const c_char, *const c_char, *const c_char) -> *mut c_void;
            let hash = CString::new(expected)?;
            let open = loaded.get::<OpenBound>(b"r103_create_bound_v1")?;
            open(manifest.as_ptr(), model.as_ptr(), hash.as_ptr())
        } else {
            let open = loaded.get::<Open>(b"audio_native_create")?;
            open(manifest.as_ptr(), model.as_ptr())
        }
    };
    if session.is_null() {
        return Err("Could not open full PCM backend".into());
    }
    let ready = notifications.send(FIRST, &[], deadline(500));
    drop(notifications);
    let result = ready.map_err(Into::into).and_then(|_| {
        work(
            Wire {
                audio,
                epoch: 0,
                number: 0,
                packet: None,
                error: None,
            },
            session,
            exchange,
            reset,
        )
    });
    let closed = unsafe { destroy(session) };
    if closed != 0 {
        return Err("Could not close full PCM session".into());
    }
    result
}
