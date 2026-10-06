//! Shared libmpv ownership for standalone video and embedded scene media.
mod api;
mod capture;
pub mod clock;
use anyhow::{Context, Result, bail};
pub use api::capability;
use api::*;
pub use capture::{Frame, extract_frame};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    os::{fd::AsRawFd, unix::net::UnixStream},
    ptr,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
pub mod presentation;
pub use presentation::Fit;
#[derive(Clone, Copy)]
pub struct Playback {
    pub paused: bool,
    pub mute: bool,
    pub volume: f32,
}
#[derive(Clone)]
pub struct RenderContext {
    pub resolver: Rc<khronos_egl::DynamicInstance<khronos_egl::EGL1_5>>,
    pub native_display: *mut c_void,
    pub software: bool,
    pub fit: Fit,
    pub flip_y: bool,
    pub pulse_server: Option<String>,
}
struct Wake {
    socket: UnixStream,
    render: AtomicBool,
}
extern "C" fn notify(context: *mut c_void) {
    // SAFETY: the boxed Wake remains live until both callbacks have been detached and mpv freed.
    let wake = unsafe { &*(context as *const Wake) };
    let byte = 1u8;
    // SAFETY: nonblocking write of a stack byte; EAGAIN means a wake is already pending.
    unsafe {
        libc::send(
            wake.socket.as_raw_fd(),
            ptr::from_ref(&byte).cast(),
            1,
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
        );
    }
}
extern "C" fn render_notify(context: *mut c_void) {
    // SAFETY: same callback lifetime as notify; only an atomic flag and fd are accessed.
    unsafe { &*(context as *const Wake) }
        .render
        .store(true, Ordering::Release);
    notify(context);
}
extern "C" fn gl_address(context: *mut c_void, name: *const c_char) -> *mut c_void {
    // SAFETY: mpv supplies a NUL-terminated symbol; libEGL is held by the current Gpu.
    let api = unsafe { &*(context as *const khronos_egl::DynamicInstance<khronos_egl::EGL1_5>) };
    let Ok(name) = (unsafe { CStr::from_ptr(name) }).to_str() else {
        return ptr::null_mut();
    };
    api.get_proc_address(name)
        .map_or(ptr::null_mut(), |function| function as *mut c_void)
}

pub struct Player {
    api: Arc<Api>,
    handle: *mut c_void,
    render: *mut c_void,
    wake: Box<Wake>,
    pub loaded: bool,
    dirty: bool,
    failed: Option<String>,
    pub first_frame: bool,
    playback: Playback,
    gl_api: Option<Rc<khronos_egl::DynamicInstance<khronos_egl::EGL1_5>>>,
    pub ended: bool,
    eof: bool,
    eof_reported: bool,
    flip_y: bool,
    clock: Option<crate::clock::Timeline>,
    position: Option<(f64, u64)>,
    duration: Option<f64>,
    frame_duration: f64,
    next_sync: u64,
    seeking: bool,
    seek_goal: Option<f64>,
    seek_serial: u64,
    seek_reply: Option<u64>,
    seek_started: bool,
    sync_speed: f64,
    base_rate: f64,
    align_clock: bool,
    clock_seek: bool,
    seek_decoded_ns: Option<u64>,
    seek_latency_ns: u64,
    looping: bool,
    null_audio: bool,
    next_audio_retry: u64,
}

impl Player {
    pub fn video(
        context: RenderContext,
        path: &std::path::Path,
        playback: Playback,
        wake: &UnixStream,
    ) -> Result<Self> {
        let server = context.pulse_server.clone();
        Self::new(Some(context), path, playback, wake, true, server.as_deref())
    }
    pub fn audio(
        path: &std::path::Path,
        playback: Playback,
        wake: &UnixStream,
        looping: bool,
    ) -> Result<Self> {
        Self::audio_on_server(path, playback, wake, looping, None)
    }
    pub fn audio_on_server(
        path: &std::path::Path,
        playback: Playback,
        wake: &UnixStream,
        looping: bool,
        server: Option<&str>,
    ) -> Result<Self> {
        Self::new(None, path, playback, wake, looping, server)
    }
    fn new(
        context: Option<RenderContext>,
        path: &std::path::Path,
        playback: Playback,
        wake: &UnixStream,
        looping: bool,
        pulse_server: Option<&str>,
    ) -> Result<Self> {
        let api = Api::load()?;
        // SAFETY: creates an independent core; no render context exists during synchronous setup.
        let handle = unsafe { (api.create)() };
        anyhow::ensure!(!handle.is_null(), "mpv_create failed");
        let socket = match wake.try_clone() {
            Ok(socket) => socket,
            Err(error) => {
                unsafe {
                    (api.destroy)(handle);
                }
                return Err(error.into());
            }
        };
        let mut video = Self {
            api,
            handle,
            render: ptr::null_mut(),
            wake: Box::new(Wake {
                socket,
                render: AtomicBool::new(false),
            }),
            loaded: false,
            dirty: false,
            failed: None,
            first_frame: false,
            playback,
            gl_api: context.as_ref().map(|c| c.resolver.clone()),
            ended: false,
            eof: false,
            eof_reported: false,
            flip_y: context.as_ref().is_some_and(|c| c.flip_y),
            clock: None,
            position: None,
            duration: None,
            frame_duration: 1. / 30.,
            next_sync: 0,
            seeking: false,
            seek_goal: None,
            seek_serial: 1,
            seek_reply: None,
            seek_started: false,
            sync_speed: 1.0,
            base_rate: 1.0,
            align_clock: false,
            clock_seek: false,
            seek_decoded_ns: None,
            seek_latency_ns: 0,
            looping,
            null_audio: false,
            next_audio_retry: 0,
        };
        video.option("volume", &playback.volume.to_string())?;
        if let Some(server) = pulse_server {
            video.option("pulse-host", server)?;
        }
        for (name, value) in [
            ("config", "no"),
            ("terminal", "no"),
            ("msg-level", "all=no"),
            ("vo", if context.is_some() { "libmpv" } else { "null" }),
            ("vid", if context.is_some() { "auto" } else { "no" }),
            ("audio-display", "no"),
            ("access-references", "no"),
            ("autoload-files", "no"),
            ("load-scripts", "no"),
            ("ytdl", "no"),
            ("hwdec", "auto"),
            // Wallpaper buffers are opaque, including transparent video/GIF regions.
            ("background", "color"),
            ("loop-file", if looping { "inf" } else { "no" }),
            ("keep-open", "yes"),
            ("idle", "yes"),
            ("osc", "no"),
            ("osd-level", "0"),
            ("input-default-bindings", "no"),
            ("sub", "no"),
            ("audio", "auto"),
            ("ao", "pulse"),
            // Keep decoding when the audio server is temporarily unavailable;
            // a reconnect can re-open Pulse without replacing scene resources.
            ("audio-fallback-to-null", "yes"),
            ("audio-client-name", "wallpaperd"),
            ("mute", if playback.mute { "yes" } else { "no" }),
            ("pause", if playback.paused { "yes" } else { "no" }),
            (
                "keepaspect",
                if context.as_ref().is_some_and(|c| c.fit == Fit::Stretch) {
                    "no"
                } else {
                    "yes"
                },
            ),
            (
                "panscan",
                if context.as_ref().is_some_and(|c| c.fit == Fit::Cover) {
                    "1"
                } else {
                    "0"
                },
            ),
            ("video-timing-offset", "0"),
            ("vd-lavc-threads", "2"),
        ] {
            video.option(name, value)?;
        }
        // load-scripts only disables user scripts. Embedded players also do
        // not need mpv's built-in UI clients, each of which owns a Lua thread.
        // Newer optional UI switches must not reject older libmpv versions.
        for name in [
            c"load-auto-profiles",
            c"load-console",
            c"load-osd-console",
            c"load-stats-overlay",
            c"load-select",
            c"load-positioning",
            c"load-commands",
            c"load-context-menu",
        ] {
            // SAFETY: the core is uninitialized and copies these static strings.
            let code = unsafe { (video.api.option)(handle, name.as_ptr(), c"no".as_ptr()) };
            if code != -5 {
                // MPV_ERROR_OPTION_NOT_FOUND
                video.api.check(code)?;
            }
        }
        if context.as_ref().is_some_and(|c| c.software) {
            // Mesa software GL renders mpv's Lanczos chain black in the real
            // image/video switch regression. Keep hardware defaults, and use
            // the verified bilinear path for software rendering.
            video.option("scale", "bilinear")?;
        }
        // SAFETY: initialization precedes creation of any rendering dependency.
        video.api.check(unsafe { (video.api.initialize)(handle) })?;
        let callback = ptr::from_mut(&mut *video.wake).cast();
        // SAFETY: callback state is boxed and stable for the complete core lifetime.
        unsafe {
            (video.api.set_wakeup)(handle, Some(notify), callback);
        }
        if let Some(context) = &context {
            let mut init = GlInit {
                get_proc_address: gl_address,
                context: Rc::as_ptr(video.gl_api.as_ref().unwrap()).cast_mut().cast(),
            };
            let mut params = [
                Param {
                    kind: 1,
                    data: c"opengl".as_ptr().cast_mut().cast(),
                },
                Param::new(2, &mut init),
                Param {
                    kind: 9,
                    data: context.native_display,
                },
                Param::end(),
            ];
            // SAFETY: the worker's EGL context is current; parameters remain live throughout creation.
            video.api.check(unsafe {
                (video.api.render_create)(&mut video.render, handle, params.as_mut_ptr())
            })?;
            unsafe {
                (video.api.render_callback)(video.render, Some(render_notify), callback);
            }
        }
        let path = CString::new(path.as_os_str().as_encoded_bytes())?;
        let args = [
            c"loadfile".as_ptr(),
            path.as_ptr(),
            c"replace".as_ptr(),
            ptr::null(),
        ];
        // SAFETY: the async command copies all arguments before returning.
        video
            .api
            .check(unsafe { (video.api.command)(handle, 0, args.as_ptr()) })?;
        for (id, name) in [(1, c"time-pos"), (2, c"duration")] {
            // SAFETY: observation copies property values into nonblocking events.
            video
                .api
                .check(unsafe { (video.api.observe)(handle, id, name.as_ptr(), 5) })?;
        }
        if context.is_some() {
            // SAFETY: observation copies FPS into nonblocking property events.
            video
                .api
                .check(unsafe { (video.api.observe)(handle, 4, c"container-fps".as_ptr(), 5) })?;
        }
        // SAFETY: string property events own their char pointer until wait_event().
        video
            .api
            .check(unsafe { (video.api.observe)(handle, 3, c"current-ao".as_ptr(), 1) })?;
        // keep-open retains the file at EOF, so END_FILE alone does not
        // report a naturally completed, replayable short sound.
        video
            .api
            .check(unsafe { (video.api.observe)(handle, 5, c"eof-reached".as_ptr(), 3) })?;
        Ok(video)
    }

    pub fn load_file(&mut self, path: &std::path::Path) -> Result<()> {
        self.loaded = false;
        self.first_frame = self.render.is_null();
        self.ended = false;
        self.eof = false;
        self.eof_reported = false;
        self.position = None;
        self.duration = None;
        self.next_sync = 0;
        self.seeking = false;
        self.seek_goal = None;
        self.seek_reply = None;
        self.seek_started = false;
        self.clock_seek = false;
        self.seek_decoded_ns = None;
        self.seek_latency_ns = 0;
        self.command(&[
            "loadfile",
            path.to_str().context("non-UTF8 media asset path")?,
            "replace",
        ])
    }
    pub fn seek(&mut self, seconds: f64) -> Result<()> {
        anyhow::ensure!(seconds.is_finite() && seconds >= 0., "invalid media seek");
        let position = CString::new(format!("{seconds:.6}"))?;
        let args = [
            c"seek".as_ptr(),
            position.as_ptr(),
            c"absolute+exact".as_ptr(),
            ptr::null(),
        ];
        self.seek_serial = self
            .seek_serial
            .checked_add(1)
            .context("seek command counter overflow")?;
        // SAFETY: mpv copies the command arguments. Its unique reply forms a
        // barrier against restart/position events belonging to an earlier seek.
        self.api
            .check(unsafe { (self.api.command)(self.handle, self.seek_serial, args.as_ptr()) })?;
        self.seeking = true;
        self.eof = false;
        self.eof_reported = false;
        self.seek_goal = Some(seconds);
        self.seek_decoded_ns = None;
        self.seek_reply = Some(self.seek_serial);
        self.seek_started = false;
        self.clock_seek = false;
        self.align_clock = false;
        self.next_sync = crate::clock::now().saturating_add(500_000_000);
        Ok(())
    }
    pub fn command(&self, args: &[&str]) -> Result<()> {
        let strings = args
            .iter()
            .map(|s| CString::new(*s))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut args = strings.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
        args.push(ptr::null());
        // SAFETY: the asynchronous command copies these strings before returning.
        self.api
            .check(unsafe { (self.api.command)(self.handle, 0, args.as_ptr()) })
    }
    pub fn duration(&self) -> Option<f64> {
        self.duration
    }
    pub fn frame_duration(&self) -> f64 {
        self.frame_duration
    }
    pub fn position(&self) -> Option<f64> {
        self.position.map(|(p, _)| p)
    }
    /// A decoded frame is publishable after the shared clock has been checked.
    pub fn ready(&self) -> bool {
        self.first_frame
            && !self.pending_seek()
            && (!self.align_clock || self.playback.paused)
            && !self.clock_seek
    }
    pub fn pending_seek(&self) -> bool {
        self.seeking || self.seek_goal.is_some()
    }
    pub fn seek_target(&self) -> Option<f64> {
        self.seek_goal
    }
    fn complete_seek(&mut self) {
        if self.seeking {
            return;
        }
        let tolerance = if self.render.is_null() {
            0.1
        } else {
            self.frame_duration.max(0.1) + 0.002
        };
        if self
            .seek_goal
            .zip(self.position)
            .is_some_and(|(goal, (position, _))| (position - goal).abs() < tolerance)
        {
            if let Some(decoded) = self.seek_decoded_ns.take() {
                self.seek_latency_ns = crate::clock::now()
                    .saturating_sub(decoded)
                    .min(2_000_000_000);
            }
            self.seek_goal = None;
            if self.render.is_null() && std::mem::take(&mut self.clock_seek) {
                self.align_clock = true;
                self.next_sync = 0;
            }
        }
    }
    fn option(&self, name: &str, value: &str) -> Result<()> {
        let name = CString::new(name)?;
        let value = CString::new(value)?;
        // SAFETY: called only before mpv initialization/render context creation.
        self.api
            .check(unsafe { (self.api.option)(self.handle, name.as_ptr(), value.as_ptr()) })
    }

    pub fn playback(&mut self, playback: Playback) -> Result<()> {
        if self.playback.paused != playback.paused {
            let now = crate::clock::now();
            if let Some((position, sampled)) = &mut self.position {
                if !self.playback.paused {
                    *position += now.saturating_sub(*sampled) as f64 / 1e9 * self.sync_speed;
                }
                *sampled = now;
            }
        }
        if self.playback.paused && !playback.paused {
            self.next_sync = 0;
            self.align_clock = self.clock.is_some();
        }
        for (name, value, old) in [
            (c"pause", playback.paused, self.playback.paused),
            (c"mute", playback.mute, self.playback.mute),
        ] {
            if value == old {
                continue;
            }
            let mut flag = i32::from(value);
            // SAFETY: the async API copies the typed flag and cannot wait on the renderer.
            self.api.check(unsafe {
                (self.api.property)(
                    self.handle,
                    0,
                    name.as_ptr(),
                    3,
                    ptr::from_mut(&mut flag).cast(),
                )
            })?;
        }
        if playback.volume != self.playback.volume {
            let mut volume = playback.volume as f64;
            self.api.check(unsafe {
                (self.api.property)(
                    self.handle,
                    0,
                    c"volume".as_ptr(),
                    5,
                    ptr::from_mut(&mut volume).cast(),
                )
            })?;
        }
        // FPS is a presentation ceiling, not a CPU video filter. Preserve native
        // timestamps and hardware frames; the presenter controls submission rate.
        self.playback = playback;
        Ok(())
    }

    pub fn synchronize(&mut self, clock: crate::clock::Timeline) -> Result<()> {
        self.align_clock |= self.clock != Some(clock);
        self.clock = Some(clock);
        self.next_sync = 0;
        Ok(())
    }
    pub fn synchronize_rate(&mut self, clock: crate::clock::Timeline, rate: f64) -> Result<()> {
        anyhow::ensure!(
            rate.is_finite() && (0.01..=100.).contains(&rate),
            "invalid media rate"
        );
        if self.base_rate != rate {
            self.base_rate = rate;
            self.align_clock = true;
            self.speed(rate)?;
        }
        self.synchronize(clock)
    }
    pub fn set_loop(&mut self, looping: bool) -> Result<()> {
        if looping != self.looping {
            self.command(&["set", "loop-file", if looping { "inf" } else { "no" }])?;
            self.looping = looping;
            self.next_sync = 0;
        }
        Ok(())
    }
    fn speed(&mut self, mut speed: f64) -> Result<()> {
        if (speed - self.sync_speed).abs() > 0.005 {
            // SAFETY: mpv copies the typed speed before this async call returns.
            self.api.check(unsafe {
                (self.api.property)(
                    self.handle,
                    0,
                    c"speed".as_ptr(),
                    5,
                    ptr::from_mut(&mut speed).cast(),
                )
            })?;
            self.sync_speed = speed;
        }
        Ok(())
    }

    fn sync_position(&mut self) -> Result<()> {
        let now = crate::clock::now();
        if now < self.next_sync
            || self.seeking
            || self.seek_goal.is_some()
            || !self.loaded
            || (self.playback.paused && self.first_frame)
        {
            return Ok(());
        }
        let (Some(clock), Some(duration), Some((position, sampled))) =
            (self.clock, self.duration, self.position)
        else {
            return Ok(());
        };
        self.next_sync = now.saturating_add(500_000_000);
        let normalize = |position: f64| {
            if self.looping {
                position.rem_euclid(duration)
            } else {
                position.clamp(0., duration)
            }
        };
        let expected = normalize(
            clock.position_ns as f64 / 1e9
                + if clock.running {
                    now.saturating_sub(clock.sampled_ns) as f64 / 1e9 * self.base_rate
                } else {
                    0.
                },
        );
        let actual = normalize(
            position
                + if self.playback.paused {
                    0.0
                } else {
                    now.saturating_sub(sampled) as f64 / 1e9 * self.sync_speed
                },
        );
        let drift = if self.looping {
            (expected - actual + duration / 2.).rem_euclid(duration) - duration / 2.
        } else {
            expected - actual
        };
        // A late join needs one seek. Correct small drift with speed, avoiding repeated seek stalls.
        if (self.align_clock && drift.abs() > self.frame_duration.max(0.025) + 0.01)
            || drift.abs() > 0.5
        {
            let lead = if clock.running && !self.playback.paused {
                self.seek_latency_ns as f64 / 1e9 * self.base_rate
            } else {
                0.
            };
            let goal = normalize(expected + lead);
            let goal = if !self.looping && !self.render.is_null() {
                goal.min((duration - self.frame_duration * 1.001).max(0.))
            } else {
                goal
            };
            self.seek(goal)?;
            self.clock_seek = true;
            // The old drift no longer applies after an exact seek.
            self.speed(self.base_rate)?;
            return Ok(());
        }
        self.align_clock = false;
        let speed = if drift.abs() < 0.025 {
            self.base_rate
        } else {
            (self.base_rate * (1.0 + drift.clamp(-0.1, 0.1))).clamp(0.01, 100.)
        };
        self.speed(speed)?;
        Ok(())
    }

    pub fn poll(&mut self) -> Result<()> {
        for _ in 0..256 {
            // SAFETY: timeout zero is nonblocking and explicitly render-thread safe.
            let event = unsafe { &*(self.api.wait_event)(self.handle, 0.0) };
            match event.kind {
                0 => break,
                5 if self.seek_reply == Some(event.userdata) => {
                    self.seek_reply = None;
                } // MPV_EVENT_COMMAND_REPLY
                20 if self.seek_reply.is_none() && self.seek_goal.is_some() => {
                    self.seek_started = true;
                } // MPV_EVENT_SEEK
                8 => {
                    self.loaded = true;
                    self.ended = false;
                    self.eof = false;
                    self.eof_reported = false;
                    // keep-open / AO reinitialization can pause mpv internally.
                    // Reassert the owner's state after a replacement has loaded,
                    // even if the cached requested Playback did not change.
                    self.command(&[
                        "set",
                        "pause",
                        if self.playback.paused { "yes" } else { "no" },
                    ])?;
                    if self.render.is_null() {
                        self.first_frame = true;
                    }
                } // MPV_EVENT_FILE_LOADED
                21 if self.seek_started => {
                    self.seeking = false;
                    self.seek_started = false;
                    self.ended = false;
                    // A seek to the current paused position may emit no
                    // time-pos change. Playback restart still completes it.
                    self.complete_seek();
                } // MPV_EVENT_PLAYBACK_RESTART
                22 if !event.data.is_null() => {
                    // SAFETY: mpv_event_property and its typed data remain live until wait_event().
                    let property = unsafe { &*(event.data as *const Property) };
                    if event.userdata == 5 && property.format == 3 && !property.data.is_null() {
                        self.eof = unsafe { *(property.data as *const i32) } != 0;
                        if !self.eof {
                            self.eof_reported = false;
                        }
                    }
                    if event.userdata == 3 && property.format == 1 && !property.data.is_null() {
                        let value = unsafe { *(property.data as *const *const c_char) };
                        if !value.is_null() {
                            self.null_audio =
                                unsafe { CStr::from_ptr(value) }.to_bytes() == b"null";
                        }
                    }
                    if property.format == 5 && !property.data.is_null() {
                        let value = unsafe { *(property.data as *const f64) };
                        if value.is_finite() {
                            match event.userdata {
                                1 => {
                                    let sampled = crate::clock::now();
                                    if self.seek_decoded_ns.is_none()
                                        && self
                                            .seek_goal
                                            .is_some_and(|goal| (goal - value).abs() < 0.01)
                                    {
                                        self.seek_decoded_ns = Some(sampled);
                                    }
                                    self.position = Some((value, sampled));
                                    self.complete_seek();
                                }
                                2 if value > 0.0 => self.duration = Some(value),
                                4 if (0.001..=1000.).contains(&value) => {
                                    self.frame_duration = 1. / value
                                }
                                _ => {}
                            }
                        }
                    }
                }
                7 if !event.data.is_null() => {
                    // SAFETY: MPV_EVENT_END_FILE owns this documented struct until the next wait_event.
                    let end = unsafe { &*(event.data as *const EndFile) };
                    // loadfile replace ends the previous entry with STOP (2).
                    // Playlist redirection (5) also continues playback. Neither
                    // completes the new sound/video or cancels its pending seek.
                    if matches!(end.reason, 2 | 5) {
                        continue;
                    }
                    self.ended = true;
                    self.eof_reported = true;
                    self.seek_goal = None;
                    self.seeking = false;
                    if end.reason == 4 || !self.first_frame {
                        self.failed = Some(format!(
                            "media ended before playback: {}",
                            self.api.check(end.error).err().map_or_else(
                                || format!("reason {}", end.reason),
                                |e| e.to_string()
                            )
                        ));
                    }
                }
                1 => self.failed = Some("libmpv shut down".into()),
                _ if event.error < 0 => {
                    self.failed = Some(self.api.check(event.error).unwrap_err().to_string())
                }
                _ => {}
            }
        }
        // Property notifications may deliver EOF before the final time-pos.
        // Latch it until the matching endpoint arrives, and publish only once.
        if self.eof
            && !self.eof_reported
            && self.loaded
            && !self.looping
            && !self.pending_seek()
            && self
                .position
                .zip(self.duration)
                .is_some_and(|((position, _), duration)| {
                    position >= duration - self.frame_duration.max(0.1)
                })
        {
            self.ended = true;
            self.eof_reported = true;
        }
        if !self.render.is_null() && self.wake.render.swap(false, Ordering::AcqRel) {
            // SAFETY: same current GL context as creation, never called from the callback.
            self.dirty |= unsafe { (self.api.render_update)(self.render) } & 1 != 0;
        }
        if let Some(error) = self.failed.take() {
            bail!(error);
        }
        self.sync_position()?;
        let now = crate::clock::now();
        if self.null_audio
            && self.loaded
            && !self.ended
            && !self.playback.paused
            && !self.playback.mute
            && now >= self.next_audio_retry
        {
            self.next_audio_retry = now.saturating_add(1_000_000_000);
            self.command(&["ao-reload"])?;
            // A short file may already be fully buffered when AO reload resets
            // the filter chain. Refill it through the existing seek barrier;
            // otherwise no further packet arrives to initialize Pulse again.
            if self.render.is_null()
                && !self.pending_seek()
                && let Some(position) = self.position()
            {
                self.seek(position)?;
            }
            self.command(&["set", "pause", "no"])?;
        }
        Ok(())
    }

    pub fn needs_render(&self, force: bool) -> bool {
        !self.render.is_null()
            && self.loaded
            && (!self.align_clock || force)
            && !self.seeking
            && self.seek_goal.is_none()
            && (self.dirty || (force && self.first_frame))
            && (!self.playback.paused || !self.first_frame || force)
    }

    pub fn render(&mut self, fbo: u32, size: [u32; 2], force: bool) -> Result<bool> {
        self.render_frame(fbo, size, force, false)
    }
    /// A seek is complete only when a newly queued video frame reaches the FBO.
    pub fn render_seek(&mut self, fbo: u32, size: [u32; 2]) -> Result<bool> {
        self.render_frame(fbo, size, true, true)
    }
    fn render_frame(&mut self, fbo: u32, size: [u32; 2], force: bool, seek: bool) -> Result<bool> {
        // Queued pre-pause updates must not replace the displayed frame. A
        // forced redraw is still permitted for snapshots and geometry changes.
        if !self.needs_render(force) {
            return Ok(false);
        }
        let mut info = FrameInfo {
            flags: 0,
            target_time: 0,
        };
        // SAFETY: query is made on the rendering thread and fills the public FrameInfo ABI.
        self.api
            .check(unsafe { (self.api.render_info)(self.render, Param::new(11, &mut info)) })?;
        if seek && info.flags & 1 == 0 {
            self.dirty = false;
            return Ok(false);
        }
        let new_frame = info.flags & 1 != 0 && info.flags & 6 == 0;
        if !self.first_frame && (info.flags & 1 == 0 || info.flags & 2 != 0) {
            self.dirty = false;
            return Ok(false);
        }
        let mut fbo = Fbo {
            fbo: fbo as i32,
            width: size[0] as i32,
            height: size[1] as i32,
            format: glow::RGBA8 as i32,
        };
        let mut flip = i32::from(self.flip_y);
        let mut block = 0i32;
        let mut params = [
            Param::new(3, &mut fbo),
            Param::new(4, &mut flip),
            Param::new(12, &mut block),
            Param::end(),
        ];
        // SAFETY: target is complete in the current context; rendering does not wait for frame timing.
        self.api
            .check(unsafe { (self.api.render)(self.render, params.as_mut_ptr()) })?;
        self.dirty = false;
        self.first_frame = true;
        // Decode and initial GL setup can delay the sampled seek position.
        // Consume that frame before rechecking: mpv may need this handoff to
        // finish playback restart, and repeated seeks must not starve the VO.
        if std::mem::take(&mut self.clock_seek) {
            self.align_clock = true;
            self.next_sync = 0;
        }
        // Redraw requests can precede the decoded seek frame. Drain them, but
        // keep the caller waiting for the actual frame instead of committing
        // a redraw of the previous position.
        Ok(!seek || new_frame)
    }

    pub fn report_swap(&self) {
        // SAFETY: notification for a frame just committed using this render context.
        if !self.render.is_null() {
            unsafe {
                (self.api.report_swap)(self.render);
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: detach callbacks before freeing their state, and free render context with
        // the same GL context current, before terminating the owning core.
        unsafe {
            (self.api.set_wakeup)(self.handle, None, ptr::null_mut());
            if !self.render.is_null() {
                (self.api.render_callback)(self.render, None, ptr::null_mut());
                (self.api.render_free)(self.render);
            }
            (self.api.destroy)(self.handle);
        }
    }
}
