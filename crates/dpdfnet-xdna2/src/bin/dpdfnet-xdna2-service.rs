use dpdfnet_xdna2::{
    deadline, file_hash, Channel, Stamp, AUTO, CPU, FRAME, OBJECT, PREFER_NPU, READY, RESET,
    SERVICE, WORDS,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    env,
    error::Error,
    ffi::{c_char, c_void, CString},
    fs::{self, OpenOptions},
    io::{self, Write},
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::Duration,
};
use zbus::{fdo, zvariant::OwnedFd as BusFd};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const FIRST: Stamp = Stamp {
    kind: READY,
    epoch: 0,
    number: 0,
};

#[derive(Clone, Deserialize)]
struct Profile {
    library: PathBuf,
    library_sha256: String,
    manifest: PathBuf,
    manifest_sha256: String,
    model_sha256: String,
    qualified_for_live_audio: bool,
    auto_qualified: bool,
}

#[derive(Deserialize)]
struct Providers {
    version: u32,
    profiles: BTreeMap<String, Profile>,
}

fn local_path(variable: &str, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(fallback)
}

fn read_policy(path: &Path) -> u8 {
    match fs::read_to_string(path).ok().as_deref().map(str::trim) {
        Some("cpu") => CPU,
        Some("prefer-npu") => PREFER_NPU,
        _ => AUTO,
    }
}

fn save_policy(path: &Path, policy: u8) -> io::Result<()> {
    let value = match policy {
        CPU => "cpu",
        AUTO => "auto",
        PREFER_NPU => "prefer-npu",
        _ => return Err(io::ErrorKind::InvalidInput.into()),
    };
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".policy-{}.tmp", std::process::id()));
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = writeln!(out, "{value}")
        .and_then(|_| out.sync_all())
        .and_then(|_| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

struct Broker {
    settings: PathBuf,
    policy: Mutex<u8>,
    profiles: BTreeMap<String, Profile>,
    lease: Arc<Mutex<bool>>,
    development: bool,
}

#[zbus::interface(name = "io.github.fovty.HushMic.XDNA2.V1")]
impl Broker {
    fn get_policy(&self) -> (u8, bool, bool) {
        let policy = *self.policy.lock().unwrap();
        let device = fs::read_dir("/dev/accel").ok().is_some_and(|entries| {
            use std::os::unix::fs::FileTypeExt;
            entries.filter_map(std::result::Result::ok).any(|entry| {
                entry.file_type().is_ok_and(|ty| ty.is_char_device())
                    && CString::new(entry.path().as_os_str().as_encoded_bytes()).is_ok_and(
                        |path| unsafe { libc::access(path.as_ptr(), libc::R_OK | libc::W_OK) == 0 },
                    )
            })
        });
        let profiles = self.profiles.values();
        let ready = device
            && profiles
                .clone()
                .any(|p| p.qualified_for_live_audio || self.development);
        let automatic = device
            && profiles
                .clone()
                .any(|p| p.qualified_for_live_audio && p.auto_qualified);
        (policy, ready, automatic)
    }

    fn set_policy(&self, value: u8) -> fdo::Result<()> {
        let mut policy = self.policy.lock().unwrap();
        save_policy(&self.settings, value).map_err(|e| fdo::Error::Failed(e.to_string()))?;
        *policy = value;
        Ok(())
    }

    fn open_session(&self, model_hash: &str) -> fdo::Result<BusFd> {
        let policy = *self.policy.lock().unwrap();
        let Some(profile) = self
            .profiles
            .values()
            .find(|p| p.model_sha256 == model_hash)
        else {
            return Err(fdo::Error::NotSupported("Unqualified model".into()));
        };
        let allowed = match policy {
            AUTO => profile.qualified_for_live_audio && profile.auto_qualified,
            PREFER_NPU => profile.qualified_for_live_audio || self.development,
            _ => false,
        };
        if !allowed {
            return Err(fdo::Error::NotSupported(
                "NPU profile not enabled for this policy".into(),
            ));
        }
        let lease = Lease::acquire(self.lease.clone())
            .ok_or_else(|| fdo::Error::LimitsExceeded("NPU is busy".into()))?;
        let result = start(profile, lease);
        result
            .map(BusFd::from)
            .map_err(|e| fdo::Error::Failed(e.to_string()))
    }
}

struct Lease(Arc<Mutex<bool>>);

impl Lease {
    fn acquire(active: Arc<Mutex<bool>>) -> Option<Self> {
        let mut flag = active.lock().unwrap();
        if *flag {
            return None;
        }
        *flag = true;
        drop(flag);
        Some(Self(active))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = false;
    }
}

fn start(profile: &Profile, lease: Lease) -> Result<OwnedFd> {
    for (path, hash) in [
        (&profile.library, &profile.library_sha256),
        (&profile.manifest, &profile.manifest_sha256),
    ] {
        if !path.is_absolute() || file_hash(path)? != *hash {
            return Err("NPU artifact identity changed".into());
        }
    }
    let (client, relay) = Channel::pair()?;
    let (control, worker) = Channel::pair()?;
    let parent = std::process::id() as libc::pid_t;
    let mut command = Command::new(env::current_exe()?);
    command
        .arg("--worker")
        .arg(&profile.library)
        .arg(&profile.manifest)
        .stdin(Stdio::from(worker.0))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // A service crash must also end an abandoned XRT owner.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 || libc::getppid() != parent
            {
                return Err(io::Error::from_raw_os_error(libc::ECHILD));
            }
            Ok(())
        });
    }
    let (started, startup) = mpsc::channel();
    thread::Builder::new().name("dpdfnet-xdna2-relay".into()).spawn(move || {
        let lease = lease;
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = started.send(Err(error));
                return;
            }
        };
        drop(command);
        let ready = control.recv(&mut [0.0; WORDS], deadline(900)).and_then(|stamp| {
            if stamp == FIRST {
                Ok(())
            } else {
                Err(io::Error::new(io::ErrorKind::InvalidData, "Missing NPU worker readiness"))
            }
        });
        let is_ready = ready.is_ok();
        if started.send(ready).is_ok() && is_ready {
            let _ = bridge(&relay, &control);
        }
        drop(relay);
        drop(control);
        // The audio deadline has already expired. The worker can still be
        // inside an XRT call. Keep the lease until it exits, without sending
        // a signal into a driver that may be waking or releasing the device.
        if let Err(error) = child.wait() {
            eprintln!("Could not reap NPU worker {}: {error}", child.id());
            // A wait failure does not prove the device has been released.
            std::mem::forget(lease);
        }
    })?;
    startup.recv_timeout(Duration::from_secs(1))??;
    Ok(client.0)
}

fn valid(stamp: Stamp, epoch: u32, number: u64) -> bool {
    stamp.number == number
        && match stamp.kind {
            FRAME => stamp.epoch == epoch && number < u64::MAX,
            RESET => epoch.checked_add(1) == Some(stamp.epoch),
            _ => false,
        }
}

fn bridge(client: &Channel, worker: &Channel) -> io::Result<()> {
    let mut data = [0.0; WORDS];
    let (mut epoch, mut number) = (0, 0);
    loop {
        let stamp = client.recv(&mut data, deadline(300_000))?;
        if !valid(stamp, epoch, number) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let until = deadline(if stamp.kind == FRAME { 24 } else { 25 });
        worker.send(stamp, if stamp.kind == FRAME { &data } else { &[] }, until)?;
        if worker.recv(&mut data, until)? != stamp {
            return Err(io::ErrorKind::InvalidData.into());
        }
        client.send(stamp, if stamp.kind == FRAME { &data } else { &[] }, until)?;
        if stamp.kind == FRAME {
            number += 1;
        } else {
            epoch = stamp.epoch;
        }
    }
}

fn worker(library: &Path, manifest: &Path) -> Result<()> {
    type Counter = unsafe extern "C" fn() -> u32;
    type Open = unsafe extern "C" fn(*const c_char) -> *mut c_void;
    type Step = unsafe extern "C" fn(*mut c_void, *const f32, *mut f32) -> i32;
    type Reset = unsafe extern "C" fn(*mut c_void) -> i32;
    type Close = unsafe extern "C" fn(*mut c_void);
    let manifest = CString::new(manifest.as_os_str().as_encoded_bytes())?;
    let socket = Channel(unsafe { OwnedFd::from_raw_fd(libc::STDIN_FILENO) });
    // The broker pins the library and manifest before it launches this worker.
    let library = unsafe { libloading::Library::new(library)? };
    unsafe {
        let abi = library.get::<Counter>(b"dpdfnet_xdna2_spectral_abi")?;
        let open = library.get::<Open>(b"dpdfnet_xdna2_spectral_open")?;
        let step = library.get::<Step>(b"dpdfnet_xdna2_spectral_step")?;
        let reset = library.get::<Reset>(b"dpdfnet_xdna2_spectral_reset")?;
        let close = library.get::<Close>(b"dpdfnet_xdna2_spectral_close")?;
        if abi() != 1 {
            return Err("Incompatible spectral ABI".into());
        }
        let session = open(manifest.as_ptr());
        if session.is_null() {
            return Err("Could not start spectral session".into());
        }
        let result = (|| -> Result<()> {
            socket.send(FIRST, &[], deadline(500))?;
            let mut input = [0.0; WORDS];
            let mut output = [0.0; WORDS];
            loop {
                let stamp = socket.recv(&mut input, deadline(300_000))?;
                let (result, reply) = match stamp.kind {
                    FRAME => (
                        step(session, input.as_ptr(), output.as_mut_ptr()),
                        output.as_slice(),
                    ),
                    RESET => (reset(session), &[][..]),
                    _ => return Err("Unexpected worker packet".into()),
                };
                if result != 0 {
                    return Err("Spectral inference failed".into());
                }
                socket.send(stamp, reply, deadline(10))?;
            }
        })();
        close(session);
        result
    }
}

fn main() -> Result<()> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() == 3 && args[0] == "--worker" {
        return worker(Path::new(&args[1]), Path::new(&args[2]));
    }
    if !args.is_empty() {
        return Err("Unsupported service arguments".into());
    }
    let catalog = local_path("HUSHMIC_XDNA2_PROVIDERS", || {
        PathBuf::from("/usr/share/dpdfnet-xdna2/providers.json")
    });
    let providers: Providers = serde_json::from_slice(&fs::read(catalog)?)?;
    if providers.version != 1 {
        return Err("Unsupported provider catalog version".into());
    }
    let settings = local_path("XDG_CONFIG_HOME", || {
        PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".config")
    })
    .join("hushmic/xdna2-policy");
    let policy = Mutex::new(read_policy(&settings));
    let broker = Broker {
        settings,
        policy,
        profiles: providers.profiles,
        lease: Arc::new(Mutex::new(false)),
        development: env::var_os("HUSHMIC_XDNA2_DEVELOPMENT").as_deref() == Some("1".as_ref()),
    };
    let connection = zbus::blocking::connection::Builder::session()?
        .name(SERVICE)?
        .serve_at(OBJECT, broker)?
        .build()?;
    for message in zbus::blocking::MessageIterator::from(&connection) {
        if message.is_err() {
            break;
        }
    }
    Ok(())
}
