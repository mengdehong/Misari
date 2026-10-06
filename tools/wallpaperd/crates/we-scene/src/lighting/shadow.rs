//! Depth maps for LightingV1. No resources are created for disabled lights.

use crate::{
    gpu::{Frame, GPU_BUDGET, Pass, Usage},
    mdl,
    model_bounds::{self, Bounds, corners},
};
use anyhow::{Result, ensure};
use glam::{Mat4, Vec3};
use glow::HasContext;
use std::rc::Rc;

// Internal quality choices, not authored WE properties. GLES 3 guarantees at
// least 256 array layers; even 15 point + 15 wide spot + 15 directional fit.
pub(crate) const SIZE: i32 = 512;
const MAX_DRAWS: usize = 16384;
pub(crate) struct Renderer {
    pub target: Rc<Target>,
    opaque: Pass,
    cutout: Pass,
}

fn view(origin: Vec3, direction: Vec3) -> Mat4 {
    let up = if direction.y.abs() > 0.99 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    Mat4::look_at_rh(origin, origin + direction, up)
}
fn cube(origin: Vec3, near: f32, far: f32) -> [Mat4; 6] {
    let projection = Mat4::perspective_rh_gl(std::f32::consts::FRAC_PI_2, 1., near, far);
    [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z]
        .map(|direction| projection * view(origin, direction))
}
fn directional(direction: Vec3, bounds: Bounds) -> Mat4 {
    let center = (Vec3::from_array(bounds[0]) + Vec3::from_array(bounds[1])) * 0.5;
    let extent = (Vec3::from_array(bounds[1]) - Vec3::from_array(bounds[0]))
        .length()
        .max(0.01);
    let view = view(center - direction * extent, direction);
    let mut range = None;
    for corner in corners(bounds) {
        model_bounds::include(&mut range, view.transform_point3(corner).to_array());
    }
    let [min, max] = range.unwrap().map(Vec3::from_array);
    let pad = extent / SIZE as f32 * 2.;
    Mat4::orthographic_rh_gl(
        min.x - pad,
        max.x + pad,
        min.y - pad,
        max.y + pad,
        (-max.z - pad).max(0.001),
        -min.z + pad,
    ) * view
}

/// Fit directional maps to all visible model bounds, including non-casting
/// receivers. Shadow casters need not be visible in the main camera frustum.
pub(crate) fn plan(
    lights: &mut super::Snapshot,
    models: &[mdl::Renderer],
    nodes: &[crate::scene::Node],
) -> Result<Vec<Mat4>> {
    if !(0..lights.modern.count).any(|slot| {
        lights.modern.casts[slot]
            && Vec3::from_slice(&lights.modern.color[slot * 4..slot * 4 + 3]) != Vec3::ZERO
    }) {
        return Ok(Vec::new());
    }
    let mut bounds = None;
    let mut draws = 0usize;
    for model in models.iter().filter(|m| m.state.visible) {
        if let Some(local) = model.local_bounds() {
            for corner in corners(local) {
                model_bounds::include(
                    &mut bounds,
                    model.state.transform.transform_point3(corner).to_array(),
                );
            }
        }
        if nodes[model.index].casts_shadow {
            draws += model.shadow_draw_count();
        }
    }
    let mut matrices = Vec::new();
    if draws == 0 {
        return Ok(matrices);
    }
    for slot in 0..lights.modern.count {
        let i = slot * 4;
        let modern = &mut lights.modern;
        if !modern.casts[slot] || Vec3::from_slice(&modern.color[i..i + 3]) == Vec3::ZERO {
            continue;
        }
        let origin = Vec3::from_slice(&modern.origin[i..i + 3]);
        let direction = Vec3::from_slice(&modern.direction[i..i + 3]);
        let kind = modern.direction[i + 3] as u8;
        let base = matrices.len();
        if kind == 3 {
            if let Some(bounds) = bounds {
                matrices.push(directional(direction, bounds));
            }
        } else if kind == 0 || kind == 1 {
            let far = modern.color[i + 3];
            if far <= 0. {
                continue;
            }
            // Raise the near plane when the light is outside the scene's world
            // bounds: distant lights otherwise lose precision with a tiny fixed
            // near plane. The AABB distance is a conservative lower bound.
            let distance = bounds.map_or(0., |b| {
                origin.distance(origin.clamp(Vec3::from_array(b[0]), Vec3::from_array(b[1])))
            });
            let planar = kind == 1 && modern.extra[i + 1] > 0.001;
            let depth = distance * if planar { modern.extra[i + 1] } else { 1. };
            let near = if depth > 0. {
                depth * 0.25
            } else {
                (far * 1e-5).clamp(0.001, 0.01)
            }
            .min(far * 0.1);
            // A planar spot projection cannot represent cones reaching behind
            // the source; use the same six faces as point lights in that case.
            if !planar {
                matrices.extend(cube(origin, near, far));
                if kind == 1 {
                    modern.extra[i + 2] = 1.;
                }
            } else {
                let angle = modern.extra[i + 1].acos().max(0.001);
                matrices.push(
                    Mat4::perspective_rh_gl(angle * 2., 1., near, far) * view(origin, direction),
                );
            }
        }
        if matrices.len() != base {
            modern.extra[i + 3] = (base + 1) as f32;
        }
    }
    ensure!(
        matrices.iter().all(|m| m.is_finite()),
        "non-finite shadow projection"
    );
    ensure!(
        matrices
            .len()
            .checked_mul(draws)
            .is_some_and(|n| n <= MAX_DRAWS),
        "shadow draw budget exceeded: {} maps × {draws} geometry draws (limit {MAX_DRAWS})",
        matrices.len()
    );
    Ok(matrices)
}

impl Renderer {
    pub fn load(gl: Rc<glow::Context>, pages: usize, used: u64) -> Result<Self> {
        ensure!(
            used + Target::bytes(pages) <= GPU_BUDGET,
            "shadow targets exceed scene GPU budget: {pages} depth maps"
        );
        let target = Rc::new(Target::new(gl.clone(), pages)?);
        let opaque = Pass::compile(
            gl.clone(),
            include_str!("shadow.vert"),
            include_str!("shadow.frag"),
        )?;
        let cutout = Pass::compile(
            gl,
            include_str!("shadow.vert"),
            include_str!("shadow_cutout.frag"),
        )?;
        Ok(Self {
            target,
            opaque,
            cutout,
        })
    }
    pub fn resources(&self, usage: &mut Usage) {
        self.target.resources(usage);
    }
    pub fn draw<'a>(
        &self,
        gl: &glow::Context,
        matrices: &[Mat4],
        models: &'a [mdl::Renderer],
        nodes: &[crate::scene::Node],
        frame: &Frame<'_>,
        scene: impl Fn(&str) -> Result<Option<&'a crate::gpu::Texture>>,
    ) -> Result<()> {
        self.target.upload(matrices);
        unsafe {
            gl.viewport(0, 0, SIZE, SIZE);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(true);
            gl.enable(glow::POLYGON_OFFSET_FILL);
            gl.polygon_offset(1., 1.);
            gl.color_mask(false, false, false, false);
        }
        let result = (|| {
            for (page, &matrix) in matrices.iter().enumerate() {
                self.target.bind(page);
                unsafe {
                    gl.clear_depth_f32(1.);
                    gl.clear(glow::DEPTH_BUFFER_BIT);
                }
                for model in models
                    .iter()
                    .filter(|m| m.state.visible && nodes[m.index].casts_shadow)
                {
                    model.draw_shadow(gl, matrix, &self.opaque, &self.cutout, frame, &scene)?;
                }
            }
            Ok(())
        })();
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.disable(glow::POLYGON_OFFSET_FILL);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.color_mask(true, true, true, true);
            gl.front_face(glow::CCW);
        }
        result
    }
}

pub(crate) struct Target {
    gl: Rc<glow::Context>,
    depth: glow::Texture,
    matrices: Option<glow::Texture>,
    framebuffer: Option<glow::Framebuffer>,
    pub pages: usize,
}
impl Target {
    pub fn bytes(pages: usize) -> u64 {
        pages as u64 * (SIZE as u64 * SIZE as u64 * 4 + 64)
    }
    pub fn new(gl: Rc<glow::Context>, pages: usize) -> Result<Self> {
        unsafe {
            ensure!(
                pages > 0 && pages <= gl.get_parameter_i32(glow::MAX_ARRAY_TEXTURE_LAYERS) as usize,
                "shadow depth layers exceed GLES capacity: {pages}"
            );
            ensure!(
                gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) >= SIZE
                    && gl.get_parameter_i32(glow::MAX_TEXTURE_IMAGE_UNITS) >= 10,
                "shadows require 512px depth targets and 10 fragment texture slots"
            );
            let mut target = Self {
                depth: gl.create_texture().map_err(anyhow::Error::msg)?,
                gl,
                matrices: None,
                framebuffer: None,
                pages,
            };
            let gl = &target.gl;
            gl.active_texture(glow::TEXTURE8);
            gl.bind_texture(glow::TEXTURE_2D_ARRAY, Some(target.depth));
            gl.tex_storage_3d(
                glow::TEXTURE_2D_ARRAY,
                1,
                glow::DEPTH_COMPONENT24,
                SIZE,
                SIZE,
                pages as i32,
            );
            for (name, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D_ARRAY, name, value as i32);
            }
            let matrices = gl.create_texture().map_err(anyhow::Error::msg)?;
            target.matrices = Some(matrices);
            gl.active_texture(glow::TEXTURE9);
            gl.bind_texture(glow::TEXTURE_2D, Some(matrices));
            gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::RGBA32F, 4, pages as i32);
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            let framebuffer = gl.create_framebuffer().map_err(anyhow::Error::msg)?;
            target.framebuffer = Some(framebuffer);
            target.bind(0);
            gl.draw_buffers(&[glow::NONE]);
            gl.read_buffer(glow::NONE);
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            let error = gl.get_error();
            ensure!(
                status == glow::FRAMEBUFFER_COMPLETE && error == glow::NO_ERROR,
                "GLES DEPTH_COMPONENT24 shadow target unavailable: framebuffer {status:#x}, GL {error:#x}"
            );
            Ok(target)
        }
    }
    pub fn resources(&self, usage: &mut Usage) {
        if usage.textures.insert(self.depth) {
            usage.bytes += Self::bytes(self.pages) - self.pages as u64 * 64;
        }
        if usage.textures.insert(self.matrices.unwrap()) {
            usage.bytes += self.pages as u64 * 64;
        }
    }
    pub fn bind(&self, page: usize) {
        unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);
            self.gl.framebuffer_texture_layer(
                glow::FRAMEBUFFER,
                glow::DEPTH_ATTACHMENT,
                Some(self.depth),
                0,
                page as i32,
            );
        }
    }
    pub fn upload(&self, matrices: &[Mat4]) {
        let values = matrices
            .iter()
            .flat_map(|m| m.to_cols_array())
            .collect::<Vec<_>>();
        unsafe {
            self.gl.active_texture(glow::TEXTURE9);
            self.gl.bind_texture(glow::TEXTURE_2D, self.matrices);
            self.gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                4,
                self.pages as i32,
                glow::RGBA,
                glow::FLOAT,
                glow::PixelUnpackData::Slice(Some(std::slice::from_raw_parts(
                    values.as_ptr().cast(),
                    values.len() * 4,
                ))),
            );
        }
    }
}
impl Drop for Target {
    fn drop(&mut self) {
        unsafe {
            self.gl.delete_texture(self.depth);
            if let Some(texture) = self.matrices {
                self.gl.delete_texture(texture);
            }
            if let Some(framebuffer) = self.framebuffer {
                self.gl.delete_framebuffer(framebuffer);
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Uniforms {
    depth: Option<glow::UniformLocation>,
    matrices: Option<glow::UniformLocation>,
    receive: Option<glow::UniformLocation>,
}
impl Uniforms {
    pub fn new(gl: &glow::Context, program: glow::Program) -> Self {
        unsafe {
            Self {
                depth: gl.get_uniform_location(program, "wallpaperShadowDepth"),
                matrices: gl.get_uniform_location(program, "wallpaperShadowMatrices"),
                receive: gl.get_uniform_location(program, "wallpaperShadowReceive"),
            }
        }
    }
    pub fn bind(&self, gl: &glow::Context, lights: &crate::lighting::Snapshot, receive: bool) {
        if self.depth.is_none() && self.matrices.is_none() && self.receive.is_none() {
            return;
        }
        unsafe {
            gl.uniform_1_i32(self.receive.as_ref(), i32::from(receive));
            gl.active_texture(glow::TEXTURE8);
            gl.bind_texture(
                glow::TEXTURE_2D_ARRAY,
                lights.shadows.as_ref().map(|t| t.depth),
            );
            gl.uniform_1_i32(self.depth.as_ref(), 8);
            gl.active_texture(glow::TEXTURE9);
            gl.bind_texture(
                glow::TEXTURE_2D,
                lights.shadows.as_ref().and_then(|t| t.matrices),
            );
            gl.uniform_1_i32(self.matrices.as_ref(), 9);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projections_cover_all_cube_faces_and_directional_bounds() {
        let matrices = cube(Vec3::ZERO, 0.01, 100.);
        for (i, point) in [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z]
            .into_iter()
            .enumerate()
        {
            let p = matrices[i].project_point3(point * 10.);
            assert!(p.x.abs() < 1e-6 && p.y.abs() < 1e-6 && p.z.abs() < 1.);
        }
        let bounds = [[-10., -20., -30.], [1., 2., 3.]];
        for direction in [
            Vec3::X,
            Vec3::Y,
            -Vec3::Z,
            Vec3::new(1., 2., 3.).normalize(),
        ] {
            let matrix = directional(direction, bounds);
            assert!(!model_bounds::outside(bounds, matrix));
            for corner in corners(bounds) {
                assert!(matrix.project_point3(corner).abs().max_element() <= 1.);
            }
        }
        assert!(
            crate::scene::Node::decode(&serde_json::json!({}), [32.; 2])
                .unwrap()
                .casts_shadow
        );
        assert!(
            !crate::scene::Node::decode(&serde_json::json!({"castshadow":false}), [32.; 2])
                .unwrap()
                .casts_shadow
        );
    }
}
