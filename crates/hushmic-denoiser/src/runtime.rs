//! ONNX Runtime environment lifecycle (dynamic loading builds only).
//!
//! ort's environment commit is process-global and first-wins. Two rules keep
//! that livable for embedders:
//! 1. Failed attempts never latch — only a successful commit is permanent,
//!    so an early implicit failure cannot poison a later explicit
//!    `init_runtime` with a valid path.
//! 2. `init_runtime` reports `AlreadyInitialized` when some runtime was
//!    already committed — possibly not the caller's — so bundling apps can
//!    detect losing the race instead of silently running a foreign runtime.

/// Static-linking builds have no environment to resolve: ort brings its own.
#[cfg(not(feature = "load-dynamic"))]
pub(crate) fn ensure_runtime() -> Result<(), crate::error::Error> {
    Ok(())
}

#[cfg(feature = "load-dynamic")]
pub(crate) use dynamic::ensure_runtime;
#[cfg(feature = "load-dynamic")]
pub use dynamic::{init_runtime, RuntimeInit};

#[cfg(feature = "load-dynamic")]
mod dynamic {

    use crate::error::Error;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    /// True once an environment is committed (by us or by other ort users in
    /// the process). Monotonic false -> true, so the lock-free Acquire loads
    /// on the fast paths are sound; all mutation happens under `INIT_LOCK`.
    static COMMITTED: AtomicBool = AtomicBool::new(false);
    static INIT_LOCK: Mutex<()> = Mutex::new(());

    /// Outcome of [`init_runtime`].
    #[non_exhaustive]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RuntimeInit {
        /// This call committed the environment. Caveat: if another ort
        /// consumer in this process already loaded a runtime library without
        /// committing environment options, that library stays the one in use
        /// and `Committed` is still returned — ort provides no way to detect
        /// a loaded-but-uncommitted runtime.
        Committed,
        /// A runtime environment was already committed — possibly a different
        /// library than the path passed here. If your application bundles its own
        /// ONNX Runtime, call `init_runtime` before creating the first
        /// [`Denoiser`](crate::Denoiser) so yours is the one that wins.
        AlreadyInitialized,
    }

    /// Load ONNX Runtime from an explicit shared-library path and commit it as
    /// the process-wide environment. Idempotent after success; a failed call
    /// leaves the process ready for another attempt.
    ///
    /// Known limitation: in a process where another component initialized ort
    /// from a path this crate cannot resolve, a failing path here still yields
    /// `Error::Runtime` even though sessions would work — initialize before
    /// such components do, or point `ORT_DYLIB_PATH` at their library.
    pub fn init_runtime(dylib_path: impl AsRef<Path>) -> Result<RuntimeInit, Error> {
        if COMMITTED.load(Ordering::Acquire) {
            return Ok(RuntimeInit::AlreadyInitialized);
        }
        let _g = INIT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if COMMITTED.load(Ordering::Acquire) {
            return Ok(RuntimeInit::AlreadyInitialized);
        }
        commit_locked(dylib_path.as_ref())
    }

    /// Constructor-path init: no-op once committed, otherwise resolve
    /// `ORT_DYLIB_PATH` (empty counts as unset, as in ort itself), falling
    /// back to the platform soname (dlopen default search, i.e. a
    /// distro-installed ONNX Runtime).
    pub(crate) fn ensure_runtime() -> Result<(), Error> {
        if COMMITTED.load(Ordering::Acquire) {
            return Ok(());
        }
        let _g = INIT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if COMMITTED.load(Ordering::Acquire) {
            return Ok(());
        }
        let soname = if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else if cfg!(any(target_os = "macos", target_os = "ios")) {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };
        let dylib = std::env::var("ORT_DYLIB_PATH")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| soname.to_string());
        commit_locked(Path::new(&dylib)).map(|_| ())
    }

    /// Mirror ort's relative-path resolution exactly, so the pre-flight check
    /// below tests the very file ort will load. `Denoiser::from_file` docs
    /// describe this order: an executable-adjacent copy wins over the dlopen
    /// default search.
    fn resolve_like_ort(path: &Path) -> PathBuf {
        if path.is_absolute() {
            return path.to_path_buf();
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join(path);
                if candidate.exists() {
                    return candidate;
                }
            }
        }
        path.to_path_buf()
    }

    /// Validate the dylib BEFORE ort touches it. In ort 2.0.0-rc.12 a failed
    /// `init_from` deadlocks the calling thread: constructing its error calls
    /// `CreateStatus`, which needs the API whose load just failed, re-entering
    /// the OnceLock this thread is mid-initializing. So `init_from` must only
    /// ever run on a library already known to dlopen and pass the version gate —
    /// this replicates ort's own checks 1:1 (dlopen, `OrtGetApiBase`, minor
    /// version floor) with our own error type. Returns the resolved path plus
    /// the live handle: the caller must keep the handle alive across ort's own
    /// load so the file cannot be unloaded (or swapped on disk) in between,
    /// which would reopen the very failure path this guards against.
    fn preflight(path: &Path) -> Result<(PathBuf, libloading::Library), Error> {
        let resolved = resolve_like_ort(path);
        let lib = unsafe { libloading::Library::new(&resolved) }.map_err(|e| {
            Error::Runtime(format!(
                "ONNX Runtime failed to load from {}: {e}",
                resolved.display()
            ))
        })?;
        let base_getter: libloading::Symbol<unsafe extern "C" fn() -> *const ort::sys::OrtApiBase> =
            unsafe { lib.get(b"OrtGetApiBase") }.map_err(|_| {
                Error::Runtime(format!(
                    "{} is not an ONNX Runtime library (no OrtGetApiBase symbol)",
                    resolved.display()
                ))
            })?;
        let base = unsafe { base_getter() };
        if base.is_null() {
            return Err(Error::Runtime(format!(
                "{}: OrtGetApiBase returned null",
                resolved.display()
            )));
        }
        let version_ptr = unsafe { ((*base).GetVersionString)() };
        if version_ptr.is_null() {
            return Err(Error::Runtime(format!(
                "{}: GetVersionString returned null",
                resolved.display()
            )));
        }
        let version = unsafe { std::ffi::CStr::from_ptr(version_ptr) }
            .to_string_lossy()
            .into_owned();
        let minor = version
            .split('.')
            .nth(1)
            .and_then(|x| x.parse::<u32>().ok())
            .unwrap_or(0);
        if minor < ort::MINOR_VERSION {
            return Err(Error::Runtime(format!(
                "ONNX Runtime at {} is version {version}; need >= 1.{}",
                resolved.display(),
                ort::MINOR_VERSION
            )));
        }
        Ok((resolved, lib))
    }

    /// Caller holds `INIT_LOCK` and has checked `COMMITTED` is false.
    fn commit_locked(dylib: &Path) -> Result<RuntimeInit, Error> {
        // Passing the RESOLVED path collapses ort's own resolution to the
        // trivial absolute-path branch, and the held handle pins the library
        // in memory: for absolute paths every failure path inside ort's
        // loader is dead code here; for the bare-soname fallback (relative,
        // resolved by the dynamic linker itself) the window is narrowed, not
        // closed. (ort's failure path deadlocks; see preflight.)
        let (resolved, _pin) = preflight(dylib)?;
        match ort::init_from(&resolved) {
            Ok(builder) => {
                // commit() returns false when an environment was already
                // installed (e.g. the host app uses ort directly) — the process
                // has a runtime either way.
                let installed = builder.commit();
                COMMITTED.store(true, Ordering::Release);
                Ok(if installed {
                    RuntimeInit::Committed
                } else {
                    RuntimeInit::AlreadyInitialized
                })
            }
            Err(e) => Err(Error::Runtime(format!(
                "ONNX Runtime failed to load from {}: {e}",
                resolved.display()
            ))),
        }
    }
}
