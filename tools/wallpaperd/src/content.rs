//! Engines own content and playback; the worker owns presentation and transactions.
use std::{fs::File, io::Read, os::unix::net::UnixStream, time::Duration};

use anyhow::{Context, Result};
use glow::HasContext;
#[cfg(test)]
use std::time::Instant;
use wallpaper_media::clock::PlaybackClock as Clock;

use crate::{
    catalog::{self, Kind},
    domain::{Playback, Selection},
    graphics::{Gpu, Program, Target},
};

enum Engine {
    Image,
    Video(wallpaper_media::Player),
    Web(Box<crate::web::Web>),
    RustScene {
        renderer: Box<we_scene::Renderer>,
        clock: Clock,
        fit: we_scene::Fit,
    },
    Shader {
        program: Program<4>,
        clock: Clock,
        frame: i32,
    },
}

pub struct Content {
    engine: Engine,
    // Dynamic content retains a texture only while preparing or blending frames.
    target: Option<Target>,
    dirty: bool,
    paused: bool,
    ready: bool,
    failed: bool,
    mouse: Mouse,
}

impl Content {
    pub fn image(gpu: &Gpu, image: image::RgbaImage, selection: &Selection) -> Result<Self> {
        Ok(Self {
            target: Some(gpu.image(image, selection.fit)?),
            engine: Engine::Image,
            dirty: false,
            paused: false,
            ready: true,
            failed: false,
            mouse: Mouse::default(),
        })
    }

    #[cfg(test)]
    pub fn load(
        gpu: &Gpu,
        selection: &Selection,
        playback: Playback,
        wake: &UnixStream,
    ) -> Result<Self> {
        Self::load_with_properties(gpu, selection, playback, wake, &Default::default())
    }

    pub fn load_with_properties(
        gpu: &Gpu,
        selection: &Selection,
        playback: Playback,
        wake: &UnixStream,
        properties: &crate::properties::Values,
    ) -> Result<Self> {
        Self::load_synchronized(gpu, selection, playback, wake, properties, None)
    }

    pub fn load_synchronized(
        gpu: &Gpu,
        selection: &Selection,
        playback: Playback,
        wake: &UnixStream,
        properties: &crate::properties::Values,
        timeline: Option<wallpaper_media::clock::Timeline>,
    ) -> Result<Self> {
        Self::load_for_output(gpu, selection, playback, wake, properties, timeline, None)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn load_for_output(
        gpu: &Gpu,
        selection: &Selection,
        playback: Playback,
        wake: &UnixStream,
        properties: &crate::properties::Values,
        timeline: Option<wallpaper_media::clock::Timeline>,
        output: Option<&str>,
    ) -> Result<Self> {
        let engine = match catalog::kind(catalog::path(selection)) {
            Some(Kind::WeWeb) => {
                let source = catalog::source(selection).map_err(|e| anyhow::anyhow!(e.message))?;
                Engine::Web(Box::new(crate::web::Web::load(
                    catalog::path(selection),
                    &source,
                    gpu,
                    playback,
                    properties,
                    wake,
                )?))
            }
            Some(Kind::WeScene) => Self::scene(gpu, selection, playback, wake, properties, output)?,
            Some(Kind::Video) => {
                let source = catalog::source(selection).map_err(|e| anyhow::anyhow!(e.message))?;
                Engine::Video(wallpaper_media::Player::video(
                    mpv_context(gpu, selection.fit),
                    &source,
                    mpv_playback(playback),
                    wake,
                )?)
            }
            Some(Kind::Shader) => {
                let mut source = String::new();
                File::open(catalog::path(selection))?
                    .take(1024 * 1024 + 1)
                    .read_to_string(&mut source)?;
                anyhow::ensure!(source.len() <= 1024 * 1024, "shader exceeds 1 MiB");
                let fragment = format!(
                    "#version 300 es\nprecision highp float;\nuniform float iTime;\nuniform vec3 iResolution;\nuniform int iFrame;\nuniform vec4 iMouse;\nout vec4 wallpaperColor;\n#line 1\n{source}\nvoid main() {{ mainImage(wallpaperColor, gl_FragCoord.xy); wallpaperColor.a = 1.0; }}"
                );
                let program = Program::new(
                    gpu.gl.clone(),
                    &fragment,
                    ["iTime", "iResolution", "iFrame", "iMouse"],
                )
                .context("loading fragment shader")?;
                Engine::Shader {
                    program,
                    clock: Clock::new(playback.paused),
                    frame: 0,
                }
            }
            _ => anyhow::bail!("unsupported dynamic content type"),
        };
        let mut content = Self {
            engine,
            target: None,
            dirty: true,
            paused: playback.paused,
            ready: false,
            failed: false,
            mouse: Mouse::default(),
        };
        if let Some(timeline) = timeline {
            content.synchronize(timeline)?;
        }
        Ok(content)
    }

    fn scene(
        gpu: &Gpu,
        selection: &Selection,
        playback: Playback,
        wake: &UnixStream,
        properties: &crate::properties::Values,
        output: Option<&str>,
    ) -> Result<Engine> {
        let config = crate::store::Config::load()?.wallpaper_engine;
        anyhow::ensure!(
            !matches!(config.scene_backend, crate::store::SceneBackend::Native),
            "native WE backend has been removed; set wallpaper_engine.scene_backend to rust or auto"
        );
        let package = catalog::source(selection).map_err(|e| anyhow::anyhow!(e.message))?;
        let common = crate::catalog::we_assets(&config).ok();
        let result = we_scene::Renderer::load_with_storage(
            gpu.gl.clone(),
            catalog::path(selection),
            &package,
            common.as_deref(),
            properties,
            output.map(|output| we_scene::ScriptStorage {
                directory: crate::store::state_path().parent().unwrap().join("scenes"),
                output: output.into(),
            }),
        )
        .and_then(|mut renderer| {
            renderer.attach_media(
                mpv_context(gpu, crate::domain::Fit::Stretch),
                wake,
                mpv_playback(playback),
            )?;
            Ok(renderer)
        });
        gpu.reset();
        let mut renderer = result.context("loading Rust scene")?;
        renderer.pause(playback.paused);
        crate::decoder::release_scratch();
        Ok(Engine::RustScene {
            renderer: Box::new(renderer),
            clock: Clock::new(playback.paused),
            fit: selection.fit,
        })
    }

    pub fn properties_available(&self) -> bool {
        matches!(self.engine, Engine::RustScene { .. } | Engine::Web(_))
    }

    pub fn media_available(&self) -> bool {
        matches!(&self.engine, Engine::RustScene { .. } | Engine::Web(_))
    }
    pub fn requires_audio(&self) -> bool {
        match &self.engine {
            Engine::RustScene { renderer, .. } => renderer.requires_audio(),
            Engine::Web(web) => web.requires_audio(),
            _ => false,
        }
    }
    pub fn set_audio(&mut self, audio: &we_scene::audio::AudioSnapshot) {
        if let Engine::Web(web) = &mut self.engine {
            web.audio(audio);
        }
        if let Engine::RustScene { renderer, .. } = &mut self.engine {
            renderer.set_audio(audio);
            self.dirty |= !self.paused && renderer.requires_audio();
        }
    }

    pub fn error_generation(&self) -> u64 {
        match &self.engine {
            Engine::Web(web) => u64::from(web.requires_audio()),
            Engine::RustScene { renderer, .. } => renderer.error_generation(),
            _ => 0,
        }
    }
    pub fn runtime_errors(&self) -> Option<String> {
        match &self.engine {
            Engine::RustScene { renderer, .. } if renderer.error_generation() > 0 => {
                let errors = renderer.errors();
                (!errors.is_empty()).then_some(errors)
            }
            _ => None,
        }
    }

    pub fn set_media(&mut self, media: &crate::media::Snapshot) {
        if let Engine::Web(web) = &mut self.engine {
            web.media(media);
        }
        if let Engine::RustScene { renderer, .. } = &mut self.engine {
            renderer.set_media(serde_json::to_value(media).unwrap_or_default());
            self.dirty |= !self.paused;
        }
    }

    pub fn set_properties(&mut self, properties: &crate::properties::Values) -> Result<()> {
        match &mut self.engine {
            Engine::Web(web) => web.properties(properties),
            Engine::RustScene { renderer, .. } => {
                renderer.set_properties(properties)?;
                self.dirty |= !self.paused;
                Ok(())
            }
            _ => anyhow::bail!("current backend does not support user properties"),
        }
    }

    pub fn backend(&self) -> crate::domain::Backend {
        use crate::domain::Backend;
        match self.engine {
            Engine::Image => Backend::Image,
            Engine::Video(_) => Backend::Libmpv,
            Engine::Web(_) => Backend::Cef,
            Engine::RustScene { .. } => Backend::RustScene,
            Engine::Shader { .. } => Backend::Shader,
        }
    }

    pub fn playback(&mut self, mut playback: Playback) -> Result<()> {
        playback.paused |= self.failed;
        if playback.paused && !self.paused && self.mouse.down {
            self.pointer(None, None)?;
        }
        match &mut self.engine {
            Engine::Web(web) => web.playback(playback)?,
            Engine::Video(video) => video.playback(mpv_playback(playback))?,
            Engine::Shader { clock, .. } => clock.pause(playback.paused),
            Engine::RustScene {
                renderer, clock, ..
            } => {
                self.dirty |= renderer.pause(playback.paused);
                renderer.media_playback(mpv_playback(playback))?;
                clock.pause(playback.paused)
            }
            Engine::Image => {}
        }
        self.paused = playback.paused;
        Ok(())
    }

    pub fn synchronize(&mut self, timeline: wallpaper_media::clock::Timeline) -> Result<()> {
        match &mut self.engine {
            Engine::Video(video) => video.synchronize(timeline),
            Engine::RustScene {
                renderer, clock, ..
            } => {
                clock.synchronize(timeline);
                renderer.synchronize_media(timeline)
            }
            Engine::Shader { clock, .. } => {
                clock.synchronize(timeline);
                Ok(())
            }
            Engine::Image | Engine::Web(_) => Ok(()),
        }
    }

    pub fn accepts_pointer(&self) -> bool {
        !self.failed
            && match &self.engine {
                Engine::Shader { program, .. } => program.uniform(3).is_some(),
                Engine::RustScene { renderer, .. } => renderer.accepts_pointer(),
                Engine::Web(_) => true,
                _ => false,
            }
    }

    pub fn pointer(&mut self, position: Option<[f32; 2]>, button: Option<bool>) -> Result<()> {
        if !self.accepts_pointer() {
            return Ok(());
        }
        if let Engine::Web(web) = &mut self.engine {
            if !self.paused || position.is_none() {
                web.pointer(position, button)?;
                self.mouse.update(position, button);
                self.mouse.focused = position.is_some();
                if position.is_none() {
                    self.mouse.down = false;
                }
            }
            return Ok(());
        }
        if let Engine::RustScene { renderer, .. } = &mut self.engine {
            self.mouse.update(position, button);
            self.mouse.focused = position.is_some();
            if position.is_none() {
                self.mouse.down = false;
            }
            renderer.pointer(position, button);
            self.dirty |= !self.paused;
            return Ok(());
        }
        if self.paused {
            return Ok(());
        }
        if position.is_none() {
            if self.mouse.down || self.mouse.focused {
                self.mouse.down = false;
                self.mouse.focused = false;
                self.dirty = true;
            }
            return Ok(());
        }
        let button = button.filter(|down| *down != self.mouse.down);
        if button.is_none() && self.mouse.focused && position == Some(self.mouse.position) {
            return Ok(());
        }
        self.mouse.update(position, button);
        self.dirty = true;
        Ok(())
    }

    pub fn poll(&mut self) -> Result<()> {
        if self.failed {
            return Ok(());
        }
        match &mut self.engine {
            Engine::Web(web) => web.poll()?,
            Engine::Video(video) => video.poll()?,
            Engine::RustScene { renderer, .. } => {
                self.dirty |= !self.paused && renderer.poll_media()?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn diagnostics(&self) -> String {
        match &self.engine {
            Engine::RustScene { renderer, .. } => renderer.diagnostics(),
            Engine::Web(web) => web.diagnostics().to_string(),
            _ => String::new(),
        }
    }

    /// Release borrowed browser GPU frames even while Wayland stops frame callbacks.
    pub fn prepare_frame(&mut self, gpu: Option<&Gpu>) -> Result<()> {
        if let (Some(gpu), Engine::Web(web)) = (gpu, &mut self.engine) {
            web.prepare_gpu(gpu)?;
            #[cfg(feature = "web")]
            if web.direct_frame().is_some() {
                self.ready = true;
            }
        }
        Ok(())
    }

    #[cfg(feature = "web")]
    pub fn direct_frame(&self) -> Option<&crate::web::frames::Frame> {
        match &self.engine {
            Engine::Web(web) => web.direct_frame(),
            _ => None,
        }
    }

    #[cfg(feature = "web")]
    pub fn direct_presented(&mut self) {
        if let Engine::Web(web) = &mut self.engine {
            web.direct_presented();
            self.target = None;
            self.dirty = false;
        }
    }

    pub fn animated(&self) -> bool {
        !self.paused
            && match &self.engine {
                Engine::Image => false,
                Engine::RustScene { renderer, .. } => renderer.animated(),
                _ => true,
            }
    }
    pub fn dynamic(&self) -> bool {
        !matches!(self.engine, Engine::Image)
    }

    pub fn needs_render(&self) -> bool {
        if self.failed {
            return false;
        }
        match &self.engine {
            Engine::Image => false,
            Engine::Video(video) => video.needs_render(self.dirty),
            Engine::Web(web) => self.dirty || web.needs_render(),
            Engine::Shader { .. } => self.dirty || !self.paused,
            Engine::RustScene { renderer, .. } => {
                self.dirty || (!self.paused && renderer.animated())
            }
        }
    }
    pub fn ready(&self) -> bool {
        self.ready
            && match &self.engine {
                Engine::Video(video) => video.ready(),
                Engine::Web(web) => web.ready(),
                _ => true,
            }
    }

    pub fn target(&self) -> &Target {
        self.target.as_ref().expect("offscreen frame rendered")
    }

    pub fn into_snapshot(self) -> Target {
        self.target.expect("snapshot rendered before transition")
    }

    pub fn snapshot(&mut self, gpu: &Gpu) -> Result<()> {
        if self.dynamic() && self.target.is_none() {
            self.render_to(gpu, false, true)?;
        }
        Ok(())
    }

    pub fn export_frame(&mut self, gpu: &Gpu) -> Result<image::RgbaImage> {
        let transient = self.dynamic() && self.target.is_none();
        if self.dynamic() {
            self.poll()?;
            self.render_to(gpu, false, true)?;
        }
        let image = gpu.read_image(self.target());
        if transient {
            self.target = None;
        }
        image
    }

    pub fn fail(&mut self, mut playback: Playback) {
        self.failed = true;
        playback.paused = true;
        let _ = self.playback(playback);
    }

    pub fn resized(&mut self) {
        if !self.failed && self.dynamic() {
            self.target = None;
            self.dirty = true;
        }
    }

    pub fn render(&mut self, gpu: &Gpu) -> Result<bool> {
        self.render_to(gpu, false, false)
    }

    pub fn render_direct(&mut self, gpu: &Gpu, force: bool) -> Result<bool> {
        let changed = self.render_to(gpu, true, force)?;
        if changed {
            self.target = None;
        }
        Ok(changed)
    }

    fn render_to(&mut self, gpu: &Gpu, direct: bool, force: bool) -> Result<bool> {
        if !direct && self.target.is_none() {
            self.target = Some(Target::new(gpu.gl.clone(), gpu.size)?);
        }
        let fbo = if direct {
            None
        } else {
            Some(self.target().fbo)
        };
        let changed = match &mut self.engine {
            Engine::Image => false,
            Engine::Web(web) => web.draw(gpu, fbo, self.dirty || force)?,
            Engine::Video(video) => video.render(
                fbo.map_or(0, |fbo| fbo.0.get()),
                [gpu.size.width, gpu.size.height],
                self.dirty || force,
            )?,
            Engine::RustScene {
                renderer,
                clock,
                fit,
            } if self.dirty || force || (!self.paused && renderer.animated()) => {
                let result = renderer.draw(
                    [gpu.size.width, gpu.size.height],
                    fbo,
                    *fit,
                    clock.elapsed().as_secs_f32(),
                );
                gpu.reset();
                result?;
                true
            }
            Engine::Shader {
                program,
                clock,
                frame,
            } if self.dirty || force || !self.paused => {
                program.bind();
                // SAFETY: this engine's program and target belong to the current worker context.
                unsafe {
                    gpu.gl.bind_framebuffer(glow::FRAMEBUFFER, fbo);
                    gpu.gl
                        .viewport(0, 0, gpu.size.width as i32, gpu.size.height as i32);
                    gpu.gl
                        .uniform_1_f32(program.uniform(0), clock.elapsed().as_secs_f32());
                    gpu.gl.uniform_3_f32(
                        program.uniform(1),
                        gpu.size.width as f32,
                        gpu.size.height as f32,
                        1.0,
                    );
                    gpu.gl.uniform_1_i32(program.uniform(2), *frame);
                    let mouse = self.mouse.uniform(gpu.size);
                    gpu.gl.uniform_4_f32(
                        program.uniform(3),
                        mouse[0],
                        mouse[1],
                        mouse[2],
                        mouse[3],
                    );
                    gpu.gl.draw_arrays(glow::TRIANGLES, 0, 3);
                }
                gpu.reset();
                *frame = frame.saturating_add(1);
                true
            }
            Engine::Shader { .. } | Engine::RustScene { .. } => false,
        };
        if changed {
            self.ready = match &self.engine {
                Engine::RustScene { renderer, .. } => renderer.ready(),
                Engine::Video(video) => video.ready(),
                Engine::Web(web) => web.ready(),
                _ => true,
            };
            self.dirty = !self.ready;
        }
        Ok(changed)
    }

    pub fn report_swap(&self) {
        if let Engine::Video(video) = &self.engine {
            video.report_swap();
        }
        if let Engine::RustScene { renderer, .. } = &self.engine {
            renderer.report_swap();
        }
    }
}

#[cfg(all(test, feature = "web"))]
#[path = "content/web_tests.rs"]
mod web_tests;

/// Render one deterministic shader frame without a desktop surface or playback loop.
pub fn shader_thumbnail(path: &std::path::Path) -> Result<image::RgbaImage> {
    let gpu = Gpu::headless(crate::pixels::Size::new(640, 360)?)?;
    let selection = catalog::resolve(&path.to_string_lossy(), crate::domain::Fit::Stretch)
        .map_err(|error| anyhow::anyhow!(error.message))?;
    anyhow::ensure!(
        catalog::kind(catalog::path(&selection)) == Some(Kind::Shader),
        "thumbnail worker requires a shader"
    );
    let (wake, _reader) = UnixStream::pair()?;
    let mut content = Content::load_with_properties(
        &gpu,
        &selection,
        Playback {
            paused: true,
            ..Playback::default()
        },
        &wake,
        &Default::default(),
    )?;
    if let Engine::Shader { clock, .. } = &mut content.engine {
        clock.seek(Duration::from_secs(2));
    }
    content.render(&gpu)?;
    gpu.read_image(content.target())
}

pub(crate) fn mpv_context(gpu: &Gpu, fit: crate::domain::Fit) -> wallpaper_media::RenderContext {
    wallpaper_media::RenderContext {
        resolver: gpu.resolver(),
        native_display: gpu.native_display,
        software: gpu.software(),
        flip_y: true,
        pulse_server: None,
        fit,
    }
}
fn mpv_playback(value: Playback) -> wallpaper_media::Playback {
    wallpaper_media::Playback {
        paused: value.paused,
        mute: value.mute,
        volume: value.volume as f32,
    }
}

/// Normalize logical output coordinates before either engine applies output scale or fit.
pub(crate) fn normalized_pointer(position: (f64, f64), logical: (u32, u32)) -> Option<[f32; 2]> {
    if logical.0 == 0 || logical.1 == 0 || !position.0.is_finite() || !position.1.is_finite() {
        return None;
    }
    Some([
        (position.0 / f64::from(logical.0)).clamp(0.0, 1.0) as f32,
        (position.1 / f64::from(logical.1)).clamp(0.0, 1.0) as f32,
    ])
}

#[derive(Clone, Copy)]
pub struct Mouse {
    pub position: [f32; 2],
    pub origin: Option<[f32; 2]>,
    pub down: bool,
    pub focused: bool,
}
impl Default for Mouse {
    fn default() -> Self {
        Self {
            position: [0.5; 2],
            origin: None,
            down: false,
            focused: false,
        }
    }
}
impl Mouse {
    pub fn update(&mut self, position: Option<[f32; 2]>, button: Option<bool>) {
        if let Some(position) = position {
            self.position = position;
            self.focused = true;
        }
        if let Some(down) = button {
            if down && !self.down {
                self.origin = Some(self.position);
            }
            self.down = down;
        }
    }

    pub fn uniform(self, size: crate::pixels::Size) -> [f32; 4] {
        let pixels = |p: [f32; 2]| [p[0] * size.width as f32, (1.0 - p[1]) * size.height as f32];
        let xy = pixels(self.position);
        let zw = self.origin.map_or([0.0; 2], |p| {
            pixels(p).map(|v| {
                let value = v.max(f32::MIN_POSITIVE);
                if self.down { value } else { -value }
            })
        });
        [xy[0], xy[1], zw[0], zw[1]]
    }
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
use serde_json::json;
#[cfg(test)]
use test_support::{
    assert_gl_clean, canvas, fixture, frame, rendered, renderer, set_properties, tex_header,
    write_json,
};

#[cfg(test)]
#[path = "content/tests.rs"]
mod tests;

#[cfg(test)]
mod media_tests;

#[cfg(test)]
include!("content/compositing_tests.rs");
#[cfg(test)]
include!("content/scene_tests.rs");
#[cfg(test)]
use model_tests::mdl_tests;

#[cfg(test)]
mod mouse_tests {
    use super::*;

    #[test]
    fn logical_coordinates_and_click_origin_survive_scale_and_resize() {
        let pos = normalized_pointer((100.0, 25.0), (200, 100)).unwrap();
        let mut mouse = Mouse::default();
        mouse.update(Some(pos), Some(true));
        assert_eq!(
            mouse.uniform(crate::pixels::Size::new(300, 150).unwrap()),
            [150.0, 112.5, 150.0, 112.5]
        );
        mouse.update(Some([0.75, 0.5]), Some(false));
        assert_eq!(
            mouse.uniform(crate::pixels::Size::new(400, 200).unwrap()),
            [300.0, 100.0, -200.0, -150.0]
        );
        assert!(normalized_pointer((f64::NAN, 0.0), (100, 100)).is_none());
        assert!(normalized_pointer((0.0, 0.0), (0, 100)).is_none());
    }
}
