#![allow(clippy::too_many_arguments)]

use crate::{MAX_PIXELS, Shared, audio::Audio, frames};
use cef::*;
use memmap2::{MmapMut, MmapOptions};
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fs::File,
    io::Read,
    os::fd::{AsRawFd, BorrowedFd},
    path::PathBuf,
    rc::Rc as StateRc,
    sync::{Arc, Weak, atomic::Ordering, mpsc},
    time::Duration,
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
        frame_rate: u16,
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
                frame, self.root.clone(), &self.url, self.size, self.frame_rate, self.accelerated, self.shared.clone()));
            if !started {
                STATES.with(|states| states.borrow_mut().remove(&self.id));
                self.shared.error("creating offscreen browser failed".into());
                self.shared.closed();
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VisibilityBarrier {
    Idle,
    Hiding,
    Showing,
    Visible,
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
    struct PublishPaint { id: u64, revision: i32 }
    impl Task {
        fn execute(&self) {
            if let Some(state) = current(self.id)
                && state.revision.get() == self.revision {
                state.paint_retry.set(false);
                state.publish_pending(self.id);
            }
        }
    }
}
wrap_task! {
    struct PublishReady { id: u64, revision: i32 }
    impl Task {
        fn execute(&self) {
            if let Some(state) = current(self.id)
                && state.loaded.get()
                && state.revision.get() == self.revision
            {
                match state.visibility_barrier.get() {
                    VisibilityBarrier::Idle if state.freeze_after_frame.get() => {
                        state.visibility_barrier.set(VisibilityBarrier::Hiding);
                        if let Some(host) = state.host() {
                            host.was_hidden(1);
                        }
                        return;
                    }
                    VisibilityBarrier::Visible => {
                        state.visibility_barrier.set(VisibilityBarrier::Idle)
                    }
                    VisibilityBarrier::Idle => {}
                    _ => return,
                }
                state.ready.set(true);
                if state.pending_paint.borrow().is_some() {
                    state.publish_pending(self.id);
                } else if let Some(host) = state.host() {
                    host.invalidate(PaintElementType::VIEW);
                }
            }
        }
    }
}

wrap_task! {
    struct RevisionTimeout { id: u64, revision: i32 }
    impl Task {
        fn execute(&self) {
            if let Some(state) = current(self.id)
                && state.loaded.get()
                && state.revision.get() == self.revision
                && (!state.ready.get() || state.freeze_after_frame.get())
                && !state.closing.get()
            {
                state.error("browser revision did not become ready".into());
            }
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
    frame_rate: u16,
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
    let resources = Arc::new(Resources::new(root, url.to_owned()));
    let state = StateRc::new(BrowserState {
        id,
        frame,
        mapping: RefCell::new(mapping),
        pending_paint: RefCell::new(None),
        paint_retry: Cell::new(false),
        accelerated,
        shared: shared.clone(),
        resources: resources.clone(),
        painted_size: Cell::new((0, 0)),
        capture_counter: Cell::new(None),
        full_frame: Cell::new(true),
        width: Cell::new(width),
        height: Cell::new(height),
        frame_rate: Cell::new(frame_rate.into()),
        rate_boosted: Cell::new(false),
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
        visibility_barrier: Cell::new(VisibilityBarrier::Idle),
        frozen: Cell::new(false),
        lifecycle_message: Cell::new(0),
        thaw_request: Cell::new(None),
        closing: Cell::new(false),
        mouse_down: Cell::new(false),
        mouse_focused: Cell::new(false),
        mouse: RefCell::new(MouseEvent::default()),
    });
    STATES.with(|states| states.borrow_mut().insert(id, state));
    let mut client = WebClient::new(id, resources, audio, shared);
    let mut window = WindowInfo::default().set_as_windowless(0);
    window.shared_texture_enabled = accelerated.into();
    let settings = BrowserSettings {
        windowless_frame_rate: frame_rate.into(),
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
    id: u64,
    frame: File,
    mapping: RefCell<MmapMut>,
    pending_paint: RefCell<Option<(MmapMut, [i32; 2])>>,
    paint_retry: Cell<bool>,
    accelerated: bool,
    shared: Arc<Shared>,
    resources: Arc<Resources>,
    painted_size: Cell<(i32, i32)>,
    capture_counter: Cell<Option<u64>>,
    full_frame: Cell<bool>,
    width: Cell<i32>,
    height: Cell<i32>,
    frame_rate: Cell<i32>,
    rate_boosted: Cell<bool>,
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
    visibility_barrier: Cell<VisibilityBarrier>,
    frozen: Cell<bool>,
    lifecycle_message: Cell<i32>,
    thaw_request: Cell<Option<i32>>,
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
        let mut prepared = incoming.clone();
        if let Some(properties) = incoming.get("properties") {
            self.resources.select(properties);
            prepared["directories"] = self.resources.directories(properties);
        }
        let incoming = &prepared;
        let repaint = {
            let mut value = self.value.borrow_mut();
            let repaint = ["properties", "size"].into_iter().any(|key| {
                incoming
                    .get(key)
                    .is_some_and(|item| value.get(key) != Some(item))
            });
            for (key, item) in incoming.as_object().unwrap() {
                value[key] = item.clone();
            }
            repaint
        };
        if repaint {
            self.full_frame.set(true);
        }
        self.apply(repaint, Some(incoming));
    }
    pub fn close(&self) {
        self.closing.set(true);
        self.restore_frame_rate();
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
        let message_id = if self.lifecycle_message.get() == i32::MAX {
            1
        } else {
            self.lifecycle_message.get() + 1
        };
        self.lifecycle_message.set(message_id);
        self.thaw_request.set((!frozen).then_some(message_id));
        if frozen {
            host.was_hidden(1);
        }
        let Some(mut params) = dictionary_value_create() else {
            self.error("creating CEF lifecycle request failed".into());
            return;
        };
        params.set_string(
            Some(&"state".into()),
            Some(&if frozen { "frozen" } else { "active" }.into()),
        );
        if host.execute_dev_tools_method(
            message_id,
            Some(&"Page.setWebLifecycleState".into()),
            Some(&mut params),
        ) == 0
        {
            self.thaw_request.set(None);
            self.error("submitting CEF lifecycle request failed".into());
        }
    }
    fn restore_frame_rate(&self) {
        if self.rate_boosted.replace(false)
            && let Some(host) = self.host()
        {
            host.set_windowless_frame_rate(self.frame_rate.get());
        }
    }
    fn apply(&self, repaint: bool, incoming: Option<&Value>) {
        let Some(host) = self.host() else {
            return;
        };
        // CEF can call us again during these calls; release RefCell borrows first.
        let (playback, size, pointer, initialized) = {
            let value = self.value.borrow();
            (
                value["playback"].clone(),
                value["size"].clone(),
                value["pointer"].clone(),
                value.get("playback").is_some() && value.get("properties").is_some(),
            )
        };
        let paused = playback["paused"].as_bool().unwrap_or(false);
        let fps = playback["fps"].as_i64().unwrap_or(1).clamp(1, 240) as i32;
        if playback.is_object() {
            self.audio.playback(
                paused || playback["mute"].as_bool().unwrap_or(false),
                playback["volume"].as_i64().unwrap_or(0).clamp(0, 100) as f32 / 100.0,
            );
            // CEF resets its VSync phase even when the requested rate is unchanged.
            if self.frame_rate.replace(fps) != fps {
                host.set_windowless_frame_rate(fps);
            }
        }
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
        if self.loaded.get() && initialized && (!paused || !self.frozen.get() || repaint) {
            let revision_changed =
                repaint || self.revision.get() == 0 || paused != self.sent_paused.get();
            let mut outgoing = match incoming {
                Some(incoming) if !revision_changed => incoming.clone(),
                _ => self.value.borrow().clone(),
            };
            if revision_changed {
                self.freeze_after_frame.set(false);
                self.ready.set(false);
                self.pending_paint.borrow_mut().take();
                self.paint_retry.set(false);
                if self.visibility_barrier.replace(VisibilityBarrier::Idle)
                    != VisibilityBarrier::Idle
                {
                    // A superseded hide/show handshake must not strand the next
                    // revision in a hidden renderer where its rAF cannot run.
                    host.was_hidden(0);
                }
                self.revision.set(self.revision.get().wrapping_add(1));
                outgoing["frame_revision"] = self.revision.get().into();
                self.value.borrow_mut()["frame_revision"] = outgoing["frame_revision"].clone();
                let mut timeout = RevisionTimeout::new(self.id, self.revision.get());
                if post_delayed_task(ThreadId::UI, Some(&mut timeout), 5_000) == 0 {
                    self.error("scheduling browser revision deadline failed".into());
                }
                if paused && fps < 60 {
                    host.set_windowless_frame_rate(60);
                    self.rate_boosted.set(true);
                } else {
                    self.restore_frame_rate();
                }
            }
            self.sent_paused.set(paused);
            self.lifecycle(false);
            self.freeze_after_frame.set(paused);
            let renderer_update = outgoing.as_object().is_some_and(|value| {
                ["playback", "properties", "audio", "media", "frame_revision"]
                    .into_iter()
                    .any(|key| value.contains_key(key))
            });
            let browser = renderer_update
                .then(|| self.browser.borrow().clone())
                .flatten();
            if let Some(frame) = browser.and_then(|browser| browser.main_frame())
                && let Some(mut message) = process_message_create(Some(&"wallpaperd-state".into()))
                && let Some(arguments) = message.argument_list()
            {
                arguments.set_string(0, Some(&outgoing.to_string().as_str().into()));
                frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
            }
        }
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
        self.restore_frame_rate();
        self.shared.error(message);
    }
    unsafe fn stage_paint(&self, buffer: *const u8, width: i32, height: i32) {
        let bytes = width as usize * height as usize * 4;
        let mut pending = self.pending_paint.borrow_mut();
        if pending
            .as_ref()
            .is_none_or(|(pixels, _)| pixels.len() != bytes)
        {
            match MmapMut::map_anon(bytes) {
                Ok(pixels) => *pending = Some((pixels, [width, height])),
                Err(error) => {
                    self.error(format!("staging browser frame: {error}"));
                    return;
                }
            }
        }
        let (pixels, size) = pending.as_mut().unwrap();
        pixels.copy_from_slice(unsafe { std::slice::from_raw_parts(buffer, bytes) });
        *size = [width, height];
    }
    fn publish_pending(&self, id: u64) {
        if !self.ready.get() || self.shared.closing.load(Ordering::Acquire) {
            return;
        }
        let pending = self.pending_paint.borrow_mut().take();
        if let Some((pixels, [width, height])) = pending {
            self.full_frame.set(true);
            // The mapping owns a complete frame, including all skipped damage.
            if !unsafe { self.paint(PaintElementType::VIEW, None, pixels.as_ptr(), width, height) }
            {
                *self.pending_paint.borrow_mut() = Some((pixels, [width, height]));
                self.retry_paint(id);
            }
        }
    }
    fn retry_paint(&self, id: u64) {
        if !self.paint_retry.replace(true) {
            let mut task = PublishPaint::new(id, self.revision.get());
            // Only a busy reader needs a retry; a published or paused frame
            // leaves no timer behind. Retain the mapping without recopying it.
            if post_delayed_task(ThreadId::UI, Some(&mut task), 1) == 0 {
                self.paint_retry.set(false);
                self.error("scheduling browser frame publication failed".into());
            }
        }
    }
    // False means the reader owns the shared mapping and this frame must be retried.
    unsafe fn paint(
        &self,
        kind: PaintElementType,
        rects: Option<&[Rect]>,
        buffer: *const u8,
        width: i32,
        height: i32,
    ) -> bool {
        if kind != PaintElementType::VIEW
            || self.shared.closing.load(Ordering::Acquire)
            || width != self.width.get()
            || height != self.height.get()
            || buffer.is_null()
        {
            return true;
        }
        let bytes = width as usize * height as usize * 4;
        if bytes as u64 > MAX_PIXELS {
            return true;
        }
        if !self.ready.get() {
            // The final paint can precede the renderer's ready message. Static
            // pages need not paint again after Invalidate, so retain owned pixels
            // until that message instead of discarding the only updated frame.
            unsafe { self.stage_paint(buffer, width, height) };
            return true;
        }
        let fd = self.frame.as_raw_fd();
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) {
                self.error(format!("locking browser frame: {error}"));
                return true;
            }
            return false;
        }
        let result = (|| -> std::io::Result<Option<[u32; 4]>> {
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
            let unchanged = !self.freeze_after_frame.get()
                && self.painted_size.get() == (width, height)
                && (y..y + h).all(|row| {
                    let start = row * stride + x * 4;
                    let end = start + w * 4;
                    mapping[16 + start..16 + end] == pixels[start..end]
                });
            if unchanged {
                self.full_frame.set(false);
                return Ok(None);
            }
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
            Ok(Some(damage))
        })();
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
        // A newer successful CEF paint supersedes a queued retry of older pixels.
        self.pending_paint.borrow_mut().take();
        let damage = match result {
            Ok(Some(damage)) => damage,
            Ok(None) => return true,
            Err(error) => {
                self.error(format!("writing browser frame: {error}"));
                return true;
            }
        };
        {
            let mut update = self.shared.update.lock().unwrap();
            update.frame = true;
            update.damage = Some((self.sequence.get(), damage));
        }
        self.shared.notify();
        if self.freeze_after_frame.replace(false) {
            self.restore_frame_rate();
            self.lifecycle(true);
        }
        true
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
                    ack: Some(frames::Ack::Local(ack)),
                });
                update.damage = Some((sequence, damage));
                self.shared.gpu_seen.store(true, Ordering::Release);
            }
            self.shared.notify();
            // Retain CEF's pool slot until the GL thread completes its copy.
            match receipt.recv_timeout(Duration::from_secs(5)) {
                Ok(true) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // The consumer may still have a GPU read in flight. Terminate
                    // this disposable host so CEF cannot recycle the DMA-BUF.
                    eprintln!("we-web: accelerated frame lease timed out");
                    // This code only runs in the disposable web host, which is
                    // its own process-group leader. Stop CEF's whole process
                    // tree before its buffer pool can reuse this timed-out slot.
                    unsafe {
                        libc::kill(0, libc::SIGKILL);
                        libc::_exit(1);
                    }
                }
                _ if self.shared.closing.load(Ordering::Acquire) => return Ok(()),
                _ => {
                    return Err(std::io::Error::other(
                        "worker rejected accelerated browser frame",
                    ));
                }
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
                self.restore_frame_rate();
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
                        let mut task = PublishReady::new(self.id, state.revision.get());
                        if post_task(ThreadId::UI, Some(&mut task)) == 0 {
                            state.error("scheduling browser frame publication failed".into());
                        }
                    }
                    "hidden"
                        if state.visibility_barrier.get() == VisibilityBarrier::Hiding
                            && arguments.int(1) == state.revision.get() =>
                    {
                        state.visibility_barrier.set(VisibilityBarrier::Showing);
                        if let Some(host) = state.host() {
                            host.was_hidden(0);
                        }
                    }
                    "visible"
                        if state.visibility_barrier.get() == VisibilityBarrier::Showing
                            && arguments.int(1) == state.revision.get() =>
                    {
                        state.visibility_barrier.set(VisibilityBarrier::Visible);
                        let mut task = PublishReady::new(self.id, state.revision.get());
                        if post_task(ThreadId::UI, Some(&mut task)) == 0 {
                            state.error("scheduling browser frame publication failed".into());
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
                    if !state.paint(kind, dirty_rects, buffer, width, height) {
                        state.stage_paint(buffer, width, height);
                        state.retry_paint(self.id);
                    }
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
                    state.apply(false, None);
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
                state.apply(true, None);
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
    selectors: HashMap<String, bool>,
    directory_definitions: HashMap<String, Value>,
    selected: std::sync::Mutex<Vec<(PathBuf, bool)>>,
}
impl Resources {
    fn new(root: PathBuf, url: String) -> Self {
        let project: Value = File::open(root.join("project.json"))
            .ok()
            .and_then(|file| serde_json::from_reader(file.take(1024 * 1024 + 1)).ok())
            .unwrap_or_default();
        let selectors = project["general"]["properties"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| match value["type"].as_str() {
                Some("file") => Some((key.clone(), false)),
                Some("directory") => Some((key.clone(), true)),
                _ => None,
            })
            .collect();
        let directory_definitions = project["general"]["properties"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, definition)| definition["type"] == "directory")
            .map(|(key, definition)| (key.clone(), definition.clone()))
            .collect();
        Self {
            root,
            url,
            selectors,
            directory_definitions,
            selected: Default::default(),
        }
    }
    fn directories(&self, properties: &Value) -> Value {
        self.directory_definitions.iter().map(|(key, definition)| {
            let mut files = Vec::new();
            let mut bytes = 0;
            if let Some(directory) = properties[key].as_str().and_then(|path| std::path::Path::new(path).canonicalize().ok())
                && self.selected.lock().unwrap().iter().any(|(selected, is_dir)| *is_dir && *selected == directory)
                && let Ok(entries) = std::fs::read_dir(&directory) {
                let video = definition.get("fileType").or_else(|| definition.get("filetype")).and_then(Value::as_str) == Some("video");
                for entry in entries.take(4096).flatten() {
                    let Ok(path) = entry.path().canonicalize() else { continue; };
                    if !path.is_file() || !path.starts_with(&directory) { continue; }
                    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("").to_ascii_lowercase();
                    let accepted = if video { ["webm", "ogg", "ogv", "mp4"].contains(&extension.as_str()) }
                        else { ["jpeg", "jpg", "png", "pnga", "bmp", "gif", "svg", "webp"].contains(&extension.as_str()) };
                    if !accepted { continue; }
                    let Some(path) = path.to_str() else { continue; };
                    if files.len() >= 512 || bytes + path.len() > 32 * 1024 { break; }
                    bytes += path.len(); files.push(path.to_owned());
                }
            }
            files.sort(); files.dedup();
            (key.clone(), serde_json::json!({"mode":definition["mode"].as_str().unwrap_or("ondemand"),"files":files}))
        }).collect::<serde_json::Map<_, _>>().into()
    }
    fn select(&self, properties: &Value) {
        // Only the host's typed file selectors grant access. A wallpaper cannot
        // broaden this list through URLs, text fields or renderer JavaScript.
        *self.selected.lock().unwrap() = self
            .selectors
            .iter()
            .filter_map(|(key, directory)| {
                let path = std::path::Path::new(properties[key].as_str()?);
                if !path.is_absolute() {
                    return None;
                }
                let path = path.canonicalize().ok()?;
                ((*directory && path.is_dir()) || (!directory && path.is_file()))
                    .then_some((path, *directory))
            })
            .collect();
    }
    fn allows(&self, url: &str) -> bool {
        let Ok(url) = url::Url::parse(url) else {
            return false;
        };
        match url.scheme() {
            "file" => url
                .to_file_path()
                .ok()
                .and_then(|path| path.canonicalize().ok())
                .is_some_and(|path| {
                    path.starts_with(&self.root)
                        || self
                            .selected
                            .lock()
                            .unwrap()
                            .iter()
                            .any(|(selected, directory)| {
                                path == *selected || *directory && path.starts_with(selected)
                            })
                }),
            "http" | "https" | "data" | "blob" | "about" => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod resource_tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[ignore = "requires Node.js; exercises bridge callbacks against a mocked browser"]
    fn directory_bridge_delivers_fetchall_random_and_reset_events() {
        let script = r#"
const assert = require('assert');
const vm = require('vm');
const changes = [], removals = [], random = [];
const window = {__wallpaperdHost() {}, wallpaperPropertyListener:{
    applyUserProperties() {},
    userDirectoryFilesAddedOrChanged(name, files) { changes.push([name, ...files]); },
    userDirectoryFilesRemoved(name, files) { removals.push([name, ...files]); }
}};
vm.runInNewContext(process.argv[1], {window, document:{addEventListener() {}}, console});
window.__wallpaperdReceive({properties:{folder:'/selected'}, directories:{folder:{mode:'fetchall',files:['/selected/a.png']}}});
assert.deepStrictEqual(changes, [['folder','/selected/a.png']]);
window.wallpaperRequestRandomFileForProperty('folder', (name, file) => random.push([name,file]));
assert.deepStrictEqual(random, [['folder','/selected/a.png']]);
window.__wallpaperdReceive({directories:{folder:{mode:'fetchall',files:[]}}});
assert.deepStrictEqual(removals, [['folder','/selected/a.png']]);
window.wallpaperRequestRandomFileForProperty('folder', (name, file) => assert.strictEqual(file,''));
"#;
        assert!(
            std::process::Command::new("node")
                .args(["-e", script, include_str!("bridge.js")])
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn selected_files_and_directories_are_scoped_revocable_and_symlink_safe() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("selected image.png");
        let other = outside.path().join("other.txt");
        let folder = outside.path().join("photos");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(&file, []).unwrap();
        std::fs::write(&other, []).unwrap();
        std::fs::write(folder.join("image.png"), []).unwrap();
        std::fs::write(project.path().join("project.json"), json!({"general":{"properties":{
            "file":{"type":"file","value":file},"folder":{"type":"directory","value":folder},"text":{"type":"textinput"}
        }}}).to_string()).unwrap();
        let resources = Resources::new(project.path().to_owned(), String::new());
        let url = |path: &std::path::Path| url::Url::from_file_path(path).unwrap().to_string();
        assert!(
            !resources.allows(&url(&file)),
            "authored defaults grant nothing"
        );
        resources.select(&json!({"file":file,"folder":folder,"text":other}));
        assert!(resources.allows(&url(&file)));
        assert!(resources.allows(&url(&folder.join("image.png"))));
        assert_eq!(
            resources.directories(&json!({"folder":folder}))["folder"]["files"],
            json!([folder.join("image.png")])
        );
        assert!(!resources.allows(&url(&other)));
        let link = folder.join("escape.txt");
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert!(!resources.allows(&url(&link)));
        resources.select(&json!({"file":"","folder":""}));
        assert!(!resources.allows(&url(&file)));
        assert!(!resources.allows(&url(&folder.join("image.png"))));
        assert!(resources.allows(&url(&project.path().join("project.json"))));
        assert!(!resources.allows("file://remote.example/etc/passwd"));
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
        fn on_render_process_unresponsive(
            &self,
            _browser: Option<&mut Browser>,
            callback: Option<&mut UnresponsiveProcessCallback>,
        ) -> i32 {
            self.shared.error("Chromium renderer became unresponsive".into());
            if let Some(callback) = callback {
                callback.terminate();
            }
            1
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
            message_id: i32,
            success: i32,
            result: Option<&[u8]>,
        ) {
            if let Some(state) = current(self.id) {
                if success == 0 {
                    state.error(format!(
                        "CEF lifecycle: {}",
                        String::from_utf8_lossy(result.unwrap_or_default())
                    ));
                } else if state.thaw_request.get() == Some(message_id) && !state.frozen.get() {
                    state.thaw_request.set(None);
                    if let Some(host) = state.host() {
                        // Page.setWebLifecycleState(active) must complete before WasShown.
                        // Showing a still-frozen WebContents can lose the only OSR capture for
                        // a static page, leaving the pre-property frame visible indefinitely.
                        host.was_hidden(0);
                        host.invalidate(PaintElementType::VIEW);
                    }
                }
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
