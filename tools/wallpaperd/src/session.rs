//! Single-owner state machine. No locks, client threads, blocking worker waits, or idle ticks.
use crate::{
    catalog,
    desktop::Desktop,
    domain::{
        self, Error, Fit, Playback, PlaybackPatch, PlaybackPolicy, Request, Selection, Transition,
        WorkerReply, WorkerRuntime,
    },
    ipc::{self, server::Server},
    library::{Edit, Rotation},
    policy::logind::{self as session_policy, Policy},
    properties::{self, Resolved, Schema, Values},
    renderer::Renderer,
    store::{Config, SavedOutput, Store, socket_path},
};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

#[derive(Serialize)]
struct OutputView {
    current: Option<String>,
    fit: Option<Fit>,
    revision: u64,
    background: Option<Background>,
    background_error: Option<Error>,
    pending: Option<String>,
    #[serde(flatten)]
    playback: Playback,
    effective_mute: bool,
    mute_reasons: Vec<&'static str>,
    effective_paused: bool,
    pause_reasons: Vec<&'static str>,
    kind: Option<catalog::Kind>,
    error: Option<Error>,
    renderer: &'static str,
    backend: Option<crate::domain::Backend>,
    pointer: crate::domain::PointerScope,
    properties_available: bool,
    media_available: bool,
    media_error: Option<String>,
    diagnostics: Option<String>,
    recovery_attempts: u32,
    properties: Values,
    property_warnings: BTreeMap<String, String>,
}

#[derive(Clone, Serialize)]
struct Background {
    current: String,
    revision: u64,
    path: std::path::PathBuf,
}

struct Pending {
    id: u64,
    group: u64,
    selection: Option<Selection>,
    kind: Option<catalog::Kind>,
    deadline: Instant,
    playback: Playback,
    properties: Resolved,
    rotation: Option<Rotation>,
    history_cursor: Option<usize>,
}

struct Control {
    id: u64,
    group: u64,
    deadline: Instant,
    playback: Playback,
}

struct PropertyControl {
    id: u64,
    selection_id: u64,
    group: u64,
    deadline: Instant,
    asset_id: String,
    properties: Resolved,
}

struct Output {
    // Acknowledged intent and selection; status is always derived from these facts.
    current: Option<String>,
    revision: u64,
    playback: Playback,
    kind: Option<catalog::Kind>,
    error: Option<Error>,
    properties: Values,
    property_warnings: BTreeMap<String, String>,
    worker: Option<Renderer>,
    frozen: Option<UnixStream>,
    recoverable: Option<Selection>,
    recovery: Recovery,
    watchdog: Watchdog,
    pending: Option<Pending>,
    control: Option<Control>,
    property_control: Option<PropertyControl>,
    selection_id: u64,
    snapshot_control: Option<SnapshotControl>,
    background: Option<Background>,
    background_error: Option<Error>,
    background_attempt: Option<u64>,
    background_due: Option<Instant>,
    runtime: WorkerRuntime,
    fullscreen: bool,
    battery: bool,
    pause_on_session: bool,
    auto_muted: bool,
    sent_clocks: BTreeMap<String, wallpaper_media::clock::Timeline>,
}

struct SnapshotControl {
    id: u64,
    clients: Vec<u64>,
    revision: u64,
    deadline: Instant,
}

#[derive(Default)]
struct Recovery {
    attempts: u32,
    due: Option<Instant>,
    healthy_since: Option<Instant>,
}

#[derive(Clone, Copy, Default)]
enum Watchdog {
    #[default]
    Idle,
    Progress(Instant),
    Probe(Instant),
}

impl Watchdog {
    const TIMEOUT: Duration = Duration::from_secs(10);

    fn accepted(&mut self, active: bool, now: Instant) {
        *self = if active {
            Self::Progress(now + Self::TIMEOUT)
        } else {
            Self::Idle
        };
    }

    fn runtime(&mut self, was_active: bool, active: bool, now: Instant) {
        if !active {
            *self = Self::Idle;
        } else if !was_active && matches!(self, Self::Idle) {
            *self = Self::Progress(now + Self::TIMEOUT);
        }
    }

    fn progress(&mut self, healthy_active: bool, now: Instant) {
        if healthy_active {
            *self = Self::Progress(now + Self::TIMEOUT);
        }
    }

    fn probe(&mut self) {
        if matches!(self, Self::Probe(_)) {
            *self = Self::Idle;
        }
    }

    fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Idle => None,
            Self::Progress(deadline) | Self::Probe(deadline) => Some(*deadline),
        }
    }
}

impl Recovery {
    fn schedule(&mut self, now: Instant) {
        if self.due.is_some() {
            return;
        }
        if self
            .healthy_since
            .take()
            .is_some_and(|start| now.saturating_duration_since(start) >= Duration::from_secs(60))
        {
            self.attempts = 0;
        }
        let delay = Duration::from_secs(1u64 << self.attempts.min(5));
        self.attempts = self.attempts.saturating_add(1);
        self.due = Some(now + delay);
    }

    fn accepted(&mut self) {
        self.due = None;
        self.healthy_since = Some(Instant::now());
    }
}

impl Output {
    fn background_deadline(&self) -> Option<Instant> {
        (self.current.is_some()
            && self.worker.is_some()
            && self.runtime.committed
            && !self.runtime.failed
            && self.pending.is_none()
            && self.property_control.is_none()
            && self.snapshot_control.is_none())
        .then_some(self.background_due)
        .flatten()
    }

    fn new(saved: &SavedOutput, defaults: Playback) -> Self {
        Self {
            current: None,
            revision: 0,
            playback: saved.playback(defaults),
            kind: None,
            error: None,
            properties: Values::new(),
            property_warnings: BTreeMap::new(),
            worker: None,
            frozen: None,
            recoverable: (!saved.released).then(|| saved.selection.clone()).flatten(),
            recovery: Recovery::default(),
            watchdog: Watchdog::default(),
            pending: None,
            control: None,
            property_control: None,
            selection_id: 0,
            snapshot_control: None,
            background: None,
            background_error: None,
            background_attempt: None,
            background_due: None,
            runtime: WorkerRuntime::default(),
            fullscreen: false,
            battery: false,
            pause_on_session: true,
            auto_muted: false,
            sent_clocks: BTreeMap::new(),
        }
    }

    fn policy(&self, session: Policy) -> PlaybackPolicy {
        PlaybackPolicy {
            battery: self.battery,
            session: if self.pause_on_session {
                session
            } else {
                Policy::default()
            },
            fullscreen: self.fullscreen,
            other_audio: self.auto_muted,
        }
    }

    fn view(&self, policy: Policy) -> OutputView {
        let policy = self.policy(policy);
        let actual = policy.commanded(self.playback).for_engine(
            self.runtime.throttled,
            self.runtime.failed,
            false,
        );
        OutputView {
            current: self.current.clone(),
            fit: self
                .recoverable
                .as_ref()
                .filter(|s| self.current.as_deref() == Some(&s.asset_id))
                .map(|s| s.fit),
            revision: self.revision,
            background: self.background.clone(),
            background_error: self.background_error.clone(),
            pending: self
                .pending
                .as_ref()
                .and_then(|p| p.selection.as_ref())
                .map(|s| s.asset_id.clone()),
            playback: self.playback,
            effective_mute: actual.mute,
            mute_reasons: policy.mute_reasons(self.playback),
            effective_paused: actual.paused,
            pause_reasons: policy.pause_reasons(self.playback, &self.runtime),
            kind: self.kind,
            error: self.error.clone(),
            renderer: if self.worker.is_some() {
                "running"
            } else {
                "stopped"
            },
            backend: self.runtime.backend,
            pointer: self.runtime.pointer,
            properties_available: self.runtime.properties,
            media_available: self.runtime.media,
            media_error: self.runtime.media_error.clone(),
            diagnostics: self.runtime.diagnostics.clone(),
            recovery_attempts: self.recovery.attempts,
            properties: self.properties.clone(),
            property_warnings: self.property_warnings.clone(),
        }
    }

    fn playback(&self) -> Playback {
        self.control
            .as_ref()
            .map(|c| c.playback)
            .or_else(|| self.pending.as_ref().map(|p| p.playback))
            .unwrap_or(self.playback)
    }

    fn effective(&self, playback: Playback, policy: Policy) -> Playback {
        self.policy(policy).commanded(playback)
    }

    fn actual(&self, policy: Policy) -> Playback {
        self.effective(self.playback, policy).for_engine(
            self.runtime.throttled,
            self.runtime.failed,
            false,
        )
    }

    fn wants_audio(&self, policy: Policy) -> bool {
        self.worker.is_some()
            && self.runtime.audio
            && !self.runtime.failed
            && !self.runtime.throttled
            && !self.effective(self.playback(), policy).paused
    }

    fn wants_media(&self) -> bool {
        self.runtime.media
            || self.pending.as_ref().is_some_and(|pending| {
                matches!(
                    pending.kind,
                    Some(catalog::Kind::WeScene | catalog::Kind::WeWeb)
                )
            })
    }

    fn preserve_worker_frame(&mut self) {
        // A compositor can apply the surface commit just before the worker dies,
        // without the following sync callback reaching the supervisor. An
        // accepted current selection makes that unconfirmed connection the only
        // possible last-frame owner. Never let it replace an older known frame.
        let allow_unconfirmed = self.frozen.is_none() && self.current.is_some();
        if let Some(connection) = self
            .worker
            .as_mut()
            .and_then(|worker| worker.preserve_frame(allow_unconfirmed))
        {
            self.frozen = Some(connection);
        }
    }

    fn retire_failed_worker(&mut self) {
        if !self.runtime.failed {
            return;
        }
        self.preserve_worker_frame();
        self.worker = None;
        self.runtime = WorkerRuntime::default();
        self.watchdog = Watchdog::Idle;
    }
}

struct Group {
    client: Option<u64>,
    remaining: usize,
    outputs: Vec<String>,
    current: Option<String>,
    error: Option<Error>,
}

struct Session {
    outputs: BTreeMap<String, Output>,
    groups: BTreeMap<u64, Group>,
    store: Store,
    config: Config,
    server: Server,
    next_id: u64,
    dirty: bool,
    save_needed: bool,
    completed: Vec<(Group, Result<(), Error>)>,
    policy: Policy,
    power: crate::policy::power::Snapshot,
    power_due: Option<Instant>,
    niri: crate::policy::niri::Snapshot,
    audio: crate::policy::audio::Snapshot,
    spectrum: we_scene::audio::AudioSnapshot,
    media: crate::media::Snapshot,
    audio_mute: AudioMute,
    rotations: BTreeMap<String, RotationRuntime>,
    catalog_names: BTreeMap<String, String>,
    clocks: BTreeMap<String, wallpaper_media::clock::Timeline>,
    synchronized: BTreeSet<String>,
}

#[derive(Default)]
struct RotationRuntime {
    schedule_active: Option<bool>,
    due: Option<Instant>,
    remaining: Option<Duration>,
    history: Vec<String>,
    cursor: usize,
    error: Option<Error>,
}

impl RotationRuntime {
    fn record(&mut self, asset: &str, cursor: Option<usize>) {
        if let Some(cursor) = cursor.filter(|&i| self.history.get(i).is_some_and(|id| id == asset))
        {
            self.cursor = cursor;
        } else if self.history.get(self.cursor).is_none_or(|id| id != asset) {
            self.history.truncate(self.cursor + 1);
            self.history.push(asset.into());
            if self.history.len() > 256 {
                self.history.remove(0);
            }
            self.cursor = self.history.len() - 1;
        }
    }

    fn history_step(&self, members: &[String], previous: bool) -> Option<usize> {
        if previous {
            (0..self.cursor)
                .rev()
                .find(|&i| members.contains(&self.history[i]))
        } else {
            (self.cursor + 1..self.history.len()).find(|&i| members.contains(&self.history[i]))
        }
    }

    fn reset_timer(&mut self, settings: &Rotation, paused: bool) {
        self.due = None;
        self.remaining = None;
        self.schedule_active = Some(settings.scheduled_now());
        if settings.enabled {
            let interval = Duration::from_secs(settings.interval_seconds);
            if paused {
                self.remaining = Some(interval);
            } else {
                self.due = Some(Instant::now() + interval);
            }
        }
    }
}

enum AudioMute {
    Clear,
    Active,
    Grace(Instant),
}

enum Source {
    Listener,
    Desktop,
    Client(u64),
    WorkerRead(String),
    WorkerWrite(String),
    Frozen(String),
    Policy,
    Power,
    Niri,
    Audio,
    Media,
    Spectrum,
}

pub fn serve() -> Result<()> {
    // Bind before starting any renderer, so a second daemon cannot paint the desktop.
    let server = Server::bind(socket_path())?;
    let mut desktop = Desktop::connect()?;
    let config = Config::load()?;
    // Evidence stays live so changing a policy never requires a daemon restart.
    let mut monitor = Some(session_policy::start()?);
    let mut niri_monitor = Some(crate::policy::niri::start()?);
    let mut audio_monitor = Some(crate::policy::audio::start()?);
    let mut power_watch = crate::policy::power::Watch::open().ok();
    let store = Store::load()?;
    let mut capture = crate::audio_capture::Capture::start()?;
    let mut media_monitor = if config.media_integration.unwrap_or(true) {
        Some(crate::media::Monitor::start()?)
    } else {
        None
    };
    let catalog_names = catalog::list(&config, &store.library)
        .iter()
        .filter(|asset| asset.error.is_none())
        .map(|a| (a.id.clone(), a.display_name().to_ascii_lowercase()))
        .collect();
    let mut session = Session {
        outputs: BTreeMap::new(),
        groups: BTreeMap::new(),
        store,
        config,
        server,
        next_id: 1,
        dirty: false,
        save_needed: false,
        completed: Vec::new(),
        policy: Policy::default(),
        power: crate::policy::power::read(),
        power_due: power_watch
            .is_none()
            .then(|| Instant::now() + Duration::from_secs(15)),
        niri: crate::policy::niri::Snapshot::default(),
        audio: crate::policy::audio::Snapshot::default(),
        spectrum: Default::default(),
        media: crate::media::Snapshot {
            enabled: media_monitor.is_some(),
            ..Default::default()
        },
        audio_mute: AudioMute::Clear,
        rotations: BTreeMap::new(),
        catalog_names,
        clocks: BTreeMap::new(),
        synchronized: BTreeSet::new(),
    };
    let mut sources = Vec::new();
    let mut fds = Vec::new();
    loop {
        if session.power_due.is_some_and(|due| due <= Instant::now()) {
            power_watch = crate::policy::power::Watch::open().ok();
            session.power_changed(crate::policy::power::read());
            session.power_due = power_watch
                .is_none()
                .then(|| Instant::now() + Duration::from_secs(15));
        }
        if let Some(outputs) = desktop.dispatch()? {
            session.reconcile(outputs);
        }
        session.expire();
        session.finish_turn();
        capture.set_active(session.needs_audio());
        if let Some(monitor) = &media_monitor {
            monitor.set_active(session.outputs.values().any(Output::wants_media));
        }
        let needs_write = ipc::flush_wayland(&desktop.connection)?;
        let Some(guard) = desktop.connection.prepare_read() else {
            continue;
        };
        sources.clear();
        sources.extend([Source::Listener, Source::Desktop]);
        fds.clear();
        fds.extend([
            ipc::interest(&session.server.listener, false),
            ipc::interest(&guard.connection_fd(), needs_write),
        ]);
        if let Some(monitor) = &monitor {
            sources.push(Source::Policy);
            fds.push(ipc::interest(&monitor.wake, false));
        }
        if let Some(watch) = &power_watch {
            sources.push(Source::Power);
            fds.push(ipc::interest(watch, false));
        }
        if let Some(monitor) = &niri_monitor {
            sources.push(Source::Niri);
            fds.push(ipc::interest(&monitor.wake, false));
        }
        if let Some(monitor) = &audio_monitor {
            sources.push(Source::Audio);
            fds.push(ipc::interest(&monitor.wake, false));
        }
        if let Some(monitor) = &media_monitor {
            sources.push(Source::Media);
            fds.push(ipc::interest(&monitor.wake, false));
        }
        sources.push(Source::Spectrum);
        fds.push(ipc::interest(&capture.wake, false));
        for (id, fd) in session.server.interests() {
            sources.push(Source::Client(id));
            fds.push(fd);
        }
        for (name, output) in &session.outputs {
            if let Some(connection) = &output.frozen {
                sources.push(Source::Frozen(name.clone()));
                fds.push(ipc::interest(connection, false));
            }
            if let Some(worker) = &output.worker {
                sources.push(Source::WorkerRead(name.clone()));
                fds.push(ipc::interest(&worker.output, false));
                if worker.wants_write() {
                    sources.push(Source::WorkerWrite(name.clone()));
                    fds.push(ipc::interest(&worker.input, true));
                }
            }
        }
        ipc::poll(&mut fds, session.timeout())?;
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            ipc::read_wayland(guard).context("reading desktop Wayland events")?;
            if let Some(outputs) = desktop.dispatch()? {
                session.reconcile(outputs);
            }
        } else {
            drop(guard);
        }
        // Client selections are processed before old renderer completions from this turn.
        for (source, fd) in sources.drain(..).zip(fds.drain(..)) {
            if fd.revents == 0 {
                continue;
            }
            match source {
                Source::Desktop => {}
                Source::Power => match power_watch.as_ref().map(|watch| watch.changed()) {
                    Some(Ok(true)) => session.power_changed(crate::policy::power::read()),
                    Some(Err(error)) => {
                        eprintln!("wallpaperd: power notifications: {error}");
                        power_watch = None;
                        session.power_due = Some(Instant::now() + Duration::from_secs(15));
                    }
                    _ => {}
                },
                Source::Policy => {
                    if let Some(monitor) = &mut monitor {
                        session.policy_changed(monitor.receive()?);
                    }
                }
                Source::Niri => {
                    if let Some(monitor) = &mut niri_monitor {
                        session.niri_changed(monitor.receive()?);
                    }
                }
                Source::Audio => {
                    if let Some(monitor) = &mut audio_monitor {
                        session.audio_changed(monitor.receive()?);
                    }
                }
                Source::Listener => session.server.accept()?,
                Source::Media => {
                    if let Some(monitor) = &mut media_monitor {
                        session.media_changed(monitor.receive()?);
                    }
                }
                Source::Spectrum => {
                    session.spectrum_changed(capture.receive()?);
                }
                Source::Client(id) => match session.server.ready(id, fd.revents) {
                    Ok(Some(request)) => session.request(id, request),
                    Ok(None) => {}
                    Err(error) => eprintln!("wallpaperd: client: {error:#}"),
                },
                Source::WorkerRead(name) => {
                    let result = session
                        .outputs
                        .get_mut(&name)
                        .and_then(|o| o.worker.as_mut())
                        .map(Renderer::read);
                    match result {
                        Some(Ok((replies, eof))) => {
                            for reply in replies {
                                session.worker_reply(&name, reply);
                            }
                            if eof {
                                session.worker_failed(&name, "render worker exited");
                            }
                        }
                        Some(Err(error)) => session.worker_failed(&name, &error.to_string()),
                        None => {}
                    }
                }
                Source::WorkerWrite(name) => {
                    if let Some(worker) = session
                        .outputs
                        .get_mut(&name)
                        .and_then(|o| o.worker.as_mut())
                        && let Err(error) = worker.flush()
                    {
                        session.worker_failed(&name, &error.to_string());
                    }
                }
                Source::Frozen(name) => {
                    if let Some(output) = session.outputs.get_mut(&name)
                        && let Some(connection) = &mut output.frozen
                    {
                        let mut bytes = [0; 4096];
                        // Ordinary read discards (and closes) any received
                        // ancillary descriptors; no protocol state is reused.
                        match connection.read(&mut bytes) {
                            Ok(0) => output.frozen = None,
                            Err(error)
                                if error.kind() != std::io::ErrorKind::WouldBlock
                                    && error.kind() != std::io::ErrorKind::Interrupted =>
                            {
                                output.frozen = None
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

impl Session {
    fn power_changed(&mut self, power: crate::policy::power::Snapshot) {
        if power != self.power {
            self.power = power;
            self.update_policy(self.policy);
        }
    }

    fn media_changed(&mut self, media: crate::media::Snapshot) {
        if self.media == media {
            return;
        }
        self.media = media;
        for output in self.outputs.values_mut() {
            if output.wants_media()
                && let Some(worker) = &mut output.worker
            {
                worker.set_media(&self.media);
            }
        }
        self.dirty = true;
    }

    fn snapshot(&self) -> Value {
        #[derive(Serialize)]
        struct Snapshot<'a> {
            api: u32,
            outputs: BTreeMap<&'a str, OutputView>,
            backends: Value,
            session_policy: Policy,
            policy_settings: crate::store::RuntimePolicy,
            power_policy: crate::policy::power::Snapshot,
            niri_policy: &'a crate::policy::niri::Snapshot,
            audio_policy: &'a crate::policy::audio::Snapshot,
            audio_spectrum: Value,
            media: &'a crate::media::Snapshot,
            library: &'a crate::library::Library,
            rotations: BTreeMap<&'a str, Value>,
        }
        serde_json::to_value(Snapshot {
            api: domain::API,
            backends: json!({
                "application": crate::application::capability(),
                "image": {"available":true,"content":["image"],"frame_output":["texture","shm"]},
                "libmpv": wallpaper_media::capability(),
                "cef": crate::web::capability(),
                "shader": {"available":true,"requires":"EGL/GLES 3","content":["shader"],"pause":true,"fps_limit":true,"input":true},
                "rust_scene": {
                    "available": !matches!(self.config.wallpaper_engine.scene_backend, crate::store::SceneBackend::Native),
                    "requires":"EGL/GLES 3, libshaderc",
                    "content":["we_scene"],
                    "scope":"scene image/text/particle/model layers, scripts, hierarchy, audio, embedded media and effects",
                    "properties":true,"input":true,"audio":true,"fbo_formats":["rgba8888","rgba_backbuffer","rg88","r8","rgba16f","rg16f","r16f","r32f","rg32f","rgba32f"],
                    "pause":true,"fps_limit":true,"frame_output":["texture"]
                },
                // Keep the legacy capability key for API 1 clients.
                "we": {"available":false,"content":["we_scene"],"video_available":wallpaper_media::capability()["available"],"video_backend":"libmpv","properties":false,"input":false,"synchronized_playback":false,"media":false,"error":"native WE backend has been removed; use rust_scene"},
                "coverage_pause": {"available":false,"reason":"niri IPC does not report opaque coverage"}
            }),
            session_policy: self.policy,
            power_policy: self.power,
            policy_settings: crate::store::RuntimePolicy {
                allow_applications: Some(self.store.policy.allow_applications.unwrap_or(false)),
                pause_on_battery: Some(self.store.policy.pause_on_battery.unwrap_or(false)),
                pause_on_session: Some(self.store.policy.pause_on_session.or(self.config.pause_on_session).unwrap_or(true)),
                pause_on_fullscreen: Some(self.store.policy.pause_on_fullscreen.or(self.config.pause_on_fullscreen).unwrap_or(true)),
                mute_on_other_audio: Some(self.store.policy.mute_on_other_audio.or(self.config.mute_on_other_audio).unwrap_or(true)),
            },
            niri_policy: &self.niri,
            audio_policy: &self.audio,
            audio_spectrum: json!({"available":self.spectrum.available,"capturing":self.spectrum.capturing,"device":self.spectrum.device,"sequence":self.spectrum.sequence}),
            media: &self.media,
            library: &self.store.library,
            rotations: self.outputs.keys().map(|name| {
                let settings = self.store.outputs.get(name).map(|o| o.rotation.clone()).unwrap_or_default();
                let error = self.rotations.get(name).and_then(|r| r.error.as_ref()).or(self.outputs[name].error.as_ref());
                let members = self.store.library.available_members(&settings.source, &self.catalog_names).unwrap_or_default();
                let runtime = self.rotations.get(name);
                let can_previous = settings.mode != crate::library::Mode::Random
                    || runtime.is_some_and(|r| r.history_step(&members, true).is_some());
                (name.as_str(), json!({"settings":settings,"members":members,"can_previous":can_previous,"error":error}))
            }).collect(),
            outputs: self
                .outputs
                .iter()
                .map(|(name, output)| (name.as_str(), output.view(self.policy)))
                .collect(),
        })
        .expect("serializing state")
    }

    fn reconcile(&mut self, present: BTreeSet<String>) {
        let removed: Vec<_> = self
            .outputs
            .keys()
            .filter(|name| !present.contains(*name))
            .cloned()
            .collect();
        for name in removed {
            self.rotations.remove(&name);
            if let Some(output) = self.outputs.remove(&name) {
                if let Some(control) = output.snapshot_control {
                    for client in control.clients {
                        self.server.reply(
                            client,
                            &fail(Error::new("output_missing", "output disconnected")),
                        );
                    }
                }
                for group in output
                    .pending
                    .map(|p| p.group)
                    .into_iter()
                    .chain(output.control.map(|c| c.group))
                    .chain(output.property_control.map(|c| c.group))
                {
                    self.complete(
                        group,
                        Err(Error::new("output_missing", "output disconnected")),
                    );
                }
            }
            self.dirty = true;
        }
        for name in present {
            if self.outputs.contains_key(&name) {
                continue;
            }
            let saved = self
                .store
                .outputs
                .get(&name)
                .cloned()
                .unwrap_or_else(|| SavedOutput {
                    paused: self.config.playback.paused,
                    ..SavedOutput::default()
                });
            let selection = self.initial_selection(&name, &saved);
            self.outputs
                .insert(name.clone(), Output::new(&saved, self.config.playback));
            {
                let output = self.outputs.get_mut(&name).unwrap();
                output.recoverable = selection.clone();
                output.battery =
                    self.store.policy.pause_on_battery.unwrap_or(false) && self.power.on_battery;
                output.pause_on_session = self
                    .store
                    .policy
                    .pause_on_session
                    .or(self.config.pause_on_session)
                    .unwrap_or(true);
                output.fullscreen = self
                    .store
                    .policy
                    .pause_on_fullscreen
                    .or(self.config.pause_on_fullscreen)
                    .unwrap_or(true)
                    && self.niri.fullscreen_outputs.contains(&name);
                output.auto_muted = self
                    .store
                    .policy
                    .mute_on_other_audio
                    .or(self.config.mute_on_other_audio)
                    .unwrap_or(true)
                    && !matches!(self.audio_mute, AudioMute::Clear);
            }
            self.dirty = true;
            if saved.rotation.enabled {
                self.rotations.insert(
                    name.clone(),
                    RotationRuntime {
                        due: Some(
                            Instant::now()
                                + Duration::from_secs(
                                    saved.rotation.interval_seconds.clamp(10, 86400),
                                ),
                        ),
                        ..RotationRuntime::default()
                    },
                );
            }
            if let Some(selection) = selection
                && let Err(error) = self.start(
                    None,
                    vec![name.clone()],
                    Some(selection),
                    Transition::Cut,
                    &PlaybackPatch::default(),
                    &Values::new(),
                    false,
                )
            {
                self.outputs.get_mut(&name).unwrap().error = Some(error);
            }
        }
    }

    fn initial_selection(&self, name: &str, saved: &SavedOutput) -> Option<Selection> {
        if saved.released {
            return None;
        }
        saved.selection.clone().or_else(|| {
            self.config.wallpaper_for(name).and_then(|path| {
                catalog::resolve(&path.to_string_lossy(), self.config.fit_for(name)).ok()
            })
        })
    }

    fn request(&mut self, client: u64, request: Request) {
        let output = request
            .params
            .get("output")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Err(error) = self.dispatch(client, request) {
            let mut value = fail(error);
            if let Some(output) = output {
                value["output"] = json!(output);
            }
            self.server.reply(client, &value);
        }
    }

    fn dispatch(&mut self, client: u64, request: Request) -> Result<(), Error> {
        if request.api != domain::API {
            return Err(Error::new("bad_api", "unsupported API version"));
        }
        match request.method.as_str() {
            "get" => self
                .server
                .reply(client, &json!({"ok":true,"state":self.snapshot()})),
            "subscribe" => self.server.subscribe(client, &self.snapshot()),
            "catalog" => {
                let assets = catalog::list(&self.config, &self.store.library);
                self.catalog_names = assets
                    .iter()
                    .filter(|asset| asset.error.is_none())
                    .map(|a| (a.id.clone(), a.display_name().to_ascii_lowercase()))
                    .collect();
                self.server
                    .reply(client, &json!({"ok":true,"assets":assets}));
            }
            "get_library" => self.server.reply(
                client,
                &json!({"ok":true,"library":self.store.library,
                "configured_sources":self.config.asset_dirs.iter()
                    .filter(|path| self.store.library.source_enabled(path)).collect::<Vec<_>>(),
                "automatic_sources":crate::catalog::automatic_sources()
                    .filter(|path| self.store.library.source_enabled(path)).collect::<Vec<_>>()}),
            ),
            "edit_library" => {
                let edit: Edit = serde_json::from_value(request.params)
                    .map_err(|e| Error::new("bad_request", e.to_string()))?;
                let sources_changed =
                    matches!(&edit, Edit::Import { .. } | Edit::RemoveSource { .. });
                let mut candidate = self.store.clone();
                let created = candidate.library.edit(edit)?;
                for output in candidate.outputs.values_mut() {
                    if !matches!(
                        output.rotation.source.as_str(),
                        "favorites" | "library" | "single"
                    ) && !candidate
                        .library
                        .playlists
                        .contains_key(&output.rotation.source)
                    {
                        output.rotation.enabled = false;
                        output.rotation.source = "single".into();
                    }
                }
                candidate
                    .save()
                    .map_err(|e| Error::new("save_failed", e.to_string()))?;
                self.store = candidate;
                if sources_changed {
                    self.catalog_names = catalog::list(&self.config, &self.store.library)
                        .iter()
                        .filter(|asset| asset.error.is_none())
                        .map(|a| (a.id.clone(), a.display_name().to_ascii_lowercase()))
                        .collect();
                }
                self.sync_rotation_timers();
                self.dirty = true;
                self.server.reply(
                    client,
                    &json!({"ok":true,"created":created,"library":self.store.library}),
                );
            }
            "set_rotation" => {
                let settings: Rotation = serde_json::from_value(request.params["rotation"].clone())
                    .map_err(|e| Error::new("bad_request", e.to_string()))?;
                settings.validate(&self.store.library)?;
                let outputs = self.targets(&request.params)?;
                let mut candidate = self.store.clone();
                for name in &outputs {
                    candidate.outputs.entry(name.clone()).or_default().rotation = settings.clone();
                }
                candidate
                    .save()
                    .map_err(|e| Error::new("save_failed", e.to_string()))?;
                let restart = request.params["restart"].as_bool().unwrap_or(true);
                for name in &outputs {
                    let previous = &self
                        .store
                        .outputs
                        .get(name)
                        .cloned()
                        .unwrap_or_default()
                        .rotation;
                    let runtime = self.rotations.entry(name.clone()).or_default();
                    if previous.source != settings.source {
                        *runtime = RotationRuntime::default();
                    }
                    if restart
                        || previous.enabled != settings.enabled
                        || previous.interval_seconds != settings.interval_seconds
                        || previous.source != settings.source
                    {
                        runtime
                            .reset_timer(&settings, self.outputs[name].actual(self.policy).paused);
                        if restart && settings.enabled && runtime.remaining.is_none() {
                            runtime.due = Some(Instant::now());
                        }
                    }
                    runtime.error = None;
                }
                self.store = candidate;
                self.dirty = true;
                self.server.reply(client, &success(outputs, None));
            }
            "rotation_next" | "rotation_previous" => {
                let outputs = self.targets(&request.params)?;
                if outputs.len() != 1 {
                    return Err(Error::new("bad_request", "next requires one output"));
                }
                self.step_rotation(
                    &outputs[0],
                    Some(client),
                    request.method == "rotation_previous",
                )?;
            }
            "set_policy" => {
                let policy: crate::store::RuntimePolicy = serde_json::from_value(request.params)
                    .map_err(|e| Error::new("bad_request", e.to_string()))?;
                let mut candidate = self.store.clone();
                candidate.policy = policy;
                candidate
                    .save()
                    .map_err(|e| Error::new("save_failed", e.to_string()))?;
                self.store = candidate;
                self.update_policy(self.policy);
                if !self.store.policy.allow_applications.unwrap_or(false) {
                    let outputs: Vec<_> = self
                        .outputs
                        .iter()
                        .filter(|(_, output)| {
                            matches!(
                                output.runtime.backend,
                                Some(crate::domain::Backend::Application)
                            ) || output
                                .pending
                                .as_ref()
                                .and_then(|pending| pending.selection.as_ref())
                                .is_some_and(|selection| {
                                    catalog::kind(catalog::path(selection))
                                        == Some(catalog::Kind::WeApplication)
                                })
                        })
                        .map(|(name, _)| name.clone())
                        .collect();
                    if !outputs.is_empty() {
                        self.start(
                            None,
                            outputs.clone(),
                            None,
                            Transition::Cut,
                            &PlaybackPatch::default(),
                            &Values::new(),
                            false,
                        )?;
                        self.stop_rotation(&outputs);
                    }
                }
                self.server
                    .reply(client, &json!({"ok":true,"policy":self.store.policy}));
            }
            "get_properties" => self.get_properties(client, &request.params)?,
            "set_properties" => self.set_properties(client, &request.params)?,
            "get_snapshot" => self.get_snapshot(Some(client), &request.params)?,
            "apply" => {
                let transition = serde_json::from_value(
                    request
                        .params
                        .get("transition")
                        .cloned()
                        .unwrap_or(json!(self.config.transition)),
                )
                .map_err(|_| {
                    Error::new(
                        "unsupported",
                        "supported transitions are cut, fade, disc, honeycomb, spiral and stripes",
                    )
                })?;
                let asset = request
                    .params
                    .get("asset_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::new("bad_request", "asset_id is required"))?;
                let fit = serde_json::from_value(
                    request
                        .params
                        .get("fit")
                        .cloned()
                        .unwrap_or(json!(self.config.fit)),
                )
                .map_err(|_| Error::new("bad_request", "invalid fit"))?;
                let selection = catalog::resolve(asset, fit)?;
                let outputs = self.targets(&request.params)?;
                let patch = self.playback_patch(&request.params, &outputs)?;
                let patch_properties = properties::patch(&request.params)?;
                let source = match request.params.get("source") {
                    Some(Value::String(source)) => {
                        if !self
                            .store
                            .library
                            .available_members(source, &self.catalog_names)?
                            .contains(&selection.asset_id)
                        {
                            return Err(Error::new(
                                "bad_request",
                                "wallpaper is not in this playback source",
                            ));
                        }
                        source.clone()
                    }
                    None => {
                        if self.catalog_names.contains_key(&selection.asset_id) {
                            "library".into()
                        } else {
                            "single".into()
                        }
                    }
                    Some(_) => return Err(Error::new("bad_request", "source must be a string")),
                };
                let rotations: BTreeMap<_, _> = outputs
                    .iter()
                    .map(|name| {
                        let mut settings = self
                            .store
                            .outputs
                            .get(name)
                            .cloned()
                            .unwrap_or_default()
                            .rotation;
                        settings.enabled = request.params.get("source").is_some()
                            && settings.source == source
                            && settings.enabled;
                        settings.source = source.clone();
                        settings.fit = if request.params.get("fit").is_none() {
                            self.config.fit_for(name)
                        } else {
                            fit
                        };
                        settings.transition = transition;
                        (name.clone(), settings)
                    })
                    .collect();
                let mut properties = self
                    .store
                    .library
                    .playlists
                    .get(&source)
                    .and_then(|playlist| playlist.properties.get(&selection.asset_id))
                    .map(|saved| {
                        Schema::load(&selection).map(|schema| schema.snapshot_patch(saved))
                    })
                    .transpose()?
                    .unwrap_or_default();
                properties.extend(patch_properties);
                self.start(
                    Some(client),
                    outputs.clone(),
                    Some(selection),
                    transition,
                    &patch,
                    &properties,
                    request.params.get("fit").is_none(),
                )?;
                for name in &outputs {
                    if let Some(pending) =
                        self.outputs.get_mut(name).and_then(|o| o.pending.as_mut())
                    {
                        pending.rotation = Some(rotations[name].clone());
                    }
                }
            }
            "release" => {
                let outputs = self.targets(&request.params)?;
                self.start(
                    Some(client),
                    outputs.clone(),
                    None,
                    Transition::Cut,
                    &PlaybackPatch::default(),
                    &Values::new(),
                    false,
                )?;
                self.stop_rotation(&outputs);
            }
            "set_playback" => {
                let outputs = self.targets(&request.params)?;
                let patch = self.playback_patch(&request.params, &outputs)?;
                if patch.empty() {
                    return Err(Error::new(
                        "bad_request",
                        "paused, mute, volume or fps is required",
                    ));
                }
                self.control(client, outputs, &patch);
            }
            _ => return Err(Error::new("bad_request", "unknown method")),
        }
        Ok(())
    }

    fn get_snapshot(&mut self, client: Option<u64>, params: &Value) -> Result<(), Error> {
        let outputs = self.targets(params)?;
        if outputs.len() != 1 {
            return Err(Error::new("bad_request", "snapshot requires one output"));
        }
        let entry = self.outputs.get_mut(&outputs[0]).unwrap();
        if entry.pending.is_some() || entry.property_control.is_some() {
            return Err(Error::new("busy", "output is processing another operation"));
        }
        if let Some(revision) = params.get("revision").filter(|v| !v.is_null()) {
            let revision = revision
                .as_u64()
                .ok_or_else(|| Error::new("bad_request", "revision must be an unsigned integer"))?;
            if revision != entry.revision {
                return Err(Error::new("stale_selection", "wallpaper revision changed"));
            }
        }
        if let Some(control) = &mut entry.snapshot_control {
            if control.revision != entry.revision {
                return Err(Error::new("busy", "output is exporting an older frame"));
            }
            control.clients.extend(client);
            return Ok(());
        }
        let worker = entry
            .worker
            .as_mut()
            .filter(|_| entry.current.is_some())
            .ok_or_else(|| Error::new("asset_unavailable", "no wallpaper is playing"))?;
        let id = self.next_id;
        self.next_id += 1;
        worker.snapshot(id, entry.selection_id);
        entry.snapshot_control = Some(SnapshotControl {
            id,
            clients: client.into_iter().collect(),
            revision: entry.revision,
            deadline: Instant::now() + Duration::from_secs(10),
        });
        Ok(())
    }

    fn stop_rotation(&mut self, outputs: &[String]) {
        for name in outputs {
            self.store
                .outputs
                .entry(name.clone())
                .or_default()
                .rotation
                .enabled = false;
            self.rotations.remove(name);
        }
        self.save_needed = true;
        self.dirty = true;
    }

    fn sync_rotation_timers(&mut self) {
        for (name, runtime) in &mut self.rotations {
            if !self
                .store
                .outputs
                .get(name)
                .is_some_and(|o| o.rotation.enabled)
            {
                runtime.due = None;
                runtime.remaining = None;
                runtime.error = None;
            }
        }
    }

    fn advance_rotation(&mut self, name: &str, client: Option<u64>) -> Result<(), Error> {
        self.step_rotation(name, client, false)
    }

    fn step_rotation(
        &mut self,
        name: &str,
        client: Option<u64>,
        previous: bool,
    ) -> Result<(), Error> {
        let output = &self.outputs[name];
        if output.pending.is_some() || output.control.is_some() || output.property_control.is_some()
        {
            return Err(Error::new("busy", "output is processing another operation"));
        }
        let settings = self
            .store
            .outputs
            .get(name)
            .map(|o| o.rotation.clone())
            .unwrap_or_default();
        settings.validate(&self.store.library)?;
        let members: Vec<_> = self
            .store
            .library
            .available_members(&settings.source, &self.catalog_names)?
            .into_iter()
            .filter(|id| {
                catalog::resolve(id, settings.fit).is_ok_and(|selection| {
                    catalog::kind(catalog::path(&selection)) != Some(catalog::Kind::WeApplication)
                        || self.store.policy.allow_applications.unwrap_or(false)
                })
            })
            .collect();
        let current = output.current.as_deref();
        let runtime = self.rotations.get(name);
        let history_cursor = (settings.mode == crate::library::Mode::Random)
            .then(|| runtime.and_then(|r| r.history_step(&members, previous)))
            .flatten();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64;
        let next = if let Some(cursor) = history_cursor {
            Some(runtime.unwrap().history[cursor].as_str())
        } else if previous {
            if settings.mode == crate::library::Mode::Random {
                return Err(Error::new("no_history", "还没有上一张壁纸"));
            }
            members
                .iter()
                .position(|id| Some(id.as_str()) == current)
                .and_then(|i| members.get((i + members.len() - 1) % members.len()))
                .or_else(|| members.last())
                .map(String::as_str)
        } else {
            settings.next(&members, current, seed)
        }
        .ok_or_else(|| Error::new("empty_playlist", "播放来源没有可用素材"))?
        .to_owned();
        let selection = catalog::resolve(&next, settings.fit)?;
        let properties = if let Some(saved) = self
            .store
            .library
            .playlists
            .get(&settings.source)
            .and_then(|playlist| playlist.properties.get(&next))
        {
            // A playlist snapshot restores defaults too; unknown authored fields stay stored.
            Schema::load(&selection)?.snapshot_patch(saved)
        } else {
            Values::new()
        };
        self.start(
            client,
            vec![name.into()],
            Some(selection),
            settings.transition,
            &PlaybackPatch::default(),
            &properties,
            false,
        )?;
        if let Some(pending) = self.outputs.get_mut(name).and_then(|o| o.pending.as_mut()) {
            pending.history_cursor = history_cursor;
        }
        self.dirty = true;
        Ok(())
    }

    fn targets(&self, params: &Value) -> Result<Vec<String>, Error> {
        match params.get("output") {
            Some(Value::String(name)) if self.outputs.contains_key(name) => Ok(vec![name.clone()]),
            Some(Value::String(name)) => Err(Error::new(
                "output_missing",
                format!("output {name} is not connected"),
            )),
            None | Some(Value::Null) if self.outputs.is_empty() => {
                Err(Error::new("output_missing", "no outputs are connected"))
            }
            None | Some(Value::Null) => Ok(self.outputs.keys().cloned().collect()),
            Some(_) => Err(Error::new("bad_request", "output must be a string")),
        }
    }

    fn property_selection(params: &Value) -> Result<Selection, Error> {
        let asset = params
            .get("asset_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("bad_request", "asset_id is required"))?;
        let selection = catalog::resolve(asset, Fit::Cover)?;
        if !matches!(selection.asset(), crate::domain::AssetRef::Project(_)) {
            return Err(Error::new("unsupported", "properties require a WE project"));
        }
        Ok(selection)
    }

    fn get_properties(&mut self, client: u64, params: &Value) -> Result<(), Error> {
        let selection = Self::property_selection(params)?;
        let schema = Schema::load(&selection)?;
        let patch = properties::patch(params)?;
        let names: BTreeSet<String> = match params.get("output") {
            Some(Value::String(name)) => {
                if !self.outputs.contains_key(name) && !self.store.outputs.contains_key(name) {
                    return Err(Error::new(
                        "output_missing",
                        format!("unknown output {name}"),
                    ));
                }
                BTreeSet::from([name.clone()])
            }
            None | Some(Value::Null) => self
                .outputs
                .keys()
                .chain(self.store.outputs.keys())
                .cloned()
                .collect(),
            Some(_) => return Err(Error::new("bad_request", "output must be a string")),
        };
        let mut outputs = BTreeMap::new();
        for name in names {
            let saved = self
                .store
                .outputs
                .get(&name)
                .and_then(|o| o.properties.get(&selection.asset_id))
                .cloned()
                .unwrap_or_default();
            let resolved = schema.resolve(&saved, &patch)?;
            let current = self
                .outputs
                .get(&name)
                .filter(|o| o.current.as_deref() == Some(&selection.asset_id));
            outputs.insert(name, json!({"values":resolved.values,"overrides":resolved.overrides,"visible":resolved.visible,"warnings":resolved.warnings,
                "current_values":current.map(|o| &o.properties),
                "available":current.is_some_and(|o| o.runtime.properties && !o.runtime.failed),
                "pending":current.is_some_and(|o| o.property_control.is_some())}));
        }
        let defaults = schema.resolve(&Values::new(), &Values::new())?;
        self.server.reply(client, &json!({"ok":true,"asset_id":selection.asset_id,"definitions":schema.definitions,"values":defaults.values,"visible":defaults.visible,"presets":self.store.library.presets.get(&selection.asset_id),"outputs":outputs}));
        Ok(())
    }

    fn set_properties(&mut self, client: u64, params: &Value) -> Result<(), Error> {
        let selection = Self::property_selection(params)?;
        if !matches!(
            catalog::kind(catalog::path(&selection)),
            Some(catalog::Kind::WeScene | catalog::Kind::WeWeb)
        ) {
            return Err(Error::new(
                "unsupported",
                "property updates require a WE scene or web project",
            ));
        }
        let schema = Schema::load(&selection)?;
        let mut patch = match params.get("preset") {
            None | Some(Value::Null) => Values::new(),
            Some(Value::String(name)) => {
                let preset = self
                    .store
                    .library
                    .presets
                    .get(&selection.asset_id)
                    .and_then(|presets| presets.get(name))
                    .ok_or_else(|| {
                        Error::new(
                            "preset_missing",
                            "preset no longer exists for this wallpaper",
                        )
                    })?;
                schema.snapshot_patch(preset)
            }
            Some(_) => return Err(Error::new("bad_request", "preset must be a name")),
        };
        patch.extend(properties::patch(params)?);
        if patch.is_empty() {
            return Err(Error::new(
                "bad_request",
                "choose a preset or at least one editable property",
            ));
        }
        let outputs = self.targets(params)?;
        let mut resolved = BTreeMap::new();
        for name in &outputs {
            let output = &self.outputs[name];
            if output.pending.is_some() {
                return Err(Error::new(
                    "busy",
                    format!("wallpaper is switching on {name}"),
                ));
            }
            if output.current.as_deref() != Some(&selection.asset_id) {
                return Err(Error::new(
                    "stale_selection",
                    format!("requested wallpaper is not current on {name}"),
                ));
            }
            if !output.runtime.properties || output.runtime.failed || output.worker.is_none() {
                return Err(Error::new(
                    "unsupported",
                    format!("current backend does not support properties on {name}"),
                ));
            }
            let saved = output
                .property_control
                .as_ref()
                .filter(|c| c.asset_id == selection.asset_id)
                .map(|c| c.properties.overrides.clone())
                .or_else(|| {
                    self.store
                        .outputs
                        .get(name)
                        .and_then(|o| o.properties.get(&selection.asset_id))
                        .cloned()
                })
                .unwrap_or_default();
            resolved.insert(name.clone(), schema.resolve(&saved, &patch)?);
        }
        let group = self.next_id;
        self.next_id += 1;
        self.groups.insert(
            group,
            Group {
                client: Some(client),
                remaining: outputs.len(),
                outputs: outputs.clone(),
                current: Some(selection.asset_id.clone()),
                error: None,
            },
        );
        for name in outputs {
            if let Some(previous) = self.outputs.get_mut(&name).unwrap().property_control.take() {
                self.complete(
                    previous.group,
                    Err(Error::new(
                        "superseded",
                        "a newer property setting replaced this request",
                    )),
                );
            }
            let id = self.next_id;
            self.next_id += 1;
            let output = self.outputs.get_mut(&name).unwrap();
            let properties = resolved.remove(&name).unwrap();
            output.worker.as_mut().unwrap().set_properties(
                id,
                output.selection_id,
                selection.asset_id.clone(),
                properties.values.clone(),
            );
            output.property_control = Some(PropertyControl {
                id,
                selection_id: output.selection_id,
                group,
                deadline: Instant::now() + Duration::from_secs(10),
                asset_id: selection.asset_id.clone(),
                properties,
            });
        }
        self.dirty = true;
        Ok(())
    }

    fn playback_patch(&self, params: &Value, outputs: &[String]) -> Result<PlaybackPatch, Error> {
        let patch: PlaybackPatch = serde_json::from_value(params.clone()).map_err(|_| {
            Error::new(
                "bad_request",
                "paused/mute must be booleans and volume/fps integers",
            )
        })?;
        for output in outputs {
            patch.update(self.outputs[output].playback())?;
        }
        Ok(patch)
    }

    fn control(&mut self, client: u64, outputs: Vec<String>, patch: &PlaybackPatch) {
        let group = self.next_id;
        self.next_id += 1;
        self.groups.insert(
            group,
            Group {
                client: Some(client),
                remaining: outputs.len(),
                outputs: outputs.clone(),
                current: None,
                error: None,
            },
        );
        for name in outputs {
            let playback = patch
                .update(self.outputs[&name].playback())
                .expect("validated playback");
            if let Some(previous) = self.outputs.get_mut(&name).unwrap().control.take() {
                self.complete(
                    previous.group,
                    Err(Error::new(
                        "superseded",
                        "a newer playback setting replaced this request",
                    )),
                );
            }
            let id = self.next_id;
            self.next_id += 1;
            let output = self.outputs.get_mut(&name).unwrap();
            if let Some(pending) = &mut output.pending {
                pending.playback = playback;
            }
            output.control = Some(Control {
                id,
                group,
                playback,
                deadline: Instant::now() + Duration::from_secs(10),
            });
            let effective = output.effective(playback, self.policy);
            if let Some(worker) = &mut output.worker {
                worker.set_playback(id, effective);
                if output.runtime.audio
                    && !effective.paused
                    && !output.runtime.failed
                    && !output.runtime.throttled
                {
                    worker.set_audio(&self.spectrum);
                }
            } else {
                self.worker_reply(
                    &name,
                    WorkerReply {
                        id,
                        ok: true,
                        error: None,
                        runtime: None,
                        snapshot: None,
                        progress: None,
                        probe: None,
                    },
                );
            }
        }
    }

    fn policy_changed(&mut self, policy: Policy) {
        if self.policy == policy {
            return;
        }
        let previous = self.policy;
        self.policy = policy;
        self.update_policy(previous);
    }

    fn niri_changed(&mut self, snapshot: crate::policy::niri::Snapshot) {
        if self.niri == snapshot {
            return;
        }
        self.niri = snapshot;
        self.update_policy(self.policy);
    }

    fn needs_audio(&self) -> bool {
        self.outputs
            .values()
            .any(|output| output.wants_audio(self.policy))
    }

    fn spectrum_changed(&mut self, snapshot: we_scene::audio::AudioSnapshot) {
        self.dirty |= (
            self.spectrum.available,
            self.spectrum.capturing,
            &self.spectrum.device,
        ) != (snapshot.available, snapshot.capturing, &snapshot.device);
        self.spectrum = snapshot;
        for output in self.outputs.values_mut() {
            if (output.wants_audio(self.policy) || output.runtime.audio && !self.spectrum.capturing)
                && let Some(worker) = &mut output.worker
            {
                worker.set_audio(&self.spectrum);
            }
        }
    }

    fn audio_changed(&mut self, snapshot: crate::policy::audio::Snapshot) {
        if self.audio == snapshot {
            return;
        }
        self.audio = snapshot;
        self.audio_mute = if snapshot.other_audio_active {
            AudioMute::Active
        } else if !snapshot.available || matches!(self.audio_mute, AudioMute::Clear) {
            AudioMute::Clear
        } else {
            AudioMute::Grace(Instant::now() + Duration::from_secs(1))
        };
        self.update_policy(self.policy);
    }

    fn update_policy(&mut self, previous_session: Policy) {
        for (name, output) in &mut self.outputs {
            let was_paused = output.actual(previous_session).paused;
            output.battery =
                self.store.policy.pause_on_battery.unwrap_or(false) && self.power.on_battery;
            output.pause_on_session = self
                .store
                .policy
                .pause_on_session
                .or(self.config.pause_on_session)
                .unwrap_or(true);
            output.fullscreen = self
                .store
                .policy
                .pause_on_fullscreen
                .or(self.config.pause_on_fullscreen)
                .unwrap_or(true)
                && self.niri.fullscreen_outputs.contains(name);
            output.auto_muted = self
                .store
                .policy
                .mute_on_other_audio
                .or(self.config.mute_on_other_audio)
                .unwrap_or(true)
                && !matches!(self.audio_mute, AudioMute::Clear);
            let playback = output.playback();
            let effective = output.effective(playback, self.policy);

            if was_paused && !output.actual(self.policy).paused && output.current.is_some() {
                output.revision = self.next_id;
                self.next_id += 1;
            }
            if let Some(worker) = &mut output.worker {
                // Keep a pending user command's id when replacing its queued policy update.
                let id = output.control.as_ref().map_or(0, |c| c.id);
                worker.set_playback(id, effective);
                if output.runtime.audio
                    && !effective.paused
                    && !output.runtime.failed
                    && !output.runtime.throttled
                {
                    worker.set_audio(&self.spectrum);
                }
            }
        }
        self.dirty = true;
    }

    // Keep the selection transaction inputs together at this internal boundary.
    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        client: Option<u64>,
        outputs: Vec<String>,
        selection: Option<Selection>,
        transition: Transition,
        patch: &PlaybackPatch,
        property_patch: &Values,
        configured_fit: bool,
    ) -> Result<(), Error> {
        let schema = selection
            .as_ref()
            .map(Schema::load)
            .transpose()?
            .unwrap_or_default();
        if !property_patch.is_empty()
            && selection.as_ref().is_none_or(|s| {
                !matches!(
                    catalog::kind(catalog::path(s)),
                    Some(catalog::Kind::WeScene | catalog::Kind::WeWeb)
                )
            })
        {
            return Err(Error::new(
                "unsupported",
                "property updates require a WE scene or web project",
            ));
        }
        let mut properties = BTreeMap::new();
        for name in &outputs {
            let saved = selection
                .as_ref()
                .and_then(|s| {
                    self.outputs[name]
                        .property_control
                        .as_ref()
                        .filter(|c| c.asset_id == s.asset_id)
                        .map(|c| &c.properties.overrides)
                        .or_else(|| {
                            self.store
                                .outputs
                                .get(name)
                                .and_then(|o| o.properties.get(&s.asset_id))
                        })
                })
                .cloned()
                .unwrap_or_default();
            properties.insert(name.clone(), schema.resolve(&saved, property_patch)?);
        }
        let group = self.next_id;
        self.next_id += 1;
        self.groups.insert(
            group,
            Group {
                client,
                remaining: outputs.len(),
                outputs: outputs.clone(),
                current: selection.as_ref().map(|s| s.asset_id.clone()),
                error: None,
            },
        );
        for name in outputs {
            let selection = selection.as_ref().map(|selection| Selection {
                fit: if configured_fit {
                    self.config.fit_for(&name)
                } else {
                    selection.fit
                },
                ..selection.clone()
            });
            let playback = patch
                .update(self.outputs[&name].playback())
                .expect("validated playback");
            let previous = self.outputs.get_mut(&name).unwrap().pending.take();
            if let Some(previous) = previous {
                self.complete(
                    previous.group,
                    Err(Error::new(
                        "superseded",
                        "a newer selection replaced this request",
                    )),
                );
            }
            let id = self.next_id;
            self.next_id += 1;
            let entry = self.outputs.get_mut(&name).unwrap();
            entry.watchdog = Watchdog::Idle;
            if client.is_some() || selection.is_none() {
                entry.recovery = Recovery::default();
            }
            if selection.is_none() {
                entry.frozen = None;
            }
            entry.recovery.due = None;
            let clock = selection.as_ref().and_then(|selection| {
                self.synchronized
                    .contains(&selection.asset_id)
                    .then(|| self.clocks[&selection.asset_id])
            });
            if selection.is_some() && entry.worker.is_none() {
                match Renderer::spawn(&name) {
                    Ok(worker) => {
                        entry.worker = Some(worker);
                    }
                    Err(error) => {
                        entry.error = Some(Error::new("renderer_failed", error.to_string()));
                        if entry.recoverable.is_some() {
                            entry.recovery.schedule(Instant::now());
                        }
                        let error = entry.error.clone().unwrap();
                        self.complete(group, Err(error));
                        self.dirty = true;
                        continue;
                    }
                }
            }
            entry.error = None;
            entry.pending = Some(Pending {
                id,
                group,
                selection: selection.clone(),
                kind: selection
                    .as_ref()
                    .and_then(|s| catalog::kind(catalog::path(s))),
                deadline: Instant::now()
                    + Duration::from_secs(if selection.is_some() { 60 } else { 10 }),
                playback,
                properties: properties.remove(&name).unwrap(),
                rotation: None,
                history_cursor: None,
            });

            let effective = entry.effective(playback, self.policy);
            if let Some(worker) = &mut entry.worker {
                worker.send(
                    id,
                    selection.clone(),
                    transition,
                    effective,
                    entry.pending.as_ref().unwrap().properties.values.clone(),
                );
                worker.set_media(&self.media);
                if let (Some(selection), Some(clock)) = (&selection, clock) {
                    worker.set_clock(selection.asset_id.clone(), clock);
                }
            } else {
                self.worker_reply(
                    &name,
                    WorkerReply {
                        id,
                        ok: true,
                        error: None,
                        runtime: None,
                        snapshot: None,
                        progress: None,
                        probe: None,
                    },
                );
            }
            self.dirty = true;
        }
        Ok(())
    }

    fn worker_reply(&mut self, name: &str, reply: WorkerReply) {
        let Some(entry) = self.outputs.get_mut(name) else {
            return;
        };
        if let Some(selection_id) = reply.progress {
            if selection_id == entry.selection_id && entry.pending.is_none() {
                entry.watchdog.progress(
                    entry.runtime.active && !entry.runtime.failed,
                    Instant::now(),
                );
            }
            return;
        }
        if let Some(selection_id) = reply.probe {
            if selection_id == entry.selection_id {
                entry.watchdog.probe();
            }
            return;
        }
        if entry
            .snapshot_control
            .as_ref()
            .is_some_and(|c| c.id == reply.id)
        {
            let control = entry.snapshot_control.take().unwrap();
            let value = if control.revision != entry.revision
                || entry.pending.is_some()
                || entry.property_control.is_some()
            {
                entry.background_attempt = None;
                fail(Error::new("stale_selection", "wallpaper revision changed"))
            } else if reply.ok
                && let Some(path) = reply.snapshot
            {
                entry.background = Some(Background {
                    current: entry.current.clone().unwrap(),
                    revision: control.revision,
                    path: path.clone(),
                });
                entry.background_error = None;
                entry.background_attempt = Some(control.revision);
                entry.background_due = None;
                json!({"ok":true,"output":name,"current":entry.current,"revision":control.revision,"path":path})
            } else {
                let error = reply.error.unwrap_or_else(|| {
                    Error::new("snapshot_failed", "worker did not export a frame")
                });
                entry.background_error = Some(error.clone());
                fail(error)
            };
            for client in control.clients {
                self.server.reply(client, &value);
            }
            self.dirty = true;
            return;
        }
        if reply.id == 0 {
            if let Some(runtime) = reply.runtime {
                if runtime.selection_id != entry.selection_id {
                    return;
                }
                let was_active = entry.runtime.active;
                entry.runtime = runtime;
                entry
                    .watchdog
                    .runtime(was_active, entry.runtime.active, Instant::now());
                if entry.runtime.committed {
                    entry.frozen = None;
                }
                if entry.runtime.failed && entry.pending.is_none() {
                    entry.recovery.schedule(Instant::now());
                }

                if entry.wants_audio(self.policy)
                    && let Some(worker) = &mut entry.worker
                {
                    worker.set_audio(&self.spectrum);
                }
                self.dirty = true;
            }
            if !reply.ok {
                entry.error = reply.error;
                self.dirty = true;
            }
            return;
        }
        if entry
            .property_control
            .as_ref()
            .is_some_and(|c| c.id == reply.id)
        {
            let control = entry.property_control.take().unwrap();
            let result = if reply.ok {
                if entry.selection_id == control.selection_id
                    && entry.current.as_deref() == Some(&control.asset_id)
                {
                    entry.properties = control.properties.values;
                    entry.property_warnings = control.properties.warnings;
                    entry.revision = control.id;
                }
                self.store
                    .outputs
                    .entry(name.into())
                    .or_default()
                    .accept_properties(&control.asset_id, control.properties.overrides);
                self.save_needed = true;
                Ok(())
            } else {
                Err(reply
                    .error
                    .unwrap_or_else(|| Error::new("property_failed", "property setting failed")))
            };
            if let Err(error) = &result {
                entry.error = Some(error.clone());
            } else if !entry.runtime.failed {
                entry.error = None;
            }
            self.complete(control.group, result);
            self.dirty = true;
            return;
        }
        if entry.control.as_ref().is_some_and(|c| c.id == reply.id) {
            let control = entry.control.take().unwrap();
            let result = if reply.ok {
                let was_paused = entry.actual(self.policy).paused;
                entry.playback = control.playback;
                if was_paused && !entry.actual(self.policy).paused {
                    entry.revision = control.id;
                }
                self.store
                    .outputs
                    .entry(name.into())
                    .or_default()
                    .accept_playback(control.playback);
                self.save_needed = true;
                Ok(())
            } else {
                Err(reply
                    .error
                    .unwrap_or_else(|| Error::new("playback_failed", "playback setting failed")))
            };
            if let Err(error) = &result {
                entry.error = Some(error.clone());
            } else if !entry.runtime.failed {
                entry.error = None;
            }
            self.complete(control.group, result);
            self.dirty = true;
            return;
        }
        if entry.pending.as_ref().is_none_or(|p| p.id != reply.id) {
            return;
        }
        let pending = entry.pending.take().unwrap();
        let mut released_control = None;
        let result = if reply.ok {
            entry.recovery.accepted();
            entry.selection_id = pending.id;
            if let Some(runtime) = reply.runtime {
                entry.runtime = runtime;
                if entry.runtime.committed {
                    entry.frozen = None;
                }
            }
            entry.revision = pending.id;
            let previous = entry.current.clone();
            entry.current = pending.selection.as_ref().map(|s| s.asset_id.clone());
            entry.kind = pending.kind;
            entry.playback = pending.playback;
            entry.recoverable = pending.selection.clone();
            entry
                .watchdog
                .accepted(entry.runtime.active, Instant::now());
            let saved = self.store.outputs.entry(name.into()).or_default();
            let source_changed = pending
                .rotation
                .as_ref()
                .is_some_and(|settings| settings.source != saved.rotation.source);
            if let Some(settings) = pending.rotation {
                saved.rotation = settings;
            }
            if let Some(current) = &entry.current {
                let runtime = self.rotations.entry(name.into()).or_default();
                if source_changed {
                    *runtime = RotationRuntime::default();
                } else if let Some(previous) = previous {
                    runtime.record(&previous, None);
                }
                runtime.record(current, pending.history_cursor);
                runtime.error = None;
                runtime.reset_timer(&saved.rotation, entry.actual(self.policy).paused);
            }
            entry.properties = pending.properties.values;
            entry.property_warnings = pending.properties.warnings;
            if let Some(selection) = &pending.selection {
                saved.accept_properties(&selection.asset_id, pending.properties.overrides);
            }
            saved.accept_playback(pending.playback);
            saved.released = pending.selection.is_none();
            saved.selection = pending.selection;
            if saved.released {
                if let Some(control) = entry.snapshot_control.take() {
                    for client in control.clients {
                        self.server.reply(
                            client,
                            &fail(Error::new("asset_unavailable", "wallpaper was released")),
                        );
                    }
                }
                released_control = entry.control.take();
                entry.worker = None;
                entry.frozen = None;
                entry.runtime = WorkerRuntime::default();
                entry.watchdog = Watchdog::Idle;
            }
            self.save_needed = true;
            Ok(())
        } else {
            if pending.selection.is_some() && entry.current.is_some() {
                entry.watchdog.accepted(
                    entry.runtime.active && !entry.runtime.failed,
                    Instant::now(),
                );
            }
            Err(reply
                .error
                .unwrap_or_else(|| Error::new("load_failed", "content loading failed")))
        };
        entry.error = result.as_ref().err().cloned();
        if (entry.runtime.failed || result.is_err() && entry.current.is_none())
            && entry.recoverable.is_some()
        {
            entry.recovery.schedule(Instant::now());
        }
        self.complete(pending.group, result);
        if let Some(control) = released_control {
            self.complete(control.group, Ok(()));
        }
        self.dirty = true;
    }

    fn worker_failed(&mut self, name: &str, message: &str) {
        let Some(entry) = self.outputs.get_mut(name) else {
            return;
        };
        // A release reply may already have removed the worker in this readiness batch.
        if entry.worker.is_none() {
            return;
        }
        // Every engine uses the same recovery policy. Never replay an unaccepted
        // candidate, or turn an explicit release into a wallpaper resurrection.
        if entry.pending.as_ref().is_none_or(|p| p.selection.is_some())
            && entry.recoverable.is_some()
        {
            entry.recovery.schedule(Instant::now());
        } else {
            entry.recovery.due = None;
        }
        entry.preserve_worker_frame();
        if entry.recovery.due.is_none() {
            entry.frozen = None;
        }
        entry.worker = None;
        entry.runtime = WorkerRuntime::default();
        entry.watchdog = Watchdog::Idle;
        entry.current = None;
        entry.kind = None;
        entry.properties.clear();
        entry.property_warnings.clear();
        let error = Error::new("renderer_failed", message);
        entry.error = Some(error.clone());
        if let Some(control) = entry.snapshot_control.take() {
            for client in control.clients {
                self.server.reply(client, &fail(error.clone()));
            }
        }
        let pending = entry.pending.take();
        let control = entry.control.take();
        let property_control = entry.property_control.take();
        if let Some(pending) = pending {
            self.complete(pending.group, Err(error.clone()));
        }
        if let Some(control) = control {
            self.complete(control.group, Err(error.clone()));
        }
        if let Some(control) = property_control {
            self.complete(control.group, Err(error));
        }
        self.dirty = true;
    }

    fn complete(&mut self, id: u64, result: Result<(), Error>) {
        let Some(group) = self.groups.get_mut(&id) else {
            return;
        };
        group.remaining -= 1;
        if group.error.is_none() {
            group.error = result.err();
        }
        if group.remaining == 0 {
            let group = self.groups.remove(&id).unwrap();
            let result = group.error.clone().map_or(Ok(()), Err);
            self.completed.push((group, result));
        }
    }

    fn sync_clocks(&mut self) {
        let now = wallpaper_media::clock::now();
        let mut members = BTreeMap::<String, BTreeSet<String>>::new();
        let mut running = BTreeMap::<String, bool>::new();
        for (name, output) in &self.outputs {
            if output.worker.is_none() {
                continue;
            }
            let playback = output.effective(output.playback(), self.policy);
            let assets = output.current.iter().chain(
                output
                    .pending
                    .iter()
                    .filter_map(|p| p.selection.as_ref().map(|s| &s.asset_id)),
            );
            for asset in assets {
                members
                    .entry(name.clone())
                    .or_default()
                    .insert(asset.clone());
                *running.entry(asset.clone()).or_default() |=
                    !playback.paused && !output.runtime.throttled && !output.runtime.failed;
            }
        }
        self.clocks.retain(|asset, _| running.contains_key(asset));
        self.synchronized
            .retain(|asset| running.contains_key(asset));
        for asset in running.keys() {
            if members
                .values()
                .filter(|assets| assets.contains(asset))
                .count()
                > 1
            {
                self.synchronized.insert(asset.clone());
            }
        }
        for (asset, running) in running {
            self.clocks
                .entry(asset)
                .or_insert_with(|| wallpaper_media::clock::Timeline::new(now, false))
                .set_running(now, running);
        }
        for (name, output) in &mut self.outputs {
            let assets = members.get(name);
            output
                .sent_clocks
                .retain(|asset, _| assets.is_some_and(|a| a.contains(asset)));
            for asset in assets.into_iter().flatten() {
                if !self.synchronized.contains(asset) {
                    continue;
                }
                let clock = self.clocks[asset];
                if output.sent_clocks.get(asset) != Some(&clock) {
                    if let Some(worker) = &mut output.worker {
                        worker.set_clock(asset.clone(), clock);
                    }
                    output.sent_clocks.insert(asset.clone(), clock);
                }
            }
        }
    }

    fn sync_backgrounds(&mut self) {
        let now = Instant::now();
        let mut ready = Vec::new();
        for (name, output) in &mut self.outputs {
            if output.current.is_none() {
                if output.recoverable.is_none() {
                    output.background = None;
                    output.background_error = None;
                    output.background_attempt = None;
                    output.background_due = None;
                }
                continue;
            }
            if output.background_attempt != Some(output.revision) {
                output.background_attempt = Some(output.revision);
                output.background_error = None;
                // ponytail: native WE acknowledges properties before painting them.
                // Retain the existing settling delay until it exposes frame revisions.
                let delay =
                    if output.background.as_ref().is_some_and(|background| {
                        Some(&background.current) == output.current.as_ref()
                    }) {
                        Duration::from_secs(5)
                    } else {
                        Duration::ZERO
                    };
                output.background_due = Some(now + delay);
            }
            if output.background_deadline().is_some_and(|due| due <= now) {
                output.background_due = None;
                ready.push(name.clone());
            }
        }
        for name in ready {
            if let Err(error) = self.get_snapshot(None, &json!({"output":name})) {
                self.outputs.get_mut(&name).unwrap().background_error = Some(error);
                self.dirty = true;
            }
        }
    }

    fn finish_turn(&mut self) {
        self.sync_clocks();
        self.sync_backgrounds();
        let save_error = if std::mem::take(&mut self.save_needed) {
            self.store
                .save()
                .err()
                .map(|e| Error::new("save_failed", e.to_string()))
        } else {
            None
        };
        if let Some(error) = &save_error {
            for (group, result) in &mut self.completed {
                if result.is_ok() {
                    *result = Err(error.clone());
                    for name in &group.outputs {
                        if let Some(output) = self.outputs.get_mut(name) {
                            output.error = Some(error.clone());
                        }
                    }
                }
            }
        }
        for (group, result) in self.completed.drain(..) {
            if let Some(client) = group.client {
                let value = match result {
                    Ok(()) => success(group.outputs, group.current),
                    Err(error) => {
                        let mut value = fail(error);
                        if let [output] = group.outputs.as_slice() {
                            value["output"] = json!(output);
                        }
                        value
                    }
                };
                self.server.reply(client, &value);
            }
        }
        if std::mem::take(&mut self.dirty) {
            self.server.publish(&self.snapshot());
        }
    }

    fn expire(&mut self) {
        self.server.expire();
        if matches!(self.audio_mute, AudioMute::Grace(deadline) if deadline <= Instant::now()) {
            self.audio_mute = AudioMute::Clear;
            self.update_policy(self.policy);
        }
        let expired: Vec<_> = self
            .outputs
            .iter()
            .filter(|(_, o)| {
                o.pending
                    .as_ref()
                    .is_some_and(|p| p.deadline <= Instant::now())
                    || o.control
                        .as_ref()
                        .is_some_and(|c| c.deadline <= Instant::now())
                    || o.property_control
                        .as_ref()
                        .is_some_and(|c| c.deadline <= Instant::now())
                    || o.snapshot_control
                        .as_ref()
                        .is_some_and(|c| c.deadline <= Instant::now())
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in expired {
            self.worker_failed(&name, "worker response timed out");
        }
        let now = Instant::now();
        for output in self.outputs.values_mut() {
            if output.pending.is_none()
                && matches!(output.watchdog, Watchdog::Progress(deadline) if deadline <= now)
                && let Some(worker) = &mut output.worker
            {
                worker.probe();
                output.watchdog = Watchdog::Probe(now + Watchdog::TIMEOUT);
            }
        }
        let stalled: Vec<_> = self
            .outputs
            .iter()
            .filter(|(_, output)| {
                output.pending.is_none()
                    && matches!(output.watchdog, Watchdog::Probe(deadline) if deadline <= now)
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in stalled {
            self.worker_failed(&name, "render worker stopped responding");
        }
        let recover: Vec<_> = self
            .outputs
            .iter()
            .filter(|(_, output)| {
                output.pending.is_none()
                    && output.recovery.due.is_some_and(|due| due <= Instant::now())
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in recover {
            let output = self.outputs.get_mut(&name).unwrap();
            output.recovery.due = None;
            output.retire_failed_worker();
            let selection = output.recoverable.clone();
            if let Some(selection) = selection
                && let Err(error) = self.start(
                    None,
                    vec![name.clone()],
                    Some(selection),
                    Transition::Cut,
                    &PlaybackPatch::default(),
                    &Values::new(),
                    false,
                )
            {
                let output = self.outputs.get_mut(&name).unwrap();
                output.error = Some(error);
                output.recovery.schedule(Instant::now());
                self.dirty = true;
            }
        }
        for (name, runtime) in &mut self.rotations {
            let settings = &self.store.outputs[name].rotation;
            if settings.enabled {
                if self.outputs[name].actual(self.policy).paused {
                    if let Some(due) = runtime.due.take() {
                        runtime.remaining = Some(due.saturating_duration_since(Instant::now()));
                    }
                    continue;
                } else if let Some(remaining) = runtime.remaining.take() {
                    runtime.due = Some(Instant::now() + remaining);
                }
            }
            if settings.enabled && !settings.schedule.is_empty() {
                let active = settings.scheduled_now();
                if active && runtime.schedule_active != Some(true) {
                    runtime.due = Some(Instant::now());
                }
                runtime.schedule_active = Some(active);
            }
        }
        let due: Vec<_> = self
            .rotations
            .iter()
            .filter(|(_, r)| r.due.is_some_and(|d| d <= Instant::now()))
            .map(|(name, _)| name.clone())
            .collect();
        for name in due {
            if !self.store.outputs[&name].rotation.scheduled_now() {
                self.rotations.get_mut(&name).unwrap().due =
                    Some(Instant::now() + Duration::from_secs(15));
                continue;
            }
            let interval = self.store.outputs[&name]
                .rotation
                .interval_seconds
                .clamp(10, 86400);
            // Restart from now after sleep; never catch up with a burst of switches.
            self.rotations.get_mut(&name).unwrap().due =
                Some(Instant::now() + Duration::from_secs(interval));
            if let Err(error) = self.advance_rotation(&name, None) {
                self.rotations.get_mut(&name).unwrap().error = Some(error);
                self.dirty = true;
            }
        }
    }

    fn timeout(&self) -> i32 {
        let deadline = self
            .outputs
            .values()
            .filter_map(|o| o.pending.as_ref().map(|p| p.deadline))
            .chain(
                self.outputs
                    .values()
                    .filter_map(|o| o.control.as_ref().map(|c| c.deadline)),
            )
            .chain(self.server.deadline())
            .chain(
                self.outputs
                    .values()
                    .filter_map(|o| o.snapshot_control.as_ref().map(|c| c.deadline)),
            )
            .chain(
                self.outputs
                    .values()
                    .filter_map(|o| o.property_control.as_ref().map(|c| c.deadline)),
            )
            .chain(match self.audio_mute {
                AudioMute::Grace(deadline) => Some(deadline),
                _ => None,
            })
            .chain(self.power_due)
            .chain(
                self.outputs
                    .values()
                    .filter_map(Output::background_deadline),
            )
            .chain(self.rotations.values().filter_map(|r| r.due))
            .chain(self.outputs.values().filter_map(|o| o.recovery.due))
            .chain(
                self.outputs
                    .values()
                    .filter(|o| o.pending.is_none())
                    .filter_map(|o| o.watchdog.deadline()),
            )
            .min();
        deadline.map_or(-1, |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .saturating_add(1)
                .min(i32::MAX as u128) as i32
        })
    }
}

fn fail(error: Error) -> Value {
    json!({"ok":false,"error":error})
}
fn success(outputs: Vec<String>, current: Option<String>) -> Value {
    let mut value = match outputs.as_slice() {
        [output] => json!({"ok":true,"output":output}),
        _ => json!({"ok":true,"outputs":outputs}),
    };
    if let Some(current) = current {
        value["current"] = json!(current);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_backs_off_until_a_stable_run() {
        let mut recovery = Recovery::default();
        let mut now = Instant::now();
        for seconds in [1, 2, 4, 8, 16, 32, 32] {
            recovery.schedule(now);
            assert_eq!(recovery.due, Some(now + Duration::from_secs(seconds)));
            let attempts = recovery.attempts;
            recovery.schedule(now);
            assert_eq!(recovery.attempts, attempts);
            now = recovery.due.take().unwrap();
        }
        recovery.healthy_since = Some(now);
        now += Duration::from_secs(60);
        recovery.schedule(now);
        assert_eq!(recovery.due, Some(now + Duration::from_secs(1)));
        assert_eq!(recovery.attempts, 1);
    }

    #[test]
    fn accepted_unconfirmed_frame_is_fallback_but_never_replaces_an_older_one() {
        use std::os::fd::AsRawFd;

        let mut output = Output::new(&SavedOutput::default(), Playback::default());
        output.current = Some("local:/accepted.png".into());
        let (worker, _peer) = Renderer::echo_connected();
        output.worker = Some(worker);
        output.runtime.failed = true;
        output.retire_failed_worker();
        assert!(output.frozen.is_some());

        let frozen_fd = output.frozen.as_ref().unwrap().as_raw_fd();
        let (replacement, _replacement_peer) = Renderer::echo_connected();
        output.worker = Some(replacement);
        output.runtime.failed = true;
        output.retire_failed_worker();
        assert_eq!(output.frozen.as_ref().unwrap().as_raw_fd(), frozen_fd);
    }

    #[test]
    fn watchdog_probes_once_and_only_real_progress_rearms_it() {
        let start = Instant::now();
        let mut watchdog = Watchdog::default();

        watchdog.accepted(false, start);
        assert!(
            watchdog.deadline().is_none(),
            "static content must stay idle"
        );

        watchdog.accepted(true, start);
        assert_eq!(watchdog.deadline(), Some(start + Watchdog::TIMEOUT));
        watchdog.runtime(true, true, start + Duration::from_secs(3));
        assert_eq!(
            watchdog.deadline(),
            Some(start + Watchdog::TIMEOUT),
            "periodic runtime metadata must not hide a render stall"
        );

        watchdog = Watchdog::Probe(start + Duration::from_secs(20));
        watchdog.runtime(true, true, start + Duration::from_secs(11));
        assert_eq!(
            watchdog.deadline(),
            Some(start + Duration::from_secs(20)),
            "runtime metadata must not extend a pending probe"
        );
        watchdog.probe();
        assert!(watchdog.deadline().is_none());
        watchdog.runtime(true, true, start + Duration::from_secs(21));
        assert!(
            watchdog.deadline().is_none(),
            "an acknowledged static engine must not be polled again"
        );

        watchdog.progress(false, start + Duration::from_secs(22));
        assert!(
            watchdog.deadline().is_none(),
            "late progress from paused or failed content must stay idle"
        );
        watchdog.progress(true, start + Duration::from_secs(22));
        assert_eq!(
            watchdog.deadline(),
            Some(start + Duration::from_secs(22) + Watchdog::TIMEOUT)
        );
        watchdog.runtime(true, false, start + Duration::from_secs(23));
        assert!(
            watchdog.deadline().is_none(),
            "pause must cancel the watchdog"
        );
    }

    #[test]
    fn media_updates_do_not_wake_image_video_or_shader_workers() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        for (name, media) in [
            ("image", false),
            ("video", false),
            ("shader", false),
            ("scene", true),
            ("web", true),
        ] {
            let mut output = Output::new(&SavedOutput::default(), Playback::default());
            output.worker = Some(Renderer::echo());
            output.runtime.media = media;
            session.outputs.insert(name.into(), output);
        }
        session.media_changed(crate::media::Snapshot {
            revision: 1,
            ..Default::default()
        });
        for (name, output) in &session.outputs {
            assert_eq!(
                output.worker.as_ref().unwrap().wants_write(),
                name == "scene" || name == "web"
            );
        }
    }

    #[test]
    fn backgrounds_follow_committed_revisions_and_discard_superseded_frames() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        let mut output = Output::new(&SavedOutput::default(), Playback::default());
        output.current = Some("local:/first.png".into());
        output.revision = 1;
        output.selection_id = 1;
        output.worker = Some(Renderer::echo());
        session.next_id = 2;
        session.outputs.insert("A".into(), output);

        session.sync_backgrounds();
        assert!(session.outputs["A"].snapshot_control.is_none());
        session.outputs.get_mut("A").unwrap().runtime.committed = true;
        session.sync_backgrounds();
        let first = session.outputs["A"].snapshot_control.as_ref().unwrap().id;
        session
            .get_snapshot(Some(7), &json!({"output":"A", "revision":1}))
            .unwrap();
        assert_eq!(
            session.outputs["A"]
                .snapshot_control
                .as_ref()
                .unwrap()
                .clients,
            [7]
        );
        let frame = |id, path: &str| WorkerReply {
            id,
            ok: true,
            snapshot: Some(path.into()),
            error: None,
            runtime: None,
            progress: None,
            probe: None,
        };
        session.worker_reply("A", frame(first, "/frames/first.png"));
        assert!(session.dirty);
        assert_eq!(
            session.snapshot()["outputs"]["A"]["background"],
            json!({
                "current":"local:/first.png", "revision":1, "path":"/frames/first.png"
            })
        );
        session.sync_backgrounds();
        assert!(session.outputs["A"].snapshot_control.is_none());
        assert!(session.outputs["A"].background_deadline().is_none());

        let output = session.outputs.get_mut("A").unwrap();
        output.current = Some("local:/second.png".into());
        output.revision = 10;
        output.selection_id = 10;
        session.sync_backgrounds();
        let stale = session.outputs["A"].snapshot_control.as_ref().unwrap().id;
        let output = session.outputs.get_mut("A").unwrap();
        output.current = Some("local:/latest.png".into());
        output.revision = 20;
        output.selection_id = 20;
        session.worker_reply("A", frame(stale, "/frames/stale.png"));
        assert_eq!(
            session.outputs["A"].background.as_ref().unwrap().revision,
            1
        );
        session.sync_backgrounds();
        let latest = session.outputs["A"].snapshot_control.as_ref().unwrap().id;
        session.worker_reply("A", frame(latest, "/frames/latest.png"));
        assert_eq!(
            session.outputs["A"].background.as_ref().unwrap().revision,
            20
        );

        // Changes to the same wallpaper settle without a recurring idle timer.
        session.outputs.get_mut("A").unwrap().revision = 30;
        session.sync_backgrounds();
        assert!(session.outputs["A"].snapshot_control.is_none());
        assert!(session.outputs["A"].background_deadline().unwrap() > Instant::now());
        session.outputs.get_mut("A").unwrap().background_due = Some(Instant::now());
        session.sync_backgrounds();
        let failed = session.outputs["A"].snapshot_control.as_ref().unwrap().id;
        let mut reply = frame(failed, "/frames/failed.png");
        reply.ok = false;
        reply.error = Some(Error::new("snapshot_failed", "fixture"));
        session.worker_reply("A", reply);
        assert_eq!(
            session.outputs["A"].background.as_ref().unwrap().revision,
            20
        );
        assert!(session.outputs["A"].background_error.is_some());
        session.sync_backgrounds();
        assert!(session.outputs["A"].snapshot_control.is_none());
        session
            .get_snapshot(Some(7), &json!({"output":"A"}))
            .unwrap();
        session
            .start(
                None,
                vec!["A".into()],
                None,
                Transition::Cut,
                &PlaybackPatch::default(),
                &Values::new(),
                false,
            )
            .unwrap();
        let release = session.outputs["A"].pending.as_ref().unwrap().id;
        let mut reply = frame(release, "/frames/unused.png");
        reply.snapshot = None;
        session.worker_reply("A", reply);
        session.sync_backgrounds();
        assert!(session.snapshot()["outputs"]["A"]["background"].is_null());
        assert!(session.outputs["A"].snapshot_control.is_none());
        assert!(session.outputs["A"].background_due.is_none());
    }

    #[test]
    fn stalled_snapshot_recovers_even_when_the_wallpaper_is_idle() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        let saved = SavedOutput {
            selection: Some(Selection {
                asset_id: "local:/accepted.png".into(),
                fit: Fit::Contain,
            }),
            ..Default::default()
        };
        let mut output = Output::new(&saved, Playback::default());
        output.current = Some(saved.selection.as_ref().unwrap().asset_id.clone());
        output.worker = Some(Renderer::echo());
        output.snapshot_control = Some(SnapshotControl {
            id: 1,
            clients: vec![7],
            revision: 0,
            deadline: Instant::now(),
        });
        assert!(!output.runtime.active);
        session.outputs.insert("A".into(), output);
        session.expire();
        let output = &session.outputs["A"];
        assert!(output.worker.is_none());
        assert!(output.snapshot_control.is_none());
        assert!(output.recovery.due.is_some());
        let recovered = output.recoverable.as_ref().unwrap();
        let accepted = saved.selection.as_ref().unwrap();
        assert_eq!(recovered.asset_id, accepted.asset_id);
        assert_eq!(recovered.fit, accepted.fit);
    }

    #[test]
    fn failed_workers_restore_accepted_intent_and_release_cancels_recovery() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        let selection = Selection {
            asset_id: "local:/accepted.png".into(),
            fit: Fit::Contain,
        };
        let saved = SavedOutput {
            selection: Some(selection.clone()),
            ..Default::default()
        };
        let mut output = Output::new(&saved, Playback::default());
        output.current = Some(selection.asset_id.clone());
        output.worker = Some(Renderer::echo());
        session.store.outputs.insert("A".into(), saved);
        session.outputs.insert("A".into(), output);
        session.worker_failed("A", "crashed decoder");
        assert_eq!(session.outputs["A"].recovery.attempts, 1);
        assert!(session.outputs["A"].recovery.due.is_some());
        let output = session.outputs.get_mut("A").unwrap();
        output.worker = Some(Renderer::echo());
        output.recovery.due = Some(Instant::now());
        session.expire();
        let restored = session.outputs["A"]
            .pending
            .as_ref()
            .unwrap()
            .selection
            .as_ref()
            .unwrap();
        assert_eq!(restored.asset_id, selection.asset_id);
        assert_eq!(restored.fit, selection.fit);
        assert_eq!(session.outputs["A"].recovery.due, None);
        session
            .start(
                Some(7),
                vec!["A".into()],
                None,
                Transition::Cut,
                &PlaybackPatch::default(),
                &Values::new(),
                false,
            )
            .unwrap();
        session.worker_failed("A", "exit during release");
        assert!(session.outputs["A"].recovery.due.is_none());
        assert_eq!(session.timeout(), -1, "released session has no timer work");
    }

    #[test]
    fn configured_initial_intent_retries_without_becoming_accepted() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        let selection = Selection {
            asset_id: "local:/configured.png".into(),
            fit: Fit::Cover,
        };
        let mut output = Output::new(&SavedOutput::default(), Playback::default());
        output.recoverable = Some(selection.clone());
        output.worker = Some(Renderer::echo());
        session.outputs.insert("A".into(), output);

        session.worker_failed("A", "failed before first acceptance");
        assert!(session.outputs["A"].recovery.due.is_some());
        assert!(!session.store.outputs.contains_key("A"));

        let output = session.outputs.get_mut("A").unwrap();
        output.worker = Some(Renderer::echo());
        output.recovery.due = Some(Instant::now());
        session.expire();
        let retried = session.outputs["A"]
            .pending
            .as_ref()
            .and_then(|pending| pending.selection.as_ref())
            .unwrap();
        assert_eq!(retried.asset_id, selection.asset_id);
        assert_eq!(retried.fit, selection.fit);
        assert!(!session.store.outputs.contains_key("A"));
    }

    #[test]
    fn runtime_failure_retires_worker_but_candidate_failure_keeps_it() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        let accepted = Selection {
            asset_id: "local:/accepted.png".into(),
            fit: Fit::Cover,
        };
        let saved = SavedOutput {
            selection: Some(accepted.clone()),
            ..Default::default()
        };
        let mut output = Output::new(&saved, Playback::default());
        output.current = Some(accepted.asset_id.clone());
        output.worker = Some(Renderer::echo());
        output.runtime.failed = true;
        let failed_pid = output.worker.as_ref().unwrap().pid();
        output.retire_failed_worker();
        assert!(output.worker.is_none());
        assert!(!output.runtime.failed);
        output.worker = Some(Renderer::echo());
        output.runtime.active = true;
        assert_ne!(output.worker.as_ref().unwrap().pid(), failed_pid);
        session.store.outputs.insert("A".into(), saved);
        session.outputs.insert("A".into(), output);

        session
            .start(
                Some(7),
                vec!["A".into()],
                Some(Selection {
                    asset_id: "local:/broken-candidate.png".into(),
                    fit: Fit::Contain,
                }),
                Transition::Cut,
                &PlaybackPatch::default(),
                &Values::new(),
                false,
            )
            .unwrap();
        let worker_pid = session.outputs["A"].worker.as_ref().unwrap().pid();
        let pending = session.outputs["A"].pending.as_ref().unwrap().id;
        session.worker_reply(
            "A",
            WorkerReply {
                id: pending,
                ok: false,
                error: Some(Error::new("load_failed", "fixture")),
                runtime: None,
                snapshot: None,
                progress: None,
                probe: None,
            },
        );
        assert_eq!(
            session.outputs["A"].worker.as_ref().unwrap().pid(),
            worker_pid
        );
        let recoverable = session.outputs["A"].recoverable.as_ref().unwrap();
        assert_eq!(recoverable.asset_id, accepted.asset_id);
        assert_eq!(recoverable.fit, accepted.fit);
        assert!(session.outputs["A"].recovery.due.is_none());
        assert!(matches!(
            session.outputs["A"].watchdog,
            Watchdog::Progress(_)
        ));

        session.outputs.get_mut("A").unwrap().runtime.active = false;
        session
            .start(
                Some(8),
                vec!["A".into()],
                Some(Selection {
                    asset_id: "local:/another-broken-candidate.png".into(),
                    fit: Fit::Cover,
                }),
                Transition::Cut,
                &PlaybackPatch::default(),
                &Values::new(),
                false,
            )
            .unwrap();
        let pending = session.outputs["A"].pending.as_ref().unwrap().id;
        session.worker_reply(
            "A",
            WorkerReply {
                id: pending,
                ok: false,
                error: Some(Error::new("load_failed", "paused fixture")),
                runtime: None,
                snapshot: None,
                progress: None,
                probe: None,
            },
        );
        assert!(matches!(session.outputs["A"].watchdog, Watchdog::Idle));
        assert!(session.outputs["A"].recovery.due.is_none());
    }

    #[test]
    fn configured_wallpapers_preserve_saved_choices_and_release() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        let global = root.path().join("global.png");
        let portrait = root.path().join("portrait.png");
        std::fs::write(&global, []).unwrap();
        std::fs::write(&portrait, []).unwrap();
        session.config.default = Some(global.clone());
        session.config.fit = Fit::Stretch;
        session.config.outputs.insert(
            "DP-1".into(),
            crate::store::OutputConfig {
                default: Some(portrait.clone()),
                fit: Some(Fit::Contain),
            },
        );
        session.config.outputs.insert(
            "DP-2".into(),
            crate::store::OutputConfig {
                fit: Some(Fit::Cover),
                ..Default::default()
            },
        );
        for (name, path, fit) in [
            ("DP-1", &portrait, Fit::Contain),
            ("DP-2", &global, Fit::Cover),
            ("unknown", &global, Fit::Stretch),
        ] {
            let selection = session
                .initial_selection(name, &SavedOutput::default())
                .unwrap();
            assert_eq!(catalog::path(&selection), path);
            assert_eq!(selection.fit, fit);
        }
        let mut saved = SavedOutput {
            selection: Some(catalog::resolve(&global.to_string_lossy(), Fit::Cover).unwrap()),
            ..Default::default()
        };
        let selection = session.initial_selection("DP-1", &saved).unwrap();
        assert_eq!(catalog::path(&selection), global);
        assert_eq!(selection.fit, Fit::Cover);
        saved.released = true;
        assert!(session.initial_selection("DP-1", &saved).is_none());
        saved.selection = None;
        assert!(session.initial_selection("DP-1", &saved).is_none());
    }

    #[test]
    fn apply_inherits_configured_fit_and_transition_and_accepts_overrides() {
        let root = tempfile::tempdir().unwrap();
        let asset = root.path().join("wallpaper.png");
        std::fs::write(&asset, []).unwrap();
        let mut session = session(root.path());
        session.config.fit = Fit::Stretch;
        session.config.transition = Transition::Fade;
        session.config.outputs.insert(
            "DP-1".into(),
            crate::store::OutputConfig {
                fit: Some(Fit::Contain),
                ..Default::default()
            },
        );
        for name in ["DP-1", "DP-2"] {
            let mut output = Output::new(&SavedOutput::default(), Playback::default());
            output.worker = Some(Renderer::echo());
            session.outputs.insert(name.into(), output);
        }
        for (params, fits, transition) in [
            (
                json!({"asset_id":asset}),
                [Fit::Contain, Fit::Stretch],
                Transition::Fade,
            ),
            (
                json!({"asset_id":asset,"fit":"cover","transition":"cut"}),
                [Fit::Cover; 2],
                Transition::Cut,
            ),
        ] {
            session
                .dispatch(
                    1,
                    Request {
                        api: domain::API,
                        method: "apply".into(),
                        params,
                    },
                )
                .unwrap();
            assert_eq!(
                session.groups.len(),
                1,
                "multi-output apply must retain one response group"
            );
            for (name, fit) in ["DP-1", "DP-2"].into_iter().zip(fits) {
                let entry = session.outputs.get_mut(name).unwrap();
                let id = entry.pending.as_ref().unwrap().id;
                assert_eq!(
                    entry
                        .pending
                        .as_ref()
                        .unwrap()
                        .selection
                        .as_ref()
                        .unwrap()
                        .fit,
                    fit
                );
                let worker = entry.worker.as_mut().unwrap();
                worker.flush().unwrap();
                let mut lines = ipc::Lines::default();
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    assert!(Instant::now() < deadline, "worker did not receive apply");
                    let mut fds = [ipc::interest(&worker.output, false)];
                    ipc::poll(&mut fds, 100).unwrap();
                    let commands = lines.read(&mut worker.output).unwrap().0;
                    if let Some(command) = commands.into_iter().find_map(|line| {
                        let command: crate::domain::WorkerCommand =
                            serde_json::from_slice(&line).unwrap();
                        (command.id == id).then_some(command)
                    }) {
                        let crate::domain::WorkerAction::Apply {
                            selection,
                            transition: actual,
                            ..
                        } = command.action
                        else {
                            panic!("expected apply");
                        };
                        assert_eq!(selection.fit, fit);
                        assert_eq!(actual, transition);
                        break;
                    }
                }
                session.worker_reply(
                    name,
                    WorkerReply {
                        id,
                        ok: true,
                        error: None,
                        runtime: None,
                        snapshot: None,
                        progress: None,
                        probe: None,
                    },
                );
                assert_eq!(
                    session.store.outputs[name].selection.as_ref().unwrap().fit,
                    fit
                );
            }
        }
        for params in [
            json!({"asset_id":asset,"fit":"invalid"}),
            json!({"asset_id":asset,"transition":"invalid"}),
        ] {
            assert!(
                session
                    .dispatch(
                        1,
                        Request {
                            api: domain::API,
                            method: "apply".into(),
                            params
                        }
                    )
                    .is_err()
            );
            assert!(
                session
                    .outputs
                    .values()
                    .all(|output| output.pending.is_none())
            );
        }
    }

    #[test]
    fn replacements_late_observations_failure_release_and_hotplug_preserve_intent() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        for name in ["A", "B"] {
            let mut output = Output::new(&SavedOutput::default(), Playback::default());
            output.worker = Some(Renderer::echo());
            session.outputs.insert(name.into(), output);
        }
        let selection = |name: &str| Selection {
            asset_id: format!("local:/{name}.png"),
            fit: Fit::Cover,
        };
        let start = |session: &mut Session, names: &[&str], asset: &str| {
            session
                .start(
                    None,
                    names.iter().map(|name| (*name).into()).collect(),
                    Some(selection(asset)),
                    Transition::Cut,
                    &PlaybackPatch::default(),
                    &Values::new(),
                    false,
                )
                .unwrap();
        };
        let ack = |id, ok| WorkerReply {
            id,
            ok,
            error: (!ok).then(|| Error::new("load_failed", "fixture")),
            runtime: None,
            snapshot: None,
            progress: None,
            probe: None,
        };
        start(&mut session, &["A", "B"], "old");
        for name in ["A", "B"] {
            let id = session.outputs[name].pending.as_ref().unwrap().id;
            session.worker_reply(name, ack(id, true));
        }
        start(&mut session, &["A"], "first");
        let stale = session.outputs["A"].pending.as_ref().unwrap().id;
        start(&mut session, &["A"], "last");
        let latest = session.outputs["A"].pending.as_ref().unwrap().id;
        session.worker_reply("A", ack(stale, true));
        assert_eq!(
            session.outputs["A"].current.as_deref(),
            Some("local:/old.png")
        );
        session.worker_reply("A", ack(latest, false));
        assert_eq!(
            session.store.outputs["A"]
                .selection
                .as_ref()
                .unwrap()
                .asset_id,
            "local:/old.png"
        );
        assert_eq!(
            session.outputs["B"].current.as_deref(),
            Some("local:/old.png")
        );
        start(&mut session, &["A"], "accepted");
        let latest = session.outputs["A"].pending.as_ref().unwrap().id;
        let mut reply = ack(latest, true);
        reply.runtime = Some(WorkerRuntime {
            selection_id: latest,
            properties: true,
            ..Default::default()
        });
        session.worker_reply("A", reply);
        session.worker_reply(
            "A",
            WorkerReply {
                id: 0,
                runtime: Some(WorkerRuntime {
                    selection_id: stale,
                    failed: true,
                    ..Default::default()
                }),
                ..ack(0, true)
            },
        );
        assert!(session.outputs["A"].runtime.properties);
        assert!(!session.outputs["A"].runtime.failed);
        assert!(session.completed.iter().any(|(_, result)| {
            result
                .as_ref()
                .is_err_and(|error| error.code == "superseded")
        }));
        start(&mut session, &["A"], "unconfirmed");
        session.reconcile(BTreeSet::from(["B".into()]));
        assert_eq!(
            session.store.outputs["A"]
                .selection
                .as_ref()
                .unwrap()
                .asset_id,
            "local:/accepted.png"
        );
        assert!(session.completed.iter().any(|(_, result)| {
            result
                .as_ref()
                .is_err_and(|error| error.code == "output_missing")
        }));
        session
            .start(
                None,
                vec!["B".into()],
                None,
                Transition::Cut,
                &PlaybackPatch::default(),
                &Values::new(),
                false,
            )
            .unwrap();
        let id = session.outputs["B"].pending.as_ref().unwrap().id;
        session.worker_reply("B", ack(id, true));
        assert!(session.outputs["B"].worker.is_none());
        assert!(session.store.outputs["B"].released);
        assert!(session.store.outputs["B"].selection.is_none());
    }

    #[test]
    fn policy_resume_revision_and_worker_exit_have_no_stale_pause_causes() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        let mut output = Output::new(&SavedOutput::default(), Playback::default());
        output.worker = Some(Renderer::echo());
        output.current = Some("local:/test.png".into());
        session.outputs.insert("A".into(), output);
        session.policy_changed(Policy {
            locked: true,
            ..Default::default()
        });
        assert!(session.outputs["A"].actual(session.policy).paused);
        session.policy_changed(Policy::default());
        assert!(session.outputs["A"].revision > 0);
        session.outputs.get_mut("A").unwrap().runtime.failed = true;
        session.worker_failed("A", "fixture exit");
        assert!(
            session.outputs["A"]
                .view(session.policy)
                .pause_reasons
                .is_empty()
        );
        assert!(
            session.outputs["A"]
                .error
                .as_ref()
                .is_some_and(|error| error.code == "renderer_failed")
        );
    }

    #[test]
    #[ignore = "starts private PipeWire/Pulse; verifies daemon fanout to two bounded worker pipes"]
    fn daemon_audio_two_workers_share_capture_latest_fft_pause_mute_and_exit() {
        use crate::domain::{WorkerAction, WorkerCommand};
        use std::{io::Write, os::fd::AsRawFd, thread};
        let root = tempfile::tempdir().unwrap();
        let server = crate::audio_capture::tests::Server::new();
        let pcm = root.path().join("stereo.f32");
        let mut file = std::fs::File::create(&pcm).unwrap();
        for sample in 0..48000 * 8 {
            for (hz, gain) in [(750.0, 0.01), (6000.0, 0.1)] {
                let value = gain * (std::f32::consts::TAU * hz * sample as f32 / 48000.0).sin();
                file.write_all(&value.to_le_bytes()).unwrap();
            }
        }
        drop(file);
        let _play = server.play(&pcm);
        let mut session = session(root.path());
        for name in ["DP-1", "DP-2"] {
            let mut output = Output::new(&SavedOutput::default(), Playback::default());
            output.worker = Some(Renderer::echo());
            output.runtime.audio = true;
            output.current = Some("we:/reactive".into());
            session.outputs.insert(name.into(), output);
        }
        // Backpressure is deterministic and cannot hide in a large kernel pipe.
        let slow = session.outputs["DP-2"].worker.as_ref().unwrap();
        let slow_pid = slow.pid() as i32;
        // SAFETY: signal only this test's owned child; Renderer::drop kills and reaps it on failure.
        assert_eq!(unsafe { libc::kill(slow_pid, libc::SIGSTOP) }, 0);
        for fd in [slow.input.as_raw_fd(), slow.output.as_raw_fd()] {
            // SAFETY: resize only the owned test pipe, using a valid Linux fcntl operation.
            assert!(unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, 4096) } >= 0);
        }
        let mut capture =
            crate::audio_capture::Capture::start_with_server(Some(server.address())).unwrap();
        assert!(server.source_outputs().as_array().unwrap().is_empty());
        capture.set_active(session.needs_audio());
        let mut lines = ipc::Lines::default();
        let mut fast = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(6);
        while fast.len() < 35 {
            assert!(
                Instant::now() < deadline,
                "two-worker spectrum did not progress"
            );
            let snapshot = capture.receive().unwrap();
            if snapshot.available {
                let sequence = snapshot.sequence;
                session.spectrum_changed(snapshot);
                for output in session.outputs.values_mut() {
                    output.worker.as_mut().unwrap().flush().unwrap();
                }
                let worker = session
                    .outputs
                    .get_mut("DP-1")
                    .unwrap()
                    .worker
                    .as_mut()
                    .unwrap();
                for line in lines.read(&mut worker.output).unwrap().0 {
                    let command: WorkerCommand = serde_json::from_slice(&line).unwrap();
                    if let WorkerAction::SetAudio { audio } = command.action {
                        assert!(audio.valid());
                        assert!(audio.sequence <= sequence);
                        fast.push(*audio);
                    }
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            server.source_outputs().as_array().unwrap().len(),
            1,
            "outputs opened independent monitors"
        );
        assert!(
            fast.last().unwrap().sequence > fast[0].sequence + 20,
            "slow worker blocked FFT"
        );
        let peak = |v: &[f32]| v.iter().copied().reduce(f32::max).unwrap();
        let latest = fast.last().unwrap();
        assert!(
            peak(&latest.bands[2].left) > 0.1
                && peak(&latest.bands[2].left) < peak(&latest.bands[2].right),
            "quiet PCM must reach workers as a visible scene response"
        );
        assert!(peak(&latest.bands[2].right) > 0.8);

        let wanted = session.spectrum.sequence;
        // SAFETY: resume the same test-owned consumer so it can drain its bounded input.
        assert_eq!(unsafe { libc::kill(slow_pid, libc::SIGCONT) }, 0);
        let slow = session
            .outputs
            .get_mut("DP-2")
            .unwrap()
            .worker
            .as_mut()
            .unwrap();
        let mut lines = ipc::Lines::default();
        let mut slow_values = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while slow_values
            .last()
            .is_none_or(|a: &we_scene::audio::AudioSnapshot| a.sequence != wanted)
        {
            assert!(
                Instant::now() < deadline,
                "latest spectrum remained behind history"
            );
            slow.flush().unwrap();
            for line in lines.read(&mut slow.output).unwrap().0 {
                let command: WorkerCommand = serde_json::from_slice(&line).unwrap();
                if let WorkerAction::SetAudio { audio } = command.action {
                    slow_values.push(*audio);
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            slow_values.len() < 10,
            "slow worker accumulated {} FFT snapshots",
            slow_values.len()
        );
        assert_eq!(
            slow_values.last().unwrap(),
            &session.spectrum,
            "workers received different FFT/channel data"
        );
        session.outputs.get_mut("DP-1").unwrap().playback.paused = true;
        session.outputs.get_mut("DP-2").unwrap().playback.mute = true;
        assert!(
            session.needs_audio(),
            "wallpaper mute disabled other-application response"
        );
        session.outputs.get_mut("DP-2").unwrap().runtime.throttled = true;
        assert!(
            !session.needs_audio(),
            "suspended outputs kept capture alive"
        );
        session.outputs.get_mut("DP-2").unwrap().runtime.throttled = false;
        session.outputs.get_mut("DP-2").unwrap().playback.paused = true;
        capture.set_active(session.needs_audio());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !server.source_outputs().as_array().unwrap().is_empty() {
            assert!(
                Instant::now() < deadline,
                "all-paused daemon retained capture"
            );
            thread::sleep(Duration::from_millis(20));
        }
        session.spectrum_changed(capture.receive().unwrap());
        assert!(!session.spectrum.capturing);
        assert_eq!(peak(&session.spectrum.bands[2].average), 0.);
        session.outputs.get_mut("DP-2").unwrap().playback.paused = false;
        assert!(session.needs_audio());
        session.outputs.remove("DP-2");
        assert!(
            !session.needs_audio(),
            "consumer exit retained capture demand"
        );
        drop(capture);
        assert!(server.source_outputs().as_array().unwrap().is_empty());
    }

    fn session(root: &std::path::Path) -> Session {
        Session {
            outputs: BTreeMap::new(),
            groups: BTreeMap::new(),
            store: Store::default(),
            config: Config::default(),
            server: Server::bind(root.join("daemon.sock")).unwrap(),
            next_id: 1,
            dirty: false,
            save_needed: false,
            completed: Vec::new(),
            policy: Policy::default(),
            power: Default::default(),
            power_due: None,
            niri: Default::default(),
            audio: Default::default(),
            spectrum: Default::default(),
            media: Default::default(),
            audio_mute: AudioMute::Clear,
            rotations: BTreeMap::new(),
            catalog_names: BTreeMap::new(),
            clocks: BTreeMap::new(),
            synchronized: BTreeSet::new(),
        }
    }

    #[test]
    fn same_wallpaper_automatically_shares_clock_and_rejoins_after_pause() {
        let temp = tempfile::tempdir().unwrap();
        let mut session = session(temp.path());
        let output = || {
            let mut output = Output::new(&SavedOutput::default(), Playback::default());
            output.worker = Some(Renderer::echo());
            output.current = Some("local:/same.frag".into());
            output
        };
        session.outputs.insert("DP-1".into(), output());
        session.sync_clocks();
        assert!(
            session.synchronized.is_empty(),
            "single-screen playback remains native"
        );
        let original = session.clocks["local:/same.frag"];
        session.outputs.insert("DP-2".into(), output());
        session.sync_clocks();
        assert_eq!(
            session.clocks["local:/same.frag"], original,
            "late join must not restart playback"
        );
        assert_eq!(
            session.outputs["DP-1"].sent_clocks,
            session.outputs["DP-2"].sent_clocks
        );
        session.outputs.get_mut("DP-1").unwrap().playback.paused = true;
        session.sync_clocks();
        assert!(session.clocks["local:/same.frag"].running);
        session.outputs.get_mut("DP-2").unwrap().playback.paused = true;
        session.sync_clocks();
        let frozen = session.clocks["local:/same.frag"];
        assert!(!frozen.running);
        session.outputs.get_mut("DP-2").unwrap().playback.paused = false;
        session.sync_clocks();
        assert_eq!(
            session.clocks["local:/same.frag"].position_ns,
            frozen.position_ns
        );
        assert!(session.clocks["local:/same.frag"].running);
        session.outputs.get_mut("DP-1").unwrap().current = Some("local:/different.frag".into());
        session.sync_clocks();
        assert!(
            !session.outputs["DP-1"]
                .sent_clocks
                .contains_key("local:/same.frag")
        );
        assert!(!session.synchronized.contains("local:/different.frag"));
        session.outputs.remove("DP-2");
        session.sync_clocks();
        assert!(!session.clocks.contains_key("local:/same.frag"));
    }

    #[test]
    fn playback_during_loading_preserves_pending_fps_and_selection() {
        let mut output = Output::new(&SavedOutput::default(), Playback::default());
        output.pending = Some(Pending {
            id: 4,
            group: 3,
            selection: Some(Selection {
                asset_id: "local:/video.mkv".into(),
                fit: Fit::Cover,
            }),
            kind: Some(catalog::Kind::Video),
            deadline: Instant::now(),
            playback: Playback {
                fps: 15,
                ..Playback::default()
            },
            properties: Resolved::default(),
            rotation: None,
            history_cursor: None,
        });
        let pause = PlaybackPatch {
            paused: Some(true),
            ..PlaybackPatch::default()
        };
        let playback = pause.update(output.playback()).unwrap();
        assert_eq!(playback.fps, 15);
        assert!(playback.paused);
        assert_eq!(output.pending.as_ref().unwrap().id, 4);
        output.control = Some(Control {
            id: 6,
            group: 5,
            deadline: Instant::now(),
            playback,
        });
        assert_eq!(output.playback(), playback);
    }

    #[test]
    fn session_unlock_does_not_clear_persisted_user_pause() {
        let saved = SavedOutput {
            paused: true,
            ..SavedOutput::default()
        };
        let mut output = Output::new(&saved, Playback::default());
        assert_eq!(
            output
                .view(Policy {
                    locked: true,
                    ..Policy::default()
                })
                .pause_reasons,
            ["user", "session_locked"]
        );
        let playback = output.playback();
        output.playback = playback;
        assert_eq!(output.view(Policy::default()).pause_reasons, ["user"]);
        assert!(output.actual(Policy::default()).paused);
        let mut saved = SavedOutput::default();
        saved.accept_playback(playback);
        assert!(saved.paused);
    }

    #[test]
    fn legacy_saved_outputs_inherit_new_playback_defaults() {
        let saved: SavedOutput = serde_json::from_str(
            r#"{"selection":{"asset_id":"local:/old.png","fit":"cover"},"paused":true}"#,
        )
        .unwrap();
        let output = Output::new(
            &saved,
            Playback {
                fps: 24,
                mute: false,
                paused: false,
                volume: 100,
            },
        );
        assert_eq!(output.playback.fps, 24);
        assert!(!output.playback.mute);
        assert!(output.playback.paused);
    }

    #[test]
    fn playback_fields_keep_the_public_output_shape() {
        let output = Output::new(
            &SavedOutput {
                paused: true,
                mute: Some(false),
                fps: Some(24),
                volume: Some(42),
                ..Default::default()
            },
            Playback::default(),
        );
        let value = serde_json::to_value(output.view(Policy::default())).unwrap();
        assert_eq!(value["paused"], true);
        assert_eq!(value["mute"], false);
        assert_eq!(value["fps"], 24);
        assert_eq!(value["volume"], 42);
        assert!(value.get("playback").is_none());
    }

    #[test]
    fn other_audio_mute_grace_reactivation_and_disconnect() {
        use crate::policy::audio::Snapshot;
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        session.outputs.insert(
            "DP-1".into(),
            Output::new(
                &SavedOutput {
                    mute: Some(false),
                    ..Default::default()
                },
                Playback::default(),
            ),
        );
        let active = Snapshot {
            available: true,
            other_audio_active: true,
        };
        let quiet = Snapshot {
            available: true,
            other_audio_active: false,
        };
        session.audio_changed(active);
        assert!(matches!(session.audio_mute, AudioMute::Active));
        assert!(session.outputs["DP-1"].actual(session.policy).mute);
        session.audio_changed(quiet);
        assert!(matches!(session.audio_mute, AudioMute::Grace(_)));
        assert!(session.outputs["DP-1"].actual(session.policy).mute);
        session.audio_changed(active);
        assert!(matches!(session.audio_mute, AudioMute::Active));
        session.audio_changed(quiet);
        session.audio_mute = AudioMute::Grace(Instant::now());
        session.expire();
        assert!(matches!(session.audio_mute, AudioMute::Clear));
        assert!(!session.outputs["DP-1"].actual(session.policy).mute);
        session.audio_changed(active);
        session.audio_changed(Snapshot::default());
        assert!(matches!(session.audio_mute, AudioMute::Clear));
        assert!(!session.outputs["DP-1"].actual(session.policy).mute);
    }
    #[test]
    fn power_events_apply_policy_without_an_idle_poll_deadline() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        session.outputs.insert(
            "A".into(),
            Output::new(&SavedOutput::default(), Playback::default()),
        );
        session.store.policy.pause_on_battery = Some(true);
        assert_eq!(session.timeout(), -1);
        let battery = crate::policy::power::Snapshot {
            available: true,
            on_battery: true,
        };
        session.power_changed(battery);
        assert!(session.outputs["A"].actual(session.policy).paused);
        session.dirty = false;
        session.power_changed(battery);
        assert!(!session.dirty);
        session.power_changed(crate::policy::power::Snapshot {
            on_battery: false,
            ..battery
        });
        assert!(!session.outputs["A"].actual(session.policy).paused);
        assert_eq!(session.timeout(), -1);
        session.power_due = Some(Instant::now() + Duration::from_secs(15));
        assert!((1..=15_001).contains(&session.timeout()));
    }

    #[test]
    fn runtime_policy_toggles_keep_user_intent_and_resume_revision() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        session.outputs.insert(
            "A".into(),
            Output::new(&SavedOutput::default(), Playback::default()),
        );
        session.outputs.get_mut("A").unwrap().current = Some("we:/test".into());
        session.policy_changed(Policy {
            locked: true,
            available: true,
            ..Policy::default()
        });
        assert!(session.outputs["A"].actual(session.policy).paused);
        session.store.policy.pause_on_session = Some(false);
        session.update_policy(session.policy);
        assert!(!session.outputs["A"].actual(session.policy).paused);
        assert!(session.outputs["A"].revision > 0);
        session.outputs.get_mut("A").unwrap().playback.paused = true;
        session.store.policy.pause_on_session = Some(true);
        session.update_policy(session.policy);
        session.policy_changed(Policy::default());
        assert!(session.outputs["A"].actual(session.policy).paused);
        session.outputs.get_mut("A").unwrap().playback.paused = false;
        session.niri.fullscreen_outputs.insert("A".into());
        session.update_policy(session.policy);
        assert!(session.outputs["A"].actual(session.policy).paused);
        session.store.policy.pause_on_fullscreen = Some(false);
        session.update_policy(session.policy);
        assert!(!session.outputs["A"].actual(session.policy).paused);
        session.power = crate::policy::power::Snapshot {
            available: true,
            on_battery: true,
        };
        session.store.policy.pause_on_battery = Some(true);
        session.update_policy(session.policy);
        assert!(session.outputs["A"].actual(session.policy).paused);
        assert!(
            session.outputs["A"]
                .view(session.policy)
                .pause_reasons
                .contains(&"battery")
        );
        session.power.on_battery = false;
        session.update_policy(session.policy);
        assert!(!session.outputs["A"].actual(session.policy).paused);
        let restored: Store =
            serde_json::from_value(serde_json::to_value(&session.store).unwrap()).unwrap();
        assert_eq!(restored.policy.pause_on_fullscreen, Some(false));
    }
    #[test]
    fn manual_playback_history_commits_only_after_loading_and_pause_freezes_rotation() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        let mut output = Output::new(&SavedOutput::default(), Playback::default());
        output.worker = Some(Renderer::echo());
        session.outputs.insert("A".into(), output);
        let assets: Vec<_> = ["a.jpg", "b.jpg", "c.jpg"]
            .into_iter()
            .map(|name| {
                let path = root.path().join(name);
                std::fs::write(&path, b"fixture").unwrap();
                let id = format!("local:{}", path.display());
                session.catalog_names.insert(id.clone(), name.into());
                id
            })
            .collect();
        let ack = |session: &mut Session, ok| {
            let id = session.outputs["A"].pending.as_ref().unwrap().id;
            session.worker_reply(
                "A",
                WorkerReply {
                    id,
                    ok,
                    error: (!ok).then(|| Error::new("load_failed", "fixture")),
                    runtime: None,
                    snapshot: None,
                    progress: None,
                    probe: None,
                },
            );
        };
        let apply = |session: &mut Session, id: &str, source: Option<&str>| {
            let mut params = json!({"asset_id":id,"output":"A"});
            if let Some(source) = source {
                params["source"] = json!(source);
            }
            session
                .dispatch(
                    1,
                    Request {
                        api: domain::API,
                        method: "apply".into(),
                        params,
                    },
                )
                .unwrap();
        };
        apply(&mut session, &assets[0], None);
        ack(&mut session, true);
        assert_eq!(session.store.outputs["A"].rotation.source, "library");
        assert!(!session.store.outputs["A"].rotation.enabled);
        session.advance_rotation("A", None).unwrap();
        assert_eq!(
            session.outputs["A"]
                .pending
                .as_ref()
                .unwrap()
                .selection
                .as_ref()
                .unwrap()
                .asset_id,
            assets[1]
        );
        ack(&mut session, true);
        assert!(
            session.rotations["A"].due.is_none(),
            "manual next must not enable a timer"
        );
        session.advance_rotation("A", None).unwrap();
        ack(&mut session, false);
        assert_eq!(session.rotations["A"].history, assets[..2]);
        session.store.outputs.get_mut("A").unwrap().rotation.mode = crate::library::Mode::Random;
        session.step_rotation("A", None, true).unwrap();
        ack(&mut session, true);
        assert_eq!(session.outputs["A"].current.as_ref(), Some(&assets[0]));
        session.advance_rotation("A", None).unwrap();
        ack(&mut session, true);
        assert_eq!(
            session.outputs["A"].current.as_ref(),
            Some(&assets[1]),
            "next retraces random history after previous"
        );
        session.advance_rotation("A", None).unwrap();
        let next = &session.outputs["A"]
            .pending
            .as_ref()
            .unwrap()
            .selection
            .as_ref()
            .unwrap()
            .asset_id;
        assert_ne!(next, &assets[1], "random must avoid immediate repeats");
        ack(&mut session, true);

        session.store.library.playlists.insert(
            "playlist:1".into(),
            crate::library::Playlist {
                name: "Test".into(),
                members: assets[..2].to_vec(),
                properties: BTreeMap::new(),
            },
        );
        apply(&mut session, &assets[0], Some("playlist:1"));
        ack(&mut session, false);
        assert_eq!(session.store.outputs["A"].rotation.source, "library");
        apply(&mut session, &assets[0], Some("playlist:1"));
        ack(&mut session, true);
        assert_eq!(session.store.outputs["A"].rotation.source, "playlist:1");
        assert_eq!(session.rotations["A"].history, [assets[0].clone()]);

        session.store.outputs.get_mut("A").unwrap().rotation.enabled = true;
        session.rotations.get_mut("A").unwrap().due =
            Some(Instant::now() + Duration::from_secs(42));
        session.outputs.get_mut("A").unwrap().playback.paused = true;
        session.expire();
        let remaining = session.rotations["A"].remaining.unwrap();
        assert!(remaining > Duration::from_secs(40));
        assert!(session.rotations["A"].due.is_none());
        session.outputs.get_mut("A").unwrap().playback.paused = false;
        session.expire();
        assert!(session.rotations["A"].due.unwrap() > Instant::now() + Duration::from_secs(40));
        assert!(session.rotations["A"].remaining.is_none());
        apply(&mut session, &assets[2], None);
        ack(&mut session, true);
        assert_eq!(session.store.outputs["A"].rotation.source, "library");
        assert!(
            !session.store.outputs["A"].rotation.enabled,
            "direct apply returns to manual library playback"
        );
        let temporary = root.path().join("outside-library.jpg");
        std::fs::write(&temporary, b"fixture").unwrap();
        apply(
            &mut session,
            &format!("local:{}", temporary.display()),
            None,
        );
        ack(&mut session, true);
        assert_eq!(session.store.outputs["A"].rotation.source, "single");
        assert!(
            session.snapshot()["rotations"]["A"]["members"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            session.advance_rotation("A", None).unwrap_err().code,
            "empty_playlist"
        );
    }

    #[test]
    fn scheduled_rotation_is_owned_by_daemon_without_frontend() {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path());
        session.outputs.insert(
            "A".into(),
            Output::new(&SavedOutput::default(), Playback::default()),
        );
        let now = unsafe { libc::time(std::ptr::null_mut()) };
        let mut local: libc::tm = unsafe { std::mem::zeroed() };
        unsafe {
            libc::localtime_r(&now, &mut local);
        }
        let day = if local.tm_wday == 0 {
            7
        } else {
            local.tm_wday as u8
        };
        let rotation = crate::library::Rotation {
            enabled: true,
            schedule: vec![crate::library::TimeRule {
                start: "00:00".into(),
                end: "00:00".into(),
                days: vec![day % 7 + 1],
            }],
            ..Default::default()
        };
        session.store.outputs.insert(
            "A".into(),
            SavedOutput {
                rotation,
                ..Default::default()
            },
        );
        session.rotations.insert(
            "A".into(),
            RotationRuntime {
                due: Some(Instant::now()),
                ..Default::default()
            },
        );
        session.expire();
        assert!(session.rotations["A"].error.is_none());
        assert!(session.rotations["A"].due.unwrap() > Instant::now());
        session
            .store
            .outputs
            .get_mut("A")
            .unwrap()
            .rotation
            .schedule
            .clear();
        session.rotations.get_mut("A").unwrap().due = Some(Instant::now());
        session.expire();
        assert_eq!(
            session.rotations["A"].error.as_ref().unwrap().code,
            "empty_playlist"
        );
    }
}
