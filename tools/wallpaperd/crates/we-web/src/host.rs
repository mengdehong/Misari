use crate::{HostRenderer, ShutdownGuard, frames, wire};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    os::unix::net::UnixStream,
};

pub(crate) fn argument() -> Option<RawFd> {
    std::env::args().find_map(|arg| arg.strip_prefix("--we-web-host-fd=")?.parse().ok())
}

pub(crate) fn guardian_argument() -> Option<(RawFd, libc::pid_t)> {
    let mut fd = None;
    let mut group = None;
    for arg in std::env::args() {
        fd = fd.or_else(|| arg.strip_prefix("--we-web-guardian-fd=")?.parse().ok());
        group = group.or_else(|| arg.strip_prefix("--we-web-host-pgid=")?.parse().ok());
    }
    fd.zip(group)
}

pub(crate) fn guard(inherited: RawFd, group: libc::pid_t) -> Result<()> {
    ensure!(group > 1, "invalid Web host process group");
    let fd = unsafe { libc::fcntl(inherited, libc::F_DUPFD_CLOEXEC, 3) };
    ensure!(
        fd >= 0,
        "duplicating Web guardian pipe: {}",
        io::Error::last_os_error()
    );
    unsafe { libc::close(inherited) };
    let mut pipe = unsafe { File::from_raw_fd(fd) };
    let mut byte = [0];
    loop {
        match pipe.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    unsafe { libc::kill(-group, libc::SIGKILL) };
    for _ in 0..100 {
        if !process_group_running(group) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if let Some(profile) = std::env::var_os("WALLPAPERD_WEB_PROFILE") {
        let profile = std::path::PathBuf::from(profile);
        let _ = std::fs::remove_dir_all(&profile);
        if let Some(parent) = profile.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    Ok(())
}

fn process_group_running(group: libc::pid_t) -> bool {
    std::fs::read_dir("/proc").is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                return false;
            };
            let Some((_, fields)) = stat.rsplit_once(") ") else {
                return false;
            };
            let mut fields = fields.split_whitespace();
            let state = fields.next();
            let _parent = fields.next();
            state != Some("Z") && fields.next().and_then(|v| v.parse().ok()) == Some(group)
        })
    })
}

pub(crate) fn run(inherited: RawFd) -> Result<()> {
    let socket = unsafe { libc::fcntl(inherited, libc::F_DUPFD_CLOEXEC, 3) };
    ensure!(
        socket >= 0,
        "duplicating Web host socket: {}",
        io::Error::last_os_error()
    );
    unsafe { libc::close(inherited) };
    let socket = unsafe { OwnedFd::from_raw_fd(socket) };
    let (mut wake_read, wake_write) = UnixStream::pair()?;
    wake_read.set_nonblocking(true)?;
    let _runtime = ShutdownGuard;
    let mut renderers = HashMap::<u64, HostRenderer>::new();
    let mut pending = HashMap::<(u64, u64), frames::Frame>::new();
    loop {
        let mut polls = [
            libc::pollfd {
                fd: socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_read.as_raw_fd(),
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
        if polls[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            break;
        }
        if polls[0].revents & libc::POLLIN != 0 {
            let (command, mut fds) = wire::receive(socket.as_raw_fd())?;
            let id = command["id"].as_u64().unwrap_or(0);
            match command["kind"].as_str().unwrap_or("") {
                "start" => {
                    let result = (|| -> Result<_> {
                        ensure!(id != 0 && fds.len() == 1, "invalid Web start message");
                        let size = command["size"].as_array().context("missing Web size")?;
                        let size = [
                            u32::try_from(size[0].as_u64().context("width")?)?,
                            u32::try_from(size[1].as_u64().context("height")?)?,
                        ];
                        HostRenderer::load(
                            command["root"].as_str().context("missing root")?.as_ref(),
                            command["entry"].as_str().context("missing entry")?.as_ref(),
                            size,
                            u16::try_from(command["frame_rate"].as_u64().context("frame rate")?)?,
                            command["accelerated"].as_bool().unwrap_or(false),
                            command["platform"].as_str().unwrap_or("headless"),
                            &wake_write,
                            File::from(fds.remove(0)),
                        )
                    })();
                    match result {
                        Ok(renderer) => {
                            renderers.insert(id, renderer);
                        }
                        Err(error) => send_error(socket.as_raw_fd(), id, format!("{error:#}"))?,
                    }
                }
                "control" => {
                    ensure!(fds.is_empty(), "descriptors on Web control");
                    if let Some(renderer) = renderers.get(&id)
                        && let Err(error) = renderer.send(
                            command["value"].clone(),
                            command["edge"].as_bool().unwrap_or(false),
                        )
                    {
                        send_error(socket.as_raw_fd(), id, format!("{error:#}"))?;
                    }
                }
                "ack" => {
                    ensure!(fds.is_empty(), "descriptors on Web ack");
                    let sequence = command["sequence"].as_u64().context("missing sequence")?;
                    if let Some(frame) = pending.remove(&(id, sequence)) {
                        frame.complete(command["accepted"].as_bool().unwrap_or(false));
                    }
                }
                "close" => {
                    renderers.remove(&id);
                    pending.retain(|(owner, _), _| *owner != id);
                    wire::send(socket.as_raw_fd(), &json!({"kind":"closed","id":id}), &[])?;
                    if renderers.is_empty() {
                        break;
                    }
                }
                "shutdown" => break,
                _ => anyhow::bail!("invalid Web host command"),
            }
        }
        if polls[1].revents & libc::POLLIN != 0 {
            let mut bytes = [0; 256];
            while wake_read.read(&mut bytes).is_ok() {}
        }
        for (&id, renderer) in &renderers {
            let mut update = renderer.poll();
            if !update.frame && update.gpu.is_none() && !update.audio && update.error.is_none() {
                continue;
            }
            let mut event = json!({"kind":"update","id":id,"frame":update.frame,"audio":update.audio,"error":update.error});
            if let Some((sequence, damage)) = update.damage {
                event["damage"] = json!([sequence, damage]);
            }
            if let Some(frame) = update.gpu.take() {
                let sequence = frame.info.sequence;
                event["gpu"] = wire::frame_value(&frame.info);
                wire::send(socket.as_raw_fd(), &event, &wire::raw(&frame.fds))?;
                if frame.ack.is_some() {
                    pending.insert((id, sequence), frame);
                }
            } else {
                wire::send(socket.as_raw_fd(), &event, &[])?;
            }
        }
    }
    drop(pending);
    drop(renderers);
    Ok(())
}

fn send_error(socket: RawFd, id: u64, error: String) -> Result<()> {
    wire::send(socket, &json!({"kind":"update","id":id,"error":error}), &[])
}
