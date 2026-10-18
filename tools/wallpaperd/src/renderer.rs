//! Process isolation with nonblocking pipes; the session owns all reply handling.
use crate::{
    domain::{Playback, Selection, Transition, WorkerAction, WorkerCommand, WorkerReply},
    ipc::{self, Lines, Outbox},
};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, net::UnixStream, process::CommandExt},
    },
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Mutex, mpsc},
    time::Duration,
};

fn connect_unix_now(path: &Path) -> io::Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if !path.is_absolute()
        || bytes.is_empty()
        || bytes.contains(&0)
        || bytes.len() >= address.sun_path.len()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Unix socket path",
        ));
    }

    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (destination, source) in address.sun_path.iter_mut().zip(bytes) {
        *destination = *source as libc::c_char;
    }
    let address_len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    if unsafe {
        libc::connect(
            descriptor.as_raw_fd(),
            std::ptr::addr_of!(address).cast(),
            address_len,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }

    let connection = UnixStream::from(descriptor);
    connection.set_nonblocking(false)?;
    Ok(connection)
}

pub struct Renderer {
    child: Option<Child>,
    reaper: mpsc::Sender<Reap>,
    // The supervisor owns the connection lifetime. An abrupt worker exit can
    // leave its last committed surface visible until a replacement is mapped.
    connection: Option<UnixStream>,
    committed: bool,
    #[cfg(feature = "web")]
    web_profile: Option<std::path::PathBuf>,
    pub input: ChildStdin,
    pub output: ChildStdout,
    lines: Lines,
    outbox: Outbox,
    apply: Option<Vec<u8>>,
    playback: Option<Vec<u8>>,
    properties: Option<Vec<u8>>,
    snapshot: Option<Vec<u8>>,
    probe: bool,
    clocks: BTreeMap<String, Vec<u8>>,
    media: Option<Vec<u8>>,
    audio: Option<Vec<u8>>,
}

impl Renderer {
    #[cfg(test)]
    pub(crate) fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    #[cfg(test)]
    pub fn echo() -> Self {
        let reaper = reaper_sender().expect("starting child reaper");
        let mut child = Command::new("cat")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let renderer = Self {
            child: Some(child),
            reaper,
            connection: None,
            committed: false,
            #[cfg(feature = "web")]
            web_profile: None,
            input,
            output,
            lines: Lines::default(),
            outbox: Outbox::default(),
            apply: None,
            playback: None,
            properties: None,
            snapshot: None,
            probe: false,
            clocks: BTreeMap::new(),
            media: None,
            audio: None,
        };
        // Construct the owner before fallible pipe setup so a test failure
        // still transfers the child to the already-running reaper.
        ipc::nonblocking(&renderer.input).unwrap();
        ipc::nonblocking(&renderer.output).unwrap();
        renderer
    }

    #[cfg(test)]
    pub(crate) fn echo_connected() -> (Self, UnixStream) {
        let mut renderer = Self::echo();
        let (connection, peer) = UnixStream::pair().unwrap();
        renderer.connection = Some(connection);
        (renderer, peer)
    }

    pub fn spawn(output: &str) -> Result<Self> {
        let reaper = reaper_sender().context("starting renderer reaper")?;
        let display = std::env::var_os("WAYLAND_DISPLAY")
            .map(std::path::PathBuf::from)
            .context("WAYLAND_DISPLAY is missing")?;
        let path = if display.is_absolute() {
            display
        } else {
            let directory = std::env::var_os("XDG_RUNTIME_DIR")
                .map(std::path::PathBuf::from)
                .context("XDG_RUNTIME_DIR is missing")?;
            anyhow::ensure!(directory.is_absolute(), "XDG_RUNTIME_DIR must be absolute");
            directory.join(display)
        };
        let connection = connect_unix_now(&path).context("connecting renderer to Wayland")?;
        // The inherited socket must survive stdio remapping even when the
        // daemon was started with one of descriptors 0..2 closed.
        let descriptor = unsafe { libc::fcntl(connection.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        anyhow::ensure!(
            descriptor >= 0,
            "duplicating Wayland connection: {}",
            io::Error::last_os_error()
        );
        let inherited = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let parent = std::process::id() as i32;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .process_group(0)
            .arg("render-worker")
            .arg("--output")
            .arg(output)
            .env("WAYLAND_SOCKET", descriptor.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // SAFETY: the child only clears CLOEXEC on its inherited descriptor.
        // Connection::connect_to_env consumes it and restores CLOEXEC before
        // any backend can spawn a subprocess. The parent's descriptor is unchanged.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
                if libc::fcntl(descriptor, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        #[cfg(feature = "web")]
        let web_profile = {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("wallpaperd-web-{}-{nonce}", std::process::id()));
            // Created lazily by CEF; the daemon owns cleanup even after worker SIGKILL.
            command.env("WALLPAPERD_WEB_PROFILE", &path);
            path
        };
        let mut child = command
            .spawn()
            .with_context(|| format!("starting renderer for {output}"))?;
        drop(inherited);
        let result = (|| -> Result<_> {
            let input = child.stdin.take().context("worker stdin missing")?;
            let output = child.stdout.take().context("worker stdout missing")?;
            ipc::nonblocking(&input)?;
            ipc::nonblocking(&output)?;
            Ok((input, output))
        })();
        let (input, output) = match result {
            Ok(pipes) => pipes,
            Err(error) => {
                let _ = child.kill();
                // The reaper was started before the child, so this path can
                // never abandon an unwaited process after pipe setup fails.
                let _ = reaper.send(Reap {
                    child,
                    #[cfg(feature = "web")]
                    web_profile: Some(web_profile),
                });
                return Err(error);
            }
        };
        Ok(Self {
            child: Some(child),
            reaper,
            connection: Some(connection),
            committed: false,
            #[cfg(feature = "web")]
            web_profile: Some(web_profile),
            input,
            output,
            lines: Lines::default(),
            outbox: Outbox::default(),
            apply: None,
            playback: None,
            properties: None,
            snapshot: None,
            probe: false,
            clocks: BTreeMap::new(),
            media: None,
            audio: None,
        })
    }

    pub fn send(
        &mut self,
        id: u64,
        selection: Option<Selection>,
        transition: Transition,
        playback: Playback,
        properties: crate::properties::Values,
    ) {
        let command = WorkerCommand {
            id,
            action: match selection {
                Some(selection) => WorkerAction::Apply {
                    selection,
                    transition,
                    playback,
                    properties,
                },
                None => WorkerAction::Release,
            },
        };
        self.apply = Some(ipc::encode(&command));
    }

    pub fn set_playback(&mut self, id: u64, playback: Playback) {
        self.playback = Some(ipc::encode(&WorkerCommand {
            id,
            action: WorkerAction::SetPlayback { playback },
        }));
    }

    pub fn wants_write(&self) -> bool {
        self.outbox.pending()
            || self.apply.is_some()
            || self.playback.is_some()
            || self.properties.is_some()
            || self.snapshot.is_some()
            || self.probe
            || !self.clocks.is_empty()
            || self.media.is_some()
            || self.audio.is_some()
    }

    pub fn probe(&mut self) {
        self.probe = true;
    }

    pub fn snapshot(&mut self, id: u64, selection_id: u64) {
        self.snapshot = Some(ipc::encode(&WorkerCommand {
            id,
            action: WorkerAction::Snapshot { selection_id },
        }));
    }

    pub fn set_clock(&mut self, asset_id: String, clock: wallpaper_media::clock::Timeline) {
        self.clocks.insert(
            asset_id.clone(),
            ipc::encode(&WorkerCommand {
                id: 0,
                action: WorkerAction::SetClock { asset_id, clock },
            }),
        );
    }

    pub fn set_media(&mut self, media: &crate::media::Snapshot) {
        self.media = Some(ipc::encode(&WorkerCommand {
            id: 0,
            action: WorkerAction::SetMedia {
                media: Box::new(media.clone()),
            },
        }));
    }
    pub fn set_audio(&mut self, audio: &we_scene::audio::AudioSnapshot) {
        self.audio = Some(ipc::encode(&WorkerCommand {
            id: 0,
            action: WorkerAction::SetAudio {
                audio: Box::new(audio.clone()),
            },
        }));
    }

    pub fn set_properties(
        &mut self,
        id: u64,
        selection_id: u64,
        asset_id: String,
        properties: crate::properties::Values,
    ) {
        self.properties = Some(ipc::encode(&WorkerCommand {
            id,
            action: WorkerAction::SetProperties {
                selection_id,
                asset_id,
                properties,
            },
        }));
    }

    pub fn flush(&mut self) -> io::Result<()> {
        loop {
            if !self.outbox.pending() {
                let Some(bytes) = self
                    .apply
                    .take()
                    .or_else(|| self.playback.take())
                    .or_else(|| self.properties.take())
                    .or_else(|| {
                        self.probe.then(|| {
                            self.probe = false;
                            ipc::encode(&WorkerCommand {
                                id: 0,
                                action: WorkerAction::Probe,
                            })
                        })
                    })
                    .or_else(|| self.clocks.pop_first().map(|(_, bytes)| bytes))
                    .or_else(|| self.media.take())
                    .or_else(|| self.snapshot.take())
                    .or_else(|| self.audio.take())
                else {
                    return Ok(());
                };
                self.outbox.replace(bytes);
            }
            self.outbox.flush(&mut self.input)?;
            if self.outbox.pending() {
                return Ok(());
            }
        }
    }

    pub fn read(&mut self) -> Result<(Vec<WorkerReply>, bool)> {
        let (lines, eof) = self.lines.read(&mut self.output)?;
        let replies: Vec<WorkerReply> = lines
            .iter()
            .map(|line| serde_json::from_slice(line))
            .collect::<Result<_, _>>()?;
        self.committed |= replies.iter().any(|reply| {
            reply
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.committed)
        });
        Ok((replies, eof))
    }

    pub fn preserve_frame(&mut self, allow_unconfirmed: bool) -> Option<UnixStream> {
        if self.committed || allow_unconfirmed {
            let connection = self.connection.take()?;
            // Never read the protocol while its worker is alive. Once the
            // worker has gone, discard events so an interactive frozen surface
            // cannot exhaust the compositor's client queue during recovery.
            connection.set_nonblocking(true).ok()?;
            Some(connection)
        } else {
            None
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        // The worker owns its process group, including Chromium subprocesses.
        // Cleanup must also work after a browser-host crash skips CefShutdown.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        // The sender is acquired before spawning the child and the receiver
        // has no fatal path, so Drop never needs to start a thread or wait.
        let _ = self.reaper.send(Reap {
            child,
            #[cfg(feature = "web")]
            web_profile: self.web_profile.take(),
        });
    }
}

struct Reap {
    child: Child,
    #[cfg(feature = "web")]
    web_profile: Option<std::path::PathBuf>,
}

impl Reap {
    fn cleanup(self) {
        #[cfg(feature = "web")]
        if let Some(path) = self.web_profile {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn reaper_sender() -> io::Result<mpsc::Sender<Reap>> {
    // Keep initialization retryable: a transient thread creation failure must
    // not be cached for the daemon lifetime or permit a child to be spawned.
    static REAPER: Mutex<Option<mpsc::Sender<Reap>>> = Mutex::new(None);
    let mut reaper = REAPER.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(sender) = reaper.as_ref() {
        return Ok(sender.clone());
    }
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("wallpaper-reaper".into())
        .spawn(move || reaper_loop(receiver))?;
    *reaper = Some(sender.clone());
    Ok(sender)
}

fn reaper_loop(receiver: mpsc::Receiver<Reap>) {
    let mut children = Vec::new();
    loop {
        if children.is_empty() {
            let Ok(child) = receiver.recv() else {
                return;
            };
            children.push(child);
        } else {
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(child) => children.push(child),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        children.extend(receiver.try_iter());
        let mut index = 0;
        while index < children.len() {
            match children[index].child.try_wait() {
                Ok(None) => index += 1,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => index += 1,
                Ok(Some(_)) | Err(_) => children.swap_remove(index).cleanup(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_unix_backlog_fails_immediately() {
        use std::os::unix::net::UnixListener;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wayland-test");
        let listener = UnixListener::bind(&path).unwrap();
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);

        let start = std::time::Instant::now();
        let mut queued = Vec::new();
        let error = loop {
            match connect_unix_now(&path) {
                Ok(connection) => {
                    let flags = unsafe { libc::fcntl(connection.as_raw_fd(), libc::F_GETFL) };
                    assert!(flags >= 0);
                    assert_eq!(flags & libc::O_NONBLOCK, 0);
                    queued.push(connection);
                    assert!(queued.len() < 32, "test could not saturate listen backlog");
                }
                Err(error) => break error,
            }
        };

        assert!(
            error.kind() == io::ErrorKind::WouldBlock
                || error.raw_os_error() == Some(libc::EINPROGRESS),
            "unexpected connect error: {error}"
        );
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "full backlog connect did not fail promptly"
        );
    }

    #[test]
    fn slow_audio_consumer_keeps_only_current_line_and_latest_snapshot() {
        use std::os::fd::AsRawFd;
        let mut renderer = Renderer::echo();
        // SAFETY: limit the test-owned pipe so backpressure does not depend on host pipe sizing.
        assert!(unsafe { libc::fcntl(renderer.input.as_raw_fd(), libc::F_SETPIPE_SZ, 4096) } >= 0);
        // SAFETY: signal only the child owned by this test. Drop reaps it even on failure.
        assert_eq!(
            unsafe { libc::kill(renderer.pid() as i32, libc::SIGSTOP) },
            0
        );
        for sequence in 1..=500 {
            renderer.set_audio(&we_scene::audio::AudioSnapshot {
                sequence,
                ..Default::default()
            });
            renderer.flush().unwrap();
        }
        assert!(
            renderer.audio.is_some(),
            "test did not fill the worker pipe"
        );
        let pending: WorkerCommand =
            serde_json::from_slice(renderer.audio.as_ref().unwrap()).unwrap();
        assert!(
            matches!(pending.action, WorkerAction::SetAudio { audio } if audio.sequence == 500)
        );
        renderer.set_playback(
            7,
            Playback {
                paused: true,
                ..Playback::default()
            },
        );
        // SAFETY: resume that same live child so it can drain the bounded pipe.
        assert_eq!(
            unsafe { libc::kill(renderer.pid() as i32, libc::SIGCONT) },
            0
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut sequences = Vec::new();
        let mut paused = false;
        while sequences.last() != Some(&500) {
            assert!(
                std::time::Instant::now() < deadline,
                "worker queue did not drain"
            );
            renderer.flush().unwrap();
            let (lines, _) = renderer.lines.read(&mut renderer.output).unwrap();
            for line in lines {
                let command: WorkerCommand = serde_json::from_slice(&line).unwrap();
                match command.action {
                    WorkerAction::SetAudio { audio } => sequences.push(audio.sequence),
                    WorkerAction::SetPlayback { playback } => paused = playback.paused,
                    _ => panic!("unexpected worker action"),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(paused, "latest audio delayed the playback control");
        assert!(
            sequences.len() < 10,
            "historical spectrum queue: {sequences:?}"
        );
        assert!(!renderer.wants_write());
    }

    #[test]
    fn reaper_accepts_live_children_without_waiting_for_them() {
        let reaper = reaper_sender().unwrap();
        let child = Command::new("sleep").arg("0.5").spawn().unwrap();
        let pid = child.id() as i32;
        let start = std::time::Instant::now();
        reaper
            .send(Reap {
                child,
                #[cfg(feature = "web")]
                web_profile: None,
            })
            .unwrap();
        assert!(start.elapsed() < Duration::from_millis(200));

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            // A zombie still has a PID; ESRCH proves that the background thread reaped it.
            if unsafe { libc::kill(pid, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn selection_and_playback_queues_do_not_replace_each_other() {
        let mut renderer = Renderer::echo();
        let selection = Selection {
            asset_id: "local:/video.mkv".into(),
            fit: crate::domain::Fit::Cover,
        };
        renderer.send(
            1,
            Some(selection.clone()),
            Transition::Cut,
            Playback::default(),
            Default::default(),
        );
        renderer.send(
            2,
            Some(selection),
            Transition::Cut,
            Playback::default(),
            Default::default(),
        );
        renderer.set_playback(
            3,
            Playback {
                paused: true,
                ..Playback::default()
            },
        );
        renderer.set_properties(4, 2, "we:/project".into(), Default::default());
        renderer.set_properties(
            5,
            2,
            "we:/project".into(),
            serde_json::from_value(serde_json::json!({"style":"0","strength":0.5})).unwrap(),
        );
        renderer.snapshot(6, 2);
        let clock = wallpaper_media::clock::Timeline::new(100, true);
        renderer.set_clock("we:/project".into(), clock);
        renderer.set_clock("we:/project".into(), clock);
        renderer.set_media(&crate::media::Snapshot {
            revision: 1,
            ..Default::default()
        });
        renderer.set_media(&crate::media::Snapshot {
            revision: 2,
            ..Default::default()
        });
        renderer.probe();
        renderer.probe();
        renderer.flush().unwrap();
        let mut commands: Vec<WorkerCommand> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while commands.len() < 7 {
            assert!(std::time::Instant::now() < deadline);
            let mut fds = [ipc::interest(&renderer.output, false)];
            ipc::poll(&mut fds, 100).unwrap();
            let (lines, _) = renderer.lines.read(&mut renderer.output).unwrap();
            commands.extend(
                lines
                    .iter()
                    .map(|line| serde_json::from_slice::<WorkerCommand>(line).unwrap()),
            );
        }
        assert_eq!(
            commands.iter().map(|c| c.id).collect::<Vec<_>>(),
            [2, 3, 5, 0, 0, 0, 6]
        );
        assert!(matches!(commands[0].action, WorkerAction::Apply { .. }));
        assert!(matches!(
            commands[1].action,
            WorkerAction::SetPlayback { .. }
        ));
        assert!(
            matches!(&commands[2].action, WorkerAction::SetProperties { properties, selection_id: 2, .. } if properties["style"] == "0" && properties["strength"] == 0.5)
        );
        assert!(matches!(
            commands[6].action,
            WorkerAction::Snapshot { selection_id: 2 }
        ));
        assert!(matches!(commands[3].action, WorkerAction::Probe));
        assert!(
            matches!(&commands[4].action, WorkerAction::SetClock { asset_id, clock: actual }
            if asset_id == "we:/project" && *actual == clock)
        );
        assert!(
            matches!(&commands[5].action, WorkerAction::SetMedia { media } if media.revision == 2)
        );
    }
}
