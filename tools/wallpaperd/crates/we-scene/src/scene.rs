use crate::{
    assets::Assets,
    scene::bindings::{Properties, components, has_binding, resolve},
};
use anyhow::{Context, Result, ensure};
use glam::{EulerRot, Mat4, Quat, Vec3, Vec4};
use serde_json::Value;
use serde_json::json;
use std::{collections::HashMap, rc::Rc};

pub(crate) struct Scene {
    pub size: [f32; 2],
    pub general: Value,
    pub layers: Vec<Layer>,
    pub objects: Vec<Value>,
    pub particles: Vec<(usize, String)>,
    pub models: Vec<(usize, Rc<crate::mdl::Model>)>,
    pub custom_models: Vec<(usize, usize)>,
    pub sounds: Vec<usize>,
    pub settings_node: usize,
}
pub(crate) struct Layer {
    pub node: usize,
    pub text: bool,
    pub fullscreen: bool,
    pub puppet: Option<Rc<crate::mdl::Model>>,
    pub size: [f32; 2],
    pub base: Value,
    pub effects: Vec<Effect>,
}
pub(crate) struct Effect {
    pub index: usize,
    pub file: String,
    pub visible: Value,
    pub fbos: Vec<Value>,
    pub steps: Vec<Step>,
}
pub(crate) enum Step {
    Draw {
        pass: Value,
        target: String,
        bind: Vec<(usize, String)>,
    },
    Copy {
        source: String,
        target: String,
    },
}
#[derive(Clone, PartialEq)]
pub(crate) struct State {
    pub visible: bool,
    pub blend_mode: u8,
    pub transform: Mat4,
    pub color: Vec4,
    pub tint: Vec3,
    pub brightness: f32,
    pub depth: [f32; 2],
    pub parallax_anchor: Vec3,
    pub perspective: bool,
}

/// Effective core values, decoded at the file/property/script boundary.
/// The JSON mirror retains open SceneScript fields; rendering reads these facts.
#[derive(Clone)]
pub(crate) struct Node {
    pub state: State,
    pub lifetime: Lifetime,
    pub texture: TexturePlayback,
    pub light: Option<crate::lighting::Light>,
    pub casts_shadow: bool,
    pub reflected: Option<bool>,
    pub solid: bool,
    pub layer_order: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lifetime {
    Alive,
    Destroyed,
}

#[derive(Clone, Copy)]
pub(crate) struct TexturePlayback {
    pub playing: bool,
    joined: bool,
    rate: f32,
    position: f32,
    anchor: f32,
}

impl TexturePlayback {
    fn decode(object: &Value) -> Result<Self> {
        let value = &object["__texture"];
        if !value.is_object() {
            return Ok(Self {
                playing: true,
                joined: true,
                rate: 1.,
                position: 0.,
                anchor: 0.,
            });
        }
        Ok(Self {
            playing: value["playing"] == true,
            joined: value["joined"] == true,
            rate: components(&value["rate"], &[1.], 1)?[0],
            position: components(&value["position"], &[0.], 1)?[0],
            anchor: components(&value["anchor"], &[0.], 1)?[0],
        })
    }

    pub fn phase(self, time: f32) -> f32 {
        if self.joined {
            time * self.rate
        } else {
            self.position
                + if self.playing {
                    (time - self.anchor) * self.rate
                } else {
                    0.
                }
        }
    }
}

impl Node {
    pub fn decode(object: &Value, size: [f32; 2]) -> Result<Self> {
        Ok(Self {
            state: local_state(object, size, !object["parent"].is_null())?,
            lifetime: if object["__destroyed"] == true {
                Lifetime::Destroyed
            } else {
                Lifetime::Alive
            },
            texture: TexturePlayback::decode(object)?,
            light: crate::lighting::Light::decode(object)?,
            casts_shadow: object["castshadow"].as_bool().unwrap_or(true),
            reflected: (!object["reflected"].is_null()).then(|| object["reflected"] == true),
            solid: object["solid"] == true,
            layer_order: object["__layerOrder"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|node| node.as_u64().map(|node| node as usize))
                .collect(),
        })
    }

    pub fn alive(&self) -> bool {
        self.lifetime == Lifetime::Alive
    }
}

/// JSON angles are radians; SceneScript matrices and layer angles are degrees.
/// glam and GLES use column-major matrices and column vectors; local = T * R * S.
pub(crate) const EULER_ORDER: EulerRot = EulerRot::XYZ;

pub(crate) fn local_transform(origin: Vec3, radians: Vec3, scale: Vec3) -> Mat4 {
    Mat4::from_translation(origin)
        * Mat4::from_quat(Quat::from_euler(
            EULER_ORDER,
            radians.x,
            radians.y,
            radians.z,
        ))
        * Mat4::from_scale(scale)
}
pub(crate) fn local_state(object: &Value, scene_size: [f32; 2], child: bool) -> Result<State> {
    let origin = vector(
        &object["origin"],
        if child {
            [0.0; 3]
        } else {
            [scene_size[0] / 2.0, scene_size[1] / 2.0, 0.0]
        },
    )?;
    let scale = vector(&object["scale"], [1.0; 3])?;
    let angles = vector(&object["angles"], [0.0; 3])?;
    let brightness = components(&object["brightness"], &[1.0], 1)?[0];
    let alpha = components(&object["alpha"], &[1.0], 1)?[0];
    let tint = vector(&object["color"], [1.0; 3])?;
    let color = tint * brightness;
    let depth = components(&object["parallaxDepth"], &[0.0; 2], 2)?;
    ensure!(color.is_finite(), "layer color overflows");
    let transform = local_transform(origin, angles, scale);
    ensure!(transform.is_finite(), "layer transform overflows");
    let blend_mode = components(&object["colorBlendMode"], &[0.], 1)?[0];
    ensure!(
        (0.0..=32.0).contains(&blend_mode) && blend_mode.fract() == 0.,
        "layer colorBlendMode must be an integer in 0..=32"
    );
    Ok(State {
        visible: object["__destroyed"] != true && visible(&object["visible"])?,
        blend_mode: blend_mode as u8,
        // Wallpaper Engine stores angles in radians.
        transform,
        color: color.extend(alpha),
        tint,
        brightness,
        depth: [depth[0], depth[1]],
        parallax_anchor: origin,
        perspective: object["perspective"].as_bool().unwrap_or(false),
    })
}
impl Scene {
    pub fn validate_layout(&self, properties: &Properties) -> Result<()> {
        for layer in &self.layers {
            let object = resolve(&self.objects[layer.node], properties)?;
            ensure!(
                components(&object["size"], &self.size, 2)? == layer.size,
                "changing layer size requires a scene reload"
            );
        }
        Ok(())
    }
    pub fn load(assets: &Assets, properties: &Properties) -> Result<Self> {
        let scene = assets.json("scene.json")?;
        Self::from_value(assets, properties, &scene)
    }
    pub fn from_value(assets: &Assets, properties: &Properties, scene: &Value) -> Result<Self> {
        let general = &scene["general"];
        let projection = general
            .get("orthogonalprojection")
            .or_else(|| general.get("orthographicprojection"));
        let projection = projection.filter(|projection| {
            !projection.is_null()
                && !projection
                    .as_object()
                    .is_some_and(serde_json::Map::is_empty)
        });
        let perspective = projection.is_none();
        let size = if let Some(projection) = projection {
            ensure!(
                !has_binding(projection),
                "bound scene projection is unsupported"
            );
            [
                projection["width"].as_f64().context("scene width")? as f32,
                projection["height"].as_f64().context("scene height")? as f32,
            ]
        } else {
            [1.0; 2]
        };
        ensure!(
            size.iter()
                .all(|v| v.is_finite() && *v > 0.0 && *v <= 32768.0),
            "invalid scene size"
        );
        let mut layers = Vec::new();
        let mut particles = Vec::new();
        let mut particle_points = Vec::new();
        let mut sounds = Vec::new();
        let mut models = HashMap::new();
        let mut model_metadata = Vec::new();
        let mut model_layers = Vec::new();
        let mut custom_models = Vec::new();
        let mut material_metadata = Vec::new();
        let mut effect_metadata = Vec::new();
        let mut image_sizes = Vec::new();
        let mut objects = scene["objects"]
            .as_array()
            .context("scene objects")?
            .clone();
        if perspective {
            for object in &mut objects {
                if object["origin"].is_null() {
                    object["origin"] = serde_json::json!([0, 0, 0]);
                }
            }
        }
        let settings_node = objects.len();
        let mut general = general.clone();
        general["id"] = serde_json::json!("__wallpaperd_scene_settings");
        general["__scene"] = serde_json::json!(true);
        general["_perspective"] = serde_json::json!(perspective);
        general["origin"] = serde_json::json!([0, 0, 0]);
        let mut camera = scene["camera"].clone();
        if !camera.is_object() {
            camera = serde_json::json!({});
        }
        camera["zoom"] = general.get("zoom").cloned().unwrap_or(serde_json::json!(1));
        general["cameraTransforms"] = camera;
        objects.push(general.clone());
        crate::scene::hierarchy::Hierarchy::validate_topology(&objects)?;
        for (node, object) in objects.iter().enumerate() {
            if object["model"].is_object() {
                let id = object["model"]["__wallpaperd_model_data"]
                    .as_u64()
                    .context("invalid custom model handle")?;
                custom_models.push((node, usize::try_from(id)?));
                continue;
            }
            if let Some(file) = object["particle"].as_str() {
                particles.push((node, file.into()));
                let definition = resolve(&assets.json(file)?, properties)?;
                let mut points = vec![serde_json::json!([0, 0, 0]); 8];
                for cp in definition["controlpoint"].as_array().into_iter().flatten() {
                    let id = components(&cp["id"], &[0.], 1)?[0];
                    ensure!(
                        (0.0..8.0).contains(&id) && id.fract() == 0.,
                        "invalid particle control point id"
                    );
                    points[id as usize] =
                        serde_json::json!(components(&cp["offset"], &[0.; 3], 3)?);
                }
                particle_points.push((node, points));
                continue;
            }
            if let Some(file) = object["model"].as_str() {
                let model = if let Some(model) = models.get(file) {
                    Rc::clone(model)
                } else {
                    let model = Rc::new(
                        crate::mdl::parse(&assets.read(file)?)
                            .with_context(|| format!("reading model {file}"))?,
                    );
                    models.insert(file.to_owned(), model.clone());
                    model
                };
                let skin = resolve(&object["skin"], properties)?.as_u64().unwrap_or(0) as usize;
                let materials = model
                    .meshes
                    .iter()
                    .map(|mesh| {
                        let file = mesh
                            .materials
                            .get(skin)
                            .context("model skin outside materials")?;
                        let mut passes = assets.json(file)?["passes"].clone();
                        for pass in passes.as_array_mut().context("model material passes")? {
                            crate::shader::material_properties(assets, pass)?;
                        }
                        Ok(passes)
                    })
                    .collect::<Result<Vec<_>>>()?;
                material_metadata.push((node, Value::Array(materials)));
                model_metadata.push((node, model.metadata()));
                model_layers.push((node, model));
                continue;
            }
            let sound = &object["sound"];
            if !sound.is_null() && !sound.as_array().is_some_and(Vec::is_empty) {
                let files = sound.as_array().context("sound asset list")?;
                ensure!(files.len() <= 64, "sound asset count exceeds 64");
                for file in files {
                    assets.validate(file.as_str().context("sound asset path")?)?;
                }
                sounds.push(node);
                continue;
            }
            let text = !object["text"].is_null();
            if object["image"].is_null() && !text {
                continue;
            }
            let mut puppet = None;
            let mut fullscreen = false;
            let mut image_size = size;
            let mut material = if text {
                serde_json::json!({"passes":[{"blending":"translucent"}]})
            } else {
                let model =
                    assets.json(object["image"].as_str().context("object has no image")?)?;
                fullscreen = model["fullscreen"].as_bool().unwrap_or(false);
                image_size = [
                    components(&model["width"], &[size[0]], 1)?[0],
                    components(&model["height"], &[size[1]], 1)?[0],
                ];
                if let Some(file) = model["puppet"].as_str() {
                    let mesh = if let Some(model) = models.get(file) {
                        Rc::clone(model)
                    } else {
                        let model = Rc::new(
                            crate::mdl::parse(&assets.read(file)?)
                                .with_context(|| format!("reading puppet {file}"))?,
                        );
                        models.insert(file.to_owned(), model.clone());
                        model
                    };
                    model_metadata.push((node, mesh.metadata()));
                    puppet = Some(mesh);
                }
                let material =
                    assets.json(model["material"].as_str().context("model material")?)?;
                if (model["width"].is_null()
                    || model["height"].is_null()
                    || image_size.contains(&0.))
                    && let Some(texture) =
                        material["passes"][0]["textures"][0]
                            .as_str()
                            .filter(|name| {
                                !name.starts_with("_rt_")
                                    && !name.starts_with("_alias_")
                                    && !name.starts_with("_system$")
                                    && *name != "previous"
                            })
                {
                    let intrinsic = assets.texture_info(texture)?.size.map(|v| v as f32);
                    for axis in 0..2 {
                        if model[if axis == 0 { "width" } else { "height" }].is_null()
                            || image_size[axis] == 0.
                        {
                            image_size[axis] = intrinsic[axis];
                        }
                    }
                }
                material
            };
            for pass in material["passes"]
                .as_array_mut()
                .context("material passes")?
            {
                crate::shader::material_properties(assets, pass)?;
            }
            let passes = material["passes"].as_array().context("material passes")?;
            material_metadata.push((node, serde_json::json!([passes])));
            ensure!(
                passes.len() == 1 && passes[0].is_object(),
                "base material must contain one pass"
            );
            let mut effects = Vec::new();
            let mut pass_count = 1;
            if let Some(values) = object["effects"].as_array() {
                for (index, effect) in values.iter().enumerate() {
                    let file = effect["file"].as_str().context("effect file")?;
                    let scoped = assets.effect(file)?;
                    let definition = scoped.json(file)?;
                    ensure!(
                        definition["commands"].is_null(),
                        "top-level effect commands are unsupported"
                    );
                    let fbos = definition["fbos"].as_array().cloned().unwrap_or_default();
                    ensure!(fbos.len() <= 32, "too many named effect buffers");
                    let mut steps = Vec::new();
                    let mut material_passes = Vec::new();
                    let mut material_index = 0;
                    for entry in definition["passes"].as_array().context("effect passes")? {
                        if let Some(command) = entry["command"].as_str() {
                            ensure!(command == "copy", "unsupported effect command {command}");
                            steps.push(Step::Copy {
                                source: entry["source"].as_str().context("copy source")?.into(),
                                target: entry["target"].as_str().context("copy target")?.into(),
                            });
                            continue;
                        }
                        let material =
                            scoped.json(entry["material"].as_str().context("effect material")?)?;
                        for base in material["passes"]
                            .as_array()
                            .context("effect material passes")?
                        {
                            let mut pass = base.clone();

                            if let Some(overrides) = effect["passes"]
                                .get(material_index)
                                .and_then(Value::as_object)
                            {
                                for (key, v) in overrides {
                                    match key.as_str() {
                                        "textures" => {
                                            let mut textures =
                                                pass[key].as_array().cloned().unwrap_or_default();
                                            for (slot, texture) in v
                                                .as_array()
                                                .context("effect textures")?
                                                .iter()
                                                .enumerate()
                                            {
                                                if textures.len() <= slot {
                                                    textures.resize(slot + 1, Value::Null);
                                                }
                                                if !texture.is_null() {
                                                    textures[slot] = texture.clone();
                                                }
                                            }
                                            pass[key] = Value::Array(textures);
                                        }
                                        "combos" | "constantshadervalues" => {
                                            if pass[key].is_null() {
                                                pass[key] = serde_json::json!({});
                                            }
                                            for (name, value) in
                                                v.as_object().context("effect settings")?
                                            {
                                                pass[key][name] = value.clone();
                                            }
                                        }
                                        _ => pass[key] = v.clone(),
                                    }
                                }
                            }
                            crate::shader::material_properties(&scoped, &mut pass)?;
                            material_passes.push(pass.clone());
                            let mut bind = Vec::new();
                            if let Some(bindings) = entry["bind"].as_array() {
                                for binding in bindings {
                                    let index =
                                        binding["index"].as_u64().context("bind index")? as usize;
                                    ensure!(index < 8, "invalid bind index");
                                    bind.push((
                                        index,
                                        binding["name"].as_str().context("bind name")?.into(),
                                    ));
                                }
                            }
                            steps.push(Step::Draw {
                                pass,
                                target: entry["target"].as_str().unwrap_or("_rt_default").into(),
                                bind,
                            });
                            material_index += 1;
                            pass_count += 1;
                        }
                    }
                    ensure!(steps.len() <= 64, "too many effect steps");
                    effect_metadata.push((node, index, material_passes));
                    effects.push(Effect {
                        index,
                        file: file.into(),
                        visible: effect["visible"].clone(),
                        fbos,
                        steps,
                    });
                }
            }
            ensure!(pass_count <= 32, "too many image passes");
            let dimensions = components(&resolve(&object["size"], properties)?, &image_size, 2)?;
            ensure!(
                dimensions.iter().all(|v| *v > 0.0 && *v <= 32768.0),
                "invalid layer size"
            );
            let mut base = passes[0].clone();

            // Image vertices are in layer units; WE passthrough defaults to clip
            // vertices for effect passes, but base layers need their local matrix.
            if base["shader"] == "passthrough" {
                if !base["combos"].is_object() {
                    base["combos"] = serde_json::json!({});
                }
                base["combos"]["TRANSFORM"] = serde_json::json!(1);
            }
            if object["size"].is_null() {
                let value = serde_json::json!(dimensions);
                image_sizes.push((node, value));
            }
            layers.push(Layer {
                node,
                text,
                fullscreen,
                puppet,
                size: [dimensions[0], dimensions[1]],
                base,
                effects,
            });
        }
        for (node, size) in image_sizes {
            objects[node]["size"] = size;
        }
        for (node, points) in particle_points {
            if objects[node]["instanceoverride"].is_null() {
                objects[node]["instanceoverride"] = serde_json::json!({});
            }
            objects[node]["__particle"] =
                serde_json::json!({"playing":true,"live":0,"commands":[],"controlpoints":points});
        }
        for node in &sounds {
            objects[*node]["__sound"] = serde_json::json!({"playing":!objects[*node]["startsilent"].as_bool().unwrap_or(false),"paused":false,"revision":0,"started":0});
        }
        for (node, metadata) in model_metadata {
            objects[node]["__model"] = metadata;
            objects[node]["__boneOverrides"] = serde_json::json!({});
            objects[node]["__boneOverrideTime"] = serde_json::json!({});
            objects[node]["__bonePhysics"] = serde_json::json!({});
        }
        for (node, materials) in material_metadata {
            objects[node]["__materials"] = materials;
        }
        for (node, effect, passes) in effect_metadata {
            objects[node]["effects"][effect]["passes"] = Value::Array(passes);
        }
        crate::scene::hierarchy::Hierarchy::new(&objects)?;
        Ok(Self {
            size,
            general: general.clone(),
            layers,
            objects: objects.clone(),
            settings_node,
            particles,
            models: model_layers,
            custom_models,
            sounds,
        })
    }
}
pub(crate) fn visible(v: &Value) -> Result<bool> {
    if v.is_null() {
        Ok(true)
    } else {
        v.as_bool().context("visible must be boolean")
    }
}
pub(crate) fn numbers(v: &Value, default: &[f32]) -> Result<Vec<f32>> {
    let v = v.get("value").unwrap_or(v);
    let values = match v {
        Value::Null => default.to_vec(),
        Value::Bool(value) => vec![if *value { 1.0 } else { 0.0 }],
        Value::Number(v) => vec![v.as_f64().context("number")? as f32],
        Value::String(v) => v
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|v| !v.is_empty())
            .map(str::parse)
            .collect::<std::result::Result<Vec<f32>, _>>()?,
        Value::Array(v) => v
            .iter()
            .map(|v| v.as_f64().map(|v| v as f32).context("vector number"))
            .collect::<Result<Vec<_>>>()?,
        _ => anyhow::bail!("unsupported value {v}"),
    };
    ensure!(
        values.iter().all(|v| v.is_finite()),
        "non-finite scene value"
    );
    Ok(values)
}
fn vector(v: &Value, default: [f32; 3]) -> Result<Vec3> {
    let v = components(v, &default, 3)?;
    Ok(Vec3::new(v[0], v[1], v[2]))
}

pub(crate) fn prepare_creation(
    assets: &Assets,
    configuration: &Value,
    general: &Value,
    properties: &Properties,
    node: usize,
    size: [f32; 2],
) -> Result<Value> {
    ensure!(node < 4096, "scene exceeds 4096 objects");
    let mut object = if let Some(file) = configuration
        .as_str()
        .or_else(|| configuration["file"].as_str())
    {
        assets.validate(file)?;
        match std::path::Path::new(file)
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "mdl" => json!({"model":file}),
            "mp3" | "wav" | "ogg" | "flac" | "opus" | "m4a" => {
                json!({"sound":[file],"playbackmode":"single"})
            }
            "json" => {
                let definition = assets.json(file)?;
                if !definition["image"].is_null()
                    || !definition["model"].is_null()
                    || !definition["text"].is_null()
                    || !definition["particle"].is_null()
                {
                    definition
                } else if !definition["emitter"].is_null()
                    || !definition["initializer"].is_null()
                    || !definition["renderer"].is_null()
                {
                    json!({"particle":file})
                } else if definition["material"].is_string() {
                    json!({"image":file})
                } else {
                    anyhow::bail!("asset is not a layer configuration: {file}")
                }
            }
            _ => anyhow::bail!("unsupported layer asset {file}"),
        }
    } else {
        configuration.clone()
    };
    let fields = object
        .as_object_mut()
        .context("layer configuration must be an object")?;
    fields.retain(|key, _| !key.starts_with("__"));
    fields.insert("id".into(), json!(format!("__wallpaperd_dynamic_{node}")));
    for key in ["image", "particle", "model", "font"] {
        if let Some(value) = fields.get_mut(key)
            && let Some(file) = value["file"].as_str()
        {
            *value = json!(file);
        }
    }
    let child = !object["parent"].is_null();
    if object["origin"].is_null() {
        object["origin"] = if child || general["_perspective"] == true {
            json!([0, 0, 0])
        } else {
            json!([size[0] / 2., size[1] / 2., 0.])
        };
    }
    let parent = object.as_object_mut().unwrap().remove("parent");
    let attachment = object.as_object_mut().unwrap().remove("attachment");
    let mut general = general.clone();
    if general["_perspective"] != true
        && general["orthogonalprojection"].is_null()
        && general["orthographicprojection"].is_null()
    {
        general["orthogonalprojection"] = json!({"width":size[0],"height":size[1]});
    }
    let mut scene = Scene::from_value(
        assets,
        properties,
        &json!({"general":general,"objects":[object]}),
    )?;
    prepare_texture_animations(&mut scene, assets)?;
    let mut object = scene.objects.remove(0);
    if let Some(parent) = parent {
        object["parent"] = parent;
    }
    if let Some(attachment) = attachment {
        object["attachment"] = attachment;
    }
    local_state(&resolve(&object, properties)?, size, child)?;
    crate::lighting::validate(&resolve(&object, properties)?)?;
    Ok(object)
}

pub(crate) fn prepare_texture_animations(scene: &mut Scene, assets: &Assets) -> Result<()> {
    let mut masters = HashMap::new();
    for layer in &scene.layers {
        if layer.text {
            continue;
        }
        let Some(name) = layer.base["textures"][0].as_str().filter(|n| {
            !n.starts_with("_rt_")
                && !n.starts_with("_system$")
                && !n.starts_with("_alias_")
                && *n != "previous"
        }) else {
            continue;
        };
        let info = assets.texture_info(name)?;
        let mut state = texture_metadata(&info);
        let playback = json!({"asset":name,"rate":1,"position":0,"anchor":0,"playing":true,"joined":true,"loop":true,"revision":0});
        state
            .as_object_mut()
            .unwrap()
            .extend(playback.as_object().unwrap().clone());
        scene.objects[layer.node]["__texture"] = state;
        if info.video {
            scene.objects[layer.node]["__texture"]["key"] =
                json!(format!("{:?}", assets.texture_key(name)?));
            let master = *masters
                .entry(assets.texture_key(name)?)
                .or_insert(layer.node);
            scene.objects[layer.node]["__videoMaster"] = master.into();
        }
    }
    Ok(())
}
pub(crate) fn texture_metadata(info: &crate::assets::TextureInfo) -> Value {
    // TEX JSON has no verified runtime sequence selector; preserve the default
    // sequence's existing ITextureAnimation frame addresses.
    let frames = &info.frames[crate::assets::sequence_range(&info.frames, 0)];
    let durations = frames
        .iter()
        .map(|frame| {
            if frame.duration > 0. {
                frame.duration
            } else {
                1. / 60.
            }
        })
        .collect::<Vec<_>>();
    json!({"loaded":true,"video":info.video,"durations":durations,"frameCount":frames.len(),"duration":if info.video {info.duration} else {f64::from(durations.iter().sum::<f32>())}})
}

pub(crate) mod bindings {
    use anyhow::{Context, Result, ensure};
    use serde_json::Value;
    use std::collections::BTreeMap;

    pub type Properties = BTreeMap<String, Value>;

    /// Resolve authored bindings only when settings change, never in the frame loop.
    pub(crate) fn resolve(value: &Value, properties: &Properties) -> Result<Value> {
        match value {
            Value::Object(object) if object.contains_key("value") => {
                let fallback = resolve(&object["value"], properties)?;
                let Some(user) = object.get("user") else {
                    return Ok(fallback);
                };
                let name = user
                    .as_str()
                    .or_else(|| user["name"].as_str())
                    .context("invalid user binding")?;
                let Some(selected) = properties.get(name) else {
                    return Ok(fallback);
                };
                if let Some(condition) = user.get("condition") {
                    let condition = condition
                        .as_str()
                        .context("invalid binding condition")?
                        .trim();
                    let matches = condition_matches(selected, condition);
                    // Boolean condition bindings cache the authored selection's
                    // result in `value`; switching options must recompute it.
                    if fallback.is_boolean() {
                        return Ok(Value::Bool(matches));
                    }
                    return Ok(if matches {
                        fallback.clone()
                    } else {
                        neutral(&fallback)
                    });
                }
                Ok(selected.clone())
            }
            Value::Object(object) => object
                .iter()
                .map(|(key, value)| Ok((key.clone(), resolve(value, properties)?)))
                .collect::<Result<serde_json::Map<_, _>>>()
                .map(Value::Object),
            Value::Array(array) => array
                .iter()
                .map(|v| resolve(v, properties))
                .collect::<Result<Vec<_>>>()
                .map(Value::Array),
            _ => Ok(value.clone()),
        }
    }

    fn condition_matches(selected: &Value, condition: &str) -> bool {
        let truthy = |value: &Value| -> bool {
            match value {
                Value::Bool(value) => *value,
                Value::Number(value) => value.as_f64().is_some_and(|v| v.abs() > 0.0001),
                Value::Array(values) => values
                    .iter()
                    .any(|v| v.as_f64().is_some_and(|v| v.abs() > 0.0001)),
                Value::String(value) => {
                    !["", "0", "false"].contains(&value.trim().to_ascii_lowercase().as_str())
                }
                _ => false,
            }
        };
        let condition = condition.trim();
        if condition.is_empty() {
            return truthy(selected);
        }
        if let Ok(expected) = condition.parse::<f64>() {
            if let Some(boolean) = selected.as_bool() {
                if expected.abs() < 0.0001 {
                    return boolean;
                }
                if (expected - 1.0).abs() < 0.0001 {
                    return !boolean;
                }
                return false;
            }
            let number = selected.as_f64().or_else(|| {
                selected
                    .as_array()
                    .and_then(|v| v.first())
                    .and_then(Value::as_f64)
            });
            if let Some(number) = number {
                return (expected - number).abs() < 0.0001;
            }
            return selected.as_str().is_some_and(|v| v.trim() == condition);
        }
        match condition.to_ascii_lowercase().as_str() {
            "true" => truthy(selected),
            "false" => !truthy(selected),
            _ => selected.as_str().is_some_and(|v| v.trim() == condition),
        }
    }

    fn neutral(value: &Value) -> Value {
        match value {
            Value::Bool(_) => Value::Bool(false),
            Value::Number(_) => Value::from(0),
            Value::String(value) => {
                if value.split_whitespace().all(|v| v.parse::<f32>().is_ok()) {
                    Value::String(
                        value
                            .split_whitespace()
                            .map(|_| "0")
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                } else {
                    Value::String(String::new())
                }
            }
            Value::Array(values) => Value::Array(values.iter().map(neutral).collect()),
            _ => Value::Null,
        }
    }

    pub(crate) fn has_binding(value: &Value) -> bool {
        match value {
            Value::Object(values) => {
                values.contains_key("user")
                    || values.contains_key("script")
                    || values.contains_key("animation")
                    || values.values().any(has_binding)
            }
            Value::Array(values) => values.iter().any(has_binding),
            _ => false,
        }
    }
    pub(crate) fn components(value: &Value, defaults: &[f32], count: usize) -> Result<Vec<f32>> {
        let mut values = crate::scene::numbers(value, defaults)?;
        if values.len() == 1 && count > 1 {
            values.resize(count, values[0]);
        }
        ensure!(
            values.len() == count,
            "expected {count} components, got {value}"
        );
        Ok(values)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;
        #[test]
        fn conditional_visibility_uses_selection_instead_of_cached_value() {
            let binding = json!({"user":{"name":"style","condition":"2"},"value":false});
            for (style, visible) in [("1", false), ("2", true), ("3", false)] {
                let properties = Properties::from([("style".into(), json!(style))]);
                assert_eq!(resolve(&binding, &properties).unwrap(), json!(visible));
            }
            assert_eq!(resolve(&binding, &Properties::new()).unwrap(), json!(false));
        }
        #[test]
        fn bindings_conditions_missing_values_and_scalar_vectors() {
            let values = Properties::from([
                ("enabled".into(), json!(true)),
                ("size".into(), json!(0.25)),
            ]);
            assert_eq!(
                resolve(&json!({"user":"missing","value":0.8}), &values).unwrap(),
                json!(0.8)
            );
            assert_eq!(
                resolve(
                    &json!({"user":{"name":"enabled","condition":"0"},"value":true}),
                    &values
                )
                .unwrap(),
                json!(true)
            );
            assert_eq!(
                resolve(
                    &json!({"user":{"name":"enabled","condition":"1"},"value":"1 2"}),
                    &values
                )
                .unwrap(),
                json!("0 0")
            );
            let scalar = resolve(&json!({"user":"size","value":"1 1"}), &values).unwrap();
            assert_eq!(components(&scalar, &[], 2).unwrap(), vec![0.25; 2]);
            assert!(components(&json!("NaN 1"), &[], 2).is_err());
            assert!(condition_matches(&json!(true), "0.0"));
            assert!(condition_matches(&json!(0.2), "TRUE"));
            assert!(condition_matches(&json!([0, 0, 0.5]), ""));
            assert!(condition_matches(&json!(" false "), "false"));
            assert!(!condition_matches(&json!("01"), "1"));
            assert_eq!(components(&json!(true), &[], 2).unwrap(), vec![1.0; 2]);
        }
    }
}

pub(crate) mod hierarchy {
    //! Validated scene topology. Authored order stays intact for compositing.
    use anyhow::{Context, Result, ensure};
    use glam::Mat4;
    use serde_json::Value;
    use std::collections::HashMap;

    pub(crate) struct Hierarchy {
        parents: Vec<Option<usize>>,
        order: Vec<usize>,
        attachments: Vec<Option<(usize, Mat4)>>,
        propagation: Vec<bool>,
    }

    impl Hierarchy {
        pub fn parents(&self) -> &[Option<usize>] {
            &self.parents
        }
        pub fn propagates(&self, node: usize) -> bool {
            self.propagation[node]
        }
        pub fn order(&self) -> &[usize] {
            &self.order
        }
        pub fn new(objects: &[Value]) -> Result<Self> {
            Self::from_objects(&objects.iter().collect::<Vec<_>>())
        }
        pub fn from_objects(objects: &[&Value]) -> Result<Self> {
            Self::build(objects, true)
        }
        pub fn validate_topology(objects: &[Value]) -> Result<()> {
            Self::build(&objects.iter().collect::<Vec<_>>(), false).map(|_| ())
        }
        fn build(objects: &[&Value], resolve_attachments: bool) -> Result<Self> {
            ensure!(objects.len() <= 4096, "scene exceeds 4096 objects");
            let mut ids = HashMap::new();
            for (index, object) in objects.iter().enumerate() {
                if let Some(id) = object.get("id") {
                    ensure!(id.is_number() || id.is_string(), "invalid object id");
                    ensure!(
                        ids.insert(id.to_string(), index).is_none(),
                        "duplicate object id {id}"
                    );
                }
            }
            let parents = objects
                .iter()
                .map(
                    |object| match object.get("parent").filter(|v| !v.is_null()) {
                        None => Ok(None),
                        Some(id) => ids
                            .get(&id.to_string())
                            .copied()
                            .map(Some)
                            .with_context(|| format!("missing parent {id} for {}", object["name"])),
                    },
                )
                .collect::<Result<Vec<_>>>()?;
            let mut order = Vec::with_capacity(objects.len());
            let mut marks = vec![0u8; objects.len()];
            // Iterative traversal bounds stack use even for deeply nested input.
            for start in 0..objects.len() {
                if marks[start] == 2 {
                    continue;
                }
                let mut chain = Vec::new();
                let mut next = Some(start);
                while let Some(index) = next {
                    ensure!(
                        marks[index] != 1,
                        "scene parent cycle at {}",
                        objects[index]["id"]
                    );
                    if marks[index] == 2 {
                        break;
                    }
                    marks[index] = 1;
                    chain.push(index);
                    next = parents[index];
                }
                for index in chain.into_iter().rev() {
                    marks[index] = 2;
                    order.push(index);
                }
            }
            let mut attachments = Vec::with_capacity(objects.len());
            for (object, parent) in objects.iter().zip(&parents) {
                let key = &object["attachment"];
                if !resolve_attachments || key.is_null() || key.as_str() == Some("") {
                    attachments.push(None);
                    continue;
                }
                let parent = parent.context("attachment has no parent")?;
                let metadata = &objects[parent]["__model"];
                let items = metadata["attachments"]
                    .as_array()
                    .context("parent has no attachments")?;
                let index = if let Some(name) = key.as_str() {
                    items.iter().position(|a| a["name"] == name)
                } else {
                    key.as_u64().map(|n| n as usize)
                };
                let index = index
                    .filter(|i| *i < items.len())
                    .context("unknown parent attachment")?;
                let item = &items[index];
                let bones = metadata["bones"]
                    .as_array()
                    .context("attachment skeleton")?;
                let mut bone = item["bone"].as_u64().context("attachment bone")? as usize;
                let mut chain = Vec::new();
                while bone < bones.len() && !chain.contains(&bone) {
                    chain.push(bone);
                    let Some(parent) = bones[bone]["parent"].as_u64() else {
                        break;
                    };
                    bone = parent as usize;
                }
                ensure!(
                    !chain.is_empty() && bone < bones.len() && bones[bone]["parent"].is_null(),
                    "invalid attachment bone hierarchy"
                );
                let mut bind = Mat4::IDENTITY;
                for bone in chain.into_iter().rev() {
                    bind *= matrix(&bones[bone]["local"])?;
                }
                bind *= matrix(&item["matrix"])?;
                ensure!(bind.is_finite(), "attachment transform overflow");
                attachments.push(Some((index, bind)));
            }
            let propagation = objects
                .iter()
                .map(|o| o["disablepropagation"] != true)
                .collect();
            Ok(Self {
                parents,
                order,
                attachments,
                propagation,
            })
        }

        pub fn compose(
            &self,
            states: &mut [crate::scene::State],
            poses: &HashMap<usize, Vec<Mat4>>,
        ) -> Result<()> {
            for &index in &self.order {
                self.compose_node(index, states, poses)?;
            }
            Ok(())
        }
        pub fn compose_node(
            &self,
            index: usize,
            states: &mut [crate::scene::State],
            poses: &HashMap<usize, Vec<Mat4>>,
        ) -> Result<()> {
            if let Some(parent) = self.parents[index] {
                let attachment = self.attachments[index].map_or(Mat4::IDENTITY, |(slot, bind)| {
                    poses
                        .get(&parent)
                        .and_then(|a| a.get(slot))
                        .copied()
                        .unwrap_or(bind)
                });
                states[index].transform =
                    states[parent].transform * attachment * states[index].transform;
                states[index].visible &= states[parent].visible;
            }
            ensure!(
                states[index].transform.is_finite(),
                "scene world transform overflows"
            );
            states[index].parallax_anchor = states[index].transform.w_axis.truncate();
            if let Some(parent) = self.parents[index].filter(|p| self.propagation[*p]) {
                states[index].depth = states[parent].depth;
                states[index].parallax_anchor = states[parent].parallax_anchor;
            }
            Ok(())
        }
    }

    fn matrix(value: &Value) -> Result<Mat4> {
        let values = value
            .as_array()
            .filter(|v| v.len() == 16)
            .context("invalid attachment matrix")?;
        let mut out = [0.0; 16];
        for (x, v) in out.iter_mut().zip(values) {
            *x = v.as_f64().context("invalid attachment matrix component")? as f32;
        }
        let matrix = Mat4::from_cols_array(&out);
        ensure!(matrix.is_finite(), "non-finite attachment matrix");
        Ok(matrix)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use glam::{Mat4, Vec3, Vec4};
        use serde_json::json;
        #[test]
        fn out_of_order_parent_rotation_scale_visibility_and_invalid_topology() {
            let graph = Hierarchy::new(&[json!({"id":2,"parent":1}), json!({"id":1})]).unwrap();
            let state = |transform, visible| crate::scene::State {
                blend_mode: 0,
                perspective: false,
                transform,
                visible,
                color: Vec4::ONE,
                tint: Vec3::ONE,
                brightness: 1.,
                depth: [0.0; 2],
                parallax_anchor: glam::Vec3::ZERO,
            };
            let mut states = [
                state(Mat4::from_translation(Vec3::X), true),
                state(
                    Mat4::from_translation(Vec3::new(3.0, 4.0, 0.0))
                        * Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2)
                        * Mat4::from_scale(Vec3::splat(2.0)),
                    false,
                ),
            ];
            graph.compose(&mut states, &HashMap::new()).unwrap();
            assert!(
                states[0]
                    .transform
                    .transform_point3(Vec3::ZERO)
                    .abs_diff_eq(Vec3::new(3.0, 6.0, 0.0), 0.00001)
            );
            assert!(!states[0].visible);
            assert!(
                Hierarchy::new(&[json!({"id":1,"parent":2}), json!({"id":2,"parent":1})]).is_err()
            );
            assert!(Hierarchy::new(&[json!({"id":1,"parent":9})]).is_err());
            assert!(Hierarchy::new(&[json!({"id":1}), json!({"id":1})]).is_err());
        }
        #[test]
        fn attachment_bind_live_pose_parent_parallax_cutoff_and_overflow() {
            let matrix = |x, y| Mat4::from_translation(Vec3::new(x, y, 0.)).to_cols_array();
            let mut objects = vec![
                json!({"id":2,"parent":1,"attachment":"hook","origin":[1,0,0],"parallaxDepth":[0.9,0.8]}),
                json!({"id":1,"origin":[10,10,0],"scale":[2,2,1],"angles":[0,0,std::f32::consts::FRAC_PI_2],"parallaxDepth":[0.5,0.25],"__model":{"bones":[{"parent":null,"local":matrix(1.,0.)}],"attachments":[{"name":"hook","bone":0,"matrix":matrix(2.,0.)}]}}),
                json!({"id":3,"parent":2,"origin":[0,1,0],"parallaxDepth":[0.1,0.2]}),
            ];
            let states = |objects: &[Value]| {
                objects
                    .iter()
                    .map(|o| {
                        crate::scene::local_state(o, [32.; 2], !o["parent"].is_null()).unwrap()
                    })
                    .collect::<Vec<_>>()
            };
            let graph = Hierarchy::new(&objects).unwrap();
            let mut values = states(&objects);
            graph.compose(&mut values, &HashMap::new()).unwrap();
            assert!(
                values[0]
                    .transform
                    .w_axis
                    .truncate()
                    .abs_diff_eq(Vec3::new(10., 18., 0.), 1e-5)
            );
            assert_eq!(values[0].depth, [0.5, 0.25]);
            assert_eq!(values[2].parallax_anchor, Vec3::new(10., 10., 0.));
            let poses = HashMap::from([(1, vec![Mat4::from_translation(Vec3::new(0., 4., 0.))])]);
            let mut values = states(&objects);
            graph.compose(&mut values, &poses).unwrap();
            assert!(
                values[0]
                    .transform
                    .w_axis
                    .truncate()
                    .abs_diff_eq(Vec3::new(2., 12., 0.), 1e-5)
            );
            assert!(
                values[2]
                    .transform
                    .w_axis
                    .truncate()
                    .abs_diff_eq(Vec3::new(0., 12., 0.), 1e-5)
            );
            objects[1]["disablepropagation"] = json!(true);
            let mut values = states(&objects);
            Hierarchy::new(&objects)
                .unwrap()
                .compose(&mut values, &poses)
                .unwrap();
            assert_eq!(values[2].depth, [0.9, 0.8]);
            assert_eq!(values[2].parallax_anchor, values[0].parallax_anchor);
            objects[0]["attachment"] = json!("missing");
            assert!(Hierarchy::new(&objects).is_err());
            let objects = vec![
                json!({"id":1,"scale":[1e30,1e30,1e30]}),
                json!({"id":2,"parent":1,"scale":[1e30,1e30,1e30]}),
            ];
            assert!(
                Hierarchy::new(&objects)
                    .unwrap()
                    .compose(&mut states(&objects), &HashMap::new())
                    .is_err()
            );
        }
    }
}

pub(crate) mod runtime {
    //! Runtime values and scripts are separate from authored definitions and GPU resources.
    use crate::{
        assets::Assets,
        scene::bindings::{Properties, resolve},
        scene::hierarchy::Hierarchy,
        scene::{self, State},
        script::Scripts,
    };
    use anyhow::{Context, Result, ensure};
    use serde_json::{Value, json};
    use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

    #[derive(Default)]
    pub(crate) struct PatchImpact {
        pub states: bool,
        pub skeletons: bool,
    }

    const CHECK_LIGHT: u8 = 1;
    const CHECK_PARTICLES: u8 = 2;
    const CHECK_MODEL: u8 = 4;
    const CHECK_VIDEO: u8 = 8;
    const CHECK_CAMERA: u8 = 16;
    const CHECK_PROJECTION: u8 = 32;
    const CHECK_ALL: u8 =
        CHECK_LIGHT | CHECK_PARTICLES | CHECK_MODEL | CHECK_VIDEO | CHECK_CAMERA | CHECK_PROJECTION;

    fn control_checks(key: &str, child: Option<&str>) -> u8 {
        match key {
            "light" | "intensity" | "radius" | "castshadow" => CHECK_LIGHT,
            "__particle" => {
                if child.is_none_or(|key| key == "commands") {
                    CHECK_PARTICLES
                } else {
                    // live and the other particle statistics do not alter controls.
                    0
                }
            }
            "instanceoverride" => CHECK_PARTICLES,
            "__model" | "animationlayers" | "__boneOverrides" | "__bonePhysics" => CHECK_MODEL,
            "__texture" => CHECK_VIDEO,
            "camera" | "zoom" => CHECK_PROJECTION,
            "fov" => CHECK_CAMERA | CHECK_PROJECTION,
            "__scene"
            | "_perspective"
            | "cameraTransforms"
            | "__cameraPose"
            | "__cameraOverride"
            | "nearz"
            | "farz"
            | "perspectiveoverridefov" => CHECK_CAMERA,
            "id" | "attachment" | "disablepropagation" | "reflected" | "solid" | "__layerOrder"
            | "effects" | "__materials" | "text" | "__sound" | "__boneOverrideTime"
            | "__animations" => 0,
            key if state_field(key) => 0,
            // New or unfamiliar fields retain the full validation path.
            _ => CHECK_ALL,
        }
    }

    pub(crate) struct Runtime {
        pub objects: Vec<Value>,
        pub attachments: HashMap<usize, Vec<glam::Mat4>>,
        pub(crate) authored: Vec<Value>,
        hierarchy: Hierarchy,
        pub scripts: Option<Scripts>,
        pub animations: crate::animation::Animations,
        pub camera: crate::camera::Controller,
        pub created: Vec<usize>,
        pub structure_dirty: bool,
        changes: HashMap<(usize, Vec<String>), Value>,
        validated: Vec<u8>,
        errors: VecDeque<String>,
        error_generation: u64,
        size: [f32; 2],
        pub nodes: Vec<scene::Node>,
    }
    impl Runtime {
        pub fn parents(&self) -> &[Option<usize>] {
            self.hierarchy.parents()
        }
        pub fn propagates(&self, node: usize) -> bool {
            self.hierarchy.propagates(node)
        }
        pub fn order(&self) -> &[usize] {
            self.hierarchy.order()
        }
        pub fn compose_node(&self, node: usize, states: &mut [State]) -> Result<()> {
            self.hierarchy.compose_node(node, states, &self.attachments)
        }
        pub fn new(
            assets: &Assets,
            objects: &[Value],
            properties: &Properties,
            size: [f32; 2],
            storage: Option<crate::ScriptStorage>,
            initialize_scripts: bool,
        ) -> Result<Self> {
            let mut authored = objects.to_vec();
            for object in &mut authored {
                let default = if object["parent"].is_null() {
                    json!([size[0] / 2.0, size[1] / 2.0, 0])
                } else {
                    json!([0, 0, 0])
                };
                let values = object
                    .as_object_mut()
                    .context("scene object must be an object")?;
                values.entry("origin").or_insert(default);
            }
            let mut animations = crate::animation::Animations::new(&mut authored)?;
            animations.properties(&authored, properties)?;
            let mut values = authored
                .iter()
                .map(|v| resolve(v, properties))
                .collect::<Result<Vec<_>>>()?;
            let hierarchy = Hierarchy::new(&values)?;
            for object in &values {
                crate::lighting::validate(object)?;
                crate::particles::validate_controls(object)?;
            }
            let mut camera = crate::camera::Controller::new(assets, &values)?;
            if camera.present() {
                let mut states = values
                    .iter()
                    .map(|object| scene::local_state(object, size, !object["parent"].is_null()))
                    .collect::<Result<Vec<_>>>()?;
                hierarchy.compose(&mut states, &HashMap::new())?;
                camera.update(&mut values, &states, 0.)?;
                for (authored, value) in authored.iter_mut().zip(&values) {
                    if value["__cameraPose"].is_object() {
                        authored["__cameraPose"] = value["__cameraPose"].clone();
                    }
                }
            }
            let scripts = if initialize_scripts {
                Scripts::load_with_samples(
                    assets,
                    &authored,
                    properties,
                    size,
                    0x5745,
                    Some(animations.samples.clone()),
                    storage,
                )?
            } else {
                None
            };
            let nodes = values
                .iter()
                .map(|v| scene::Node::decode(v, size))
                .collect::<Result<_>>()?;
            let mut runtime = Self {
                hierarchy,
                validated: vec![0; values.len()],
                objects: values,
                attachments: HashMap::new(),
                authored,
                scripts,
                animations,
                camera,
                created: Vec::new(),
                structure_dirty: false,
                changes: HashMap::new(),
                errors: VecDeque::new(),
                error_generation: 0,
                size,
                nodes,
            };
            if let Some(scripts) = &runtime.scripts {
                let patch = scripts.flush()?;
                runtime.apply(&patch)?;
            }
            runtime.camera.reset();
            runtime.update_camera(0.)?;
            Ok(runtime)
        }
        pub fn states(&self) -> Result<Vec<State>> {
            let mut states = self.local_states()?;
            self.compose_states(&mut states)?;
            Ok(states)
        }
        pub fn local_states(&self) -> Result<Vec<State>> {
            Ok(self.nodes.iter().map(|node| node.state.clone()).collect())
        }
        pub fn alive(&self, node: usize) -> bool {
            self.nodes[node].alive()
        }

        fn resolve_properties(&self, properties: &Properties) -> Result<Vec<Value>> {
            let mut values = self
                .authored
                .iter()
                .map(|v| resolve(v, properties))
                .collect::<Result<Vec<_>>>()?;
            let mut changes = self.changes.iter().collect::<Vec<_>>();
            changes.sort_unstable_by_key(|((_, path), _)| path.len());
            for ((node, path), value) in changes {
                set_value(&mut values[*node], path, value.clone())?;
            }
            Ok(values)
        }

        pub fn preview_properties(&self, properties: &Properties) -> Result<Vec<State>> {
            let values = self.resolve_properties(properties)?;
            let mut states = values
                .iter()
                .map(|v| scene::Node::decode(v, self.size).map(|n| n.state))
                .collect::<Result<Vec<_>>>()?;
            Hierarchy::new(&values)?.compose(&mut states, &self.attachments)?;
            Ok(states)
        }

        pub fn compose_states(&self, states: &mut [State]) -> Result<()> {
            self.hierarchy.compose(states, &self.attachments)
        }
        pub fn frame(&mut self, frame: &impl crate::script::FrameData) -> PatchImpact {
            let length = self.objects.len();
            if let Some(scripts) = &mut self.scripts {
                let result = scripts.frame(frame).and_then(|patch| self.apply(&patch));
                match result {
                    Ok(impact) => return impact,
                    Err(error) => self.error(format!("{error:#}")),
                }
            }
            // A rejected creation still reserves tombstone handles on both sides.
            PatchImpact {
                states: self.objects.len() != length,
                skeletons: false,
            }
        }
        pub fn update_camera(&mut self, time: f64) -> Result<()> {
            if self.camera.present() {
                let states = self.states()?;
                self.update_camera_with_states(time, &states)?;
            }
            Ok(())
        }
        pub fn update_camera_with_states(&mut self, time: f64, states: &[State]) -> Result<()> {
            self.camera.update(&mut self.objects, states, time)
        }
        pub fn animate(&mut self, time: f32) -> Result<Vec<Value>> {
            let patches = match self.animations.advance(time as f64, &mut self.objects) {
                Ok(patches) => patches,
                Err(error) => {
                    self.validated.fill(0);
                    return Err(error);
                }
            };
            for patch in &patches {
                if let (Some(node), Some(key)) = (patch[0].as_u64(), patch[1][0].as_str()) {
                    self.validated[node as usize] &= !control_checks(key, patch[1][1].as_str());
                }
            }
            let changed = patches
                .iter()
                .filter(|p| p[1][0].as_str().is_none_or(node_field))
                .filter_map(|p| p[0].as_u64())
                .collect::<BTreeSet<_>>();
            for node in changed {
                self.nodes[node as usize] =
                    scene::Node::decode(&self.objects[node as usize], self.size)?;
            }
            Ok(patches)
        }
        pub fn properties(&mut self, properties: &Properties) -> Result<()> {
            let values = self.resolve_properties(properties)?;
            let nodes = values
                .iter()
                .map(|v| scene::Node::decode(v, self.size))
                .collect::<Result<Vec<_>>>()?;
            let hierarchy = Hierarchy::new(&values)?;
            for object in &values {
                crate::lighting::validate(object)?;
                crate::particles::validate_controls(object)?;
            }
            self.animations.properties(&self.authored, properties)?;
            self.objects = values;
            self.validated.fill(0);
            self.nodes = nodes;
            self.hierarchy = hierarchy;
            self.update_camera(self.camera.time())?;
            if let Some(scripts) = &mut self.scripts {
                scripts.sync_nodes(&self.objects)?;
                let patch = scripts.set_properties(properties)?;
                self.apply(&patch)?;
            }
            Ok(())
        }
        pub fn event(&mut self, name: &str, value: &Value) {
            if let Some(scripts) = &mut self.scripts {
                let result = scripts
                    .event(name, value)
                    .and_then(|patch| self.apply(&patch));
                if let Err(error) = result {
                    self.error(format!("{error:#}"));
                }
            }
        }
        pub fn diagnostics(&self) -> Vec<String> {
            self.errors
                .iter()
                .cloned()
                .chain(self.scripts.iter().flat_map(Scripts::diagnostics))
                .collect()
        }
        pub fn errors(&self) -> Vec<String> {
            self.errors
                .iter()
                .cloned()
                .chain(
                    self.scripts
                        .iter()
                        .flat_map(Scripts::diagnostics)
                        .filter(|s| !s.starts_with("console.")),
                )
                .collect()
        }
        pub(crate) fn error(&mut self, message: String) {
            self.error_generation = self.error_generation.wrapping_add(1);
            if self.errors.len() == 32 {
                self.errors.pop_front();
            }
            self.errors.push_back(message);
        }
        pub fn error_generation(&self) -> u64 {
            self.error_generation
                .wrapping_add(self.scripts.as_ref().map_or(0, Scripts::error_generation))
        }
        pub(crate) fn reject_created(&mut self, nodes: &[usize]) -> Result<()> {
            for node in nodes {
                self.objects[*node]["__destroyed"] = json!(true);
                self.objects[*node]["visible"] = json!(false);
                self.nodes[*node] = scene::Node::decode(&self.objects[*node], self.size)?;
                self.changes
                    .insert((*node, vec!["__destroyed".into()]), json!(true));
                self.changes
                    .insert((*node, vec!["visible".into()]), json!(false));
            }
            self.hierarchy = Hierarchy::new(&self.objects)?;
            self.structure_dirty = true;
            if let Some(scripts) = &self.scripts {
                scripts.sync_nodes(&self.objects)?;
            }
            Ok(())
        }
        pub(crate) fn apply(&mut self, patch: &Value) -> Result<PatchImpact> {
            let length = self.objects.len();
            let mut undo = Vec::new();
            let result = self.apply_patch(patch, &mut undo);
            if result.is_err() {
                for (node, path, value) in undo.into_iter().rev() {
                    replace_value(&mut self.objects[node], &path, value)?;
                }
                self.objects.truncate(length);
            }
            if result.is_err()
                && let Some(scripts) = &self.scripts
            {
                // Keep rejected creation slots as tombstones, so old handles never revive.
                let length = patch
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|p| p[1].as_array().is_some_and(Vec::is_empty))
                    .filter_map(|p| p[0].as_u64())
                    .filter(|index| *index < 4096)
                    .max()
                    .map_or(self.objects.len(), |index| index as usize + 1);
                while self.objects.len() < length {
                    let tombstone = json!({"origin":[0,0,0],"visible":false,"__destroyed":true});
                    self.authored.push(tombstone.clone());
                    self.nodes.push(scene::Node::decode(&tombstone, self.size)?);
                    self.objects.push(tombstone);
                    self.validated.push(0);
                }
                if self.objects.len() != self.hierarchy.parents().len() {
                    self.hierarchy = Hierarchy::new(&self.objects)?;
                }
                // Invalid authored topology/property updates are atomic on both
                // sides of the host boundary, preserving existing vector handles.
                scripts.sync_nodes(&self.objects)?;
            }
            result
        }
        fn apply_patch(
            &mut self,
            patch: &Value,
            undo: &mut Vec<(usize, Vec<String>, Option<Value>)>,
        ) -> Result<PatchImpact> {
            let values = patch
                .as_array()
                .context("SceneScript patch is not an array")?;
            if values.is_empty() {
                return Ok(PatchImpact::default());
            }
            // Keep only the replaced fields for rollback. No script or renderer
            // observes these writes until the complete batch has been validated.
            let mut objects = BTreeMap::new();
            let mut node_objects = BTreeSet::new();
            let mut previous_states = BTreeMap::new();
            let mut topology_changed = false;
            let mut skeletons_changed = false;
            let initial_length = self.objects.len();
            let mut length = initial_length;
            let mut changes = Vec::new();
            let mut definitions = Vec::new();
            for entry in values {
                let node = entry[0].as_u64().context("invalid SceneScript node")? as usize;
                let path = entry[1]
                    .as_array()
                    .context("invalid SceneScript property path")?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .context("invalid property path component")
                    })
                    .collect::<Result<Vec<_>>>()?;
                if path.is_empty() {
                    ensure!(
                        node == length && node < 4096,
                        "invalid dynamic SceneScript node"
                    );
                    let definition = &entry[2]["definition"];
                    let value = &entry[2]["values"];
                    ensure!(
                        definition.is_object() && value.is_object() && value["__scene"] != true,
                        "invalid dynamic layer definition"
                    );
                    definitions.push((node, definition.clone()));
                    self.objects.push(value.clone());
                    objects.insert(node, CHECK_ALL);
                    node_objects.insert(node);
                    topology_changed = true;
                    skeletons_changed = true;
                    length += 1;
                    continue;
                }
                ensure!(node < length, "SceneScript node is outside scene");
                ensure!(
                    path.len() <= 32
                        && !path
                            .iter()
                            .any(|v| v == "__proto__" || v == "prototype" || v == "constructor"),
                    "invalid SceneScript property path"
                );
                let current = &self.objects[node];
                let unchanged = path.iter().try_fold(current, |value, key| match value {
                    Value::Array(array) => array.get(key.parse::<usize>().ok()?),
                    Value::Object(map) => map.get(key),
                    _ => None,
                }) == Some(&entry[2]);
                if !unchanged {
                    let key = path[0].as_str();
                    topology_changed |= matches!(
                        key,
                        "id" | "parent" | "attachment" | "disablepropagation" | "__model"
                    );
                    skeletons_changed |=
                        matches!(key, "animationlayers" | "__boneOverrides" | "__bonePhysics");
                    if node < initial_length
                        && state_field(key)
                        && !previous_states.contains_key(&node)
                    {
                        previous_states.insert(node, self.nodes[node].state.clone());
                    }
                    let previous =
                        replace_value(&mut self.objects[node], &path, Some(entry[2].clone()))?;
                    undo.push((node, path.clone(), previous));
                    *objects.entry(node).or_insert(0) |=
                        control_checks(key, path.get(1).map(String::as_str));
                    if node_field(key) {
                        node_objects.insert(node);
                    }
                }
                // An identical write still freezes that authored property across
                // future user-property changes, so retain its override transaction.
                changes.push(((node, path), entry[2].clone()));
            }
            let mut impact = PatchImpact {
                states: topology_changed,
                skeletons: skeletons_changed,
            };
            let mut states = Vec::with_capacity(objects.len());
            for (&node, &dirty) in &objects {
                let object = &self.objects[node];
                // First use checks every category. Only complete batches certify
                // values; curves and property updates invalidate their dependencies.
                let checks = dirty | (CHECK_ALL & !self.validated.get(node).copied().unwrap_or(0));
                if node_objects.contains(&node) {
                    let node_value = scene::Node::decode(object, self.size)?;
                    if let Some(previous) = previous_states.get(&node) {
                        impact.states |= &node_value.state != previous;
                    }
                    states.push((node, node_value));
                }
                if checks & CHECK_LIGHT != 0 {
                    crate::lighting::validate(object)?;
                }
                if checks & CHECK_PARTICLES != 0 {
                    crate::particles::validate_controls(object)?;
                }
                if checks & CHECK_MODEL != 0 && !object["__model"].is_null() {
                    crate::mdl::validate_controls(object)?;
                }
                if checks & CHECK_VIDEO != 0 && object["__texture"]["video"] == true {
                    crate::scene_media::video_clock::validate(&object["__texture"])?;
                }
                if checks & CHECK_CAMERA != 0 && object["__scene"] == true {
                    crate::camera::Camera::prepare(object)?;
                }
                if checks & CHECK_PROJECTION != 0 && object["camera"].is_string() {
                    let zoom = crate::scene::bindings::components(&object["zoom"], &[1.], 1)?[0];
                    let fov = crate::scene::bindings::components(&object["fov"], &[50.], 1)?[0];
                    ensure!(
                        zoom > 0. && zoom <= 1000. && (fov == 0. || (0.01..179.).contains(&fov)),
                        "invalid camera layer projection"
                    );
                }
            }
            let hierarchy = if topology_changed {
                Some(Hierarchy::new(&self.objects)?)
            } else {
                None
            };
            impact.states |= topology_changed;
            if let Some(hierarchy) = hierarchy {
                self.hierarchy = hierarchy;
            }
            // No core facts are committed until the entire patch and topology validate.
            self.validated.resize(self.objects.len(), 0);
            for &node in objects.keys() {
                self.validated[node] = CHECK_ALL;
            }
            for (node, value) in states {
                if node == self.nodes.len() {
                    self.nodes.push(value);
                } else {
                    self.nodes[node] = value;
                }
            }
            for (node, definition) in definitions {
                self.authored.push(definition);
                for key in ["__texture", "__videoMaster"] {
                    if let Some(value) = self.objects[node].get(key) {
                        self.changes.insert((node, vec![key.into()]), value.clone());
                    }
                }
                self.created.push(node);
                self.structure_dirty = true;
            }
            for ((node, path), value) in changes {
                self.structure_dirty |= matches!(
                    path.first().map(String::as_str),
                    Some("__destroyed" | "__layerOrder")
                );
                self.changes
                    .retain(|(owner, child), _| *owner != node || !child.starts_with(&path));
                self.changes.insert((node, path), value);
            }
            Ok(impact)
        }
    }
    pub(crate) fn set_value(object: &mut Value, path: &[String], value: Value) -> Result<()> {
        replace_value(object, path, Some(value)).map(|_| ())
    }
    fn replace_value(
        object: &mut Value,
        path: &[String],
        value: Option<Value>,
    ) -> Result<Option<Value>> {
        let mut target = object;
        for key in &path[..path.len() - 1] {
            target = match target {
                Value::Array(array) => array.get_mut(key.parse::<usize>()?),
                Value::Object(map) => map.get_mut(key),
                _ => None,
            }
            .context("SceneScript path refers to a missing object")?;
        }
        let key = &path[path.len() - 1];
        match target {
            Value::Object(map) => Ok(match value {
                Some(value) => map.insert(key.clone(), value),
                None => map.remove(key),
            }),
            Value::Array(array) => {
                let target = array
                    .get_mut(key.parse::<usize>()?)
                    .context("SceneScript array index outside object")?;
                Ok(Some(std::mem::replace(
                    target,
                    value.context("cannot remove an array element")?,
                )))
            }
            _ => anyhow::bail!("SceneScript property owner is not an object"),
        }
    }

    fn state_field(key: &str) -> bool {
        matches!(
            key,
            "origin"
                | "scale"
                | "angles"
                | "brightness"
                | "alpha"
                | "color"
                | "parallaxDepth"
                | "colorBlendMode"
                | "visible"
                | "__destroyed"
                | "perspective"
                | "parent"
        )
    }

    fn node_field(key: &str) -> bool {
        // These open content/command fields do not contribute to the normalized
        // Node. Unknown fields still take the full validation/decode path.
        !matches!(
            key,
            "effects"
                | "__materials"
                | "text"
                | "__particle"
                | "instanceoverride"
                | "__sound"
                | "animationlayers"
                | "__boneOverrides"
                | "__boneOverrideTime"
                | "__bonePhysics"
                | "__animations"
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        fn runtime(objects: &[Value]) -> Runtime {
            let root = tempfile::tempdir().unwrap();
            let package = root.path().join("scene.pkg");
            let mut bytes = 8u32.to_le_bytes().to_vec();
            bytes.extend(b"PKGV0001");
            bytes.extend(0u32.to_le_bytes());
            std::fs::write(&package, bytes).unwrap();
            let assets = Assets::open(root.path(), &package, None).unwrap();
            Runtime::new(&assets, objects, &Properties::new(), [10.; 2], None, false).unwrap()
        }

        #[test]
        fn normalized_nodes_follow_animation_properties_scripts_and_atomic_rejection() {
            let mut runtime = runtime(&[json!({"id":1,"light":"point","radius":10,
                "color":{"user":"tint","value":"1 1 1"},
                "origin":{"value":[0,0,0],"animation":{"c0":[{"frame":0,"value":0},{"frame":2,"value":8}],"c1":[{"frame":0,"value":0}],"c2":[{"frame":0,"value":0}],"options":{"fps":2,"length":2,"mode":"single"}}}})]);
            runtime.animate(0.).unwrap();
            runtime.animate(0.25).unwrap();
            assert_eq!(runtime.nodes[0].state.transform.w_axis.x, 2.);
            let properties = Properties::from([("tint".into(), json!([0.5, 0.25, 0]))]);
            runtime.properties(&properties).unwrap();
            assert_eq!(runtime.nodes[0].state.tint, glam::Vec3::new(0.5, 0.25, 0.));
            runtime
                .apply(&json!([[0,["origin"],[4,0,0]], [0,["color"],[0,1,0]],
                [0,["__texture"],{"playing":false,"joined":false,"position":3,"rate":2,"anchor":1}],
                [0,["__layerOrder"],[0]], [0,["castshadow"],false]]))
                .unwrap();
            runtime.properties(&properties).unwrap();
            assert_eq!(runtime.nodes[0].state.tint, glam::Vec3::Y);
            assert_eq!(runtime.nodes[0].texture.phase(20.), 3.);
            assert_eq!(runtime.nodes[0].layer_order, [0]);
            assert!(!runtime.nodes[0].casts_shadow);
            let lights = crate::lighting::Snapshot::collect(
                &runtime.nodes,
                &runtime.states().unwrap(),
                [0; 4],
            )
            .unwrap();
            assert_eq!(&lights.position[..3], &[4., 0., 0.]);
            assert_eq!(&lights.color_radius[..4], &[0., 1., 0., 10.]);
            assert!(
                runtime
                    .apply(&json!([[0, ["origin"], [9, 0, 0]], [0, ["radius"], -1]]))
                    .is_err()
            );
            assert_eq!(runtime.nodes[0].state.transform.w_axis.x, 4.);
            runtime.apply(&json!([[0, ["__destroyed"], true]])).unwrap();
            assert!(!runtime.alive(0));
            assert!(!runtime.states().unwrap()[0].visible);
        }

        #[test]
        fn patches_preserve_unwritten_objects_and_topology_and_roll_back_the_whole_batch() {
            let mut runtime = runtime(&[
                json!({"id":1,"origin":[0,0,0],"alpha":1,"__particle":{"commands":[]},"untouched":[1,2,3]}),
                json!({"id":2,"parent":1,"origin":[2,0,0],"untouched":[1,2,3]}),
            ]);
            let untouched = runtime.objects[1]["untouched"].as_array().unwrap().as_ptr();
            let parents = runtime.parents().as_ptr();
            let same = runtime.objects[0]["untouched"].as_array().unwrap().as_ptr();
            let impact = runtime.apply(&json!([[0, ["alpha"], 1]])).unwrap();
            assert!(!impact.states && !impact.skeletons);
            assert_eq!(
                runtime.objects[0]["untouched"].as_array().unwrap().as_ptr(),
                same
            );
            assert_eq!(runtime.changes[&(0, vec!["alpha".into()])], json!(1));
            let impact = runtime
                .apply(&json!([[0, ["__particle", "commands"], []]]))
                .unwrap();
            assert!(!impact.states && !impact.skeletons);
            let impact = runtime
                .apply(&json!([[0, ["animationlayers"], []]]))
                .unwrap();
            assert!(!impact.states && impact.skeletons);
            runtime.apply(&json!([[0, ["alpha"], 0.5]])).unwrap();
            assert_eq!(
                runtime.objects[0]["untouched"].as_array().unwrap().as_ptr(),
                same
            );
            let impact = runtime
                .apply(&json!([
                    [0, ["origin"], [3, 0, 0]],
                    [0, ["origin", "0"], 4]
                ]))
                .unwrap();
            assert!(impact.states && !impact.skeletons);
            assert_eq!(runtime.states().unwrap()[1].transform.w_axis.x, 6.);
            assert_eq!(
                runtime.objects[1]["untouched"].as_array().unwrap().as_ptr(),
                untouched
            );
            assert_eq!(runtime.parents().as_ptr(), parents);

            let previous = runtime.objects.clone();
            let changes = runtime.changes.clone();
            for patch in [
                json!([[0, ["alpha"], 0.5], [1, ["scale"], "invalid"]]),
                json!([[0, ["alpha"], 0.5], [0, ["parent"], 2]]),
                json!([[0, ["id"], 9]]), // The unwritten child's parent must still resolve.
                json!([[0,["temporary"],{"nested":[1,2]}],[0,["temporary","nested","0"],3],[1,["scale"],"invalid"]]),
                json!([
                    [0, ["untouched", "0"], 9],
                    [0, ["untouched"], [4, 5]],
                    [1, ["scale"], "invalid"]
                ]),
                json!([[2,[],{"definition":{"id":3},"values":{"id":3,"parent":2}}],[2,["parent"],3]]),
            ] {
                assert!(runtime.apply(&patch).is_err());
                assert_eq!(runtime.objects, previous);
                assert_eq!(runtime.changes, changes);
                assert_eq!(runtime.authored.len(), 2);
                assert!(runtime.created.is_empty());
                assert!(!runtime.structure_dirty);
            }
            let impact = runtime.apply(&json!([
            [2,[],{"definition":{"id":3,"parent":2},"values":{"id":3,"parent":2,"origin":"invalid until the following write"}}],
            [2,["origin"],[5,0,0]],
            [0,["origin","0"],10]
        ])).unwrap();
            assert!(impact.states && impact.skeletons);
            assert_eq!(runtime.parents(), &[None, Some(0), Some(1)]);
            assert_eq!(runtime.states().unwrap()[2].transform.w_axis.x, 17.);
            assert_eq!(runtime.created, [2]);
            assert!(runtime.structure_dirty);
        }

        #[test]
        fn patch_control_checks_reject_each_dependency_and_restore_the_batch() {
            let mut runtime = runtime(&[
                json!({"id":1,"alpha":1,"light":"point","radius":10,"intensity":1,
                    "__particle":{"commands":[]},"instanceoverride":{},
                    "__model":{"bones":[{}]},"animationlayers":[{"rate":1,"blend":1}],
                    "__boneOverrides":{},"__bonePhysics":{},
                    "__texture":{"video":true,"rate":1,"position":0,"anchor":0},
                    "camera":"camera","zoom":1,"fov":50}),
                json!({"id":2,"__scene":true,"fov":50,"nearz":0.01,"farz":10000}),
            ]);
            runtime
                .apply(&json!([[0, ["alpha"], 0.5], [1, ["alpha"], 0.5]]))
                .unwrap();
            let previous = runtime.objects.clone();
            let state = runtime.nodes[0].state.clone();
            for invalid in [
                json!([0, ["radius"], -1]),
                json!([0, ["__particle", "commands"], [["emit", 20001]]]),
                json!([0, ["instanceoverride", "controlpoint0"], "invalid"]),
                json!([0, ["animationlayers", "0", "rate"], 129]),
                json!([0,["__boneOverrides"],{"0":[1,2]}]),
                json!([0,["__bonePhysics"],{"1":{"revision":1,"time":0}}]),
                json!([0, ["__texture", "rate"], 101]),
                json!([0, ["zoom"], 0]),
                json!([1, ["farz"], 0.001]),
            ] {
                assert!(
                    runtime
                        .apply(&json!([[0, ["alpha"], 0.25], invalid]))
                        .is_err()
                );
                assert_eq!(runtime.objects, previous);
                assert!(runtime.nodes[0].state == state);
            }
            runtime
                .apply(&json!([
                    [0, ["animationlayers", "0", "rate"], 129],
                    [0, ["animationlayers", "0", "rate"], 2],
                    [0, ["brightness"], 2],
                    [0, ["color"], [0.25, 0.5, 1]],
                ]))
                .unwrap();
            assert_eq!(
                runtime.nodes[0].state.color,
                glam::Vec4::new(0.5, 1., 2., 0.5)
            );
            assert_eq!(runtime.nodes[0].state.transform, state.transform);
        }

        #[test]
        fn patch_checks_cover_activation_initial_values_and_rejected_repairs() {
            for (object, activation) in [
                (
                    json!({"light":null,"intensity":"invalid"}),
                    json!([0, ["light"], "point"]),
                ),
                (
                    json!({"animationlayers":[{"rate":129}]}),
                    json!([0,["__model"],{"bones":[]}]),
                ),
                (
                    json!({"instanceoverride":{"controlpoint0":"invalid"}}),
                    json!([0,["__particle"],{"commands":[]}]),
                ),
                (
                    json!({"__texture":{"video":false,"rate":101}}),
                    json!([0, ["__texture", "video"], true]),
                ),
                (json!({"zoom":0}), json!([0, ["camera"], "camera"])),
                (json!({"fov":180}), json!([0, ["__scene"], true])),
            ] {
                let mut runtime = runtime(&[object]);
                runtime.apply(&json!([[0, ["alpha"], 0.5]])).unwrap();
                let previous = runtime.objects.clone();
                assert!(runtime.apply(&json!([activation])).is_err());
                assert_eq!(runtime.objects, previous);
            }
            let mut runtime = runtime(&[
                json!({"__model":{"bones":[]},"animationlayers":[{"rate":129}]}),
                json!({}),
            ]);
            assert!(runtime.apply(&json!([[0, ["alpha"], 0.5]])).is_err());
            let previous = runtime.objects.clone();
            assert!(
                runtime
                    .apply(&json!([
                        [0, ["animationlayers", "0", "rate"], 1],
                        [1, ["scale"], "invalid"],
                    ]))
                    .is_err()
            );
            assert_eq!(runtime.objects, previous);
            assert!(runtime.apply(&json!([[0, ["alpha"], 0.5]])).is_err());
        }

        #[test]
        fn patch_checks_follow_author_curves_and_user_property_updates() {
            let mut properties = runtime(&[json!({"__model":{"bones":[]},
                "animationlayers":[{"rate":{"user":"speed","value":1}}]})]);
            properties.apply(&json!([[0, ["alpha"], 0.5]])).unwrap();
            properties
                .properties(&Properties::from([("speed".into(), json!(129))]))
                .unwrap();
            assert!(properties.apply(&json!([[0, ["alpha"], 0.25]])).is_err());

            let mut animated = runtime(&[json!({"__model":{"bones":[]},
                "animationlayers":[{"rate":{"value":1,"animation":{
                    "c0":[{"frame":0,"value":1},{"frame":2,"value":999}],
                    "options":{"fps":2,"length":2,"mode":"single"}}}}]})]);
            animated.animate(0.).unwrap();
            animated.apply(&json!([[0, ["alpha"], 0.5]])).unwrap();
            animated.animate(0.5).unwrap();
            assert!(
                animated.objects[0]["animationlayers"][0]["rate"]
                    .as_f64()
                    .unwrap()
                    > 128.
            );
            assert!(animated.apply(&json!([[0, ["alpha"], 0.25]])).is_err());
        }

        #[test]
        fn parent_child_mutations_survive_properties_and_parent_replacement_discards_old_children()
        {
            let root = tempfile::tempdir().unwrap();
            let package = root.path().join("scene.pkg");
            let mut bytes = 8u32.to_le_bytes().to_vec();
            bytes.extend(b"PKGV0001");
            bytes.extend(0u32.to_le_bytes());
            std::fs::write(&package, bytes).unwrap();
            let assets = Assets::open(root.path(), &package, None).unwrap();
            let objects = [
                json!({"id":1,"origin":[0,0,0],"effects":[{"visible":true,"passes":[{"constantshadervalues":{"color":[1,1,1],"amount":0}}]}]}),
            ];
            let mut runtime =
                Runtime::new(&assets, &objects, &Properties::new(), [10.; 2], None, false).unwrap();
            let replacement = json!([{"visible":true,"passes":[{"constantshadervalues":{"color":[0.2,0.3,0.4],"amount":1}}]}]);
            runtime
                .apply(&json!([[0, ["effects"], replacement]]))
                .unwrap();
            runtime
                .apply(&json!([[
                    0,
                    [
                        "effects",
                        "0",
                        "passes",
                        "0",
                        "constantshadervalues",
                        "color"
                    ],
                    [0.8, 0.7, 0.6]
                ]]))
                .unwrap();
            for _ in 0..16 {
                runtime.properties(&Properties::new()).unwrap();
                assert_eq!(
                    runtime.objects[0]["effects"][0]["passes"][0]["constantshadervalues"]["color"],
                    json!([0.8, 0.7, 0.6])
                );
            }
            runtime
                .apply(&json!([[0,["effects"],[{"visible":false,"passes":[]}]]]))
                .unwrap();
            runtime.properties(&Properties::new()).unwrap();
            assert_eq!(
                runtime.objects[0]["effects"],
                json!([{"visible":false,"passes":[]}])
            );
            let previous = runtime.objects.clone();
            assert!(runtime.apply(&json!([[0, ["parent"], 1]])).is_err());
            assert_eq!(runtime.objects, previous);
            runtime.properties(&Properties::new()).unwrap();
            assert_eq!(runtime.objects, previous);
        }
    }
}
