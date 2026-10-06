use crate::shader::MaterialDomain;
use crate::{
    assets::{AssetKey, Assets, Texture as Pixels},
    scene::bindings::{Properties, resolve},
    shader::Sources,
    shader::Uniform,
};
use anyhow::{Context, Result, ensure};
use glam::Mat4;
use glow::HasContext;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
};

pub(crate) const GPU_BUDGET: u64 = 1024 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct Usage {
    pub textures: HashSet<glow::Texture>,
    buffers: HashSet<glow::Framebuffer>,
    meshes: HashSet<glow::VertexArray>,
    pub bytes: u64,
}
impl Usage {
    pub fn texture(&mut self, texture: &Texture) {
        if self.textures.insert(texture.handle) {
            self.bytes += texture.byte_size();
        }
    }
    pub fn buffer(&mut self, buffer: &Buffer) {
        self.texture(&buffer.texture);
        if self.buffers.insert(buffer.handle) && buffer.depth.is_some() {
            self.bytes += buffer.texture.size[0] as u64 * buffer.texture.size[1] as u64 * 4;
        }
    }
    pub fn pass(&mut self, pass: &Pass) {
        for texture in pass.textures.iter().flatten() {
            self.texture(texture);
        }
    }
    pub fn mesh(&mut self, mesh: &Mesh) {
        if self.meshes.insert(mesh.vao) {
            self.bytes += mesh.byte_size();
        }
    }
}

pub(crate) struct Texture {
    gl: Rc<glow::Context>,
    pub handle: glow::Texture,
    pub size: [u32; 2],
    pub content: [u32; 2],
    pub format: u32,
    pub frames: Vec<crate::assets::SpriteFrame>,
    pub video: std::cell::RefCell<Option<Rc<[u8]>>>,
    ends: Vec<f32>,
    bytes: u64,
}
impl Texture {
    fn supports_compression(gl: &glow::Context, kind: u32) -> bool {
        let extensions = gl.supported_extensions();
        extensions.contains("GL_EXT_texture_compression_s3tc")
            || (kind == 7 && extensions.contains("GL_EXT_texture_compression_dxt1"))
            || match kind {
                4 => extensions.contains("GL_ANGLE_texture_compression_dxt5"),
                6 => extensions.contains("GL_ANGLE_texture_compression_dxt3"),
                _ => false,
            }
    }
    pub fn byte_size(&self) -> u64 {
        self.bytes
    }
    pub fn upload(&self, pixels: &Pixels) -> Result<()> {
        ensure!(
            self.format == glow::RGBA8
                && self.size == [pixels.width, pixels.height]
                && pixels.rgba.len() == pixels.width as usize * pixels.height as usize * 4,
            "invalid dynamic texture upload"
        );
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.handle));
            self.gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                self.size[0] as i32,
                self.size[1] as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&pixels.rgba)),
            );
            ensure!(
                self.gl.get_error() == glow::NO_ERROR,
                "uploading dynamic scene texture failed"
            );
        }
        Ok(())
    }
    pub fn new(
        gl: Rc<glow::Context>,
        pixels: Option<Pixels>,
        size: [u32; 2],
        budget: &mut u64,
    ) -> Result<Rc<Self>> {
        Self::allocate(gl, pixels, size, glow::RGBA8, budget)
    }
    fn allocate(
        gl: Rc<glow::Context>,
        mut pixels: Option<Pixels>,
        size: [u32; 2],
        format: u32,
        budget: &mut u64,
    ) -> Result<Rc<Self>> {
        let compressed = pixels
            .as_ref()
            .and_then(|p| p.compressed.as_ref())
            .filter(|(kind, _)| Self::supports_compression(&gl, *kind));
        let format = if let Some((kind, _)) = compressed {
            match kind {
                4 => glow::COMPRESSED_RGBA_S3TC_DXT5_EXT,
                6 => glow::COMPRESSED_RGBA_S3TC_DXT3_EXT,
                _ => glow::COMPRESSED_RGBA_S3TC_DXT1_EXT,
            }
        } else {
            format
        };
        let channels = match format {
            glow::R8 => 1,
            glow::RG8 | glow::R16F => 2,
            glow::RG16F | glow::R32F => 4,
            glow::RGBA16F | glow::RG32F => 8,
            glow::RGBA32F => 16,
            _ => 4,
        };
        ensure!(
            size.iter().all(|v| *v > 0 && *v <= 32768)
                && size[0] as u64 * size[1] as u64 * 4 <= 512 * 1024 * 1024,
            "GPU texture exceeds limit"
        );
        let mut bytes = compressed
            .map_or(size[0] as u64 * size[1] as u64 * channels, |(_, bytes)| {
                bytes.len() as u64
            });
        let use_compressed = compressed.is_some();
        if let Some(pixels) = &pixels {
            for mip in &pixels.mipmaps {
                bytes += if use_compressed {
                    mip.compressed
                        .as_ref()
                        .context("compressed texture mip has no block data")?
                        .len() as u64
                } else {
                    mip.size[0] as u64 * mip.size[1] as u64 * channels
                };
            }
        }
        let compressed_data = if compressed.is_some() {
            pixels
                .as_mut()
                .unwrap()
                .compressed
                .take()
                .map(|(_, bytes)| bytes)
        } else {
            None
        };
        let mipmaps = pixels
            .as_mut()
            .map_or_else(Vec::new, |p| std::mem::take(&mut p.mipmaps));
        ensure!(
            *budget + bytes <= GPU_BUDGET,
            "scene GPU textures exceed 1 GiB: used={}, new={} for {}x{} format={format:#x}",
            *budget,
            bytes,
            size[0],
            size[1]
        );
        *budget += bytes;
        // The caller keeps this context current throughout allocation and destruction.
        unsafe {
            let handle = gl.create_texture().map_err(anyhow::Error::msg)?;
            let content = pixels.as_ref().map_or(size, |p| p.content);
            let frames = pixels.as_ref().map_or_else(Vec::new, |p| p.frames.clone());
            let flags = pixels.as_ref().map_or(2, |p| p.flags);
            let video = pixels.as_mut().and_then(|p| p.video.take()).map(Rc::from);
            let mut total = 0.0;
            let mut sequence = 0;
            let ends = frames
                .iter()
                .map(|f| {
                    if f.sequence != sequence {
                        total = 0.;
                        sequence = f.sequence;
                    }
                    total += if f.duration > 0. {
                        f.duration
                    } else {
                        1.0 / 60.0
                    };
                    total
                })
                .collect();
            let texture = Self {
                gl,
                handle,
                size,
                content,
                format,
                frames,
                video: std::cell::RefCell::new(video),
                ends,
                bytes,
            };
            texture.gl.bind_texture(glow::TEXTURE_2D, Some(handle));
            texture.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAX_LEVEL,
                mipmaps.len() as i32,
            );
            texture.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                match (flags & 1 != 0, mipmaps.is_empty()) {
                    (true, true) => glow::NEAREST,
                    (false, true) => glow::LINEAR,
                    (true, false) => glow::NEAREST_MIPMAP_NEAREST,
                    (false, false) => glow::LINEAR_MIPMAP_LINEAR,
                } as i32,
            );
            texture.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                if flags & 1 != 0 {
                    glow::NEAREST
                } else {
                    glow::LINEAR
                } as i32,
            );
            for axis in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
                texture.gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    axis,
                    if flags & 2 != 0 {
                        glow::CLAMP_TO_EDGE
                    } else {
                        glow::REPEAT
                    } as i32,
                );
            }
            if let Some(data) = compressed_data {
                texture.gl.compressed_tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    format as i32,
                    size[0] as i32,
                    size[1] as i32,
                    0,
                    data.len() as i32,
                    &data,
                );
            } else {
                texture.gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    format as i32,
                    size[0] as i32,
                    size[1] as i32,
                    0,
                    match format {
                        glow::R8 | glow::R16F | glow::R32F => glow::RED,
                        glow::RG8 | glow::RG16F | glow::RG32F => glow::RG,
                        _ => glow::RGBA,
                    },
                    match format {
                        glow::R16F | glow::RG16F | glow::RGBA16F => glow::HALF_FLOAT,
                        glow::R32F | glow::RG32F | glow::RGBA32F => glow::FLOAT,
                        _ => glow::UNSIGNED_BYTE,
                    },
                    glow::PixelUnpackData::Slice(
                        pixels
                            .as_ref()
                            .filter(|p| !p.rgba.is_empty())
                            .map(|p| p.rgba.as_slice()),
                    ),
                );
            }
            for (index, mip) in mipmaps.into_iter().enumerate() {
                if use_compressed {
                    let data = mip.compressed.unwrap();
                    texture.gl.compressed_tex_image_2d(
                        glow::TEXTURE_2D,
                        index as i32 + 1,
                        format as i32,
                        mip.size[0] as i32,
                        mip.size[1] as i32,
                        0,
                        data.len() as i32,
                        &data,
                    );
                } else {
                    texture.gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        index as i32 + 1,
                        format as i32,
                        mip.size[0] as i32,
                        mip.size[1] as i32,
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(Some(&mip.rgba)),
                    );
                }
            }
            ensure!(
                texture.gl.get_error() == glow::NO_ERROR,
                "allocating scene texture failed"
            );
            Ok(Rc::new(texture))
        }
    }
}
impl Texture {
    pub fn sequence_frames(&self, sequence: usize) -> &[crate::assets::SpriteFrame] {
        &self.frames[crate::assets::sequence_range(&self.frames, sequence)]
    }
    pub fn frame(&self, time: f32) -> Option<(usize, &crate::assets::SpriteFrame)> {
        let range = crate::assets::sequence_range(&self.frames, 0);
        let ends = &self.ends[range.clone()];
        let total = *ends.last()?;
        let phase = time.rem_euclid(total);
        let index = range.start
            + ends
                .partition_point(|end| *end <= phase)
                .min(ends.len() - 1);
        Some((index, &self.frames[index]))
    }
}
impl Drop for Texture {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_texture(self.handle);
        }
    }
}
pub(crate) struct Buffer {
    gl: Rc<glow::Context>,
    pub texture: Rc<Texture>,
    pub handle: glow::Framebuffer,
    depth: Option<glow::Renderbuffer>,
}
impl Buffer {
    pub fn has_depth(&self) -> bool {
        self.depth.is_some()
    }
    pub fn from_texture(gl: Rc<glow::Context>, texture: Rc<Texture>) -> Result<Self> {
        unsafe {
            let buffer = Self {
                handle: gl.create_framebuffer().map_err(anyhow::Error::msg)?,
                gl,
                texture,
                depth: None,
            };
            buffer
                .gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(buffer.handle));
            buffer.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(buffer.texture.handle),
                0,
            );
            ensure!(
                buffer.gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE,
                "media framebuffer incomplete"
            );
            Ok(buffer)
        }
    }
    pub fn formatted(
        gl: Rc<glow::Context>,
        size: [u32; 2],
        format: u32,
        budget: &mut u64,
    ) -> Result<Self> {
        let texture = Texture::allocate(gl.clone(), None, size, format, budget)?;
        unsafe {
            let buffer = Self {
                handle: gl.create_framebuffer().map_err(anyhow::Error::msg)?,
                gl,
                texture,
                depth: None,
            };
            buffer
                .gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(buffer.handle));
            buffer.gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(buffer.texture.handle),
                0,
            );
            ensure!(
                buffer.gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE,
                "scene framebuffer incomplete"
            );
            buffer.gl.disable(glow::SCISSOR_TEST);
            buffer.gl.color_mask(true, true, true, true);
            buffer.gl.clear_color(0., 0., 0., 0.);
            buffer.gl.clear(glow::COLOR_BUFFER_BIT);
            Ok(buffer)
        }
    }
    pub fn formatted_with_depth(
        gl: Rc<glow::Context>,
        size: [u32; 2],
        format: u32,
        budget: &mut u64,
    ) -> Result<Self> {
        let mut buffer = Self::formatted(gl, size, format, budget)?;
        let bytes = size[0] as u64 * size[1] as u64 * 4;
        ensure!(
            *budget + bytes <= GPU_BUDGET,
            "scene depth buffer exceeds 1 GiB"
        );
        unsafe {
            let depth = buffer
                .gl
                .create_renderbuffer()
                .map_err(anyhow::Error::msg)?;
            buffer.depth = Some(depth);
            buffer.gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
            buffer.gl.renderbuffer_storage(
                glow::RENDERBUFFER,
                glow::DEPTH_COMPONENT24,
                size[0] as i32,
                size[1] as i32,
            );
            buffer
                .gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(buffer.handle));
            buffer.gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::DEPTH_ATTACHMENT,
                glow::RENDERBUFFER,
                Some(depth),
            );
            ensure!(
                buffer.gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE
                    && buffer.gl.get_error() == glow::NO_ERROR,
                "model depth framebuffer incomplete"
            );
        }
        *budget += bytes;
        Ok(buffer)
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_framebuffer(self.handle);
            if let Some(depth) = self.depth {
                self.gl.delete_renderbuffer(depth);
            }
        }
    }
}

pub(crate) struct Pass {
    gl: Rc<glow::Context>,
    handle: glow::Program,
    pub textures: Vec<Option<Rc<Texture>>>,
    position: Option<u32>,
    uv: Option<u32>,
    attributes: HashMap<&'static str, u32>,
    time: Option<glow::UniformLocation>,
    color: Option<glow::UniformLocation>,
    domain: MaterialDomain,
    samplers: Vec<Option<glow::UniformLocation>>,
    resolutions: Vec<Option<glow::UniformLocation>>,
    matrix: Option<glow::UniformLocation>,
    builtins: Vec<(&'static str, glow::UniformLocation)>,
    audio: Vec<(glow::UniformLocation, usize, usize, usize, usize)>,
    lights: Vec<(glow::UniformLocation, u8, usize)>,
    shadows: crate::lighting::shadow::Uniforms,
    uniforms: Vec<(Uniform, glow::UniformLocation, bool)>,
    uniforms_dirty: Cell<bool>,
    values: Vec<Vec<f32>>,
    base_values: Vec<Vec<f32>>,
    overrides: serde_json::Value,
    pub references: Vec<Option<String>>,
    pub structure: serde_json::Value,
    pub alpha_coverage: bool,
    spec: serde_json::Value,
}

#[derive(Clone, Copy)]
pub(crate) struct Appearance {
    pub tint: glam::Vec3,
    pub brightness: f32,
    pub alpha: f32,
}
impl From<&crate::scene::State> for Appearance {
    fn from(state: &crate::scene::State) -> Self {
        Self {
            tint: state.tint,
            brightness: state.brightness,
            alpha: state.color.w,
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) struct Frame<'a> {
    pub audio: &'a crate::audio::AudioSnapshot,
    pub time: f32,
    pub delta: f32,
    pub pointer: [f32; 2],
    pub last_pointer: [f32; 2],
    pub down: bool,
    pub parallax: [f32; 2],
    pub projection: Mat4,
    pub screen: [u32; 2],
    pub appearance: Option<Appearance>,
    pub composite: bool,
    pub lights: &'a crate::lighting::Snapshot,
    pub ambient: glam::Vec3,
    pub skylight: glam::Vec3,
}
pub(crate) fn structure(
    spec: &serde_json::Value,
    properties: &Properties,
) -> Result<serde_json::Value> {
    let mut value = resolve(spec, properties)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("constantshadervalues");
    }
    Ok(value)
}
impl Pass {
    #[allow(clippy::too_many_arguments)]
    pub fn load(
        gl: Rc<glow::Context>,
        assets: &Assets,
        spec: &serde_json::Value,
        properties: &Properties,
        named: &HashSet<String>,
        cache: &mut HashMap<AssetKey, Rc<Texture>>,
        budget: &mut u64,
        domain: MaterialDomain,
    ) -> Result<Self> {
        let mut resolved = resolve(spec, properties)?;
        // Preserve authored uniform bindings for subsequent transactional updates.
        resolved["constantshadervalues"] = spec["constantshadervalues"].clone();
        ensure!(
            resolved["textures"].as_array().is_none_or(|v| v.len() <= 8),
            "more than eight texture slots are unsupported"
        );
        let sources = Sources::load(assets, &resolved, named, domain)?;
        let mut pass =
            Self::compile_with_uniforms(gl, &sources.vertex, &sources.fragment, &sources.uniforms)
                .with_context(|| format!("material shader {}", resolved["shader"]))?;
        pass.structure = structure(spec, properties)?;
        pass.alpha_coverage = sources.alpha_coverage;
        pass.spec = spec.clone();
        pass.domain = domain;
        for index in 0..8 {
            if index != 0 && pass.samplers[index].is_none() {
                pass.references.push(None);
                pass.textures.push(None);
                continue;
            }
            let name = resolved["textures"][index]
                .as_str()
                .or_else(|| sources.defaults.get(&index).map(String::as_str));
            let system = resolved["usertextures"][index]
                .get("type")
                .and_then(serde_json::Value::as_str)
                == Some("system");
            let system_name = resolved["usertextures"][index]["name"]
                .as_str()
                .unwrap_or("");
            if system {
                ensure!(
                    ["$mediaThumbnail", "$mediaPreviousThumbnail"].contains(&system_name),
                    "unknown system texture {system_name}"
                );
            }
            let system_reference = system.then(|| format!("_system{system_name}"));
            let reference = name.filter(|name| {
                named.contains(*name)
                    || name.starts_with("_rt_")
                    || name.starts_with("_alias_")
                    || *name == "previous"
            });
            pass.references
                .push(system_reference.or_else(|| reference.map(str::to_owned)));
            if reference.is_some() && !system {
                pass.textures.push(None);
                continue;
            }
            let texture = if let Some(name) = name {
                let key = assets.texture_key(name)?;
                if let Some(texture) = cache.get(&key) {
                    Some(texture.clone())
                } else {
                    let pixels = assets
                        .texture(name, |kind| Texture::supports_compression(&pass.gl, kind))?;
                    let size = [pixels.width, pixels.height];
                    let texture = Texture::new(pass.gl.clone(), Some(pixels), size, budget)?;
                    cache.insert(key, texture.clone());
                    Some(texture)
                }
            } else {
                ensure!(
                    index == 0 || system,
                    "active texture slot {index} has no asset"
                );
                if system {
                    Some(Texture::new(
                        pass.gl.clone(),
                        Some(Pixels {
                            compressed: None,
                            mipmaps: Vec::new(),
                            flags: 2,
                            video: None,
                            width: 1,
                            height: 1,
                            content: [1, 1],
                            frames: Vec::new(),
                            rgba: vec![0; 4],
                        }),
                        [1, 1],
                        budget,
                    )?)
                } else {
                    None
                }
            };
            pass.textures.push(texture);
        }
        for uniform in sources.uniforms {
            if let Some(location) =
                unsafe { pass.gl.get_uniform_location(pass.handle, &uniform.name) }
            {
                let dynamic = pass.time == Some(location)
                    || pass.color == Some(location)
                    || pass.matrix == Some(location)
                    || pass.builtins.iter().any(|(_, value)| *value == location)
                    || pass.audio.iter().any(|(value, ..)| *value == location)
                    || pass.lights.iter().any(|(value, ..)| *value == location)
                    || pass.samplers.contains(&Some(location))
                    || pass.resolutions.contains(&Some(location))
                    || matches!(
                        uniform.name.as_str(),
                        "g_TintColor"
                            | "g_TintAlpha"
                            | "wallpaperShadowDepth"
                            | "wallpaperShadowMatrices"
                            | "wallpaperShadowReceive"
                    );
                pass.uniforms.push((uniform, location, dynamic));
            }
        }
        pass.apply(pass.prepare(properties)?);
        Ok(pass)
    }
    pub fn prepare(&self, properties: &Properties) -> Result<Vec<Vec<f32>>> {
        self.uniforms
            .iter()
            .map(|(uniform, ..)| uniform.components(&resolve(&uniform.value, properties)?))
            .collect()
    }
    pub fn changed(&self, properties: &Properties) -> Result<bool> {
        if self.spec.is_null() {
            return Ok(false);
        }
        Ok(structure(&self.spec, properties)? != self.structure)
    }
    pub fn apply(&mut self, values: Vec<Vec<f32>>) {
        self.base_values = values.clone();
        self.overrides = serde_json::Value::Null;
        self.values = values;
        self.uniforms_dirty.set(true);
    }
    pub fn value_count(&self) -> usize {
        self.values.len()
    }
    pub fn apply_constants(&mut self, overrides: &serde_json::Value) -> Result<()> {
        if self.overrides == *overrides {
            return Ok(());
        }
        let mut values = self.base_values.clone();
        for ((uniform, ..), value) in self.uniforms.iter().zip(&mut values) {
            if let Some(key) = &uniform.binding
                && let Some(input) = overrides.get(key)
            {
                *value = uniform.components(input)?;
            }
        }
        self.values = values;
        self.overrides = overrides.clone();
        self.uniforms_dirty.set(true);
        Ok(())
    }
    pub fn compile(gl: Rc<glow::Context>, vertex: &str, fragment: &str) -> Result<Self> {
        Self::compile_with_uniforms(gl, vertex, fragment, &[])
    }
    fn compile_with_uniforms(
        gl: Rc<glow::Context>,
        vertex: &str,
        fragment: &str,
        authored: &[Uniform],
    ) -> Result<Self> {
        unsafe {
            let mut pass = Self {
                handle: gl.create_program().map_err(anyhow::Error::msg)?,
                gl,
                textures: Vec::new(),
                position: None,
                uv: None,
                attributes: HashMap::new(),
                time: None,
                color: None,
                domain: MaterialDomain::Effect,
                samplers: Vec::new(),
                resolutions: Vec::new(),
                matrix: None,
                builtins: Vec::new(),
                audio: Vec::new(),
                lights: Vec::new(),
                shadows: Default::default(),
                uniforms: Vec::new(),
                uniforms_dirty: Cell::new(true),
                values: Vec::new(),
                base_values: Vec::new(),
                overrides: serde_json::Value::Null,
                references: Vec::new(),
                structure: serde_json::Value::Null,
                alpha_coverage: false,
                spec: serde_json::Value::Null,
            };
            for (kind, source) in [
                (glow::VERTEX_SHADER, vertex),
                (glow::FRAGMENT_SHADER, fragment),
            ] {
                let shader = pass.gl.create_shader(kind).map_err(anyhow::Error::msg)?;
                pass.gl.shader_source(shader, source);
                pass.gl.compile_shader(shader);
                let compiled = pass.gl.get_shader_compile_status(shader);
                let log = pass.gl.get_shader_info_log(shader);
                if compiled {
                    pass.gl.attach_shader(pass.handle, shader);
                }
                pass.gl.delete_shader(shader);
                ensure!(compiled, "WE shader compile: {log}");
            }
            pass.gl.link_program(pass.handle);
            ensure!(
                pass.gl.get_program_link_status(pass.handle),
                "WE shader link: {}",
                pass.gl.get_program_info_log(pass.handle)
            );
            for index in 0..pass.gl.get_active_uniforms(pass.handle) {
                if let Some(uniform) = pass.gl.get_active_uniform(pass.handle, index) {
                    if matches!(
                        uniform.name.as_str(),
                        "wallpaperShadowDepth"
                            | "wallpaperShadowMatrices"
                            | "wallpaperShadowReceive"
                    ) {
                        ensure!(
                            uniform.size == 1
                                && uniform.utype
                                    == if uniform.name == "wallpaperShadowDepth" {
                                        glow::SAMPLER_2D_ARRAY
                                    } else if uniform.name == "wallpaperShadowMatrices" {
                                        glow::SAMPLER_2D
                                    } else {
                                        glow::INT
                                    },
                            "invalid shadow uniform {}",
                            uniform.name
                        );
                        continue;
                    }
                    let light = match uniform.name.trim_end_matches("[0]") {
                        "g_LightsPosition" => Some((0, glow::FLOAT_VEC3)),
                        "g_LightsColorRadius" => Some((1, glow::FLOAT_VEC4)),
                        "g_LightsColorPremultiplied" => Some((2, glow::FLOAT_VEC3)),
                        "wallpaperLightOrigin" => Some((3, glow::FLOAT_VEC4)),
                        "wallpaperLightColor" => Some((4, glow::FLOAT_VEC4)),
                        "wallpaperLightDirection" => Some((5, glow::FLOAT_VEC4)),
                        "wallpaperLightExtra" => Some((6, glow::FLOAT_VEC4)),
                        "wallpaperLightCount" => Some((7, glow::INT)),
                        _ => None,
                    };
                    if let Some((kind, ty)) = light {
                        ensure!(
                            uniform.utype == ty
                                && (1..=if kind >= 3 {
                                    crate::lighting::MODERN_LIGHTS as i32
                                } else {
                                    crate::lighting::LEGACY_LIGHTS as i32
                                })
                                    .contains(&uniform.size),
                            "invalid light uniform {}",
                            uniform.name
                        );
                        if let Some(location) =
                            pass.gl.get_uniform_location(pass.handle, &uniform.name)
                        {
                            pass.lights.push((location, kind, uniform.size as usize));
                        }
                        continue;
                    }
                    if let Some((channel, count)) = audio_uniform(&uniform.name) {
                        let width = if uniform.utype == glow::FLOAT_VEC4 {
                            4
                        } else {
                            1
                        };
                        ensure!(
                            [glow::FLOAT, glow::FLOAT_VEC4].contains(&uniform.utype)
                                && uniform.size as usize * width <= count,
                            "invalid audio uniform {}",
                            uniform.name
                        );
                        if let Some(location) =
                            pass.gl.get_uniform_location(pass.handle, &uniform.name)
                        {
                            pass.audio.push((
                                location,
                                channel,
                                count,
                                width,
                                uniform.size as usize * width,
                            ));
                        }
                        continue;
                    }
                    ensure!(
                        authored.iter().any(|value| value.name == uniform.name)
                            || [
                                "g_PointerPosition",
                                "g_PointerPositionLast",
                                "g_PointerState"
                            ]
                            .contains(&uniform.name.as_str())
                            || ![
                                "g_Audio",
                                "g_Mouse",
                                "g_Pointer",
                                "g_Cursor",
                                "g_Bones",
                                "g_Lights"
                            ]
                            .iter()
                            .any(|prefix| uniform.name.starts_with(prefix)),
                        "external input {} requires the native backend",
                        uniform.name
                    );
                    if uniform.utype == glow::SAMPLER_2D {
                        ensure!(
                            uniform.size == 1
                                && uniform
                                    .name
                                    .strip_prefix("g_Texture")
                                    .and_then(|slot| slot.parse::<usize>().ok())
                                    .is_some_and(|slot| slot < 8),
                            "unsupported sampler {}",
                            uniform.name
                        );
                    }
                }
            }
            pass.shadows = crate::lighting::shadow::Uniforms::new(&pass.gl, pass.handle);
            pass.position = pass.gl.get_attrib_location(pass.handle, "a_Position");
            pass.uv = pass.gl.get_attrib_location(pass.handle, "a_TexCoord");
            for name in [
                "a_Position",
                "a_TexCoord",
                "a_Normal",
                "a_Tangent4",
                "a_TexCoordVec4",
                "a_TexCoordC1",
                "a_Corner",
                "a_ParticleRotationSize",
                "a_Color",
                "a_TexCoordVec4C1",
                "a_ParticleUVRange",
                "a_ParticleFrame0",
                "a_ParticleRight",
                "a_ParticleUp",
                "a_ParticleForward",
                "a_ParticleEye",
                "a_ParticleFrame1",
                "a_ParticleFrame2",
                "a_ParticleFrameMix",
                "a_RopeEnd",
                "a_RopeEndColor",
                "a_RopePrevious",
                "a_RopeAfter",
                "a_InstanceRow0",
                "a_InstanceRow1",
                "a_InstanceRow2",
            ] {
                if let Some(location) = pass.gl.get_attrib_location(pass.handle, name) {
                    pass.attributes.insert(name, location);
                }
            }
            pass.time = pass.gl.get_uniform_location(pass.handle, "g_Time");
            pass.color = pass.gl.get_uniform_location(pass.handle, "g_Color4");
            for index in 0..8 {
                pass.samplers.push(
                    pass.gl
                        .get_uniform_location(pass.handle, &format!("g_Texture{index}")),
                );
                pass.resolutions.push(
                    pass.gl
                        .get_uniform_location(pass.handle, &format!("g_Texture{index}Resolution")),
                );
            }
            pass.matrix = pass
                .gl
                .get_uniform_location(pass.handle, "g_ModelViewProjectionMatrix");
            for name in [
                "g_Frametime",
                "g_PointerPosition",
                "g_PointerPositionLast",
                "g_PointerState",
                "g_ParallaxPosition",
                "g_EffectTextureProjectionMatrix",
                "g_EffectModelViewProjectionMatrix",
                "g_EffectTextureProjectionMatrixInverse",
                "g_ModelViewProjectionMatrixInverse",
                "g_ScreenSize",
                "g_TexelSize",
                "g_TexelSizeHalf",
                "g_ModelMatrixInverse",
                "g_ModelMatrix",
                "g_NormalModelMatrix",
                "g_ViewProjectionMatrix",
                "g_LightAmbientColor",
                "g_LightSkylightColor",
                "g_OrientationRight",
                "g_OrientationUp",
                "g_OrientationForward",
                "g_ViewRight",
                "g_ViewUp",
                "g_EyePosition",
                "g_NativeReflectionClipPlane",
                "g_RenderVar0",
                "g_Compose",
                "g_RenderVar1",
                "g_Brightness",
                "g_UserAlpha",
                "g_Alpha",
                "g_Color",
                "g_CompositeColor",
            ] {
                if let Some(location) = pass.gl.get_uniform_location(pass.handle, name) {
                    pass.builtins.push((name, location));
                }
            }
            Ok(pass)
        }
    }
    pub fn animated(&self) -> bool {
        self.animated_with_source(true)
    }
    pub fn animated_with_source(&self, source_playing: bool) -> bool {
        self.textures.iter().enumerate().any(|(i, t)| {
            (i != 0 || source_playing) && t.as_ref().is_some_and(|t| t.frames.len() > 1)
        }) || self.requires_audio()
            || self.time.is_some()
            || self.builtin("g_Frametime").is_some()
    }
    pub fn requires_audio(&self) -> bool {
        !self.audio.is_empty()
    }
    pub fn accepts_pointer(&self) -> bool {
        [
            "g_PointerPosition",
            "g_PointerPositionLast",
            "g_PointerState",
            "g_ParallaxPosition",
        ]
        .iter()
        .any(|name| self.builtin(name).is_some())
    }
    pub fn requires_projection(&self) -> bool {
        [
            "g_EffectTextureProjectionMatrix",
            "g_EffectModelViewProjectionMatrix",
            "g_EffectTextureProjectionMatrixInverse",
        ]
        .iter()
        .any(|name| self.builtin(name).is_some())
    }
    fn builtin(&self, name: &str) -> Option<&glow::UniformLocation> {
        self.builtins
            .iter()
            .find_map(|(key, location)| (*key == name).then_some(location))
    }

    pub fn draw(
        &self,
        mesh: &Mesh,
        matrix: Mat4,
        frame: &Frame<'_>,
        inputs: &[Option<&Texture>; 8],
        color: glam::Vec4,
    ) {
        unsafe {
            self.gl.use_program(Some(self.handle));
            self.shadows
                .bind(&self.gl, frame.lights, self.domain == MaterialDomain::Model);
            self.gl.uniform_matrix_4_f32_slice(
                self.matrix.as_ref(),
                false,
                &matrix.to_cols_array(),
            );
            self.gl.uniform_1_f32(self.time.as_ref(), frame.time);
            for (location, kind, count) in &self.lights {
                match kind {
                    0 => self
                        .gl
                        .uniform_3_f32_slice(Some(location), &frame.lights.position[..count * 3]),
                    1 => self.gl.uniform_4_f32_slice(
                        Some(location),
                        &frame.lights.color_radius[..count * 4],
                    ),
                    2 => self.gl.uniform_3_f32_slice(
                        Some(location),
                        &frame.lights.premultiplied[..count * 3],
                    ),
                    3 => self.gl.uniform_4_f32_slice(
                        Some(location),
                        &frame.lights.modern.origin[..count * 4],
                    ),
                    4 => self.gl.uniform_4_f32_slice(
                        Some(location),
                        &frame.lights.modern.color[..count * 4],
                    ),
                    5 => self.gl.uniform_4_f32_slice(
                        Some(location),
                        &frame.lights.modern.direction[..count * 4],
                    ),
                    6 => self.gl.uniform_4_f32_slice(
                        Some(location),
                        &frame.lights.modern.extra[..count * 4],
                    ),
                    _ => self
                        .gl
                        .uniform_1_i32(Some(location), frame.lights.modern.count as i32),
                }
            }
            for (name, color) in [
                ("g_LightAmbientColor", frame.ambient),
                ("g_LightSkylightColor", frame.skylight),
            ] {
                self.gl
                    .uniform_3_f32(self.builtin(name), color.x, color.y, color.z);
            }
            for (location, channel, count, width, length) in &self.audio {
                if let Some(spectrum) = frame.audio.spectrum(*count) {
                    let values = match channel {
                        0 => &spectrum.left,
                        1 => &spectrum.right,
                        _ => &spectrum.average,
                    };
                    if *width == 4 {
                        self.gl
                            .uniform_4_f32_slice(Some(location), &values[..*length]);
                    } else {
                        self.gl
                            .uniform_1_f32_slice(Some(location), &values[..*length]);
                    }
                }
            }
            self.gl
                .uniform_1_f32(self.builtin("g_Frametime"), frame.delta);
            self.gl.uniform_2_f32(
                self.builtin("g_PointerPosition"),
                frame.pointer[0],
                frame.pointer[1],
            );
            self.gl.uniform_2_f32(
                self.builtin("g_PointerPositionLast"),
                frame.last_pointer[0],
                frame.last_pointer[1],
            );
            self.gl.uniform_4_f32(
                self.builtin("g_PointerState"),
                0.0,
                0.0,
                if frame.down { 1.0 } else { 0.0 },
                0.0,
            );
            self.gl.uniform_2_f32(
                self.builtin("g_ParallaxPosition"),
                frame.parallax[0],
                frame.parallax[1],
            );
            let inverse = |matrix: Mat4| {
                if matrix.determinant().abs() >= 1e-12 {
                    matrix.inverse()
                } else {
                    Mat4::IDENTITY
                }
            };
            for (name, matrix, invert) in [
                ("g_EffectTextureProjectionMatrix", frame.projection, false),
                ("g_EffectModelViewProjectionMatrix", frame.projection, false),
                (
                    "g_EffectTextureProjectionMatrixInverse",
                    frame.projection,
                    true,
                ),
                ("g_ModelViewProjectionMatrixInverse", matrix, true),
            ] {
                if let Some(location) = self.builtin(name) {
                    let matrix = if invert { inverse(matrix) } else { matrix };
                    self.gl.uniform_matrix_4_f32_slice(
                        Some(location),
                        false,
                        &matrix.to_cols_array(),
                    );
                }
            }
            let [w, h] = frame.screen.map(|v| v as f32);
            self.gl
                .uniform_3_f32(self.builtin("g_ScreenSize"), w, h, w / h);
            self.gl
                .uniform_2_f32(self.builtin("g_TexelSize"), 1.0 / w, 1.0 / h);
            self.gl
                .uniform_2_f32(self.builtin("g_TexelSizeHalf"), 0.5 / w, 0.5 / h);
            // Uniform storage belongs to this program and survives program switches.
            // Authored constants only need uploading after a committed change;
            // frame inputs and model tint must still be restored on every draw.
            let uniforms_dirty = self.uniforms_dirty.replace(false);
            for ((uniform, location, dynamic), values) in self.uniforms.iter().zip(&self.values) {
                if !uniforms_dirty && !dynamic {
                    continue;
                }
                match (uniform.kind.as_str(), values.as_slice()) {
                    ("int" | "bool", [v]) => self.gl.uniform_1_i32(Some(location), *v as i32),
                    ("float", [v]) => self.gl.uniform_1_f32(
                        Some(location),
                        if self.domain == MaterialDomain::Model && uniform.name == "g_TintAlpha" {
                            *v * color.w
                        } else {
                            *v
                        },
                    ),
                    ("vec2", [x, y]) => self.gl.uniform_2_f32(Some(location), *x, *y),
                    ("vec3", [x, y, z]) => {
                        let tint = if self.domain == MaterialDomain::Model
                            && uniform.name == "g_TintColor"
                        {
                            color.truncate()
                        } else {
                            glam::Vec3::ONE
                        };
                        self.gl.uniform_3_f32(
                            Some(location),
                            *x * tint.x,
                            *y * tint.y,
                            *z * tint.z,
                        );
                    }
                    ("vec4", [x, y, z, w]) => self.gl.uniform_4_f32(Some(location), *x, *y, *z, *w),
                    ("mat3", values) => {
                        self.gl
                            .uniform_matrix_3_f32_slice(Some(location), false, values)
                    }
                    ("mat4", values) => {
                        self.gl
                            .uniform_matrix_4_f32_slice(Some(location), false, values)
                    }
                    _ => unreachable!("uniforms validated before committing settings"),
                }
            }
            let color = if self.domain == MaterialDomain::Image
                && let Some(state) = frame.appearance
            {
                for (name, value) in [
                    ("g_Brightness", state.brightness),
                    ("g_UserAlpha", state.alpha),
                    ("g_Alpha", state.alpha),
                ] {
                    self.gl.uniform_1_f32(self.builtin(name), value);
                }
                for name in ["g_Color", "g_CompositeColor"] {
                    self.gl.uniform_3_f32(
                        self.builtin(name),
                        state.tint.x,
                        state.tint.y,
                        state.tint.z,
                    );
                }
                // Older image shaders apply separate brightness/alpha uniforms;
                // genericimage3/4 carry those factors in g_Color4 instead.
                let brightness = if self.builtin("g_Brightness").is_some() {
                    1.
                } else {
                    state.brightness
                };
                let alpha =
                    if self.builtin("g_UserAlpha").is_some() || self.builtin("g_Alpha").is_some() {
                        1.
                    } else {
                        state.alpha
                    };
                (state.tint * brightness).extend(alpha)
            } else {
                color
            };
            self.gl
                .uniform_4_f32(self.color.as_ref(), color.x, color.y, color.z, color.w);
            for (index, input) in inputs.iter().enumerate() {
                if self.samplers[index].is_none() && self.resolutions[index].is_none() {
                    continue;
                }
                let texture = input.or_else(|| self.textures.get(index).and_then(Option::as_deref));
                self.gl.active_texture(glow::TEXTURE0 + index as u32);
                self.gl
                    .bind_texture(glow::TEXTURE_2D, texture.map(|v| v.handle));
                if uniforms_dirty
                    || self
                        .uniforms
                        .iter()
                        .any(|(_, location, _)| self.samplers[index] == Some(*location))
                {
                    self.gl
                        .uniform_1_i32(self.samplers[index].as_ref(), index as i32);
                }
                let (size, content) = texture.map_or(([1; 2], [1; 2]), |t| (t.size, t.content));
                self.gl.uniform_4_f32(
                    self.resolutions[index].as_ref(),
                    size[0] as f32,
                    size[1] as f32,
                    content[0] as f32,
                    content[1] as f32,
                );
            }
            self.gl.bind_vertex_array(Some(mesh.vao));
            let mut bindings = mesh.bindings.borrow_mut();
            if mesh.instance.is_none() && !mesh.vertex_layout {
                let layout = [self.position, self.uv];
                if !matches!(&*bindings, Some(VertexBindings::Quad(previous)) if *previous == layout)
                {
                    self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(mesh.vbo));
                    for (location, count, offset) in [(self.position, 3, 0), (self.uv, 2, 12)] {
                        if let Some(location) = location {
                            self.gl.enable_vertex_attrib_array(location);
                            self.gl.vertex_attrib_pointer_f32(
                                location,
                                count,
                                glow::FLOAT,
                                false,
                                20,
                                offset,
                            );
                            self.gl.vertex_attrib_divisor(location, 0);
                        }
                    }
                    *bindings = Some(VertexBindings::Quad(layout));
                }
            } else {
                let changed = !matches!(&*bindings, Some(VertexBindings::Mesh(previous)) if *previous == self.attributes);
                for (name, &location) in &self.attributes {
                    if !mesh
                        .attributes
                        .iter()
                        .any(|attribute| attribute.name == *name)
                    {
                        if changed {
                            self.gl.disable_vertex_attrib_array(location);
                        }
                        let value = match *name {
                            "a_Normal" => [0., 0., 1., 0.],
                            "a_Tangent4" => [1., 0., 0., -1.],
                            "a_Color" => [1.; 4],
                            _ => [0., 0., 0., 1.],
                        };
                        // Constant attributes belong to the context, not the VAO.
                        self.gl
                            .vertex_attrib_4_f32(location, value[0], value[1], value[2], value[3]);
                    }
                }
                if changed {
                    for attribute in &mesh.attributes {
                        if let Some(&location) = self.attributes.get(attribute.name) {
                            self.gl.bind_buffer(
                                glow::ARRAY_BUFFER,
                                if attribute.instance {
                                    mesh.instance
                                } else {
                                    Some(mesh.vbo)
                                },
                            );
                            self.gl.enable_vertex_attrib_array(location);
                            self.gl.vertex_attrib_pointer_f32(
                                location,
                                attribute.count,
                                glow::FLOAT,
                                false,
                                attribute.stride,
                                attribute.offset,
                            );
                            self.gl
                                .vertex_attrib_divisor(location, u32::from(attribute.instance));
                        }
                    }
                    *bindings = Some(VertexBindings::Mesh(self.attributes.clone()));
                }
            }
            if mesh.instance.is_some() {
                self.gl
                    .draw_arrays_instanced(glow::TRIANGLE_STRIP, 0, 4, mesh.count);
            } else if mesh.indices.is_some() {
                self.gl
                    .bind_buffer(glow::ELEMENT_ARRAY_BUFFER, mesh.indices);
                self.gl
                    .draw_elements(glow::TRIANGLES, mesh.count, glow::UNSIGNED_INT, 0);
            } else {
                self.gl.draw_arrays(mesh.primitive, 0, mesh.count);
            }
        }
    }
    pub fn vector3(&self, name: &str, value: glam::Vec3) {
        let Some(location) = self.builtin(name) else {
            return;
        };
        unsafe {
            self.gl.use_program(Some(self.handle));
            self.gl
                .uniform_3_f32(Some(location), value.x, value.y, value.z);
        }
    }
    pub fn vector4(&self, name: &str, value: glam::Vec4) {
        let Some(location) = self.builtin(name) else {
            return;
        };
        unsafe {
            self.gl.use_program(Some(self.handle));
            self.gl
                .uniform_4_f32(Some(location), value.x, value.y, value.z, value.w);
        }
    }
    pub fn matrix4(&self, name: &str, value: Mat4) {
        let Some(location) = self.builtin(name) else {
            return;
        };
        unsafe {
            self.gl.use_program(Some(self.handle));
            self.gl
                .uniform_matrix_4_f32_slice(Some(location), false, &value.to_cols_array());
        }
    }
    pub fn matrix3(&self, name: &str, value: glam::Mat3) {
        let Some(location) = self.builtin(name) else {
            return;
        };
        unsafe {
            self.gl.use_program(Some(self.handle));
            self.gl
                .uniform_matrix_3_f32_slice(Some(location), false, &value.to_cols_array());
        }
    }
}
fn audio_uniform(name: &str) -> Option<(usize, usize)> {
    let name = name.strip_suffix("[0]").unwrap_or(name);
    for (channel, label) in ["Left", "Right", "Average"].iter().enumerate() {
        if let Some(count) = name
            .strip_prefix("g_AudioSpectrum")
            .and_then(|value| {
                value
                    .strip_suffix(label)
                    .or_else(|| value.strip_prefix(label))
            })
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| [16, 32, 64].contains(value))
        {
            return Some((channel, count));
        }
    }
    None
}
impl Drop for Pass {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_program(self.handle);
        }
    }
}
pub(crate) struct Mesh {
    gl: Rc<glow::Context>,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    instance: Option<glow::Buffer>,
    indices: Option<glow::Buffer>,
    attributes: Vec<Attribute>,
    count: i32,
    vertices: [f32; 20],
    last_frame: Option<usize>,
    vertex_bytes: u64,
    index_bytes: u64,
    instance_bytes: u64,
    instance_floats: usize,
    vertex_layout: bool,
    primitive: u32,
    bindings: RefCell<Option<VertexBindings>>,
}
enum VertexBindings {
    Quad([Option<u32>; 2]),
    Mesh(HashMap<&'static str, u32>),
}
struct Attribute {
    name: &'static str,
    count: i32,
    stride: i32,
    offset: i32,
    instance: bool,
}
impl Mesh {
    pub fn byte_size(&self) -> u64 {
        self.vertex_bytes + self.index_bytes + self.instance_bytes
    }
    pub fn instance_byte_size(&self) -> u64 {
        self.instance_bytes
    }
    pub fn new(gl: Rc<glow::Context>, size: [f32; 2], uv: [f32; 2], flip: bool) -> Result<Self> {
        unsafe {
            let vao = gl.create_vertex_array().map_err(anyhow::Error::msg)?;
            let vbo = match gl.create_buffer() {
                Ok(v) => v,
                Err(e) => {
                    gl.delete_vertex_array(vao);
                    return Err(anyhow::Error::msg(e));
                }
            };
            let mut mesh = Self {
                gl,
                vao,
                vbo,
                instance: None,
                indices: None,
                count: 4,
                vertices: [0.0; 20],
                last_frame: None,
                vertex_bytes: 80,
                index_bytes: 0,
                instance_bytes: 0,
                instance_floats: 0,
                vertex_layout: false,
                primitive: glow::TRIANGLE_STRIP,
                bindings: RefCell::new(None),
                attributes: vec![
                    Attribute {
                        name: "a_Position",
                        count: 3,
                        stride: 20,
                        offset: 0,
                        instance: false,
                    },
                    Attribute {
                        name: "a_TexCoord",
                        count: 2,
                        stride: 20,
                        offset: 12,
                        instance: false,
                    },
                ],
            };
            let [x, y] = [size[0] / 2.0, size[1] / 2.0];
            let [v0, v1] = if flip { [uv[1], 0.0] } else { [0.0, uv[1]] };
            let vertices: [f32; 20] = [
                -x, -y, 0.0, 0.0, v0, x, -y, 0.0, uv[0], v0, -x, y, 0.0, 0.0, v1, x, y, 0.0, uv[0],
                v1,
            ];
            mesh.vertices = vertices;
            mesh.gl.bind_vertex_array(Some(vao));
            mesh.gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let bytes = std::slice::from_raw_parts(
                vertices.as_ptr().cast::<u8>(),
                std::mem::size_of_val(&vertices),
            );
            mesh.gl
                .buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STATIC_DRAW);
            Ok(mesh)
        }
    }
    pub fn particles(
        gl: Rc<glow::Context>,
        pass: &Pass,
    ) -> Result<(Self, crate::particles::instances::Layout)> {
        let layout =
            crate::particles::instances::Layout::new(|name| pass.attributes.contains_key(name));
        let mut mesh = Self::new(gl, [1.0; 2], [1.0; 2], false)?;
        unsafe {
            mesh.instance = Some(mesh.gl.create_buffer().map_err(anyhow::Error::msg)?);
            mesh.gl.bind_buffer(glow::ARRAY_BUFFER, Some(mesh.vbo));
            let quad = [0.0_f32, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
            let bytes =
                std::slice::from_raw_parts(quad.as_ptr().cast(), std::mem::size_of_val(&quad));
            mesh.gl
                .buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STATIC_DRAW);
        }
        mesh.attributes = vec![Attribute {
            name: "a_Corner",
            count: 2,
            stride: 8,
            offset: 0,
            instance: false,
        }];
        for &(name, count, offset) in &layout.attributes {
            mesh.attributes.push(Attribute {
                name,
                count: count as i32,
                stride: (layout.floats * 4) as i32,
                offset: (offset * 4) as i32,
                instance: true,
            });
        }
        mesh.count = 0;
        mesh.vertex_bytes = 32;
        mesh.instance_floats = layout.floats;
        Ok((mesh, layout))
    }
    pub fn indexed(gl: Rc<glow::Context>, vertices: &[f32], indices: &[u32]) -> Result<Self> {
        ensure!(
            vertices.len().is_multiple_of(14)
                && vertices.len() / 14 <= 2_000_000
                && indices.len() <= 6_000_000
                && indices.len().is_multiple_of(3),
            "invalid model mesh budget"
        );
        ensure!(
            indices.iter().all(|i| (*i as usize) < vertices.len() / 14),
            "model triangle outside vertex buffer"
        );
        let mut mesh = Self::new(gl, [1.0; 2], [1.0; 2], false)?;
        unsafe {
            let buffer = mesh.gl.create_buffer().map_err(anyhow::Error::msg)?;
            mesh.indices = Some(buffer);
            mesh.gl.bind_vertex_array(Some(mesh.vao));
            mesh.gl
                .bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(buffer));
            let bytes =
                std::slice::from_raw_parts(indices.as_ptr().cast(), std::mem::size_of_val(indices));
            mesh.gl
                .buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, bytes, glow::STATIC_DRAW);
        }
        mesh.attributes = [
            ("a_Position", 3, 0),
            ("a_Normal", 3, 12),
            ("a_Tangent4", 4, 24),
            ("a_TexCoord", 2, 40),
            ("a_TexCoordVec4", 4, 40),
            ("a_TexCoordC1", 2, 48),
        ]
        .into_iter()
        .map(|(name, count, offset)| Attribute {
            name,
            count,
            offset,
            stride: 56,
            instance: false,
        })
        .collect();
        mesh.count = indices.len() as i32;
        mesh.vertex_layout = true;
        mesh.primitive = glow::TRIANGLES;
        mesh.index_bytes = std::mem::size_of_val(indices) as u64;
        mesh.upload_model(vertices)?;
        Ok(mesh)
    }
    pub fn upload_model(&mut self, vertices: &[f32]) -> Result<()> {
        ensure!(
            vertices.len().is_multiple_of(14)
                && vertices.len() / 14 <= 2_000_000
                && vertices.iter().all(|v| v.is_finite()),
            "invalid model vertex input"
        );
        unsafe {
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            let bytes = std::slice::from_raw_parts(
                vertices.as_ptr().cast(),
                std::mem::size_of_val(vertices),
            );
            self.gl
                .buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::DYNAMIC_DRAW);
        }
        self.vertex_bytes = std::mem::size_of_val(vertices) as u64;
        Ok(())
    }
    pub fn custom(
        gl: Rc<glow::Context>,
        vertices: &[f32],
        indices: Option<&[u32]>,
        format: &[(&'static str, i32)],
        dynamic: [bool; 2],
    ) -> Result<Self> {
        let stride = format.iter().map(|(_, count)| *count).sum::<i32>();
        ensure!(
            (3..=16).contains(&stride) && vertices.len().is_multiple_of(stride as usize),
            "invalid custom vertex layout"
        );
        ensure!(
            vertices.len() / stride as usize <= 2_000_000 && vertices.iter().all(|v| v.is_finite()),
            "invalid custom vertex budget"
        );
        let mut mesh = Self::new(gl, [1.; 2], [1.; 2], false)?;
        mesh.vertex_layout = true;
        mesh.primitive = glow::TRIANGLES;
        mesh.attributes.clear();
        let mut offset = 0;
        for (name, count) in format {
            mesh.attributes.push(Attribute {
                name,
                count: *count,
                stride: stride * 4,
                offset,
                instance: false,
            });
            if *name == "a_TexCoord" {
                mesh.attributes.push(Attribute {
                    name: "a_TexCoordVec4",
                    count: 2,
                    stride: stride * 4,
                    offset,
                    instance: false,
                });
            }
            offset += count * 4;
        }
        unsafe {
            mesh.gl.bind_vertex_array(Some(mesh.vao));
            mesh.gl.bind_buffer(glow::ARRAY_BUFFER, Some(mesh.vbo));
            let bytes = std::slice::from_raw_parts(
                vertices.as_ptr().cast(),
                std::mem::size_of_val(vertices),
            );
            mesh.gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytes,
                if dynamic[0] {
                    glow::DYNAMIC_DRAW
                } else {
                    glow::STATIC_DRAW
                },
            );
            if let Some(indices) = indices {
                ensure!(
                    indices.len() <= 6_000_000
                        && indices.len().is_multiple_of(3)
                        && indices
                            .iter()
                            .all(|i| (*i as usize) < vertices.len() / stride as usize),
                    "invalid custom indices"
                );
                mesh.indices = Some(mesh.gl.create_buffer().map_err(anyhow::Error::msg)?);
                mesh.gl
                    .bind_buffer(glow::ELEMENT_ARRAY_BUFFER, mesh.indices);
                let bytes = std::slice::from_raw_parts(
                    indices.as_ptr().cast(),
                    std::mem::size_of_val(indices),
                );
                mesh.gl.buffer_data_u8_slice(
                    glow::ELEMENT_ARRAY_BUFFER,
                    bytes,
                    if dynamic[1] {
                        glow::DYNAMIC_DRAW
                    } else {
                        glow::STATIC_DRAW
                    },
                );
                mesh.index_bytes = std::mem::size_of_val(indices) as u64;
                mesh.count = indices.len() as i32;
            } else {
                ensure!(
                    (vertices.len() / stride as usize).is_multiple_of(3),
                    "custom non-indexed vertices must form triangles"
                );
                mesh.count = (vertices.len() / stride as usize) as i32;
            }
        }
        mesh.vertex_bytes = std::mem::size_of_val(vertices) as u64;
        Ok(mesh)
    }
    pub fn update_custom(
        &mut self,
        vertices: Option<&[f32]>,
        indices: Option<&[u32]>,
    ) -> Result<()> {
        ensure!(
            vertices.is_none_or(|v| std::mem::size_of_val(v) as u64 == self.vertex_bytes
                && v.iter().all(|f| f.is_finite()))
                && indices.is_none_or(|i| std::mem::size_of_val(i) as u64 == self.index_bytes
                    && self.indices.is_some()),
            "custom buffer update changes layout"
        );
        unsafe {
            self.gl.bind_vertex_array(Some(self.vao));
            if let Some(vertices) = vertices {
                self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
                self.gl.buffer_sub_data_u8_slice(
                    glow::ARRAY_BUFFER,
                    0,
                    std::slice::from_raw_parts(
                        vertices.as_ptr().cast(),
                        std::mem::size_of_val(vertices),
                    ),
                );
            }
            if let Some(indices) = indices {
                self.gl
                    .bind_buffer(glow::ELEMENT_ARRAY_BUFFER, self.indices);
                self.gl.buffer_sub_data_u8_slice(
                    glow::ELEMENT_ARRAY_BUFFER,
                    0,
                    std::slice::from_raw_parts(
                        indices.as_ptr().cast(),
                        std::mem::size_of_val(indices),
                    ),
                );
            }
        }
        Ok(())
    }
    pub fn upload_particles(&mut self, instances: &[f32]) -> Result<()> {
        ensure!(
            self.instance.is_some()
                && self.instance_floats > 0
                && instances.len().is_multiple_of(self.instance_floats)
                && instances.len() / self.instance_floats <= 100_000,
            "particle GPU instance budget exceeded"
        );
        ensure!(
            instances.iter().all(|v| v.is_finite()),
            "non-finite particle GPU input"
        );
        unsafe {
            self.gl.bind_buffer(glow::ARRAY_BUFFER, self.instance);
            let bytes = std::slice::from_raw_parts(
                instances.as_ptr().cast(),
                std::mem::size_of_val(instances),
            );
            self.gl
                .buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STREAM_DRAW);
        }
        self.count = (instances.len() / self.instance_floats) as i32;
        self.instance_bytes = std::mem::size_of_val(instances) as u64;
        Ok(())
    }
    pub fn animate(&mut self, texture: &Texture, time: f32, flip: bool) {
        let Some((index, frame)) = texture.frame(time) else {
            return;
        };
        if self.last_frame == Some(index) {
            return;
        }
        self.last_frame = Some(index);
        for (i, [s, t]) in [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]
            .into_iter()
            .enumerate()
        {
            let t = if flip { 1. - t } else { t };
            self.vertices[i * 5 + 3] = frame.origin[0] + s * frame.u[0] + t * frame.v[0];
            self.vertices[i * 5 + 4] = frame.origin[1] + s * frame.u[1] + t * frame.v[1];
        }
        unsafe {
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            let bytes = std::slice::from_raw_parts(
                self.vertices.as_ptr().cast(),
                std::mem::size_of_val(&self.vertices),
            );
            self.gl
                .buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, bytes);
        }
    }
}
impl Drop for Mesh {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_buffer(self.vbo);
            if let Some(buffer) = self.instance {
                self.gl.delete_buffer(buffer);
            }
            if let Some(buffer) = self.indices {
                self.gl.delete_buffer(buffer);
            }
            self.gl.delete_vertex_array(self.vao);
        }
    }
}

pub(crate) fn blend_name(value: &serde_json::Value) -> Result<String> {
    let name = value["blending"].as_str().unwrap_or("normal");
    ensure!(
        ["translucent", "additive", "normal"].contains(&name),
        "unsupported blending {name}"
    );
    Ok(name.into())
}
