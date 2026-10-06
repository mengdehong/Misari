//! Layer drawing, composition inputs, picking, lighting and particle emission.
use super::*;
use particles::emission::{Bitmap, Cache, MAX_EDGE, Pose, Snapshot, UvMesh};

#[derive(Clone, Copy)]
pub(super) struct Surface {
    pub output: [u32; 2],
    pub target: Option<glow::Framebuffer>,
    pub view: camera::View,
    pub clip: [i32; 4],
    pub group: Option<usize>,
    pub position: usize,
}
impl Renderer {
    fn image_matrix(&self, layer: &Layer, view: camera::View) -> Mat4 {
        if layer.fullscreen {
            // Postprocess layers cover their destination even when parented to
            // a transformed scene node. Keep the top-first effect UV convention.
            Mat4::orthographic_rh_gl(
                -layer.size[0] / 2.,
                layer.size[0] / 2.,
                -layer.size[1] / 2.,
                layer.size[1] / 2.,
                -1.,
                1.,
            )
        } else {
            view.matrix(layer.state.perspective) * self.image_transform(layer)
        }
    }
    pub(super) fn image_transform(&self, layer: &Layer) -> Mat4 {
        let mut transform = layer.state.transform;
        if layer.offset != [0.; 2] {
            transform *= Mat4::from_translation(Vec3::new(layer.offset[0], layer.offset[1], 0.));
        }
        if self.general.parallax {
            let origin = layer.state.parallax_anchor;
            let [w, h] = self.scene.size;
            let offset = Vec3::new(
                (origin.x - w / 2.0 + (0.5 - self.filtered[0]) * w * self.general.influence)
                    * layer.state.depth[0]
                    * self.general.amount,
                (origin.y - h / 2.0 - (0.5 - self.filtered[1]) * h * self.general.influence)
                    * layer.state.depth[1]
                    * self.general.amount,
                0.0,
            );
            transform = Mat4::from_translation(offset) * transform;
        }
        transform
    }
    pub(super) fn draw_image_content(
        &self,
        index: usize,
        frame: &mut Frame<'_>,
        surface: &Surface,
        composed: bool,
        prepared: &[u8],
    ) -> Result<()> {
        let layer = &self.layers[index];
        if !self.runtime.alive(self.scene.layers[index].node)
            || self.paused && layer.referenced && layer.cached.get()
        {
            return Ok(());
        }
        frame.appearance = Some((&layer.state).into());
        let transform = self.image_transform(layer);
        let output = surface.output;
        let target = surface.target;
        let view = surface.view;
        let matrix = self.image_matrix(layer, view);
        layer.base.matrix4("g_ModelMatrix", transform);
        layer.base.matrix4(
            "g_ModelMatrixInverse",
            if transform.determinant().abs() > 1e-12 {
                transform.inverse()
            } else {
                Mat4::IDENTITY
            },
        );
        layer.base.matrix4(
            "g_ViewProjectionMatrix",
            view.matrix(layer.state.perspective),
        );
        let normal = glam::Mat3::from_mat4(transform);
        layer.base.matrix3(
            "g_NormalModelMatrix",
            if normal.determinant().abs() > 1e-12 {
                normal.inverse().transpose()
            } else {
                normal
            },
        );
        layer.base.vector3("g_EyePosition", view.eye);
        let [left, bottom, width, height] = surface.clip;
        unsafe {
            if layer.needs_backdrop(composed)
                && let Some(backdrop) = self.surface_backdrop(surface)
            {
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, target);
                self.gl
                    .bind_texture(glow::TEXTURE_2D, Some(backdrop.texture.handle));
                self.gl.copy_tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    0,
                    0,
                    output[0] as i32,
                    output[1] as i32,
                );
            }
            let mut base_inputs = [None; 8];
            for (slot, name) in layer.base.references.iter().enumerate() {
                if !composed && let Some(name) = name {
                    base_inputs[slot] =
                        if let Some(history) = self.cyclic_base_texture(name, prepared) {
                            Some(history)
                        } else {
                            self.surface_texture(name, surface, prepared)?
                        };
                }
            }
            let Some(buffers) = &layer.buffers else {
                if layer.base.requires_projection() {
                    frame.projection = matrix
                        * Mat4::from_scale(Vec3::new(layer.size[0] / 2., -layer.size[1] / 2., 1.));
                }
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, target);
                self.gl.viewport(0, 0, output[0] as i32, output[1] as i32);
                self.gl.enable(glow::SCISSOR_TEST);
                self.gl.scissor(left, bottom, width, height);
                self.blend_surface(&layer.blend, surface);
                self.gl
                    .color_mask(true, true, true, surface.group.is_some());
                layer.base.draw(
                    &layer.effect_mesh,
                    matrix,
                    frame,
                    &base_inputs,
                    layer.state.color,
                );
                self.gl.color_mask(true, true, true, true);
                self.gl.disable(glow::SCISSOR_TEST);
                return Ok(());
            };
            frame.projection =
                matrix * Mat4::from_scale(Vec3::new(layer.size[0] / 2., -layer.size[1] / 2., 1.));
            let local = Mat4::orthographic_rh_gl(
                -layer.size[0] / 2.,
                layer.size[0] / 2.,
                -layer.size[1] / 2.,
                layer.size[1] / 2.,
                -1.,
                1.,
            );
            self.bind(&buffers[0], true);
            self.blend("normal");
            if composed {
                let mut inputs = [None; 8];
                inputs[0] = Some(layer.compose_target.as_ref().unwrap().texture.as_ref());
                self.copy.vector4("g_Compose", Vec4::X);
                self.copy
                    .draw(&layer.effect_mesh, local, frame, &inputs, Vec4::ONE);
                self.copy.vector4("g_Compose", Vec4::ZERO);
            } else {
                layer.base.draw(
                    &layer.source_mesh,
                    if layer.base.structure["shader"] == "composelayer" {
                        view.matrix(layer.state.perspective)
                            * transform
                            * Mat4::from_scale(Vec3::new(1., -1., 1.))
                    } else {
                        local
                    },
                    frame,
                    &base_inputs,
                    if layer.puppet.is_some() {
                        Vec4::ONE
                    } else {
                        layer.state.color
                    },
                );
            }
            if let Some(published) = self.global_buffers.get(&layer.published[2]) {
                self.copy_buffer(published, &buffers[0], frame);
            }
            let mut current = 0;
            for effect in layer.effects.iter().filter(|effect| effect.visible) {
                for step in &effect.steps {
                    if let Step::Draw { inputs, .. } = step {
                        for (_, route) in inputs {
                            if let Route::Scene(name) = route {
                                self.surface_texture(name, surface, prepared)?;
                            }
                        }
                    }
                    if let Step::Copy {
                        source: Route::Scene(name),
                        ..
                    } = step
                    {
                        self.surface_texture(name, surface, prepared)?;
                    }
                }
                let input = &buffers[current];
                let destination = &buffers[1 - current];
                if (self.paused || frame.delta == 0.)
                    && effect.cached.get()
                    && let Some(cache) = &effect.cache
                {
                    self.copy_buffer(destination, cache, frame);
                    current = 1 - current;
                    continue;
                }
                let buffer = |route: &Route| -> &Buffer {
                    match route {
                        Route::Input => input,
                        Route::Output => destination,
                        Route::Named(name) => effect.buffers[name].read(destination),
                        Route::Scene(name) => {
                            // The reference's unresolved active edge uses this chain's
                            // current input. Self-composite is not temporal feedback.
                            if name.starts_with("_rt_imageLayerComposite")
                                && self.layers.iter().enumerate().any(|(i, l)| {
                                    prepared[i] == 1 && l.published.iter().any(|n| n == name)
                                })
                            {
                                input
                            } else {
                                self.surface_buffer(name, surface, prepared)
                                    .expect("validated scene reference")
                            }
                        }
                    }
                };
                let write = |route: &Route| -> &Buffer {
                    match route {
                        Route::Named(name) => effect.buffers[name].write(destination),
                        _ => buffer(route),
                    }
                };
                let mut wrote_output = false;
                for step in &effect.steps {
                    let read = |route: &Route, target: &Route| -> &Buffer {
                        if *route == Route::Output && (*target == Route::Output || !wrote_output) {
                            effect.output_history.as_ref().unwrap()
                        } else {
                            buffer(route)
                        }
                    };
                    match step {
                        Step::Copy {
                            source,
                            target,
                            clear,
                        } => {
                            let target_buffer = write(target);
                            self.bind(target_buffer, *clear);
                            self.blend("normal");
                            let mut inputs = [None; 8];
                            inputs[0] = if let Route::Scene(name) = source
                                && name.starts_with("_system$")
                            {
                                self.surface_texture(name, surface, prepared)?
                            } else {
                                Some(read(source, target).texture.as_ref())
                            };
                            self.copy
                                .draw(&self.quad, Mat4::IDENTITY, frame, &inputs, Vec4::ONE);
                        }
                        Step::Draw {
                            pass,
                            target,
                            blend,
                            inputs: bindings,
                            clear,
                        } => {
                            let target_buffer = write(target);
                            self.bind(target_buffer, *clear);
                            self.blend(blend);
                            let mut inputs: [Option<&Texture>; 8] = [None; 8];
                            for (slot, source) in bindings {
                                inputs[*slot] = if let Route::Scene(name) = source {
                                    if name.starts_with("_system$") {
                                        self.surface_texture(name, surface, prepared)?
                                    } else {
                                        Some(&read(source, target).texture)
                                    }
                                } else {
                                    Some(&read(source, target).texture)
                                };
                            }
                            if pass.textures[0].is_none() && inputs[0].is_none() {
                                inputs[0] = Some(&input.texture);
                            }
                            pass.draw(&self.quad, Mat4::IDENTITY, frame, &inputs, Vec4::ONE);
                        }
                    }
                    let route = match step {
                        Step::Draw { target, .. } | Step::Copy { target, .. } => target,
                    };
                    if let Route::Named(name) = route {
                        effect.buffers[name].commit();
                    }
                    if *route == Route::Output {
                        wrote_output = true;
                    }
                }
                if let Some(history) = &effect.output_history {
                    self.copy_buffer(history, destination, frame);
                }
                if let Some(cache) = &effect.cache {
                    if effect
                        .output_history
                        .as_ref()
                        .is_none_or(|history| history.handle != cache.handle)
                    {
                        self.copy_buffer(cache, destination, frame);
                    }
                    effect.cached.set(true);
                }
                current = 1 - current;
            }
            // Reuse the completed group target for the styled final result. This
            // also makes named consumers see container opacity/tint exactly once.
            let result = if composed {
                let group = layer.compose_target.as_ref().unwrap();
                let source = &buffers[current];
                if source.handle != group.handle || layer.state.color != Vec4::ONE {
                    // An odd effect chain can already end in the recycled group
                    // target. Tint through the other scratch to avoid self-sampling.
                    let target = if source.handle == group.handle {
                        &buffers[1 - current]
                    } else {
                        group
                    };
                    self.bind(target, true);
                    self.blend("normal");
                    let mut inputs = [None; 8];
                    inputs[0] = Some(source.texture.as_ref());
                    self.copy.draw(
                        &self.quad,
                        Mat4::IDENTITY,
                        frame,
                        &inputs,
                        layer.state.color,
                    );
                    self.copy_buffer(group, target, frame);
                }
                group.as_ref()
            } else {
                buffers[current].as_ref()
            };
            let mut last_publication = None;
            for (name, source) in [
                (&layer.published[0], result),
                // Cross-layer A/B names alias the completed publication in the
                // reference; neither may expose another layer's shared scratch.
                (&layer.published[1], result),
            ] {
                if let Some(published) = self.global_buffers.get(name)
                    && last_publication != Some(published.handle)
                {
                    self.copy_buffer(published, source, frame);
                    last_publication = Some(published.handle);
                }
            }
            layer.cached.set(true);
        }
        Ok(())
    }
    pub(super) fn present_image(
        &self,
        index: usize,
        frame: &mut Frame<'_>,
        surface: &Surface,
        composed: bool,
    ) -> Result<()> {
        let layer = &self.layers[index];
        if !self.surface_visible(self.scene.layers[index].node, layer.state.visible, surface) {
            return Ok(());
        }
        let Some(buffers) = &layer.buffers else {
            return Ok(());
        };
        let current = layer.effects.iter().filter(|e| e.visible).count() % 2;
        let texture = self.global_buffers.get(&layer.published[0]).map_or_else(
            || {
                if composed {
                    layer.compose_target.as_ref().unwrap().texture.as_ref()
                } else {
                    buffers[current].texture.as_ref()
                }
            },
            |b| b.texture.as_ref(),
        );
        let matrix = self.image_matrix(layer, surface.view);
        let [left, bottom, width, height] = surface.clip;
        frame.appearance = Some((&layer.state).into());
        let mut inputs = [None; 8];
        inputs[0] = Some(texture);
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, surface.target);
            self.gl
                .viewport(0, 0, surface.output[0] as i32, surface.output[1] as i32);
            self.gl.enable(glow::SCISSOR_TEST);
            self.gl.scissor(left, bottom, width, height);
            self.blend_surface(&layer.blend, surface);
            self.gl
                .color_mask(true, true, true, surface.group.is_some());
            let pass = if layer.state.blend_mode == 0 {
                &self.copy
            } else {
                self.capture_material_backdrop(std::iter::once("_rt_FullFrameBuffer"), surface);
                inputs[1] = self.surface_backdrop(surface).map(|b| b.texture.as_ref());
                self.gl.disable(glow::BLEND);
                let pass = self
                    .color_blend
                    .as_ref()
                    .context("missing layer blending program")?;
                pass.vector4(
                    "g_Compose",
                    Vec4::new(
                        layer.state.blend_mode as f32,
                        surface.group.is_some() as u8 as f32,
                        surface.output[0] as f32,
                        surface.output[1] as f32,
                    ),
                );
                pass
            };
            pass.draw(
                &layer.effect_mesh,
                matrix,
                frame,
                &inputs,
                if layer.puppet.is_some() {
                    layer.state.color
                } else {
                    Vec4::ONE
                },
            );
            self.gl.color_mask(true, true, true, true);
            self.gl.disable(glow::SCISSOR_TEST);
        }
        Ok(())
    }

    pub(crate) fn prepare_color_blending(&mut self) -> Result<()> {
        if !self.layers.iter().any(|layer| layer.state.blend_mode != 0) {
            return Ok(());
        }
        let pass = if self.color_blend.is_none() {
            let assets = match &self.assets {
                Some(assets) => assets.clone(),
                None => Assets::open(&self.paths.0, &self.paths.1, self.paths.2.as_deref())?,
            };
            Some(load(self.gl.clone(), &assets)?)
        } else {
            None
        };
        let mut budget = self.resources().bytes;
        let mut replacements = Vec::new();
        for (index, layer) in self.layers.iter().enumerate() {
            if layer.state.blend_mode != 0 && layer.buffers.is_none() {
                let size = layer.size.map(|n| n.ceil() as u32);
                replacements.push((
                    index,
                    creation::scratch_buffers(
                        self.gl.clone(),
                        size,
                        if self.general.post.hdr {
                            glow::RGBA16F
                        } else {
                            glow::RGBA8
                        },
                        layer.effects.is_empty(),
                        None,
                        &mut budget,
                    )?,
                ));
            }
        }
        for (index, buffers) in replacements {
            self.layers[index].buffers = Some(buffers);
        }
        if let Some(pass) = pass {
            self.color_blend = Some(pass);
        }
        self.capture_backdrop = true;
        self.budget = budget;
        Ok(())
    }

    pub(super) fn prepare_camera_fade(&mut self) -> Result<()> {
        if self.runtime.camera.fade_cover() > 0. && self.camera_fade.is_none() {
            self.camera_fade = Some(Pass::compile(
                self.gl.clone(),
                COPY_VERTEX,
                "#version 300 es\nprecision highp float; uniform vec4 g_Color4; out vec4 color; void main(){color=g_Color4;}",
            )?);
        }
        Ok(())
    }
    pub(super) fn draw_camera_fade(
        &self,
        target: Option<glow::Framebuffer>,
        output: [u32; 2],
        frame: &Frame<'_>,
    ) -> Result<()> {
        let cover = self.runtime.camera.fade_cover();
        if cover <= 0. {
            return Ok(());
        }
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, target);
            self.gl.viewport(0, 0, output[0] as i32, output[1] as i32);
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.disable(glow::DEPTH_TEST);
            self.gl.disable(glow::CULL_FACE);
            self.gl.enable(glow::BLEND);
            self.gl.blend_equation(glow::FUNC_ADD);
            self.gl.blend_func_separate(
                glow::SRC_ALPHA,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
            );
        }
        self.camera_fade.as_ref().unwrap().draw(
            &self.quad,
            Mat4::IDENTITY,
            frame,
            &[None; 8],
            Vec4::new(
                self.general.clear[0],
                self.general.clear[1],
                self.general.clear[2],
                cover,
            ),
        );
        unsafe {
            self.gl.disable(glow::BLEND);
        }
        Ok(())
    }

    pub(super) fn pointer_hits(
        &self,
        view: camera::View,
        world: [f32; 2],
        screen: [f32; 2],
        states: &[scene::State],
    ) -> Result<PointerHits> {
        let point = Vec3::new(world[0], world[1], 0.);
        let inverses = states
            .iter()
            .map(|state| {
                let determinant = state.transform.determinant();
                (determinant.is_finite() && determinant.abs() >= 1e-12)
                    .then(|| state.transform.inverse())
                    .filter(|inverse| inverse.is_finite())
            })
            .collect::<Vec<_>>();
        let mut locals = inverses
            .iter()
            .map(|inverse| {
                inverse.map_or([0.; 3], |inverse| {
                    inverse.transform_point3(point).to_array()
                })
            })
            .collect::<Vec<_>>();
        let active = states
            .iter()
            .enumerate()
            .map(|(node, state)| {
                state.visible && inverses[node].is_some() && self.runtime.nodes[node].solid
            })
            .collect::<Vec<_>>();
        let eligible = active
            .iter()
            .enumerate()
            .filter_map(|(node, &active)| active.then_some(node))
            .collect();
        let mut hits = Vec::new();
        if !self.focused {
            return Ok((hits, locals, eligible));
        }
        // Keep image/text rectangles and their local event coordinates unchanged.
        for (spec, layer) in self.scene.layers.iter().zip(&self.layers) {
            if active[spec.node] {
                let point = Vec3::from_array(locals[spec.node]);
                if (point.x - layer.offset[0]).abs() <= layer.size[0] / 2.
                    && (point.y - layer.offset[1]).abs() <= layer.size[1] / 2.
                {
                    hits.push(spec.node);
                }
            }
        }
        for model in &self.models {
            let node = model.index;
            if !active[node] {
                continue;
            }
            let state = &states[node];
            if let Some(ray) =
                crate::model_bounds::ray(view.matrix(state.perspective) * state.transform, screen)
            {
                let direction = ray[1] - ray[0];
                if direction.z.abs() > f32::MIN_POSITIVE {
                    let local = ray[0] - direction * (ray[0].z / direction.z);
                    if local.is_finite() {
                        locals[node] = local.to_array();
                    }
                }
                if let Some(point) = model
                    .local_bounds()
                    .and_then(|bounds| crate::model_bounds::hit(bounds, ray))
                {
                    hits.push(node);
                    locals[node] = point.to_array();
                }
            }
        }
        Ok((hits, locals, eligible))
    }

    pub(super) fn prepare_shadow_sources(
        &self,
        groups: &compose::Groups,
        prepared: &mut [u8],
        frame: &mut Frame<'_>,
        output: [u32; 2],
        view: camera::View,
    ) -> Result<()> {
        if self.shadows.is_none() {
            return Ok(());
        }
        let surface = draw::Surface {
            output,
            target: self.model_target.as_ref().map(|b| b.handle),
            view,
            clip: [0, 0, output[0] as i32, output[1] as i32],
            group: None,
            position: 0,
        };
        for model in self
            .models
            .iter()
            .filter(|m| m.state.visible && self.runtime.nodes[m.index].casts_shadow)
        {
            for name in model.shadow_references() {
                if let Some(index) = self
                    .layers
                    .iter()
                    .position(|layer| layer.published.iter().any(|n| n == name))
                {
                    // Backdrop masks use the existing published preceding-frame
                    // output. Ordinary sources share the main production visits,
                    // so a feedback effect is never advanced twice this frame.
                    if !self.image_backdrops[index] {
                        self.draw_composed_object(
                            RenderObject::Image(index),
                            groups,
                            prepared,
                            frame,
                            &surface,
                        )?;
                    }
                }
            }
        }
        frame.appearance = None;
        Ok(())
    }
    pub(super) fn prepare_shadows(&mut self, lights: &mut lighting::Snapshot) -> Result<Vec<Mat4>> {
        let matrices = lighting::shadow::plan(lights, &self.models, &self.runtime.nodes)?;
        if matrices.is_empty() {
            self.shadows = None;
        } else if self
            .shadows
            .as_ref()
            .is_none_or(|s| s.target.pages != matrices.len())
        {
            // Allocate before replacing: failure preserves the accepted resources
            // and the caller's last presentation. Count the transient old target.
            self.shadows = Some(lighting::shadow::Renderer::load(
                self.gl.clone(),
                matrices.len(),
                self.resources().bytes,
            )?);
        }
        lights.shadows = self.shadows.as_ref().map(|s| s.target.clone());
        self.budget = self.resources().bytes;
        Ok(matrices)
    }
    pub(crate) fn initial_shadows(&mut self) -> Result<()> {
        let mut lights = lighting::Snapshot::collect(
            &self.runtime.nodes,
            &self.runtime.states()?,
            self.general.light_capacity,
        )?;
        self.prepare_shadows(&mut lights)?;
        Ok(())
    }

    /// Set one world-space planar model reflection, n·position + d = 0.
    /// Normals are normalized together with d. None restores the legacy Y=0
    /// capture without halfspace clipping. Explicit planes clip geometry on the
    /// opposite side from the source camera (normal side when it is on-plane).
    /// This host API does not introduce an undocumented scene JSON property.
    pub fn set_reflection_plane(&mut self, plane: Option<[f32; 4]>) -> Result<()> {
        let plane = plane.map(normalize).transpose()?;
        if self.paused {
            self.pending_reflection_plane = Some(plane);
        } else {
            self.reflection_plane = plane;
        }
        Ok(())
    }
    pub(super) fn prepare_reflection(&mut self, output: [u32; 2]) -> Result<()> {
        if !self.capture_reflection {
            return Ok(());
        }
        let format = if self.general.post.hdr {
            glow::RGBA16F
        } else {
            glow::RGBA8
        };
        if self
            .reflection
            .as_ref()
            .is_none_or(|b| b.texture.size != output || b.texture.format != format)
        {
            let old = self.reflection.as_ref().map_or(0, |b| {
                b.texture.byte_size() + b.texture.size[0] as u64 * b.texture.size[1] as u64 * 4
            });
            let mut budget = self.budget.saturating_sub(old);
            let candidate =
                Buffer::formatted_with_depth(self.gl.clone(), output, format, &mut budget)?;
            self.reflection = Some(candidate);
            self.budget = budget;
        }
        Ok(())
    }
    pub(super) fn render_reflection(
        &self,
        output: [u32; 2],
        view: camera::View,
        frame: &mut Frame<'_>,
    ) -> Result<()> {
        if !self.capture_reflection {
            return Ok(());
        }
        let buffer = self.reflection.as_ref().unwrap();
        unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(buffer.handle));
            self.gl.viewport(0, 0, output[0] as i32, output[1] as i32);
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.disable(glow::BLEND);
            self.gl.color_mask(true, true, true, true);
            self.gl.depth_mask(true);
            self.gl.clear_color(
                self.general.clear[0],
                self.general.clear[1],
                self.general.clear[2],
                0.,
            );
            self.gl.clear_depth_f32(1.);
            self.gl
                .clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        }
        let explicit = self
            .models
            .iter()
            .any(|m| self.runtime.nodes[m.index].reflected.is_some());
        let mirror = mirror(self.reflection_plane.unwrap_or(Vec4::Y));
        let eye = mirror.transform_point3(view.eye);
        frame.appearance = None;
        for model in &self.models {
            if !model.state.visible
                || explicit && self.runtime.nodes[model.index].reflected != Some(true)
            {
                continue;
            }
            let projection = view.matrix(model.state.perspective) * mirror;
            let clip_plane = self
                .reflection_plane
                .map_or(Vec4::ZERO, |plane| clip_plane(projection, plane, view.eye));
            frame.projection = projection * model.state.transform;
            model.draw_reflected(&self.gl, projection, eye, clip_plane, frame, |name| {
                self.scene_texture(name)
            })?;
        }
        Ok(())
    }

    pub(super) fn emission_requests(&self) -> HashMap<String, bool> {
        let mut requests = HashMap::new();
        for particle in &self.particles {
            if (particle.state.visible || self.compose_node_needed(particle.index))
                && (!self.paused || !particle.initialized() || !particle.ready())
            {
                for (id, periodic) in particle.image_requests() {
                    let commands = &self.runtime.objects[particle.index]["__particle"]["commands"];
                    let demand = particle.image_demand()
                        || commands.as_array().is_some_and(|a| !a.is_empty());
                    *requests.entry(id.to_owned()).or_insert(false) |= periodic && demand;
                }
            }
        }
        requests
    }

    pub(super) fn emission_sources(
        &mut self,
        view: camera::View,
        time: f32,
        delta: f32,
        states: &[scene::State],
    ) -> Result<Sources> {
        let requests = self.emission_requests();
        for layer in &mut self.layers {
            if let Some(cache) = &mut layer.emission {
                cache.periodic = false;
            }
        }
        let mut sources = Sources {
            snapshots: HashMap::new(),
            pending: std::collections::HashSet::new(),
        };
        if requests.is_empty() {
            return Ok(sources);
        }
        let lights = if self.has_lights {
            lighting::Snapshot::collect(&self.runtime.nodes, states, self.general.light_capacity)?
        } else {
            lighting::Snapshot::default()
        };
        for index in 0..self.layers.len() {
            let object = &self.runtime.objects[self.scene.layers[index].node];
            let id = object["id"].to_string();
            if !self.runtime.nodes[self.scene.layers[index].node].alive() {
                self.layers[index].emission = None;
                continue;
            }
            let Some(&periodic) = requests.get(&id) else {
                continue;
            };
            let texture_ready = self.layers[index]
                .base
                .textures
                .iter()
                .flatten()
                .all(|texture| self.media_system.texture_ready(texture));
            if !texture_ready {
                sources.pending.insert(id.clone());
                if self.layers[index].emission.is_none() {
                    continue;
                }
            }
            let refresh = texture_ready
                && self.layers[index].emission.as_ref().is_none_or(|cache| {
                    periodic && (time < cache.captured || time - cache.captured >= 0.1)
                });
            let bitmap = if refresh {
                let frame = Frame {
                    composite: false,
                    audio: &self.audio,
                    time,
                    delta,
                    pointer: self.filtered,
                    last_pointer: self.previous,
                    down: self.down,
                    parallax: [0.5; 2],
                    projection: Mat4::IDENTITY,
                    screen: self.scene.size.map(|n| n.ceil() as u32),
                    appearance: Some((&self.layers[index].state).into()),
                    lights: &lights,
                    ambient: self.general.ambient,
                    skylight: self.general.skylight,
                };
                let bitmap = self.capture_emission_bitmap(index, view, &frame)?;
                let used: usize = self
                    .layers
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != index)
                    .filter_map(|(_, l)| l.emission.as_ref())
                    .map(Cache::bytes)
                    .sum();
                ensure!(
                    used + bitmap.bytes() <= 64 * 1024 * 1024,
                    "scene particle emission bitmaps exceed 64 MiB"
                );
                Rc::new(bitmap)
            } else {
                self.layers[index].emission.as_ref().unwrap().bitmap.clone()
            };
            let layer = &mut self.layers[index];
            let mesh = if let Some(cache) = &layer.emission {
                cache.mesh.clone()
            } else {
                layer
                    .puppet
                    .as_ref()
                    .map(|rig| UvMesh::new(&rig.model).map(Rc::new))
                    .transpose()?
            };
            let pose = Pose {
                transform: layer.state.transform
                    * Mat4::from_translation(Vec3::new(layer.offset[0], layer.offset[1], 0.)),
                size: glam::Vec2::from_array(layer.size),
                positions: layer.puppet.as_mut().map(mdl::Rig::emission_positions),
            };
            let previous = layer.emission.as_ref().and_then(|cache| {
                let dt = time - cache.time;
                (dt > 1e-6).then(|| (cache.snapshot.pose.clone(), dt))
            });
            let snapshot = Rc::new(Snapshot {
                bitmap: bitmap.clone(),
                mesh: mesh.clone(),
                pose,
                previous,
            });
            let captured = if refresh {
                time
            } else {
                layer.emission.as_ref().unwrap().captured
            };
            let cache = Cache {
                source: id.clone(),
                bitmap,
                mesh,
                time,
                snapshot: snapshot.clone(),
                captured,
                periodic,
            };
            let used: usize = self
                .layers
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != index)
                .filter_map(|(_, l)| l.emission.as_ref())
                .map(Cache::bytes)
                .sum();
            ensure!(
                used + cache.bytes() <= 64 * 1024 * 1024,
                "scene particle emission resources exceed 64 MiB"
            );
            self.layers[index].emission = Some(cache);
            sources.snapshots.insert(id, snapshot);
        }
        Ok(sources)
    }
    pub(crate) fn emission_uses_audio(&self) -> bool {
        self.layers.iter().any(|layer| {
            layer.emission.as_ref().is_some_and(|cache| {
                cache.periodic
                    && layer.base.requires_audio()
                    && self.particles.iter().any(|particle| {
                        particle.state.visible
                            && particle.image_demand()
                            && particle
                                .image_requests()
                                .any(|(id, periodic)| periodic && id == cache.source)
                    })
            })
        })
    }
    fn capture_emission_bitmap(
        &self,
        index: usize,
        view: camera::View,
        frame: &Frame<'_>,
    ) -> Result<Bitmap> {
        let layer = &self.layers[index];
        let ratio = (MAX_EDGE as f32 / layer.size[0].max(layer.size[1])).min(1.);
        let size = layer.size.map(|n| (n * ratio).ceil().max(1.) as u32);
        let mut budget = self.budget;
        let buffer = Buffer::formatted(self.gl.clone(), size, glow::RGBA8, &mut budget)?;
        let transform = layer.state.transform;
        layer.base.matrix4("g_ModelMatrix", transform);
        layer.base.matrix4(
            "g_ModelMatrixInverse",
            if transform.determinant().abs() > 1e-12 {
                transform.inverse()
            } else {
                Mat4::IDENTITY
            },
        );
        layer.base.matrix4(
            "g_ViewProjectionMatrix",
            view.matrix(layer.state.perspective),
        );
        let normal = glam::Mat3::from_mat4(transform);
        layer.base.matrix3(
            "g_NormalModelMatrix",
            if normal.determinant().abs() > 1e-12 {
                normal.inverse().transpose()
            } else {
                normal
            },
        );
        layer.base.vector3("g_EyePosition", view.eye);
        let mut inputs = [None; 8];
        for (slot, name) in layer.base.references.iter().enumerate() {
            if let Some(name) = name {
                inputs[slot] = self.scene_texture(name)?;
            }
        }
        let local = Mat4::orthographic_rh_gl(
            -layer.size[0] * 0.5,
            layer.size[0] * 0.5,
            -layer.size[1] * 0.5,
            layer.size[1] * 0.5,
            -1.,
            1.,
        );
        let mut rgba = vec![0; size[0] as usize * size[1] as usize * 4];
        unsafe {
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.disable(glow::DEPTH_TEST);
            self.gl.disable(glow::CULL_FACE);
            self.gl.disable(glow::BLEND);
            self.gl.color_mask(true, true, true, true);
            self.bind(&buffer, true);
            layer
                .base
                .draw(&layer.source_mesh, local, frame, &inputs, layer.state.color);
            self.gl.read_pixels(
                0,
                0,
                size[0] as i32,
                size[1] as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut rgba)),
            );
            ensure!(
                self.gl.get_error() == glow::NO_ERROR,
                "reading particle emission bitmap failed"
            );
        }
        Bitmap::new(size, rgba)
    }
}

pub(crate) fn dependencies<'a>(
    layers: impl Iterator<Item = &'a Layer>,
    globals: &HashMap<String, Rc<Buffer>>,
) -> Result<(Vec<Vec<usize>>, Vec<bool>)> {
    let layers = layers.collect::<Vec<_>>();
    let producers = layers
        .iter()
        .enumerate()
        .flat_map(|(i, l)| l.published.iter().map(move |name| (name.as_str(), i)))
        .collect::<HashMap<_, _>>();
    let mut dependencies = Vec::new();
    let mut backdrops = Vec::new();
    for layer in layers {
        let mut names = layer
            .base
            .references
            .iter()
            .flatten()
            .map(String::as_str)
            .collect::<Vec<_>>();
        for effect in &layer.effects {
            for step in &effect.steps {
                match step {
                    Step::Draw { inputs, .. } => {
                        names.extend(inputs.iter().filter_map(|(_, r)| {
                            if let Route::Scene(name) = r {
                                Some(name.as_str())
                            } else {
                                None
                            }
                        }))
                    }
                    Step::Copy {
                        source: Route::Scene(name),
                        ..
                    } => names.push(name),
                    _ => {}
                }
            }
        }
        backdrops.push(names.contains(&"_rt_FullFrameBuffer"));
        let mut own = Vec::new();
        for name in names
            .into_iter()
            .filter(|n| n.starts_with("_rt_imageLayer"))
        {
            let Some(&producer) = producers.get(name) else {
                ensure!(globals.contains_key(name), "unknown scene texture {name}");
                continue;
            };
            if !own.contains(&producer) {
                own.push(producer);
            }
        }
        dependencies.push(own);
    }
    Ok((dependencies, backdrops))
}

pub(crate) fn load(gl: Rc<glow::Context>, assets: &Assets) -> Result<Pass> {
    let source = String::from_utf8(assets.read("shaders/common_blending.h")?)?;
    ensure!(
        source.len() <= 1024 * 1024,
        "WE blending header exceeds 1 MiB"
    );
    let (helpers, function) = source
        .split_once("vec3 ApplyBlending")
        .context("WE blending header has no ApplyBlending function")?;
    let mut fragment = format!(
        "#version 300 es\nprecision highp float;\n#define HDR 1\n#define CAST3(x) vec3(x)\n#define saturate(x) clamp(x,0.0,1.0)\n{helpers}\nvec3 ApplyBlending{function}"
    );
    // The shipped function uses preprocessor branches for one material's mode.
    // Make those exact branches runtime choices so dynamic properties share one program.
    for mode in 1..=32 {
        fragment = fragment.replace(
            &format!("#if BLENDMODE == {mode}\r\n"),
            &format!("if (blendMode == {mode}) {{\n"),
        );
        fragment = fragment.replace(
            &format!("#if BLENDMODE == {mode}\n"),
            &format!("if (blendMode == {mode}) {{\n"),
        );
    }
    // Only the function's mode branches are closed here; helper HDR guards stay intact.
    let boundary = fragment.find("vec3 ApplyBlending").unwrap();
    let converted = fragment[boundary..].replace("#endif", "}");
    fragment.truncate(boundary);
    fragment.push_str(&converted);
    ensure!(
        !fragment.contains("BLENDMODE"),
        "unsupported WE blending header branches"
    );
    fragment.push_str(
        "\nin vec2 uv;\nuniform sampler2D g_Texture0;\nuniform sampler2D g_Texture1;\nuniform vec4 g_Color4;\nuniform vec4 g_Compose;\nout vec4 color;\nvoid main(){\nvec4 src=texture(g_Texture0,uv)*g_Color4;\nvec4 dst=texture(g_Texture1,gl_FragCoord.xy/g_Compose.zw);\nint mode=int(g_Compose.x);\nfloat a=clamp(src.a,0.0,1.0);\nif(g_Compose.y<0.5){color=vec4(ApplyBlending(mode,dst.rgb,src.rgb,a),dst.a);}\nelse{\nvec3 base=dst.a>0.000001?dst.rgb/dst.a:vec3(0.0);\nvec3 blended=ApplyBlending(mode,base,src.rgb,1.0);\ncolor=vec4((1.0-a)*dst.rgb+a*(1.0-dst.a)*src.rgb+a*dst.a*blended,a+dst.a*(1.0-a));\n}\n}\n",
    );
    Pass::compile(gl, COPY_VERTEX, &fragment)
}

type PointerHits = (Vec<usize>, Vec<[f32; 3]>, Vec<usize>);

fn normalize(plane: [f32; 4]) -> Result<Vec4> {
    let plane = Vec4::from_array(plane);
    ensure!(plane.is_finite(), "non-finite reflection plane");
    let length = plane.truncate().as_dvec3().length();
    ensure!(length > 0., "invalid reflection plane normal");
    let plane = (plane.as_dvec4() / length).as_vec4();
    ensure!(
        plane.is_finite() && mirror(plane).is_finite(),
        "reflection plane overflows"
    );
    Ok(plane)
}
fn mirror(plane: Vec4) -> Mat4 {
    let n = plane.truncate();
    Mat4::from_cols(
        (Vec3::X - 2. * n.x * n).extend(0.),
        (Vec3::Y - 2. * n.y * n).extend(0.),
        (Vec3::Z - 2. * n.z * n).extend(0.),
        (-2. * plane.w * n).extend(1.),
    )
}
fn clip_plane(view: Mat4, mut plane: Vec4, eye: Vec3) -> Vec4 {
    if plane.dot(eye.extend(1.)) < 0. {
        plane = -plane;
    }
    view.inverse().transpose() * plane
}

pub(super) struct Sources {
    pub snapshots: HashMap<String, Rc<Snapshot>>,
    pub pending: std::collections::HashSet<String>,
}

#[cfg(test)]
mod reflection_tests {
    use super::*;
    #[test]
    fn arbitrary_plane_involution_translation_and_clip_halfspace() {
        for p in [[0., 2., 0., -6.], [1., 1., 0., -2.], [0., 0., -1., -4.]] {
            let plane = normalize(p).unwrap();
            let reflection = mirror(plane);
            assert!((reflection * reflection).abs_diff_eq(Mat4::IDENTITY, 1e-5));
            assert!((reflection.determinant() + 1.).abs() < 1e-5);
            let point = Vec3::new(1., 4., -6.);
            let mirrored = reflection.transform_point3(point);
            assert!((plane.dot(point.extend(1.)) + plane.dot(mirrored.extend(1.))).abs() < 1e-5);
        }
        for projection in [
            Mat4::orthographic_rh_gl(-10., 10., -10., 10., 1., 100.),
            Mat4::perspective_rh_gl(90f32.to_radians(), 1., 1., 100.),
        ] {
            let eye = Vec3::new(0., 0., 10.);
            let plane = Vec4::new(1., 0., 0., 2.);
            let view = projection * Mat4::from_translation(-eye) * mirror(plane);
            let clip = clip_plane(view, plane, eye);
            let inside = view * Vec3::new(-1., 0., 0.).extend(1.);
            let outside = view * Vec3::new(-3., 0., 0.).extend(1.);
            assert!(clip.dot(inside) > 0. && clip.dot(outside) < 0.);
        }
        assert!(normalize([0.; 4]).is_err());
        assert!(normalize([f32::NAN, 1., 0., 0.]).is_err());
    }
}
