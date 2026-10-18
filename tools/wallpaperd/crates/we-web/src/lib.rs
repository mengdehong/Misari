//! In-process CEF browser host. Browser callbacks run on CEF's UI thread;
//! the caller retains its event loop and GL context.
mod audio;
mod browser;
mod client;
pub mod frames;
mod host;
mod loader;
mod renderer;
mod wire;

pub use client::{Renderer, cleanup_pending};

use anyhow::{Context, Result, bail, ensure};
use cef::*;
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{self, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

const MAX_LINE: usize = 64 * 1024;
const MAX_PIXELS: u64 = 512 * 1024 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static INITIALIZER: OnceLock<std::thread::ThreadId> = OnceLock::new();
static SHUT_DOWN: AtomicBool = AtomicBool::new(false);
type Browsers = Arc<(Mutex<usize>, Condvar)>;
thread_local! {
    static RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) };
}

/// Call before the application's argument parser. Chromium reuses this executable.
pub fn execute_process() -> Option<i32> {
    let cef_child = std::env::args_os().any(|arg| arg.as_encoded_bytes().starts_with(b"--type="));
    if !cef_child {
        if let Some((fd, group)) = host::guardian_argument() {
            return Some(host::guard(fd, group).map_or_else(
                |error| {
                    eprintln!("we-web guardian: {error:#}");
                    1
                },
                |()| 0,
            ));
        }
        return host::argument().map(|fd| {
            host::run(fd).map_or_else(
                |error| {
                    eprintln!("we-web host: {error:#}");
                    1
                },
                |()| 0,
            )
        });
    }
    if let Err(error) = loader::ensure_loaded() {
        eprintln!("we-web: {error}");
        return Some(1);
    }
    let args = cef::args::Args::new();
    let mut app = renderer::WebApp::new(None);
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let code = cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    Some(code)
}

/// Keep this guard on the initialization thread, outside the lifetime of all renderers.
/// No CEF runtime is created until the first renderer is loaded.
pub struct ShutdownGuard;
impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        RUNTIME.with(|slot| drop(slot.borrow_mut().take()));
    }
}

pub fn runtime_directory() -> Option<PathBuf> {
    loader::directory()
}

struct Runtime {
    app: Option<App>,
    browsers: Browsers,
    _profile: Profile,
}
impl Runtime {
    fn ensure(platform: &str) -> Result<Browsers> {
        ensure!(
            !SHUT_DOWN.load(Ordering::Acquire),
            "CEF has already shut down"
        );
        let thread = std::thread::current().id();
        ensure!(
            *INITIALIZER.get_or_init(|| thread) == thread,
            "CEF browser creation must use its initialization thread"
        );
        RUNTIME.with(|slot| {
            let mut runtime = slot.borrow_mut();
            if runtime.is_none() {
                loader::ensure_loaded().map_err(anyhow::Error::msg)?;
                let directory = runtime_directory().context("locating the loaded CEF runtime")?;
                let profile = Profile::new()?;
                let args = cef::args::Args::new();
                let mut app = renderer::WebApp::new(Some(platform.to_owned()));
                let mut executable = std::env::current_exe()?;
                if let Some(path) = std::env::var_os("WALLPAPERD_WEB_SUBPROCESS") {
                    executable = path.into();
                } else if executable.parent().is_some_and(|p| p.ends_with("deps")) {
                    // Rust test executables have no Chromium dispatch entry point.
                    executable = executable
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join("wallpaperd");
                }
                ensure!(
                    executable.is_file(),
                    "CEF subprocess executable is missing: {}",
                    executable.display()
                );
                let settings = Settings {
                    multi_threaded_message_loop: 1,
                    windowless_rendering_enabled: 1,
                    browser_subprocess_path: executable.to_string_lossy().as_ref().into(),
                    resources_dir_path: directory.to_string_lossy().as_ref().into(),
                    locales_dir_path: directory.join("locales").to_string_lossy().as_ref().into(),
                    root_cache_path: profile.0.to_string_lossy().as_ref().into(),
                    log_file: profile.0.join("cef.log").to_string_lossy().as_ref().into(),
                    log_severity: LogSeverity::WARNING,
                    background_color: 0xff000000,
                    ..Default::default()
                };
                let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
                if initialize(
                    Some(args.as_main_args()),
                    Some(&settings),
                    Some(&mut app),
                    std::ptr::null_mut(),
                ) == 0
                {
                    // CEF requires exiting after failed initialization; retrying
                    // or calling its other APIs in this process is invalid.
                    // The daemon observes worker EOF and restores accepted content.
                    eprintln!("we-web: initializing CEF failed");
                    std::process::exit(1);
                }
                *runtime = Some(Self {
                    app: Some(app),
                    browsers: Arc::new((Mutex::new(0), Condvar::new())),
                    _profile: profile,
                });
            }
            Ok(runtime.as_ref().unwrap().browsers.clone())
        })
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let mut task = browser::CloseAll::new();
        post_task(ThreadId::UI, Some(&mut task));
        drop(task);
        let (count, done) = &*self.browsers;
        let count = count.lock().unwrap();
        let (count, _) = done
            .wait_timeout_while(count, Duration::from_secs(5), |n| *n != 0)
            .unwrap();
        if *count != 0 {
            // Calling CefShutdown with live browsers is invalid. The daemon can
            // restart a failed worker, rather than hanging its release forever.
            eprintln!("we-web: CEF browsers did not close during worker shutdown");
            std::process::abort();
        }
        drop(count);
        drop(self.app.take());
        SHUT_DOWN.store(true, Ordering::Release);
        shutdown();
    }
}

#[derive(Default)]
pub struct Update {
    pub frame: bool,
    pub damage: Option<(u64, [u32; 4])>,
    pub gpu: Option<frames::Frame>,
    pub audio: bool,
    pub error: Option<String>,
}
struct Controls {
    queue: VecDeque<(Value, bool)>,
    scheduled: bool,
}
pub(crate) struct Shared {
    update: Mutex<Update>,
    controls: Mutex<Controls>,
    closing: AtomicBool,
    gpu_seen: AtomicBool,
    wake: Mutex<UnixStream>,
    browsers: Browsers,
}
impl Shared {
    fn notify(&self) {
        let _ = self.wake.lock().unwrap().write(&[1]);
    }
    pub(crate) fn error(&self, message: String) {
        if !self.closing.load(Ordering::Acquire) {
            self.update.lock().unwrap().error.get_or_insert(message);
            self.notify();
        }
    }
    pub(crate) fn closed(&self) {
        self.error("CEF browser closed".into());
        self.closing.store(true, Ordering::Release);
        let (count, done) = &*self.browsers;
        *count.lock().unwrap() -= 1;
        done.notify_all();
    }
}

/// Browser implementation owned by the disposable CEF host process.
pub(crate) struct HostRenderer {
    id: u64,
    shared: Arc<Shared>,
}
pub struct Pixels<'a> {
    pub sequence: u64,
    pub size: [u32; 2],
    pub data: &'a [u8],
}
impl HostRenderer {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn load(
        root: &Path,
        entry: &Path,
        size: [u32; 2],
        frame_rate: u16,
        accelerated: bool,
        platform: &str,
        wake: &UnixStream,
        frames: File,
    ) -> Result<Self> {
        ensure!(
            browser::valid_size(size[0] as i32, size[1] as i32),
            "invalid browser dimensions"
        );
        let root = root.canonicalize().context("resolving web project root")?;
        let entry = entry.canonicalize().context("resolving web entry")?;
        ensure!(
            entry.starts_with(&root),
            "web entry escapes the project directory"
        );
        let url = url::Url::from_file_path(&entry)
            .map_err(|()| anyhow::anyhow!("invalid web entry path"))?;
        let browsers = Runtime::ensure(platform)?;
        // Keep the existing mmap path and its dirty-region copies. The producer
        // opens a separate file description so flock also synchronizes threads.
        let fd = frames.as_raw_fd();
        let producer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/proc/self/fd/{fd}"))?;
        let wake = wake.try_clone()?;
        wake.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            update: Mutex::new(Update::default()),
            controls: Mutex::new(Controls {
                queue: VecDeque::new(),
                scheduled: false,
            }),
            closing: AtomicBool::new(false),
            gpu_seen: AtomicBool::new(false),
            wake: Mutex::new(wake),
            browsers,
        });
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        *shared.browsers.0.lock().unwrap() += 1;
        let mut task = browser::Start::new(
            id,
            Arc::new(producer),
            root,
            url.to_string(),
            size,
            frame_rate,
            accelerated,
            shared.clone(),
        );
        if post_task(ThreadId::UI, Some(&mut task)) == 0 {
            shared.closed();
            bail!("scheduling CEF browser creation failed");
        }
        if accelerated {
            let mut timeout = browser::GpuTimeout::new(Arc::downgrade(&shared));
            post_delayed_task(ThreadId::UI, Some(&mut timeout), 8_000);
        }
        Ok(Self { id, shared })
    }
    pub(crate) fn send(&self, value: Value, edge: bool) -> Result<()> {
        ensure!(
            !SHUT_DOWN.load(Ordering::Acquire),
            "CEF has already shut down"
        );
        ensure!(
            value.is_object() && serde_json::to_vec(&value)?.len() <= MAX_LINE,
            "invalid or oversized CEF control message"
        );
        ensure!(
            !self.shared.closing.load(Ordering::Acquire),
            "CEF browser is closing"
        );
        let mut controls = self.shared.controls.lock().unwrap();
        if !edge && controls.queue.back().is_some_and(|(_, edge)| !edge) {
            let queued = &mut controls.queue.back_mut().unwrap().0;
            let mut merged = queued.clone();
            merged
                .as_object_mut()
                .unwrap()
                .extend(value.as_object().unwrap().clone());
            ensure!(
                serde_json::to_vec(&merged)?.len() <= MAX_LINE,
                "coalesced CEF control message is oversized"
            );
            *queued = merged;
        } else {
            ensure!(controls.queue.len() < 64, "CEF control queue is full");
            controls.queue.push_back((value, edge));
        }
        if !controls.scheduled {
            controls.scheduled = true;
            let mut task = browser::Control::new(self.id, self.shared.clone());
            if post_task(ThreadId::UI, Some(&mut task)) == 0 {
                controls.scheduled = false;
                bail!("scheduling CEF control failed");
            }
        }
        Ok(())
    }
    pub(crate) fn poll(&self) -> Update {
        std::mem::take(&mut *self.shared.update.lock().unwrap())
    }
}
impl Drop for HostRenderer {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        // This releases any GPU slot still in the mailbox before posting Close.
        self.shared.update.lock().unwrap().gpu.take();
        if !SHUT_DOWN.load(Ordering::Acquire) {
            let mut task = browser::Control::new(self.id, self.shared.clone());
            post_task(ThreadId::UI, Some(&mut task));
        }
    }
}

struct Profile(PathBuf);
impl Profile {
    fn new() -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let configured = std::env::var_os("WALLPAPERD_WEB_PROFILE").map(PathBuf::from);
        let path = configured.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("we-web-{}-{nonce}", std::process::id()))
        });
        std::fs::DirBuilder::new()
            .recursive(configured.is_some())
            .mode(0o700)
            .create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct FrameLock<'a>(&'a File);
impl<'a> FrameLock<'a> {
    fn try_read(file: &'a File) -> io::Result<Option<Self>> {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
            return Ok(Some(Self(file)));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error)
        }
    }
}
impl Drop for FrameLock<'_> {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
