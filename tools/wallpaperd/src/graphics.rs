//! Worker-owned EGL context and textures. All GL calls stay on the Wayland thread.
use std::{ffi::c_void, rc::Rc};

use anyhow::{Context, Result, bail};
use glow::HasContext;
use khronos_egl as egl;
use wayland_client::{Proxy, protocol::wl_surface::WlSurface};
use wayland_egl::WlEglSurface;

use crate::{
    domain::{Fit, Transition},
    pixels::Size,
};

const VERTEX: &str = "#version 300 es\nprecision highp float;\nout vec2 uv;\nvoid main() { vec2 p = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2); uv = p; gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0); }";
const COMPOSE: &str = r#"#version 300 es
precision highp float;
precision highp int;
in vec2 uv;
out vec4 color;
uniform sampler2D old_frame;
uniform sampler2D new_frame;
uniform float progress;
uniform int transition;
uniform vec4 geometry; // output extent, feather, inverse stripe delay span
uniform float edge;

float coverage() {
    if (progress <= 0.0) return 0.0;
    if (progress >= 1.0) return 1.0;
    // The integer values follow domain::Transition; cut/fade retain their behavior.
    if (transition <= 1) return progress;
    float t = progress;
    vec2 extent = geometry.xy;
    vec2 p = (uv - 0.5) * extent;
    float feather = geometry.z;
    float field;
    float front = edge;
    if (transition == 3) {
        float radius = length(p);
        // A cell center is at most 0.036 screen units away from its pixels.
        if (radius + 0.036 <= front - feather) return 1.0;
        if (radius - 0.036 >= front + feather) return 0.0;
        // Nearest center in two offset triangular lattices gives hexagonal cells.
        float cell_size = 0.06;
        vec2 grid = p / cell_size;
        vec2 span = vec2(1.0, 1.7320508);
        vec2 a = mod(grid, span) - span * 0.5;
        vec2 b = mod(grid - span * 0.5, span) - span * 0.5;
        vec2 local = dot(a, a) < dot(b, b) ? a : b;
        field = length(p - local * cell_size);
    } else if (transition == 4) {
        // A periodic three-arm spiral has no seam at the polar angle wrap.
        float amplitude = 0.13;
        field = length(p);
        // Evaluate polar trigonometry only in the spiral's moving edge band.
        if (field + amplitude <= front - feather) return 1.0;
        if (field - amplitude >= front + feather) return 0.0;
        float angle = atan(p.y, p.x + 0.0000001);
        field += amplitude * sin(3.0 * angle - 18.0 * field + 6.2831853 * t);
    } else if (transition == 5) {
        vec2 direction = vec2(0.8660254, 0.5);
        vec2 perpendicular = vec2(-direction.y, direction.x);
        float stripe = dot(p, direction) / 0.08;
        float fraction = fract(stripe);
        field = mod(floor(stripe), 2.0) < 1.0 ? fraction : 1.0 - fraction;
        float delay = (dot(p, perpendicular) * geometry.w + 0.5) * 0.2;
        float local_progress = clamp((t - delay) / (1.0 - delay), 0.0, 1.0);
        front = mix(-feather, 1.0 + feather, local_progress);
    } else {
        field = length(p);
    }
    return 1.0 - smoothstep(front - feather, front + feather, field);
}

void main() {
    float mask = coverage();
    if (transition <= 1) {
        color = vec4(mix(texture(old_frame, uv).rgb, texture(new_frame, uv).rgb, mask), 1.0);
        return;
    }
    // Fully revealed/covered regions need only one wallpaper texture fetch.
    if (mask <= 0.0) color = vec4(texture(old_frame, uv).rgb, 1.0);
    else if (mask >= 1.0) color = vec4(texture(new_frame, uv).rgb, 1.0);
    else color = vec4(mix(texture(old_frame, uv).rgb, texture(new_frame, uv).rgb, mask), 1.0);
}
"#;
const COPY: &str = "#version 300 es\nprecision highp float;\nin vec2 uv;\nout vec4 color;\nuniform sampler2D source;\nvoid main() { color = vec4(texture(source, uv).rgb, 1.0); }";
const IMAGE: &str = "#version 300 es\nprecision highp float;\nin vec2 uv;\nout vec4 color;\nuniform sampler2D source;\nuniform vec2 factor;\nuniform bool bgra;\nvoid main() { vec2 p = (uv - 0.5) * factor + 0.5; if (any(lessThan(p, vec2(0.0))) || any(greaterThan(p, vec2(1.0)))) color = vec4(0.0, 0.0, 0.0, 1.0); else { vec3 rgb = texture(source, vec2(p.x, 1.0-p.y)).rgb; color = vec4(bgra ? rgb.bgr : rgb, 1.0); } }";

struct Window {
    surface: egl::Surface,
    native: WlEglSurface,
}

pub struct Gpu {
    // Programs and GL objects are dropped while the context is still current.
    compose: Program<6>,
    copy: Program<1>,
    image: Program<3>,
    pub gl: Rc<glow::Context>,
    window: Option<Window>,
    pbuffer: Option<egl::Surface>,
    context: egl::Context,
    display: egl::Display,
    api: Rc<egl::DynamicInstance<egl::EGL1_5>>,
    pub size: Size,
    pub native_display: *mut c_void,
    software: bool,
}

impl Gpu {
    pub fn wayland(display: *mut c_void, surface: &WlSurface, size: Size) -> Result<Self> {
        Self::create(display, Some(surface), size)
    }

    pub fn headless(size: Size) -> Result<Self> {
        Self::create(std::ptr::null_mut(), None, size)
    }

    fn create(
        native_display: *mut c_void,
        surface: Option<&WlSurface>,
        size: Size,
    ) -> Result<Self> {
        // SAFETY: libEGL implements the loaded C ABI and remains owned by this context.
        let api = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required()? };
        // SAFETY: the worker keeps its Wayland connection alive until after Gpu is dropped.
        let display = unsafe {
            if surface.is_some() {
                api.get_display(native_display)
                    .context("EGL Wayland display")?
            } else {
                api.get_platform_display(0x31DD, std::ptr::null_mut(), &[egl::ATTRIB_NONE])?
            }
        };
        api.initialize(display).context("initializing EGL")?;
        let initialized = (|| -> Result<_> {
            api.bind_api(egl::OPENGL_ES_API)?;
            let config = api
                .choose_first_config(
                    display,
                    &[
                        egl::SURFACE_TYPE,
                        if surface.is_some() {
                            egl::WINDOW_BIT
                        } else {
                            egl::PBUFFER_BIT
                        },
                        egl::RENDERABLE_TYPE,
                        egl::OPENGL_ES3_BIT,
                        egl::RED_SIZE,
                        8,
                        egl::GREEN_SIZE,
                        8,
                        egl::BLUE_SIZE,
                        8,
                        egl::ALPHA_SIZE,
                        8,
                        egl::NONE,
                    ],
                )?
                .context("no GLES 3 EGL config")?;
            let context = api.create_context(
                display,
                config,
                None,
                &[egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE],
            )?;
            let target = (|| -> Result<_> {
                if let Some(surface) = surface {
                    let native =
                        WlEglSurface::new(surface.id(), size.width as i32, size.height as i32)?;
                    // SAFETY: native owns a wl_egl_window for the live surface, with a compatible config.
                    let surface = unsafe {
                        api.create_window_surface(
                            display,
                            config,
                            native.ptr().cast_mut().cast(),
                            None,
                        )?
                    };
                    Ok((Some(Window { surface, native }), None, surface))
                } else {
                    let pbuffer = api.create_pbuffer_surface(
                        display,
                        config,
                        &[
                            egl::WIDTH,
                            size.width as i32,
                            egl::HEIGHT,
                            size.height as i32,
                            egl::NONE,
                        ],
                    )?;
                    Ok((None, Some(pbuffer), pbuffer))
                }
            })();
            let (window, pbuffer, target) = match target {
                Ok(value) => value,
                Err(error) => {
                    let _ = api.destroy_context(display, context);
                    return Err(error);
                }
            };
            let setup = (|| -> Result<_> {
                api.make_current(display, Some(target), Some(target), Some(context))?;
                // Explicit Wayland frame callbacks provide pacing; swap must not wait for vsync.
                api.swap_interval(display, 0)?;
                // SAFETY: the EGL context is current and the function pointers live with the API.
                let gl = Rc::new(unsafe {
                    glow::Context::from_loader_function(|name| {
                        api.get_proc_address(name)
                            .map_or(std::ptr::null(), |p| p as *const c_void)
                    })
                });
                let compose = Program::new(
                    gl.clone(),
                    COMPOSE,
                    [
                        "old_frame",
                        "new_frame",
                        "progress",
                        "transition",
                        "geometry",
                        "edge",
                    ],
                )?;
                let copy = Program::new(gl.clone(), COPY, ["source"])?;
                let image = Program::new(gl.clone(), IMAGE, ["source", "factor", "bgra"])?;
                unsafe {
                    compose.bind();
                    gl.uniform_1_i32(compose.uniform(1), 1);
                    gl.use_program(None);
                }
                Ok((gl, compose, copy, image))
            })();
            match setup {
                Ok((gl, compose, copy, image)) => {
                    Ok((context, window, pbuffer, gl, compose, copy, image))
                }
                Err(error) => {
                    let _ = api.make_current(display, None, None, None);
                    let _ = api.destroy_surface(display, target);
                    let _ = api.destroy_context(display, context);
                    Err(error)
                }
            }
        })();
        match initialized {
            Ok((context, window, pbuffer, gl, compose, copy, image)) => {
                // SAFETY: initialization left this context current.
                let renderer =
                    unsafe { gl.get_parameter_string(glow::RENDERER) }.to_ascii_lowercase();
                let software = renderer.contains("llvmpipe") || renderer.contains("softpipe");
                Ok(Self {
                    compose,
                    copy,
                    image,
                    gl,
                    window,
                    pbuffer,
                    context,
                    display,
                    api: Rc::new(api),
                    size,
                    native_display,
                    software,
                })
            }
            Err(error) => {
                let _ = api.terminate(display);
                Err(error)
            }
        }
    }

    pub fn resize(&mut self, size: Size) {
        if self.size != size {
            if let Some(window) = &self.window {
                window
                    .native
                    .resize(size.width as i32, size.height as i32, 0, 0);
            }
            self.size = size;
        }
    }

    pub fn resolver(&self) -> Rc<egl::DynamicInstance<egl::EGL1_5>> {
        self.api.clone()
    }

    pub fn image(&self, image: image::RgbaImage, fit: Fit) -> Result<Target> {
        let source_size = Size::new(image.width(), image.height())?;
        let source = Target::new(self.gl.clone(), source_size)?;
        let target = Target::new(self.gl.clone(), self.size)?;
        source.upload(image.as_raw());
        self.draw_raster(&source, Some(target.fbo), fit, false);
        Ok(target)
    }

    /// Browser frames and decoded images share the same origin and fit mapping.
    pub fn draw_raster(
        &self,
        source: &Target,
        fbo: Option<glow::Framebuffer>,
        fit: Fit,
        bgra: bool,
    ) {
        // SAFETY: both textures and the destination belong to this current GL context.
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, fbo);
            self.gl
                .viewport(0, 0, self.size.width as i32, self.size.height as i32);
            self.image.bind();
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(source.texture));
            let (x, y) = image_factor(source.size, self.size, fit);
            self.gl.uniform_2_f32(self.image.uniform(1), x, y);
            self.gl
                .uniform_1_i32(self.image.uniform(2), i32::from(bgra));
            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
        self.reset();
    }

    pub fn present(
        &self,
        current: &Target,
        previous: Option<&Target>,
        transition: Transition,
        progress: f32,
    ) -> Result<()> {
        // SAFETY: every object belongs to this worker's current context.
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.gl
                .viewport(0, 0, self.size.width as i32, self.size.height as i32);
            self.gl.active_texture(glow::TEXTURE0);
            if let Some(previous) = previous.filter(|_| progress > 0.0 && progress < 1.0) {
                self.compose.bind();
                self.gl
                    .bind_texture(glow::TEXTURE_2D, Some(previous.texture));
                self.gl.active_texture(glow::TEXTURE1);
                self.gl
                    .bind_texture(glow::TEXTURE_2D, Some(current.texture));
                let short_side = self.size.width.min(self.size.height) as f32;
                let extent = (
                    self.size.width as f32 / short_side,
                    self.size.height as f32 / short_side,
                );
                let max_radius = extent.0.hypot(extent.1) * 0.5;
                let mut feather = (1.5 / short_side).max(0.012);
                let eased = progress * progress * (3.0 - 2.0 * progress);
                let (start, end) = match transition {
                    Transition::Honeycomb => (-feather, max_radius + 0.06 + feather),
                    Transition::Spiral => (-0.13 - feather, max_radius + 0.13 + feather),
                    _ => (-feather, max_radius + feather),
                };
                if transition == Transition::Stripes {
                    feather = (feather / 0.08).max(0.02);
                }
                self.gl.uniform_1_f32(
                    self.compose.uniform(2),
                    if transition == Transition::Fade {
                        progress
                    } else {
                        eased
                    },
                );
                self.gl
                    .uniform_1_i32(self.compose.uniform(3), transition as i32);
                self.gl.uniform_4_f32(
                    self.compose.uniform(4),
                    extent.0,
                    extent.1,
                    feather,
                    1.0 / (extent.0 * 0.5 + extent.1 * 0.8660254),
                );
                self.gl
                    .uniform_1_f32(self.compose.uniform(5), start + (end - start) * eased);
            } else {
                self.copy.bind();
                self.gl.bind_texture(
                    glow::TEXTURE_2D,
                    Some(if progress <= 0.0 {
                        previous.unwrap_or(current).texture
                    } else {
                        current.texture
                    }),
                );
            }
            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);
            self.reset();
        }
        self.swap()
    }

    pub fn swap(&self) -> Result<()> {
        if let Some(window) = &self.window {
            self.api
                .swap_buffers(self.display, window.surface)
                .context("submitting EGL buffer")?;
        }
        Ok(())
    }

    pub fn software(&self) -> bool {
        self.software
    }

    pub fn reset(&self) {
        // SAFETY: restore the GL state expected by libmpv, on the context's owning thread.
        unsafe {
            self.gl.use_program(None);
            self.gl.bind_vertex_array(None);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, None);
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.gl.active_texture(glow::TEXTURE1);
            self.gl.bind_texture(glow::TEXTURE_2D, None);
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, None);
            self.gl.disable(glow::BLEND);
            self.gl.disable(glow::SCISSOR_TEST);
        }
    }

    pub fn read_image(&self, target: &Target) -> Result<image::RgbaImage> {
        anyhow::ensure!(target.size == self.size, "wallpaper target size changed");
        let mut image = image::RgbaImage::new(self.size.width, self.size.height);
        // SAFETY: the target belongs to this current context and the destination
        // has exactly width * height * 4 bytes. Readback is requested on demand.
        let error = unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(target.fbo));
            self.gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            self.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            self.gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 0);
            self.gl.pixel_store_i32(glow::PACK_SKIP_PIXELS, 0);
            self.gl.pixel_store_i32(glow::PACK_SKIP_ROWS, 0);
            self.gl.read_pixels(
                0,
                0,
                self.size.width as i32,
                self.size.height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(image.as_mut())),
            );
            self.gl.get_error()
        };
        self.reset();
        anyhow::ensure!(
            error == glow::NO_ERROR,
            "wallpaper readback failed: GL {error:#x}"
        );
        image::imageops::flip_vertical_in_place(&mut image);
        // The wallpaper surface is opaque, including contain's black margins.
        for pixel in image.pixels_mut() {
            pixel.0[3] = 255;
        }
        Ok(image)
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // Fields normally drop after this method; delete programs before unbinding EGL.
        self.compose.delete();
        self.copy.delete();
        self.image.delete();
        let _ = self.api.make_current(self.display, None, None, None);
        if let Some(window) = self.window.take() {
            let _ = self.api.destroy_surface(self.display, window.surface);
        }
        if let Some(surface) = self.pbuffer.take() {
            let _ = self.api.destroy_surface(self.display, surface);
        }
        let _ = self.api.destroy_context(self.display, self.context);
        let _ = self.api.terminate(self.display);
    }
}

pub struct Program<const N: usize> {
    gl: Rc<glow::Context>,
    handle: Option<glow::Program>,
    uniforms: [Option<glow::UniformLocation>; N],
}

impl<const N: usize> Program<N> {
    pub fn new(gl: Rc<glow::Context>, fragment: &str, names: [&str; N]) -> Result<Self> {
        // SAFETY: the context is current; every error path destroys all objects created here.
        unsafe {
            let program = gl.create_program().map_err(anyhow::Error::msg)?;
            for (kind, source) in [
                (glow::VERTEX_SHADER, VERTEX),
                (glow::FRAGMENT_SHADER, fragment),
            ] {
                let shader = match gl.create_shader(kind) {
                    Ok(v) => v,
                    Err(e) => {
                        gl.delete_program(program);
                        bail!(e);
                    }
                };
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    let log = gl.get_shader_info_log(shader);
                    gl.delete_shader(shader);
                    gl.delete_program(program);
                    bail!("shader compile: {log}");
                }
                gl.attach_shader(program, shader);
                gl.delete_shader(shader);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                bail!("shader link: {log}");
            }
            Ok(Self {
                uniforms: names.map(|name| gl.get_uniform_location(program, name)),
                gl,
                handle: Some(program),
            })
        }
    }

    pub fn bind(&self) {
        // SAFETY: the program was successfully linked in the current context.
        unsafe {
            self.gl.use_program(self.handle);
        }
    }

    pub fn uniform(&self, index: usize) -> Option<&glow::UniformLocation> {
        self.uniforms[index].as_ref()
    }

    fn delete(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: owners drop programs before destroying the current context.
            unsafe {
                self.gl.delete_program(handle);
            }
        }
    }
}

impl<const N: usize> Drop for Program<N> {
    fn drop(&mut self) {
        self.delete();
    }
}

pub struct Target {
    gl: Rc<glow::Context>,
    pub texture: glow::Texture,
    pub fbo: glow::Framebuffer,
    pub size: Size,
}

impl Target {
    pub fn new(gl: Rc<glow::Context>, size: Size) -> Result<Self> {
        // SAFETY: allocation sizes have been bounded by Size; objects use the current context.
        unsafe {
            let texture = gl.create_texture().map_err(anyhow::Error::msg)?;
            let fbo = match gl.create_framebuffer() {
                Ok(v) => v,
                Err(e) => {
                    gl.delete_texture(texture);
                    bail!(e);
                }
            };
            let target = Self {
                gl,
                texture,
                fbo,
                size,
            };
            target.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            target.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            target.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            target.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            target.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            target.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                size.width as i32,
                size.height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            target.bind();
            target.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            let complete =
                target.gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE;
            target.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            target.gl.bind_texture(glow::TEXTURE_2D, None);
            anyhow::ensure!(
                complete,
                "incomplete content framebuffer (GPU allocation failed)"
            );
            Ok(target)
        }
    }

    pub fn upload(&self, pixels: &[u8]) {
        self.upload_rect(pixels, [0, 0, self.size.width, self.size.height]);
    }

    /// Upload a rectangle from a full-size RGBA/BGRA image without packing its rows.
    pub fn upload_rect(&self, pixels: &[u8], [x, y, width, height]: [u32; 4]) {
        assert_eq!(pixels.len(), self.size.bytes());
        assert!(width > 0 && height > 0);
        assert!(
            x.checked_add(width)
                .is_some_and(|end| end <= self.size.width)
        );
        assert!(
            y.checked_add(height)
                .is_some_and(|end| end <= self.size.height)
        );
        let offset = (y as usize * self.size.width as usize + x as usize) * 4;
        // SAFETY: the validated slice exactly covers the owned RGBA8 texture.
        unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            self.gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            self.gl
                .pixel_store_i32(glow::UNPACK_ROW_LENGTH, self.size.width as i32);
            self.gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 0);
            self.gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 0);
            self.gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                x as i32,
                y as i32,
                width as i32,
                height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&pixels[offset..])),
            );
            self.gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
        }
    }

    pub fn bind(&self) {
        // SAFETY: this target belongs to the current context.
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            self.gl
                .viewport(0, 0, self.size.width as i32, self.size.height as i32);
        }
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        // SAFETY: content targets are destroyed before the worker's EGL context.
        unsafe {
            self.gl.delete_framebuffer(self.fbo);
            self.gl.delete_texture(self.texture);
        }
    }
}

fn image_factor(source: Size, target: Size, fit: Fit) -> (f32, f32) {
    let [x, y] = fit.texture_scale(
        [source.width as f32, source.height as f32],
        [target.width as f32, target.height as f32],
    );
    (x, y)
}

#[cfg(feature = "web")]
mod dmabuf {
    //! Import CEF's borrowed GPU frame and finish copying before its callback returns.
    use super::*;
    use crate::web::frames::{ABGR8888, ARGB8888, Frame};
    use std::os::fd::AsRawFd;

    impl Gpu {
        pub fn dma_buf_supported(&self) -> bool {
            !self.software
                && self
                    .api
                    .query_string(Some(self.display), egl::EXTENSIONS)
                    .is_ok_and(|extensions| {
                        extensions
                            .to_bytes()
                            .split(|byte| *byte == b' ')
                            .any(|extension| extension == b"EGL_EXT_image_dma_buf_import")
                    })
                && self.gl.supported_extensions().contains("GL_OES_EGL_image")
                && self
                    .api
                    .get_proc_address("glEGLImageTargetTexture2DOES")
                    .is_some()
        }

        pub fn copy_dma_buf(&self, target: &Target, frame: &Frame, damage: [u32; 4]) -> Result<()> {
            let info = &frame.info;
            let size = Size::new(info.size[0], info.size[1])?;
            let [x, y, width, height] = info.visible;
            anyhow::ensure!(
                info.sequence > 0
                    && width == target.size.width
                    && height == target.size.height
                    && x.checked_add(width).is_some_and(|end| end <= size.width)
                    && y.checked_add(height).is_some_and(|end| end <= size.height),
                "invalid DMA-BUF geometry"
            );
            let [dx, dy, dw, dh] = damage;
            anyhow::ensure!(
                dw > 0
                    && dh > 0
                    && dx.checked_add(dw).is_some_and(|end| end <= width)
                    && dy.checked_add(dh).is_some_and(|end| end <= height),
                "invalid DMA-BUF damage"
            );
            anyhow::ensure!(
                matches!(info.format, ARGB8888 | ABGR8888),
                "unsupported DMA-BUF format"
            );
            anyhow::ensure!(
                !frame.fds.is_empty()
                    && frame.fds.len() <= 4
                    && frame.fds.len() == info.planes.len(),
                "invalid DMA-BUF planes"
            );
            let mut attributes = vec![
                egl::WIDTH as egl::Attrib,
                size.width as egl::Attrib,
                egl::HEIGHT as egl::Attrib,
                size.height as egl::Attrib,
                0x3271,
                info.format as egl::Attrib,
            ];
            let modifiers = info.modifier != 0x00ff_ffff_ffff_ffff;
            if modifiers {
                anyhow::ensure!(
                    self.api
                        .query_string(Some(self.display), egl::EXTENSIONS)?
                        .to_bytes()
                        .split(|byte| *byte == b' ')
                        .any(|extension| extension == b"EGL_EXT_image_dma_buf_import_modifiers"),
                    "EGL does not support DMA-BUF modifiers"
                );
            }
            for (index, (plane, fd)) in info.planes.iter().zip(&frame.fds).enumerate() {
                anyhow::ensure!(plane.stride > 0, "empty DMA-BUF stride");
                let offset = i32::try_from(plane.offset)
                    .context("DMA-BUF plane offset exceeds EGL limits")?;
                let stride = i32::try_from(plane.stride)
                    .context("DMA-BUF plane stride exceeds EGL limits")?;
                let base = if index == 3 {
                    0x3440
                } else {
                    0x3272 + index as egl::Attrib * 3
                };
                attributes.extend([
                    base,
                    fd.as_raw_fd() as egl::Attrib,
                    base + 1,
                    offset as egl::Attrib,
                    base + 2,
                    stride as egl::Attrib,
                ]);
                if modifiers {
                    let base = 0x3443 + index as egl::Attrib * 2;
                    attributes.extend([
                        base,
                        info.modifier as u32 as i32 as egl::Attrib,
                        base + 1,
                        (info.modifier >> 32) as u32 as i32 as egl::Attrib,
                    ]);
                }
            }
            attributes.push(egl::ATTRIB_NONE);
            // SAFETY: DMA-BUF import requires EGL_NO_CONTEXT and a null client buffer.
            // The received descriptors remain owned until after the copy has completed.
            let image = unsafe {
                self.api.create_image(
                    self.display,
                    egl::Context::from_ptr(std::ptr::null_mut()),
                    0x3270,
                    egl::ClientBuffer::from_ptr(std::ptr::null_mut()),
                    &attributes,
                )
            }
            .context("importing browser DMA-BUF")?;
            let result = self.copy_dma_buf_image(target, image, [x + dx, y + dy, dw, dh], damage);
            let destroyed = self
                .api
                .destroy_image(self.display, image)
                .context("releasing browser EGLImage");
            result.and(destroyed)
        }

        fn copy_dma_buf_image(
            &self,
            target: &Target,
            image: egl::Image,
            [x, y, width, height]: [u32; 4],
            [dx, dy, dw, dh]: [u32; 4],
        ) -> Result<()> {
            let bind_image = self
                .api
                .get_proc_address("glEGLImageTargetTexture2DOES")
                .context("missing GLES EGLImage import")?;
            // SAFETY: EGL resolves this extension with the documented GLES signature.
            let bind_image: unsafe extern "system" fn(u32, *mut c_void) =
                unsafe { std::mem::transmute(bind_image) };
            // SAFETY: all GL objects below belong to this current worker context.
            unsafe {
                let texture = self.gl.create_texture().map_err(anyhow::Error::msg)?;
                let fbo = match self.gl.create_framebuffer() {
                    Ok(fbo) => fbo,
                    Err(error) => {
                        self.gl.delete_texture(texture);
                        return Err(anyhow::Error::msg(error));
                    }
                };
                let scissor = self.gl.is_enabled(glow::SCISSOR_TEST);
                let result = (|| -> Result<()> {
                    self.gl.active_texture(glow::TEXTURE0);
                    self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                    bind_image(glow::TEXTURE_2D, image.as_ptr());
                    self.gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
                    self.gl.framebuffer_texture_2d(
                        glow::READ_FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        glow::TEXTURE_2D,
                        Some(texture),
                        0,
                    );
                    anyhow::ensure!(
                        self.gl.check_framebuffer_status(glow::READ_FRAMEBUFFER)
                            == glow::FRAMEBUFFER_COMPLETE,
                        "browser DMA-BUF is not a readable framebuffer"
                    );
                    self.gl.disable(glow::SCISSOR_TEST);
                    self.gl
                        .bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(target.fbo));
                    self.gl.blit_framebuffer(
                        x as i32,
                        y as i32,
                        (x + width) as i32,
                        (y + height) as i32,
                        dx as i32,
                        dy as i32,
                        (dx + dw) as i32,
                        (dy + dh) as i32,
                        glow::COLOR_BUFFER_BIT,
                        glow::NEAREST,
                    );
                    let error = self.gl.get_error();
                    anyhow::ensure!(
                        error == glow::NO_ERROR,
                        "copying browser DMA-BUF: GL error {error:#x}"
                    );
                    Ok(())
                })();
                // ponytail: synchronously finish one GPU copy per frame. CEF provides
                // no release-fence API, so its pool must not reuse a still-read buffer.
                self.gl.finish();
                if scissor {
                    self.gl.enable(glow::SCISSOR_TEST);
                }
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                self.gl.bind_texture(glow::TEXTURE_2D, None);
                self.gl.delete_framebuffer(fbo);
                self.gl.delete_texture(texture);
                result
            }
        }
    }
}
