use crate::{FrameLock, MAX_LINE, Pixels, Update, browser, frames, wire};
use anyhow::{Context, Result, ensure};
use memmap2::{Mmap, MmapOptions};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_HOST: AtomicU64 = AtomicU64::new(1);
static LIVE_HOSTS: AtomicUsize = AtomicUsize::new(0);
thread_local! { static HOST: RefCell<Weak<Host>> = const { RefCell::new(Weak::new()) }; }

struct Browser {
    update: Mutex<Update>,
    wake: Mutex<UnixStream>,
}
struct Shared {
    browsers: Mutex<HashMap<u64, Arc<Browser>>>,
    alive: AtomicBool,
    cleanup_wake: Mutex<UnixStream>,
}
struct Packet {
    value: Value,
    fds: Vec<OwnedFd>,
}
struct Transport {
    queue: Mutex<VecDeque<Packet>>,
    wake: Mutex<UnixStream>,
    socket: OwnedFd,
}
struct Host {
    transport: Arc<Transport>,
    shared: Arc<Shared>,
    group: libc::pid_t,
}
struct Children {
    host: Child,
    guardian: Child,
    signal: Option<OwnedFd>,
    group: libc::pid_t,
    profile: PathBuf,
}
struct LiveHost(Arc<Shared>);
impl Drop for LiveHost {
    fn drop(&mut self) {
        LIVE_HOSTS.fetch_sub(1, Ordering::AcqRel);
        let _ = self.0.cleanup_wake.lock().unwrap().write(&[1]);
    }
}
impl Drop for Children {
    fn drop(&mut self) {
        unsafe { libc::kill(-self.group, libc::SIGKILL) };
        drop(self.signal.take());
        let _ = self.host.wait();
        let _ = self.guardian.wait();
        let _ = std::fs::remove_dir_all(&self.profile);
        if let Some(parent) = self.profile.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

pub struct Renderer {
    id: u64,
    host: Arc<Host>,
    browser: Arc<Browser>,
    frames: File,
    mapping: Mmap,
    reading: Mutex<()>,
    accelerated: bool,
}

impl Renderer {
    pub fn load(
        root: &Path,
        entry: &Path,
        size: [u32; 2],
        frame_rate: u16,
        accelerated: bool,
        platform: &str,
        wake: &UnixStream,
    ) -> Result<Self> {
        ensure!(
            browser::valid_size(size[0] as i32, size[1] as i32),
            "invalid browser dimensions"
        );
        ensure!(
            (1..=240).contains(&frame_rate),
            "invalid browser frame rate"
        );
        let root = root.canonicalize().context("resolving web project root")?;
        let entry = entry.canonicalize().context("resolving web entry")?;
        ensure!(
            entry.starts_with(&root),
            "web entry escapes the project directory"
        );
        let host = host(wake)?;
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
        frames.set_len(if accelerated {
            16
        } else {
            16 + size[0] as u64 * size[1] as u64 * 4
        })?;
        let mapping = unsafe { MmapOptions::new().map(&frames) }?;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let browser_wake = wake.try_clone()?;
        browser_wake.set_nonblocking(true)?;
        let browser = Arc::new(Browser {
            update: Mutex::new(Update::default()),
            wake: Mutex::new(browser_wake),
        });
        host.shared
            .browsers
            .lock()
            .unwrap()
            .insert(id, browser.clone());
        let start = json!({"kind":"start","id":id,"root":root,"entry":entry,"size":size,"frame_rate":frame_rate,
            "accelerated":accelerated,"platform":platform});
        if let Err(error) = host.send(&start, &[frames.as_raw_fd()]) {
            host.shared.browsers.lock().unwrap().remove(&id);
            return Err(error);
        }
        Ok(Self {
            id,
            host,
            browser,
            frames,
            mapping,
            reading: Mutex::new(()),
            accelerated,
        })
    }

    pub fn send(&self, value: Value, edge: bool) -> Result<()> {
        ensure!(
            value.is_object() && serde_json::to_vec(&value)?.len() <= MAX_LINE,
            "invalid or oversized CEF control message"
        );
        self.host.send(
            &json!({"kind":"control","id":self.id,"value":value,"edge":edge}),
            &[],
        )
    }
    pub fn poll(&self) -> Update {
        std::mem::take(&mut *self.browser.update.lock().unwrap())
    }
    pub fn resize(&mut self, size: [u32; 2]) -> Result<()> {
        ensure!(
            browser::valid_size(size[0] as i32, size[1] as i32),
            "invalid browser dimensions"
        );
        if !self.accelerated {
            self.frames.set_len(
                self.frames
                    .metadata()?
                    .len()
                    .max(16 + size[0] as u64 * size[1] as u64 * 4),
            )?;
        }
        self.mapping = unsafe { MmapOptions::new().map(&self.frames) }?;
        Ok(())
    }
    pub fn with_pixels<T>(&self, read: impl FnOnce(Pixels<'_>) -> T) -> io::Result<Option<T>> {
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
        self.host.shared.browsers.lock().unwrap().remove(&self.id);
        let _ = self.host.send(&json!({"kind":"close","id":self.id}), &[]);
    }
}

impl Host {
    fn send(&self, value: &Value, fds: &[i32]) -> Result<()> {
        ensure!(
            self.shared.alive.load(Ordering::Acquire),
            "CEF host is unavailable"
        );
        self.transport.send(value, fds).inspect_err(|_| {
            self.transport.abort();
        })
    }
}
impl Transport {
    fn send(&self, value: &Value, fds: &[i32]) -> Result<()> {
        ensure!(fds.len() <= wire::MAX_FDS, "too many Web host descriptors");
        ensure!(
            serde_json::to_vec(value)?.len() <= wire::MAX_MESSAGE,
            "oversized Web host message"
        );
        let fds = fds
            .iter()
            .map(|fd| unsafe { BorrowedFd::borrow_raw(*fd) }.try_clone_to_owned())
            .collect::<io::Result<Vec<_>>>()?;
        let mut queue = self.queue.lock().unwrap();
        if fds.is_empty()
            && value["kind"] == "control"
            && !value["edge"].as_bool().unwrap_or(false)
            && let Some(last) = queue.back_mut()
            && last.value["kind"] == "control"
            && last.value["id"] == value["id"]
            && !last.value["edge"].as_bool().unwrap_or(false)
        {
            let mut merged = last.value["value"].clone();
            merged
                .as_object_mut()
                .unwrap()
                .extend(value["value"].as_object().unwrap().clone());
            let mut packet = last.value.clone();
            packet["value"] = merged;
            if serde_json::to_vec(&packet)?.len() <= wire::MAX_MESSAGE {
                last.value = packet;
                drop(queue);
                let _ = self.wake.lock().unwrap().write(&[1]);
                return Ok(());
            }
        }
        ensure!(queue.len() < 128, "CEF host command queue is full");
        queue.push_back(Packet {
            value: value.clone(),
            fds,
        });
        drop(queue);
        let _ = self.wake.lock().unwrap().write(&[1]);
        Ok(())
    }
    fn send_ack(&self, value: Value) -> Result<()> {
        let mut queue = self.queue.lock().unwrap();
        ensure!(queue.len() < 128, "CEF host command queue is full");
        queue.push_front(Packet {
            value,
            fds: Vec::new(),
        });
        drop(queue);
        let _ = self.wake.lock().unwrap().write(&[1]);
        Ok(())
    }
    fn abort(&self) {
        let _ = unsafe { libc::shutdown(self.socket.as_raw_fd(), libc::SHUT_RDWR) };
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        unsafe { libc::kill(-self.group, libc::SIGKILL) };
        self.transport.abort();
    }
}

pub fn cleanup_pending() -> bool {
    LIVE_HOSTS.load(Ordering::Acquire) != 0
}

fn host(wake: &UnixStream) -> Result<Arc<Host>> {
    HOST.with(|slot| {
        if let Some(host) = slot
            .borrow()
            .upgrade()
            .filter(|h| h.shared.alive.load(Ordering::Acquire))
        {
            return Ok(host);
        }
        let host = spawn(wake)?;
        *slot.borrow_mut() = Arc::downgrade(&host);
        Ok(host)
    })
}

fn spawn(wake: &UnixStream) -> Result<Arc<Host>> {
    let (parent, child_socket) = wire::pair()?;
    let child_socket = duplicate_fd(child_socket.as_raw_fd())?;
    let receive_fd = unsafe { libc::fcntl(parent.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    ensure!(
        receive_fd >= 0,
        "duplicating Web receive socket: {}",
        io::Error::last_os_error()
    );
    let receive_fd = unsafe { OwnedFd::from_raw_fd(receive_fd) };
    let mut executable = std::env::current_exe()?;
    if let Some(path) = std::env::var_os("WALLPAPERD_WEB_SUBPROCESS") {
        executable = path.into();
    } else if executable.parent().is_some_and(|p| p.ends_with("deps")) {
        executable = executable
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("wallpaperd");
    }
    ensure!(
        executable.is_file(),
        "CEF host executable is missing: {}",
        executable.display()
    );
    let profile = profile_path()?;
    let child_fd = child_socket.as_raw_fd();
    let parent_pid = unsafe { libc::getpid() };
    let mut command = Command::new(&executable);
    command
        .arg("--we-web-host-fd=3")
        .env("WALLPAPERD_WEB_PROFILE", &profile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0);
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid {
                return Err(io::Error::other("Web worker exited during host spawn"));
            }
            if libc::dup2(child_fd, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(3, libc::F_SETFD, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            cleanup_profile(&profile);
            return Err(error).context("starting CEF host");
        }
    };
    let group = child.id() as libc::pid_t;
    drop(child_socket);
    let mut guardian_pipe = [-1; 2];
    if unsafe { libc::pipe2(guardian_pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        unsafe { libc::kill(-group, libc::SIGKILL) };
        let _ = child.wait();
        cleanup_profile(&profile);
        return Err(io::Error::last_os_error().into());
    }
    let guardian_read = unsafe { OwnedFd::from_raw_fd(guardian_pipe[0]) };
    let guardian_write = unsafe { OwnedFd::from_raw_fd(guardian_pipe[1]) };
    let guardian_read = match duplicate_fd(guardian_read.as_raw_fd()) {
        Ok(fd) => fd,
        Err(error) => {
            unsafe { libc::kill(-group, libc::SIGKILL) };
            let _ = child.wait();
            cleanup_profile(&profile);
            return Err(error).context("duplicating Web guardian pipe");
        }
    };
    let guardian_fd = guardian_read.as_raw_fd();
    let mut guardian = Command::new(&executable);
    guardian
        .arg("--we-web-guardian-fd=3")
        .arg(format!("--we-web-host-pgid={group}"))
        .env("WALLPAPERD_WEB_PROFILE", &profile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0);
    unsafe {
        guardian.pre_exec(move || {
            if libc::dup2(guardian_fd, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(3, libc::F_SETFD, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let guardian_child = match guardian.spawn() {
        Ok(child) => child,
        Err(error) => {
            unsafe { libc::kill(-group, libc::SIGKILL) };
            let _ = child.wait();
            cleanup_profile(&profile);
            return Err(error).context("starting Web host guardian");
        }
    };
    drop(guardian_read);
    let children = Children {
        host: child,
        guardian: guardian_child,
        signal: Some(guardian_write),
        group,
        profile: profile.clone(),
    };
    let shared = Arc::new(Shared {
        browsers: Mutex::new(HashMap::new()),
        alive: AtomicBool::new(true),
        cleanup_wake: Mutex::new(wake.try_clone()?),
    });
    let (send_read, send_write) = UnixStream::pair()?;
    send_read.set_nonblocking(true)?;
    send_write.set_nonblocking(true)?;
    let transport = Arc::new(Transport {
        queue: Mutex::new(VecDeque::new()),
        wake: Mutex::new(send_write),
        socket: receive_fd,
    });
    shared.cleanup_wake.lock().unwrap().set_nonblocking(true)?;
    LIVE_HOSTS.fetch_add(1, Ordering::AcqRel);
    let live = LiveHost(shared.clone());
    std::thread::Builder::new()
        .name("we-web-host".into())
        .spawn({
            let shared = shared.clone();
            let transport = transport.clone();
            move || receive(parent, send_read, children, shared, transport, live)
        })?;
    Ok(Arc::new(Host {
        transport,
        shared,
        group,
    }))
}

fn duplicate_fd(fd: RawFd) -> io::Result<OwnedFd> {
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if copy < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(copy) })
    }
}

fn cleanup_profile(profile: &Path) {
    let _ = std::fs::remove_dir_all(profile);
    if let Some(parent) = profile.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

fn profile_path() -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let generation = NEXT_HOST.fetch_add(1, Ordering::Relaxed);
    let path = std::env::var_os("WALLPAPERD_WEB_PROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("host-{}-{generation}", std::process::id()));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)?;
    Ok(path)
}

fn receive(
    socket: OwnedFd,
    mut send_wake: UnixStream,
    children: Children,
    shared: Arc<Shared>,
    transport: Arc<Transport>,
    live: LiveHost,
) {
    let result = (|| -> Result<()> {
        loop {
            let wants_write = !transport.queue.lock().unwrap().is_empty();
            let mut polls = [
                libc::pollfd {
                    fd: socket.as_raw_fd(),
                    events: libc::POLLIN | if wants_write { libc::POLLOUT } else { 0 },
                    revents: 0,
                },
                libc::pollfd {
                    fd: send_wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as _, -1) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if polls[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                anyhow::bail!("CEF host connection closed");
            }
            if polls[1].revents & libc::POLLIN != 0 {
                let mut bytes = [0; 256];
                while send_wake.read(&mut bytes).is_ok() {}
            }
            if polls[0].revents & libc::POLLOUT != 0 {
                let mut queue = transport.queue.lock().unwrap();
                if let Some(packet) = queue.front() {
                    let fds = wire::raw(&packet.fds);
                    match wire::send(socket.as_raw_fd(), &packet.value, &fds) {
                        Ok(()) => {
                            queue.pop_front();
                        }
                        Err(error)
                            if error
                                .downcast_ref::<io::Error>()
                                .is_some_and(|e| e.kind() == io::ErrorKind::WouldBlock) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
            if polls[0].revents & libc::POLLIN == 0 {
                continue;
            }
            let (event, fds) = wire::receive(socket.as_raw_fd())?;
            let id = event["id"].as_u64().unwrap_or(0);
            if event["kind"] == "closed" {
                continue;
            }
            ensure!(event["kind"] == "update", "invalid CEF host event");
            let browser = shared.browsers.lock().unwrap().get(&id).cloned();
            let Some(browser) = browser else {
                if !event["gpu"].is_null() {
                    let info = wire::frame_info(&event["gpu"])?;
                    ensure!(
                        fds.len() == info.planes.len(),
                        "Web frame descriptor mismatch"
                    );
                    transport.send_ack(json!({
                        "kind":"ack","id":id,"sequence":info.sequence,"accepted":false
                    }))?;
                } else {
                    ensure!(fds.is_empty(), "unexpected Web host descriptors");
                }
                continue;
            };
            let mut update = browser.update.lock().unwrap();
            update.frame |= event["frame"].as_bool().unwrap_or(false);
            update.audio |= event["audio"].as_bool().unwrap_or(false);
            if let Some(error) = event["error"].as_str() {
                update.error.get_or_insert(error.into());
            }
            if let Some(damage) = event["damage"].as_array() {
                let rect = damage
                    .get(1)
                    .and_then(Value::as_array)
                    .context("invalid damage")?;
                ensure!(rect.len() == 4, "invalid damage rect");
                update.damage = Some((
                    damage[0].as_u64().context("damage sequence")?,
                    [
                        u32::try_from(rect[0].as_u64().context("damage x")?)?,
                        u32::try_from(rect[1].as_u64().context("damage y")?)?,
                        u32::try_from(rect[2].as_u64().context("damage width")?)?,
                        u32::try_from(rect[3].as_u64().context("damage height")?)?,
                    ],
                ));
            }
            if !event["gpu"].is_null() {
                let info = wire::frame_info(&event["gpu"])?;
                ensure!(
                    fds.len() == info.planes.len(),
                    "Web frame descriptor mismatch"
                );
                let sequence = info.sequence;
                let weak = Arc::downgrade(&transport);
                update.gpu = Some(frames::Frame {
                    info,
                    fds,
                    ack: Some(frames::Ack::Remote(Box::new(move |accepted| {
                        if let Some(transport) = weak.upgrade()
                            && transport
                                .send_ack(json!({"kind":"ack","id":id,"sequence":sequence,"accepted":accepted}))
                                .is_err()
                        {
                            transport.abort();
                        }
                    }))),
                });
            } else {
                ensure!(fds.is_empty(), "unexpected Web host descriptors");
            }
            drop(update);
            let _ = browser.wake.lock().unwrap().write(&[1]);
        }
    })();
    shared.alive.store(false, Ordering::Release);
    let message = result.err().map_or_else(
        || "CEF host exited".into(),
        |e| format!("CEF host failed: {e:#}"),
    );
    for browser in shared.browsers.lock().unwrap().values() {
        browser
            .update
            .lock()
            .unwrap()
            .error
            .get_or_insert(message.clone());
        let _ = browser.wake.lock().unwrap().write(&[1]);
    }
    drop(children);
    drop(live);
}
