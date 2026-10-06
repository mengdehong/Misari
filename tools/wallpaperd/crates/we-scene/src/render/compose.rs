//! Compose owns pixels; ordinary hierarchy parents only own transforms.

use super::*;

pub(super) struct Groups {
    owners: Vec<Option<usize>>,
    positions: Vec<usize>,
    pub children: Vec<Vec<RenderObject>>,
    pub containers: Vec<bool>,
}
pub(super) struct GroupTarget {
    pub index: usize,
    pub target: Rc<Buffer>,
    pub backdrop: Option<Rc<Buffer>>,
}
impl Renderer {
    fn object_node(&self, object: RenderObject) -> usize {
        match object {
            RenderObject::Image(i) => self.scene.layers[i].node,
            RenderObject::Model(i) => self.models[i].index,
            RenderObject::Particle(i) => self.particles[i].index,
        }
    }
    pub(super) fn compose_groups(&self) -> Groups {
        let parents = self.runtime.parents();
        let mut containers = vec![false; self.layers.len()];
        let mut by_node = vec![None; parents.len()];
        for (index, (spec, layer)) in self.scene.layers.iter().zip(&self.layers).enumerate() {
            if layer.base.structure["shader"] == "composelayer" {
                by_node[spec.node] = Some(index);
            }
        }
        for (node, parent) in parents.iter().enumerate() {
            if self.runtime.alive(node)
                && let Some(index) = parent.and_then(|p| by_node[p])
            {
                containers[index] = true;
            }
        }
        let mut owners = vec![None; parents.len()];
        let mut known = vec![false; parents.len()];
        for start in 0..parents.len() {
            let mut chain = Vec::new();
            let mut node = start;
            let owner = loop {
                if known[node] {
                    break owners[node];
                }
                chain.push(node);
                let Some(parent) = parents[node] else {
                    break None;
                };
                if let Some(group) = by_node[parent].filter(|g| containers[*g]) {
                    break Some(group);
                }
                node = parent;
            };
            for node in chain {
                owners[node] = owner;
                known[node] = true;
            }
        }
        let mut children = vec![Vec::new(); self.layers.len()];
        let mut positions = vec![0; parents.len()];
        for (position, &object) in self.order.iter().enumerate() {
            let node = self.object_node(object);
            positions[node] = position;
            if let Some(owner) = owners[node] {
                children[owner].push(object);
            }
        }
        Groups {
            owners,
            positions,
            children,
            containers,
        }
    }
    pub(super) fn prepare_compose_targets(&mut self, groups: &Groups) -> Result<()> {
        let backdrops = (0..self.layers.len())
            .map(|index| groups.containers[index] && self.group_needs_backdrop(index, groups))
            .collect::<Vec<_>>();
        let format = if self.general.post.hdr {
            glow::RGBA16F
        } else {
            glow::RGBA8
        };
        let depth = !self.models.is_empty();
        let reuse = (0..self.layers.len())
            .map(|index| groups.containers[index] && self.group_can_reuse_scratch(index, groups))
            .collect::<Vec<_>>();
        let unchanged = self.layers.iter().enumerate().all(|(index, layer)| {
            if !groups.containers[index] {
                return layer.compose_target.is_none() && layer.compose_backdrop.is_none();
            }
            let size = layer.size.map(|v| v.ceil() as u32);
            layer.compose_target.as_ref().is_some_and(|b| {
                b.texture.size == size
                    && b.texture.format == format
                    && b.has_depth() == depth
                    && if reuse[index] {
                        b.handle == layer.buffers.as_ref().unwrap()[1].handle
                    } else {
                        layer.buffers.as_ref().is_none_or(|buffers| {
                            buffers.iter().all(|scratch| scratch.handle != b.handle)
                        })
                    }
            }) && if backdrops[index] {
                layer
                    .compose_backdrop
                    .as_ref()
                    .is_some_and(|b| b.texture.size == size && b.texture.format == format)
            } else {
                layer.compose_backdrop.is_none()
            }
        });
        if unchanged {
            return Ok(());
        }
        // Resource accounting is needed only when storage changes, not on every frame.
        let mut budget = self.resources().bytes;
        let replacements = self.candidate_compose_targets(
            self.layers
                .iter()
                .enumerate()
                .filter(|(i, _)| groups.containers[*i])
                .map(|(i, layer)| (i, layer, backdrops[i], reuse[i])),
            !self.models.is_empty(),
            &mut budget,
        )?;
        self.commit_compose_targets(replacements);
        for (index, layer) in self.layers.iter_mut().enumerate() {
            if !groups.containers[index] {
                layer.compose_target = None;
                layer.compose_backdrop = None;
            } else if !backdrops[index] {
                layer.compose_backdrop = None;
            }
        }
        self.budget = self.resources().bytes;
        Ok(())
    }
    fn group_can_reuse_scratch(&self, index: usize, groups: &Groups) -> bool {
        let layer = &self.layers[index];
        let Some(buffers) = &layer.buffers else {
            return false;
        };
        let format = if self.general.post.hdr {
            glow::RGBA16F
        } else {
            glow::RGBA8
        };
        if !self.models.is_empty()
            || layer.referenced
            || buffers[0].handle == buffers[1].handle
            || buffers[1].texture.size != layer.size.map(|v| v.ceil() as u32)
            || buffers[1].texture.format != format
            || buffers[1].has_depth()
        {
            return false;
        }
        let target = buffers[1].handle;
        let mut pending = groups.children[index].clone();
        let mut visited = vec![false; self.layers.len()];
        visited[index] = true;
        while let Some(object) = pending.pop() {
            let names = match object {
                RenderObject::Image(image) => {
                    if std::mem::replace(&mut visited[image], true) {
                        continue;
                    }
                    let child = &self.layers[image];
                    if child.buffers.iter().flatten().any(|b| b.handle == target)
                        || child
                            .compose_target
                            .as_ref()
                            .is_some_and(|b| b.handle == target)
                        || child
                            .effects
                            .iter()
                            .flat_map(|e| e.framebuffers())
                            .any(|b| b.handle == target)
                    {
                        return false;
                    }
                    pending.extend(groups.children[image].iter().copied());
                    pending.extend(
                        self.image_dependencies[image]
                            .iter()
                            .copied()
                            .map(RenderObject::Image),
                    );
                    continue;
                }
                RenderObject::Model(model) => self.models[model].references().collect::<Vec<_>>(),
                RenderObject::Particle(particle) => {
                    self.particles[particle].references().collect::<Vec<_>>()
                }
            };
            pending.extend(names.into_iter().filter_map(|name| {
                self.layers
                    .iter()
                    .position(|layer| layer.published.iter().any(|n| n == name))
                    .map(RenderObject::Image)
            }));
        }
        // Child pixels have had their final read after conversion into scratch[0].
        // Recheck the child/dependency graph so dynamic reparenting cannot alias live pixels.
        true
    }
    fn group_needs_backdrop(&self, index: usize, groups: &Groups) -> bool {
        let mut pending = Vec::new();
        for &object in &groups.children[index] {
            let names = match object {
                RenderObject::Image(image) => {
                    pending.push(image);
                    continue;
                }
                RenderObject::Model(model) => self.models[model].references().collect::<Vec<_>>(),
                RenderObject::Particle(particle) => {
                    self.particles[particle].references().collect::<Vec<_>>()
                }
            };
            if names.contains(&"_rt_FullFrameBuffer") {
                return true;
            }
            pending.extend(names.into_iter().filter_map(|name| {
                self.layers
                    .iter()
                    .position(|layer| layer.published.iter().any(|n| n == name))
            }));
        }
        let mut visited = vec![false; self.layers.len()];
        while let Some(image) = pending.pop() {
            if std::mem::replace(&mut visited[image], true) {
                continue;
            }
            if self.layers[image].needs_backdrop(false) {
                return true;
            }
            pending.extend(&self.image_dependencies[image]);
        }
        false
    }
    pub(super) fn candidate_compose_targets<'a>(
        &self,
        layers: impl Iterator<Item = (usize, &'a Layer, bool, bool)>,
        depth_needed: bool,
        budget: &mut u64,
    ) -> Result<Vec<GroupTarget>> {
        let format = if self.general.post.hdr {
            glow::RGBA16F
        } else {
            glow::RGBA8
        };
        let mut replacements = Vec::new();
        for (index, layer, backdrop_needed, reuse) in layers {
            let size = layer.size.map(|v| v.ceil() as u32);
            let target = if reuse {
                Some(&layer.buffers.as_ref().unwrap()[1])
            } else {
                layer.compose_target.as_ref().filter(|b| {
                    b.texture.size == size
                        && b.texture.format == format
                        && b.has_depth() == depth_needed
                        && layer.buffers.as_ref().is_none_or(|buffers| {
                            buffers.iter().all(|scratch| scratch.handle != b.handle)
                        })
                })
            };
            let backdrop = layer
                .compose_backdrop
                .as_ref()
                .filter(|b| b.texture.size == size && b.texture.format == format);
            if target.is_some_and(|b| {
                layer
                    .compose_target
                    .as_ref()
                    .is_some_and(|old| old.handle == b.handle)
            }) && (!backdrop_needed || backdrop.is_some())
            {
                continue;
            }
            // Allocate every candidate before committing; old group pixels remain
            // available if a later allocation or the existing GPU budget fails.
            let target = match target {
                Some(target) => target.clone(),
                None => {
                    // All particle/image drawing is 2D. Conservatively retain
                    // depth for every group whenever the scene has a 3D model.
                    let allocate = if depth_needed {
                        Buffer::formatted_with_depth
                    } else {
                        Buffer::formatted
                    };
                    Rc::new(allocate(self.gl.clone(), size, format, budget)?)
                }
            };
            let backdrop = if backdrop_needed {
                Some(match backdrop {
                    Some(backdrop) => backdrop.clone(),
                    None => Rc::new(Buffer::formatted(self.gl.clone(), size, format, budget)?),
                })
            } else {
                None
            };
            replacements.push(GroupTarget {
                index,
                target,
                backdrop,
            });
        }
        Ok(replacements)
    }
    pub(super) fn commit_compose_targets(&mut self, replacements: Vec<GroupTarget>) {
        for GroupTarget {
            index,
            target,
            backdrop,
        } in replacements
        {
            self.layers[index].compose_target = Some(target);
            self.layers[index].compose_backdrop = backdrop;
        }
    }
    pub(super) fn compose_owned(&self, object: RenderObject, groups: &Groups) -> bool {
        groups.owners[self.object_node(object)].is_some()
    }
    fn visible_inside_group(&self, mut node: usize, group: usize) -> bool {
        let group_node = self.scene.layers[group].node;
        loop {
            if !self.runtime.nodes[node].state.visible {
                return false;
            }
            let Some(parent) = self.runtime.parents()[node] else {
                return true;
            };
            if parent == group_node || !self.runtime.propagates(parent) {
                return true;
            }
            node = parent;
        }
    }
    pub(super) fn surface_visible(
        &self,
        node: usize,
        visible: bool,
        surface: &draw::Surface,
    ) -> bool {
        surface
            .group
            .map_or(visible, |group| self.visible_inside_group(node, group))
    }
    pub(super) fn compose_node_needed(&self, node: usize) -> bool {
        let mut parent = self.runtime.parents()[node];
        while let Some(p) = parent {
            if let Some(group) = self.scene.layers.iter().position(|s| s.node == p) {
                let layer = &self.layers[group];
                if layer.referenced && layer.base.structure["shader"] == "composelayer" {
                    return self.visible_inside_group(node, group);
                }
            }
            parent = self.runtime.parents()[p];
        }
        false
    }
    pub(super) fn surface_backdrop(&self, surface: &draw::Surface) -> Option<&Buffer> {
        surface
            .group
            .and_then(|g| self.layers[g].compose_backdrop.as_deref())
            .or(self.backdrop.as_deref())
    }
    pub(super) fn surface_texture(
        &self,
        name: &str,
        surface: &draw::Surface,
        prepared: &[u8],
    ) -> Result<Option<&Texture>> {
        if let Some(history) = self.cyclic_base_texture(name, prepared) {
            Ok(Some(history))
        } else if let Some(buffer) = self.forward_backdrop(name, prepared) {
            Ok(Some(&buffer.texture))
        } else if name == "_rt_FullFrameBuffer" {
            Ok(Some(
                self.surface_backdrop(surface)
                    .context("missing compose backdrop")?
                    .texture
                    .as_ref(),
            ))
        } else {
            self.scene_texture(name)
        }
    }
    pub(super) fn surface_buffer(
        &self,
        name: &str,
        surface: &draw::Surface,
        prepared: &[u8],
    ) -> Result<&Buffer> {
        if let Some(buffer) = self.forward_backdrop(name, prepared) {
            Ok(buffer)
        } else if name == "_rt_FullFrameBuffer" {
            self.surface_backdrop(surface)
                .context("missing compose backdrop")
        } else {
            self.scene_buffer(name)
        }
    }
    fn forward_backdrop(&self, name: &str, prepared: &[u8]) -> Option<&Buffer> {
        self.layers.iter().enumerate().find_map(|(i, l)| {
            (prepared[i] != 2
                && l.base.structure["shader"] == "composelayer"
                && l.compose_target.is_none())
            .then(|| l.backdrop_inputs.get(name).map(Rc::as_ref))
            .flatten()
        })
    }
    pub(super) fn snapshot_backdrops(&self, time: f32) {
        for layer in &self.layers {
            if layer.backdrop_time.get() == Some(time) {
                continue;
            }
            for (name, history) in &layer.backdrop_inputs {
                if let Some(source) = self.global_buffers.get(name) {
                    unsafe {
                        self.gl.disable(glow::SCISSOR_TEST);
                        self.gl
                            .bind_framebuffer(glow::READ_FRAMEBUFFER, Some(source.handle));
                        self.gl
                            .bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(history.handle));
                        let [w, h] = history.texture.size.map(|v| v as i32);
                        self.gl.blit_framebuffer(
                            0,
                            0,
                            w,
                            h,
                            0,
                            0,
                            w,
                            h,
                            glow::COLOR_BUFFER_BIT,
                            glow::NEAREST,
                        );
                    }
                }
            }
            layer.backdrop_time.set(Some(time));
        }
    }
    pub(super) fn cyclic_base_texture(&self, name: &str, prepared: &[u8]) -> Option<&Texture> {
        self.layers.iter().enumerate().find_map(|(i, l)| {
            (prepared[i] == 1)
                .then(|| l.backdrop_inputs.get(name).map(|b| b.texture.as_ref()))
                .flatten()
        })
    }
    pub(super) fn reset_feedback(&self) {
        for (spec, layer) in self.scene.layers.iter().zip(&self.layers) {
            if !self.runtime.alive(spec.node) {
                continue;
            }
            layer.cached.set(false);
            layer.backdrop_time.set(None);
            for history in layer.backdrop_inputs.values() {
                self.bind(history, true);
            }
            for effect in &layer.effects {
                effect.reset_feedback(&self.gl);
            }
            for name in &layer.published {
                if let Some(buffer) = self.global_buffers.get(name) {
                    self.bind(buffer, true);
                }
            }
        }
    }
    pub(super) fn blend_surface(&self, blend: &str, surface: &draw::Surface) {
        self.blend(blend);
        if surface.group.is_some() && blend == "normal" {
            unsafe {
                self.gl.enable(glow::BLEND);
                self.gl
                    .blend_func_separate(glow::SRC_ALPHA, glow::ZERO, glow::ONE, glow::ZERO);
            }
        }
    }
    fn capture_deferred(&self, index: usize, groups: &Groups, surface: &draw::Surface) -> bool {
        self.layers[index].base.structure["shader"] == "composelayer"
            && !groups.containers[index]
            && (groups.owners[self.scene.layers[index].node] != surface.group
                || self.image_positions[index] > surface.position)
    }
    pub(super) fn draw_composed_object(
        &self,
        object: RenderObject,
        groups: &Groups,
        prepared: &mut [u8],
        frame: &mut Frame<'_>,
        surface: &draw::Surface,
    ) -> Result<()> {
        // One explicit work stack handles nested groups and dependency visits.
        // Deep authored hierarchies do not consume the Rust call stack.
        enum Task {
            Object(RenderObject, draw::Surface),
            Image(usize, draw::Surface),
            Group(usize, draw::Surface),
            Finish(usize, draw::Surface),
            Present(RenderObject, draw::Surface),
        }
        let mut tasks = vec![Task::Object(object, *surface)];
        while let Some(task) = tasks.pop() {
            match task {
                Task::Object(object, mut surface) => {
                    let node = self.object_node(object);
                    let position = groups.positions[node];
                    surface.position = position;
                    let names = match object {
                        RenderObject::Image(index) => {
                            if !self.surface_visible(
                                node,
                                self.layers[index].state.visible,
                                &surface,
                            ) && !self.layers[index].referenced
                            {
                                continue;
                            }
                            tasks.push(Task::Present(object, surface));
                            tasks.push(Task::Image(index, surface));
                            continue;
                        }
                        RenderObject::Model(index) => {
                            if !self.surface_visible(
                                node,
                                self.models[index].state.visible,
                                &surface,
                            ) {
                                continue;
                            }
                            self.models[index].references().collect::<Vec<_>>()
                        }
                        RenderObject::Particle(index) => {
                            if !self.surface_visible(
                                node,
                                self.particles[index].state.visible,
                                &surface,
                            ) {
                                continue;
                            }
                            self.particles[index].references().collect::<Vec<_>>()
                        }
                    };
                    tasks.push(Task::Present(object, surface));
                    for name in names.into_iter().rev() {
                        if let Some(index) = self
                            .layers
                            .iter()
                            .position(|l| l.published.iter().any(|n| n == name))
                            && !self.capture_deferred(index, groups, &surface)
                        {
                            tasks.push(Task::Image(index, surface));
                        }
                    }
                }
                Task::Image(index, mut surface) => {
                    surface.position = self.image_positions[index];
                    if prepared[index] != 0 {
                        continue;
                    }
                    prepared[index] = 1;
                    tasks.push(Task::Finish(index, surface));
                    if groups.containers[index] {
                        tasks.push(Task::Group(index, surface));
                    }
                    for &dependency in self.image_dependencies[index].iter().rev() {
                        if !self.capture_deferred(dependency, groups, &surface) {
                            tasks.push(Task::Image(dependency, surface));
                        }
                    }
                }
                Task::Group(index, surface) => {
                    let layer = &self.layers[index];
                    let target = layer
                        .compose_target
                        .as_ref()
                        .context("missing compose target")?;
                    let transform = self.image_transform(layer);

                    let local = Mat4::orthographic_rh_gl(
                        -layer.size[0] / 2.,
                        layer.size[0] / 2.,
                        -layer.size[1] / 2.,
                        layer.size[1] / 2.,
                        -10000.,
                        10000.,
                    ) * if transform.determinant().abs() > 1e-12 {
                        transform.inverse()
                    } else {
                        Mat4::IDENTITY
                    };
                    let mut view = surface.view;
                    view.orthographic = local;
                    view.perspective = local;
                    view.scale = [1.; 2];
                    let output = target.texture.size;
                    let group_surface = draw::Surface {
                        output,
                        target: Some(target.handle),
                        view,
                        clip: [0, 0, output[0] as i32, output[1] as i32],
                        group: Some(index),
                        position: surface.position,
                    };
                    // Composition effects process the captured destination as
                    // well as their children, including transparent child pixels.
                    self.capture_material_backdrop(
                        layer.base.references.iter().filter_map(Option::as_deref),
                        &surface,
                    );
                    let mut inputs = [None; 8];
                    for (slot, name) in layer.base.references.iter().enumerate() {
                        if let Some(name) = name {
                            inputs[slot] = self.surface_texture(name, &surface, prepared)?;
                        }
                    }
                    unsafe {
                        self.gl.disable(glow::SCISSOR_TEST);
                        self.gl.color_mask(true, true, true, true);
                        self.gl.depth_mask(true);
                        self.gl
                            .bind_framebuffer(glow::FRAMEBUFFER, Some(target.handle));
                        self.gl.viewport(0, 0, output[0] as i32, output[1] as i32);
                        self.gl.clear_color(0., 0., 0., 0.);
                        self.gl.clear_depth_f32(1.);
                        self.gl
                            .clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
                    }
                    if transform.determinant().abs() <= 1e-12 {
                        continue;
                    }
                    self.blend("normal");
                    layer.base.draw(
                        &layer.source_mesh,
                        // The group target shares scene coordinates with its
                        // children; the later effect copy handles the UV flip.
                        surface.view.matrix(layer.state.perspective) * transform,
                        frame,
                        &inputs,
                        Vec4::ONE,
                    );
                    for &child in groups.children[index].iter().rev() {
                        tasks.push(Task::Object(child, group_surface));
                    }
                }
                Task::Finish(index, surface) => {
                    self.draw_image_content(
                        index,
                        frame,
                        &surface,
                        groups.containers[index],
                        prepared,
                    )?;
                    prepared[index] = 2;
                }
                Task::Present(object, surface) => match object {
                    RenderObject::Image(index) => {
                        self.present_image(index, frame, &surface, groups.containers[index])?
                    }
                    RenderObject::Model(index) => {
                        let model = &self.models[index];
                        unsafe {
                            self.gl.bind_framebuffer(glow::FRAMEBUFFER, surface.target);
                            self.gl.viewport(
                                0,
                                0,
                                surface.output[0] as i32,
                                surface.output[1] as i32,
                            );
                        }
                        self.capture_material_backdrop(model.references(), &surface);
                        frame.appearance = None;
                        frame.composite = surface.group.is_some();
                        frame.projection =
                            surface.view.matrix(model.state.perspective) * model.state.transform;
                        model.draw(
                            &self.gl,
                            surface.view.matrix(model.state.perspective),
                            surface.view.eye,
                            frame,
                            |name| self.surface_texture(name, &surface, prepared),
                        )?;
                    }
                    RenderObject::Particle(index) => {
                        let particle = &self.particles[index];
                        unsafe {
                            self.gl.bind_framebuffer(glow::FRAMEBUFFER, surface.target);
                            self.gl.viewport(
                                0,
                                0,
                                surface.output[0] as i32,
                                surface.output[1] as i32,
                            );
                        }
                        self.capture_material_backdrop(particle.references(), &surface);
                        frame.appearance = None;
                        frame.composite = surface.group.is_some();
                        frame.projection =
                            surface.view.matrix(particle.perspective()) * particle.state.transform;
                        particle.draw(
                            &self.gl,
                            frame,
                            |name| self.surface_texture(name, &surface, prepared),
                            surface.view,
                        )?;
                    }
                },
            }
        }
        Ok(())
    }

    pub(super) fn prepare_references(
        &self,
        added: &[Layer],
        references: &HashSet<String>,
        mut globals: HashMap<String, Rc<Buffer>>,
        budget: &mut u64,
    ) -> Result<Promotions> {
        let mut scratch = self
            .layers
            .iter()
            .chain(added)
            .filter_map(|l| {
                l.buffers.as_ref().map(|b| {
                    (
                        (b[0].texture.size, b[0].texture.format, l.effects.is_empty()),
                        b.clone(),
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        let mut layers = Vec::new();
        let mut histories = Vec::new();
        for (index, layer) in self.layers.iter().chain(added).enumerate() {
            if !layer.published.iter().any(|name| references.contains(name)) {
                continue;
            }
            let size = layer.size.map(|v| v.ceil() as u32);
            let buffers = if layer.buffers.is_some() {
                None
            } else {
                Some(creation::scratch_buffers(
                    self.gl.clone(),
                    size,
                    if self.general.post.hdr {
                        glow::RGBA16F
                    } else {
                        glow::RGBA8
                    },
                    layer.effects.is_empty(),
                    Some(&mut scratch),
                    budget,
                )?)
            };
            for name in layer
                .published
                .iter()
                .filter(|n| references.contains(*n) || *n == &layer.published[0])
            {
                if !globals.contains_key(name) {
                    let alias = layer.published[..2]
                        .iter()
                        .filter(|_| name != &layer.published[2])
                        .find_map(|alias| globals.get(alias).cloned());
                    let buffer = match alias {
                        Some(buffer) => buffer,
                        None => self.create_buffer(size, budget)?,
                    };
                    globals.insert(name.clone(), buffer);
                }
                if layer.base.structure["shader"] == "composelayer"
                    && !layer.backdrop_inputs.contains_key(name)
                {
                    histories.push((index, name.clone(), self.create_buffer(size, budget)?));
                }
            }
            layers.push((index, buffers));
        }
        for name in references {
            ensure!(
                name == "_rt_FullFrameBuffer"
                    || name == "_rt_imageLayerComposite"
                    || name == "_rt_Reflection"
                    || name.starts_with("_system$")
                    || globals.contains_key(name),
                "unknown scene texture {name}"
            );
        }
        let (dependencies, backdrops) =
            draw::dependencies(self.layers.iter().chain(added), &globals)?;
        // A base material has no effect-chain input to use on an active cyclic
        // edge. Keep its preceding logical-frame publication instead. Allocate
        // only those cyclic names (and compose captures above), inside the same
        // promotion transaction and GPU budget as the producer itself.
        let all = self.layers.iter().chain(added).collect::<Vec<_>>();
        for (consumer, layer) in all.iter().enumerate() {
            for name in layer.base.references.iter().flatten() {
                let Some(producer) = all.iter().position(|l| l.published.contains(name)) else {
                    continue;
                };
                let mut pending = vec![producer];
                let mut visited = vec![false; all.len()];
                let mut cyclic = false;
                while let Some(node) = pending.pop() {
                    if node == consumer {
                        cyclic = true;
                        break;
                    }
                    if !visited[node] {
                        visited[node] = true;
                        pending.extend(&dependencies[node]);
                    }
                }
                if cyclic
                    && !all[producer].backdrop_inputs.contains_key(name)
                    && !histories
                        .iter()
                        .any(|(i, n, _)| *i == producer && n == name)
                {
                    histories.push((
                        producer,
                        name.clone(),
                        self.create_buffer(globals[name].texture.size, budget)?,
                    ));
                }
            }
        }
        Ok(Promotions {
            layers,
            globals,
            histories,
            dependencies,
            backdrops,
            capture_backdrop: references.contains("_rt_FullFrameBuffer"),
            capture_reflection: references.contains("_rt_Reflection"),
        })
    }
    pub(super) fn commit_references(&mut self, promotions: Promotions) {
        for (index, name, buffer) in promotions.histories {
            self.layers[index].backdrop_inputs.insert(name, buffer);
            self.layers[index].backdrop_time.set(None);
        }
        for (index, buffers) in promotions.layers {
            let layer = &mut self.layers[index];
            layer.referenced = true;
            if let Some(buffers) = buffers {
                layer.buffers = Some(buffers);
                layer.cached.set(false);
            }
        }
        self.global_buffers = promotions.globals;
        self.image_dependencies = promotions.dependencies;
        self.image_backdrops = promotions.backdrops;
        self.capture_backdrop = promotions.capture_backdrop;
        self.capture_reflection = promotions.capture_reflection;
        if !self.capture_reflection {
            self.reflection = None;
        }
        self.prune_reference_usage();
    }
    pub(super) fn prune_reference_usage(&mut self) {
        let references = self
            .layers
            .iter()
            .flat_map(|l| {
                l.base
                    .references
                    .iter()
                    .flatten()
                    .cloned()
                    .chain(l.effects.iter().flat_map(Effect::scene_references).cloned())
            })
            .chain(
                self.models
                    .iter()
                    .flat_map(mdl::Renderer::references)
                    .map(str::to_owned),
            )
            .chain(
                self.particles
                    .iter()
                    .flat_map(particles::Renderer::references)
                    .map(str::to_owned),
            )
            .collect::<HashSet<_>>();
        let mut outputs = HashSet::new();
        for layer in &mut self.layers {
            layer.referenced = layer.published.iter().any(|name| references.contains(name));
            if layer.referenced {
                outputs.insert(layer.published[0].clone());
            } else if layer.effects.is_empty()
                && layer.state.blend_mode == 0
                && !(layer.base.structure["shader"] == "passthrough"
                    && layer.base.references.first().and_then(Option::as_deref)
                        == Some("_rt_FullFrameBuffer"))
                && layer.base.structure["shader"] != "composelayer"
            {
                layer.buffers = None;
            }
        }
        self.global_buffers.retain(|name, _| {
            !name.starts_with("_rt_imageLayer")
                || references.contains(name)
                || outputs.contains(name)
        });
        for layer in &mut self.layers {
            layer
                .backdrop_inputs
                .retain(|name, _| self.global_buffers.contains_key(name));
        }
        self.capture_backdrop = references.contains("_rt_FullFrameBuffer");
        self.capture_reflection = references.contains("_rt_Reflection");
        if !self.capture_backdrop {
            self.backdrop = None;
        }
        if !self.capture_reflection {
            self.reflection = None;
        }
    }
    pub(super) fn capture_material_backdrop<'a>(
        &self,
        references: impl Iterator<Item = &'a str>,
        surface: &draw::Surface,
    ) {
        if !references.into_iter().any(|n| n == "_rt_FullFrameBuffer") {
            return;
        }
        if let Some(backdrop) = self.surface_backdrop(surface) {
            unsafe {
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, surface.target);
                self.gl
                    .bind_texture(glow::TEXTURE_2D, Some(backdrop.texture.handle));
                self.gl.copy_tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    0,
                    0,
                    surface.output[0] as i32,
                    surface.output[1] as i32,
                );
            }
        }
    }
}

pub(super) struct Promotions {
    pub layers: Vec<(usize, Option<[Rc<Buffer>; 2]>)>,
    pub globals: HashMap<String, Rc<Buffer>>,
    pub histories: Vec<(usize, String, Rc<Buffer>)>,
    dependencies: Vec<Vec<usize>>,
    backdrops: Vec<bool>,
    capture_backdrop: bool,
    capture_reflection: bool,
}
