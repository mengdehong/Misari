//! Wayland negotiation stays independent of content engines and control transactions.
#[cfg(feature = "web")]
#[path = "worker_dmabuf.rs"]
mod dmabuf;
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::Buffer},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface, wl_surface::WlSurface},
};
use wayland_protocols::wp::{
    cursor_shape::v1::client::{
        wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
        wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
    },
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::{self, WpFractionalScaleV1},
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

use crate::{
    catalog::{self, Kind},
    content::Content,
    decoder::{Decoder, Frame},
    domain::{
        Error, Playback, Selection, Transition, WorkerAction, WorkerCommand, WorkerReply,
        WorkerRuntime,
    },
    graphics::{Gpu, Target},
    ipc,
    pixels::Size,
};

struct State {
    registry: RegistryState,
    outputs: OutputState,
    seats: SeatState,
    pointers: Vec<(wl_seat::WlSeat, wl_pointer::WlPointer)>,
    cursor_shapes: Option<WpCursorShapeManagerV1>,
    shm: Shm,
    presenter: Option<Presenter>,
    layer: Option<LayerSurface>,
    fractional_scale: Option<WpFractionalScaleV1>,
    output: Option<wl_output::WlOutput>,
    width: u32,
    height: u32,
    integer_scale: i32,
    preferred_scale_120: Option<u32>,
    has_viewport: bool,
    closed: bool,
}

pub fn run(output_name: &str) -> Result<()> {
    // The guard outlives content and GL resources; CEF starts lazily on the first Web load.
    #[cfg(feature = "web")]
    let _web_runtime = we_web::ShutdownGuard;
    let conn = Connection::connect_to_env().context("connecting to Wayland")?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor unavailable")?;
    let shell = LayerShell::bind(&globals, &qh).context("layer-shell unavailable")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm unavailable")?;
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let fractional_manager: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    #[cfg(feature = "web")]
    let dmabuf = dmabuf::Dmabuf::bind(&globals, &qh);
    let mut state = State {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        seats: SeatState::new(&globals, &qh),
        pointers: Vec::new(),
        cursor_shapes: globals.bind(&qh, 1..=1, ()).ok(),
        shm,
        presenter: None,
        layer: None,
        fractional_scale: None,
        output: None,
        width: 0,
        height: 0,
        integer_scale: 1,
        preferred_scale_120: None,
        has_viewport: viewporter.is_some(),
        closed: false,
    };
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;
    let output = state
        .outputs
        .outputs()
        .find(|out| state.outputs.info(out).and_then(|v| v.name).as_deref() == Some(output_name))
        .with_context(|| format!("Wayland output {output_name} is unavailable"))?;
    state.output = Some(output.clone());
    state.integer_scale = state
        .outputs
        .info(&output)
        .map_or(1, |v| v.scale_factor.max(1));
    let layer = shell.create_layer_surface(
        &qh,
        compositor.create_surface(&qh),
        Layer::Background,
        Some("wallpaperd"),
        Some(&output),
    );
    let viewport = viewporter
        .as_ref()
        .map(|manager| manager.get_viewport(layer.wl_surface(), &qh, ()));
    if viewport.is_some() {
        state.fractional_scale = fractional_manager
            .as_ref()
            .map(|manager| manager.get_fractional_scale(layer.wl_surface(), &qh, ()));
    }
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_size(0, 0);
    let empty = Region::new(&compositor)?;
    layer.wl_surface().set_input_region(Some(empty.wl_region()));
    // All engines render an opaque background. The compositor clips this
    // region to the surface extent, including subsequent resizes.
    let opaque = Region::new(&compositor)?;
    opaque.add(0, 0, i32::MAX, i32::MAX);
    layer
        .wl_surface()
        .set_opaque_region(Some(opaque.wl_region()));
    layer.commit();
    state.presenter = Some(Presenter::new(
        conn.clone(),
        layer.wl_surface().clone(),
        viewport,
        state.shm.wl_shm().clone(),
        output_name,
        #[cfg(feature = "web")]
        dmabuf,
    )?);
    state.layer = Some(layer);
    conn.flush()?;

    ipc::nonblocking(&io::stdin())?;
    let mut lines = ipc::Lines::default();
    let mut fds = Vec::with_capacity(6);
    let mut input_enabled = false;
    while !state.closed {
        queue.dispatch_pending(&mut state)?;
        state.presenter.as_mut().unwrap().tick(&qh)?;
        let enabled = state.presenter.as_ref().unwrap().accepts_pointer();
        if enabled != input_enabled {
            let surface = state.layer.as_ref().unwrap().wl_surface();
            surface.set_input_region(if enabled {
                None
            } else {
                Some(empty.wl_region())
            });
            surface.commit();
            input_enabled = enabled;
        }
        let needs_write = ipc::flush_wayland(&conn)?;
        if !needs_write {
            state.presenter.as_mut().unwrap().flushed();
        }
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let presenter = state.presenter.as_ref().unwrap();
        fds.clear();
        fds.extend([
            ipc::interest(&guard.connection_fd(), needs_write),
            ipc::interest(&io::stdin(), false),
            ipc::interest(&presenter.decoder.wake, false),
            ipc::interest(&presenter.wake, false),
        ]);
        ipc::poll(&mut fds, presenter.timeout())?;
        if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            ipc::read_wayland(guard)?;
            queue.dispatch_pending(&mut state)?;
        } else {
            drop(guard);
        }
        // New selections win over old completions from the same readiness batch.
        let presenter = state.presenter.as_mut().unwrap();
        if fds[1].revents != 0 {
            let (commands, eof) = lines.read(&mut io::stdin().lock())?;
            for line in commands {
                presenter.command(serde_json::from_slice::<WorkerCommand>(&line)?);
            }
            if eof {
                return Ok(());
            }
        }
        if fds[2].revents != 0 {
            presenter.decoded()?;
        }
        if fds[3].revents != 0 {
            presenter.drain_wake()?;
        }
    }
    Ok(())
}

impl State {
    fn configure(&mut self) -> Result<()> {
        if self.width == 0 || self.height == 0 {
            return Ok(());
        }
        anyhow::ensure!(
            self.width <= i32::MAX as u32 && self.height <= i32::MAX as u32,
            "logical output size overflow"
        );
        let scale = self
            .preferred_scale_120
            .map_or(self.integer_scale.max(1) as u64 * 120, u64::from);
        let scaled = |logical: u32| -> Result<u32> {
            u32::try_from((u64::from(logical) * scale).div_ceil(120))
                .context("output size overflow")
        };
        let geometry = Geometry {
            pixels: Size::new(scaled(self.width)?, scaled(self.height)?)?,
            logical: (self.width, self.height),
            buffer_scale: if self.has_viewport {
                1
            } else {
                self.integer_scale
            },
        };
        if let Some(presenter) = &mut self.presenter {
            presenter.configure(geometry)?;
        }
        Ok(())
    }

    fn geometry_changed(&mut self) {
        if let Err(error) = self.configure() {
            eprintln!("wallpaperd worker: configure: {error:#}");
        }
    }

    fn remove_pointer(&mut self, seat: &wl_seat::WlSeat) {
        self.pointers.retain(|(owner, pointer)| {
            if owner == seat {
                if pointer.version() >= 3 {
                    pointer.release();
                }
                false
            } else {
                true
            }
        });
        if let Some(presenter) = &mut self.presenter {
            presenter.pointer(None, None);
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        scale: i32,
    ) {
        self.integer_scale = scale.max(1);
        if self.preferred_scale_120.is_none() {
            self.geometry_changed();
        }
    }
    // Render in surface-local coordinates; the compositor applies the output transform.
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        if let Some(presenter) = &mut self.presenter {
            presenter.frame();
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.output.as_ref() == Some(&output) {
            self.integer_scale = self
                .outputs
                .info(&output)
                .map_or(1, |v| v.scale_factor.max(1));
            self.geometry_changed();
        }
    }
    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.output.as_ref() == Some(&output) {
            self.closed = true;
        }
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.closed = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.width = configure.new_size.0;
        self.height = configure.new_size.1;
        self.geometry_changed();
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            match self.seats.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointers.push((seat, pointer)),
                Err(error) => eprintln!("wallpaperd worker: pointer: {error}"),
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.remove_pointer(&seat);
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.remove_pointer(&seat);
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        use PointerEventKind::*;
        for event in events {
            if self
                .layer
                .as_ref()
                .is_none_or(|layer| &event.surface != layer.wl_surface())
            {
                continue;
            }
            let Some(presenter) = &mut self.presenter else {
                continue;
            };
            if let Enter { serial } = event.kind
                && let Some(manager) = &self.cursor_shapes
            {
                let device = manager.get_pointer(pointer, qh, ());
                device.set_shape(serial, Shape::Default);
                device.destroy();
            }
            match event.kind {
                Enter { .. } | Motion { .. } => presenter.pointer(Some(event.position), None),
                Press { button: 0x110, .. } => presenter.pointer(Some(event.position), Some(true)),
                Release { button: 0x110, .. } => {
                    presenter.pointer(Some(event.position), Some(false))
                }
                Leave { .. } => presenter.pointer(None, None),
                _ => {}
            }
        }
    }
}
delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_layer!(State);
delegate_registry!(State);
delegate_seat!(State);
delegate_pointer!(State);
wayland_client::delegate_noop!(State: ignore WpViewporter);
wayland_client::delegate_noop!(State: ignore WpViewport);
wayland_client::delegate_noop!(State: ignore WpFractionalScaleManagerV1);
wayland_client::delegate_noop!(State: ignore WpCursorShapeManagerV1);
wayland_client::delegate_noop!(State: ignore WpCursorShapeDeviceV1);
impl Dispatch<WpFractionalScaleV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.preferred_scale_120 = Some(scale.max(1));
            state.geometry_changed();
        }
    }
}
impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

const CALLBACK_STALL: Duration = Duration::from_millis(500);
const ANIMATION_DURATION: Duration = Duration::from_secs(1);

struct Pending {
    candidate: Option<Content>,
    id: u64,
    selection: Selection,
    transition: Transition,
    playback: Playback,
    properties: crate::properties::Values,
    clock: Option<wallpaper_media::clock::Timeline>,
    deadline: Instant,
}
/// A selection has one acknowledgement point: first frame committed, then flushed.
#[derive(Default)]
enum Switch {
    #[default]
    Idle,
    Loading(Box<Pending>),
    AwaitingFlush(u64),
}
impl Switch {
    fn candidate(&self) -> Option<&Content> {
        self.pending()
            .and_then(|pending| pending.candidate.as_ref())
    }
    fn candidate_mut(&mut self) -> Option<&mut Content> {
        self.pending_mut()
            .and_then(|pending| pending.candidate.as_mut())
    }
    fn take_candidate(&mut self) -> Option<Content> {
        self.pending_mut()
            .and_then(|pending| pending.candidate.take())
    }
    fn pending(&self) -> Option<&Pending> {
        match self {
            Self::Loading(request) => Some(request),
            _ => None,
        }
    }
    fn pending_mut(&mut self) -> Option<&mut Pending> {
        match self {
            Self::Loading(request) => Some(request),
            _ => None,
        }
    }
    fn take_pending(&mut self) -> Option<Pending> {
        if matches!(self, Self::Loading(_)) {
            let Self::Loading(request) = std::mem::take(self) else {
                unreachable!()
            };
            Some(*request)
        } else {
            None
        }
    }
    fn is_flushing(&self) -> bool {
        matches!(self, Self::AwaitingFlush(_))
    }
    fn cancel(&mut self) -> Option<u64> {
        match std::mem::take(self) {
            Self::Idle => None,
            Self::Loading(request) => Some(request.id),
            Self::AwaitingFlush(id) => Some(id),
        }
    }
}

struct Animation {
    previous: Target,
    start: Instant,
    last_rendered: Instant,
    kind: Transition,
}

impl Animation {
    fn progress(&self) -> f32 {
        (self.start.elapsed().as_secs_f32() / ANIMATION_DURATION.as_secs_f32()).min(1.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Geometry {
    pub pixels: Size,
    pub logical: (u32, u32),
    pub buffer_scale: i32,
}

enum FrameCallback {
    Ready,
    Waiting(Instant),
}

struct Presenter {
    output_name: String,
    // Drop content before its GL context, and GL before its Wayland connection.
    current: Option<Content>,
    switch: Switch,
    animation: Option<Animation>,
    gpu: Option<Gpu>,
    connection: Connection,
    surface: WlSurface,
    viewport: Option<WpViewport>,
    buffer: Option<Buffer>,
    #[cfg(feature = "web")]
    dmabuf: Option<dmabuf::Dmabuf>,
    pub decoder: Decoder,
    pub wake: UnixStream,
    wake_writer: UnixStream,
    generation: u64,
    selection: Option<Selection>,
    selection_id: u64,
    geometry: Option<Geometry>,
    playback: Playback,
    frame_callback: FrameCallback,
    throttled: bool,
    last_presented: Instant,
    repaint: bool,
    failed: bool,
    pointer: Option<(f64, f64)>,
    media: crate::media::Snapshot,
    audio: we_scene::audio::AudioSnapshot,
    reported_diagnostics: (u64, u64),
}

impl Presenter {
    pub fn new(
        connection: Connection,
        surface: WlSurface,
        viewport: Option<WpViewport>,
        shm: wl_shm::WlShm,
        output_name: &str,
        #[cfg(feature = "web")] dmabuf: Option<dmabuf::Dmabuf>,
    ) -> Result<Self> {
        let (wake, wake_writer) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        wake_writer.set_nonblocking(true)?;
        Ok(Self {
            output_name: output_name.into(),
            current: None,
            switch: Switch::Idle,
            animation: None,
            gpu: None,
            connection,
            surface,
            viewport,
            buffer: None,
            #[cfg(feature = "web")]
            dmabuf,
            decoder: Decoder::new(shm)?,
            wake,
            wake_writer,
            generation: 0,
            selection: None,
            selection_id: 0,
            geometry: None,
            playback: Playback::default(),
            frame_callback: FrameCallback::Ready,
            throttled: false,
            last_presented: Instant::now() - Duration::from_secs(1),
            repaint: false,
            failed: false,
            pointer: None,
            media: Default::default(),
            audio: Default::default(),
            reported_diagnostics: (0, 0),
        })
    }

    pub fn command(&mut self, command: WorkerCommand) {
        if let WorkerAction::SetAudio { ref audio } = command.action {
            if audio.valid() {
                self.audio = *audio.clone();
                if let Some(current) = &mut self.current {
                    current.set_audio(&self.audio);
                }
                if let Some(candidate) = self.switch.candidate_mut() {
                    candidate.set_audio(&self.audio);
                }
            }
            return;
        }
        if let WorkerAction::SetMedia { ref media } = command.action {
            self.media = *media.clone();
            if let Some(current) = &mut self.current {
                current.set_media(&self.media);
            }
            if let Some(candidate) = self.switch.candidate_mut() {
                candidate.set_media(&self.media);
            }
            self.runtime(Ok(()));
            return;
        }
        if let WorkerAction::SetClock {
            ref asset_id,
            clock,
        } = command.action
        {
            let result = self
                .synchronize(asset_id, clock)
                .map_err(|e| Error::new("sync_failed", format!("{e:#}")));
            if result.is_err() {
                self.runtime(result);
            }
            return;
        }
        if let WorkerAction::Snapshot { selection_id } = command.action {
            let result = self
                .export_frame(selection_id)
                .map_err(|e| Error::new("snapshot_failed", format!("{e:#}")));
            emit(
                command.id,
                result.as_ref().map(|_| ()).map_err(Clone::clone),
                None,
                result.ok(),
            );
            return;
        }
        if let WorkerAction::SetProperties {
            selection_id,
            ref asset_id,
            ref properties,
        } = command.action
        {
            let result = if self.switch.pending().is_some() || self.switch.is_flushing() {
                Err(Error::new("busy", "wallpaper is switching"))
            } else if selection_id != self.selection_id
                || self
                    .selection
                    .as_ref()
                    .is_none_or(|s| s.asset_id != *asset_id)
            {
                Err(Error::new("stale_selection", "wallpaper instance changed"))
            } else {
                self.current
                    .as_mut()
                    .filter(|c| c.properties_available())
                    .ok_or_else(|| {
                        Error::new("unsupported", "current backend does not support properties")
                    })
                    .and_then(|c| {
                        c.set_properties(properties)
                            .map_err(|e| Error::new("property_failed", format!("{e:#}")))
                    })
            };
            if let Some(gpu) = &self.gpu {
                gpu.reset();
            }
            if result.is_ok() {
                self.runtime(Ok(()));
            }
            reply(command.id, result);
            return;
        }
        if let WorkerAction::SetPlayback { playback } = command.action {
            let result = self
                .set_playback(playback)
                .map_err(|e| Error::new("playback_failed", e.to_string()));
            if result.is_ok() {
                self.runtime(Ok(()));
            }
            reply(command.id, result);
            return;
        }
        if let Some(id) = if self.switch.is_flushing() {
            self.switch.cancel()
        } else {
            None
        } {
            reply(id, Err(superseded()));
        }
        if let Some(previous) = self.switch.take_pending() {
            reply(previous.id, Err(superseded()));
        }
        self.switch.take_candidate();
        self.animation = None;
        self.generation += 1;
        self.decoder.cancel(self.generation);
        match command.action {
            WorkerAction::Apply {
                selection,
                transition,
                playback,
                properties,
            } => {
                if let Err(error) = playback.validate() {
                    reply(command.id, Err(error));
                    return;
                }
                self.switch = Switch::Loading(Box::new(Pending {
                    candidate: None,
                    id: command.id,
                    selection,
                    transition,
                    playback,
                    properties,
                    clock: None,
                    deadline: Instant::now() + Duration::from_secs(45),
                }));
                self.load();
            }
            WorkerAction::Release => {
                self.current = None;
                self.selection = None;
                self.surface.attach(None, 0, 0);
                self.surface.commit();
                self.buffer = None;
                #[cfg(feature = "web")]
                if let Some(dmabuf) = &mut self.dmabuf {
                    dmabuf.clear();
                }
                self.switch = Switch::AwaitingFlush(command.id);
                self.repaint = false;
            }
            WorkerAction::SetPlayback { .. }
            | WorkerAction::SetProperties { .. }
            | WorkerAction::SetClock { .. }
            | WorkerAction::SetMedia { .. }
            | WorkerAction::SetAudio { .. }
            | WorkerAction::Snapshot { .. } => unreachable!(),
        }
    }

    fn export_frame(&mut self, selection_id: u64) -> Result<std::path::PathBuf> {
        anyhow::ensure!(
            self.switch.pending().is_none() && !self.switch.is_flushing(),
            "wallpaper is switching"
        );
        anyhow::ensure!(
            selection_id == self.selection_id,
            "wallpaper instance changed"
        );
        let selection = self.selection.as_ref().context("no wallpaper is playing")?;
        let image = if let (Some(current), Some(gpu)) = (&mut self.current, &self.gpu) {
            current.export_frame(gpu)?
        } else {
            // EGL-free static wallpapers use the same fit as the SHM presenter.
            let size = self.geometry.context("output is not configured")?.pixels;
            let mut image = image::RgbaImage::new(size.width, size.height);
            crate::pixels::render_rgba(
                crate::pixels::decode(selection)?,
                size,
                selection.fit,
                image.as_mut(),
            )?;
            image
        };
        crate::pixels::cache_frame(
            &image,
            &crate::store::cache_path().with_file_name("snapshots"),
        )
    }

    fn effective_playback(&self) -> Playback {
        self.playback.for_engine(self.throttled, self.failed, false)
    }

    fn synchronize(
        &mut self,
        asset_id: &str,
        clock: wallpaper_media::clock::Timeline,
    ) -> Result<()> {
        if self
            .selection
            .as_ref()
            .is_some_and(|s| s.asset_id == asset_id)
            && let Some(current) = &mut self.current
        {
            current.synchronize(clock)?;
        }
        if let Some(pending) = self.switch.pending_mut()
            && pending.selection.asset_id == asset_id
        {
            pending.clock = Some(clock);
            if let Some(candidate) = &mut pending.candidate {
                candidate.synchronize(clock)?;
            }
        }
        Ok(())
    }

    fn set_playback(&mut self, playback: Playback) -> Result<()> {
        playback
            .validate()
            .map_err(|e| anyhow::anyhow!(e.message))?;
        self.playback = playback;
        if let Some(pending) = self.switch.pending_mut() {
            pending.playback = playback;
        }
        self.update_engines()
    }

    fn update_engines(&mut self) -> Result<()> {
        let effective = self.effective_playback();
        if let Some(current) = &mut self.current {
            current.playback(effective)?;
        }
        if let Some(pending) = self.switch.pending_mut()
            && let Some(candidate) = &mut pending.candidate
        {
            candidate.playback(pending.playback.for_engine(self.throttled, false, true))?;
        }
        self.send_pointer(None);
        Ok(())
    }

    pub fn accepts_pointer(&self) -> bool {
        self.current.as_ref().is_some_and(Content::accepts_pointer)
    }

    pub fn pointer(&mut self, position: Option<(f64, f64)>, button: Option<bool>) {
        self.pointer = position;
        self.send_pointer(button);
    }

    fn send_pointer(&mut self, button: Option<bool>) {
        let normalized = self.geometry.and_then(|g| {
            self.pointer
                .and_then(|p| crate::content::normalized_pointer(p, g.logical))
        });
        if let Some(current) = &mut self.current
            && let Err(error) = current.pointer(normalized, button)
        {
            self.runtime(Err(Error::new("input_failed", error.to_string())));
        }
        // A pending wallpaper receives motion, never a click intended for the current one.
        if let Some(candidate) = self.switch.candidate_mut()
            && let Err(error) = candidate.pointer(normalized, None)
        {
            self.fail(Error::new("input_failed", error.to_string()));
        }
    }

    pub fn configure(&mut self, geometry: Geometry) -> Result<()> {
        if self.geometry == Some(geometry) {
            return Ok(());
        }
        self.geometry = Some(geometry);
        if let Some(gpu) = &mut self.gpu {
            gpu.resize(geometry.pixels);
            if let Some(content) = &mut self.current {
                content.resized();
            }
            if let Some(content) = self.switch.candidate_mut() {
                content.resized();
            }
        }
        self.repaint = self.current.is_some();
        if self.switch.pending().is_some() {
            if self.switch.candidate().is_none()
                || self.switch.pending().is_some_and(|p| {
                    catalog::kind(catalog::path(&p.selection)) == Some(Kind::Image)
                })
            {
                self.load();
            }
        } else if let Some(selection) = &self.selection
            && catalog::kind(catalog::path(selection)) == Some(Kind::Image)
        {
            self.generation += 1;
            self.decoder.submit(
                self.generation,
                selection.clone(),
                geometry.pixels,
                self.gpu.is_some(),
            );
        }
        self.send_pointer(None);
        Ok(())
    }

    fn load(&mut self) {
        let (Some(geometry), Some(pending)) = (self.geometry, self.switch.pending()) else {
            return;
        };
        let kind = catalog::kind(catalog::path(&pending.selection));
        if self.gpu.is_none() {
            match Gpu::wayland(
                self.connection.backend().display_ptr().cast(),
                &self.surface,
                geometry.pixels,
            ) {
                Ok(gpu) => self.gpu = Some(gpu),
                Err(error)
                    if kind == Some(Kind::Image) && pending.transition == Transition::Cut =>
                {
                    eprintln!("wallpaperd worker: EGL unavailable, using static SHM: {error:#}");
                }
                Err(error) => {
                    self.fail(Error::new(
                        "unsupported",
                        format!("EGL/GLES 3 is required: {error:#}"),
                    ));
                    return;
                }
            }
        }
        if pending.transition != Transition::Cut
            && self.selection.is_some()
            && self.current.is_none()
        {
            self.fail(Error::new(
                "unsupported",
                "animated transitions require an EGL-rendered current frame",
            ));
            return;
        }
        if kind == Some(Kind::Image) {
            self.generation += 1;
            self.decoder.submit(
                self.generation,
                pending.selection.clone(),
                geometry.pixels,
                self.gpu.is_some(),
            );
        } else {
            let gpu = self.gpu.as_ref().unwrap();
            match Content::load_for_output(
                gpu,
                &pending.selection,
                pending.playback.for_engine(self.throttled, false, true),
                &self.wake_writer,
                &pending.properties,
                pending.clock,
                Some(&self.output_name),
            ) {
                Ok(mut content) => {
                    content.set_media(&self.media);
                    content.set_audio(&self.audio);
                    self.switch.pending_mut().unwrap().candidate = Some(content);
                    self.send_pointer(None);
                }
                Err(error) => self.fail(Error::new(
                    if (kind == Some(Kind::Video)
                        && wallpaper_media::capability()["available"] == false)
                        || (kind == Some(Kind::WeWeb)
                            && crate::web::capability()["available"] == false)
                        || (kind == Some(Kind::WeScene)
                            && crate::store::Config::load().is_ok_and(|config| {
                                matches!(
                                    config.wallpaper_engine.scene_backend,
                                    crate::store::SceneBackend::Native
                                )
                            }))
                    {
                        "unsupported"
                    } else {
                        "load_failed"
                    },
                    format!("{error:#}"),
                )),
            }
        }
    }

    fn fail(&mut self, error: Error) {
        self.generation += 1;
        self.decoder.cancel(self.generation);
        self.switch.take_candidate();
        if let Some(pending) = self.switch.take_pending() {
            reply(pending.id, Err(error));
        } else {
            eprintln!("wallpaperd worker: redraw: {}", error.message);
        }
    }

    pub fn decoded(&mut self) -> Result<()> {
        for decoded in self.decoder.receive()? {
            if decoded.generation != self.generation {
                continue;
            }
            match decoded.frame {
                Ok(Frame::Image(image)) => {
                    let selection = self
                        .switch
                        .pending()
                        .map(|p| &p.selection)
                        .or(self.selection.as_ref())
                        .context("image selection missing")?;
                    match Content::image(
                        self.gpu.as_ref().context("image GPU missing")?,
                        image,
                        selection,
                    ) {
                        Ok(content) if self.switch.pending().is_some() => {
                            self.switch.pending_mut().unwrap().candidate = Some(content)
                        }
                        Ok(content) => {
                            self.current = Some(content);
                            self.repaint = true;
                        }
                        Err(error) => self.fail(Error::new("load_failed", error.to_string())),
                    }
                }
                Ok(Frame::Shm { size, buffer }) => {
                    self.surface_geometry();
                    self.surface
                        .damage_buffer(0, 0, size.width as i32, size.height as i32);
                    buffer.attach_to(&self.surface)?;
                    self.surface.commit();
                    self.buffer = Some(buffer);
                    if let Some(pending) = self.switch.take_pending() {
                        self.selection_id = pending.id;
                        self.selection = Some(pending.selection);
                        self.playback = pending.playback;
                        self.switch = Switch::AwaitingFlush(pending.id);
                    }
                }
                Err(error) => self.fail(Error::new("load_failed", format!("{error:#}"))),
            }
        }
        Ok(())
    }

    pub fn drain_wake(&mut self) -> io::Result<()> {
        let mut bytes = [0; 256];
        loop {
            match self.wake.read(&mut bytes) {
                Ok(0) => return Err(io::ErrorKind::BrokenPipe.into()),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn surface_geometry(&self) {
        if let Some(geometry) = self.geometry {
            self.surface.set_buffer_scale(geometry.buffer_scale);
            if let Some(viewport) = &self.viewport {
                viewport.set_source(-1.0, -1.0, -1.0, -1.0);
                viewport.set_destination(geometry.logical.0 as i32, geometry.logical.1 as i32);
            }
        }
    }

    pub fn tick(&mut self, qh: &QueueHandle<State>) -> Result<()> {
        if self
            .switch
            .pending()
            .is_some_and(|p| p.deadline <= Instant::now())
        {
            self.fail(Error::new(
                "load_failed",
                format!(
                    "content did not produce a first frame within 45 seconds: {}",
                    self.switch
                        .candidate()
                        .map_or_else(String::new, Content::diagnostics)
                ),
            ));
        }
        if let Some(candidate) = self.switch.candidate_mut()
            && let Err(error) = candidate
                .poll()
                .and_then(|()| candidate.prepare_frame(self.gpu.as_ref()))
        {
            self.fail(Error::new("load_failed", error.to_string()));
        }
        if let Some(current) = &mut self.current
            && let Err(error) = current
                .poll()
                .and_then(|()| current.prepare_frame(self.gpu.as_ref()))
        {
            current.fail(self.playback);
            self.failed = true;
            self.runtime(Err(Error::new("playback_failed", error.to_string())));
        }
        // A page can subscribe to audio without painting another frame.
        self.report_diagnostics();
        if !self.throttled
            && matches!(self.frame_callback, FrameCallback::Waiting(start) if start.elapsed() >= CALLBACK_STALL)
        {
            self.throttled = true;
            self.update_engines()?;
            self.runtime(Ok(()));
        }
        if !matches!(self.frame_callback, FrameCallback::Ready)
            || self.last_presented.elapsed() < self.interval()
        {
            return Ok(());
        }
        let rendering_started = Instant::now();
        let Some(gpu) = &self.gpu else {
            return Ok(());
        };
        let animation = self
            .switch
            .pending()
            .is_some_and(|p| p.transition != Transition::Cut)
            && self.current.is_some();
        #[cfg(feature = "web")]
        let candidate_direct = !animation && self.can_present_dmabuf(self.switch.candidate());
        #[cfg(not(feature = "web"))]
        let candidate_direct = false;
        if let Some(candidate) = self.switch.candidate_mut()
            && candidate.needs_render()
            && !candidate_direct
            && let Err(error) = candidate.render(gpu)
        {
            self.fail(Error::new("load_failed", error.to_string()));
            return Ok(());
        }
        let switch = self.switch.candidate().is_some_and(Content::ready);
        let animation_finished = self
            .animation
            .as_ref()
            .is_some_and(|animation| animation.progress() >= 1.0);
        let direct = !switch
            && (self.animation.is_none() || animation_finished)
            && self.current.as_ref().is_some_and(Content::dynamic);
        #[cfg(feature = "web")]
        let dma_direct = if switch {
            candidate_direct
        } else {
            (self.animation.is_none() || animation_finished)
                && self.can_present_dmabuf(self.current.as_ref())
        };
        #[cfg(not(feature = "web"))]
        let dma_direct = false;
        // Compose the transition independently, without upsampling the content engine.
        let content_due = self.animation.as_ref().is_none_or(|animation| {
            animation.last_rendered.elapsed()
                >= Duration::from_secs_f64(1.0 / f64::from(self.playback.fps))
        });
        let changed = if let Some(current) = &mut self.current {
            if switch && animation {
                if let Err(error) = current.snapshot(gpu) {
                    // Snapshot allocation is part of the candidate transaction.
                    // Keep the already submitted frame if it cannot be prepared.
                    self.fail(Error::new("load_failed", format!("snapshot: {error:#}")));
                    return Ok(());
                }
                false
            } else if dma_direct {
                current.needs_render()
            } else if direct {
                current.render_direct(gpu, self.repaint || animation_finished)?
            } else if !switch && content_due && current.needs_render() {
                current.render(gpu)?
            } else {
                false
            }
        } else {
            false
        };
        if changed && let Some(animation) = &mut self.animation {
            animation.last_rendered = frame_anchor(
                animation.last_rendered,
                rendering_started,
                Duration::from_secs_f64(1.0 / f64::from(self.playback.fps)),
            );
        }
        if switch || changed || self.repaint || self.animation.is_some() {
            let content = if switch {
                self.switch.candidate()
            } else {
                self.current.as_ref()
            };
            let Some(content) = content else {
                return Ok(());
            };
            let (previous, kind, progress) = if switch && animation {
                (
                    self.current.as_ref().map(Content::target),
                    self.switch.pending().unwrap().transition,
                    0.0,
                )
            } else if !switch && let Some(animation) = &self.animation {
                (
                    Some(&animation.previous),
                    animation.kind,
                    animation.progress(),
                )
            } else {
                (None, Transition::Cut, 1.0)
            };
            self.surface_geometry();
            self.surface.frame(qh, self.surface.clone());
            #[cfg(feature = "web")]
            if dma_direct {
                let frame = content.direct_frame().unwrap();
                let buffer = self.dmabuf.as_mut().unwrap().buffer(frame, qh)?;
                if let Some(viewport) = &self.viewport {
                    let [x, y, w, h] = frame.info.visible;
                    let scale = f64::from(self.geometry.unwrap().buffer_scale);
                    viewport.set_source(
                        x as f64 / scale,
                        y as f64 / scale,
                        w as f64 / scale,
                        h as f64 / scale,
                    );
                }
                self.surface.attach(Some(&buffer), 0, 0);
                self.surface.damage_buffer(
                    0,
                    0,
                    frame.info.size[0] as i32,
                    frame.info.size[1] as i32,
                );
                self.surface.commit();
            }
            if !dma_direct && direct {
                gpu.swap()?;
            } else if !dma_direct {
                gpu.present(content.target(), previous, kind, progress)?;
            }
            content.report_swap();
            self.last_presented =
                frame_anchor(self.last_presented, rendering_started, self.interval());
            self.frame_callback = FrameCallback::Waiting(Instant::now());
            self.repaint = false;
            self.buffer = None;
            #[cfg(feature = "web")]
            if dma_direct {
                if switch {
                    self.switch.candidate_mut().unwrap().direct_presented();
                } else {
                    self.current.as_mut().unwrap().direct_presented();
                }
            } else if let Some(dmabuf) = &mut self.dmabuf {
                dmabuf.clear();
            }
            if switch {
                let previous = self.current.replace(self.switch.take_candidate().unwrap());
                if animation && let Some(previous) = previous {
                    self.animation = Some(Animation {
                        previous: previous.into_snapshot(),
                        start: Instant::now(),
                        last_rendered: Instant::now(),
                        kind,
                    });
                }
                if let Some(pending) = self.switch.take_pending() {
                    self.selection_id = pending.id;
                    self.selection = Some(pending.selection);
                    self.playback = pending.playback;
                    self.switch = Switch::AwaitingFlush(pending.id);
                }
                let playback = self.effective_playback();
                self.current.as_mut().unwrap().playback(playback)?;
                self.failed = false;
                self.runtime(Ok(()));
            } else if progress >= 1.0 {
                self.animation = None;
            }
        }
        self.report_diagnostics();
        Ok(())
    }

    #[cfg(feature = "web")]
    fn can_present_dmabuf(&self, content: Option<&Content>) -> bool {
        content
            .and_then(Content::direct_frame)
            .is_some_and(|frame| {
                self.dmabuf
                    .as_ref()
                    .is_some_and(|dmabuf| dmabuf.supports(frame, self.viewport.is_some()))
            })
    }

    fn report_diagnostics(&mut self) {
        let stamp = (
            self.selection_id,
            self.current.as_ref().map_or(0, Content::error_generation),
        );
        if stamp != self.reported_diagnostics {
            self.reported_diagnostics = stamp;
            self.runtime(Ok(()));
        }
    }
    pub fn frame(&mut self) {
        self.frame_callback = FrameCallback::Ready;
        if self.throttled {
            self.throttled = false;
            if let Err(error) = self.update_engines() {
                self.runtime(Err(Error::new("playback_failed", error.to_string())));
            }
            self.runtime(Ok(()));
        }
    }

    pub fn flushed(&mut self) {
        if let Some(id) = if self.switch.is_flushing() {
            self.switch.cancel()
        } else {
            None
        } {
            emit(id, Ok(()), Some(self.observed()), None);
        }
    }
    fn interval(&self) -> Duration {
        if self.animation.is_some() {
            let fps = if self.gpu.as_ref().is_some_and(Gpu::software) {
                30.0
            } else {
                60.0
            };
            return Duration::from_secs_f64(1.0 / fps);
        }
        let fps = if self.selection.is_none() {
            self.switch
                .pending()
                .map_or(self.playback.fps, |p| p.playback.fps)
        } else {
            self.playback.fps
        };
        Duration::from_secs_f64(1.0 / f64::from(fps))
    }

    pub fn timeout(&self) -> i32 {
        let active = self.repaint
            || self.animation.is_some()
            || self.switch.candidate().is_some()
            || self.current.as_ref().is_some_and(Content::animated);
        // Dynamic engines wake us when a frame is available. Their playback
        // clock is not a reason to poll at the presentation ceiling.
        let work = self.repaint
            || self.animation.is_some()
            || self
                .switch
                .candidate()
                .is_some_and(|c| c.ready() || c.needs_render())
            || self.current.as_ref().is_some_and(Content::needs_render);
        let render = (work && matches!(self.frame_callback, FrameCallback::Ready))
            .then(|| self.last_presented + self.interval());
        let throttle = match self.frame_callback {
            FrameCallback::Waiting(start) if !self.throttled && active => {
                Some(start + CALLBACK_STALL)
            }
            _ => None,
        };
        render
            .into_iter()
            .chain(throttle)
            .chain(self.switch.pending().map(|p| p.deadline))
            .min()
            .map_or(-1, |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .saturating_add(1)
                    .min(i32::MAX as u128) as i32
            })
    }

    fn observed(&self) -> WorkerRuntime {
        WorkerRuntime {
            selection_id: self.selection_id,
            audio: self.current.as_ref().is_some_and(Content::requires_audio)
                || self.switch.candidate().is_some_and(Content::requires_audio),
            backend: self.current.as_ref().map(Content::backend),
            throttled: self.throttled,
            failed: self.failed,
            properties: self
                .current
                .as_ref()
                .is_some_and(Content::properties_available),
            media: self.current.as_ref().is_some_and(Content::media_available),
            media_error: None,
            diagnostics: self.current.as_ref().and_then(Content::runtime_errors),
            pointer: if !self.current.as_ref().is_some_and(Content::accepts_pointer) {
                crate::domain::PointerScope::None
            } else {
                crate::domain::PointerScope::Desktop
            },
        }
    }
    fn runtime(&self, result: Result<(), Error>) {
        emit(0, result, Some(self.observed()), None);
    }
}

fn superseded() -> Error {
    Error::new("superseded", "a newer selection replaced this request")
}

// Pace frame starts against the preceding deadline. Rendering and swapping
// consume the current frame's budget, rather than adding to the next interval.
// Rebase after a stall instead of issuing a burst of catch-up frames.
fn frame_anchor(previous: Instant, started: Instant, interval: Duration) -> Instant {
    let deadline = previous + interval;
    if started.saturating_duration_since(deadline) < interval {
        deadline
    } else {
        started
    }
}

pub fn reply(id: u64, result: Result<(), Error>) {
    emit(id, result, None, None);
}

fn emit(
    id: u64,
    result: Result<(), Error>,
    runtime: Option<WorkerRuntime>,
    snapshot: Option<std::path::PathBuf>,
) {
    let (ok, error) = match result {
        Ok(()) => (true, None),
        Err(error) => (false, Some(error)),
    };
    let mut stdout = io::stdout().lock();
    let _ = serde_json::to_writer(
        &mut stdout,
        &WorkerReply {
            id,
            ok,
            error,
            runtime,
            snapshot,
        },
    );
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_pacing_accounts_for_rendering_and_rebases_after_a_stall() {
        let start = Instant::now();
        let interval = Duration::from_secs_f64(1. / 30.);
        let mut anchor = start;
        for frame in 1..=300 {
            let started = anchor + interval + Duration::from_micros(600);
            anchor = frame_anchor(anchor, started, interval);
            assert_eq!(anchor, start + interval * frame);
            let finished = started + Duration::from_millis(5);
            assert!(anchor + interval > finished);
        }
        let resumed = anchor + Duration::from_secs(2);
        assert_eq!(frame_anchor(anchor, resumed, interval), resumed);
    }
}
