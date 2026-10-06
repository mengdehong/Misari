//! Structural script changes commit between script updates and rendering.
use super::*;
use serde_json::json;

impl Renderer {
    pub(super) fn transient_buffers(&self) -> Vec<Rc<Buffer>> {
        let mut handles = HashSet::new();
        self.layers
            .iter()
            .flat_map(|layer| &layer.effects)
            .flat_map(Effect::transient_buffers)
            .filter(|buffer| handles.insert(buffer.handle))
            .cloned()
            .collect()
    }
    pub(crate) fn process_structure(&mut self) -> Result<()> {
        self.refresh_custom_models()?;
        if !self.runtime.structure_dirty {
            return Ok(());
        }
        let created = std::mem::take(&mut self.runtime.created)
            .into_iter()
            .filter(|node| self.runtime.alive(*node))
            .collect::<Vec<_>>();
        self.release_destroyed()?;
        if !created.is_empty()
            && let Err(error) = self.load_created(&created)
        {
            self.runtime.reject_created(&created)?;
            self.runtime
                .error(format!("Scene layer creation: {error:#}"));
            self.release_destroyed()?;
        }
        self.update_order();
        self.has_lights = self
            .runtime
            .nodes
            .iter()
            .any(|node| node.alive() && node.light.is_some());
        self.runtime.structure_dirty = false;
        Ok(())
    }
    fn reparse_scene(&self, assets: &Assets) -> Result<scene::Scene> {
        let mut nodes = Vec::new();
        let mut objects = Vec::new();
        for (node, authored) in self.runtime.authored.iter().enumerate() {
            if node == self.scene.settings_node {
                continue;
            }
            let mut object = authored.clone();
            if !self.runtime.alive(node) {
                for key in [
                    "image", "model", "particle", "sound", "text", "camera", "light", "effects",
                ] {
                    object.as_object_mut().unwrap().remove(key);
                }
                object["visible"] = json!(false);
            }
            nodes.push(node);
            objects.push(object);
        }
        let mut scene = scene::Scene::from_value(
            assets,
            &self.properties,
            &json!({"general":self.scene.general,"camera":self.scene.general["cameraTransforms"],"objects":objects}),
        )?;
        for layer in &mut scene.layers {
            layer.node = nodes[layer.node];
        }
        for (node, _) in &mut scene.models {
            *node = nodes[*node];
        }
        for (node, _) in &mut scene.custom_models {
            *node = nodes[*node];
        }
        for (node, _) in &mut scene.particles {
            *node = nodes[*node];
        }
        for node in &mut scene.sounds {
            *node = nodes[*node];
        }
        scene.settings_node = self.scene.settings_node;
        scene.objects = self.runtime.authored.clone();
        Ok(scene)
    }
    fn load_created(&mut self, nodes: &[usize]) -> Result<()> {
        let assets = self
            .assets
            .clone()
            .context("dynamic scene assets are missing")?;
        let mut scene = self.reparse_scene(&assets)?;
        let mut references = scene
            .layers
            .iter()
            .flat_map(|layer| {
                std::iter::once(&layer.base).chain(layer.effects.iter().flat_map(|effect| {
                    effect.steps.iter().filter_map(|step| {
                        if let scene::Step::Draw { pass, .. } = step {
                            Some(pass)
                        } else {
                            None
                        }
                    })
                }))
            })
            .flat_map(texture_references)
            .chain(
                scene
                    .layers
                    .iter()
                    .flat_map(|layer| layer.effects.iter())
                    .flat_map(|effect| effect.steps.iter())
                    .flat_map(|step| {
                        if let scene::Step::Copy { source, target } = step {
                            [source, target]
                                .into_iter()
                                .filter(|name| {
                                    (name.starts_with("_rt_") && *name != "_rt_default")
                                        || name.starts_with("_alias_")
                                })
                                .cloned()
                                .collect::<Vec<_>>()
                        } else {
                            vec![]
                        }
                    }),
            )
            .chain(
                self.runtime
                    .objects
                    .iter()
                    .filter(|o| o["__destroyed"] != true)
                    .flat_map(texture_references),
            )
            .collect::<HashSet<_>>();
        let states = self.runtime.states()?;
        let cameras = self
            .runtime
            .camera
            .prepare_created(&assets, &self.runtime.objects, nodes)?;
        let mut budget = self.resources().bytes;
        let mut textures = self
            .texture_cache
            .iter()
            .filter_map(|(key, texture)| texture.upgrade().map(|t| (key.clone(), t)))
            .collect::<HashMap<_, _>>();
        let mut globals = self.global_buffers.clone();
        let mut transients = self.transient_buffers();
        let mut scratch = self
            .layers
            .iter()
            .filter_map(|layer| {
                layer.buffers.as_ref().map(|b| {
                    (
                        (
                            b[0].texture.size,
                            b[0].texture.format,
                            layer.effects.is_empty(),
                        ),
                        b.clone(),
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        let specs = scene
            .layers
            .iter()
            .filter(|spec| nodes.contains(&spec.node))
            .collect::<Vec<_>>();
        let mut loader = LayerLoader {
            gl: self.gl.clone(),
            assets: &assets,
            properties: &self.properties,
            references: &references,
            hdr: self.general.post.hdr,
            textures: &mut textures,
            budget: &mut budget,
            scratch: &mut scratch,
            transients: &mut transients,
            globals: &mut globals,
            text: &mut self.text,
        };
        let layers = specs
            .iter()
            .map(|spec| {
                loader.load(
                    spec,
                    &self.runtime.objects[spec.node],
                    states[spec.node].clone(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let containers = self
            .runtime
            .parents()
            .iter()
            .enumerate()
            .filter_map(|(node, parent)| (self.runtime.alive(node)).then_some(*parent).flatten())
            .collect::<HashSet<_>>();
        // Dynamic candidates conservatively reserve backdrop storage before
        // committing; the draw plan releases it for groups with no consumers.
        let backdrop_needed = references.contains("_rt_FullFrameBuffer")
            || self
                .layers
                .iter()
                .chain(&layers)
                .any(|l| l.state.blend_mode != 0);
        let group_targets = self.candidate_compose_targets(
            self.layers
                .iter()
                .chain(&layers)
                .zip(
                    self.scene
                        .layers
                        .iter()
                        .map(|s| s.node)
                        .chain(specs.iter().map(|s| s.node)),
                )
                .enumerate()
                .filter(|(_, (layer, node))| {
                    containers.contains(node) && layer.base.structure["shader"] == "composelayer"
                })
                .map(|(index, (layer, _))| (index, layer, backdrop_needed, false)),
            !scene.models.is_empty() || !scene.custom_models.is_empty(),
            &mut budget,
        )?;
        let mut models = scene
            .models
            .iter()
            .filter(|(node, _)| nodes.contains(node))
            .map(|(node, model)| {
                mdl::Renderer::load(
                    self.gl.clone(),
                    &assets,
                    model.clone(),
                    &self.runtime.objects[*node],
                    *node,
                    states[*node].clone(),
                    &self.properties,
                    &mut textures,
                    &mut budget,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        for (node, id) in scene
            .custom_models
            .iter()
            .filter(|(node, _)| nodes.contains(node))
        {
            let data = self
                .runtime
                .scripts
                .as_ref()
                .context("custom models require SceneScript")?
                .model_data
                .borrow()
                .retained(*id)?;
            models.push(mdl::Renderer::load_custom(
                self.gl.clone(),
                &assets,
                data,
                &self.runtime.objects[*node],
                *node,
                states[*node].clone(),
                &self.properties,
                &mut textures,
                &mut budget,
            )?);
        }
        let particles = scene
            .particles
            .iter()
            .filter(|(node, _)| nodes.contains(node))
            .map(|(node, file)| {
                particles::Renderer::load(
                    self.gl.clone(),
                    &assets,
                    file,
                    &self.runtime.objects[*node],
                    *node,
                    states[*node].clone(),
                    &self.properties,
                    &mut textures,
                    &mut budget,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        references = self
            .layers
            .iter()
            .chain(&layers)
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
            .collect();
        references.extend(
            models
                .iter()
                .flat_map(mdl::Renderer::references)
                .map(str::to_owned),
        );
        references.extend(
            particles
                .iter()
                .flat_map(particles::Renderer::references)
                .map(str::to_owned),
        );
        let promotions = self.prepare_references(&layers, &references, globals, &mut budget)?;
        ensure!(
            self.particles
                .iter()
                .chain(&particles)
                .map(particles::Renderer::capacity)
                .sum::<usize>()
                <= 100_000,
            "scene particle capacity exceeds 100000"
        );
        let mut usage = self.resources();
        for group in &group_targets {
            usage.buffer(&group.target);
            if let Some(backdrop) = &group.backdrop {
                usage.buffer(backdrop);
            }
        }
        for (_, _, buffer) in &promotions.histories {
            usage.buffer(buffer);
        }
        for layer in &layers {
            layer.resources(&mut usage);
        }
        for (_, buffers) in &promotions.layers {
            for buffer in buffers.iter().flatten() {
                usage.buffer(buffer);
            }
        }
        for buffer in promotions.globals.values() {
            usage.buffer(buffer);
        }
        for model in &models {
            model.resources(&mut usage);
        }
        for particle in &particles {
            particle.resources(&mut usage);
        }
        ensure!(
            usage.bytes <= gpu::GPU_BUDGET,
            "dynamic scene exceeds GPU budget"
        );
        let sounds = std::mem::take(&mut scene.sounds);
        scene.sounds = sounds
            .iter()
            .copied()
            .filter(|node| nodes.contains(node))
            .collect();
        let addition = scene_media::Media::new(
            &assets,
            &scene,
            specs.iter().zip(&layers).map(|(s, l)| (s.node, l)),
            textures.values().cloned(),
        );
        scene.sounds = sounds;
        self.media_system
            .extend(self.gl.clone(), addition, &self.runtime.objects)?;
        self.layers.extend(layers);
        self.commit_compose_targets(group_targets);
        self.models.extend(models);
        self.particles.extend(particles);
        self.commit_references(promotions);
        self.texture_cache = textures
            .iter()
            .map(|(key, texture)| (key.clone(), Rc::downgrade(texture)))
            .collect();
        self.scene = scene;
        self.budget = self.resources().bytes;
        self.runtime.camera.commit_created(cameras);
        Ok(())
    }
    pub(super) fn create_buffer(&self, size: [u32; 2], budget: &mut u64) -> Result<Rc<Buffer>> {
        Ok(Rc::new(Buffer::formatted(
            self.gl.clone(),
            size,
            if self.general.post.hdr {
                glow::RGBA16F
            } else {
                glow::RGBA8
            },
            budget,
        )?))
    }
    fn release_destroyed(&mut self) -> Result<()> {
        self.runtime.camera.release_destroyed(&self.runtime.objects);
        let alive = |node: usize| self.runtime.alive(node);
        if self.scene.layers.iter().all(|l| alive(l.node))
            && self.particles.iter().all(|p| alive(p.index))
            && self.models.iter().all(|m| alive(m.index))
            && self.scene.sounds.iter().all(|node| alive(*node))
        {
            return Ok(());
        }
        let specs = std::mem::take(&mut self.scene.layers);
        let layers = std::mem::take(&mut self.layers);
        for (spec, layer) in specs.into_iter().zip(layers) {
            if alive(spec.node) {
                self.scene.layers.push(spec);
                self.layers.push(layer);
            }
        }
        self.particles.retain(|p| alive(p.index));
        let emission_sources = self
            .particles
            .iter()
            .flat_map(|p| p.image_requests().map(|(id, _)| id.to_owned()))
            .collect::<HashSet<_>>();
        for layer in &mut self.layers {
            if layer
                .emission
                .as_ref()
                .is_some_and(|cache| !emission_sources.contains(&cache.source))
            {
                layer.emission = None;
                if let Some(rig) = &mut layer.puppet {
                    rig.release_emission_positions();
                }
            }
        }
        self.scene.particles.retain(|(node, _)| alive(*node));
        self.models.retain(|m| alive(m.index));
        self.scene.models.retain(|(node, _)| alive(*node));
        self.scene.custom_models.retain(|(node, _)| alive(*node));
        self.scene.sounds.retain(|node| alive(*node));
        self.prune_reference_usage();
        let usage = self.resources_without_media();
        self.media_system
            .release_destroyed(&self.runtime.objects, &usage.textures);
        self.texture_cache
            .retain(|_, texture| texture.strong_count() > 0);
        let (dependencies, backdrops) =
            draw::dependencies(self.layers.iter(), &self.global_buffers)?;
        self.image_dependencies = dependencies;
        self.image_backdrops = backdrops;
        if self.models.is_empty() && !self.general.post.enabled() {
            self.model_target = None;
        }
        self.budget = self.resources().bytes;
        Ok(())
    }
    pub(crate) fn update_order(&mut self) {
        let rank = self.runtime.nodes[self.scene.settings_node]
            .layer_order
            .iter()
            .copied()
            .enumerate()
            .map(|(rank, node)| (node, rank))
            .collect::<HashMap<_, _>>();
        let mut order = self
            .scene
            .layers
            .iter()
            .enumerate()
            .map(|(i, s)| (s.node, RenderObject::Image(i)))
            .chain(
                self.particles
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (p.index, RenderObject::Particle(i))),
            )
            .chain(
                self.models
                    .iter()
                    .enumerate()
                    .map(|(i, m)| (m.index, RenderObject::Model(i))),
            )
            .collect::<Vec<_>>();
        order.sort_by_key(|(node, _)| {
            (rank.get(node).copied().unwrap_or(rank.len() + *node), *node)
        });
        self.image_positions = vec![0; self.layers.len()];
        for (position, (_, object)) in order.iter().enumerate() {
            if let RenderObject::Image(index) = object {
                self.image_positions[*index] = position;
            }
        }
        self.order = order.into_iter().map(|(_, object)| object).collect();
    }
    fn resources_without_media(&self) -> gpu::Usage {
        let mut usage = gpu::Usage::default();
        for layer in &self.layers {
            layer.resources(&mut usage);
        }
        for particle in &self.particles {
            particle.resources(&mut usage);
        }
        for model in &self.models {
            model.resources(&mut usage);
        }
        if let Some(shadows) = &self.shadows {
            shadows.resources(&mut usage);
        }
        for buffer in self.global_buffers.values() {
            usage.buffer(buffer);
        }
        for buffer in self.backdrop.iter() {
            usage.buffer(buffer);
        }
        for buffer in self.model_target.iter().chain(&self.reflection) {
            usage.buffer(buffer);
        }
        if let Some(postprocess) = &self.postprocess {
            postprocess.resources(&mut usage);
        }
        usage.pass(&self.copy);
        if let Some(pass) = &self.color_blend {
            usage.pass(pass);
        }
        usage.mesh(&self.quad);
        usage
    }
    pub(crate) fn resources(&self) -> gpu::Usage {
        let mut usage = self.resources_without_media();
        self.media_system.resources(&mut usage);
        usage
    }

    pub(super) fn refresh_custom_models(&mut self) -> Result<()> {
        let affected = self
            .models
            .iter()
            .enumerate()
            .filter_map(|(index, model)| model.needs_custom_reload().then_some(index))
            .collect::<Vec<_>>();
        if affected.is_empty() {
            return Ok(());
        }
        let assets = self
            .assets
            .clone()
            .context("custom model assets are missing")?;
        let mut textures = self
            .texture_cache
            .iter()
            .filter_map(|(k, v)| v.upgrade().map(|v| (k.clone(), v)))
            .collect::<HashMap<_, _>>();
        let mut budget = self.resources().bytes;
        let mut prepare = || -> Result<_> {
            let states = self.runtime.states()?;
            let mut replacements = Vec::new();
            for &index in &affected {
                let previous = &self.models[index];
                let data = previous.custom_data().unwrap();
                let mut object = self.runtime.objects[previous.index].clone();
                let mut materials = previous.custom_materials();
                for (slot, shape) in data.borrow().shapes.iter().enumerate() {
                    if let Some(shape) = shape
                        && previous.same_custom_material(slot, &shape.material)
                        && !object["__materials"][slot].is_null()
                    {
                        materials[slot] = object["__materials"][slot].clone();
                    }
                }
                object["__materials"] = materials.clone();
                let renderer = mdl::Renderer::load_custom(
                    self.gl.clone(),
                    &assets,
                    data,
                    &object,
                    previous.index,
                    states[previous.index].clone(),
                    &self.properties,
                    &mut textures,
                    &mut budget,
                )?;
                replacements.push((index, renderer, materials));
            }
            let references = self
                .layers
                .iter()
                .flat_map(|layer| layer.base.references.iter().flatten().cloned())
                .chain(
                    self.layers
                        .iter()
                        .flat_map(|layer| layer.effects.iter())
                        .flat_map(Effect::scene_references)
                        .cloned(),
                )
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
                .chain(
                    replacements
                        .iter()
                        .flat_map(|(_, model, _)| model.references())
                        .map(str::to_owned),
                )
                .collect::<HashSet<_>>();
            let promotions = self.prepare_references(
                &[],
                &references,
                self.global_buffers.clone(),
                &mut budget,
            )?;
            let mut usage = self.resources();
            for (_, _, buffer) in &promotions.histories {
                usage.buffer(buffer);
            }
            for buffer in promotions.globals.values() {
                usage.buffer(buffer);
            }
            for (_, buffers) in &promotions.layers {
                for buffer in buffers.iter().flatten() {
                    usage.buffer(buffer);
                }
            }
            for (_, renderer, _) in &replacements {
                renderer.resources(&mut usage);
            }
            ensure!(
                usage.bytes <= gpu::GPU_BUDGET,
                "custom replacement exceeds GPU budget"
            );
            Ok((replacements, promotions))
        };
        match prepare() {
            Ok((replacements, promotions)) => {
                for (index, renderer, materials) in replacements {
                    let node = renderer.index;
                    self.runtime.objects[node]["__materials"] = materials.clone();
                    self.runtime.authored[node]["__materials"] = materials;
                    self.models[index] = renderer;
                }
                self.commit_references(promotions);
                if let Some(scripts) = &self.runtime.scripts
                    && let Err(error) = scripts.sync_nodes(&self.runtime.objects)
                {
                    self.runtime
                        .error(format!("ModelData material host: {error:#}"));
                }
                self.texture_cache = textures
                    .iter()
                    .map(|(key, value)| (key.clone(), Rc::downgrade(value)))
                    .collect();
                self.budget = self.resources().bytes;
            }
            Err(error) => {
                for &index in &affected {
                    self.models[index].acknowledge_custom_reload();
                }
                self.runtime
                    .error(format!("ModelData replacement: {error:#}"));
            }
        }
        Ok(())
    }
}
impl Layer {
    fn resources(&self, usage: &mut gpu::Usage) {
        usage.pass(&self.base);
        usage.mesh(&self.source_mesh);
        usage.mesh(&self.effect_mesh);
        for buffer in self.buffers.iter().flatten() {
            usage.buffer(buffer);
        }
        if let Some(buffer) = &self.compose_target {
            usage.buffer(buffer);
        }
        if let Some(buffer) = &self.compose_backdrop {
            usage.buffer(buffer);
        }
        for buffer in self.backdrop_inputs.values() {
            usage.buffer(buffer);
        }
        for effect in &self.effects {
            effect.resources(usage);
        }
    }
}

pub(super) type Scratch = HashMap<([u32; 2], u32, bool), [Rc<Buffer>; 2]>;

pub(super) fn scratch_buffers(
    gl: Rc<glow::Context>,
    size: [u32; 2],
    format: u32,
    single: bool,
    scratch: Option<&mut Scratch>,
    budget: &mut u64,
) -> Result<[Rc<Buffer>; 2]> {
    let existing = scratch.as_deref().and_then(|pool| {
        pool.get(&(size, format, false))
            .or_else(|| pool.get(&(size, format, true)))
    });
    let first = match existing {
        Some(buffers) => buffers[0].clone(),
        None => Rc::new(Buffer::formatted(gl.clone(), size, format, budget)?),
    };
    let second = if single {
        first.clone()
    } else if let Some(buffers) = existing.filter(|b| b[0].handle != b[1].handle) {
        buffers[1].clone()
    } else {
        Rc::new(Buffer::formatted(gl, size, format, budget)?)
    };
    let buffers = [first, second];
    if let Some(pool) = scratch {
        pool.insert((size, format, single), buffers.clone());
    }
    Ok(buffers)
}

pub(super) struct LayerLoader<'a> {
    pub gl: Rc<glow::Context>,
    pub assets: &'a Assets,
    pub properties: &'a Properties,
    pub references: &'a HashSet<String>,
    pub hdr: bool,
    pub textures: &'a mut HashMap<assets::AssetKey, Rc<Texture>>,
    pub budget: &'a mut u64,
    pub scratch: &'a mut Scratch,
    pub transients: &'a mut Vec<Rc<Buffer>>,
    pub globals: &'a mut HashMap<String, Rc<Buffer>>,
    pub text: &'a mut Option<text::TextSystem>,
}
impl LayerLoader<'_> {
    pub fn load(
        &mut self,
        layer: &scene::Layer,
        object: &serde_json::Value,
        state: scene::State,
    ) -> Result<Layer> {
        let gl = &self.gl;
        let assets = self.assets;
        let properties = self.properties;
        let references = self.references;
        let budget = &mut *self.budget;
        let scratch = &mut *self.scratch;
        let global_buffers = &mut *self.globals;
        let text_system = &mut *self.text;
        let mut state = state;
        let mut text_cache = None;
        let mut offset = [0.0; 2];
        let base = if layer.text {
            let system = text_system.get_or_insert_with(text::TextSystem::new);
            let mut cache = text::TextCache::default();
            let raster = cache.prepare(system, assets, object, layer.size, state.visible)?;
            // Validate hidden text, but defer glyphs and pixel storage until
            // it becomes visible or another layer consumes its pixels.
            let (pixels, size) = if let Some(raster) = raster {
                offset = raster.offset;
                let size = [raster.pixels.width, raster.pixels.height];
                (Some(raster.pixels), size)
            } else {
                (None, [1; 2])
            };
            let mut pass = Pass::compile(gl.clone(), COPY_VERTEX, TEXT_FRAGMENT)?;
            pass.textures = vec![Some(Texture::new(gl.clone(), pixels, size, budget)?)];
            pass.references = vec![None];
            text_cache = Some(cache);
            state.color = text::material_color(object, state.color);
            pass
        } else {
            Pass::load(
                gl.clone(),
                assets,
                &layer.base,
                properties,
                &HashSet::new(),
                self.textures,
                budget,
                crate::shader::MaterialDomain::Image,
            )?
        };
        let first = base.textures[0].as_ref();
        let uv = first.map_or([1.; 2], |first| {
            [
                first.content[0] as f32 / first.size[0] as f32,
                first.content[1] as f32 / first.size[1] as f32,
            ]
        });
        let render_size = if layer.text {
            first.context("text has no texture")?.size.map(|v| v as f32)
        } else {
            layer.size
        };
        let size = render_size.map(|v| v.ceil() as u32);
        let id = object["id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| object["id"].to_string());
        let names = [
            format!("_rt_imageLayerComposite_{id}_a"),
            format!("_rt_imageLayerComposite_{id}_b"),
            format!("_rt_imageLayerAlbedo_{id}"),
        ];
        let persistent = names.iter().any(|n| references.contains(n));
        let backdrop_passthrough = base.structure["shader"] == "passthrough"
            && base.references.first().and_then(Option::as_deref) == Some("_rt_FullFrameBuffer");
        let buffers = if layer.effects.is_empty()
            && !persistent
            && !backdrop_passthrough
            && object["colorBlendMode"].is_null()
            && base.structure["shader"] != "composelayer"
        {
            None
        } else {
            Some(scratch_buffers(
                gl.clone(),
                size,
                if self.hdr { glow::RGBA16F } else { glow::RGBA8 },
                layer.effects.is_empty(),
                Some(scratch),
                budget,
            )?)
        };
        if persistent {
            for name in names
                .iter()
                .filter(|name| references.contains(*name) || *name == &names[0])
            {
                let alias = names[..2]
                    .iter()
                    .filter(|_| name != &names[2])
                    .find_map(|alias| global_buffers.get(alias).cloned());
                let buffer = match alias {
                    Some(buffer) => buffer,
                    None => Rc::new(Buffer::formatted(
                        gl.clone(),
                        size,
                        if self.hdr { glow::RGBA16F } else { glow::RGBA8 },
                        budget,
                    )?),
                };
                global_buffers.insert(name.clone(), buffer);
            }
        }
        let effects = layer
            .effects
            .iter()
            .map(|definition| {
                Effect::load(
                    gl.clone(),
                    assets,
                    definition,
                    properties,
                    self.textures,
                    budget,
                    (&buffers.as_ref().unwrap()[0], &mut *self.transients),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let (puppet, effect_mesh) = if let Some(model) = &layer.puppet {
            let mut rig = mdl::Rig::new(model.clone())?;
            rig.advance_in_space(
                object["__createdAt"].as_f64().unwrap_or(0.) as f32,
                object,
                state.transform,
            )?;
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            for (index, geometry) in model.meshes.iter().enumerate() {
                let offset = (vertices.len() / 14) as u32;
                vertices.extend(rig.vertices(index));
                indices.extend(geometry.indices.iter().map(|i| *i + offset));
            }
            let bytes = (vertices.len() * 4 + indices.len() * 4) as u64;
            ensure!(
                *budget + bytes <= gpu::GPU_BUDGET,
                "scene puppet buffers exceed GPU budget"
            );
            let mesh = Mesh::indexed(gl.clone(), &vertices, &indices)?;
            *budget += bytes;
            (Some(rig), mesh)
        } else {
            // Direct draws sample the asset's content rectangle; completed
            // effect buffers have no TEX padding and use the full rectangle.
            let uv = if buffers.is_none() { uv } else { [1.; 2] };
            (None, Mesh::new(gl.clone(), render_size, uv, true)?)
        };
        Ok(Layer {
            fullscreen: layer.fullscreen,
            size: render_size,
            offset,
            text: text_cache,
            puppet,
            state,
            base,
            effects,
            buffers,
            compose_target: None,
            compose_backdrop: None,
            backdrop_inputs: HashMap::new(),
            backdrop_time: std::cell::Cell::new(None),
            // Keep assets and intermediate buffers in WE's top-first UV convention.
            // Flip once when compositing the completed layer onto the GLES output.
            source_mesh: Mesh::new(gl.clone(), render_size, uv, backdrop_passthrough)?,
            effect_mesh,
            blend: blend_name(&layer.base)?,
            referenced: persistent,
            published: names,
            cached: std::cell::Cell::new(false),
            emission: None,
        })
    }
}
