use crate::{MAX_PIXELS, Shared, audio::Audio, frames};
use cef::*;
use memmap2::{MmapMut, MmapOptions};
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fs::File,
    os::fd::{AsRawFd, BorrowedFd},
    path::PathBuf,
    rc::Rc as StateRc,
    sync::{Arc, Weak, atomic::Ordering, mpsc},
};

// Each CEF handler identifies its own browser; old and candidate content coexist.
thread_local! {
    static STATES: RefCell<HashMap<u64, StateRc<BrowserState>>> = RefCell::new(HashMap::new());
}
fn current(id: u64) -> Option<StateRc<BrowserState>> {
    STATES.with(|states| states.borrow().get(&id).cloned())
}
pub fn valid_size(width: i32, height: i32) -> bool {
    width > 0 && height > 0 && width as u64 * height as u64 * 4 <= MAX_PIXELS
}

wrap_task! {
    pub(crate) struct Start {
        id: u64,
        frame: Arc<File>,
        root: PathBuf,
        url: String,
        size: [u32; 2],
        accelerated: bool,
        shared: Arc<Shared>,
    }
    impl Task {
        fn execute(&self) {
            if self.shared.closing.load(Ordering::Acquire) {
                self.shared.closed();
                return;
            }
            let started = self.frame.try_clone().is_ok_and(|frame| start(self.id,
                frame, self.root.clone(), &self.url, self.size, self.accelerated, self.shared.clone()));
            if !started {
                STATES.with(|states| states.borrow_mut().remove(&self.id));
                self.shared.error("creating offscreen browser failed".into());
                self.shared.closed();
            }
        }
    }
}
wrap_task! {
    pub(crate) struct Control { id: u64, shared: Arc<Shared> }
    impl Task {
        fn execute(&self) {
            let values = {
                let mut controls = self.shared.controls.lock().unwrap();
                controls.scheduled = false;
                std::mem::take(&mut controls.queue)
            };
            if let Some(state) = current(self.id) {
                if self.shared.closing.load(Ordering::Acquire) {
                    state.close();
                } else {
                    for (value, _) in values { state.receive(&value); }
                }
            }
        }
    }
}
wrap_task! {
    pub(crate) struct CloseAll;
    impl Task {
        fn execute(&self) {
            let states: Vec<_> = STATES.with(|states| states.borrow().values().cloned().collect());
            for state in states { state.close(); }
        }
    }
}

wrap_task! {
    pub(crate) struct GpuTimeout { shared: Weak<Shared> }
    impl Task {
        fn execute(&self) {
            if let Some(shared) = self.shared.upgrade()
                && !shared.gpu_seen.load(Ordering::Acquire) {
                shared.error("CEF did not produce a GPU frame".into());
            }
        }
    }
}

fn start(
    id: u64,
    frame: File,
    root: PathBuf,
    url: &str,
    size: [u32; 2],
    accelerated: bool,
    shared: Arc<Shared>,
) -> bool {
    let [width, height] = size.map(|n| n as i32);
    // SAFETY: the renderer owns this private memfd and only grows it. Paint byte
    // access is protected by the same flock used by the renderer's read-only map.
    let mapping = match unsafe { MmapOptions::new().map_mut(&frame) } {
        Ok(mapping) => mapping,
        Err(error) => {
            eprintln!("we-web: mapping browser frame: {error}");
            return false;
        }
    };
    let audio = Arc::new(Audio::new());
    let state = StateRc::new(BrowserState {
        frame,
        mapping: RefCell::new(mapping),
        accelerated,
        direct: accelerated && std::env::var("WALLPAPERD_WEB_DIRECT").as_deref() == Ok("1"),
        shared: shared.clone(),
        painted_size: Cell::new((0, 0)),
        capture_counter: Cell::new(None),
        full_frame: Cell::new(true),
        width: Cell::new(width),
        height: Cell::new(height),
        frame_rate: Cell::new(30),
        browser: RefCell::new(None),
        registration: RefCell::new(None),
        audio: audio.clone(),
        value: RefCell::new(serde_json::json!({})),
        sequence: Cell::new(0),
        revision: Cell::new(0),
        sent_paused: Cell::new(false),
        loaded: Cell::new(false),
        ready: Cell::new(false),
        freeze_after_frame: Cell::new(false),
        frozen: Cell::new(false),
        closing: Cell::new(false),
        mouse_down: Cell::new(false),
        mouse_focused: Cell::new(false),
        mouse: RefCell::new(MouseEvent::default()),
    });
    STATES.with(|states| states.borrow_mut().insert(id, state));
    let mut client = WebClient::new(
        id,
        Arc::new(Resources {
            root,
            url: url.to_owned(),
        }),
        audio,
        shared,
    );
    let mut window = WindowInfo::default().set_as_windowless(0);
    window.shared_texture_enabled = accelerated.into();
    let settings = BrowserSettings {
        windowless_frame_rate: 30,
        background_color: 0xff000000,
        ..Default::default()
    };
    // Empty cache_path creates a distinct off-the-record context for this browser.
    let mut context =
        request_context_create_context(Some(&RequestContextSettings::default()), None);
    browser_host_create_browser(
        Some(&window),
        Some(&mut client),
        Some(&url.into()),
        Some(&settings),
        None,
        context.as_mut(),
    ) != 0
}

pub struct BrowserState {
    frame: File,
    mapping: RefCell<MmapMut>,
    accelerated: bool,
    direct: bool,
    shared: Arc<Shared>,
    painted_size: Cell<(i32, i32)>,
    capture_counter: Cell<Option<u64>>,
    full_frame: Cell<bool>,
    width: Cell<i32>,
    height: Cell<i32>,
    frame_rate: Cell<i32>,
    browser: RefCell<Option<Browser>>,
    registration: RefCell<Option<Registration>>,
    audio: Arc<Audio>,
    value: RefCell<Value>,
    sequence: Cell<u64>,
    revision: Cell<i32>,
    sent_paused: Cell<bool>,
    loaded: Cell<bool>,
    ready: Cell<bool>,
    freeze_after_frame: Cell<bool>,
    frozen: Cell<bool>,
    closing: Cell<bool>,
    mouse_down: Cell<bool>,
    mouse_focused: Cell<bool>,
    mouse: RefCell<MouseEvent>,
}

impl BrowserState {
    fn host(&self) -> Option<BrowserHost> {
        let browser = self.browser.borrow().clone();
        browser.and_then(|browser| browser.host())
    }
    fn rect(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: self.width.get(),
            height: self.height.get(),
        }
    }
    pub fn receive(&self, incoming: &Value) {
        let repaint = {
            let mut value = self.value.borrow_mut();
            let repaint = value.get("properties").is_none()
                || value.get("size").is_none()
                || value.get("properties") != incoming.get("properties")
                || value.get("size") != incoming.get("size");
            for (key, item) in incoming.as_object().unwrap() {
                value[key] = item.clone();
            }
            repaint
        };
        if repaint {
            self.full_frame.set(true);
        }
        self.apply(repaint);
    }
    pub fn close(&self) {
        self.closing.set(true);
        if let Some(host) = self.host() {
            host.close_browser(1);
        }
    }
    fn lifecycle(&self, frozen: bool) {
        let Some(host) = self.host() else {
            return;
        };
        if self.frozen.replace(frozen) == frozen {
            return;
        }
        host.was_hidden(frozen.into());
        if let Some(mut params) = dictionary_value_create() {
            params.set_string(
                Some(&"state".into()),
                Some(&if frozen { "frozen" } else { "active" }.into()),
            );
            host.execute_dev_tools_method(
                0,
                Some(&"Page.setWebLifecycleState".into()),
                Some(&mut params),
            );
        }
    }
    fn apply(&self, repaint: bool) {
        let Some(host) = self.host() else {
            return;
        };
        // CEF can call us again during these calls; release RefCell borrows first.
        let mut value = self.value.borrow().clone();
        let playback = &value["playback"];
        let paused = playback["paused"].as_bool().unwrap_or(false);
        if playback.is_object() {
            self.audio.playback(
                paused || playback["mute"].as_bool().unwrap_or(false),
                playback["volume"].as_i64().unwrap_or(0).clamp(0, 100) as f32 / 100.0,
            );
            let fps = playback["fps"].as_i64().unwrap_or(1).clamp(1, 240) as i32;
            // CEF resets its VSync phase even when the requested rate is unchanged.
            if self.frame_rate.replace(fps) != fps {
                host.set_windowless_frame_rate(fps);
            }
        }
        let size = &value["size"];
        if let (Some(w), Some(h)) = (
            size["width"].as_i64().and_then(|n| i32::try_from(n).ok()),
            size["height"].as_i64().and_then(|n| i32::try_from(n).ok()),
        ) && valid_size(w, h)
            && (w != self.width.get() || h != self.height.get())
        {
            self.width.set(w);
            self.height.set(h);
            host.was_resized();
        }
        // Thaw to apply properties; freeze again after the acknowledged frame.
        if self.loaded.get()
            && value.get("playback").is_some()
            && value.get("properties").is_some()
            && (!paused || !self.frozen.get() || repaint)
        {
            if repaint || self.revision.get() == 0 || paused != self.sent_paused.get() {
                self.ready.set(false);
                self.revision.set(self.revision.get().wrapping_add(1));
                value["frame_revision"] = self.revision.get().into();
                self.value.borrow_mut()["frame_revision"] = value["frame_revision"].clone();
            }
            self.sent_paused.set(paused);
            self.lifecycle(false);
            self.freeze_after_frame.set(paused);
            let browser = self.browser.borrow().clone();
            if let Some(frame) = browser.and_then(|browser| browser.main_frame())
                && let Some(mut message) = process_message_create(Some(&"wallpaperd-state".into()))
                && let Some(arguments) = message.argument_list()
            {
                arguments.set_string(0, Some(&value.to_string().as_str().into()));
                frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
            }
        }
        let pointer = &value["pointer"];
        let mut mouse = self.mouse.borrow().clone();
        if pointer.is_object() && !paused {
            let previous = mouse.clone();
            let focused = pointer["focused"].as_bool().unwrap_or(false);
            mouse.x = ((pointer["x"].as_f64().unwrap_or(0.0) * self.width.get() as f64) as i32)
                .clamp(0, self.width.get() - 1);
            mouse.y = ((pointer["y"].as_f64().unwrap_or(0.0) * self.height.get() as f64) as i32)
                .clamp(0, self.height.get() - 1);
            mouse.modifiers = if self.mouse_down.get() {
                cef::sys::cef_event_flags_t::EVENTFLAG_LEFT_MOUSE_BUTTON.0
            } else {
                0
            };
            let down = focused && pointer["down"].as_bool().unwrap_or(false);
            let focus_changed = self.mouse_focused.replace(focused) != focused;
            if repaint
                || focus_changed
                || mouse.x != previous.x
                || mouse.y != previous.y
                || mouse.modifiers != previous.modifiers
            {
                host.send_mouse_move_event(Some(&mouse), (!focused).into());
            }
            if down != self.mouse_down.replace(down) {
                host.send_mouse_click_event(Some(&mouse), MouseButtonType::LEFT, (!down).into(), 1);
            }
            *self.mouse.borrow_mut() = mouse;
        } else if self.mouse_down.replace(false) {
            host.send_mouse_click_event(Some(&mouse), MouseButtonType::LEFT, 1, 1);
        }
    }
    fn error(&self, message: String) {
        self.shared.error(message);
    }
    unsafe fn paint(
        &self,
        kind: PaintElementType,
        rects: Option<&[Rect]>,
        buffer: *const u8,
        width: i32,
        height: i32,
    ) {
        if kind != PaintElementType::VIEW
            || !self.ready.get()
            || self.shared.closing.load(Ordering::Acquire)
            || width != self.width.get()
            || height != self.height.get()
            || buffer.is_null()
        {
            return;
        }
        let bytes = width as usize * height as usize * 4;
        if bytes as u64 > MAX_PIXELS {
            return;
        }
        let fd = self.frame.as_raw_fd();
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            // CEF's next damage list does not include this dropped paint.
            self.full_frame.set(true);
            if let Some(host) = self.host() {
                host.invalidate(PaintElementType::VIEW);
            }
            return;
        }
        let result = (|| -> std::io::Result<[u32; 4]> {
            let mut mapping = self.mapping.borrow_mut();
            if mapping.len() < bytes + 16 {
                if self.frame.metadata()?.len() < (bytes + 16) as u64 {
                    return Err(std::io::Error::other("shared browser frame is too small"));
                }
                // SAFETY: the parent has grown the memfd; no slices from the old map survive.
                *mapping = unsafe { MmapOptions::new().map_mut(&self.frame) }?;
            }
            let damage = if self.full_frame.get() || self.painted_size.get() != (width, height) {
                [0, 0, width as u32, height as u32]
            } else {
                damage_bounds(rects, [0, 0], width, height)
            };
            // CEF supplies a complete BGRA image even when only a region changed.
            let pixels = unsafe { std::slice::from_raw_parts(buffer, bytes) };
            let [x, y, w, h] = damage.map(|value| value as usize);
            let stride = width as usize * 4;
            if w == width as usize {
                let start = y * stride;
                let end = start + h * stride;
                mapping[16 + start..16 + end].copy_from_slice(&pixels[start..end]);
            } else {
                for row in y..y + h {
                    let start = row * stride + x * 4;
                    let end = start + w * 4;
                    mapping[16 + start..16 + end].copy_from_slice(&pixels[start..end]);
                }
            }
            if self.painted_size.get() != (width, height) && mapping.len() > bytes + 16 {
                // Release old pixel pages after a shrink, while retaining the file
                // length so live mappings and queued paints cannot encounter SIGBUS.
                // SAFETY: this owned memfd range is within the mapping; flock
                // excludes concurrent pixel readers until the header is published.
                if unsafe {
                    libc::fallocate(
                        fd,
                        libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                        (bytes + 16) as libc::off_t,
                        (mapping.len() - bytes - 16) as libc::off_t,
                    )
                } != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            let sequence = self.sequence.get().wrapping_add(1);
            mapping[..8].copy_from_slice(&sequence.to_ne_bytes());
            mapping[8..12].copy_from_slice(&(width as u32).to_ne_bytes());
            mapping[12..16].copy_from_slice(&(height as u32).to_ne_bytes());
            self.sequence.set(sequence);
            self.painted_size.set((width, height));
            self.full_frame.set(false);
            Ok(damage)
        })();
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
        let damage = match result {
            Ok(damage) => damage,
            Err(error) => {
                self.error(format!("writing browser frame: {error}"));
                return;
            }
        };
        {
            let mut update = self.shared.update.lock().unwrap();
            update.frame = true;
            update.damage = Some((self.sequence.get(), damage));
        }
        self.shared.notify();
        if self.freeze_after_frame.replace(false) {
            self.lifecycle(true);
        }
    }

    fn paint_gpu(
        &self,
        kind: PaintElementType,
        rects: Option<&[Rect]>,
        info: &AcceleratedPaintInfo,
    ) {
        if kind != PaintElementType::VIEW
            || !self.ready.get()
            || self.shared.closing.load(Ordering::Acquire)
        {
            return;
        }
        if !self.accelerated {
            return;
        }
        let result = (|| -> std::io::Result<()> {
            let size = &info.extra.coded_size;
            let rect = &info.extra.visible_rect;
            if rect.width != self.width.get() || rect.height != self.height.get() {
                return Ok(());
            }
            if !valid_size(size.width, size.height)
                || !(1..=4).contains(&info.plane_count)
                || rect.x < 0
                || rect.y < 0
                || rect.width <= 0
                || rect.height <= 0
            {
                return Err(std::io::Error::other("invalid accelerated browser frame"));
            }
            let format = if info.format == ColorType::BGRA_8888 {
                frames::ARGB8888
            } else if info.format == ColorType::RGBA_8888 {
                frames::ABGR8888
            } else {
                return Err(std::io::Error::other(
                    "unsupported accelerated browser format",
                ));
            };
            let planes = &info.planes[..info.plane_count as usize];
            let sequence = self.sequence.get().wrapping_add(1);
            let frame = frames::Info {
                sequence,
                size: [size.width as u32, size.height as u32],
                visible: [
                    rect.x as u32,
                    rect.y as u32,
                    rect.width as u32,
                    rect.height as u32,
                ],
                format,
                modifier: info.modifier,
                planes: planes
                    .iter()
                    .map(|p| frames::Plane {
                        stride: p.stride,
                        offset: p.offset,
                    })
                    .collect(),
            };
            let fds = planes
                .iter()
                .map(|plane| {
                    if plane.fd < 0 {
                        return Err(std::io::Error::other("invalid DMA-BUF descriptor"));
                    }
                    // CEF owns the descriptors until this paint callback returns.
                    unsafe { BorrowedFd::borrow_raw(plane.fd) }.try_clone_to_owned()
                })
                .collect::<std::io::Result<Vec<_>>>()?;
            let (ack, receipt) = mpsc::sync_channel(1);
            let damage =
                if self.full_frame.get() || self.painted_size.get() != (rect.width, rect.height) {
                    [0, 0, rect.width as u32, rect.height as u32]
                } else {
                    accelerated_damage(self.capture_counter.get(), rects, info)
                };
            {
                let mut update = self.shared.update.lock().unwrap();
                if self.shared.closing.load(Ordering::Acquire) {
                    return Ok(());
                }
                update.gpu = Some(frames::Frame {
                    info: frame,
                    fds,
                    ack: (!self.direct).then_some(ack),
                });
                update.damage = Some((sequence, damage));
                self.shared.gpu_seen.store(true, Ordering::Release);
            }
            self.shared.notify();
            // Retain CEF's pool slot until the GL thread completes its copy.
            // Dropping a renderer or an unconsumed frame also releases this wait.
            // The direct experiment returns immediately, like we-layerd. Duplicated
            // descriptors retain the allocation, but do not reserve CEF's pool slot.
            if !self.direct && receipt.recv() != Ok(true) {
                if self.shared.closing.load(Ordering::Acquire) {
                    return Ok(());
                }
                return Err(std::io::Error::other(
                    "worker rejected accelerated browser frame",
                ));
            }
            self.sequence.set(sequence);
            self.painted_size.set((rect.width, rect.height));
            self.capture_counter
                .set((info.extra.has_capture_counter != 0).then_some(info.extra.capture_counter));
            self.full_frame.set(false);
            // Preserve the shared frame header for read-only frame-rate sampling.
            // No pixel bytes are copied or read by the worker in GPU mode.
            let fd = self.frame.as_raw_fd();
            if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                let mut mapping = self.mapping.borrow_mut();
                mapping[..8].copy_from_slice(&sequence.to_ne_bytes());
                mapping[8..12].copy_from_slice(&(rect.width as u32).to_ne_bytes());
                mapping[12..16].copy_from_slice(&(rect.height as u32).to_ne_bytes());
                unsafe {
                    libc::flock(fd, libc::LOCK_UN);
                }
            }
            if self.freeze_after_frame.replace(false) {
                self.lifecycle(true);
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.error(format!("accelerated browser frame: {error}"));
            self.close();
        }
    }
}

fn accelerated_damage(
    previous: Option<u64>,
    rects: Option<&[Rect]>,
    info: &AcceleratedPaintInfo,
) -> [u32; 4] {
    let visible = &info.extra.visible_rect;
    // CEF damage is relative to the preceding capture, including frames that
    // may never have reached our callback. Missing metadata requires a full copy.
    let consecutive = info.extra.has_capture_counter != 0
        && previous.and_then(|counter| counter.checked_add(1)) == Some(info.extra.capture_counter);
    damage_bounds(
        rects.filter(|_| consecutive),
        [visible.x, visible.y],
        visible.width,
        visible.height,
    )
}

fn damage_bounds(rects: Option<&[Rect]>, origin: [i32; 2], width: i32, height: i32) -> [u32; 4] {
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
    for rect in rects.unwrap_or_default() {
        let x = rect.x.saturating_sub(origin[0]).clamp(0, width);
        let y = rect.y.saturating_sub(origin[1]).clamp(0, height);
        let end_x = rect
            .x
            .saturating_add(rect.width)
            .saturating_sub(origin[0])
            .clamp(0, width);
        let end_y = rect
            .y
            .saturating_add(rect.height)
            .saturating_sub(origin[1])
            .clamp(0, height);
        if x < end_x && y < end_y {
            left = left.min(x);
            top = top.min(y);
            right = right.max(end_x);
            bottom = bottom.max(end_y);
        }
    }
    if left < right && top < bottom {
        [
            left as u32,
            top as u32,
            (right - left) as u32,
            (bottom - top) as u32,
        ]
    } else {
        [0, 0, width as u32, height as u32]
    }
}

#[test]
fn accelerated_damage_keeps_pixels_from_missing_captures() {
    let mut info = AcceleratedPaintInfo::default();
    info.extra.visible_rect = Rect {
        x: 5,
        y: 7,
        width: 64,
        height: 64,
    };
    info.extra.has_capture_counter = 1;
    info.extra.capture_counter = 9;
    let rects = [Rect {
        x: 12,
        y: 18,
        width: 9,
        height: 13,
    }];
    assert_eq!(
        accelerated_damage(Some(8), Some(&rects), &info),
        [7, 11, 9, 13]
    );
    // The published callback can be consecutive even when CEF skipped a capture.
    assert_eq!(
        accelerated_damage(Some(7), Some(&rects), &info),
        [0, 0, 64, 64]
    );
    info.extra.has_capture_counter = 0;
    assert_eq!(
        accelerated_damage(Some(8), Some(&rects), &info),
        [0, 0, 64, 64]
    );
}

wrap_client! {
    struct WebClient {
        id: u64,
        resources: Arc<Resources>,
        audio: Arc<Audio>,
        shared: Arc<Shared>,
    }
    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(Paint::new(self.id))
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(Lifetime::new(self.id))
        }
        fn load_handler(&self) -> Option<LoadHandler> {
            Some(Loading::new(self.id))
        }
        fn request_handler(&self) -> Option<RequestHandler> {
            Some(Requests::new(self.resources.clone(), self.shared.clone()))
        }
        fn audio_handler(&self) -> Option<AudioHandler> {
            Some(Sound::new(self.audio.clone()))
        }
        fn dialog_handler(&self) -> Option<DialogHandler> {
            Some(Dialogs::new())
        }
        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(JsDialogs::new())
        }
        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            let Some(message) = message else {
                return 0;
            };
            if source_process != ProcessId::RENDERER
                || CefString::from(&message.name()).to_string() != "wallpaperd-event"
            {
                return 0;
            }
            if let (Some(state), Some(arguments)) = (current(self.id), message.argument_list()) {
                match CefString::from(&arguments.string(0)).to_string().as_str() {
                    "ready" if state.loaded.get() && arguments.int(1) == state.revision.get() => {
                        state.ready.set(true);
                        if let Some(host) = state.host() {
                            host.invalidate(PaintElementType::VIEW);
                        }
                    }
                    "audio" => {
                        state.shared.update.lock().unwrap().audio = true;
                        state.shared.notify();
                    },
                    _ => (),
                }
            }
            1
        }
    }
}

wrap_render_handler! {
    struct Paint { id: u64 }
    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let (Some(state), Some(rect)) = (current(self.id), rect) {
                *rect = state.rect();
            }
        }
        fn screen_info(&self, _browser: Option<&mut Browser>, info: Option<&mut ScreenInfo>) -> i32 {
            if let (Some(state), Some(info)) = (current(self.id), info) {
                info.device_scale_factor = 1.0;
                info.rect = state.rect();
                info.available_rect = state.rect();
                return 1;
            }
            0
        }
        fn on_accelerated_paint(&self, _browser: Option<&mut Browser>, kind: PaintElementType,
            dirty_rects: Option<&[Rect]>, info: Option<&AcceleratedPaintInfo>) {
            if let (Some(state),Some(info)) = (current(self.id),info) { state.paint_gpu(kind,dirty_rects,info); }
        }
        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            kind: PaintElementType,
            dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: i32,
            height: i32,
        ) {
            if let Some(state) = current(self.id) {
                unsafe {
                    state.paint(kind, dirty_rects, buffer, width, height);
                }
            }
        }
    }
}

wrap_life_span_handler! {
    struct Lifetime { id: u64 }
    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let (Some(state), Some(browser)) = (current(self.id), browser) {
                *state.browser.borrow_mut() = Some(browser.clone());
                if let Some(host) = state.host() {
                    let mut observer = Devtools::new(self.id);
                    *state.registration.borrow_mut() =
                        host.add_dev_tools_message_observer(Some(&mut observer));
                }
                if state.closing.get() {
                    state.close();
                } else {
                    state.apply(false);
                }
            }
        }
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let state = STATES.with(|states| states.borrow_mut().remove(&self.id));
            if let Some(state) = state {
                state.registration.borrow_mut().take();
                state.browser.borrow_mut().take();
                state.shared.closed();
            }
        }
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: i32,
            _target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: i32,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut i32>,
        ) -> i32 {
            1
        }
    }
}

wrap_load_handler! {
    struct Loading { id: u64 }
    impl LoadHandler {
        fn on_load_start(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _transition_type: TransitionType,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0)
                && let Some(state) = current(self.id)
            {
                state.loaded.set(false);
                state.ready.set(false);
            }
        }
        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _http_status_code: i32,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0)
                && let Some(state) = current(self.id)
            {
                state.loaded.set(true);
                state.apply(true);
            }
        }
        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            _failed_url: Option<&CefString>,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0) && error_code != Errorcode::ABORTED
                && let Some(state) = current(self.id) {
                state.error(error_text.map(ToString::to_string).unwrap_or_default());
            }
        }
    }
}

struct Resources {
    root: PathBuf,
    url: String,
}
impl Resources {
    fn allows(&self, url: &str) -> bool {
        let Ok(url) = url::Url::parse(url) else {
            return false;
        };
        match url.scheme() {
            "file" => url
                .to_file_path()
                .ok()
                .and_then(|path| path.canonicalize().ok())
                .is_some_and(|path| path.starts_with(&self.root)),
            "http" | "https" | "data" | "blob" | "about" => true,
            _ => false,
        }
    }
}

wrap_request_handler! {
    struct Requests {
        resources: Arc<Resources>,
        shared: Arc<Shared>,
    }
    impl RequestHandler {
        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: i32,
            _is_redirect: i32,
        ) -> i32 {
            (frame.is_some_and(|frame| frame.is_main() != 0)
                && request.is_some_and(|request| {
                    CefString::from(&request.url()).to_string() != self.resources.url
                }))
            .into()
        }
        fn resource_request_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            _is_navigation: i32,
            _is_download: i32,
            _request_initiator: Option<&CefString>,
            _disable_default_handling: Option<&mut i32>,
        ) -> Option<ResourceRequestHandler> {
            Some(ResourceRequests::new(self.resources.clone()))
        }
        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut Browser>,
            _status: TerminationStatus,
            _error_code: i32,
            error_string: Option<&CefString>,
        ) {
            self.shared.error(format!("Chromium renderer exited: {}",
                error_string.map(ToString::to_string).unwrap_or_default()));
        }
    }
}

wrap_resource_request_handler! {
    struct ResourceRequests {
        resources: Arc<Resources>,
    }
    impl ResourceRequestHandler {
        fn on_before_resource_load(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _callback: Option<&mut Callback>,
        ) -> ReturnValue {
            if request.is_some_and(|request| {
                self.resources
                    .allows(&CefString::from(&request.url()).to_string())
            }) {
                ReturnValue::CONTINUE
            } else {
                ReturnValue::CANCEL
            }
        }
    }
}

wrap_audio_handler! {
    struct Sound {
        audio: Arc<Audio>,
    }
    impl AudioHandler {
        fn audio_parameters(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&mut AudioParameters>,
        ) -> i32 {
            if let Some(params) = params {
                params.channel_layout = ChannelLayout::LAYOUT_STEREO;
                params.sample_rate = 48000;
                params.frames_per_buffer = 512;
                return 1;
            }
            0
        }
        fn on_audio_stream_started(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&AudioParameters>,
            channels: i32,
        ) {
            if let Some(params) = params {
                self.audio.start(params.sample_rate, channels);
            }
        }
        fn on_audio_stream_packet(
            &self,
            _browser: Option<&mut Browser>,
            data: *mut *const f32,
            frames: i32,
            _pts: i64,
        ) {
            unsafe {
                self.audio.packet(data, frames);
            }
        }
        fn on_audio_stream_error(&self, _browser: Option<&mut Browser>, message: Option<&CefString>) {
            eprintln!(
                "we-web audio: {}",
                message.map(ToString::to_string).unwrap_or_default()
            );
        }
    }
}

wrap_dev_tools_message_observer! {
    struct Devtools { id: u64 }
    impl DevToolsMessageObserver {
        fn on_dev_tools_method_result(
            &self,
            _browser: Option<&mut Browser>,
            _message_id: i32,
            success: i32,
            result: Option<&[u8]>,
        ) {
            if success == 0 && let Some(state) = current(self.id) {
                state.error(format!("CEF lifecycle: {}", String::from_utf8_lossy(result.unwrap_or_default())));
            }
        }
    }
}

wrap_dialog_handler! {
    struct Dialogs;
    impl DialogHandler {
        fn on_file_dialog(
            &self,
            _browser: Option<&mut Browser>,
            _mode: FileDialogMode,
            _title: Option<&CefString>,
            _default_file_path: Option<&CefString>,
            _accept_filters: Option<&mut CefStringList>,
            _accept_extensions: Option<&mut CefStringList>,
            _accept_descriptions: Option<&mut CefStringList>,
            callback: Option<&mut FileDialogCallback>,
        ) -> i32 {
            if let Some(callback) = callback {
                callback.cancel();
            }
            1
        }
    }
}

wrap_jsdialog_handler! {
    struct JsDialogs;
    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            _browser: Option<&mut Browser>,
            _origin_url: Option<&CefString>,
            _dialog_type: JsdialogType,
            _message_text: Option<&CefString>,
            _default_prompt_text: Option<&CefString>,
            _callback: Option<&mut JsdialogCallback>,
            suppress_message: Option<&mut i32>,
        ) -> i32 {
            if let Some(suppress) = suppress_message {
                *suppress = 1;
            }
            0
        }
    }
}
