//! Optional evidence producers notify the owner with one replaceable snapshot.
use parking_lot::Mutex;
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    sync::Arc,
    thread,
    time::Duration,
};

pub struct Monitor<T> {
    pub wake: UnixStream,
    latest: Arc<Mutex<T>>,
}

impl<T: Clone + Default + PartialEq + Send + 'static> Monitor<T> {
    pub fn start(
        name: &str,
        watch: impl Fn(&mut dyn FnMut(T)) -> anyhow::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let (wake, mut writer) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        let latest = Arc::new(Mutex::new(T::default()));
        let shared = latest.clone();
        let name = name.to_owned();
        thread::Builder::new().name(name.clone()).spawn(move || {
            loop {
                let mut publish = |snapshot: T| {
                    let mut previous = shared.lock();
                    if *previous != snapshot {
                        *previous = snapshot;
                        let _ = writer.write_all(&[1]);
                    }
                };
                if let Err(error) = watch(&mut publish) {
                    eprintln!("wallpaperd: {name}: {error:#}");
                }
                publish(T::default());
                thread::sleep(Duration::from_secs(5));
            }
        })?;
        Ok(Self { wake, latest })
    }

    pub fn receive(&mut self) -> io::Result<T> {
        let mut bytes = [0; 64];
        loop {
            match self.wake.read(&mut bytes) {
                Ok(0) => return Err(io::ErrorKind::BrokenPipe.into()),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(self.latest.lock().clone())
    }
}

pub mod power {
    use serde::Serialize;
    use std::{
        io,
        os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        path::Path,
    };

    pub struct Watch(OwnedFd);

    impl Watch {
        pub fn open() -> io::Result<Self> {
            // Subscribe before reading sysfs so a change during the initial
            // snapshot stays queued for the session's next poll.
            let raw = unsafe {
                libc::socket(
                    libc::AF_NETLINK,
                    libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                    libc::NETLINK_KOBJECT_UEVENT,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
            let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            address.nl_groups = 1;
            if unsafe {
                libc::bind(
                    descriptor.as_raw_fd(),
                    std::ptr::addr_of!(address).cast(),
                    std::mem::size_of_val(&address) as libc::socklen_t,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(descriptor))
        }

        pub fn changed(&self) -> io::Result<bool> {
            let mut changed = false;
            let mut bytes = [0; 8192];
            // Bound each turn so a busy uevent source cannot starve clients.
            for _ in 0..64 {
                let mut sender: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
                let mut sender_len = std::mem::size_of_val(&sender) as libc::socklen_t;
                let count = unsafe {
                    libc::recvfrom(
                        self.as_raw_fd(),
                        bytes.as_mut_ptr().cast(),
                        bytes.len(),
                        0,
                        std::ptr::addr_of_mut!(sender).cast(),
                        &mut sender_len,
                    )
                };
                if count < 0 {
                    let error = io::Error::last_os_error();
                    match error.kind() {
                        io::ErrorKind::WouldBlock => break,
                        io::ErrorKind::Interrupted => continue,
                        // A full receive queue lost evidence. Refresh sysfs
                        // once and keep using the still-valid subscription.
                        _ if error.raw_os_error() == Some(libc::ENOBUFS) => changed = true,
                        _ => return Err(error),
                    }
                } else if sender.nl_pid == 0 {
                    // A truncated datagram might hide SUBSYSTEM; rescan rather
                    // than silently miss a power change in that case.
                    changed |=
                        count as usize == bytes.len() || power_event(&bytes[..count as usize]);
                }
            }
            Ok(changed)
        }
    }

    impl AsRawFd for Watch {
        fn as_raw_fd(&self) -> RawFd {
            self.0.as_raw_fd()
        }
    }

    fn power_event(bytes: &[u8]) -> bool {
        bytes
            .split(|byte| *byte == 0)
            .any(|field| field == b"SUBSYSTEM=power_supply")
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
    pub struct Snapshot {
        pub available: bool,
        pub on_battery: bool,
    }
    pub fn read() -> Snapshot {
        read_from(Path::new("/sys/class/power_supply"))
    }
    fn read_from(root: &Path) -> Snapshot {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Snapshot::default();
        };
        let mut battery = false;
        let mut discharging = false;
        let mut online = None;
        for entry in entries.flatten() {
            let path = entry.path();
            let read = |name| {
                std::fs::read_to_string(path.join(name))
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            };
            if read("type") == "Battery" {
                if read("present") == "0" {
                    continue;
                }
                let status = read("status");
                battery |= matches!(
                    status.as_str(),
                    "Charging" | "Discharging" | "Full" | "Not charging"
                );
                discharging |= status == "Discharging";
            } else if let Ok(value) = read("online").parse::<u8>() {
                online = Some(online.unwrap_or(false) || value > 0);
            }
        }
        Snapshot {
            available: battery || online.is_some(),
            on_battery: battery && online.map_or(discharging, |value| !value),
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn only_power_supply_events_refresh_evidence() {
            assert!(power_event(
                b"change@/devices/BAT0\0SUBSYSTEM=power_supply\0"
            ));
            assert!(power_event(b"remove@/devices/AC\0SUBSYSTEM=power_supply\0"));
            assert!(!power_event(b"change@/devices/usb\0SUBSYSTEM=usb\0"));
            assert!(!power_event(b"SUBSYSTEM=power_supply_other\0"));
            assert!(!power_event(b"OTHER=SUBSYSTEM=power_supply\0"));
        }

        #[test]
        fn power_evidence_handles_ac_battery_and_missing_hardware() {
            let root = tempfile::tempdir().unwrap();
            assert!(!read_from(root.path()).available);
            let battery = root.path().join("BAT0");
            std::fs::create_dir(&battery).unwrap();
            std::fs::write(battery.join("type"), "Battery\n").unwrap();
            std::fs::write(battery.join("status"), "Discharging\n").unwrap();
            assert!(read_from(root.path()).on_battery);
            let ac = root.path().join("AC");
            std::fs::create_dir(&ac).unwrap();
            std::fs::write(ac.join("type"), "Mains").unwrap();
            std::fs::write(ac.join("online"), "1\n").unwrap();
            assert!(!read_from(root.path()).on_battery);
            std::fs::write(ac.join("online"), "0\n").unwrap();
            assert!(read_from(root.path()).on_battery);
            std::fs::write(battery.join("present"), "0\n").unwrap();
            assert!(!read_from(root.path()).on_battery);
        }
    }
}

pub mod logind {
    //! logind events are optional desktop evidence, independent of the user's persisted pause.
    use std::{collections::HashMap, io};

    use anyhow::Result;
    use serde::Serialize;
    use zbus::{
        MatchRule,
        blocking::{Connection, MessageIterator, Proxy},
        message::Type,
        zvariant::{OwnedObjectPath, OwnedValue},
    };

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
    pub struct Policy {
        pub available: bool,
        pub locked: bool,
        pub sleeping: bool,
        pub inactive: bool,
    }

    pub fn start() -> io::Result<super::Monitor<Policy>> {
        super::Monitor::start("wallpaper-session", |publish| watch(publish))
    }

    fn watch(mut publish: impl FnMut(Policy)) -> Result<()> {
        let conn = Connection::system()?;
        let manager = Proxy::new(
            &conn,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )?;
        let path = session_path(&conn, &manager)?;
        let session = Proxy::new(
            &conn,
            "org.freedesktop.login1",
            path.as_str(),
            "org.freedesktop.login1.Session",
        )?;
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .sender("org.freedesktop.login1")?
            .build();
        // Register before reading properties so startup cannot lose a lock/unlock event.
        let messages = MessageIterator::for_match_rule(rule, &conn, Some(64))?;
        let mut policy = Policy {
            available: true,
            locked: session.get_property("LockedHint")?,
            inactive: !session.get_property::<bool>("Active")?,
            sleeping: manager.get_property("PreparingForSleep")?,
        };
        publish(policy);
        for message in messages {
            let message = message?;
            let header = message.header();
            let member = header.member().map(|v| v.as_str()).unwrap_or("");
            let interface = header.interface().map(|v| v.as_str()).unwrap_or("");
            let message_path = header.path().map(|v| v.as_str()).unwrap_or("");
            if interface == "org.freedesktop.login1.Manager" && member == "PrepareForSleep" {
                policy.sleeping = message.body().deserialize::<bool>()?;
            } else if message_path == path.as_str() && interface == "org.freedesktop.login1.Session"
            {
                match member {
                    "Lock" => policy.locked = true,
                    "Unlock" => policy.locked = false,
                    _ => {}
                }
            } else if message_path == path.as_str()
                && interface == "org.freedesktop.DBus.Properties"
                && member == "PropertiesChanged"
            {
                let (interface, changed, invalidated): (
                    String,
                    HashMap<String, OwnedValue>,
                    Vec<String>,
                ) = message.body().deserialize()?;
                if interface == "org.freedesktop.login1.Session" {
                    if let Some(value) = changed.get("LockedHint") {
                        policy.locked = bool::try_from(value)?;
                    }
                    if let Some(value) = changed.get("Active") {
                        policy.inactive = !bool::try_from(value)?;
                    }
                    if invalidated.iter().any(|v| v == "LockedHint") {
                        policy.locked = session.get_property("LockedHint")?;
                    }
                    if invalidated.iter().any(|v| v == "Active") {
                        policy.inactive = !session.get_property::<bool>("Active")?;
                    }
                }
            }
            publish(policy);
        }
        anyhow::bail!("logind signal stream disconnected")
    }

    fn session_path(conn: &Connection, manager: &Proxy<'_>) -> Result<OwnedObjectPath> {
        if let Ok(id) = std::env::var("XDG_SESSION_ID") {
            return Ok(manager.call("GetSession", &(id,))?);
        }
        if let Ok(path) = manager.call("GetSessionByPID", &(std::process::id(),)) {
            return Ok(path);
        }
        // systemd user services commonly live outside the session's process cgroup.
        // logind's Display names the user's primary graphical session in that case.
        let user_path: OwnedObjectPath = manager.call("GetUser", &(unsafe { libc::geteuid() },))?;
        let user = Proxy::new(
            conn,
            "org.freedesktop.login1",
            user_path.as_str(),
            "org.freedesktop.login1.User",
        )?;
        let (id, path): (String, OwnedObjectPath) = user.get_property("Display")?;
        anyhow::ensure!(
            !id.is_empty(),
            "logind has no graphical session for this user"
        );
        Ok(path)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        #[ignore = "requires logind and a graphical session; reads only the initial session state"]
        fn logind_initial_snapshot() {
            let mut monitor = start().unwrap();
            let mut fds = [crate::ipc::interest(&monitor.wake, false)];
            crate::ipc::poll(&mut fds, 3000).unwrap();
            assert_ne!(
                fds[0].revents, 0,
                "logind did not provide an initial snapshot"
            );
            let policy = monitor.receive().unwrap();
            assert!(policy.available, "no graphical logind session was found");
        }
    }
}

pub mod audio {
    //! Activity means an uncorked, audible playback stream, not PCM silence detection.
    use libpulse_binding::{
        callbacks::ListResult,
        context::{Context, FlagSet, State, introspect::SinkInputInfo, subscribe::InterestMaskSet},
        mainloop::standard::{IterateResult, Mainloop},
    };
    use serde::Serialize;
    use std::{
        cell::{Cell, RefCell},
        path::Path,
        rc::Rc,
    };

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
    pub struct Snapshot {
        pub available: bool,
        pub other_audio_active: bool,
    }

    fn other_audio(info: &SinkInputInfo<'_>, executable: &Path) -> bool {
        if info.corked
            || info.mute
            || (info.has_volume && info.volume.get().iter().all(|volume| volume.0 == 0))
        {
            return false;
        }
        if info.proplist.get_str("application.id").as_deref() == Some("misari.wallpaperd") {
            return false;
        }
        if let Some(pid) = info
            .proplist
            .get_str("application.process.id")
            .and_then(|value| value.parse::<u32>().ok())
            && std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|path| path == executable)
        {
            return false;
        }
        true
    }

    pub fn start() -> std::io::Result<super::Monitor<Snapshot>> {
        super::Monitor::start("audio-policy", |publish| {
            let mut mainloop =
                Mainloop::new().ok_or_else(|| anyhow::anyhow!("creating libpulse mainloop"))?;
            let mut context = Context::new(&mainloop, "wallpaperd-policy")
                .ok_or_else(|| anyhow::anyhow!("creating libpulse context"))?;
            context.connect(None, FlagSet::NOAUTOSPAWN, None)?;
            loop {
                iterate(&mut mainloop)?;
                match context.get_state() {
                    State::Ready => break,
                    State::Failed | State::Terminated => anyhow::bail!("libpulse disconnected"),
                    _ => {}
                }
            }
            let dirty = Rc::new(Cell::new(true));
            let changed = dirty.clone();
            context.set_subscribe_callback(Some(Box::new(move |_, _, _| changed.set(true))));
            let subscribed = Rc::new(Cell::new(None));
            let subscribed_reply = subscribed.clone();
            let _subscription = context.subscribe(InterestMaskSet::SINK_INPUT, move |ok| {
                subscribed_reply.set(Some(ok))
            });
            while subscribed.get().is_none() {
                iterate(&mut mainloop)?;
            }
            anyhow::ensure!(
                subscribed.get() == Some(true),
                "subscribing to playback streams failed"
            );
            let result = Rc::new(RefCell::new(None));
            let mut query = None;
            loop {
                if query.is_none() && dirty.replace(false) {
                    let reply = result.clone();
                    let executable = std::env::current_exe()?;
                    let mut active = false;
                    query =
                        Some(context.introspect().get_sink_input_info_list(
                            move |item| match item {
                                ListResult::Item(info) => active |= other_audio(info, &executable),
                                ListResult::End => *reply.borrow_mut() = Some(Ok(active)),
                                ListResult::Error => {
                                    *reply.borrow_mut() =
                                        Some(Err("querying playback streams failed"))
                                }
                            },
                        ));
                }
                iterate(&mut mainloop)?;
                anyhow::ensure!(
                    !matches!(context.get_state(), State::Failed | State::Terminated),
                    "libpulse disconnected"
                );
                if let Some(result) = result.borrow_mut().take() {
                    query = None;
                    publish(Snapshot {
                        available: true,
                        other_audio_active: result.map_err(anyhow::Error::msg)?,
                    });
                }
            }
        })
    }
    fn iterate(mainloop: &mut Mainloop) -> anyhow::Result<()> {
        match mainloop.iterate(true) {
            IterateResult::Success(_) => Ok(()),
            _ => anyhow::bail!("libpulse mainloop stopped"),
        }
    }
}

pub mod niri {
    //! niri supplies evidence; only the session decides how it affects playback.
    use niri_ipc::{
        Event, Request, Response,
        socket::Socket,
        state::{EventStreamState, EventStreamStatePart},
    };
    use serde::Serialize;
    use std::collections::BTreeSet;

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    pub struct Snapshot {
        pub available: bool,
        pub fullscreen_available: bool,
        pub overview: bool,
        pub fullscreen_outputs: BTreeSet<String>,
    }

    #[derive(Default)]
    struct Evidence {
        state: EventStreamState,
        initialized: [bool; 3],
    }
    impl Evidence {
        fn apply(&mut self, event: Event) -> Snapshot {
            match &event {
                Event::WorkspacesChanged { .. } => self.initialized[0] = true,
                Event::WindowsChanged { .. } => self.initialized[1] = true,
                Event::OverviewOpenedOrClosed { .. } => self.initialized[2] = true,
                _ => {}
            }
            self.state.apply(event);
            if !self.initialized.iter().all(|ready| *ready) {
                return Snapshot::default();
            }
            let mut snapshot = Snapshot {
                available: true,
                fullscreen_available: self
                    .state
                    .windows
                    .windows
                    .values()
                    .any(|window| window.is_fullscreen.is_some()),
                overview: self.state.overview.is_open,
                ..Snapshot::default()
            };
            if !snapshot.overview {
                for workspace in self
                    .state
                    .workspaces
                    .workspaces
                    .values()
                    .filter(|workspace| workspace.is_active)
                {
                    let window = workspace
                        .active_window_id
                        .and_then(|id| self.state.windows.windows.get(&id));
                    if window.is_some_and(|window| {
                        window.workspace_id == Some(workspace.id)
                            && window.is_fullscreen == Some(true)
                    }) && let Some(output) = &workspace.output
                    {
                        snapshot.fullscreen_outputs.insert(output.clone());
                    }
                }
            }
            snapshot
        }
    }

    pub fn start() -> std::io::Result<super::Monitor<Snapshot>> {
        super::Monitor::start("niri-policy", |publish| {
            let mut socket = Socket::connect()?;
            anyhow::ensure!(
                matches!(socket.send(Request::EventStream)?, Ok(Response::Handled)),
                "niri refused event stream"
            );
            let mut read = socket.read_events();
            let mut evidence = Evidence::default();
            loop {
                publish(evidence.apply(read()?));
            }
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;
        fn window(id: u64, workspace: u64, fullscreen: Option<bool>) -> niri_ipc::Window {
            let mut v = json!({"id":id,"title":null,"app_id":null,"pid":null,"workspace_id":workspace,"is_focused":false,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":null,"tile_size":[1,1],"window_size":[1,1],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[0,0]},"focus_timestamp":null});
            if let Some(fullscreen) = fullscreen {
                v["is_fullscreen"] = json!(fullscreen);
            }
            serde_json::from_value(v).unwrap()
        }
        fn workspace(id: u64, output: &str, active: bool, window: u64) -> niri_ipc::Workspace {
            serde_json::from_value(json!({"id":id,"idx":1,"name":null,"output":output,"is_urgent":false,"is_active":active,"is_focused":false,"active_window_id":window})).unwrap()
        }
        #[test]
        fn fullscreen_is_per_output_active_workspace_and_overview() {
            let mut evidence = Evidence::default();
            assert!(
                !evidence
                    .apply(Event::WorkspacesChanged {
                        workspaces: vec![
                            workspace(1, "A", true, 11),
                            workspace(2, "A", false, 22),
                            workspace(3, "B", true, 33)
                        ]
                    })
                    .available
            );
            assert!(
                !evidence
                    .apply(Event::WindowsChanged {
                        windows: vec![
                            window(11, 1, Some(false)),
                            window(22, 2, Some(true)),
                            window(33, 3, Some(true))
                        ]
                    })
                    .available
            );
            let snapshot = evidence.apply(Event::OverviewOpenedOrClosed { is_open: false });
            assert_eq!(
                snapshot.fullscreen_outputs,
                BTreeSet::from(["B".to_owned()])
            );
            assert!(
                evidence
                    .apply(Event::OverviewOpenedOrClosed { is_open: true })
                    .fullscreen_outputs
                    .is_empty()
            );
            assert_eq!(
                evidence
                    .apply(Event::OverviewOpenedOrClosed { is_open: false })
                    .fullscreen_outputs
                    .len(),
                1
            );
            let snapshot = evidence.apply(Event::WorkspaceActivated {
                id: 2,
                focused: false,
            });
            assert_eq!(snapshot.fullscreen_outputs.len(), 2);
            assert!(
                evidence
                    .apply(Event::WindowClosed { id: 33 })
                    .fullscreen_outputs
                    .contains("A")
            );
            assert!(
                !evidence
                    .apply(Event::WindowOpenedOrChanged {
                        window: window(22, 2, Some(false))
                    })
                    .fullscreen_outputs
                    .contains("A")
            );
        }
        #[test]
        fn old_compositor_has_unknown_fullscreen_instead_of_inferred_geometry() {
            let mut evidence = Evidence::default();
            evidence.apply(Event::WorkspacesChanged {
                workspaces: vec![workspace(1, "A", true, 11)],
            });
            evidence.apply(Event::WindowsChanged {
                windows: vec![window(11, 1, None)],
            });
            let snapshot = evidence.apply(Event::OverviewOpenedOrClosed { is_open: false });
            assert!(snapshot.available);
            assert!(!snapshot.fullscreen_available);
            assert!(snapshot.fullscreen_outputs.is_empty());
        }
    }
}
