// Portions adapted from Smithay examples.
// MIT License
//
// Copyright (c) 2017 Victor Berger and Victoria Brekenfeld
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

mod protocol;

use crate::{
    model::{Enhancement, save_json},
    render::{Pipeline, View, fit},
    stream::{self, Stream},
};
use anyhow::{Context, Result, anyhow};
use smithay::{
    backend::{
        drm::DrmNode,
        egl::{
            EGLContext, EGLDevice, EGLDisplay, context::GlAttributes, native::EGLSurfacelessDisplay,
        },
        input::{
            AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
            InputTime, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
        },
        renderer::{
            Bind, Frame, ImportDma, Renderer,
            element::{
                AsRenderElements, Kind,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
            utils::draw_render_elements,
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{
        Seat, SeatState,
        keyboard::{FilterResult, keysyms},
        pointer::{AxisFrame, ButtonEvent, CursorImageStatus, CursorImageSurfaceData, MotionEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, LoopSignal, Mode as PollMode, PostAction, channel,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        wayland_server::{Display, DisplayHandle, protocol::wl_surface::WlSurface},
        winit::{
            cursor::Cursor, dpi::PhysicalSize, monitor::Fullscreen,
            platform::wayland::WindowAttributesWayland, window::WindowAttributes,
        },
    },
    utils::{Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Transform},
    wayland::{
        compositor::CompositorState,
        dmabuf::{DmabufFeedbackBuilder, DmabufState},
        output::OutputManagerState,
        seat::WaylandFocus,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
        xwayland_shell::XWaylandShellState,
    },
    xwayland::{X11Wm, XWayland, XWaylandEvent},
};
use std::{
    cell::RefCell,
    ffi::OsString,
    path::PathBuf,
    process::{Child, Command, Stdio},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

type Backend = Rc<RefCell<WinitGraphicsBackend<GlesRenderer>>>;

struct State {
    display: DisplayHandle,
    handle: LoopHandle<'static, Self>,
    signal: LoopSignal,
    backend: Option<Backend>,
    offscreen: Option<GlesRenderer>,
    stream: Option<Stream>,
    remote_buttons: Vec<u32>,
    remote_keys: Vec<u32>,
    settings: Enhancement,
    output: Output,
    compositor: CompositorState,
    shell: XdgShellState,
    xwayland_shell: XWaylandShellState,
    shm: ShmState,
    dmabuf: DmabufState,
    _output_manager: OutputManagerState,
    seat_state: SeatState<Self>,
    seat: Seat<Self>,
    data_device: DataDeviceState,
    popups: PopupManager,
    space: Space<Window>,
    xwm: Option<X11Wm>,
    child: Option<Child>,
    launch: Option<Command>,
    pipeline: Option<Pipeline>,
    viewport: Option<View>,
    pointer: Point<f64, Physical>,
    cursor: CursorImageStatus,
    buttons: Vec<u32>,
    fullscreen_key: bool,
    dirty: bool,
    started: Instant,
    last_metrics: Option<Instant>,
    metrics_path: PathBuf,
    error: Option<anyhow::Error>,
    game_exit: Option<std::process::ExitStatus>,
    clients: protocol::ClientRects,
}

fn pci_selector(node: DrmNode) -> Option<String> {
    let render = node.dev_path()?;
    let info = std::fs::read_to_string(
        std::path::Path::new("/sys/class/drm")
            .join(render.file_name()?)
            .join("device/uevent"),
    )
    .ok()?;
    let slot = info
        .lines()
        .find_map(|line| line.strip_prefix("PCI_SLOT_NAME="))?;
    Some(format!("pci-{}", slot.replace([':', '.'], "_")))
}

fn headless_renderer() -> Result<GlesRenderer> {
    let make_renderer = |display: EGLDisplay| -> Result<GlesRenderer> {
        Ok(unsafe { GlesRenderer::new(EGLContext::new(&display)?)? })
    };
    if let Ok(prime) = std::env::var("DRI_PRIME")
        && prime.starts_with("pci-")
        && let Some(device) = EGLDevice::enumerate()?.find(|device| {
            device
                .try_get_render_node()
                .ok()
                .flatten()
                .and_then(pci_selector)
                .as_deref()
                == Some(prime.trim_end_matches('!'))
        })
    {
        return make_renderer(unsafe { EGLDisplay::new(device)? });
    }
    // Prefer the driver's default GPU. Drivers without the surfaceless
    // platform can expose their hardware through EGL_EXT_platform_device.
    if let Ok(display) = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }
        && EGLDevice::device_for_display(&display)
            .and_then(|device| device.try_get_render_node())
            .is_ok_and(|node| node.is_some())
        && let Ok(renderer) = make_renderer(display)
    {
        return Ok(renderer);
    }
    for device in EGLDevice::enumerate()? {
        if device
            .try_get_render_node()
            .is_ok_and(|node| node.is_some())
            && let Ok(display) = unsafe { EGLDisplay::new(device) }
            && let Ok(renderer) = make_renderer(display)
        {
            return Ok(renderer);
        }
    }
    anyhow::bail!("找不到可用于无窗口串流的 EGL/DRM 设备")
}

pub fn run(
    settings: Enhancement,
    metrics_path: PathBuf,
    stream_socket: Option<PathBuf>,
    headless: bool,
    command: Vec<OsString>,
) -> Result<()> {
    anyhow::ensure!(!command.is_empty(), "原生会话没有指定游戏启动命令");
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .try_init();
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display = Display::<State>::new()?;
    let dh = display.handle();
    let (mut backend, mut offscreen) = if headless {
        (None, Some(headless_renderer()?))
    } else {
        let attributes = WindowAttributes::default()
            .with_title("GalScaler — 启动中")
            .with_platform_attributes(Box::new(
                WindowAttributesWayland::default().with_name("io.misari.galscaler", "GalScaler"),
            ))
            .with_surface_size(PhysicalSize::new(
                settings.output.width,
                settings.output.height,
            ))
            .with_fullscreen(settings.fullscreen.then_some(Fullscreen::Borderless(None)));
        let (backend, events) = winit::init_from_attributes_with_gl_attr::<GlesRenderer>(
            attributes,
            GlAttributes {
                version: (3, 2),
                profile: None,
                debug: false,
                vsync: true,
            },
        )
        .map_err(|e| anyhow!(e.to_string()))?;
        (Some((backend, events)), None)
    };
    let renderer = match &mut backend {
        Some((backend, _)) => backend.renderer(),
        None => offscreen.as_mut().unwrap(),
    };

    let mode = Mode {
        size: (settings.source.width as i32, settings.source.height as i32).into(),
        refresh: settings.fps as i32 * 1000,
    };
    let output = Output::new(
        "GalScaler".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "GalScaler".into(),
            model: "Virtual game display".into(),
            serial_number: "1".into(),
        },
    );
    output.create_global::<State>(&dh);
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    let mut dmabuf = DmabufState::new();
    let device = EGLDevice::device_for_display(renderer.egl_context().display())?;
    let node = device
        .try_get_render_node()?
        .context("无法识别渲染设备的 DRM 节点")?;
    let feedback = DmabufFeedbackBuilder::new(node.dev_id(), renderer.dmabuf_formats()).build()?;
    dmabuf.create_global_with_default_feedback::<State>(&dh, &feedback);

    let prime = headless.then(|| pci_selector(node)).flatten();
    eprintln!(
        "GalScaler native: DRM {}",
        node.dev_path()
            .as_deref()
            .map_or_else(|| format!("{node:?}"), |path| path.display().to_string(),)
    );
    let stream = stream_socket
        .map(|socket| {
            let (input, events) = channel::sync_channel(128);
            event_loop
                .handle()
                .insert_source(events, |event, _, state| {
                    if let channel::Event::Msg(input) = event {
                        state.remote_input(input);
                        let _ = state.display.flush_clients();
                    }
                })
                .map_err(|e| anyhow!("{}", e.error))?;
            Stream::start(
                socket,
                node.dev_path().as_deref(),
                settings.fps.into(),
                input,
            )
        })
        .transpose()?;

    let compositor = CompositorState::new::<State>(&dh);
    let shell = XdgShellState::new::<State>(&dh);
    let xwayland_shell = XWaylandShellState::new::<State>(&dh);
    let shm = ShmState::new::<State>(&dh, Vec::new());
    let output_manager = OutputManagerState::new_with_xdg_output::<State>(&dh);
    let data_device = DataDeviceState::new::<State>(&dh);
    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "GalScaler");
    seat.add_keyboard(Default::default(), 400, 25)?;
    seat.add_pointer();
    let mut space = Space::default();
    space.map_output(&output, (0, 0));
    let listener = ListeningSocketSource::new_auto()?;
    let socket_name = listener.socket_name().to_os_string();
    event_loop
        .handle()
        .insert_source(listener, |stream, _, state| {
            if let Err(error) = state
                .display
                .insert_client(stream, Arc::new(protocol::ClientState::default()))
            {
                eprintln!("无法接入游戏 Wayland 客户端：{error}");
            }
        })
        .map_err(|e| anyhow!("{}", e.error))?;
    event_loop
        .handle()
        .insert_source(
            Generic::new(display, Interest::READ, PollMode::Level),
            |_, display, state| {
                if let Err(error) = unsafe { display.get_mut().dispatch_clients(state) } {
                    state.fail(error.into());
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow!("{}", e.error))?;

    let rect_path = metrics_path.with_extension("client");
    let mut launch = Command::new(&command[0]);
    launch
        .args(&command[1..])
        .env("WAYLAND_DISPLAY", socket_name)
        .env(
            "GALSCALER_CLIENT_RECTS",
            format!("Z:{}", rect_path.to_string_lossy().replace('/', "\\")),
        );
    if let Some(stream) = &stream {
        launch.env("PULSE_SINK", &stream.sink);
    }
    if let Some(prime) = &prime {
        launch.env("DRI_PRIME", prime);
    }
    let (backend, events) = match backend {
        Some((backend, events)) => (Some(Rc::new(RefCell::new(backend))), Some(events)),
        None => (None, None),
    };
    let mut state = State {
        display: dh.clone(),
        handle: event_loop.handle(),
        signal: event_loop.get_signal(),
        backend,
        offscreen,
        stream,
        remote_buttons: Vec::new(),
        remote_keys: Vec::new(),
        settings,
        output,
        compositor,
        shell,
        xwayland_shell,
        shm,
        dmabuf,
        _output_manager: output_manager,
        seat_state,
        seat,
        data_device,
        popups: PopupManager::default(),
        space,
        xwm: None,
        child: None,
        launch: Some(launch),
        pipeline: None,
        viewport: None,
        pointer: (0.0, 0.0).into(),
        cursor: CursorImageStatus::default_named(),
        buttons: Vec::new(),
        fullscreen_key: false,
        dirty: true,
        started: Instant::now(),
        last_metrics: None,
        metrics_path,
        error: None,
        game_exit: None,
        clients: Default::default(),
    };

    // Smithay gives Xwayland a clean environment. Keep the same GPU
    // selection as the compositor so the exported buffers can be imported.
    let gpu_env = [
        "DRI_PRIME",
        "GBM_BACKEND",
        "__NV_PRIME_RENDER_OFFLOAD",
        "__EGL_VENDOR_LIBRARY_FILENAMES",
    ]
    .into_iter()
    .filter_map(|key| {
        let value = match key {
            "DRI_PRIME" => prime
                .as_ref()
                .map(OsString::from)
                .or_else(|| std::env::var_os(key)),
            _ => std::env::var_os(key),
        };
        value.map(|value| (key, value))
    });
    let (xwayland, client) = XWayland::spawn(
        &dh,
        None,
        gpu_env,
        std::iter::empty::<String>(),
        true,
        Stdio::inherit(),
        Stdio::inherit(),
        |_| (),
    )?;
    event_loop
        .handle()
        .insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready {
                x11_socket,
                display_number,
            } => {
                match X11Wm::start_wm(
                    state.handle.clone(),
                    &state.display,
                    x11_socket,
                    client.clone(),
                ) {
                    Ok(wm) => state.xwm = Some(wm),
                    Err(error) => {
                        state.fail(anyhow!("无法启动原生 X11 窗口管理：{error}"));
                        return;
                    }
                }
                let mut launch = state.launch.take().unwrap();
                launch.env("DISPLAY", format!(":{display_number}"));
                eprintln!(
                    "GalScaler native: Xwayland :{display_number}; {}",
                    state.settings.filter.name()
                );
                match launch.spawn() {
                    Ok(child) => state.child = Some(child),
                    Err(error) => state.fail(error.into()),
                }
            }
            XWaylandEvent::Error => state.fail(anyhow!("原生会话的 Xwayland 启动失败")),
        })
        .map_err(|e| anyhow!("{}", e.error))?;

    if let Some(events) = events {
        event_loop
            .handle()
            .insert_source(events, |event, _, state| match event {
                WinitEvent::Input(event) => state.input(event),
                WinitEvent::Resized { .. } => state
                    .backend
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .window()
                    .request_redraw(),
                WinitEvent::Redraw => {
                    let backend = state.backend.as_ref().unwrap().clone();
                    if let Err(error) = state.draw(&mut backend.borrow_mut()) {
                        state.fail(error);
                    }
                }
                WinitEvent::CloseRequested => state.close_game(),
                WinitEvent::Focus(false) => state.release_input(),
                _ => (),
            })
            .map_err(|e| anyhow!("{}", e.error))?;
    }
    let interval = Duration::from_secs_f64(1.0 / state.settings.fps as f64);
    let mut read_rects_at = Instant::now();
    event_loop
        .handle()
        .insert_source(Timer::from_duration(interval), move |_, _, state| {
            if let Some(child) = &mut state.child {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        state.game_exit = Some(status);
                        state.signal.stop();
                    }
                    Ok(None) => (),
                    Err(error) => state.fail(error.into()),
                }
            }
            if Instant::now() >= read_rects_at {
                match protocol::read_client_rects(&rect_path) {
                    Ok(Some(rects)) if state.clients != rects => {
                        state.clients = rects;
                        state.dirty = true;
                    }
                    Ok(_) => (),
                    Err(error) => state.fail(error),
                }
                read_rects_at = Instant::now() + Duration::from_millis(33);
            }
            state.space.refresh();
            state.popups.cleanup();
            for window in state.space.elements() {
                window.send_frame(
                    &state.output,
                    state.started.elapsed(),
                    Some(Duration::ZERO),
                    |_, _| Some(state.output.clone()),
                );
            }
            if let Some(stream) = &mut state.stream {
                stream.release_idle_capture();
            }
            if let Some(backend) = &state.backend {
                backend.borrow().window().request_redraw();
            } else if let Some(window) = state.primary() {
                let mut renderer = state.offscreen.take().unwrap();
                let source = state.client_area(&window).unwrap();
                let result = state.render_game(&mut renderer, source);
                state.offscreen = Some(renderer);
                if let Err(error) = result {
                    state.fail(error);
                } else if let Some(pipeline) = &mut state.pipeline {
                    pipeline.metrics.display = pipeline.metrics.enhanced;
                    if state
                        .last_metrics
                        .is_none_or(|last| last.elapsed() >= Duration::from_secs(1))
                    {
                        if let Err(error) = save_json(&state.metrics_path, &pipeline.metrics) {
                            state.fail(error);
                        }
                        state.last_metrics = Some(Instant::now());
                    }
                }
            }
            let _ = state.display.flush_clients();
            TimeoutAction::ToDuration(interval)
        })
        .map_err(|e| anyhow!("{}", e.error))?;
    event_loop.run(None, &mut state, |_| ())?;
    if let Some(error) = state.error {
        return Err(error);
    }
    if let Some(status) = state.game_exit {
        anyhow::ensure!(status.success(), "游戏启动程序退出：{status}");
    }
    Ok(())
}

impl State {
    fn release_input(&mut self) {
        let keyboard = self.seat.get_keyboard().unwrap();
        for key in keyboard.pressed_keys() {
            if self.remote_keys.contains(&key.raw()) {
                continue;
            }
            keyboard.input::<(), _>(
                self,
                key,
                KeyState::Released,
                SERIAL_COUNTER.next_serial(),
                InputTime::now(),
                |_, _, key| {
                    if key.modified_sym().raw() == keysyms::KEY_F11 {
                        FilterResult::Intercept(())
                    } else {
                        FilterResult::Forward
                    }
                },
            );
        }
        self.fullscreen_key = false;
        let pointer = self.seat.get_pointer().unwrap();
        for button in std::mem::take(&mut self.buttons) {
            if self.remote_buttons.contains(&button) {
                continue;
            }
            pointer.button(
                self,
                &ButtonEvent {
                    button,
                    state: ButtonState::Released,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: InputTime::now(),
                },
            );
        }
        pointer.frame(self);
    }

    fn fail(&mut self, error: anyhow::Error) {
        self.error = Some(error);
        self.signal.stop();
    }

    fn primary(&self) -> Option<Window> {
        self.space
            .elements()
            .filter(|w| {
                self.client_area(w)
                    .is_some_and(|r| r.size.w >= 64 && r.size.h >= 64)
                    && w.wl_surface().is_some()
            })
            .max_by_key(|w| {
                let size = self.client_area(w).unwrap().size;
                i64::from(size.w) * i64::from(size.h)
            })
            .cloned()
    }

    fn client_area(&self, window: &Window) -> Option<Rectangle<i32, Logical>> {
        if let Some(surface) = window.x11_surface() {
            self.clients.get(&surface.window_id()).copied()
        } else {
            self.space.element_geometry(window)
        }
    }

    fn focus(&mut self, window: &Window) {
        self.space.raise_element(window, true);
        self.seat.get_keyboard().unwrap().set_focus(
            self,
            Some(protocol::Focus(window.clone())),
            SERIAL_COUNTER.next_serial(),
        );
        for candidate in self.space.elements() {
            candidate.set_activated(candidate == window);
            if let Some(toplevel) = candidate.toplevel() {
                toplevel.send_pending_configure();
            }
        }
    }

    fn close_game(&mut self) {
        if let Some(window) = self.primary() {
            if let Some(toplevel) = window.toplevel() {
                toplevel.send_close();
            }
            if let Some(x11) = window.x11_surface() {
                let _ = x11.close();
            }
        } else {
            self.signal.stop();
        }
    }

    fn draw(&mut self, backend: &mut WinitGraphicsBackend<GlesRenderer>) -> Result<()> {
        let display_size = backend.window_size();
        if display_size.w == 0 || display_size.h == 0 {
            return Ok(());
        }
        if let Some(window) = self.primary() {
            let source = self.client_area(&window).unwrap();
            self.viewport = Some(View::new(source, display_size));
            let viewport = self.viewport.unwrap();
            let inside = viewport.pointer(self.pointer).is_some();
            let cursor_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = match &self.cursor
            {
                CursorImageStatus::Surface(surface) if inside => {
                    backend.window().set_cursor_visible(false);
                    let hotspot = smithay::wayland::compositor::with_states(surface, |states| {
                        states
                            .data_map
                            .get::<CursorImageSurfaceData>()
                            .map(|c| c.lock().unwrap().hotspot)
                            .unwrap_or_default()
                    });
                    let scale = viewport.target.size.w as f64 / source.size.w as f64;
                    let location = (
                        self.pointer.x - hotspot.x as f64 * scale,
                        self.pointer.y - hotspot.y as f64 * scale,
                    );
                    render_elements_from_surface_tree(
                        backend.renderer(),
                        surface,
                        (location.0.round() as i32, location.1.round() as i32),
                        scale,
                        1.0,
                        Kind::Cursor,
                    )
                }
                CursorImageStatus::Hidden if inside => {
                    backend.window().set_cursor_visible(false);
                    Vec::new()
                }
                status => {
                    backend.window().set_cursor_visible(true);
                    let icon = if inside {
                        if let CursorImageStatus::Named(icon) = status {
                            *icon
                        } else {
                            Default::default()
                        }
                    } else {
                        Default::default()
                    };
                    backend.window().set_cursor(Cursor::Icon(icon));
                    Vec::new()
                }
            };
            self.render_game(backend.renderer(), source)?;
            let pipeline = self.pipeline.as_mut().unwrap();
            let (renderer, mut target) = backend.bind()?;
            let mut frame = renderer.render(&mut target, display_size, Transform::Flipped180)?;
            let damage = [Rectangle::from_size(display_size)];
            frame.clear([0.0, 0.0, 0.0, 1.0].into(), &damage)?;
            frame.render_texture_from_to(
                &pipeline.enhanced,
                Rectangle::from_size(
                    (
                        pipeline.enhanced_size.w as f64,
                        pipeline.enhanced_size.h as f64,
                    )
                        .into(),
                ),
                viewport.target,
                &damage,
                &[],
                Transform::Normal,
                1.0,
                None,
                &[],
            )?;
            draw_render_elements(&mut frame, 1.0, &cursor_elements, &damage)?;
            let _frame_done = frame.finish()?;
            drop(target);
            backend.submit(Some(&damage))?;
            pipeline.metrics.display = [display_size.w, display_size.h];
            if self
                .last_metrics
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(1))
            {
                save_json(&self.metrics_path, &pipeline.metrics)?;
                self.last_metrics = Some(Instant::now());
                let title = window
                    .x11_surface()
                    .map(|s| s.title())
                    .unwrap_or_else(|| "游戏".into());
                backend.window().set_title(&format!(
                    "GalScaler — {title} [{}]",
                    self.settings.filter.name()
                ));
            }
        } else {
            self.viewport = None;
            let (renderer, mut target) = backend.bind()?;
            let mut frame = renderer.render(&mut target, display_size, Transform::Flipped180)?;
            let damage = [Rectangle::from_size(display_size)];
            frame.clear([0.0, 0.0, 0.0, 1.0].into(), &damage)?;
            let _frame_done = frame.finish()?;
            drop(target);
            backend.submit(Some(&damage))?;
        }
        Ok(())
    }

    fn render_game(
        &mut self,
        renderer: &mut GlesRenderer,
        source: Rectangle<i32, Logical>,
    ) -> Result<()> {
        let configured = (
            self.settings.output.width as i32,
            self.settings.output.height as i32,
        )
            .into();
        let limit = fit(source.size, configured).size;
        let streaming = self
            .stream
            .as_ref()
            .and_then(|stream| stream.resolution(source.size, limit));
        // Publish launch metrics once, then leave the offscreen pipeline
        // unloaded while no one can see its output.
        if self.backend.is_none()
            && self.stream.is_some()
            && streaming.is_none()
            && self.last_metrics.is_some()
        {
            if self.pipeline.take().is_some() {
                renderer.cleanup_texture_cache()?;
            }
            return Ok(());
        }
        let enhanced = if self.backend.is_some() {
            limit
        } else {
            streaming.unwrap_or_else(|| {
                let native = (source.size.w.min(limit.w), source.size.h.min(limit.h)).into();
                fit(source.size, native).size
            })
        };
        if self
            .pipeline
            .as_ref()
            .is_none_or(|p| p.source_size != source.size)
        {
            self.pipeline = Some(Pipeline::new(
                renderer,
                source.size,
                enhanced,
                self.settings.filter,
            )?);
            self.dirty = true;
            eprintln!(
                "GalScaler native: source {}x{} -> enhanced {:?}",
                source.size.w,
                source.size.h,
                self.pipeline.as_ref().unwrap().metrics.enhanced
            );
        }
        let pipeline = self.pipeline.as_mut().unwrap();
        if pipeline.enhanced_size != enhanced {
            pipeline.resize(renderer, enhanced)?;
            self.dirty = true;
        }
        let updated = self.dirty;
        if updated {
            let mut elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = Vec::new();
            for window in self.space.elements().rev() {
                let location = self.space.element_location(window).unwrap() - source.loc;
                elements.extend(window.render_elements(
                    renderer,
                    location.to_physical_precise_round(1.0),
                    1.0.into(),
                    1.0,
                ));
            }
            let mut target = renderer.bind(&mut pipeline.scene)?;
            let size = (source.size.w, source.size.h).into();
            let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
            let damage = [Rectangle::from_size(size)];
            frame.clear([0.0, 0.0, 0.0, 1.0].into(), &damage)?;
            draw_render_elements(&mut frame, 1.0, &elements, &damage)?;
            let _frame_done = frame.finish()?;
            drop(target);
            pipeline.process(renderer)?;
            self.dirty = false;
        }
        if let (Some(stream), Some(size)) = (&mut self.stream, streaming) {
            stream.capture(renderer, pipeline, size, updated)?;
        }
        Ok(())
    }

    fn remote_input(&mut self, event: stream::Input) {
        use stream::Input;
        let pointer = self.seat.get_pointer().unwrap();
        let time = InputTime::now();
        match event {
            Input::Pointer {
                x,
                y,
                button,
                pressed,
            } if (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y) => {
                let Some(window) = self.primary() else { return };
                let source = self.client_area(&window).unwrap().to_f64();
                let location = (
                    source.loc.x + x * source.size.w,
                    source.loc.y + y * source.size.h,
                )
                    .into();
                let target = self.surface_under(location);
                pointer.motion(
                    self,
                    target,
                    &MotionEvent {
                        location,
                        time,
                        serial: SERIAL_COUNTER.next_serial(),
                    },
                );
                pointer.frame(self);
                if let (Some(button), Some(pressed)) = (button, pressed) {
                    let button = match button {
                        stream::Mouse::Left => 272,
                        stream::Mouse::Right => 273,
                    };
                    if pressed && !self.remote_buttons.contains(&button) {
                        if !pointer.is_grabbed() {
                            self.focus(&window);
                        }
                        self.remote_buttons.push(button);
                        if !self.buttons.contains(&button) {
                            pointer.button(
                                self,
                                &ButtonEvent {
                                    button,
                                    state: ButtonState::Pressed,
                                    time,
                                    serial: SERIAL_COUNTER.next_serial(),
                                },
                            );
                        }
                    } else if !pressed && self.remote_buttons.contains(&button) {
                        self.remote_buttons.retain(|&b| b != button);
                        if !self.buttons.contains(&button) {
                            pointer.button(
                                self,
                                &ButtonEvent {
                                    button,
                                    state: ButtonState::Released,
                                    time,
                                    serial: SERIAL_COUNTER.next_serial(),
                                },
                            );
                        }
                    }
                    pointer.frame(self);
                }
            }
            Input::Scroll { steps } => {
                let steps = i32::from(steps.clamp(-8, 8));
                pointer.axis(
                    self,
                    AxisFrame::new(time)
                        .source(AxisSource::Wheel)
                        .value(Axis::Vertical, f64::from(steps) * 15.0)
                        .v120(Axis::Vertical, steps * 120),
                );
                pointer.frame(self);
            }
            Input::Key { key, pressed } => {
                let Some(window) = self.primary() else { return };
                self.focus(&window);
                let evdev: u32 = match key {
                    stream::Key::Enter => 28,
                    stream::Key::Escape => 1,
                    stream::Key::Space => 57,
                    stream::Key::Control => 29,
                    stream::Key::PageUp => 104,
                    stream::Key::PageDown => 109,
                };
                let keyboard = self.seat.get_keyboard().unwrap();
                let code = evdev + 8;
                let already_remote = self.remote_keys.contains(&code);
                if pressed && !already_remote && !keyboard.pressed_keys().contains(&code.into()) {
                    self.remote_keys.push(code);
                    self.remote_key(code, KeyState::Pressed, time);
                } else if !pressed && already_remote {
                    self.remote_keys.retain(|&key| key != code);
                    self.remote_key(code, KeyState::Released, time);
                }
            }
            Input::Release => {
                for key in std::mem::take(&mut self.remote_keys) {
                    self.remote_key(key, KeyState::Released, time);
                }
                for button in std::mem::take(&mut self.remote_buttons) {
                    if !self.buttons.contains(&button) {
                        pointer.button(
                            self,
                            &ButtonEvent {
                                button,
                                state: ButtonState::Released,
                                time,
                                serial: SERIAL_COUNTER.next_serial(),
                            },
                        );
                    }
                }
                pointer.frame(self);
            }
            _ => (),
        }
    }

    fn remote_key(&mut self, key: u32, state: KeyState, time: InputTime) {
        self.seat.get_keyboard().unwrap().input::<(), _>(
            self,
            key.into(),
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |_, _, _| FilterResult::Forward,
        );
    }

    fn surface_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space
            .element_under(point)
            .and_then(|(window, location)| {
                window
                    .surface_under(point - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, offset)| (surface, (offset + location).to_f64()))
            })
    }

    fn input<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                if self.remote_keys.contains(&event.key_code().raw()) {
                    return;
                }
                let state = event.state();
                let fullscreen = self.seat.get_keyboard().unwrap().input::<bool, _>(
                    self,
                    event.key_code(),
                    state,
                    SERIAL_COUNTER.next_serial(),
                    event.time(),
                    |data, _, key| {
                        if key.modified_sym().raw() == keysyms::KEY_F11 {
                            let toggle = state == KeyState::Pressed && !data.fullscreen_key;
                            data.fullscreen_key = state == KeyState::Pressed;
                            FilterResult::Intercept(toggle)
                        } else {
                            FilterResult::Forward
                        }
                    },
                );
                if fullscreen == Some(true) {
                    let backend = self.backend.as_ref().unwrap().borrow();
                    let fullscreen = backend
                        .window()
                        .fullscreen()
                        .is_none()
                        .then_some(Fullscreen::Borderless(None));
                    backend.window().set_fullscreen(fullscreen);
                }
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                let size = self.backend.as_ref().unwrap().borrow().window_size();
                let position = event.position_transformed((size.w, size.h).into());
                self.pointer = (position.x, position.y).into();
                if let Some(view) = self.viewport {
                    let pointer = self.seat.get_pointer().unwrap();
                    if let Some(location) = view.pointer(self.pointer) {
                        let under = self.surface_under(location);
                        pointer.motion(
                            self,
                            under,
                            &MotionEvent {
                                location,
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time(),
                            },
                        );
                    } else {
                        pointer.motion(
                            self,
                            None,
                            &MotionEvent {
                                location: pointer.current_location(),
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time(),
                            },
                        );
                    }
                    pointer.frame(self);
                }
            }
            InputEvent::PointerButton { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                if !pointer.is_grabbed()
                    && self
                        .viewport
                        .is_none_or(|view| view.pointer(self.pointer).is_none())
                {
                    return;
                }
                if event.state() == ButtonState::Pressed
                    && !pointer.is_grabbed()
                    && let Some((window, _)) = self
                        .space
                        .element_under(pointer.current_location())
                        .map(|(w, l)| (w.clone(), l))
                {
                    self.focus(&window);
                }
                match event.state() {
                    ButtonState::Pressed => self.buttons.push(event.button_code()),
                    ButtonState::Released => self.buttons.retain(|b| *b != event.button_code()),
                }
                if self.remote_buttons.contains(&event.button_code()) {
                    return;
                }
                pointer.button(
                    self,
                    &ButtonEvent {
                        button: event.button_code(),
                        state: event.state(),
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event, .. } => {
                let mut frame = AxisFrame::new(event.time()).source(event.source());
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    let value = event
                        .amount(axis)
                        .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) / 8.0);
                    if value != 0.0 {
                        frame = frame.value(axis, value);
                    }
                    if let Some(discrete) = event.amount_v120(axis) {
                        frame = frame.v120(axis, discrete as i32);
                    }
                }
                let pointer = self.seat.get_pointer().unwrap();
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => (),
        }
    }
}
