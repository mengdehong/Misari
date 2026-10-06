//! Indexed model parts preserve their own WE materials and share one skeletal pose.
use super::*;
use crate::{
    assets::{AssetKey, Assets},
    gpu::{Frame, Mesh, Pass, Texture},
    model_data::{Data, GpuShape},
    scene::State,
    scene::bindings::Properties,
};
use anyhow::{Context, Result, ensure};
use glam::Mat3;
use glow::HasContext;
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
};
pub(crate) struct Renderer {
    pub index: usize,
    pub state: State,
    pub rig: Option<Rig>,
    custom: Option<CustomState>,
    draws: Vec<Draw>,
    bounds: Option<crate::model_bounds::Bounds>,
}
struct Draw {
    pub(super) geometry: usize,
    pub(super) mesh: Rc<crate::model_data::GpuShape>,
    pub(super) pass: Pass,
    pub(super) blend: String,
    pub(super) cull: bool,
    pub(super) depth: bool,
    pub(super) write: bool,
    pub(super) material: usize,
}
impl Renderer {
    pub fn resources(&self, usage: &mut crate::gpu::Usage) {
        for draw in &self.draws {
            usage.pass(&draw.pass);
            usage.mesh(&draw.mesh.mesh.borrow());
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn load(
        gl: Rc<glow::Context>,
        assets: &Assets,
        model: Rc<Model>,
        object: &Value,
        index: usize,
        state: State,
        properties: &Properties,
        textures: &mut HashMap<AssetKey, Rc<Texture>>,
        budget: &mut u64,
    ) -> Result<Self> {
        let mut rig = Rig::new(model.clone())?;
        rig.advance_in_space(
            object["__createdAt"].as_f64().unwrap_or(0.) as f32,
            object,
            state.transform,
        )?;
        let skin = object["skin"].as_u64().unwrap_or(0) as usize;
        let mut draws = Vec::new();
        let mut bounds = None;
        for (geometry, part) in model.meshes.iter().enumerate() {
            let material = part
                .materials
                .get(skin)
                .context("model skin outside materials")?;
            let value = assets.json(material)?;
            let vertices = rig.vertices(geometry);
            let indices = &part.indices;
            for &index in indices {
                crate::model_bounds::include(
                    &mut bounds,
                    vertices[index as usize * 14..][..3].try_into().unwrap(),
                );
            }
            let bytes = (vertices.len() * 4 + indices.len() * 4) as u64;
            ensure!(
                *budget + bytes <= crate::gpu::GPU_BUDGET,
                "scene model buffers exceed GPU budget"
            );
            let mesh = Rc::new(crate::model_data::GpuShape {
                mesh: std::cell::RefCell::new(Mesh::indexed(gl.clone(), &vertices, indices)?),
                vertex_revision: std::cell::Cell::new(0),
                index_revision: std::cell::Cell::new(0),
            });
            *budget += bytes;
            for (material_index, spec) in value["passes"]
                .as_array()
                .context("model material passes")?
                .iter()
                .enumerate()
            {
                ensure!(draws.len() < 1024, "model exceeds 1024 material draws");

                draws.push(Draw {
                    geometry,
                    mesh: mesh.clone(),
                    pass: Pass::load(
                        gl.clone(),
                        assets,
                        spec,
                        properties,
                        &HashSet::new(),
                        textures,
                        budget,
                        crate::shader::MaterialDomain::Model,
                    )?,
                    blend: crate::gpu::blend_name(spec)?,
                    cull: spec["cullmode"].as_str() == Some("normal"),
                    depth: spec["depthtest"].as_str() == Some("enabled"),
                    write: spec["depthwrite"].as_str() == Some("enabled"),
                    material: material_index,
                });
            }
        }
        Ok(Self {
            index,
            state,
            rig: Some(rig),
            custom: None,
            draws,
            bounds,
        })
    }
    pub fn advance(&mut self, time: f32, object: &Value) -> Result<()> {
        if let Some(rig) = &mut self.rig
            && rig.advance_in_space(time, object, self.state.transform)?
        {
            let mut updated = HashSet::new();
            let mut bounds = None;
            for draw in &mut self.draws {
                if updated.insert(draw.geometry) {
                    let vertices = rig.vertices(draw.geometry);
                    draw.mesh.mesh.borrow_mut().upload_model(&vertices)?;
                    for &index in &rig.model.meshes[draw.geometry].indices {
                        crate::model_bounds::include(
                            &mut bounds,
                            vertices[index as usize * 14..][..3].try_into().unwrap(),
                        );
                    }
                }
            }
            self.bounds = bounds;
        }
        Ok(())
    }
    pub fn update_content(&mut self, object: &Value) -> Result<()> {
        if let Some(custom) = &self.custom {
            custom.advance()?;
        }
        for draw in &mut self.draws {
            draw.pass.apply_constants(
                &object["__materials"][draw.geometry][draw.material]["constantshadervalues"],
            )?;
        }
        Ok(())
    }
    pub fn references(&self) -> impl Iterator<Item = &str> {
        self.draws
            .iter()
            .flat_map(|d| d.pass.references.iter().filter_map(Option::as_deref))
    }
    pub fn local_bounds(&self) -> Option<crate::model_bounds::Bounds> {
        self.custom
            .as_ref()
            .map_or(self.bounds, |custom| custom.bounds.get())
    }
    pub fn shadow_draw_count(&self) -> usize {
        self.draws
            .iter()
            .map(|d| d.geometry)
            .collect::<HashSet<_>>()
            .len()
    }
    pub fn shadow_references(&self) -> impl Iterator<Item = &str> {
        self.draws
            .iter()
            .filter(|d| d.pass.alpha_coverage)
            .filter_map(|d| d.pass.references.first().and_then(Option::as_deref))
    }
    pub fn draw_shadow<'a>(
        &'a self,
        gl: &glow::Context,
        view: Mat4,
        opaque: &Pass,
        cutout: &Pass,
        frame: &Frame<'_>,
        scene: impl Fn(&str) -> Result<Option<&'a Texture>>,
    ) -> Result<()> {
        let matrix = view * self.state.transform;
        if self
            .local_bounds()
            .is_some_and(|b| crate::model_bounds::outside(b, matrix))
        {
            return Ok(());
        }
        let mut geometries = HashSet::new();
        for draw in &self.draws {
            if !geometries.insert(draw.geometry) {
                continue;
            }
            unsafe {
                if draw.cull {
                    gl.enable(glow::CULL_FACE);
                    gl.cull_face(glow::BACK);
                    gl.front_face(if self.state.transform.determinant() < 0. {
                        glow::CW
                    } else {
                        glow::CCW
                    });
                } else {
                    gl.disable(glow::CULL_FACE);
                }
            }
            let alpha = draw.pass.alpha_coverage;
            let mut inputs = [None; 8];
            if alpha {
                inputs[0] =
                    if let Some(name) = draw.pass.references.first().and_then(Option::as_deref) {
                        scene(name)?
                    } else {
                        draw.pass.textures.first().and_then(Option::as_deref)
                    };
            }
            (if alpha { cutout } else { opaque }).draw(
                &draw.mesh.mesh.borrow(),
                matrix,
                frame,
                &inputs,
                self.state.color,
            );
        }
        Ok(())
    }
    pub fn draw<'a>(
        &'a self,
        gl: &glow::Context,
        view: Mat4,
        eye: Vec3,
        frame: &Frame<'_>,
        scene: impl Fn(&str) -> Result<Option<&'a Texture>>,
    ) -> Result<()> {
        self.draw_passes(gl, view, eye, frame, scene, None)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn draw_reflected<'a>(
        &'a self,
        gl: &glow::Context,
        view: Mat4,
        eye: Vec3,
        clip_plane: Vec4,
        frame: &Frame<'_>,
        scene: impl Fn(&str) -> Result<Option<&'a Texture>>,
    ) -> Result<()> {
        self.draw_passes(gl, view, eye, frame, scene, Some(clip_plane))
    }
    #[allow(clippy::too_many_arguments)]
    fn draw_passes<'a>(
        &'a self,
        gl: &glow::Context,
        view: Mat4,
        eye: Vec3,
        frame: &Frame<'_>,
        scene: impl Fn(&str) -> Result<Option<&'a Texture>>,
        clip_plane: Option<Vec4>,
    ) -> Result<()> {
        let reflection = clip_plane.is_some();
        if self
            .local_bounds()
            .is_some_and(|bounds| crate::model_bounds::outside(bounds, view * self.state.transform))
        {
            return Ok(());
        }
        for draw in &self.draws {
            if reflection
                && draw
                    .pass
                    .references
                    .iter()
                    .flatten()
                    .any(|name| name == "_rt_Reflection")
            {
                continue;
            }
            unsafe {
                if draw.depth {
                    gl.enable(glow::DEPTH_TEST);
                    gl.depth_func(glow::LEQUAL);
                } else {
                    gl.disable(glow::DEPTH_TEST);
                }
                gl.depth_mask(draw.write);
                if draw.cull {
                    gl.enable(glow::CULL_FACE);
                    gl.cull_face(glow::BACK);
                    gl.front_face(
                        if (self.state.transform.determinant() < 0.0) != reflection {
                            glow::CW
                        } else {
                            glow::CCW
                        },
                    );
                } else {
                    gl.disable(glow::CULL_FACE);
                }
                if draw.blend == "normal" {
                    if frame.composite {
                        gl.enable(glow::BLEND);
                        gl.blend_func_separate(glow::SRC_ALPHA, glow::ZERO, glow::ONE, glow::ZERO);
                    } else {
                        gl.disable(glow::BLEND);
                    }
                } else {
                    gl.enable(glow::BLEND);
                    gl.blend_func_separate(
                        glow::SRC_ALPHA,
                        if draw.blend == "additive" {
                            glow::ONE
                        } else {
                            glow::ONE_MINUS_SRC_ALPHA
                        },
                        glow::ONE,
                        if draw.blend == "additive" {
                            glow::ONE
                        } else {
                            glow::ONE_MINUS_SRC_ALPHA
                        },
                    );
                }
            }
            let model = self.state.transform;
            draw.pass.vector4(
                "g_NativeReflectionClipPlane",
                clip_plane.unwrap_or(Vec4::ZERO),
            );
            draw.pass.matrix4("g_ModelMatrix", model);
            draw.pass.matrix4(
                "g_ModelMatrixInverse",
                if model.determinant().abs() > 1e-12 {
                    model.inverse()
                } else {
                    Mat4::IDENTITY
                },
            );
            draw.pass.matrix4("g_ViewProjectionMatrix", view);
            let normal = Mat3::from_mat4(model);
            draw.pass.matrix3(
                "g_NormalModelMatrix",
                if normal.determinant().abs() > 1e-12 {
                    normal.inverse().transpose()
                } else {
                    normal
                },
            );
            draw.pass.vector3("g_EyePosition", eye);
            draw.pass.vector3("g_LightAmbientColor", frame.ambient);
            draw.pass.vector3("g_LightSkylightColor", frame.skylight);
            let mut inputs = [None; 8];
            for (slot, name) in draw.pass.references.iter().enumerate() {
                if let Some(name) = name {
                    inputs[slot] = scene(name)?;
                }
            }
            draw.pass.draw(
                &draw.mesh.mesh.borrow(),
                view * model,
                frame,
                &inputs,
                self.state.color,
            );
        }
        unsafe {
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.depth_mask(true);
            gl.front_face(glow::CCW);
        }
        Ok(())
    }
    pub fn animated(&self) -> bool {
        self.rig.as_ref().is_some_and(Rig::animated) || self.draws.iter().any(|d| d.pass.animated())
    }
    pub fn audio(&self) -> bool {
        self.draws.iter().any(|d| d.pass.requires_audio())
    }
    pub fn pointer(&self) -> bool {
        self.draws.iter().any(|d| d.pass.accepts_pointer())
    }
    pub fn changed(&self, properties: &Properties) -> Result<bool> {
        for d in &self.draws {
            if d.pass.changed(properties)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn prepare(&self, properties: &Properties) -> Result<Vec<Vec<Vec<f32>>>> {
        self.draws
            .iter()
            .map(|d| d.pass.prepare(properties))
            .collect()
    }
    pub fn apply(&mut self, values: Vec<Vec<Vec<f32>>>) {
        for (d, v) in self.draws.iter_mut().zip(values) {
            d.pass.apply(v);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn load_custom(
        gl: Rc<glow::Context>,
        assets: &Assets,
        data: Rc<RefCell<Data>>,
        object: &Value,
        index: usize,
        state: State,
        properties: &Properties,
        textures: &mut HashMap<AssetKey, Rc<Texture>>,
        budget: &mut u64,
    ) -> Result<Self> {
        let mut draws = Vec::new();
        let mut model = data.borrow_mut();
        for (geometry, shape) in model.shapes.iter_mut().enumerate() {
            let Some(shape) = shape else { continue };
            let mesh = if let Some(mesh) = shape.gpu.upgrade() {
                mesh
            } else {
                ensure!(
                    budget.saturating_add(shape.bytes() as u64) <= crate::gpu::GPU_BUDGET,
                    "custom mesh exceeds GPU budget"
                );
                let mesh = Rc::new(GpuShape {
                    mesh: RefCell::new(Mesh::custom(
                        gl.clone(),
                        &shape.vertices,
                        shape.indices.as_ref().map(|i| i.as_slice()),
                        &shape.format.0,
                        shape.dynamic,
                    )?),
                    vertex_revision: Cell::new(shape.vertex_revision),
                    index_revision: Cell::new(shape.index_revision),
                });
                *budget += shape.bytes() as u64;
                shape.gpu = Rc::downgrade(&mesh);
                mesh
            };
            for (material, spec) in shape
                .passes
                .as_array()
                .context("custom material passes")?
                .iter()
                .enumerate()
            {
                ensure!(draws.len() < 1024, "custom model draw budget exceeds 1024");

                let mut pass = Pass::load(
                    gl.clone(),
                    assets,
                    spec,
                    properties,
                    &HashSet::new(),
                    textures,
                    budget,
                    crate::shader::MaterialDomain::Model,
                )?;
                pass.apply_constants(
                    &object["__materials"][geometry][material]["constantshadervalues"],
                )?;
                draws.push(Draw {
                    geometry,
                    material,
                    mesh: mesh.clone(),
                    pass,
                    blend: crate::gpu::blend_name(spec)?,
                    cull: spec["cullmode"].as_str() == Some("normal"),
                    depth: spec["depthtest"].as_str() == Some("enabled"),
                    write: spec["depthwrite"].as_str() == Some("enabled"),
                });
            }
        }
        let revision = model.layout_revision;
        let bounds = model.effective_bounds();
        let materials = model
            .shapes
            .iter()
            .map(|s| s.as_ref().map(|s| s.material.clone()))
            .collect();
        drop(model);
        Ok(Self {
            index,
            state,
            rig: None,
            bounds: None,
            draws,
            custom: Some(CustomState {
                data,
                loaded_revision: revision,
                attempted_revision: revision,
                materials,
                bounds: Cell::new(bounds),
            }),
        })
    }
    pub fn custom_data(&self) -> Option<Rc<RefCell<Data>>> {
        self.custom.as_ref().map(|c| c.data.clone())
    }
    pub fn needs_custom_reload(&self) -> bool {
        self.custom
            .as_ref()
            .is_some_and(|c| c.data.borrow().layout_revision != c.attempted_revision)
    }
    pub fn acknowledge_custom_reload(&mut self) {
        if let Some(custom) = &mut self.custom {
            custom.attempted_revision = custom.data.borrow().layout_revision;
        }
    }
    pub fn custom_materials(&self) -> Value {
        let custom = self.custom.as_ref().unwrap();
        custom.data.borrow().metadata()["materials"].clone()
    }
    pub fn same_custom_material(&self, index: usize, name: &str) -> bool {
        self.custom.as_ref().is_some_and(|custom| {
            custom.materials.get(index).and_then(Option::as_deref) == Some(name)
        })
    }
}

struct CustomState {
    pub data: Rc<RefCell<Data>>,
    pub loaded_revision: u64,
    pub attempted_revision: u64,
    pub materials: Vec<Option<String>>,
    // The accepted GPU layout owns its bounds even if a later replacement fails.
    pub bounds: Cell<Option<crate::model_bounds::Bounds>>,
}
impl CustomState {
    pub fn advance(&self) -> Result<()> {
        let data = self.data.borrow();
        if data.layout_revision != self.loaded_revision {
            return Ok(());
        }
        for shape in data.shapes.iter().flatten() {
            if let Some(gpu) = shape.gpu.upgrade() {
                let vertices = (gpu.vertex_revision.get() != shape.vertex_revision)
                    .then(|| shape.vertices.as_slice());
                let indices = if gpu.index_revision.get() != shape.index_revision {
                    shape.indices.as_ref().map(|i| i.as_slice())
                } else {
                    None
                };
                if vertices.is_some() || indices.is_some() {
                    gpu.mesh.borrow_mut().update_custom(vertices, indices)?;
                    gpu.vertex_revision.set(shape.vertex_revision);
                    gpu.index_revision.set(shape.index_revision);
                }
            }
        }
        self.bounds.set(data.effective_bounds());
        Ok(())
    }
}
