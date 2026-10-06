use crate::{
    assets::{AssetKey, Assets},
    gpu::{Buffer, Pass, Texture},
    scene,
    scene::bindings::{Properties, resolve},
};
use anyhow::{Context, Result, ensure};
use glow::HasContext;
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};
pub(crate) struct Effect {
    pub visible: bool,
    pub buffers: HashMap<String, History>,
    pub output_history: Option<Rc<Buffer>>,
    pub cache: Option<Rc<Buffer>>,
    pub cached: std::cell::Cell<bool>,
    pub steps: Vec<Step>,
    scene_references: Vec<String>,
}
pub(crate) struct History {
    // Empty uses the current effect output after its earlier contents die.
    // Two buffers retain temporal feedback and never enter the transient pool.
    buffers: Vec<Rc<Buffer>>,
    front: std::cell::Cell<usize>,
}
pub(crate) struct Resize {
    buffers: HashMap<String, History>,
    output_history: Option<Rc<Buffer>>,
    cache: Option<Rc<Buffer>>,
}
impl History {
    pub fn read<'a>(&'a self, output: &'a Buffer) -> &'a Buffer {
        self.buffers
            .get(self.front.get())
            .map_or(output, Rc::as_ref)
    }
    pub fn write<'a>(&'a self, output: &'a Buffer) -> &'a Buffer {
        self.buffers
            .get(if self.buffers.len() == 2 {
                1 - self.front.get()
            } else {
                0
            })
            .map_or(output, Rc::as_ref)
    }
    pub fn commit(&self) {
        if self.buffers.len() == 2 {
            self.front.set(1 - self.front.get());
        }
    }
}
pub(crate) enum Step {
    Draw {
        pass: Box<Pass>,
        target: Route,
        blend: String,
        inputs: Vec<(usize, Route)>,
        clear: bool,
    },
    Copy {
        source: Route,
        target: Route,
        clear: bool,
    },
}
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum Route {
    Input,
    Output,
    Named(String),
    Scene(String),
}
impl Effect {
    pub(crate) fn scene_references(&self) -> impl Iterator<Item = &String> {
        self.scene_references.iter()
    }
}
fn route(name: &str, named: &HashSet<String>) -> Result<Route> {
    if named.contains(name) {
        return Ok(Route::Named(name.into()));
    }
    if name == "previous"
        || name == "_rt_imageLayerComposite"
        || name.starts_with("_rt_effect_pingpong_a_")
    {
        return Ok(Route::Input);
    }
    if name == "_rt_default" || name.starts_with("_rt_effect_pingpong_b_") {
        return Ok(Route::Output);
    }
    if name == "_rt_FullFrameBuffer"
        || name == "_rt_Reflection"
        || name.starts_with("_rt_imageLayer")
        || name.starts_with("_alias_")
        || name.starts_with("_system$")
    {
        return Ok(Route::Scene(name.into()));
    }
    anyhow::bail!("unknown effect buffer {name}")
}
fn dimensions(fbo: &serde_json::Value, size: [f32; 2]) -> Result<[u32; 2]> {
    let scale = fbo["scale"].as_f64().unwrap_or(1.);
    let fit = fbo["fit"].as_f64().unwrap_or(0.);
    ensure!(
        scale.is_finite() && scale > 0. && fit.is_finite() && fit >= 0.,
        "invalid FBO scale/fit"
    );
    let factor = if fit > 0. {
        fit / size[0].max(size[1]) as f64
    } else {
        1. / scale
    };
    Ok(size.map(|v| (v as f64 * factor).round().max(1.).min(u32::MAX as f64) as u32))
}

fn allocate_buffers(
    gl: Rc<glow::Context>,
    fbos: &[serde_json::Value],
    surface: ([u32; 2], u32),
    steps: &[Step],
    histories: &HashSet<String>,
    transients: &mut Vec<Rc<Buffer>>,
    budget: &mut u64,
) -> Result<HashMap<String, History>> {
    let (size, inherited_format) = surface;
    let mut lifetimes = HashMap::new();
    for (index, step) in steps.iter().enumerate() {
        let (target, reads) = match step {
            Step::Draw { target, inputs, .. } => (
                target,
                inputs.iter().map(|(_, route)| route).collect::<Vec<_>>(),
            ),
            Step::Copy { target, source, .. } => (target, vec![source]),
        };
        for route in reads.into_iter().chain(std::iter::once(target)) {
            let range = lifetimes.entry(route.clone()).or_insert((index, index));
            range.1 = index;
        }
    }
    let output_first = steps
        .iter()
        .position(|step| match step {
            Step::Draw { target, .. } | Step::Copy { target, .. } => *target == Route::Output,
        })
        .context("effect has no output pass")?;
    let mut definitions = Vec::new();
    for fbo in fbos {
        let name = fbo["name"].as_str().context("FBO name")?;
        let format = match fbo["format"].as_str().unwrap_or("rgba8888") {
            "rgba8888" => glow::RGBA8,
            "rgba_backbuffer" => inherited_format,
            "rg88" => glow::RG8,
            "r8" => glow::R8,
            "r16f" => glow::R16F,
            "rg16f" => glow::RG16F,
            "rgba16f" | "rgba16161616f" => glow::RGBA16F,
            "r32f" => glow::R32F,
            "rg32f" => glow::RG32F,
            "rgba32f" | "rgba32323232f" => glow::RGBA32F,
            format => anyhow::bail!("unsupported effect FBO format {format}"),
        };
        let dimensions = dimensions(fbo, size.map(|v| v as f32))?;
        // Unused declarations still undergo validation, but need no storage.
        if let Some(&(first, last)) = lifetimes.get(&Route::Named(name.into())) {
            definitions.push((first, last, name, dimensions, format));
        }
    }
    definitions.sort_unstable_by_key(|&(first, _, name, _, _)| (first, name));
    let mut slots = transients
        .iter()
        .map(|b| (None, b.clone()))
        .collect::<Vec<_>>();
    let mut output_last = None;
    let mut buffers = HashMap::new();
    for (first, last, name, dimensions, format) in definitions {
        let feedback = histories.contains(name);
        let available = |end: Option<usize>| end.is_none_or(|end| end < first);
        let storage = if !feedback
            && dimensions == size
            && format == inherited_format
            && last < output_first
            && available(output_last)
        {
            output_last = Some(last);
            Vec::new()
        } else if feedback {
            (0..2)
                .map(|_| Buffer::formatted(gl.clone(), dimensions, format, budget).map(Rc::new))
                .collect::<Result<Vec<_>>>()?
        } else if let Some((end, buffer)) = slots.iter_mut().find(|(end, buffer)| {
            available(*end) && buffer.texture.size == dimensions && buffer.texture.format == format
        }) {
            *end = Some(last);
            vec![buffer.clone()]
        } else {
            let buffer = Rc::new(Buffer::formatted(gl.clone(), dimensions, format, budget)?);
            slots.push((Some(last), buffer.clone()));
            transients.push(buffer.clone());
            vec![buffer]
        };
        buffers.insert(
            name.into(),
            History {
                buffers: storage,
                front: std::cell::Cell::new(0),
            },
        );
    }
    Ok(buffers)
}

impl Effect {
    pub fn transient_buffers(&self) -> impl Iterator<Item = &Rc<Buffer>> {
        self.buffers
            .values()
            .filter(|history| history.buffers.len() == 1)
            .flat_map(|history| &history.buffers)
    }
    pub fn framebuffers(&self) -> impl Iterator<Item = &Buffer> {
        self.buffers
            .values()
            .flat_map(|h| h.buffers.iter().map(Rc::as_ref))
            .chain(self.output_history.iter().map(Rc::as_ref))
            .chain(self.cache.iter().map(Rc::as_ref))
    }
    pub fn reset_feedback(&self, gl: &glow::Context) {
        for history in self.buffers.values() {
            history.front.set(0);
        }
        for buffer in self.framebuffers() {
            unsafe {
                gl.disable(glow::SCISSOR_TEST);
                gl.color_mask(true, true, true, true);
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(buffer.handle));
                gl.clear_color(0., 0., 0., 0.);
                gl.clear(glow::COLOR_BUFFER_BIT);
            }
        }
        self.cached.set(false);
    }
    pub fn resources(&self, usage: &mut crate::gpu::Usage) {
        for buffer in self.framebuffers() {
            usage.buffer(buffer);
        }
        for step in &self.steps {
            if let Step::Draw { pass, .. } = step {
                usage.pass(pass);
            }
        }
    }
    /// Allocate all replacements before committing them. Resizing invalidates
    /// framebuffer history; shader programs and material bindings survive.
    pub fn resized(
        &self,
        gl: Rc<glow::Context>,
        definition: &scene::Effect,
        surface: &Buffer,
        transients: &mut Vec<Rc<Buffer>>,
        budget: &mut u64,
    ) -> Result<Resize> {
        let size = surface.texture.size;
        let histories = self
            .buffers
            .iter()
            .filter(|(_, h)| h.buffers.len() == 2)
            .map(|(name, _)| name.clone())
            .collect();
        let buffers = allocate_buffers(
            gl.clone(),
            &definition.fbos,
            (size, surface.texture.format),
            &self.steps,
            &histories,
            transients,
            budget,
        )?;
        let mut replace = |old: &Option<Rc<Buffer>>| -> Result<Option<Rc<Buffer>>> {
            old.as_ref()
                .map(|old| {
                    Buffer::formatted(gl.clone(), size, old.texture.format, budget).map(Rc::new)
                })
                .transpose()
        };
        let output_history = replace(&self.output_history)?;
        let cache = if self
            .cache
            .as_ref()
            .zip(self.output_history.as_ref())
            .is_some_and(|(a, b)| Rc::ptr_eq(a, b))
        {
            output_history.clone()
        } else {
            replace(&self.cache)?
        };
        Ok(Resize {
            buffers,
            output_history,
            cache,
        })
    }
    pub fn commit_resize(&mut self, resize: Resize) {
        self.buffers = resize.buffers;
        self.output_history = resize.output_history;
        self.cache = resize.cache;
        self.cached.set(false);
    }
    pub fn load(
        gl: Rc<glow::Context>,
        assets: &Assets,
        definition: &scene::Effect,
        properties: &Properties,
        textures: &mut HashMap<AssetKey, Rc<Texture>>,
        budget: &mut u64,
        buffers: (&Buffer, &mut Vec<Rc<Buffer>>),
    ) -> Result<Self> {
        let (surface, transients) = buffers;
        let size = surface.texture.size.map(|v| v as f32);
        let inherited_format = surface.texture.format;
        let scoped = assets.effect(&definition.file)?;
        let visible = scene::visible(&resolve(&definition.visible, properties)?)?;
        let mut named = HashSet::new();
        for fbo in &definition.fbos {
            let name = fbo["name"].as_str().context("FBO name")?;
            ensure!(
                !name.is_empty() && named.insert(name.into()),
                "duplicate/empty FBO name"
            );
            ensure!(
                name != "previous"
                    && name != "_rt_default"
                    && !name.starts_with("_rt_effect_pingpong_"),
                "reserved FBO name"
            );
        }
        let mut steps = Vec::new();
        let mut written = HashSet::from([Route::Input]);
        let mut output_feedback = false;
        let mut named_feedback = false;
        let mut history_buffers = HashSet::new();
        for step in &definition.steps {
            let (compiled, target, reads) = match step {
                scene::Step::Copy { source, target } => {
                    let source = route(source, &named)?;
                    let target = route(target, &named)?;
                    (
                        Step::Copy {
                            source: source.clone(),
                            target: target.clone(),
                            clear: !written.contains(&target),
                        },
                        target,
                        vec![source],
                    )
                }
                scene::Step::Draw { pass, target, bind } => {
                    let mut spec = pass.clone();
                    // Effects operate on already composed pixels. Their material
                    // g_UserAlpha/g_Color values must not be replaced by layer style.

                    if spec["textures"].is_null() {
                        spec["textures"] = serde_json::json!([]);
                    }
                    for (slot, name) in bind {
                        let textures = spec["textures"].as_array_mut().context("pass textures")?;
                        if textures.len() <= *slot {
                            textures.resize(slot + 1, serde_json::Value::Null);
                        }
                        textures[*slot] = name.clone().into();
                    }
                    let pass = Pass::load(
                        gl.clone(),
                        &scoped,
                        &spec,
                        properties,
                        &named,
                        textures,
                        budget,
                        crate::shader::MaterialDomain::Effect,
                    )?;
                    let target = route(target, &named)?;
                    let mut reads = Vec::new();
                    for name in pass.references.iter().flatten() {
                        reads.push(route(name, &named)?);
                    }
                    if pass.textures[0].is_none() && pass.references[0].is_none() {
                        reads.push(Route::Input);
                    }
                    // Native WE overrides effect materials to overwrite intermediate targets.
                    // Authored layer blending is applied only to the final screen composite.
                    let blend = "normal".to_owned();
                    let inputs = pass
                        .references
                        .iter()
                        .enumerate()
                        .filter_map(|(slot, name)| {
                            name.as_ref().map(|name| Ok((slot, route(name, &named)?)))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    (
                        Step::Draw {
                            pass: Box::new(pass),
                            target: target.clone(),
                            blend,
                            inputs,
                            clear: !written.contains(&target),
                        },
                        target,
                        reads,
                    )
                }
            };
            ensure!(
                target != Route::Input,
                "writing the previous effect input is unsupported"
            );
            ensure!(
                !matches!(target, Route::Scene(_)),
                "writing a shared scene texture is unsupported; declare a local FBO"
            );
            for source in reads {
                if matches!(source, Route::Named(_))
                    && (source == target || !written.contains(&source))
                {
                    named_feedback = true;
                    if let Route::Named(name) = &source {
                        history_buffers.insert(name.clone());
                    }
                }
                if matches!(source, Route::Scene(_) | Route::Named(_)) {
                    continue;
                }
                if source == Route::Output {
                    output_feedback |= source == target || !written.contains(&source);
                    continue;
                }
                ensure!(
                    source != target,
                    "effect samples its own render target; requires native feedback support"
                );
                ensure!(
                    written.contains(&source),
                    "effect reads a buffer before writing it; requires native feedback support"
                );
            }
            written.insert(target);
            steps.push(compiled);
        }
        ensure!(
            written.contains(&Route::Output),
            "effect has no output pass"
        );
        let effect_buffers = allocate_buffers(
            gl.clone(),
            &definition.fbos,
            (surface.texture.size, inherited_format),
            &steps,
            &history_buffers,
            transients,
            budget,
        )?;
        let mut scene_references = HashSet::new();
        for step in &steps {
            let reads = match step {
                Step::Draw { inputs, .. } => {
                    inputs.iter().map(|(_, route)| route).collect::<Vec<_>>()
                }
                Step::Copy { source, .. } => vec![source],
            };
            for route in reads {
                if let Route::Scene(name) = route {
                    scene_references.insert(name.clone());
                }
            }
        }
        let mut scene_references = scene_references.into_iter().collect::<Vec<_>>();
        scene_references.sort_unstable();
        let output_history = if output_feedback {
            Some(Rc::new(Buffer::formatted(
                gl.clone(),
                size.map(|v| v.ceil() as u32),
                inherited_format,
                budget,
            )?))
        } else {
            None
        };
        // The prior output is also the paused-frame cache; retain one copy of those pixels.
        let cache = if let Some(history) = &output_history {
            Some(history.clone())
        } else if named_feedback {
            Some(Rc::new(Buffer::formatted(
                gl.clone(),
                size.map(|v| v.ceil() as u32),
                inherited_format,
                budget,
            )?))
        } else {
            None
        };
        Ok(Self {
            visible,
            buffers: effect_buffers,
            output_history,
            cache,
            cached: std::cell::Cell::new(false),
            steps,
            scene_references,
        })
    }
}
