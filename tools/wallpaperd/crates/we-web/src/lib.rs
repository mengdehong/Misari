//! In-process CEF browser host. Browser callbacks run on CEF's UI thread;
//! the caller retains its event loop and GL context.
mod audio;
mod browser;
pub mod frames;
mod loader;
mod renderer;

use anyhow::{Context, Result, bail, ensure};
use cef::*;
use memmap2::{Mmap, MmapOptions};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::net::UnixStream,
    },
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
    if !std::env::args_os().any(|arg| arg.as_encoded_bytes().starts_with(b"--type=")) {
        return None;
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

/// One browser. Creating/dropping a renderer does not initialize/shut down CEF again.
pub struct Renderer {
    id: u64,
    shared: Arc<Shared>,
    frames: File,
    mapping: Mmap,
    reading: Mutex<()>,
}
pub struct Pixels<'a> {
    pub sequence: u64,
    pub size: [u32; 2],
    pub data: &'a [u8],
}
impl Renderer {
    pub fn load(
        root: &Path,
        entry: &Path,
        size: [u32; 2],
        accelerated: bool,
        platform: &str,
        wake: &UnixStream,
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
        let name = if accelerated {
            c"we-web-frame-dmabuf"
        } else {
            c"we-web-frame-mmap"
        };
        let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
        ensure!(
            fd >= 0,
            "creating browser frame: {}",
            io::Error::last_os_error()
        );
        let frames = unsafe { File::from_raw_fd(fd) };
        frames.set_len(16 + size[0] as u64 * size[1] as u64 * 4)?;
        let mapping = unsafe { MmapOptions::new().map(&frames) }?;
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
        Ok(Self {
            id,
            shared,
            frames,
            mapping,
            reading: Mutex::new(()),
        })
    }
    pub fn send(&self, value: Value, edge: bool) -> Result<()> {
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
            controls.queue.back_mut().unwrap().0 = value;
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
    pub fn poll(&self) -> Update {
        std::mem::take(&mut *self.shared.update.lock().unwrap())
    }
    pub fn resize(&mut self, size: [u32; 2]) -> Result<()> {
        ensure!(
            browser::valid_size(size[0] as i32, size[1] as i32),
            "invalid browser dimensions"
        );
        self.frames.set_len(
            self.frames
                .metadata()?
                .len()
                .max(16 + size[0] as u64 * size[1] as u64 * 4),
        )?;
        self.mapping = unsafe { MmapOptions::new().map(&self.frames) }?;
        Ok(())
    }
    pub fn with_pixels<T>(&self, read: impl FnOnce(Pixels<'_>) -> T) -> io::Result<Option<T>> {
        // flock is per file description. Serialize readers so one read guard
        // cannot unlock the producer while another thread still holds a slice.
        let _reading = self.reading.lock().unwrap();
        let Some(_lock) = FrameLock::try_read(&self.frames)? else {
            return Ok(None);
        };
        let sequence = u64::from_ne_bytes(self.mapping[..8].try_into().unwrap());
        let size = [
            u32::from_ne_bytes(self.mapping[8..12].try_into().unwrap()),
            u32::from_ne_bytes(self.mapping[12..16].try_into().unwrap()),
        ];
        if sequence == 0 {
            return Ok(None);
        }
        let bytes = size[0] as usize * size[1] as usize * 4;
        let data = self
            .mapping
            .get(16..16 + bytes)
            .ok_or_else(|| io::Error::other("invalid browser frame size"))?;
        Ok(Some(read(Pixels {
            sequence,
            size,
            data,
        })))
    }
}
impl Drop for Renderer {
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
        let path = std::env::var_os("WALLPAPERD_WEB_PROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("we-web-{}-{nonce}", std::process::id()))
            });
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
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
