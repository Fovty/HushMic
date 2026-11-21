//! Optional host extension. All D-Bus work happens off the inference and RT threads.
use crate::worker::{contiguous_hop, HopEngine, HopOutput};
use dpdfnet_xdna2::{self as provider, Stream};
use hushmic_denoiser::{Denoiser, Mode, HOP};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    thread,
    time::{Duration, Instant},
};

const CPU: u32 = 0;
const OPENING: u32 = 1;
const NPU: u32 = 2;
const STEREO: u32 = 3;
const SERVICE: u32 = 4;
const UNQUALIFIED: u32 = 5;
const BUSY: u32 = 6;
const LOST: u32 = 7;
const WAITING_FOR_AUDIO: u32 = 8;
const UNKNOWN_POLICY: u8 = u8::MAX;
// Close an unused session before amdxdna's five-second runtime suspend.
const SESSION_IDLE: Duration = Duration::from_secs(2);

enum RemoteStream {
    Spectral(Stream),
    Pcm(provider::pcm::Stream),
}

impl RemoteStream {
    fn reset(&mut self) -> std::io::Result<()> {
        match self {
            Self::Spectral(stream) => stream.reset(),
            Self::Pcm(stream) => stream.reset(),
        }
    }
}

struct Link {
    status: AtomicU32,
    policy: AtomicU8,
    generation: AtomicU64,
    completed: AtomicU64,
    ready: Mutex<Option<(u64, RemoteStream)>>,
    path: PathBuf,
    mono: bool,
    alive: AtomicBool,
}

impl Link {
    fn current(&self, generation: u64) -> bool {
        self.alive.load(Ordering::Acquire) && self.generation.load(Ordering::Acquire) == generation
    }

    fn request(self: &Arc<Self>, requested: Option<u8>) {
        if !self.mono {
            return;
        }
        let generation = {
            let mut ready = self.ready.lock().unwrap();
            let previous = self.policy.load(Ordering::Relaxed);
            let status = self.status.load(Ordering::Relaxed);
            if requested == Some(previous) && matches!(status, OPENING | WAITING_FOR_AUDIO | NPU) {
                return;
            }
            let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
            // A prepared socket can still own the device before the first
            // hop. Release it before this request tries to claim a lease.
            *ready = None;
            self.policy
                .store(requested.unwrap_or(UNKNOWN_POLICY), Ordering::Relaxed);
            self.status.store(
                if requested == Some(provider::CPU) {
                    CPU
                } else {
                    OPENING
                },
                Ordering::Relaxed,
            );
            generation
        };
        if requested == Some(provider::CPU) {
            return;
        }
        // The connector may outlive the LADSPA instance during a bus call.
        if !crate::rt::keep_plugin_loaded() {
            self.publish(generation, SERVICE);
            return;
        }
        let link = Arc::clone(self);
        if thread::Builder::new()
            .name("dpdfnet-npu-connect".into())
            .spawn(move || link.connect(generation))
            .is_err()
        {
            self.publish(generation, SERVICE);
        }
    }

    fn connect(&self, generation: u64) {
        // The inference worker observes policy changes even when no
        // audio is queued. An old active socket can close on that thread.
        for _ in 0..50 {
            if !self.current(generation) {
                return;
            }
            if self.completed.load(Ordering::Acquire) >= generation {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        if !self.current(generation) {
            return;
        }

        let Ok((mode, ready, automatic)) = provider::policy() else {
            self.publish(generation, SERVICE);
            return;
        };
        {
            let _guard = self.ready.lock().unwrap();
            if !self.current(generation) {
                return;
            }
            self.policy.store(mode, Ordering::Relaxed);
        }
        let full_pcm = mode == provider::PREFER_NPU
            && std::env::var_os("HUSHMIC_XDNA2_FULL_PCM").as_deref() == Some("1".as_ref());
        let (status, stream) = if mode == provider::CPU {
            (CPU, None)
        } else if (!ready && !full_pcm) || (mode == provider::AUTO && !automatic) {
            (UNQUALIFIED, None)
        } else {
            self.open(generation, full_pcm)
        };
        let mut pending = self.ready.lock().unwrap();
        if self.current(generation) {
            self.status.store(status, Ordering::Relaxed);
            *pending = stream.map(|stream| (generation, stream));
        }
    }

    fn open(&self, generation: u64, full_pcm: bool) -> (u32, Option<RemoteStream>) {
        let Ok(hash) = provider::file_hash(&self.path) else {
            return (UNQUALIFIED, None);
        };
        // The service drops its lease after the previous worker has exited.
        // Try again briefly if closure and the new request overlap.
        for attempt in 0..8 {
            if !self.current(generation) {
                break;
            }
            let stream = if full_pcm {
                match provider::pcm::Stream::open(&hash) {
                    Ok(stream) => Ok(RemoteStream::Pcm(stream)),
                    Err(error) if provider::session_busy(&error) => Err(error),
                    Err(_) => Stream::open(&hash).map(RemoteStream::Spectral),
                }
            } else {
                Stream::open(&hash).map(RemoteStream::Spectral)
            };
            match stream {
                Ok(stream) => return (WAITING_FOR_AUDIO, Some(stream)),
                Err(error) if provider::session_busy(&error) => {
                    if attempt < 7 {
                        thread::sleep(Duration::from_millis(30));
                    }
                }
                Err(_) => return (UNQUALIFIED, None),
            }
        }
        (BUSY, None)
    }

    fn publish(&self, generation: u64, status: u32) {
        if self.status.load(Ordering::Relaxed) == status {
            return;
        }
        if let Ok(_guard) = self.ready.try_lock() {
            if self.current(generation) {
                self.status.store(status, Ordering::Relaxed);
            }
        }
    }
}

static IDS: AtomicU64 = AtomicU64::new(1);
static LINKS: OnceLock<Mutex<BTreeMap<u64, Weak<Link>>>> = OnceLock::new();
thread_local! { static PENDING: RefCell<Option<Arc<Link>>> = const { RefCell::new(None) }; }

fn links() -> &'static Mutex<BTreeMap<u64, Weak<Link>>> {
    LINKS.get_or_init(Mutex::default)
}

#[no_mangle]
pub extern "C" fn hushmic_dpdfnet_prepare_xdna2_v1(mono: bool) -> u64 {
    let id = IDS.fetch_add(1, Ordering::Relaxed);
    let link = Arc::new(Link {
        status: AtomicU32::new(if mono { CPU } else { STEREO }),
        policy: AtomicU8::new(UNKNOWN_POLICY),
        generation: AtomicU64::new(0),
        completed: AtomicU64::new(0),
        ready: Mutex::new(None),
        path: crate::model_path(),
        mono,
        alive: AtomicBool::new(true),
    });
    let mut links = links().lock().unwrap();
    links.retain(|_, link| link.strong_count() != 0);
    links.insert(id, Arc::downgrade(&link));
    PENDING.with(|pending| *pending.borrow_mut() = Some(link));
    id
}

#[no_mangle]
pub extern "C" fn hushmic_dpdfnet_status_xdna2_v1(id: u64) -> u32 {
    links()
        .lock()
        .unwrap()
        .get(&id)
        .and_then(Weak::upgrade)
        .map_or(CPU, |link| link.status.load(Ordering::Relaxed))
}

#[no_mangle]
pub extern "C" fn hushmic_dpdfnet_policy_xdna2_v1() -> i32 {
    provider::policy().map_or(-1, |(mode, ready, automatic)| {
        i32::from(mode) | (i32::from(ready) << 8) | (i32::from(automatic) << 9)
    })
}

#[no_mangle]
pub extern "C" fn hushmic_dpdfnet_set_policy_xdna2_v1(mode: u8) -> i32 {
    if provider::set_policy(mode).is_err() {
        return -1;
    }
    let current: Vec<_> = links()
        .lock()
        .unwrap()
        .values()
        .filter_map(Weak::upgrade)
        .collect();
    for link in current {
        link.request(Some(mode));
    }
    0
}

pub fn clear_pending() {
    PENDING.with(|pending| {
        pending.borrow_mut().take();
    });
}

pub struct Engine {
    cpu: Denoiser,
    link: Option<Arc<Link>>,
    stream: Option<RemoteStream>,
    previous_pcm: bool,
    generation: u64,
    idle_deadline: Option<Instant>,
    resume_on_audio: bool,
}

impl Engine {
    pub fn main(cpu: Denoiser) -> Self {
        let link = PENDING.with(|pending| pending.borrow_mut().take());
        if let Some(link) = &link {
            link.request(None);
        }
        Self {
            cpu,
            link,
            stream: None,
            previous_pcm: false,
            generation: 0,
            idle_deadline: None,
            resume_on_audio: false,
        }
    }

    pub fn secondary(cpu: Denoiser) -> Self {
        Self {
            cpu,
            link: None,
            stream: None,
            previous_pcm: false,
            generation: 0,
            idle_deadline: None,
            resume_on_audio: false,
        }
    }

    fn refresh(&mut self) {
        let Some(link) = &self.link else {
            return;
        };
        if let Ok(mut ready) = link.ready.try_lock() {
            let generation = link.generation.load(Ordering::Acquire);
            if self.generation != generation {
                self.stream = None;
                self.generation = generation;
                self.idle_deadline = None;
                self.resume_on_audio = false;
                link.completed.store(generation, Ordering::Release);
            }
            if let Some((prepared, new_stream)) = ready.take() {
                if prepared == generation {
                    self.stream = Some(new_stream);
                    self.idle_deadline = Some(Instant::now() + SESSION_IDLE);
                }
            }
        }
    }

    fn retire_idle_session(&mut self, now: Instant) {
        if self.idle_deadline.is_none_or(|deadline| now < deadline) {
            return;
        }
        self.idle_deadline = None;
        self.resume_on_audio = true;
        if self.stream.take().is_some() {
            if let Some(link) = &self.link {
                link.publish(self.generation, WAITING_FOR_AUDIO);
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(link) = &self.link {
            link.alive.store(false, Ordering::Release);
        }
    }
}

impl HopEngine for Engine {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        self.process_hop_parts((input, &[]), &mut [0.0; HOP], output)
    }

    fn process_hop_parts(
        &mut self,
        input: (&[f32], &[f32]),
        scratch: &mut [f32; HOP],
        output: &mut [f32; HOP],
    ) -> Result<(), String> {
        self.process_hop_output(input, scratch, &mut HopOutput::array(output))
    }

    fn process_hop_output(
        &mut self,
        input: (&[f32], &[f32]),
        scratch: &mut [f32; HOP],
        output: &mut HopOutput<'_>,
    ) -> Result<(), String> {
        self.refresh();
        if self.idle_deadline.is_some() || self.resume_on_audio {
            let now = Instant::now();
            self.retire_idle_session(now);
            if let Some(link) = self
                .link
                .as_ref()
                .filter(|link| self.resume_on_audio && link.current(self.generation))
            {
                self.resume_on_audio = false;
                link.request(None);
            }
            if self.idle_deadline.is_some() {
                self.idle_deadline = Some(now + SESSION_IDLE);
            }
        }
        if let Some(RemoteStream::Pcm(stream)) = &mut self.stream {
            let (attenuation, bypass) = self.cpu.external_pcm_controls();
            if let Ok(parts) = output.receive_pcm(stream, input, attenuation, bypass) {
                self.previous_pcm = true;
                self.cpu.finish_external_pcm(parts);
                if let Some(link) = &self.link {
                    link.publish(self.generation, NPU);
                }
                return Ok(());
            }
            self.stream = None;
            if let Some(link) = &self.link {
                link.publish(self.generation, LOST);
            }
        }
        if std::mem::take(&mut self.previous_pcm) {
            self.cpu.reset_signal_history();
        }
        let stream = &mut self.stream;
        let link = &self.link;
        let generation = self.generation;
        let input = contiguous_hop(input, scratch);
        output.render(|audio| {
            self.cpu
                .process_hop_with_spectral_provider(input, audio, |spectrum, enhanced| {
                    if let Some(RemoteStream::Spectral(session)) = stream {
                        let success = session.step(spectrum, enhanced).is_ok();
                        if !success {
                            *stream = None;
                        }
                        if let Some(link) = link {
                            link.publish(generation, if success { NPU } else { LOST });
                        }
                        success
                    } else {
                        false
                    }
                })
                .map_err(|e| e.to_string())
        })
    }

    fn poll_control(&mut self) {
        self.refresh();
        self.retire_idle_session(Instant::now());
    }

    fn reset(&mut self) {
        self.refresh();
        self.retire_idle_session(Instant::now());
        self.cpu.reset();
        self.previous_pcm = false;
        if self
            .stream
            .as_mut()
            .is_some_and(|stream| stream.reset().is_err())
        {
            self.stream = None;
            if let Some(link) = &self.link {
                link.publish(self.generation, LOST);
            }
        }
    }

    fn set_mode(&mut self, mode: Mode) {
        self.cpu.set_mode(mode);
    }

    fn set_attenuation_limit_db(&mut self, db: f32) {
        self.cpu.set_attenuation_limit_db(db);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rechecking_an_unchanged_policy_preserves_pending_or_active_session() {
        let link = Arc::new(Link {
            status: AtomicU32::new(OPENING),
            policy: AtomicU8::new(provider::PREFER_NPU),
            generation: AtomicU64::new(3),
            completed: AtomicU64::new(3),
            ready: Mutex::new(None),
            path: PathBuf::new(),
            mono: true,
            alive: AtomicBool::new(true),
        });
        for status in [OPENING, WAITING_FOR_AUDIO, NPU] {
            link.status.store(status, Ordering::Relaxed);
            link.request(Some(provider::PREFER_NPU));
            assert_eq!(link.generation.load(Ordering::Relaxed), 3);
            assert_eq!(link.status.load(Ordering::Relaxed), status);
        }
        link.request(Some(provider::CPU));
        assert_eq!(link.generation.load(Ordering::Relaxed), 4);
        assert_eq!(link.status.load(Ordering::Relaxed), CPU);
    }
}
