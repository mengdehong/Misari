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
    time::{Duration, Instant},
};

#[derive(Serialize)]
struct OutputView {
    current: Option<String>,
    revision: u64,
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
    properties: Values,
    property_warnings: BTreeMap<String, String>,
}

struct Pending {
    id: u64,
    group: u64,
    selection: Option<Selection>,
    deadline: Instant,
    playback: Playback,
    properties: Resolved,
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
    web_host: bool,
    web_recovered: bool,
    pending: Option<Pending>,
    control: Option<Control>,
    property_control: Option<PropertyControl>,
    selection_id: u64,
    snapshot_control: Option<SnapshotControl>,
    runtime: WorkerRuntime,
    fullscreen: bool,
    auto_muted: bool,
    sent_clocks: BTreeMap<String, wallpaper_media::clock::Timeline>,
}

struct SnapshotControl {
    id: u64,
    client: u64,
    revision: u64,
    deadline: Instant,
}

impl Output {
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
            web_host: false,
            web_recovered: false,
            pending: None,
            control: None,
            property_control: None,
            selection_id: 0,
            snapshot_control: None,
            runtime: WorkerRuntime::default(),
            fullscreen: false,
            auto_muted: false,
            sent_clocks: BTreeMap::new(),
        }
    }

    fn policy(&self, session: Policy) -> PlaybackPolicy {
        PlaybackPolicy {
            session,
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
            revision: self.revision,
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
    due: Option<Instant>,
    previous: Option<String>,
    error: Option<Error>,
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
    Policy,
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
    let mut monitor = if config.pause_on_session.unwrap_or(true) {
        Some(session_policy::start()?)
    } else {
        None
    };
    let mut niri_monitor = if config.pause_on_fullscreen.unwrap_or(true) {
        Some(crate::policy::niri::start()?)
    } else {
        None
    };
    let mut audio_monitor = if config.mute_on_other_audio.unwrap_or(true) {
        Some(crate::policy::audio::start()?)
    } else {
        None
    };
    let store = Store::load()?;
    let mut capture = crate::audio_capture::Capture::start()?;
    let mut media_monitor = if config.media_integration.unwrap_or(true) {
        Some(crate::media::Monitor::start()?)
    } else {
        None
    };
    let catalog_names = catalog::list(&config, &store.library)
        .iter()
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
        if let Some(outputs) = desktop.dispatch()? {
            session.reconcile(outputs);
        }
        session.expire();
        session.finish_turn();
        capture.set_active(session.needs_audio());
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
            }
        }
    }
}

impl Session {
    fn media_changed(&mut self, media: crate::media::Snapshot) {
        if self.media == media {
            return;
        }
        self.media = media;
        for output in self.outputs.values_mut() {
            if let Some(worker) = &mut output.worker {
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
            niri_policy: &self.niri,
            audio_policy: &self.audio,
            audio_spectrum: json!({"available":self.spectrum.available,"capturing":self.spectrum.capturing,"device":self.spectrum.device,"sequence":self.spectrum.sequence}),
            media: &self.media,
            library: &self.store.library,
            rotations: self.outputs.keys().map(|name| {
                let settings = self.store.outputs.get(name).map(|o| o.rotation.clone()).unwrap_or_default();
                let error = self.rotations.get(name).and_then(|r| r.error.as_ref()).or(self.outputs[name].error.as_ref());
                (name.as_str(), json!({"settings":settings,"error":error}))
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
                    self.server.reply(
                        control.client,
                        &fail(Error::new("output_missing", "output disconnected")),
                    );
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
                output.fullscreen = self.niri.fullscreen_outputs.contains(&name);
                output.auto_muted = !matches!(self.audio_mute, AudioMute::Clear);
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
                "automatic_sources":crate::catalog::steam_workshops()
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
                    if output.rotation.source != "favorites"
                        && !candidate
                            .library
                            .playlists
                            .contains_key(&output.rotation.source)
                    {
                        output.rotation.enabled = false;
                    }
                }
                candidate
                    .save()
                    .map_err(|e| Error::new("save_failed", e.to_string()))?;
                self.store = candidate;
                if sources_changed {
                    self.catalog_names = catalog::list(&self.config, &self.store.library)
                        .iter()
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
                self.store = candidate;
                for name in &outputs {
                    self.rotations.insert(
                        name.clone(),
                        RotationRuntime {
                            due: settings.enabled.then(Instant::now),
                            ..RotationRuntime::default()
                        },
                    );
                }
                self.dirty = true;
                self.server.reply(client, &success(outputs, None));
            }
            "rotation_next" => {
                let outputs = self.targets(&request.params)?;
                if outputs.len() != 1 {
                    return Err(Error::new("bad_request", "next requires one output"));
                }
                self.advance_rotation(&outputs[0], Some(client))?;
            }
            "get_properties" => self.get_properties(client, &request.params)?,
            "set_properties" => self.set_properties(client, &request.params)?,
            "get_snapshot" => self.get_snapshot(client, &request.params)?,
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
                let properties = properties::patch(&request.params)?;
                self.start(
                    Some(client),
                    outputs.clone(),
                    Some(selection),
                    transition,
                    &patch,
                    &properties,
                    request.params.get("fit").is_none(),
                )?;
                self.stop_rotation(&outputs);
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

    fn get_snapshot(&mut self, client: u64, params: &Value) -> Result<(), Error> {
        let outputs = self.targets(params)?;
        if outputs.len() != 1 {
            return Err(Error::new("bad_request", "snapshot requires one output"));
        }
        let entry = self.outputs.get_mut(&outputs[0]).unwrap();
        if entry.pending.is_some()
            || entry.property_control.is_some()
            || entry.snapshot_control.is_some()
        {
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
            client,
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
                runtime.error = None;
            }
        }
    }

    fn advance_rotation(&mut self, name: &str, client: Option<u64>) -> Result<(), Error> {
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
            .filter(|id| catalog::resolve(id, settings.fit).is_ok())
            .collect();
        let previous = self
            .rotations
            .get(name)
            .and_then(|r| r.previous.as_deref())
            .or(output.current.as_deref());
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64;
        let next = settings
            .next(&members, previous, seed)
            .ok_or_else(|| Error::new("empty_playlist", "播放来源没有可用素材"))?
            .to_owned();
        let selection = catalog::resolve(&next, settings.fit)?;
        self.start(
            client,
            vec![name.into()],
            Some(selection),
            settings.transition,
            &PlaybackPatch::default(),
            &Values::new(),
            false,
        )?;
        let runtime = self.rotations.entry(name.into()).or_default();
        runtime.previous = Some(next);
        runtime.error = None;
        runtime.due = settings
            .enabled
            .then(|| Instant::now() + Duration::from_secs(settings.interval_seconds));
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
        self.server.reply(client, &json!({"ok":true,"asset_id":selection.asset_id,"definitions":schema.definitions,"values":defaults.values,"visible":defaults.visible,"outputs":outputs}));
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
        let patch = properties::patch(params)?;
        if patch.is_empty() {
            return Err(Error::new(
                "bad_request",
                "properties must contain at least one property",
            ));
        }
        let schema = Schema::load(&selection)?;
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
            output.fullscreen = self.niri.fullscreen_outputs.contains(name);
            output.auto_muted = !matches!(self.audio_mute, AudioMute::Clear);
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
        let web = cfg!(feature = "web")
            && selection
                .as_ref()
                .is_some_and(|s| catalog::kind(catalog::path(s)) == Some(catalog::Kind::WeWeb));
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
            let clock = selection.as_ref().and_then(|selection| {
                self.synchronized
                    .contains(&selection.asset_id)
                    .then(|| self.clocks[&selection.asset_id])
            });
            if selection.is_some() && entry.worker.is_none() {
                match Renderer::spawn(&name) {
                    Ok(worker) => {
                        entry.worker = Some(worker);
                        entry.web_host = false;
                    }
                    Err(error) => {
                        entry.error = Some(Error::new("renderer_failed", error.to_string()));
                        let error = entry.error.clone().unwrap();
                        self.complete(group, Err(error));
                        self.dirty = true;
                        continue;
                    }
                }
            }
            // Explicit choices replenish the single CEF-host recovery attempt.
            if client.is_some() {
                entry.web_recovered = false;
            }
            entry.web_host |= web;
            entry.error = None;
            entry.pending = Some(Pending {
                id,
                group,
                selection: selection.clone(),
                deadline: Instant::now()
                    + Duration::from_secs(if selection.is_some() { 60 } else { 10 }),
                playback,
                properties: properties.remove(&name).unwrap(),
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
                fail(Error::new("stale_selection", "wallpaper revision changed"))
            } else if reply.ok
                && let Some(path) = reply.snapshot
            {
                json!({"ok":true,"output":name,"current":entry.current,"revision":control.revision,"path":path})
            } else {
                fail(reply.error.unwrap_or_else(|| {
                    Error::new("snapshot_failed", "worker did not export a frame")
                }))
            };
            self.server.reply(control.client, &value);
            return;
        }
        if reply.id == 0 {
            if let Some(runtime) = reply.runtime {
                if runtime.selection_id != entry.selection_id {
                    return;
                }
                entry.runtime = runtime;

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
            entry.selection_id = pending.id;
            if let Some(runtime) = reply.runtime {
                entry.runtime = runtime;
            }
            entry.revision = pending.id;
            entry.current = pending.selection.as_ref().map(|s| s.asset_id.clone());
            entry.kind = pending
                .selection
                .as_ref()
                .and_then(|s| catalog::kind(catalog::path(s)));
            entry.playback = pending.playback;
            let saved = self.store.outputs.entry(name.into()).or_default();
            entry.properties = pending.properties.values;
            entry.property_warnings = pending.properties.warnings;
            if let Some(selection) = &pending.selection {
                saved.accept_properties(&selection.asset_id, pending.properties.overrides);
            }
            saved.accept_playback(pending.playback);
            saved.released = pending.selection.is_none();
            saved.selection = pending.selection;
            if saved.released {
                released_control = entry.control.take();
                entry.worker = None;
                entry.runtime = WorkerRuntime::default();
            }
            self.save_needed = true;
            Ok(())
        } else {
            Err(reply
                .error
                .unwrap_or_else(|| Error::new("load_failed", "content loading failed")))
        };
        entry.error = result.as_ref().err().cloned();
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
        // A CEF host can fail after initialization has made in-process retry unsafe.
        // Restore only accepted intent, once per explicit choice, never during release.
        let restore = (entry.web_host
            && !entry.web_recovered
            && entry.pending.as_ref().is_none_or(|p| p.selection.is_some()))
        .then(|| {
            self.store
                .outputs
                .get(name)
                .and_then(|s| s.selection.clone())
        })
        .flatten();
        entry.web_recovered |= restore.is_some();
        entry.worker = None;
        entry.runtime = WorkerRuntime::default();
        entry.current = None;
        entry.kind = None;
        entry.properties.clear();
        entry.property_warnings.clear();
        let error = Error::new("renderer_failed", message);
        entry.error = Some(error.clone());
        if let Some(control) = entry.snapshot_control.take() {
            self.server.reply(control.client, &fail(error.clone()));
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
        if let Some(selection) = restore
            && let Err(error) = self.start(
                None,
                vec![name.into()],
                Some(selection),
                Transition::Cut,
                &PlaybackPatch::default(),
                &Values::new(),
                false,
            )
        {
            self.outputs.get_mut(name).unwrap().error = Some(error);
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

    fn finish_turn(&mut self) {
        self.sync_clocks();
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
        for output in self.outputs.values_mut() {
            if output
                .snapshot_control
                .as_ref()
                .is_some_and(|c| c.deadline <= Instant::now())
            {
                let control = output.snapshot_control.take().unwrap();
                self.server.reply(
                    control.client,
                    &fail(Error::new("snapshot_failed", "snapshot timed out")),
                );
            }
        }
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
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in expired {
            self.worker_failed(&name, "worker response timed out");
        }
        let due: Vec<_> = self
            .rotations
            .iter()
            .filter(|(_, r)| r.due.is_some_and(|d| d <= Instant::now()))
            .map(|(name, _)| name.clone())
            .collect();
        for name in due {
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
            .chain(self.rotations.values().filter_map(|r| r.due))
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
            deadline: Instant::now(),
            playback: Playback {
                fps: 15,
                ..Playback::default()
            },
            properties: Resolved::default(),
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
}
