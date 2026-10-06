//! Bounded newline framing and readiness helpers shared by the daemon and workers.
use std::{
    io::{self, Read, Write},
    os::fd::AsRawFd,
};

use serde::Serialize;

pub const MAX_LINE: usize = 64 * 1024;

#[derive(Default)]
pub struct Lines {
    partial: Vec<u8>,
}

impl Lines {
    /// Read at most one request budget per turn so a busy peer cannot starve others.
    pub fn read(&mut self, reader: &mut impl Read) -> io::Result<(Vec<Vec<u8>>, bool)> {
        let mut lines = Vec::new();
        let mut bytes = [0; 4096];
        let mut budget = MAX_LINE;
        while budget > 0 {
            match reader.read(&mut bytes[..budget.min(4096)]) {
                Ok(0) => return Ok((lines, true)),
                Ok(n) => {
                    budget -= n;
                    for part in bytes[..n].split_inclusive(|byte| *byte == b'\n') {
                        if self.partial.len() + part.len() > MAX_LINE {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "JSON line exceeds 64 KiB",
                            ));
                        }
                        self.partial.extend_from_slice(part);
                        if self.partial.last() == Some(&b'\n') {
                            lines.push(std::mem::take(&mut self.partial));
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok((lines, false))
    }
}

pub fn encode(value: &impl Serialize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).expect("serializing protocol message");
    bytes.push(b'\n');
    bytes
}

/// A partially written line must finish before replacing subsequent snapshots.
#[derive(Default)]
pub struct Outbox {
    sending: Vec<u8>,
    offset: usize,
    next: Option<Vec<u8>>,
}

impl Outbox {
    pub fn replace(&mut self, bytes: Vec<u8>) {
        if self.offset == 0 {
            self.sending = bytes;
        } else {
            self.next = Some(bytes);
        }
    }

    pub fn pending(&self) -> bool {
        !self.sending.is_empty()
    }

    pub fn flush(&mut self, writer: &mut impl Write) -> io::Result<()> {
        while self.pending() {
            match writer.write(&self.sending[self.offset..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => self.offset += n,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
            if self.offset == self.sending.len() {
                self.sending = self.next.take().unwrap_or_default();
                self.offset = 0;
            }
        }
        Ok(())
    }
}

pub fn nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: fcntl neither takes ownership of the live fd nor accesses Rust memory.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn poll(fds: &mut [libc::pollfd], timeout: i32) -> io::Result<()> {
    // SAFETY: the slice is writable for precisely the supplied number of pollfd entries.
    if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) } < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

pub fn interest(fd: &impl AsRawFd, write: bool) -> libc::pollfd {
    libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN | if write { libc::POLLOUT } else { 0 },
        revents: 0,
    }
}

pub fn flush_wayland(conn: &wayland_client::Connection) -> anyhow::Result<bool> {
    match conn.flush() {
        Ok(()) => Ok(false),
        Err(wayland_client::backend::WaylandError::Io(error))
            if error.kind() == io::ErrorKind::WouldBlock =>
        {
            Ok(true)
        }
        Err(error) => Err(error.into()),
    }
}

pub fn read_wayland(guard: wayland_client::backend::ReadEventsGuard) -> anyhow::Result<()> {
    match guard.read() {
        Ok(_) => Ok(()),
        Err(wayland_client::backend::WaylandError::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

pub mod server {
    //! Socket ownership, client lifetimes, and bounded/coalesced subscription writes.
    use crate::{
        domain::Request,
        ipc::{self, Lines, Outbox},
    };
    use anyhow::{Context, Result};
    use serde_json::Value;
    use std::{
        collections::BTreeMap,
        fs, io,
        os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
        path::PathBuf,
        time::{Duration, Instant},
    };

    const MAX_CLIENTS: usize = 128;
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

    enum Mode {
        Reading,
        Waiting,
        Subscriber,
        Closing,
    }
    struct Client {
        stream: UnixStream,
        lines: Lines,
        outbox: Outbox,
        mode: Mode,
        read_closed: bool,
        accepted: Instant,
    }

    pub struct Server {
        pub listener: UnixListener,
        path: PathBuf,
        clients: BTreeMap<u64, Client>,
        next_id: u64,
    }

    impl Server {
        pub fn bind(path: PathBuf) -> Result<Self> {
            fs::create_dir_all(path.parent().context("socket has no parent")?)?;
            if path.exists() {
                match UnixStream::connect(&path) {
                    Ok(_) => anyhow::bail!("wallpaperd is already running"),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                        ) =>
                    {
                        fs::remove_file(&path)
                            .with_context(|| format!("removing stale {}", path.display()))?;
                    }
                    Err(error) => return Err(error).context("checking existing daemon socket"),
                }
            }
            let listener =
                UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            Ok(Self {
                listener,
                path,
                clients: BTreeMap::new(),
                next_id: 1,
            })
        }

        pub fn accept(&mut self) -> Result<()> {
            // Bound work as well as live connections under a burst of short-lived peers.
            for _ in 0..MAX_CLIENTS {
                match self.listener.accept() {
                    Ok((stream, _)) => {
                        if self.clients.len() >= MAX_CLIENTS {
                            continue;
                        }
                        stream.set_nonblocking(true)?;
                        self.clients.insert(
                            self.next_id,
                            Client {
                                stream,
                                lines: Lines::default(),
                                outbox: Outbox::default(),
                                mode: Mode::Reading,
                                read_closed: false,
                                accepted: Instant::now(),
                            },
                        );
                        self.next_id += 1;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        }

        pub fn interests(&self) -> impl Iterator<Item = (u64, libc::pollfd)> + '_ {
            self.clients.iter().map(|(&id, client)| {
                let mut fd = ipc::interest(&client.stream, client.outbox.pending());
                if client.read_closed {
                    fd.events &= !libc::POLLIN;
                }
                (id, fd)
            })
        }

        pub fn ready(&mut self, id: u64, events: i16) -> Result<Option<Request>> {
            let Some(client) = self.clients.get_mut(&id) else {
                return Ok(None);
            };
            let peer_gone = events & libc::POLLHUP != 0;
            if events & (libc::POLLERR | libc::POLLNVAL) != 0
                || (peer_gone && !matches!(client.mode, Mode::Reading))
            {
                self.clients.remove(&id);
                return Ok(None);
            }
            if events & libc::POLLOUT != 0
                && (client.outbox.flush(&mut client.stream).is_err()
                    || (!client.outbox.pending() && matches!(client.mode, Mode::Closing)))
            {
                self.clients.remove(&id);
                return Ok(None);
            }
            if events & (libc::POLLIN | libc::POLLHUP) != 0 {
                let (lines, eof) = match client.lines.read(&mut client.stream) {
                    Ok(value) => value,
                    Err(error) => {
                        self.clients.remove(&id);
                        return Err(error.into());
                    }
                };
                client.read_closed = eof;
                if let Some(line) = lines.first() {
                    if !matches!(client.mode, Mode::Reading) || lines.len() > 1 {
                        self.clients.remove(&id);
                        return Ok(None);
                    }
                    client.mode = Mode::Waiting;
                    // A complete buffered request still applies after its caller disconnects.
                    if peer_gone {
                        self.clients.remove(&id);
                    }
                    match serde_json::from_slice(line) {
                    Ok(request) => return Ok(Some(request)),
                    Err(error) => self.reply(id, &serde_json::json!({"ok":false,"error":{"code":"bad_request","message":error.to_string()}})),
                }
                } else if eof && matches!(client.mode, Mode::Reading) {
                    self.clients.remove(&id);
                }
            }
            Ok(None)
        }

        pub fn reply(&mut self, id: u64, value: &Value) {
            if let Some(client) = self.clients.get_mut(&id) {
                client.mode = Mode::Closing;
                client.outbox.replace(ipc::encode(value));
            }
        }

        pub fn subscribe(&mut self, id: u64, snapshot: &Value) {
            if let Some(client) = self.clients.get_mut(&id) {
                client.mode = Mode::Subscriber;
                client.outbox.replace(ipc::encode(snapshot));
            }
        }

        pub fn publish(&mut self, snapshot: &Value) {
            let mut encoded = None;
            for client in self
                .clients
                .values_mut()
                .filter(|c| matches!(c.mode, Mode::Subscriber))
            {
                let bytes = encoded.get_or_insert_with(|| ipc::encode(snapshot));
                client.outbox.replace(bytes.clone());
            }
        }

        pub fn expire(&mut self) {
            self.clients.retain(|_, client| {
                !matches!(client.mode, Mode::Reading) || client.accepted.elapsed() < REQUEST_TIMEOUT
            });
        }

        pub fn deadline(&self) -> Option<Instant> {
            self.clients
                .values()
                .filter(|c| matches!(c.mode, Mode::Reading))
                .map(|c| c.accepted + REQUEST_TIMEOUT)
                .min()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn fragmented_lines_and_eof() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut lines = Lines::default();
        writer.write_all(b"{\"a\":").unwrap();
        assert!(lines.read(&mut reader).unwrap().0.is_empty());
        writer.write_all(b"1}\n{}\n").unwrap();
        // A concurrent fork can temporarily inherit the fd before exec closes it.
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        drop(writer);
        let (messages, eof) = lines.read(&mut reader).unwrap();
        assert!(eof);
        assert_eq!(messages, [b"{\"a\":1}\n".to_vec(), b"{}\n".to_vec()]);
    }

    #[test]
    fn oversized_partial_line_is_rejected() {
        let mut lines = Lines::default();
        let mut source = io::Cursor::new(vec![b'x'; MAX_LINE + 1]);
        lines.read(&mut source).unwrap();
        assert_eq!(
            lines.read(&mut source).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn snapshots_coalesce_without_corrupting_partial_line() {
        struct ShortWriter(Vec<u8>);
        impl Write for ShortWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.0.is_empty() {
                    self.0.push(bytes[0]);
                    Ok(1)
                } else {
                    Err(io::ErrorKind::WouldBlock.into())
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut outbox = Outbox::default();
        outbox.replace(b"first\n".to_vec());
        let mut writer = ShortWriter(Vec::new());
        outbox.flush(&mut writer).unwrap();
        outbox.replace(b"second\n".to_vec());
        outbox.replace(b"latest\n".to_vec());
        outbox.flush(&mut writer.0).unwrap();
        assert_eq!(writer.0, b"first\nlatest\n");
        assert!(!outbox.pending());
    }
}
