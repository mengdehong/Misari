//! CEF browser integration with an optional backend.

#[cfg(not(feature = "web"))]
pub(crate) use disabled::*;
#[cfg(feature = "web")]
pub(crate) use enabled::*;

#[cfg(feature = "web")]
mod enabled {
    //! CEF browser integration; browser execution is provided by the we-web library.
    use crate::{
        domain::Playback,
        graphics::{Gpu, Target},
        pixels::Size,
        properties::Values,
    };
    use anyhow::{Context, Result};
    use serde_json::{Value, json};
    use std::{
        os::unix::net::UnixStream,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };
    pub(crate) use we_web::frames;

    pub fn capability() -> Value {
        let runtime = we_web::runtime_directory();
        let error = runtime
            .as_deref()
            .map_or_else(|| Some("CEF runtime is unavailable".into()), bundle_error);
        json!({"available":error.is_none(),"content":["we_web"],"properties":true,
        "input":true,"audio":true,"media":true,"pause":true,"fps_limit":true,
        "media_thumbnails":false,"synchronized_playback":false,"frame_output":["texture"],
        "requires":"matching CEF runtime, EGL/GLES 3","helper":null,"runtime":runtime,"error":error})
    }
    fn bundle_error(directory: &Path) -> Option<String> {
        for name in [
            "libcef.so",
            "icudtl.dat",
            "resources.pak",
            "v8_context_snapshot.bin",
        ] {
            if !directory.join(name).is_file() {
                return Some(format!(
                    "CEF runtime is incomplete: {}",
                    directory.join(name).display()
                ));
            }
        }
        None
    }

    pub struct Web {
        browser: we_web::Renderer,
        pending_gpu: Option<frames::Frame>,
        direct_frame: Option<frames::Frame>,
        direct: bool,
        source_sequence: u64,
        accelerated: bool,
        gpu_seen: bool,
        started: Instant,
        fallback: Option<String>,
        root: PathBuf,
        entry: PathBuf,
        wake: UnixStream,
        size: Size,
        sequence: u64,
        source: Option<Target>,
        source_bgra: bool,
        damage: Option<(u64, [u32; 4])>,
        state: Value,
        dirty: bool,
        audio: bool,
        error: Option<String>,
    }
    impl Web {
        pub fn load(
            root: &Path,
            entry: &Path,
            gpu: &Gpu,
            playback: Playback,
            properties: &Values,
            wake: &UnixStream,
        ) -> Result<Self> {
            let mode = std::env::var("WALLPAPERD_WEB_GPU").unwrap_or_default();
            let platform = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                Some("wayland")
            } else if std::env::var_os("DISPLAY").is_some() {
                Some("x11")
            } else {
                None
            };
            let platform = platform.filter(|_| {
                mode != "0"
                    && gpu.dma_buf_supported()
                    && (!gpu.native_display.is_null() || mode == "1")
            });
            Self::start(
                root,
                entry,
                gpu.size,
                json!({"size":{"width":gpu.size.width,"height":gpu.size.height},
                "playback":playback,"properties":web_properties(properties)}),
                wake,
                platform,
            )
        }
        fn start(
            root: &Path,
            entry: &Path,
            size: Size,
            state: Value,
            wake: &UnixStream,
            platform: Option<&str>,
        ) -> Result<Self> {
            let runtime = we_web::runtime_directory().context("locating CEF runtime")?;
            if let Some(error) = bundle_error(&runtime) {
                anyhow::bail!("{error}");
            }
            let browser = we_web::Renderer::load(
                root,
                entry,
                [size.width, size.height],
                platform.is_some(),
                platform.unwrap_or("headless"),
                wake,
            )?;
            let web = Self {
                browser,
                pending_gpu: None,
                direct_frame: None,
                direct: platform.is_some()
                    && std::env::var("WALLPAPERD_WEB_DIRECT").as_deref() == Ok("1"),
                source_sequence: 0,
                accelerated: platform.is_some(),
                gpu_seen: false,
                started: Instant::now(),
                fallback: None,
                root: root.to_owned(),
                entry: entry.to_owned(),
                wake: wake.try_clone()?,
                size,
                sequence: 0,
                source: None,
                source_bgra: true,
                damage: None,
                state,
                dirty: false,
                audio: false,
                error: None,
            };
            web.send()?;
            Ok(web)
        }
        fn send(&self) -> Result<()> {
            self.browser.send(self.state.clone(), false)
        }
        pub fn playback(&mut self, playback: Playback) -> Result<()> {
            self.state["playback"] = serde_json::to_value(playback)?;
            self.send()
        }
        pub fn properties(&mut self, values: &Values) -> Result<()> {
            self.state["properties"] = web_properties(values);
            self.send()
        }
        pub fn pointer(&mut self, position: Option<[f32; 2]>, button: Option<bool>) -> Result<()> {
            let old = self.state["pointer"]["down"].as_bool().unwrap_or(false);
            let focused = self.state["pointer"]["focused"].as_bool().unwrap_or(false);
            let [x, y] = position.unwrap_or([0.0, 0.0]);
            let down = position.is_some() && button.unwrap_or(old);
            self.state["pointer"] = json!({"focused":position.is_some(),"x":x,"y":y,"down":down});
            self.browser.send(
                self.state.clone(),
                down != old || focused != position.is_some(),
            )
        }
        pub fn audio(&mut self, audio: &we_scene::audio::AudioSnapshot) {
            if self.audio {
                let band = &audio.bands[2];
                self.state["audio"] = json!(
                    band.left
                        .iter()
                        .chain(&band.right)
                        .copied()
                        .collect::<Vec<_>>()
                );
                if let Err(error) = self.send() {
                    self.error = Some(error.to_string());
                }
            }
        }
        pub fn media(&mut self, media: &crate::media::Snapshot) {
            self.state["media"] = json!({"enabled":media.enabled,"title":media.title,"artist":media.artist,
            "album_title":media.album_title,"album_artist":media.album_artist,"genres":media.genres,
            "content_type":media.content_type,"playback":media.playback,"position":media.position,"duration":media.duration});
            if let Err(error) = self.send() {
                self.error = Some(error.to_string());
            }
        }
        pub fn requires_audio(&self) -> bool {
            self.audio
        }
        pub fn needs_render(&self) -> bool {
            self.dirty
        }
        pub fn transport(&self) -> &'static str {
            if self.accelerated { "dmabuf" } else { "mmap" }
        }
        pub fn diagnostics(&self) -> Value {
            json!({"transport":self.transport(),"fallback":self.fallback,"direct":self.direct})
        }
        fn fallback(&mut self, reason: String) -> Result<()> {
            eprintln!("wallpaperd web: DMA-BUF unavailable ({reason}); using mmap");
            self.pending_gpu.take();
            let mut replacement = Self::start(
                &self.root,
                &self.entry,
                self.size,
                self.state.clone(),
                &self.wake,
                None,
            )?;
            replacement.fallback = Some(reason);
            replacement.source = self.source.take();
            replacement.source_bgra = self.source_bgra;
            *self = replacement;
            Ok(())
        }
        pub fn ready(&self) -> bool {
            self.sequence > 0
                && (self.direct_frame().is_some()
                    || self
                        .source
                        .as_ref()
                        .is_some_and(|source| source.size == self.size))
        }
        pub fn direct_frame(&self) -> Option<&frames::Frame> {
            self.direct.then_some(())?;
            self.direct_frame
                .as_ref()
                .filter(|frame| frame.info.visible[2..] == [self.size.width, self.size.height])
        }
        pub fn direct_presented(&mut self) {
            self.dirty = false;
            self.source = None;
            self.source_sequence = 0;
        }
        pub fn poll(&mut self) -> Result<()> {
            let latest = self.browser.poll();
            if let Some(frame) = latest.gpu {
                self.gpu_seen = true;
                self.pending_gpu = Some(frame);
                self.dirty = true;
                self.damage = latest.damage;
            }
            if latest.frame {
                self.dirty = true;
                self.damage = latest.damage;
            }
            self.audio |= latest.audio;
            let error = self.error.take().or(latest.error).or_else(|| {
                (self.accelerated
                    && !self.gpu_seen
                    && self.started.elapsed() >= Duration::from_secs(8))
                .then(|| "CEF did not produce a GPU frame".into())
            });
            if let Some(error) = error {
                if self.accelerated {
                    return self.fallback(error);
                }
                anyhow::bail!("{error}");
            }
            Ok(())
        }
        fn resize(&mut self, gpu: &Gpu) -> Result<()> {
            if gpu.size != self.size {
                self.browser.resize([gpu.size.width, gpu.size.height])?;
                self.size = gpu.size;
                self.state["size"] = json!({"width":self.size.width,"height":self.size.height});
                self.dirty = true;
                self.direct_frame = None;
                self.source = None;
                self.sequence = 0;
                self.source_sequence = 0;
                self.send()?;
            }
            Ok(())
        }
        fn prepare_size(&mut self, gpu: &Gpu) -> Result<()> {
            self.resize(gpu)?;
            if self
                .source
                .as_ref()
                .is_none_or(|source| source.size != self.size)
            {
                self.source = Some(Target::new(gpu.gl.clone(), self.size)?);
                if !self.direct {
                    self.sequence = 0;
                }
                self.source_sequence = 0;
            }
            Ok(())
        }
        pub fn prepare_gpu(&mut self, gpu: &Gpu) -> Result<()> {
            if !self.accelerated {
                return Ok(());
            }
            if self.direct {
                self.resize(gpu)?;
                if let Some(frame) = self.pending_gpu.take()
                    && frame.info.visible[2..] == [self.size.width, self.size.height]
                {
                    self.sequence = frame.info.sequence;
                    self.direct_frame = Some(frame);
                    self.dirty = true;
                }
                return Ok(());
            }
            self.prepare_size(gpu)?;
            if let Some(frame) = self.pending_gpu.take() {
                let visible = frame.info.visible;
                let sequence = frame.info.sequence;
                let result = if visible[2] == self.size.width && visible[3] == self.size.height {
                    gpu.copy_dma_buf(
                        self.source.as_ref().unwrap(),
                        &frame,
                        frame_damage(self.sequence, sequence, self.damage, self.size),
                    )
                } else {
                    Ok(())
                };
                // The copy finishes GPU reads before releasing CEF's callback.
                frame.complete(result.is_ok());
                if let Err(error) = result {
                    self.fallback(format!("{error:#}"))?;
                    return Ok(());
                }
                if visible[2] == self.size.width && visible[3] == self.size.height {
                    self.sequence = sequence;
                    self.source_bgra = false;
                    self.dirty = true;
                }
            }
            Ok(())
        }
        pub fn draw(
            &mut self,
            gpu: &Gpu,
            fbo: Option<glow::Framebuffer>,
            force: bool,
        ) -> Result<bool> {
            self.prepare_size(gpu)?;
            self.prepare_gpu(gpu)?;
            if let Some(frame) = self.direct_frame()
                && self.source_sequence != self.sequence
            {
                // Snapshots and transitions still need an owned texture. The direct
                // steady path never calls draw or allocates this texture.
                if let Err(error) = gpu.copy_dma_buf(
                    self.source.as_ref().unwrap(),
                    frame,
                    frame_damage(self.source_sequence, self.sequence, self.damage, self.size),
                ) {
                    self.fallback(format!("{error:#}"))?;
                    return Ok(false);
                }
                self.source_sequence = self.sequence;
                self.source_bgra = false;
            }
            let mut changed = self.accelerated && self.dirty && self.sequence != 0;
            if self.accelerated && !self.direct {
                self.dirty = false;
            }
            if !self.accelerated && self.dirty {
                changed = self
                    .browser
                    .with_pixels(|pixels| {
                        let sequence = pixels.sequence;
                        let [width, height] = pixels.size;

                        let changed = sequence != self.sequence
                            && width == self.size.width
                            && height == self.size.height;
                        if changed {
                            let damage =
                                frame_damage(self.sequence, sequence, self.damage, self.size);
                            self.source
                                .as_ref()
                                .unwrap()
                                .upload_rect(pixels.data, damage);
                            self.sequence = sequence;
                            self.source_bgra = true;
                        }
                        if width == self.size.width && height == self.size.height {
                            self.dirty = false;
                        }
                        changed
                    })?
                    .unwrap_or(false);
            }
            if (changed || force)
                && self.sequence != 0
                && let Some(source) = &self.source
            {
                gpu.draw_raster(source, fbo, crate::domain::Fit::Stretch, self.source_bgra);
                return Ok(true);
            }
            Ok(false)
        }
    }
    fn frame_damage(
        previous: u64,
        sequence: u64,
        damage: Option<(u64, [u32; 4])>,
        size: Size,
    ) -> [u32; 4] {
        damage
            .filter(|(published, rect)| {
                previous != 0
                    && *published == sequence
                    && previous.checked_add(1) == Some(sequence)
                    && rect[2] > 0
                    && rect[3] > 0
                    && rect[0]
                        .checked_add(rect[2])
                        .is_some_and(|x| x <= size.width)
                    && rect[1]
                        .checked_add(rect[3])
                        .is_some_and(|y| y <= size.height)
            })
            .map_or([0, 0, size.width, size.height], |(_, rect)| rect)
    }
    impl Drop for Web {
        fn drop(&mut self) {
            // Release a taken frame before the renderer posts its asynchronous close.
            self.pending_gpu.take();
        }
    }
    fn web_properties(values: &Values) -> Value {
        Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    let value = if let Some(channels) = value.as_array() {
                        Value::String(
                            channels
                                .iter()
                                .map(Value::to_string)
                                .collect::<Vec<_>>()
                                .join(" "),
                        )
                    } else {
                        value.clone()
                    };
                    (key.clone(), value)
                })
                .collect(),
        )
    }

    #[cfg(test)]
    pub(crate) mod tests {
        use super::*;
        use std::thread;

        pub(crate) fn web_browser_gpu_import_failure_restores_paused_properties_and_releases_fds() {
            if std::env::var("WALLPAPERD_WEB_GPU").as_deref() != Ok("1") {
                return;
            }
            let project = tempfile::tempdir().unwrap();
            let entry = project.path().join("index.html");
            std::fs::write(&entry, r#"<!doctype html><body style="margin:0;background:red"><script>
        wallpaperPropertyListener={applyUserProperties(p){document.body.style.background=p.tint.value}};
        </script>"#).unwrap();
            let gpu = Gpu::headless(Size::new(64, 64).unwrap()).unwrap();
            let target = Target::new(gpu.gl.clone(), gpu.size).unwrap();
            let (wake, _reader) = UnixStream::pair().unwrap();
            let playback = Playback {
                paused: true,
                fps: 60,
                ..Playback::default()
            };
            let properties = Values::from([("tint".into(), json!("lime"))]);
            let mut web =
                Web::load(project.path(), &entry, &gpu, playback, &properties, &wake).unwrap();
            assert_eq!(web.transport(), "dmabuf");
            let fd_count = || std::fs::read_dir("/proc/self/fd").unwrap().count();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                web.poll().unwrap();
                if let Some(frame) = &mut web.pending_gpu {
                    frame.info.format = 0; // Fail the real import while the CEF owns its pool slot.
                    break;
                }
                assert!(Instant::now() < deadline, "GPU frame timed out");
                thread::sleep(Duration::from_millis(5));
            }
            let before = fd_count();
            web.draw(&gpu, Some(target.fbo), false).unwrap();
            assert_eq!(web.transport(), "mmap");
            assert_eq!(web.state["playback"]["paused"], true);
            assert_eq!(web.state["properties"]["tint"], "lime");
            assert!(
                web.fallback
                    .as_deref()
                    .unwrap()
                    .contains("unsupported DMA-BUF format")
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            while !web.ready() {
                web.poll().unwrap();
                web.draw(&gpu, Some(target.fbo), false).unwrap();
                assert!(Instant::now() < deadline, "CPU fallback did not render");
                thread::sleep(Duration::from_millis(5));
            }
            // CEF closes a browser and its network context asynchronously.
            let released = Instant::now() + Duration::from_secs(2);
            while fd_count() > before && Instant::now() < released {
                web.poll().unwrap();
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                fd_count() <= before,
                "old browser descriptors leaked: before={before}, after={}",
                fd_count()
            );
            let first = gpu.read_image(&target).unwrap();
            assert_eq!(first.get_pixel(32, 32).0, [0, 255, 0, 255]);
            thread::sleep(Duration::from_millis(150));
            web.poll().unwrap();
            assert!(!web.draw(&gpu, Some(target.fbo), false).unwrap());
            assert_eq!(first, gpu.read_image(&target).unwrap());
        }
        pub(crate) fn invalid_controls_leave_browser_alive(web: &mut Web) {
            assert!(web.browser.send(json!([]), false).is_err());
            assert!(
                web.browser
                    .send(json!({"oversized":"x".repeat(64*1024)}), false)
                    .is_err()
            );
            web.browser.send(web.state.clone(), false).unwrap();
        }
    }
}

#[cfg(not(feature = "web"))]
mod disabled {
    //! Builds without the optional CEF dependency keep the same content boundary.
    use crate::{domain::Playback, graphics::Gpu, properties::Values};
    use anyhow::Result;
    use serde_json::{Value, json};
    use std::{os::unix::net::UnixStream, path::Path};

    pub fn capability() -> Value {
        json!({"available":false,"content":["we_web"],"properties":true,
        "input":true,"audio":true,"media":true,"pause":true,"fps_limit":true,
        "media_thumbnails":false,"synchronized_playback":false,"frame_output":["texture"],
        "requires":"matching CEF runtime, EGL/GLES 3","helper":null,
        "error":"wallpaperd was built without Web support; run cargo build --locked --features web --release --target-dir target/we-web"})
    }

    // No instance can be constructed when the backend was omitted at build time.
    pub enum Web {}
    impl Web {
        pub fn load(
            _: &Path,
            _: &Path,
            _: &Gpu,
            _: Playback,
            _: &Values,
            _: &UnixStream,
        ) -> Result<Self> {
            anyhow::bail!(
                "wallpaperd was built without Web support; run cargo build --locked --features web --release --target-dir target/we-web"
            )
        }
        pub fn playback(&mut self, _: Playback) -> Result<()> {
            match *self {}
        }
        pub fn properties(&mut self, _: &Values) -> Result<()> {
            match *self {}
        }
        pub fn pointer(&mut self, _: Option<[f32; 2]>, _: Option<bool>) -> Result<()> {
            match *self {}
        }
        pub fn audio(&mut self, _: &we_scene::audio::AudioSnapshot) {
            match *self {}
        }
        pub fn media(&mut self, _: &crate::media::Snapshot) {
            match *self {}
        }
        pub fn requires_audio(&self) -> bool {
            match *self {}
        }
        pub fn needs_render(&self) -> bool {
            match *self {}
        }
        pub fn diagnostics(&self) -> Value {
            match *self {}
        }
        pub fn ready(&self) -> bool {
            match *self {}
        }
        pub fn poll(&mut self) -> Result<()> {
            match *self {}
        }
        pub fn prepare_gpu(&mut self, _: &Gpu) -> Result<()> {
            match *self {}
        }
        pub fn draw(&mut self, _: &Gpu, _: Option<glow::Framebuffer>, _: bool) -> Result<bool> {
            match *self {}
        }
    }
}
