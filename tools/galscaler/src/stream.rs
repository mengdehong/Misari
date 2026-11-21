use crate::render::{Pipeline, fit};
use anyhow::{Context, Result, anyhow, bail, ensure};
use galscaler::api::{FRAME_LIMIT, Reply, frame};
use serde::{Deserialize, Serialize};
use smithay::{
    backend::{
        allocator::Fourcc,
        egl::{EGLContext, fence::EGLFence},
        renderer::{
            Bind, ExportMem, Frame, Offscreen, Renderer,
            gles::{GlesRenderer, GlesTexture},
        },
    },
    reexports::calloop::channel::SyncSender,
    utils::{Logical, Physical, Rectangle, Size, Transform},
};
use std::{
    collections::BTreeMap,
    ffi::{CString, c_char, c_int, c_void},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{Notify, mpsc},
    time::{interval, sleep, timeout},
};

unsafe extern "C" {
    fn gs_peer() -> c_int;
    fn gs_track(
        peer: c_int,
        mid: *const c_char,
        payload: c_int,
        video: bool,
        delay_extension: u8,
    ) -> c_int;
    fn gs_controls(peer: c_int) -> c_int;
    fn rtcSetRemoteDescription(peer: c_int, sdp: *const c_char, kind: *const c_char) -> c_int;
    fn rtcSetLocalDescription(peer: c_int, kind: *const c_char) -> c_int;
    fn rtcGetLocalDescription(peer: c_int, buffer: *mut c_char, size: c_int) -> c_int;
    fn rtcSetGatheringStateChangeCallback(
        peer: c_int,
        callback: extern "C" fn(c_int, c_int, *mut c_void),
    ) -> c_int;
    fn rtcSetUserPointer(id: c_int, ptr: *mut c_void);
    fn rtcChainPliHandler(track: c_int, callback: extern "C" fn(c_int, *mut c_void)) -> c_int;
    fn rtcSetOpenCallback(id: c_int, callback: extern "C" fn(c_int, *mut c_void)) -> c_int;
    fn rtcSetClosedCallback(id: c_int, callback: extern "C" fn(c_int, *mut c_void)) -> c_int;
    fn rtcSetAvailableCallback(id: c_int, callback: extern "C" fn(c_int, *mut c_void)) -> c_int;
    fn rtcDeletePeerConnection(peer: c_int) -> c_int;
    fn rtcDelete(id: c_int) -> c_int;
    fn rtcIsOpen(id: c_int) -> bool;
    fn rtcIsClosed(id: c_int) -> bool;
    fn rtcGetAvailableAmount(id: c_int) -> c_int;
    fn rtcReceiveMessage(id: c_int, buffer: *mut c_char, size: *mut c_int) -> c_int;
    fn rtcSendMessage(id: c_int, bytes: *const c_char, size: c_int) -> c_int;
    fn gs_nv12_new() -> *mut c_void;
    fn gs_nv12_free(converter: *mut c_void);
    fn gs_nv12_target_new(surface: *const c_void) -> *mut c_void;
    fn gs_nv12_target_free(target: *mut c_void);
    fn gs_nv12_draw(
        converter: *mut c_void,
        target: *const c_void,
        rgb: u32,
        w: i32,
        h: i32,
    ) -> bool;
    fn gs_nv12_submit(finish: bool);
    fn gs_va_surface_new(
        device: *const c_char,
        w: i32,
        h: i32,
        other: *const c_void,
    ) -> *mut c_void;
    fn gs_va_surface_free(surface: *mut c_void);
    fn gs_va_surface_pool(surface: *const c_void) -> *const c_void;
    fn av_frame_is_writable(surface: *const c_void) -> c_int;
    fn gs_encoder_new(
        device: *const c_char,
        surface: *const c_void,
        w: i32,
        h: i32,
        fps: i32,
    ) -> *mut c_void;
    fn gs_encoder_free(encoder: *mut c_void);
    fn gs_encode_video(
        encoder: *mut c_void,
        track: c_int,
        rgba: *const u8,
        timestamp: i64,
        keyframe: bool,
    ) -> c_int;
    fn gs_encode_va(
        encoder: *mut c_void,
        track: c_int,
        surface: *const c_void,
        timestamp: i64,
        keyframe: bool,
    ) -> c_int;
    fn gs_audio_new() -> *mut c_void;
    fn opus_encoder_destroy(encoder: *mut c_void);
    fn gs_encode_audio(encoder: *mut c_void, track: c_int, pcm: *const i16, timestamp: u32);
}

// The worker owns the encoder and peer. The compositor only exports frames
// when their bounded queue has room and receives input for its private seat.
pub struct Stream {
    pub sink: String,
    audio: Child,
    viewport: Arc<AtomicU32>,
    frames: mpsc::Sender<Video>,
    target: Option<(Size<i32, Physical>, GlesTexture)>,
    device: Option<CString>,
    use_dmabuf: Arc<AtomicBool>,
    events: Arc<PeerEvents>,
    va_capture: Option<VaCapture>,
    last_frame: Option<(Instant, Size<i32, Physical>)>,
    settle_frames: u8,
    fps: u32,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Input {
    Pointer {
        x: f64,
        y: f64,
        button: Option<Mouse>,
        pressed: Option<bool>,
    },
    Scroll {
        steps: i8,
    },
    Key {
        key: Key,
        pressed: bool,
    },
    Release,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mouse {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Enter,
    Escape,
    Space,
    Control,
    PageUp,
    PageDown,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Control {
    Viewport { width: u32, height: u32 },
    Input(Input),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub sdp: String,
    pub width: u32,
    pub height: u32,
}

struct Video {
    size: Size<i32, Physical>,
    data: VideoData,
    at: Instant,
}

enum VideoData {
    Rgba(Vec<u8>),
    Vaapi(Arc<VaSurface>, EGLFence),
}

struct VaSurface(NonNull<c_void>);

// The AVFrame is immutable. The encoder clones its AVBuffer refs, and the
// renderer writes its pixels only when neither the queue nor FFmpeg holds it.
unsafe impl Send for VaSurface {}
unsafe impl Sync for VaSurface {}

impl Drop for VaSurface {
    fn drop(&mut self) {
        unsafe { gs_va_surface_free(self.0.as_ptr()) }
    }
}

struct VaSlot {
    surface: Arc<VaSurface>,
    target: NonNull<c_void>,
}

struct VaCapture {
    context: EGLContext,
    converter: NonNull<c_void>,
    slots: Vec<VaSlot>,
    size: Size<i32, Physical>,
}

impl VaCapture {
    fn new(renderer: &GlesRenderer, device: &CString, size: Size<i32, Physical>) -> Result<Self> {
        let egl = renderer.egl_context();
        let context = EGLContext::new_shared(egl.display(), egl)?;
        unsafe { context.make_current()? };
        let converter = NonNull::new(unsafe { gs_nv12_new() }).context("无法初始化 NV12 着色器")?;
        let mut capture = Self {
            context,
            converter,
            slots: Vec::new(),
            size,
        };
        // One surface for GL, one for the queue, and one for the encoder.
        for _ in 0..3 {
            let existing = capture
                .slots
                .first()
                .map_or(std::ptr::null(), |slot| slot.surface.0.as_ptr());
            let surface = Arc::new(VaSurface(
                NonNull::new(unsafe {
                    gs_va_surface_new(device.as_ptr(), size.w, size.h, existing)
                })
                .context("无法分配 VA-API NV12 表面")?,
            ));
            let target = NonNull::new(unsafe { gs_nv12_target_new(surface.0.as_ptr()) })
                .context("VA-API NV12 表面无法作为 GLES 渲染目标")?;
            capture.slots.push(VaSlot { surface, target });
        }
        Ok(capture)
    }

    fn draw(
        &mut self,
        renderer: &mut GlesRenderer,
        rgb: &GlesTexture,
    ) -> Result<Option<VideoData>> {
        let Some(slot) = self.slots.iter_mut().find_map(|slot| {
            let surface = Arc::get_mut(&mut slot.surface)?;
            (unsafe { av_frame_is_writable(surface.0.as_ptr()) == 1 }).then_some(slot)
        }) else {
            return Ok(None);
        };
        let display = self.context.display();
        let acquire = renderer.with_context(|gl| {
            let fence = EGLFence::create(display)?;
            unsafe { gl.Flush() };
            Ok::<_, smithay::backend::egl::Error>(fence)
        })??;
        unsafe { self.context.make_current()? };
        acquire.wait(display)?;
        let drawn = unsafe {
            gs_nv12_draw(
                self.converter.as_ptr(),
                slot.target.as_ptr(),
                rgb.tex_id(),
                self.size.w,
                self.size.h,
            )
        };
        let release = EGLFence::create(display);
        unsafe { gs_nv12_submit(release.is_err()) };
        let release = release?;
        // The next pass may overwrite the RGB texture. Queue the dependency
        // on GL, then hand the same fence to the encoder thread.
        renderer.wait(&release.clone().into())?;
        ensure!(drawn, "GLES 写入 NV12 表面失败");
        Ok(Some(VideoData::Vaapi(slot.surface.clone(), release)))
    }
}

impl Drop for VaCapture {
    fn drop(&mut self) {
        if unsafe { self.context.make_current() }.is_ok() {
            for slot in &self.slots {
                unsafe { gs_nv12_target_free(slot.target.as_ptr()) };
            }
            unsafe { gs_nv12_free(self.converter.as_ptr()) };
        }
    }
}

impl Stream {
    pub fn start(
        socket: PathBuf,
        device: Option<&Path>,
        fps: u32,
        input: SyncSender<Input>,
    ) -> Result<Self> {
        let sink = format!("galscaler-{}", std::process::id());
        let mut audio = Command::new("pw-loopback")
            .args(["-n", &sink, "-c", "2", "-l", "20", "-i"])
            .arg(format!(r#"{{"media.class":"Audio/Sink","node.name":"{sink}","node.description":"GalScaler"}}"#))
            .args(["-o", r#"{"node.passive":true}"#])
            .stdin(Stdio::null())
            .spawn()
            .context("无法创建 PipeWire 游戏音频输出（需要 pw-loopback）")?;
        let ready = || {
            Command::new("pactl")
                .args(["list", "sinks", "short"])
                .output()
                .is_ok_and(|output| {
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout)
                            .lines()
                            .any(|line| line.split_whitespace().nth(1) == Some(&sink))
                })
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready() {
            if Instant::now() >= deadline || audio.try_wait().ok().flatten().is_some() {
                let _ = audio.kill();
                let _ = audio.wait();
                bail!("PipeWire 游戏音频输出未就绪，请检查 pipewire-pulse");
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        let (frames, video) = mpsc::channel(1);
        let viewport = Arc::new(AtomicU32::new(0));
        let fps = fps.min(60);
        let device = device.and_then(|path| CString::new(path.as_os_str().as_encoded_bytes()).ok());
        let use_dmabuf = Arc::new(AtomicBool::new(device.is_some()));
        let events = Arc::new(PeerEvents::default());
        let worker = Worker {
            sink: sink.clone(),
            device: device.clone(),
            use_dmabuf: use_dmabuf.clone(),
            events: events.clone(),
            input,
            viewport: viewport.clone(),
            video,
            fps,
        };
        let stream = Self {
            sink,
            audio,
            viewport,
            frames,
            target: None,
            device,
            use_dmabuf,
            events,
            va_capture: None,
            last_frame: None,
            settle_frames: 0,
            fps,
        };
        let listener = std::os::unix::net::UnixListener::bind(&socket)
            .with_context(|| format!("无法创建串流 socket {}", socket.display()))?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        std::thread::Builder::new()
            .name("galscaler-stream".into())
            .spawn(move || {
                let run = || -> Result<()> {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?
                        .block_on(async { worker.run(UnixListener::from_std(listener)?).await })
                };
                if let Err(error) = run() {
                    eprintln!("GalScaler stream: {error:#}");
                }
                let _ = std::fs::remove_file(socket);
            })?;
        Ok(stream)
    }

    pub fn resolution(
        &self,
        source: Size<i32, Logical>,
        ceiling: Size<i32, Physical>,
    ) -> Option<Size<i32, Physical>> {
        let packed = self.viewport.load(Relaxed);
        (packed != 0).then(|| fitted_size(source, ceiling, packed))
    }

    pub fn release_idle_capture(&mut self) {
        if self.viewport.load(Relaxed) == 0 {
            self.va_capture = None;
            self.target = None;
            self.last_frame = None;
        }
    }

    pub fn capture(
        &mut self,
        renderer: &mut GlesRenderer,
        pipeline: &Pipeline,
        size: Size<i32, Physical>,
        updated: bool,
    ) -> Result<()> {
        if updated
            || self.last_frame.is_none_or(|(_, previous)| previous != size)
            || self.events.keyframe.load(Relaxed)
        {
            // Keep the original cadence briefly after a change so the receiver
            // can present it and the encoder can refine the new picture.
            self.settle_frames = 5;
        }
        let now = Instant::now();
        if let Some((last, _)) = self.last_frame {
            let elapsed = now.duration_since(last);
            if elapsed < Duration::from_secs_f64(0.9 / f64::from(self.fps))
                || (self.settle_frames == 0 && elapsed < Duration::from_millis(250))
            {
                return Ok(());
            }
        }
        let Ok(slot) = self.frames.try_reserve() else {
            return Ok(());
        };
        if self.use_dmabuf.load(Relaxed) {
            let capture = (|| -> Result<Option<VideoData>> {
                if self
                    .va_capture
                    .as_ref()
                    .is_none_or(|capture| capture.size != size)
                {
                    self.va_capture = Some(VaCapture::new(
                        renderer,
                        self.device.as_ref().unwrap(),
                        size,
                    )?);
                }
                self.va_capture
                    .as_mut()
                    .unwrap()
                    .draw(renderer, &pipeline.enhanced)
            })();
            match capture {
                Ok(Some(data)) => {
                    slot.send(Video {
                        size,
                        data,
                        at: now,
                    });
                    self.last_frame = Some((now, size));
                    self.settle_frames = self.settle_frames.saturating_sub(1);
                    return Ok(());
                }
                Ok(None) => return Ok(()),
                Err(error) => {
                    eprintln!("GalScaler stream: DMA-BUF 不可用，切换至回读：{error:#}");
                    self.use_dmabuf.store(false, Relaxed);
                }
            }
        }
        self.va_capture = None;
        let texture = if size == pipeline.enhanced_size {
            &pipeline.enhanced
        } else {
            if self
                .target
                .as_ref()
                .is_none_or(|(current, _)| *current != size)
            {
                self.target = Some((
                    size,
                    renderer.create_buffer(Fourcc::Abgr8888, (size.w, size.h).into())?,
                ));
            }
            let (_, texture) = self.target.as_mut().unwrap();
            let mut target = renderer.bind(texture)?;
            let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
            let rect = Rectangle::from_size(size);
            frame.render_texture_from_to(
                &pipeline.enhanced,
                Rectangle::from_size(
                    pipeline
                        .enhanced_size
                        .to_f64()
                        .to_logical(1.0)
                        .to_buffer(1.0, Transform::Normal),
                ),
                rect,
                &[rect],
                &[],
                Transform::Normal,
                1.0,
                None,
                &[],
            )?;
            let _ = frame.finish()?;
            drop(target);
            texture
        };
        let mapping = renderer.copy_texture(
            texture,
            Rectangle::from_size((size.w, size.h).into()),
            Fourcc::Abgr8888,
        )?;
        let pixels = renderer.map_texture(&mapping)?.to_vec();
        slot.send(Video {
            size,
            data: VideoData::Rgba(pixels),
            at: now,
        });
        self.last_frame = Some((now, size));
        self.settle_frames = self.settle_frames.saturating_sub(1);
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.audio.kill();
        let _ = self.audio.wait();
    }
}

fn viewport(width: u32, height: u32) -> u32 {
    (width.clamp(64, 1920) << 16) | height.clamp(64, 1080)
}

fn fitted_size(
    source: Size<i32, Logical>,
    cap: Size<i32, Physical>,
    packed: u32,
) -> Size<i32, Physical> {
    let bounds = (
        ((packed >> 16) as i32).min(cap.w),
        ((packed & 0xffff) as i32).min(cap.h),
    );
    let size = fit(source, bounds.into()).size;
    ((size.w & !1).max(2), (size.h & !1).max(2)).into()
}

struct Worker {
    sink: String,
    device: Option<CString>,
    use_dmabuf: Arc<AtomicBool>,
    events: Arc<PeerEvents>,
    input: SyncSender<Input>,
    viewport: Arc<AtomicU32>,
    video: mpsc::Receiver<Video>,
    fps: u32,
}

impl Worker {
    async fn run(mut self, listener: UnixListener) -> Result<()> {
        let mut peer: Option<Peer> = None;
        let mut audio: Option<Audio> = None;
        let mut encoder = None;
        let mut tick = interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                conn = listener.accept() => {
                    let (mut conn, _) = conn?;
                    let offer = async {
                        let offer: Offer = serde_json::from_slice(&frame(&mut conn).await?)?;
                        Peer::answer(offer, &self.events).await
                    };
                    let reply = match timeout(Duration::from_secs(5), offer).await {
                        Ok(Ok((next, sdp))) => {
                            self.input.send(Input::Release)?;
                            self.viewport.store(0, Relaxed);
                            self.events.keyframe.store(true, Relaxed);
                            peer = Some(next);
                            audio = None;
                            encoder = None;
                            Ok(serde_json::json!({ "sdp": sdp }))
                        }
                        Ok(Err(error)) => Err(error),
                        Err(_) => Err(anyhow!("WebRTC 协商超时")),
                    };
                    let mut bytes = serde_json::to_vec(&Reply::from_result(reply))?;
                    bytes.push(b'\n');
                    let _ = timeout(Duration::from_secs(1), conn.write_all(&bytes)).await;
                }
                _ = async {
                    tokio::select! {
                        _ = tick.tick() => {},
                        _ = self.events.ready.notified() => {},
                    }
                }, if peer.is_some() => {
                    if let Some(active) = &mut peer {
                        if active.closed() || active.last_input.elapsed() > Duration::from_secs(15) {
                            self.viewport.store(0, Relaxed);
                            self.input.send(Input::Release)?;
                            peer = None;
                            audio = None;
                            encoder = None;
                        } else if active.open() {
                            for command in active.controls() {
                                match command {
                                    Control::Viewport { width, height } => active.viewport = viewport(width, height),
                                    Control::Input(input) => {
                                        if self.input.try_send(input).is_err() {
                                            self.input.send(Input::Release)?;
                                            break;
                                        }
                                    }
                                }
                            }
                            if unsafe { rtcGetAvailableAmount(active.dc) } > 0 {
                                self.events.ready.notify_one();
                            }
                            if audio.is_none() {
                                match Audio::start(&self.sink) {
                                    Ok(started) => audio = Some(started),
                                    Err(error) => {
                                        active.report(&format!("{error:#}"));
                                        active.failed = true;
                                        self.viewport.store(0, Relaxed);
                                        self.input.send(Input::Release)?;
                                    }
                                }
                            }
                            if !active.failed {
                                self.viewport.store(active.viewport, Relaxed);
                            }
                        }
                    }
                }
                video = self.video.recv() => {
                    let Some(video) = video else { break };
                    let Some(active) = peer.as_mut().filter(|p| p.open()) else { continue };
                    let dmabuf = matches!(video.data, VideoData::Vaapi(..));
                    if dmabuf && !self.use_dmabuf.load(Relaxed) { continue; }
                    let result = self.video_frame(active, &mut encoder, video);
                    if let Err(error) = result {
                        if dmabuf {
                            eprintln!("GalScaler stream: DMA-BUF 编码失败，切换至回读：{error:#}");
                            self.use_dmabuf.store(false, Relaxed);
                            self.events.keyframe.store(true, Relaxed);
                            encoder = None;
                            continue;
                        }
                        active.report(&format!("{error:#}"));
                        self.viewport.store(0, Relaxed);
                        self.input.send(Input::Release)?;
                        audio = None;
                        encoder = None;
                        active.failed = true;
                    }
                }
                pcm = async {
                    match &mut audio {
                        Some(audio) => audio.frames.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let (Some(active), Some(capture)) = (peer.as_mut(), audio.as_ref()) {
                        if let Some((at, pcm)) = pcm {
                            let timestamp = at.saturating_duration_since(active.start).as_secs_f64() * 48000.0;
                            unsafe { gs_encode_audio(capture.encoder.as_ptr(), active.audio, pcm.as_ptr(), timestamp as u64 as u32); }
                        } else {
                            active.report("游戏音频捕获已断开");
                            active.failed = true;
                            self.viewport.store(0, Relaxed);
                            self.input.send(Input::Release)?;
                            audio = None;
                            encoder = None;
                        }
                    }
                }
            }
        }
        self.viewport.store(0, Relaxed);
        let _ = self.input.try_send(Input::Release);
        Ok(())
    }

    fn video_frame(&self, peer: &Peer, encoder: &mut Option<Encoder>, video: Video) -> Result<()> {
        let surface = match &video.data {
            VideoData::Vaapi(surface, fence) => {
                ensure!(
                    fence.client_wait(Some(Duration::from_secs(1)), false)?,
                    "等待 NV12 表面超时"
                );
                surface.0.as_ptr()
            }
            VideoData::Rgba(_) => std::ptr::null(),
        };
        // A rapid resize can replace the pool while returning to the previous
        // dimensions before the encoder consumes an intermediate frame.
        let pool = if surface.is_null() {
            std::ptr::null()
        } else {
            unsafe { gs_va_surface_pool(surface) }
        };
        if encoder
            .as_ref()
            .is_none_or(|e| e.size != video.size || e.pool != pool)
        {
            let handle = unsafe {
                gs_encoder_new(
                    self.device
                        .as_ref()
                        .map_or(std::ptr::null(), |s| s.as_ptr()),
                    surface,
                    video.size.w,
                    video.size.h,
                    self.fps as i32,
                )
            };
            *encoder = Some(Encoder {
                handle: NonNull::new(handle)
                    .context("无法启动 H.264 硬件编码（VA-API / NVENC）")?,
                size: video.size,
                pool,
            });
        }
        let timestamp =
            (video.at.saturating_duration_since(peer.start).as_secs_f64() * 90000.0) as i64;
        let handle = encoder.as_ref().unwrap().handle.as_ptr();
        let keyframe = self.events.keyframe.swap(false, Relaxed);
        rtc(unsafe {
            match video.data {
                VideoData::Vaapi(surface, _) => {
                    gs_encode_va(handle, peer.video, surface.0.as_ptr(), timestamp, keyframe)
                }
                VideoData::Rgba(pixels) => {
                    gs_encode_video(handle, peer.video, pixels.as_ptr(), timestamp, keyframe)
                }
            }
        })
        .context("H.264 硬件编码失败")?;
        Ok(())
    }
}

struct Encoder {
    handle: NonNull<c_void>,
    size: Size<i32, Physical>,
    pool: *const c_void,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { gs_encoder_free(self.handle.as_ptr()) }
    }
}

struct Audio {
    encoder: NonNull<c_void>,
    child: tokio::process::Child,
    frames: mpsc::Receiver<(Instant, [i16; 1920])>,
    task: tokio::task::JoinHandle<()>,
}

impl Audio {
    fn start(sink: &str) -> Result<Self> {
        let mut child = tokio::process::Command::new("parec")
            .arg(format!("--device={sink}.monitor"))
            .args([
                "--raw",
                "--latency-msec=20",
                "--process-time-msec=20",
                "--channels=2",
                "--rate=48000",
                "--format=s16le",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("无法启动游戏音频采集（需要 parec）")?;
        let encoder = NonNull::new(unsafe { gs_audio_new() }).context("无法启动 Opus 编码")?;
        let mut stdout = child.stdout.take().unwrap();
        let (tx, frames) = mpsc::channel(3);
        let task = tokio::spawn(async move {
            let mut bytes = [0; 3840];
            // Anchor RTP timestamps to the first PCM block, then advance with the sample clock.
            let mut next = None;
            while stdout.read_exact(&mut bytes).await.is_ok() {
                let now = Instant::now() - Duration::from_millis(20);
                let at = next.unwrap_or(now);
                next = Some(at + Duration::from_millis(20));
                let pcm =
                    std::array::from_fn(|i| i16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]));
                if let Err(mpsc::error::TrySendError::Closed(_)) = tx.try_send((at, pcm)) {
                    break;
                }
            }
        });
        Ok(Self {
            encoder,
            child,
            frames,
            task,
        })
    }

    fn stop(&self) {
        self.task.abort();
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.stop();
        let _ = self.child.start_kill();
        unsafe { opus_encoder_destroy(self.encoder.as_ptr()) }
    }
}

struct Peer {
    id: c_int,
    video: c_int,
    audio: c_int,
    dc: c_int,
    viewport: u32,
    gather: Box<AtomicU32>,
    start: Instant,
    last_input: Instant,
    failed: bool,
}

#[derive(Default)]
struct PeerEvents {
    ready: Notify,
    keyframe: AtomicBool,
}

// libdatachannel invokes these on its own threads. Look up the ID so a
// callback racing peer deletion never dereferences freed user data.
static PEER_EVENTS: Mutex<BTreeMap<c_int, Arc<PeerEvents>>> = Mutex::new(BTreeMap::new());

extern "C" fn peer_ready(id: c_int, _: *mut c_void) {
    if let Ok(events) = PEER_EVENTS.lock()
        && let Some(peer) = events.get(&id)
    {
        peer.ready.notify_one();
    }
}

extern "C" fn request_keyframe(id: c_int, _: *mut c_void) {
    if let Ok(events) = PEER_EVENTS.lock()
        && let Some(peer) = events.get(&id)
    {
        peer.keyframe.store(true, Relaxed);
    }
}

extern "C" fn gathered(_: c_int, state: c_int, ptr: *mut c_void) {
    if let Some(state_slot) = unsafe { ptr.cast::<AtomicU32>().as_ref() } {
        state_slot.store(state as u32, Relaxed);
    }
}

impl Peer {
    async fn answer(offer: Offer, events: &Arc<PeerEvents>) -> Result<(Self, String)> {
        let (video_mid, video_payload, delay_extension) = media(&offer.sdp, "video")?;
        let (audio_mid, audio_payload, _) = media(&offer.sdp, "audio")?;
        let sdp = CString::new(offer.sdp)?;
        let now = Instant::now();
        let mut peer = Self {
            id: rtc(unsafe { gs_peer() })?,
            video: -1,
            audio: -1,
            dc: -1,
            viewport: viewport(offer.width, offer.height),
            gather: Box::default(),
            start: now,
            last_input: now,
            failed: false,
        };
        unsafe {
            rtcSetUserPointer(peer.id, (&raw mut *peer.gather).cast());
            rtc(rtcSetGatheringStateChangeCallback(peer.id, gathered))?;
            peer.video = rtc(gs_track(
                peer.id,
                video_mid.as_ptr(),
                video_payload,
                true,
                delay_extension,
            ))?;
            peer.audio = rtc(gs_track(
                peer.id,
                audio_mid.as_ptr(),
                audio_payload,
                false,
                0,
            ))?;
            peer.dc = rtc(gs_controls(peer.id))?;
            {
                let mut registered = PEER_EVENTS.lock().unwrap();
                registered.insert(peer.video, events.clone());
                registered.insert(peer.dc, events.clone());
            }
            rtc(rtcChainPliHandler(peer.video, request_keyframe))?;
            rtc(rtcSetOpenCallback(peer.video, peer_ready))?;
            rtc(rtcSetOpenCallback(peer.dc, peer_ready))?;
            rtc(rtcSetClosedCallback(peer.dc, peer_ready))?;
            rtc(rtcSetAvailableCallback(peer.dc, peer_ready))?;
            rtc(rtcSetRemoteDescription(
                peer.id,
                sdp.as_ptr(),
                c"offer".as_ptr(),
            ))?;
            rtc(rtcSetLocalDescription(peer.id, c"answer".as_ptr()))?;
        }
        while peer.gather.load(Relaxed) != 2 {
            sleep(Duration::from_millis(10)).await;
        }
        let count = rtc(unsafe { rtcGetLocalDescription(peer.id, std::ptr::null_mut(), 0) })?;
        let mut answer = vec![0u8; count as usize];
        rtc(unsafe { rtcGetLocalDescription(peer.id, answer.as_mut_ptr().cast(), count) })?;
        ensure!(answer.pop() == Some(0), "WebRTC 响应缺少结束符");
        Ok((peer, String::from_utf8(answer)?))
    }

    fn open(&self) -> bool {
        !self.failed && unsafe { rtcIsOpen(self.dc) && rtcIsOpen(self.video) }
    }

    fn closed(&self) -> bool {
        unsafe { rtcIsClosed(self.dc) }
    }

    fn controls(&mut self) -> Vec<Control> {
        let mut items = Vec::new();
        for _ in 0..32 {
            let mut buffer = [0u8; 1025];
            let mut size = buffer.len() as c_int;
            if unsafe { rtcReceiveMessage(self.dc, buffer.as_mut_ptr().cast(), &mut size) } < 0 {
                break;
            }
            let bytes = &buffer[..(size.unsigned_abs() as usize).min(buffer.len())];
            if let Ok(item) = serde_json::from_slice(bytes.strip_suffix(&[0]).unwrap_or(bytes)) {
                self.last_input = Instant::now();
                items.push(item);
            }
        }
        items
    }

    fn report(&self, error: &str) {
        eprintln!("GalScaler stream: {error}");
        let message = serde_json::json!({ "error": error }).to_string();
        if let Ok(message) = CString::new(message) {
            unsafe {
                rtcSendMessage(self.dc, message.as_ptr(), -1);
            }
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        {
            let mut events = PEER_EVENTS.lock().unwrap();
            events.remove(&self.video);
            events.remove(&self.dc);
        }
        unsafe {
            for id in [self.dc, self.video, self.audio] {
                if id >= 0 {
                    rtcDelete(id);
                }
            }
            rtcDeletePeerConnection(self.id);
        }
    }
}

fn rtc(code: c_int) -> Result<c_int> {
    ensure!(code >= 0, "WebRTC 错误 {code}");
    Ok(code)
}

fn playout_extension(section: &str) -> Option<u8> {
    section.lines().find_map(|line| {
        let mut words = line.strip_prefix("a=extmap:")?.split_ascii_whitespace();
        let id = words.next()?;
        if words.next()? != "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay" {
            return None;
        }
        let raw = match id.split_once('/') {
            Some((id, "recvonly" | "sendrecv")) => id,
            Some(_) => return None,
            None => id,
        };
        raw.parse::<u8>().ok().filter(|&id| id != 0)
    })
}

fn media(sdp: &str, kind: &str) -> Result<(CString, i32, u8)> {
    let header = format!("{kind} ");
    let section = sdp
        .split("\nm=")
        .skip(1)
        .find(|section| section.starts_with(&header))
        .with_context(|| format!("WebRTC 缺少 {kind} 媒体轨"))?;
    let mid = section
        .lines()
        .find_map(|line| line.strip_prefix("a=mid:"))
        .context("WebRTC 媒体轨缺少 MID")?;
    let mut formats = section.lines().filter_map(|line| {
        let (id, codec) = line.strip_prefix("a=rtpmap:")?.split_once(' ')?;
        let id = id.parse::<u8>().ok().filter(|id| (96..=127).contains(id))?;
        Some((id, codec))
    });
    let (payload, _) = formats
        .find(|(id, codec)| match kind {
            "audio" => codec.eq_ignore_ascii_case("opus/48000/2"),
            _ => {
                let prefix = format!("a=fmtp:{id} ");
                codec.eq_ignore_ascii_case("h264/90000")
                    && section
                        .lines()
                        .filter_map(|line| line.strip_prefix(&prefix))
                        .any(|fmtp| {
                            fmtp.split(';')
                                .any(|value| value.trim() == "packetization-mode=1")
                                && fmtp.split(';').any(|value| {
                                    value.trim().strip_prefix("profile-level-id=").is_some_and(
                                        |profile| {
                                            profile.starts_with("42e0")
                                                || profile.starts_with("42c0")
                                        },
                                    )
                                })
                        })
            }
        })
        .with_context(|| format!("设备不支持串流需要的 {kind} 编码（H.264 / Opus）"))?;
    let delay_extension = if kind == "video" {
        playout_extension(section).unwrap_or(0)
    } else {
        0
    };
    Ok((CString::new(mid)?, payload.into(), delay_extension))
}

pub async fn signal(path: &Path, offer: Offer) -> Result<serde_json::Value> {
    let mut bytes = serde_json::to_vec(&offer)?;
    ensure!(bytes.len() < FRAME_LIMIT, "WebRTC 请求过大");
    bytes.push(b'\n');
    timeout(Duration::from_secs(10), async {
        let mut socket = loop {
            match UnixStream::connect(path).await {
                Ok(socket) => break socket,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    sleep(Duration::from_millis(100)).await;
                }
                Err(error) => return Err(error.into()),
            }
        };
        socket.write_all(&bytes).await?;
        serde_json::from_slice::<Reply>(&frame(socket).await?)?.into_result()
    })
    .await
    .context("串流未就绪，请检查游戏日志")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playout_delay_requires_an_offer_that_can_receive_it() {
        let uri = "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay";
        assert_eq!(playout_extension(&format!("a=extmap:5 {uri}")), Some(5));
        assert_eq!(
            playout_extension(&format!("a=extmap:9/recvonly {uri}")),
            Some(9)
        );
        assert_eq!(
            playout_extension(&format!("a=extmap:5/sendonly {uri}")),
            None
        );
        assert_eq!(
            playout_extension("a=extmap:5 urn:ietf:params:rtp-hdrext:toffset"),
            None
        );
    }

    #[test]
    fn phone_viewport_fits_game_and_h264_limits() {
        let source = (960, 720).into();
        let cap = (2880, 2160).into();
        assert_eq!(
            fitted_size(source, cap, viewport(2376, 1080)),
            (1440, 1080).into()
        );
        assert_eq!(
            fitted_size(source, cap, viewport(1080, 2376)),
            (1080, 810).into()
        );
        assert_eq!(
            fitted_size(source, (960, 720).into(), viewport(2376, 1080)),
            (960, 720).into()
        );
        assert_eq!(
            fitted_size(source, cap, viewport(701, 799)),
            (700, 526).into()
        );
    }
}
