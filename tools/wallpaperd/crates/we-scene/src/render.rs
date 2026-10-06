//! Scene renderer, owned GPU state, frame updates and public operations.
mod compose;
mod creation;
mod draw;

use crate::assets::Assets;
use crate::effect::{Effect, Route, Step};
use crate::gpu::{Buffer, Frame, Mesh, Pass, Texture, blend_name};
use crate::scene::bindings::{Properties, components, resolve};
use crate::script::ScriptStorage;
use crate::{
    Fit, assets, audio, camera, gpu, lighting, mdl, particles, postprocess, scene, scene::bindings,
    scene::runtime, scene_media, scene_media::events as media_events, text,
};
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3, Vec4};
use glow::HasContext;
use rquickjs::{Array, Ctx, IntoJs, IteratorJs, Object, Value as JsValue};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
};
use std::{
    fs::OpenOptions,
    io::{Cursor, Read},
    os::unix::fs::OpenOptionsExt,
};

pub struct Renderer {
    gl: Rc<glow::Context>,
    scene: scene::Scene,
    general: General,
    general_value: serde_json::Value,
    layers: Vec<Layer>,
    image_dependencies: Vec<Vec<usize>>,
    image_backdrops: Vec<bool>,
    image_positions: Vec<usize>,
    particles: Vec<particles::Renderer>,
    models: Vec<mdl::Renderer>,
    puppet_vertices: Vec<f32>,
    shadows: Option<lighting::shadow::Renderer>,
    model_target: Option<Buffer>,
    reflection: Option<Buffer>,
    reflection_plane: Option<Vec4>,
    pending_reflection_plane: Option<Option<Vec4>>,
    capture_reflection: bool,
    postprocess: Option<postprocess::Postprocess>,
    order: Vec<RenderObject>,
    backdrop: Option<Rc<Buffer>>,
    global_buffers: HashMap<String, Rc<Buffer>>,
    capture_backdrop: bool,
    has_lights: bool,
    copy: Pass,
    color_blend: Option<Pass>,
    camera_fade: Option<Pass>,
    quad: Mesh,
    paths: (PathBuf, PathBuf, Option<PathBuf>),
    storage: Option<ScriptStorage>,
    view_cache: Option<([u32; 2], Fit, camera::View)>,
    pointer: [f32; 2],
    filtered: [f32; 2],
    previous: [f32; 2],
    down: bool,
    pointer_buttons: Vec<([f32; 2], bool)>,
    last_time: Option<f32>,
    has_frame: bool,
    last_delta: f32,
    script_clock: Option<wallpaper_media::clock::Timeline>,
    settling: bool,
    paused: bool,
    pending: Option<Update>,
    audio: audio::AudioSnapshot,
    runtime: runtime::Runtime,
    focused: bool,
    media: serde_json::Value,
    pending_media: Option<serde_json::Value>,
    media_texture_dirty: bool,
    media_system: scene_media::Media,
    text: Option<text::TextSystem>,
    assets: Option<Assets>,
    texture_cache: HashMap<assets::AssetKey, std::rc::Weak<Texture>>,
    properties: Properties,
    budget: u64,
}
#[derive(Clone, Copy)]
enum RenderObject {
    Image(usize),
    Particle(usize),
    Model(usize),
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ScriptFrame<'a> {
    time: f64,
    delta: f64,
    time_of_day: Option<f64>,
    screen: [u32; 2],
    #[serde(flatten)]
    input: ScriptPointer,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pointer_events: Vec<ScriptPointer>,
    #[serde(serialize_with = "serialize_audio")]
    audio: Option<&'a audio::AudioSnapshot>,
    poses: Vec<ScriptPose<'a>>,
    animation_events: Vec<ScriptAnimationEvent<'a>>,
    video_events: Vec<serde_json::Value>,
    values: Vec<serde_json::Value>,
    camera_pose: serde_json::Value,
    media_timeline: Option<serde_json::Value>,
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ScriptPointer {
    pointer: [f64; 2],
    locals: Vec<[f64; 3]>,
    screen_pointer: [f64; 2],
    down: bool,
    focused: bool,
    hits: Vec<usize>,
    eligible: Vec<usize>,
}
#[derive(serde::Serialize)]
struct ScriptPose<'a> {
    node: usize,
    time: f64,
    layers: &'a [serde_json::Value],
}
#[derive(serde::Serialize)]
struct ScriptAnimationEvent<'a> {
    node: usize,
    event: &'a serde_json::Value,
}
impl ScriptPointer {
    fn write<'js>(&self, ctx: &Ctx<'js>, object: &Object<'js>) -> rquickjs::Result<()> {
        object.set("pointer", self.pointer.as_slice())?;
        let locals = self
            .locals
            .iter()
            .map(|v| v.as_slice())
            .collect_js::<Array>(ctx)?;
        object.set("locals", locals)?;
        object.set("screenPointer", self.screen_pointer.as_slice())?;
        object.set("down", self.down)?;
        object.set("focused", self.focused)?;
        object.set("hits", self.hits.as_slice())?;
        object.set("eligible", self.eligible.as_slice())
    }
}
impl crate::script::FrameData for ScriptFrame<'_> {
    fn to_frame<'js>(&self, ctx: &Ctx<'js>) -> Result<JsValue<'js>> {
        use crate::script::Json;
        let frame = Object::new(ctx.clone())?;
        frame.set("time", self.time)?;
        frame.set("delta", self.delta)?;
        frame.set(
            "timeOfDay",
            self.time_of_day.map_or_else(
                || JsValue::new_null(ctx.clone()),
                |v| JsValue::new_float(ctx.clone(), v),
            ),
        )?;
        frame.set("screen", self.screen.as_slice())?;
        self.input.write(ctx, &frame)?;
        if !self.pointer_events.is_empty() {
            let events = Array::new(ctx.clone())?;
            for (index, event) in self.pointer_events.iter().enumerate() {
                let object = Object::new(ctx.clone())?;
                event.write(ctx, &object)?;
                events.set(index, object)?;
            }
            frame.set("pointerEvents", events)?;
        }
        let audio = if let Some(audio) = self.audio {
            let object = Object::new(ctx.clone())?;
            object.set("sequence", audio.sequence)?;
            object.set("available", audio.available)?;
            object.set("capturing", audio.capturing)?;
            object.set(
                "device",
                match &audio.device {
                    Some(device) => device.into_js(ctx)?,
                    None => JsValue::new_null(ctx.clone()),
                },
            )?;
            let bands = Array::new(ctx.clone())?;
            for (index, band) in audio.bands.iter().enumerate() {
                let spectrum = Object::new(ctx.clone())?;
                for (name, values) in [
                    ("left", &band.left),
                    ("right", &band.right),
                    ("average", &band.average),
                ] {
                    let values = values
                        .iter()
                        .map(|v| f64::from(*v))
                        .collect_js::<Array>(ctx)?;
                    spectrum.set(name, values)?;
                }
                bands.set(index, spectrum)?;
            }
            object.set("bands", bands)?;
            object.into_value()
        } else {
            JsValue::new_null(ctx.clone())
        };
        frame.set("audio", audio)?;
        let poses = Array::new(ctx.clone())?;
        for (index, pose) in self.poses.iter().enumerate() {
            let object = Object::new(ctx.clone())?;
            object.set("node", pose.node)?;
            object.set("time", pose.time)?;
            object.set(
                "layers",
                pose.layers.iter().map(Json).collect_js::<Array>(ctx)?,
            )?;
            poses.set(index, object)?;
        }
        frame.set("poses", poses)?;
        let events = Array::new(ctx.clone())?;
        for (index, event) in self.animation_events.iter().enumerate() {
            let object = Object::new(ctx.clone())?;
            object.set("node", event.node)?;
            object.set("event", Json(event.event))?;
            events.set(index, object)?;
        }
        frame.set("animationEvents", events)?;
        frame.set(
            "videoEvents",
            self.video_events
                .iter()
                .map(Json)
                .collect_js::<Array>(ctx)?,
        )?;
        frame.set(
            "values",
            self.values.iter().map(Json).collect_js::<Array>(ctx)?,
        )?;
        frame.set("cameraPose", Json(&self.camera_pose))?;
        frame.set(
            "mediaTimeline",
            match &self.media_timeline {
                Some(timeline) => Json(timeline).into_js(ctx)?,
                None => JsValue::new_null(ctx.clone()),
            },
        )?;
        Ok(frame.into_value())
    }
}
// Preserve the old JSON tree's f32 -> f64 conversion. Serializing f32
// directly rounds its decimal representation before JavaScript sees it.
fn serialize_floats<S: serde::Serializer>(
    values: &[f32],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut sequence = serializer.serialize_seq(Some(values.len()))?;
    for value in values {
        sequence.serialize_element(&f64::from(*value))?;
    }
    sequence.end()
}
fn serialize_audio<S: serde::Serializer>(
    audio: &Option<&audio::AudioSnapshot>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeStruct;
    #[derive(serde::Serialize)]
    struct Spectrum<'a> {
        #[serde(serialize_with = "serialize_floats")]
        left: &'a [f32],
        #[serde(serialize_with = "serialize_floats")]
        right: &'a [f32],
        #[serde(serialize_with = "serialize_floats")]
        average: &'a [f32],
    }
    let Some(audio) = audio else {
        return serializer.serialize_none();
    };
    let bands = audio.bands.each_ref().map(|band| Spectrum {
        left: &band.left,
        right: &band.right,
        average: &band.average,
    });
    let mut snapshot = serializer.serialize_struct("AudioSnapshot", 5)?;
    snapshot.serialize_field("sequence", &audio.sequence)?;
    snapshot.serialize_field("available", &audio.available)?;
    snapshot.serialize_field("capturing", &audio.capturing)?;
    snapshot.serialize_field("device", &audio.device)?;
    snapshot.serialize_field("bands", &bands)?;
    snapshot.end()
}
enum Update {
    Reload(Box<Renderer>, Properties),
    Values {
        properties: Properties,
        general: Box<General>,
        states: Vec<scene::State>,
        visibility: Vec<Vec<bool>>,
        values: Vec<Vec<Vec<f32>>>,
        particle_values: Vec<Vec<Vec<f32>>>,
        model_values: Vec<Vec<Vec<Vec<f32>>>>,
    },
}
struct General {
    light_capacity: [usize; 4],
    camera: camera::Camera,
    clear: [f32; 4],
    parallax: bool,
    amount: f32,
    delay: f32,
    influence: f32,
    post: postprocess::Config,
    ambient: Vec3,
    skylight: Vec3,
    shake: [f32; 3],
}
impl Layer {
    fn needs_backdrop(&self, composed: bool) -> bool {
        self.state.blend_mode != 0
            || (!composed
                && self
                    .base
                    .references
                    .iter()
                    .flatten()
                    .any(|n| n == "_rt_FullFrameBuffer"))
            || self
                .effects
                .iter()
                .flat_map(Effect::scene_references)
                .any(|n| n == "_rt_FullFrameBuffer")
    }
}
impl General {
    fn prepare(value: &serde_json::Value, properties: &Properties) -> Result<Self> {
        let value = resolve(value, properties)?;
        let clear = components(&value["clearcolor"], &[0.0; 3], 3)?;
        let scalar = |name: &str, default: f32| -> Result<f32> {
            Ok(components(&value[name], &[default], 1)?[0])
        };
        let delay = scalar("cameraparallaxdelay", 0.0)?;
        ensure!(delay >= 0.0, "negative camera parallax delay");
        Ok(Self {
            light_capacity: lighting::validate_config(&value["lightconfig"])?,
            camera: camera::Camera::prepare(&value)?,
            clear: [clear[0], clear[1], clear[2], 1.0],
            parallax: value["cameraparallax"].as_bool().unwrap_or(false),
            amount: scalar("cameraparallaxamount", 0.5)?,
            delay,
            influence: scalar("cameraparallaxmouseinfluence", 0.5)?,
            post: postprocess::Config::prepare(&value)?,
            ambient: Vec3::from_slice(&components(&value["ambientcolor"], &[1.; 3], 3)?),
            skylight: Vec3::from_slice(&components(&value["skylightcolor"], &[0.; 3], 3)?),
            shake: if value["camerashake"].as_bool() == Some(true) {
                [
                    scalar("camerashakeamplitude", 0.5)?,
                    scalar("camerashakespeed", 3.)?,
                    scalar("camerashakeroughness", 1.)?,
                ]
            } else {
                [0.; 3]
            },
        })
    }
}
pub(crate) struct Layer {
    fullscreen: bool,
    size: [f32; 2],
    offset: [f32; 2],
    text: Option<text::TextCache>,
    puppet: Option<mdl::Rig>,
    state: scene::State,
    pub(crate) base: Pass,
    pub(crate) effects: Vec<Effect>,
    buffers: Option<[Rc<Buffer>; 2]>,
    compose_target: Option<Rc<Buffer>>,
    compose_backdrop: Option<Rc<Buffer>>,
    backdrop_inputs: HashMap<String, Rc<Buffer>>,
    backdrop_time: std::cell::Cell<Option<f32>>,
    source_mesh: Mesh,
    effect_mesh: Mesh,
    blend: String,
    referenced: bool,
    published: [String; 3],
    cached: std::cell::Cell<bool>,
    emission: Option<particles::emission::Cache>,
}
impl Renderer {
    fn scene_buffer(&self, name: &str) -> Result<&Buffer> {
        if name == "_rt_Reflection" {
            self.reflection.as_ref().context("missing scene reflection")
        } else if name == "_rt_FullFrameBuffer" {
            self.backdrop.as_deref().context("missing scene backdrop")
        } else {
            self.global_buffers
                .get(name)
                .map(Rc::as_ref)
                .with_context(|| format!("unknown scene texture {name}"))
        }
    }
    fn scene_texture(&self, name: &str) -> Result<Option<&Texture>> {
        if name.starts_with("_system$") {
            Ok(self.global_buffers.get(name).map(|b| b.texture.as_ref()))
        } else {
            Ok(Some(self.scene_buffer(name)?.texture.as_ref()))
        }
    }
    pub fn ready(&self) -> bool {
        self.particles
            .iter()
            .all(|p| !(p.state.visible || self.compose_node_needed(p.index)) || p.ready())
            && self.media_system.ready()
    }
    pub fn attach_media(
        &mut self,
        context: wallpaper_media::RenderContext,
        wake: &std::os::unix::net::UnixStream,
        playback: wallpaper_media::Playback,
    ) -> Result<()> {
        self.media_system.attach(
            self.gl.clone(),
            context,
            wake,
            playback,
            &self.runtime.objects,
        )
    }
    pub fn media_playback(&mut self, playback: wallpaper_media::Playback) -> Result<()> {
        self.media_system.playback(playback, &self.runtime.objects)
    }
    pub fn synchronize_media(&mut self, clock: wallpaper_media::clock::Timeline) -> Result<()> {
        self.script_clock = Some(clock);
        self.media_system.synchronize(clock)
    }
    pub fn poll_media(&mut self) -> Result<bool> {
        self.media_system.poll()
    }
    pub fn report_swap(&self) {
        self.media_system.report_swap();
    }
    pub fn load(
        gl: Rc<glow::Context>,
        project: &Path,
        package: &Path,
        common: Option<&Path>,
    ) -> Result<Self> {
        Self::load_with_properties(gl, project, package, common, &Properties::new())
    }
    pub fn load_with_properties(
        gl: Rc<glow::Context>,
        project: &Path,
        package: &Path,
        common: Option<&Path>,
        properties: &Properties,
    ) -> Result<Self> {
        Self::load_with_storage(gl, project, package, common, properties, None)
    }
    pub fn load_with_storage(
        gl: Rc<glow::Context>,
        project: &Path,
        package: &Path,
        common: Option<&Path>,
        properties: &Properties,
        storage: Option<ScriptStorage>,
    ) -> Result<Self> {
        Self::load_inner(gl, project, package, common, properties, storage, true)
    }
    fn load_inner(
        gl: Rc<glow::Context>,
        project: &Path,
        package: &Path,
        common: Option<&Path>,
        properties: &Properties,
        storage: Option<ScriptStorage>,
        initialize_scripts: bool,
    ) -> Result<Self> {
        let assets = Assets::open(project, package, common)?;
        let mut scene = scene::Scene::load(&assets, properties)?;
        scene::prepare_texture_animations(&mut scene, &assets)?;
        let runtime = runtime::Runtime::new(
            &assets,
            &scene.objects,
            properties,
            scene.size,
            storage.clone(),
            initialize_scripts,
        )?;
        let states = runtime.states()?;
        let general_value = runtime.objects[scene.settings_node].clone();
        let general = General::prepare(&general_value, &Properties::new())?;
        let mut budget = 0;
        let mut textures = HashMap::new();
        let mut scratch = HashMap::new();
        let mut transients = Vec::new();
        let mut references = scene
            .layers
            .iter()
            .flat_map(|l| {
                std::iter::once(&l.base).chain(l.effects.iter().flat_map(|e| {
                    e.steps.iter().filter_map(|s| match s {
                        scene::Step::Draw { pass, .. } => Some(pass),
                        _ => None,
                    })
                }))
            })
            .flat_map(texture_references)
            .chain(
                scene
                    .layers
                    .iter()
                    .flat_map(|l| l.effects.iter())
                    .flat_map(|e| e.steps.iter())
                    .flat_map(|s| match s {
                        scene::Step::Copy { source, target } => [source, target]
                            .into_iter()
                            .filter(|name| {
                                (name.starts_with("_rt_") && *name != "_rt_default")
                                    || name.starts_with("_alias_")
                            })
                            .cloned()
                            .collect::<Vec<_>>(),
                        _ => vec![],
                    }),
            )
            .chain(scene.objects.iter().flat_map(texture_references))
            .collect::<HashSet<_>>();
        let capture_backdrop = references.contains("_rt_FullFrameBuffer");
        let mut global_buffers = HashMap::new();
        let mut text_system = None;
        let mut loader = creation::LayerLoader {
            gl: gl.clone(),
            assets: &assets,
            properties,
            references: &references,
            hdr: general.post.hdr,
            textures: &mut textures,
            budget: &mut budget,
            scratch: &mut scratch,
            transients: &mut transients,
            globals: &mut global_buffers,
            text: &mut text_system,
        };
        let layers = scene
            .layers
            .iter()
            .map(|layer| {
                loader.load(
                    layer,
                    &runtime.objects[layer.node],
                    states[layer.node].clone(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let copy = Pass::compile(gl.clone(), COPY_VERTEX, COPY_FRAGMENT)?;
        let mut models = scene
            .models
            .iter()
            .map(|(node, model)| {
                mdl::Renderer::load(
                    gl.clone(),
                    &assets,
                    model.clone(),
                    &runtime.objects[*node],
                    *node,
                    states[*node].clone(),
                    properties,
                    &mut textures,
                    &mut budget,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        for (node, id) in &scene.custom_models {
            let data = runtime
                .scripts
                .as_ref()
                .context("custom models require SceneScript")?
                .model_data
                .borrow()
                .retained(*id)?;
            models.push(mdl::Renderer::load_custom(
                gl.clone(),
                &assets,
                data,
                &runtime.objects[*node],
                *node,
                states[*node].clone(),
                properties,
                &mut textures,
                &mut budget,
            )?);
        }
        let particles = scene
            .particles
            .iter()
            .map(|(node, file)| {
                particles::Renderer::load(
                    gl.clone(),
                    &assets,
                    file,
                    &scene.objects[*node],
                    *node,
                    states[*node].clone(),
                    properties,
                    &mut textures,
                    &mut budget,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            particles
                .iter()
                .map(particles::Renderer::capacity)
                .sum::<usize>()
                <= 100_000,
            "scene particle capacity exceeds 100000"
        );
        let mut order = scene
            .layers
            .iter()
            .enumerate()
            .map(|(i, l)| (l.node, RenderObject::Image(i)))
            .chain(
                particles
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (p.index, RenderObject::Particle(i))),
            )
            .chain(
                models
                    .iter()
                    .enumerate()
                    .map(|(i, m)| (m.index, RenderObject::Model(i))),
            )
            .collect::<Vec<_>>();
        order.sort_by_key(|(node, _)| *node);
        let quad = Mesh::new(gl.clone(), [2.0; 2], [1.0; 2], false)?;
        let postprocess = if general.post.enabled() {
            Some(postprocess::Postprocess::load(gl.clone(), &assets)?)
        } else {
            None
        };
        let keep_assets = text_system.is_some()
            || runtime.scripts.is_some()
            || !initialize_scripts
            || bindings::has_binding(&scene.general["hdr"])
            || bindings::has_binding(&scene.general["bloom"])
            || postprocess.is_some();
        let media_system = scene_media::Media::new(
            &assets,
            &scene,
            scene.layers.iter().zip(&layers).map(|(s, l)| (s.node, l)),
            textures.values().cloned(),
        );
        let has_lights = runtime
            .nodes
            .iter()
            .any(|node| node.alive() && node.light.is_some());
        let texture_cache = textures
            .iter()
            .map(|(key, texture)| (key.clone(), Rc::downgrade(texture)))
            .collect();
        let image_positions = (0..layers.len()).collect();
        let mut renderer = Self {
            gl,
            scene,
            general,
            general_value,
            layers,
            image_dependencies: Vec::new(),
            image_backdrops: Vec::new(),
            image_positions,
            particles,
            models,
            puppet_vertices: Vec::new(),
            shadows: None,
            model_target: None,
            reflection: None,
            reflection_plane: None,
            pending_reflection_plane: None,
            capture_reflection: false,
            postprocess,
            order: order.into_iter().map(|(_, object)| object).collect(),
            backdrop: None,
            global_buffers,
            capture_backdrop,
            has_lights,
            copy,
            color_blend: None,
            camera_fade: None,
            quad,
            paths: (
                project.into(),
                package.into(),
                common.map(Path::to_path_buf),
            ),
            storage,
            view_cache: None,
            pointer: [0.5; 2],
            filtered: [0.5; 2],
            previous: [0.5; 2],
            down: false,
            pointer_buttons: Vec::new(),
            last_time: None,
            has_frame: false,
            last_delta: 0.0,
            script_clock: None,
            settling: false,
            paused: false,
            pending: None,
            audio: Default::default(),
            runtime,
            focused: false,
            media: serde_json::Value::Null,
            pending_media: None,
            media_texture_dirty: true,
            media_system,
            assets: keep_assets.then_some(assets),
            texture_cache,
            properties: properties.clone(),
            text: text_system,
            budget,
        };
        references = renderer
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
            .collect();
        references.extend(
            renderer
                .models
                .iter()
                .flat_map(mdl::Renderer::references)
                .map(str::to_owned),
        );
        references.extend(
            renderer
                .particles
                .iter()
                .flat_map(particles::Renderer::references)
                .map(str::to_owned),
        );
        let promotions = renderer.prepare_references(
            &[],
            &references,
            renderer.global_buffers.clone(),
            &mut budget,
        )?;
        renderer.commit_references(promotions);
        renderer.update_order();
        renderer.budget = renderer.resources().bytes;
        let groups = renderer.compose_groups();
        renderer.prepare_compose_targets(&groups)?;
        ensure!(
            renderer.budget <= gpu::GPU_BUDGET,
            "scene exceeds GPU budget"
        );
        renderer.process_structure()?;
        renderer.initial_shadows()?;
        renderer.prepare_color_blending()?;
        Ok(renderer)
    }

    /// Validate the complete update before changing any visible state.
    pub fn set_properties(&mut self, properties: &Properties) -> Result<()> {
        let update = self.prepare_properties(properties)?;
        if self.paused {
            self.pending = Some(update);
        } else {
            self.apply_properties(update);
        }
        Ok(())
    }
    /// Paused scenes accept settings but present them only after resuming.
    pub fn pause(&mut self, paused: bool) -> bool {
        if paused && !self.paused {
            self.cancel_pointer();
        }
        if let Some(clock) = &mut self.script_clock {
            clock.set_running(wallpaper_media::clock::now(), !paused);
        }
        self.paused = paused;
        let mut changed = false;
        if !paused && let Some(plane) = self.pending_reflection_plane.take() {
            self.reflection_plane = plane;
            changed = true;
        }
        if !paused && let Some(media) = self.pending_media.take() {
            self.set_media(media);
            changed = true;
        }
        if let Err(error) = self.media_system.pause(paused, &self.runtime.objects) {
            self.runtime.error(format!("scene media pause: {error:#}"));
        }
        if !paused && let Some(update) = self.pending.take() {
            self.apply_properties(update);
            return true;
        }
        changed
    }
    fn prepare_properties(&self, properties: &Properties) -> Result<Update> {
        let general = General::prepare(&self.scene.general, properties)?;
        let states = self.scene.validate_layout(properties).and_then(|()| {
            let nodes = self.runtime.preview_properties(properties)?;
            Ok(self
                .scene
                .layers
                .iter()
                .map(|layer| nodes[layer.node].clone())
                .collect::<Vec<_>>())
        });
        let mut visibility = Vec::new();
        let mut values = Vec::new();
        let mut rebuild = states.is_err() || general.post.hdr != self.general.post.hdr;
        for (spec, layer) in self.scene.layers.iter().zip(&self.layers) {
            rebuild |= layer.base.changed(properties)?;
            values.push(layer.base.prepare(properties)?);
            let mut visible = Vec::new();
            for (definition, effect) in spec.effects.iter().zip(&layer.effects) {
                visible.push(scene::visible(&resolve(&definition.visible, properties)?)?);
                for step in &effect.steps {
                    if let Step::Draw { pass, .. } = step {
                        rebuild |= pass.changed(properties)?;
                        values.push(pass.prepare(properties)?);
                    }
                }
            }
            visibility.push(visible);
        }
        let mut particle_values = Vec::new();
        for particle in &self.particles {
            rebuild |= particle.changed(properties)?;
            particle_values.push(particle.prepare_properties(properties)?);
        }
        let mut model_values = Vec::new();
        for model in &self.models {
            rebuild |= model.changed(properties)?;
            model_values.push(model.prepare(properties)?);
        }
        if rebuild {
            let mut candidate = Self::load_inner(
                self.gl.clone(),
                &self.paths.0,
                &self.paths.1,
                self.paths.2.as_deref(),
                properties,
                self.storage.clone(),
                false,
            )?;
            candidate.pointer = self.pointer;
            candidate.filtered = self.filtered;
            candidate.previous = self.previous;
            candidate.down = self.down;
            candidate.pointer_buttons.clone_from(&self.pointer_buttons);
            candidate.last_time = self.last_time;
            candidate.last_delta = self.last_delta;
            candidate.script_clock = self.script_clock;
            candidate.settling = self.settling;
            candidate.reflection_plane = self.reflection_plane;
            candidate.pending_reflection_plane = self.pending_reflection_plane;
            candidate.focused = self.focused;
            candidate.audio = self.audio.clone();
            candidate.media = self.media.clone();
            if let Some((context, wake, playback)) = &self.media_system.environment {
                let mut quiet = *playback;
                quiet.paused = true;
                candidate.attach_media(context.clone(), wake, quiet)?;
                candidate.media_system.environment.as_mut().unwrap().2 = *playback;
            }
            return Ok(Update::Reload(Box::new(candidate), properties.clone()));
        }
        Ok(Update::Values {
            properties: properties.clone(),
            general: Box::new(general),
            states: states?,
            visibility,
            values,
            particle_values,
            model_values,
        })
    }
    fn apply_properties(&mut self, update: Update) {
        self.refresh_script_clock();
        let Update::Values {
            properties,
            general,
            states,
            visibility,
            values,
            particle_values,
            model_values,
        } = update
        else {
            if let Update::Reload(mut candidate, properties) = update {
                candidate.reflection_plane = self.reflection_plane;
                candidate.pending_reflection_plane = self.pending_reflection_plane;
                // GPU structure changes keep this instance's module/shared state and timers.
                std::mem::swap(&mut self.runtime, &mut candidate.runtime);
                candidate.refresh_script_clock();
                if let Err(error) = candidate.runtime.properties(&properties) {
                    candidate
                        .runtime
                        .error(format!("SceneScript property update: {error:#}"));
                }
                candidate.general_value = serde_json::Value::Null;
                candidate.runtime.created = (candidate.scene.settings_node + 1
                    ..candidate.runtime.objects.len())
                    .filter(|node| candidate.runtime.alive(*node))
                    .collect();
                candidate.runtime.structure_dirty = true;
                if let Err(error) = candidate.process_structure() {
                    candidate
                        .runtime
                        .error(format!("Scene structure after reload: {error:#}"));
                }
                let rigs = self
                    .models
                    .iter()
                    .filter_map(|m| m.rig.as_ref().map(|r| (m.index, r)))
                    .chain(
                        self.scene
                            .layers
                            .iter()
                            .zip(&self.layers)
                            .filter_map(|(s, l)| l.puppet.as_ref().map(|r| (s.node, r))),
                    )
                    .collect::<HashMap<_, _>>();
                for (node, rig) in candidate
                    .models
                    .iter_mut()
                    .filter_map(|m| m.rig.as_mut().map(|r| (m.index, r)))
                    .chain(
                        candidate
                            .scene
                            .layers
                            .iter()
                            .zip(&mut candidate.layers)
                            .filter_map(|(s, l)| l.puppet.as_mut().map(|r| (s.node, r))),
                    )
                {
                    if let Some(previous) = rigs.get(&node) {
                        rig.inherit_physics(previous);
                    }
                }
                *self = *candidate;
            }
            return;
        };
        self.general = *general;
        self.view_cache = None;
        self.general_value = serde_json::Value::Null;
        if let Err(error) = self.runtime.properties(&properties) {
            self.runtime
                .error(format!("SceneScript property update: {error:#}"));
        }
        // Property events may mutate transforms, color or visibility. Commit
        // those runtime values now, including a pause before the next frame.
        let states = match self.runtime.states() {
            Ok(nodes) => self
                .scene
                .layers
                .iter()
                .map(|layer| nodes[layer.node].clone())
                .collect(),
            Err(error) => {
                self.runtime
                    .error(format!("Scene property state: {error:#}"));
                states
            }
        };
        let mut values = values.into_iter();
        for (particle, values) in self.particles.iter_mut().zip(particle_values) {
            particle.apply_properties(values);
        }
        for (model, values) in self.models.iter_mut().zip(model_values) {
            model.apply(values);
        }
        for ((layer, state), visible) in self.layers.iter_mut().zip(states).zip(visibility) {
            layer.state = state;
            layer.base.apply(values.next().unwrap());
            for (effect, visible) in layer.effects.iter_mut().zip(visible) {
                effect.visible = visible;
                for step in &mut effect.steps {
                    if let Step::Draw { pass, .. } = step {
                        pass.apply(values.next().unwrap());
                    }
                }
            }
        }
        self.properties = properties;
    }
    pub fn accepts_pointer(&self) -> bool {
        self.runtime
            .scripts
            .as_ref()
            .is_some_and(|scripts| scripts.pointer)
            || self.general.parallax
            || self.particles.iter().any(particles::Renderer::pointer)
            || self.models.iter().any(mdl::Renderer::pointer)
            || self.layers.iter().any(|layer| {
                layer.base.accepts_pointer()
                    || layer.effects.iter().any(|effect| {
                        effect.steps.iter().any(
                            |step| matches!(step,Step::Draw {pass,..} if pass.accepts_pointer()),
                        )
                    })
            })
    }
    pub fn requires_audio(&self) -> bool {
        if self.particles.iter().any(|p| p.state.visible && p.audio())
            || self.models.iter().any(|m| m.state.visible && m.audio())
        {
            return true;
        }
        self.runtime.scripts.as_ref().is_some_and(|scripts| scripts.audio) || self.emission_uses_audio() || self.layers.iter().filter(|layer| layer.state.visible || layer.referenced).any(|layer| {
            layer.base.requires_audio() || layer.effects.iter().filter(|effect| effect.visible).any(|effect| {
                effect.steps.iter().any(|step| matches!(step, Step::Draw { pass, .. } if pass.requires_audio()))
            })
        })
    }
    pub fn set_audio(&mut self, snapshot: &audio::AudioSnapshot) {
        if snapshot.valid() {
            self.audio = snapshot.clone();
        }
    }
    pub fn pointer(&mut self, position: Option<[f32; 2]>, button: Option<bool>) {
        if position.is_none() {
            self.cancel_pointer();
        }
        self.focused = position.is_some();
        if let Some(position) = position {
            self.pointer = position;
        }
        if position.is_none() {
            self.down = false;
        } else if let Some(button) = button {
            if button != self.down
                && !self.paused
                && self.runtime.scripts.as_ref().is_some_and(|s| s.pointer)
            {
                self.pointer_buttons.push((self.pointer, button));
            }
            self.down = button;
        }
        self.settling = true;
    }
    fn cancel_pointer(&mut self) {
        self.pointer_buttons.clear();
        if let Some(scripts) = &self.runtime.scripts
            && let Err(error) = scripts.cancel_pointer()
        {
            self.runtime
                .error(format!("SceneScript pointer cancellation: {error:#}"));
        }
    }
    fn script_pointer(
        &self,
        hit_view: camera::View,
        output: [u32; 2],
        screen: [f32; 2],
        down: bool,
        states: &[scene::State],
    ) -> Result<ScriptPointer> {
        let pointer = hit_view.scene_pointer(screen, output, self.scene.size);
        let world = [
            pointer[0] * self.scene.size[0],
            (1.0 - pointer[1]) * self.scene.size[1],
        ];
        let (hits, locals, eligible) = if self.runtime.scripts.as_ref().is_some_and(|s| s.pointer) {
            self.pointer_hits(hit_view, world, screen, states)?
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
        Ok(ScriptPointer {
            pointer: world.map(f64::from),
            locals: locals
                .into_iter()
                .map(|position| position.map(f64::from))
                .collect(),
            screen_pointer: [screen[0] * output[0] as f32, screen[1] * output[1] as f32]
                .map(f64::from),
            down,
            focused: self.focused,
            hits,
            eligible,
        })
    }
    pub fn animated(&self) -> bool {
        if self.media["playback"] == "playing"
            && self
                .runtime
                .scripts
                .as_ref()
                .is_some_and(|s| s.media_timeline)
        {
            return true;
        }
        if self.general.shake[0] != 0. {
            return true;
        }
        if self.media_system.animated() {
            return true;
        }
        if self.runtime.animations.active() {
            return true;
        }
        if self.runtime.camera.animated() {
            return true;
        }
        if self.particles.iter().any(|p| p.state.visible) {
            return true;
        }
        if self.models.iter().any(|m| m.state.visible && m.animated()) {
            return true;
        }
        self.runtime
            .scripts
            .as_ref()
            .is_some_and(|scripts| scripts.animated)
            || self.settling
            || self
                .layers
                .iter()
                .zip(&self.scene.layers)
                .filter(|(layer, _)| layer.state.visible || layer.referenced)
                .any(|(layer, spec)| {
                    layer.puppet.as_ref().is_some_and(mdl::Rig::animated)
                        || layer
                            .base
                            .animated_with_source(self.runtime.nodes[spec.node].texture.playing)
                        || layer
                            .effects
                            .iter()
                            .filter(|effect| effect.visible)
                            .any(|effect| {
                                effect.steps.iter().any(
                                    |step| matches!(step,Step::Draw {pass,..} if pass.animated()),
                                )
                            })
                })
    }
    pub fn diagnostics(&self) -> String {
        self.runtime.diagnostics().join("\n")
    }
    pub fn errors(&self) -> String {
        self.runtime.errors().join("\n")
    }
    pub fn error_generation(&self) -> u64 {
        self.runtime.error_generation()
    }
    pub fn resource_bytes(&self) -> u64 {
        self.resources().bytes
    }
    fn update_materials(&mut self) -> Result<()> {
        if self.runtime.scripts.is_none() && !self.runtime.animations.present() {
            return Ok(());
        }
        for (spec, layer) in self.scene.layers.iter().zip(&mut self.layers) {
            layer.base.apply_constants(
                &self.runtime.objects[spec.node]["__materials"][0][0]["constantshadervalues"],
            )?;
            for (definition, effect) in spec.effects.iter().zip(&mut layer.effects) {
                let raw = &self.runtime.objects[spec.node]["effects"][definition.index];
                effect.visible = scene::visible(&raw["visible"])?;
                let mut index = 0;
                for step in &mut effect.steps {
                    if let Step::Draw { pass, .. } = step {
                        pass.apply_constants(&raw["passes"][index]["constantshadervalues"])?;
                        index += 1;
                    }
                }
            }
        }
        Ok(())
    }
    pub fn set_media(&mut self, media: serde_json::Value) {
        if self.paused {
            self.pending_media = Some(media);
            return;
        }
        if self.media != media {
            self.media_texture_dirty |= self.media["enabled"] != media["enabled"]
                || self.media["artwork"]["path"] != media["artwork"]["path"]
                || self.media["previous_artwork"]["path"] != media["previous_artwork"]["path"];
            self.refresh_script_clock();
            for (event, arg) in media_events::changes(&self.media, &media) {
                self.runtime.event(event, &arg);
            }
            self.media = media;
        }
    }
    fn refresh_script_clock(&mut self) {
        if let (Some(clock), Some(scripts)) = (self.script_clock, &self.runtime.scripts) {
            let time = clock.position(wallpaper_media::clock::now()) as f64 / 1e9;
            if let Err(error) = scripts.set_clock(time) {
                self.runtime
                    .error(format!("SceneScript event clock: {error:#}"));
            }
        }
    }

    fn camera_view(&mut self, output: [u32; 2], fit: Fit) -> camera::View {
        if let Some((size, mode, view)) = self.view_cache
            && size == output
            && mode == fit
        {
            return view;
        }
        let view = self.general.camera.view(self.scene.size, output, fit);
        self.view_cache = Some((output, fit, view));
        view
    }
    fn frame_view(&mut self, output: [u32; 2], fit: Fit, time: f32) -> camera::View {
        let settings = &self.runtime.objects[self.scene.settings_node];
        if (self.general_value.is_null()
            || self.runtime.scripts.is_some()
            || self.runtime.animations.present()
            || self.runtime.camera.present())
            && settings != &self.general_value
        {
            match General::prepare(settings, &Properties::new()) {
                Ok(prepared) => {
                    self.general = prepared;
                    self.view_cache = None;
                    self.general_value = settings.clone();
                }
                Err(error) => self.runtime.error(format!("Scene settings: {error:#}")),
            }
        }
        let mut view = self.camera_view(output, fit);
        if self.general.shake[0] != 0. {
            let [amplitude, speed, roughness] = self.general.shake;
            let noise = |phase: f32| {
                let t = time * speed;
                let k = t.floor();
                let f = t - k;
                let h = |k: f32| (k.mul_add(12.9898, phase).sin() * 43758.547).fract();
                let a = h(k);
                a + (h(k + 1.) - a) * f * f * (3. - 2. * f)
            };
            let offset = Vec3::new(
                noise(2.71) + roughness * 0.5 * noise(9.17),
                noise(5.3) + roughness * 0.5 * noise(13.2),
                0.,
            ) * amplitude;
            view.orthographic *= Mat4::from_translation(offset);
            view.perspective *= Mat4::from_translation(offset);
            view.eye -= offset;
        }
        view
    }
    fn copy_buffer(&self, target: &Buffer, source: &Buffer, frame: &Frame<'_>) {
        if target.handle == source.handle {
            return;
        }
        if target.texture.size == source.texture.size
            && target.texture.format == source.texture.format
        {
            unsafe {
                self.gl.disable(glow::SCISSOR_TEST);
                self.gl
                    .bind_framebuffer(glow::READ_FRAMEBUFFER, Some(source.handle));
                self.gl
                    .bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(target.handle));
                let [w, h] = target.texture.size.map(|v| v as i32);
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
            return;
        }
        self.bind(target, true);
        self.blend("normal");
        self.copy.draw(
            &self.quad,
            Mat4::IDENTITY,
            frame,
            &[
                Some(&source.texture),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ],
            Vec4::ONE,
        );
    }
    pub fn draw(
        &mut self,
        output: [u32; 2],
        target: Option<glow::Framebuffer>,
        fit: Fit,
        time: f32,
    ) -> Result<()> {
        ensure!(
            output.iter().all(|v| *v > 0) && time.is_finite(),
            "invalid render arguments"
        );
        self.process_structure()?;
        if let Err(error) = self.update_media_textures() {
            self.runtime.error(format!("media artwork: {error:#}"));
            self.media_texture_dirty = false;
        }
        let initial_view = self.camera_view(output, fit);
        let pointer = initial_view.scene_pointer(self.pointer, output, self.scene.size);
        let time = if self.paused {
            self.last_time.unwrap_or(time)
        } else {
            time
        };
        if self.runtime.scripts.is_some() {
            self.script_clock = Some(wallpaper_media::clock::Timeline {
                position_ns: (time as f64 * 1e9) as u64,
                sampled_ns: wallpaper_media::clock::now(),
                running: !self.paused,
            });
        }
        let delta = if self.last_time == Some(time) {
            0.0
        } else {
            self.last_time.map_or(0.0, |last| (time - last).max(0.0))
        };
        let prewarming_same_time = self.last_time == Some(time) && !self.ready();
        if self.last_time.is_some_and(|last| time < last) {
            self.reset_feedback();
            self.has_frame = false;
        }
        if self.last_time != Some(time) {
            self.last_delta = delta;
            self.previous = self.filtered;
            let factor = if self.general.delay <= 0.0 {
                1.0
            } else {
                1.0 - (-delta * 100.0_f32.ln() / self.general.delay).exp()
            };
            for (current, target) in self.filtered.iter_mut().zip(pointer) {
                *current += (target - *current) * factor;
                if (target - *current).abs() < 0.00001 {
                    *current = target;
                }
            }
            self.last_time = Some(time);
        }
        self.settling = self.filtered != pointer || self.previous != self.filtered;
        let animation_values = if !self.paused {
            self.runtime.animate(time)?
        } else {
            Vec::new()
        };
        let mut states = self.runtime.local_states()?;
        self.update_skeletons(time, &mut states)?;
        if let Err(error) = self.runtime.update_camera_with_states(time as f64, &states) {
            self.runtime.error(format!("Scene camera: {error:#}"));
        }
        let mut impact = runtime::PatchImpact::default();
        let scripted = !self.paused && !prewarming_same_time && self.runtime.scripts.is_some();
        if scripted {
            let hit_view = self.frame_view(output, fit, time);
            if self.runtime.scripts.as_ref().is_some_and(|s| s.pointer) {
                for model in &mut self.models {
                    model.update_content(&self.runtime.objects[model.index])?;
                }
            }
            let input = self.script_pointer(hit_view, output, self.pointer, self.down, &states)?;
            let pointer_events = std::mem::take(&mut self.pointer_buttons)
                .into_iter()
                .map(|(screen, down)| self.script_pointer(hit_view, output, screen, down, &states))
                .collect::<Result<Vec<_>>>()?;
            let rigs = self
                .scene
                .layers
                .iter()
                .zip(&self.layers)
                .filter_map(|(spec, layer)| layer.puppet.as_ref().map(|rig| (spec.node, rig)))
                .chain(
                    self.models
                        .iter()
                        .filter_map(|model| model.rig.as_ref().map(|rig| (model.index, rig))),
                )
                .collect::<Vec<_>>();
            self.runtime.scripts.as_ref().unwrap().set_poses(
                rigs.iter().map(|&(node, rig)| (node, rig.local.as_slice())),
                &self.runtime.objects,
            );
            let poses = rigs
                .iter()
                .map(|&(node, rig)| ScriptPose {
                    node,
                    time: time as f64,
                    layers: &rig.layers,
                })
                .collect();
            let curve_events = self.runtime.animations.events.clone();
            let animation_events = rigs
                .iter()
                .flat_map(|(node, rig)| {
                    rig.events
                        .iter()
                        .map(move |event| ScriptAnimationEvent { node: *node, event })
                })
                .chain(
                    curve_events
                        .iter()
                        .map(|(node, event)| ScriptAnimationEvent { node: *node, event }),
                )
                .collect::<Vec<_>>();
            let video_events = self.media_system.take_video_events();
            impact = self.runtime.frame(&ScriptFrame {
                time: time as f64,
                delta: delta as f64,
                time_of_day: None,
                screen: output,
                input,
                pointer_events,
                audio: self
                    .runtime
                    .scripts
                    .as_ref()
                    .filter(|s| s.audio)
                    .map(|_| &self.audio),
                poses,
                animation_events,
                video_events,
                values: animation_values,
                camera_pose: self.runtime.objects[self.scene.settings_node]["__cameraPose"].clone(),
                media_timeline: self
                    .runtime
                    .scripts
                    .as_ref()
                    .filter(|s| s.media_timeline)
                    .map(|_| media_events::timeline(&self.media, wallpaper_media::clock::now())),
            });
        }
        let structure_changed = self.runtime.structure_dirty;
        self.process_structure()?;
        if impact.states || impact.skeletons || structure_changed {
            states = self.runtime.local_states()?;
            self.update_skeletons(time, &mut states)?;
        }
        if (scripted || structure_changed)
            && let Err(error) = self.runtime.update_camera_with_states(time as f64, &states)
        {
            self.runtime.error(format!("Scene camera: {error:#}"));
        }
        let view = self.frame_view(output, fit, time);
        let scale = view.scale;
        self.update_text()?;
        if let Err(error) = self.update_materials() {
            self.runtime
                .error(format!("Scene material update: {error:#}"));
        }
        self.prepare_color_blending()?;
        for (spec, layer) in self.scene.layers.iter().zip(&mut self.layers) {
            if let Some(texture) = &layer.base.textures[0] {
                if texture.frames.is_empty() {
                    continue;
                }
                let phase = self.runtime.nodes[spec.node].texture.phase(time);
                layer.source_mesh.animate(texture, phase, false);
                if layer.buffers.is_none() && layer.puppet.is_none() {
                    layer.effect_mesh.animate(texture, phase, true);
                }
            }
        }
        for model in &mut self.models {
            model.update_content(&self.runtime.objects[model.index])?;
        }
        let sound_changes =
            self.media_system
                .advance(time as f64, self.paused, &self.runtime.objects)?;
        if !sound_changes.is_empty() {
            let changes = serde_json::Value::Array(sound_changes);
            self.runtime.apply(&changes)?;
            if let Some(scripts) = &self.runtime.scripts {
                scripts.sync_values(&changes)?;
            }
        }
        self.media_system
            .pause(self.paused, &self.runtime.objects)?;
        if !self.particles.is_empty() {
            let emission_sources = self.emission_sources(view, time, delta, &states)?;
            let mut colliders = HashMap::new();
            if self
                .particles
                .iter()
                .any(particles::Renderer::has_model_connections)
            {
                for model in &mut self.models {
                    let object = &self.runtime.objects[model.index];
                    if self.runtime.nodes[model.index].alive()
                        && let Some(rig) = &mut model.rig
                    {
                        colliders.insert(
                            object["id"].to_string(),
                            rig.collision_capsules(states[model.index].transform),
                        );
                    }
                }
                for (spec, layer) in self.scene.layers.iter().zip(&mut self.layers) {
                    let object = &self.runtime.objects[spec.node];
                    if self.runtime.nodes[spec.node].alive()
                        && let Some(rig) = &mut layer.puppet
                    {
                        colliders.insert(
                            object["id"].to_string(),
                            rig.collision_capsules(states[spec.node].transform),
                        );
                    }
                }
            }
            let world = Vec3::new(
                pointer[0] * self.scene.size[0],
                (1.0 - pointer[1]) * self.scene.size[1],
                0.0,
            );
            let group_particles = self
                .particles
                .iter()
                .map(|p| self.compose_node_needed(p.index))
                .collect::<Vec<_>>();
            let mut particle_changes = Vec::new();
            for (particle_slot, particle) in self.particles.iter_mut().enumerate() {
                if (!self.paused || !particle.initialized() || !particle.ready())
                    && (particle.state.visible || group_particles[particle_slot])
                {
                    // Keep initial bursts and script commands until their video
                    // sources contain a decoded frame, rather than caching zeroes.
                    let commands = &self.runtime.objects[particle.index]["__particle"]["commands"];
                    if (!particle.initialized()
                        || commands.as_array().is_some_and(|c| !c.is_empty()))
                        && particle
                            .image_requests()
                            .any(|(id, _)| emission_sources.pending.contains(id))
                    {
                        continue;
                    }
                    particle.set_bounds(self.scene.size);
                    particle.set_images(&emission_sources.snapshots);
                    if particle.has_model_connections() {
                        particle.set_models(&colliders);
                    }
                    particle_changes.extend(particle.advance(
                        time,
                        world,
                        &self.runtime.objects[particle.index],
                        &self.audio,
                        self.runtime.scripts.is_some(),
                        view,
                    )?);
                }
            }
            if !particle_changes.is_empty() {
                let changes = serde_json::Value::Array(particle_changes);
                self.runtime.apply(&changes)?;
                if let Some(scripts) = &self.runtime.scripts {
                    scripts.sync_values(&changes)?;
                }
            }
            ensure!(
                self.particles
                    .iter()
                    .map(particles::Renderer::capacity)
                    .sum::<usize>()
                    <= 100_000,
                "scene particle capacity exceeds 100000"
            );
        }
        // Wait for catch-up before the first frame. Once playback has started,
        // new particle children must not freeze independent background shaders.
        if !self.has_frame && !self.ready() {
            return Ok(());
        }
        if self.capture_backdrop
            && self
                .backdrop
                .as_ref()
                .is_none_or(|b| b.texture.size != output)
        {
            if let Some(old) = self.backdrop.take() {
                self.budget = self.budget.saturating_sub(old.texture.byte_size());
            }
            self.backdrop = Some(Rc::new(Buffer::formatted(
                self.gl.clone(),
                output,
                if self.general.post.hdr {
                    glow::RGBA16F
                } else {
                    glow::RGBA8
                },
                &mut self.budget,
            )?));
        }
        self.prepare_reflection(output)?;
        let mut lights = if self.has_lights {
            lighting::Snapshot::collect(&self.runtime.nodes, &states, self.general.light_capacity)?
        } else {
            lighting::Snapshot::default()
        };
        let shadow_matrices = self.prepare_shadows(&mut lights)?;
        self.prepare_camera_fade()?;
        let groups = self.compose_groups();
        self.prepare_compose_targets(&groups)?;
        self.snapshot_backdrops(time);
        let mut frame = Frame {
            audio: &self.audio,
            time,
            delta,
            pointer: self.filtered,
            last_pointer: self.previous,
            down: self.down,
            parallax: if self.general.parallax {
                [
                    0.5 + (self.filtered[0] - 0.5) * self.general.influence,
                    0.5 - (self.filtered[1] - 0.5) * self.general.influence,
                ]
            } else {
                [0.5; 2]
            },
            projection: Mat4::IDENTITY,
            screen: output,
            appearance: None,
            composite: false,
            lights: &lights,
            ambient: self.general.ambient,
            skylight: self.general.skylight,
        };
        let presentation = target;
        let format = if self.general.post.hdr {
            glow::RGBA16F
        } else {
            glow::RGBA8
        };
        let depth = !self.models.is_empty();
        if !self.general.post.enabled() && self.postprocess.take().is_some() {
            self.budget = self.resources().bytes;
        }
        if !depth && !self.general.post.enabled() {
            if self.model_target.take().is_some() {
                self.budget = self.resources().bytes;
            }
        } else if self.model_target.as_ref().is_none_or(|b| {
            b.texture.size != output || b.texture.format != format || b.has_depth() != depth
        }) {
            // Postprocessing a 2D scene needs color storage, not a depth attachment.
            // Keep the old target until its replacement has allocated successfully.
            let allocate = if depth {
                Buffer::formatted_with_depth
            } else {
                Buffer::formatted
            };
            let mut budget = self.budget;
            let buffer = allocate(self.gl.clone(), output, format, &mut budget)?;
            self.model_target = Some(buffer);
            self.budget = self.resources().bytes;
        }
        let mut prepared_images = vec![0; self.layers.len()];
        self.prepare_shadow_sources(&groups, &mut prepared_images, &mut frame, output, view)?;
        if let Some(shadows) = &self.shadows {
            shadows.draw(
                &self.gl,
                &shadow_matrices,
                &self.models,
                &self.runtime.nodes,
                &frame,
                |name| self.scene_texture(name),
            )?;
        }
        self.render_reflection(output, view, &mut frame)?;
        let target = self.model_target.as_ref().map(|b| b.handle).or(target);
        let width = self.scene.size[0] * scale[0];
        let height = self.scene.size[1] * scale[1];
        let left = ((output[0] as f32 - width) / 2.0).floor().max(0.0) as i32;
        let bottom = ((output[1] as f32 - height) / 2.0).floor().max(0.0) as i32;
        let right = ((output[0] as f32 + width) / 2.0)
            .ceil()
            .min(output[0] as f32) as i32;
        let top = ((output[1] as f32 + height) / 2.0)
            .ceil()
            .min(output[1] as f32) as i32;
        unsafe {
            self.gl.disable(glow::DEPTH_TEST);
            self.gl.disable(glow::CULL_FACE);
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.color_mask(true, true, true, true);
            self.gl.depth_mask(true);
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, target);
            self.gl.viewport(0, 0, output[0] as i32, output[1] as i32);
            self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.gl.clear_depth_f32(1.0);
            self.gl
                .clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            self.gl.enable(glow::SCISSOR_TEST);
            self.gl.scissor(left, bottom, right - left, top - bottom);
            self.gl.clear_color(
                self.general.clear[0],
                self.general.clear[1],
                self.general.clear[2],
                1.0,
            );
            self.gl.clear(glow::COLOR_BUFFER_BIT);
            self.gl.disable(glow::SCISSOR_TEST);
            let surface = draw::Surface {
                output,
                target,
                view,
                clip: [left, bottom, right - left, top - bottom],
                group: None,
                position: 0,
            };
            for &object in &self.order {
                if !self.compose_owned(object, &groups) {
                    self.draw_composed_object(
                        object,
                        &groups,
                        &mut prepared_images,
                        &mut frame,
                        &surface,
                    )?;
                }
            }
            self.gl.disable(glow::BLEND);
            if let Some(buffer) = &self.model_target
                && !self.general.post.enabled()
            {
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, presentation);
                self.gl.viewport(0, 0, output[0] as i32, output[1] as i32);
                let mut inputs = [None; 8];
                inputs[0] = Some(buffer.texture.as_ref());
                self.copy
                    .draw(&self.quad, Mat4::IDENTITY, &frame, &inputs, Vec4::ONE);
            }
            if self.general.post.enabled() {
                if self.postprocess.is_none() {
                    self.postprocess = Some(postprocess::Postprocess::load(
                        self.gl.clone(),
                        self.assets.as_ref().context("postprocess shader assets")?,
                    )?);
                }
                self.postprocess.as_mut().unwrap().draw(
                    &self.general.post,
                    &self.model_target.as_ref().unwrap().texture,
                    presentation,
                    &self.quad,
                    &frame,
                    &mut self.budget,
                )?;
            }
            self.draw_camera_fade(presentation, output, &frame)?;
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.bind_vertex_array(None);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, None);
            self.gl.active_texture(glow::TEXTURE0);
            ensure!(
                self.gl.get_error() == glow::NO_ERROR,
                "drawing Rust scene failed"
            );
        }
        self.has_frame = true;
        Ok(())
    }
    fn bind(&self, buffer: &Buffer, clear: bool) {
        unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(buffer.handle));
            self.gl.viewport(
                0,
                0,
                buffer.texture.size[0] as i32,
                buffer.texture.size[1] as i32,
            );
            if clear {
                self.gl.clear_color(0.0, 0.0, 0.0, 0.0);
                self.gl.clear(glow::COLOR_BUFFER_BIT);
            }
        }
    }
    fn update_skeletons(&mut self, time: f32, states: &mut [scene::State]) -> Result<()> {
        let mut rigs = vec![None; states.len()];
        for (index, (spec, layer)) in self.scene.layers.iter().zip(&self.layers).enumerate() {
            if layer.puppet.is_some() {
                rigs[spec.node] = Some(RenderObject::Image(index));
            }
        }
        for (index, model) in self.models.iter().enumerate() {
            rigs[model.index] = Some(RenderObject::Model(index));
        }
        // Compose each child after its parent's current pose is available. This
        // also gives nested rigs the correct physics space without a second pass.
        for position in 0..self.runtime.order().len() {
            let node = self.runtime.order()[position];
            self.runtime.compose_node(node, states)?;
            match rigs[node] {
                Some(RenderObject::Image(index)) => {
                    let layer = &mut self.layers[index];
                    let rig = layer.puppet.as_mut().unwrap();
                    match rig.advance_in_space(
                        time,
                        &self.runtime.objects[node],
                        states[node].transform,
                    ) {
                        Ok(true) => {
                            self.puppet_vertices.clear();
                            for mesh in 0..rig.model.meshes.len() {
                                rig.append_vertices(mesh, &mut self.puppet_vertices);
                            }
                            layer.effect_mesh.upload_model(&self.puppet_vertices)?;
                        }
                        Ok(false) => {}
                        Err(error) => self.runtime.error(format!(
                            "Puppet {}: {error:#}",
                            self.runtime.objects[node]["name"]
                        )),
                    }
                    if !rig.model.attachments.is_empty() {
                        self.runtime
                            .attachments
                            .insert(node, rig.attachment_matrices());
                    }
                }
                Some(RenderObject::Model(index)) => {
                    let model = &mut self.models[index];
                    model.state = states[node].clone();
                    model.advance(time, &self.runtime.objects[node])?;
                    if let Some(rig) = &model.rig
                        && !rig.model.attachments.is_empty()
                    {
                        self.runtime
                            .attachments
                            .insert(node, rig.attachment_matrices());
                    }
                }
                _ => {}
            }
        }
        for (spec, layer) in self.scene.layers.iter().zip(&mut self.layers) {
            layer.state = states[spec.node].clone();
        }
        for particle in &mut self.particles {
            particle.state = states[particle.index].clone();
        }
        Ok(())
    }
    fn blend(&self, name: &str) {
        unsafe {
            if name == "normal" {
                self.gl.disable(glow::BLEND);
            } else {
                self.gl.enable(glow::BLEND);
                self.gl.blend_func_separate(
                    glow::SRC_ALPHA,
                    if name == "additive" {
                        glow::ONE
                    } else {
                        glow::ONE_MINUS_SRC_ALPHA
                    },
                    glow::ONE,
                    if name == "additive" {
                        glow::ONE
                    } else {
                        glow::ONE_MINUS_SRC_ALPHA
                    },
                );
            }
        }
    }

    pub(super) fn update_text(&mut self) -> Result<()> {
        if self.text.is_none() {
            return Ok(());
        }
        let emission_requests = self.emission_requests();
        for index in 0..self.layers.len() {
            let spec = &self.scene.layers[index];
            if !self.layers[index].state.visible
                && !self.layers[index].referenced
                && !self.compose_node_needed(spec.node)
                && !emission_requests
                    .contains_key(&self.runtime.objects[spec.node]["id"].to_string())
            {
                continue;
            }
            let (before, remaining) = self.layers.split_at_mut(index);
            let (layer, after) = remaining.split_first_mut().unwrap();
            let mut resized = false;
            let Some(cache) = &mut layer.text else {
                continue;
            };
            layer.state.color =
                text::material_color(&self.runtime.objects[spec.node], layer.state.color);
            if let Some(raster) = cache.update(
                self.text.as_mut().unwrap(),
                self.assets.as_ref().context("text asset scope missing")?,
                &self.runtime.objects[spec.node],
                spec.size,
            )? {
                let size = [raster.pixels.width, raster.pixels.height];
                if layer.base.textures[0].as_ref().unwrap().size == size {
                    layer.base.textures[0]
                        .as_ref()
                        .unwrap()
                        .upload(&raster.pixels)?;
                } else {
                    let mut budget = self.budget;
                    let texture =
                        Texture::new(self.gl.clone(), Some(raster.pixels), size, &mut budget)?;
                    let buffers = if let Some(old) = &layer.buffers {
                        let format = old[0].texture.format;
                        let mut scratch = before
                            .iter()
                            .chain(after.iter())
                            .filter_map(|layer| {
                                layer.buffers.as_ref().map(|buffers| {
                                    (
                                        (
                                            buffers[0].texture.size,
                                            buffers[0].texture.format,
                                            layer.effects.is_empty(),
                                        ),
                                        buffers.clone(),
                                    )
                                })
                            })
                            .collect();
                        Some(creation::scratch_buffers(
                            self.gl.clone(),
                            size,
                            format,
                            layer.effects.is_empty(),
                            Some(&mut scratch),
                            &mut budget,
                        )?)
                    } else {
                        None
                    };
                    let mut transients = Vec::new();
                    let effects = layer
                        .effects
                        .iter()
                        .zip(&spec.effects)
                        .map(|(effect, definition)| {
                            effect.resized(
                                self.gl.clone(),
                                definition,
                                &buffers.as_ref().unwrap()[0],
                                &mut transients,
                                &mut budget,
                            )
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let id = self.runtime.objects[spec.node]["id"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| self.runtime.objects[spec.node]["id"].to_string());
                    let mut published = Vec::new();
                    let mut replacements = HashMap::new();
                    for name in [
                        format!("_rt_imageLayerComposite_{id}_a"),
                        format!("_rt_imageLayerComposite_{id}_b"),
                        format!("_rt_imageLayerAlbedo_{id}"),
                    ] {
                        if let Some(buffer) = self.global_buffers.get(&name) {
                            let next = if let Some(next) = replacements.get(&buffer.handle) {
                                Rc::clone(next)
                            } else {
                                let next = Rc::new(Buffer::formatted(
                                    self.gl.clone(),
                                    size,
                                    buffer.texture.format,
                                    &mut budget,
                                )?);
                                replacements.insert(buffer.handle, next.clone());
                                next
                            };
                            published.push((name, next));
                        }
                    }
                    let render_size = size.map(|v| v as f32);
                    let source_mesh = Mesh::new(self.gl.clone(), render_size, [1.; 2], false)?;
                    let effect_mesh = Mesh::new(self.gl.clone(), render_size, [1.; 2], true)?;
                    layer.base.textures[0] = Some(texture);
                    layer.buffers = buffers;
                    layer.size = render_size;
                    layer.source_mesh = source_mesh;
                    layer.effect_mesh = effect_mesh;
                    for (effect, resize) in layer.effects.iter_mut().zip(effects) {
                        effect.commit_resize(resize);
                    }
                    for (name, buffer) in published {
                        self.global_buffers.insert(name, buffer);
                    }
                    resized = true;
                }
                layer.offset = raster.offset;
                for effect in &layer.effects {
                    effect.cached.set(false);
                }
            }
            if resized {
                self.budget = self.resources().bytes;
            }
        }
        Ok(())
    }

    pub(super) fn update_media_textures(&mut self) -> Result<()> {
        if !self.media_texture_dirty {
            return Ok(());
        }
        let mut updates = Vec::new();
        let mut budget = self.budget;
        for (field, name) in [
            ("artwork", "_system$mediaThumbnail"),
            ("previous_artwork", "_system$mediaPreviousThumbnail"),
        ] {
            let old = self
                .global_buffers
                .get(name)
                .map_or(0, |b| b.texture.byte_size());
            budget = budget.saturating_sub(old);
            let image = if let Some(path) = self.media[field]["path"]
                .as_str()
                .filter(|_| self.media["enabled"] != false)
            {
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                    .open(path)
                    .context("opening cached media artwork")?;
                ensure!(
                    file.metadata()?.is_file() && file.metadata()?.len() <= 8 * 1024 * 1024,
                    "invalid cached media artwork file"
                );
                let mut bytes = Vec::new();
                file.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() <= 8 * 1024 * 1024,
                    "cached artwork exceeds 8 MiB"
                );
                let mut reader =
                    ::image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
                let mut limits = ::image::Limits::default();
                limits.max_image_width = Some(512);
                limits.max_image_height = Some(512);
                limits.max_alloc = Some(4 * 1024 * 1024);
                reader.limits(limits);
                let image = reader.decode()?.into_rgba8();
                let size = [image.width(), image.height()];
                let texture = Texture::new(
                    self.gl.clone(),
                    Some(assets::Texture {
                        compressed: None,
                        mipmaps: Vec::new(),
                        flags: 2,
                        video: None,
                        width: size[0],
                        height: size[1],
                        content: size,
                        frames: Vec::new(),
                        rgba: image.into_raw(),
                    }),
                    size,
                    &mut budget,
                )?;
                Some(Rc::new(Buffer::from_texture(self.gl.clone(), texture)?))
            } else {
                None
            };
            updates.push((name, image));
        }
        for (name, image) in updates {
            if let Some(image) = image {
                self.global_buffers.insert(name.into(), image);
            } else {
                self.global_buffers.remove(name);
            }
        }
        self.budget = budget;
        self.media_texture_dirty = false;
        Ok(())
    }
}
fn texture_references(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(name)
            if (name.starts_with("_rt_") && name != "_rt_default")
                || name.starts_with("_alias_") =>
        {
            vec![name.clone()]
        }
        serde_json::Value::Object(map) => map.values().flat_map(texture_references).collect(),
        serde_json::Value::Array(array) => array.iter().flat_map(texture_references).collect(),
        _ => Vec::new(),
    }
}
const COPY_VERTEX: &str = "#version 300 es\nprecision highp float;\nin vec3 a_Position; in vec2 a_TexCoord; uniform mat4 g_ModelViewProjectionMatrix; out vec2 uv; void main() { uv=a_TexCoord; gl_Position=g_ModelViewProjectionMatrix*vec4(a_Position,1.0); }";
const COPY_FRAGMENT: &str = "#version 300 es\nprecision highp float;\nin vec2 uv; uniform sampler2D g_Texture0; uniform vec4 g_Color4; uniform vec4 g_Compose; out vec4 color; void main() { color=texture(g_Texture0,uv); if(g_Compose.x>0.5)color.rgb=color.a>0.000001?color.rgb/color.a:vec3(0); color*=g_Color4; }";
const TEXT_FRAGMENT: &str = "#version 300 es\nprecision highp float;\nin vec2 uv; uniform sampler2D g_Texture0;uniform vec4 g_Color4;out vec4 color;void main(){color=texture(g_Texture0,uv)*g_Color4;}";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn script_frame_keeps_host_shape_and_exact_float_values_without_a_json_tree() {
        let value = 0.1_f32;
        let mut audio = audio::AudioSnapshot {
            sequence: 9_007_199_254_740_993,
            available: true,
            capturing: true,
            device: Some("设备\0Ω".into()),
            ..Default::default()
        };
        audio.bands[0].left[0] = value;
        let layers = [json!({"name":"idle","weight":value})];
        let event = json!({"key":"loop","name":"脚步"});
        let mut frame = ScriptFrame {
            time: value as f64,
            delta: value as f64,
            time_of_day: None,
            screen: [100; 2],
            input: ScriptPointer {
                pointer: [value as f64; 2],
                locals: vec![[value as f64; 3]],
                screen_pointer: [value as f64; 2],
                down: false,
                focused: true,
                hits: vec![0],
                eligible: vec![0],
            },
            pointer_events: Vec::new(),
            audio: Some(&audio),
            poses: vec![ScriptPose {
                node: 0,
                time: value as f64,
                layers: &[],
            }],
            animation_events: vec![],
            video_events: vec![],
            values: vec![],
            camera_pose: serde_json::Value::Null,
            media_timeline: None,
        };
        let expected = json!({
            "time":value,"delta":value,"timeOfDay":null,"screen":[100,100],
            "pointer":[value,value],"locals":[[value,value,value]],"screenPointer":[value,value],
            "down":false,"focused":true,"hits":[0],"eligible":[0],"audio":audio,
            "poses":[{"node":0,"time":value,"layers":[]}],
            "animationEvents":[],"videoEvents":[],"values":[],"cameraPose":null,"mediaTimeline":null
        });
        let actual: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&frame).unwrap()).unwrap();
        assert_eq!(actual, expected);
        let runtime = rquickjs::Runtime::new().unwrap();
        let context = rquickjs::Context::full(&runtime).unwrap();
        context.with(|ctx| {
            use crate::script::FrameData;
            let equal: rquickjs::Function = ctx.eval(r#"(function equal(a,b){
                if(a===null||typeof a!=='object')return Object.is(a,b);
                const keys=Object.keys(a);return Object.getPrototypeOf(a)===Object.getPrototypeOf(b)&&
                    keys.length===Object.keys(b).length&&keys.every(k=>Object.hasOwn(b,k)&&equal(a[k],b[k]));
            })"#).unwrap();
            let check = |frame: &ScriptFrame<'_>| {
                let native = frame.to_frame(&ctx).unwrap();
                let reference = ctx.json_parse(serde_json::to_vec(frame).unwrap()).unwrap();
                assert!(equal.call::<_, bool>((native, reference)).unwrap());
            };
            check(&frame);
            frame.time_of_day = Some(0.5);
            frame.pointer_events.push(ScriptPointer {
                pointer: [-0.0, value as f64],
                locals: vec![],
                screen_pointer: [20., 30.],
                down: true,
                focused: false,
                hits: vec![],
                eligible: vec![1],
            });
            frame.audio = None;
            frame.poses[0].layers = &layers;
            frame.animation_events.push(ScriptAnimationEvent { node: 0, event: &event });
            frame.video_events.push(json!({"node":1,"ended":true}));
            frame.values.push(json!([1,["origin"],[0.1,0.2,0.3]]));
            frame.camera_pose = json!({"eye":[0,0,1000]});
            frame.media_timeline = Some(json!({"position":0.25,"duration":3.0}));
            check(&frame);
        });
    }
}
