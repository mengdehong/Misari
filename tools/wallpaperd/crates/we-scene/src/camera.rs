//! Orthographic content, perspective layers, and full 3D cameras share one view calculation.

use crate::{assets::Assets, scene::State, scene::bindings::components};
use anyhow::{Context, Result, ensure};
use glam::{Mat3, Mat4, Vec3};
use paths::{Queue, Recorded};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
struct Pose {
    eye: Vec3,
    center: Vec3,
    up: Vec3,
    fov: f32,
    zoom: f32,
}
impl Default for Pose {
    fn default() -> Self {
        Self {
            eye: Vec3::new(0., 0., 1000.),
            center: Vec3::ZERO,
            up: Vec3::Y,
            fov: 50.,
            zoom: 1.,
        }
    }
}
impl Pose {
    fn set(&mut self, index: usize, value: &[f32]) {
        match index {
            0 => self.eye = Vec3::from_slice(value),
            1 => self.center = Vec3::from_slice(value),
            2 => self.up = Vec3::from_slice(value),
            3 => self.fov = value[0],
            _ => self.zoom = value[0],
        }
    }
    fn validate(self) -> Result<()> {
        ensure!(
            self.eye.is_finite()
                && self.center.is_finite()
                && self.up.is_finite()
                && self.up.length_squared() > 1e-12,
            "invalid camera pose"
        );
        ensure!(
            (0.01..179.).contains(&self.fov) && self.zoom > 0. && self.zoom <= 1000.,
            "invalid camera projection"
        );
        Ok(())
    }

    fn from_settings(value: &Value) -> Result<Self> {
        let transform = &value["cameraTransforms"];
        let vec = |name: &str, default: [f32; 3]| -> Result<Vec3> {
            Ok(Vec3::from_slice(&components(
                &transform[name],
                &default,
                3,
            )?))
        };
        let pose = Self {
            eye: vec("eye", [0., 0., 1000.])?,
            center: vec("center", [0.; 3])?,
            up: vec("up", [0., 1., 0.])?,
            zoom: components(&transform["zoom"], &[1.], 1)?[0],
            fov: components(&value["fov"], &[50.], 1)?[0],
        };
        pose.validate()?;
        Ok(pose)
    }
    pub fn value(self) -> Value {
        json!({"eye":self.eye.to_array(),"center":self.center.to_array(),"up":self.up.to_array(),"zoom":self.zoom,"fov":self.fov})
    }
}
pub(crate) struct Camera {
    pub perspective: bool,
    eye: Vec3,
    center: Vec3,
    up: Vec3,
    fov: f32,
    near: f32,
    far: f32,
    zoom: f32,
    layer_fov: f32,
}
#[derive(Clone, Copy)]
pub(crate) struct View {
    pub scale: [f32; 2],
    pub orthographic: Mat4,
    pub perspective: Mat4,
    pub eye: Vec3,
    pub basis: Mat3,
    pub full_3d: bool,
}
impl View {
    /// Output coordinates and scene coordinates both use a top-left origin here.
    pub fn scene_pointer(&self, screen: [f32; 2], output: [u32; 2], scene: [f32; 2]) -> [f32; 2] {
        std::array::from_fn(|axis| {
            0.5 + (screen[axis] - 0.5) * output[axis] as f32 / (scene[axis] * self.scale[axis])
        })
    }
    pub fn matrix(&self, perspective: bool) -> Mat4 {
        if self.full_3d || perspective {
            self.perspective
        } else {
            self.orthographic
        }
    }
}
impl Camera {
    pub fn prepare(value: &Value) -> Result<Self> {
        let scalar = |name: &str, default: f32| -> Result<f32> {
            Ok(components(&value[name], &[default], 1)?[0])
        };
        let pose = value["__cameraPose"].is_object() && value["__cameraOverride"] != true;
        let transform = if pose {
            &value["__cameraPose"]
        } else {
            &value["cameraTransforms"]
        };
        let vector = |name: &str, default: [f32; 3]| -> Result<Vec3> {
            Ok(Vec3::from_slice(&components(
                &transform[name],
                &default,
                3,
            )?))
        };
        let perspective = value["_perspective"].as_bool().unwrap_or(false);
        let eye = vector("eye", [0.0, 0.0, 1000.0])?;
        let center = vector("center", [0.0; 3])?;
        let up = vector("up", [0.0, 1.0, 0.0])?;
        let fov = if pose {
            components(&transform["fov"], &[50.], 1)?[0]
        } else {
            scalar("fov", 50.0)?
        };
        let near = scalar("nearz", 0.01)?;
        let far = scalar("farz", 10000.0)?;
        let zoom = components(&transform["zoom"], &[1.0], 1)?[0];
        let layer_fov = scalar("perspectiveoverridefov", 0.0)?;
        ensure!(zoom > 0.0 && zoom <= 1000.0, "invalid camera zoom");
        ensure!(
            (0.01..179.0).contains(&fov) && near > 0.0 && far > near,
            "invalid camera projection"
        );
        ensure!(
            layer_fov == 0.0 || (0.01..179.0).contains(&layer_fov),
            "invalid layer perspective FOV"
        );
        if perspective {
            ensure!(
                (eye - center).cross(up).length_squared() > 1e-12,
                "degenerate scene camera"
            );
        }
        Ok(Self {
            perspective,
            eye,
            center,
            up,
            fov,
            near,
            far,
            zoom,
            layer_fov,
        })
    }
    pub fn view(&self, size: [f32; 2], output: [u32; 2], fit: Fit) -> View {
        let mut scale = fit.scale(size, output.map(|v| v as f32));
        if !self.perspective {
            for s in &mut scale {
                *s *= self.zoom;
            }
        }
        let transform = Mat4::from_translation(Vec3::new(
            output[0] as f32 / 2.0,
            output[1] as f32 / 2.0,
            0.0,
        )) * Mat4::from_scale(Vec3::new(scale[0], scale[1], 1.0))
            * Mat4::from_translation(Vec3::new(-size[0] / 2.0, -size[1] / 2.0, 0.0));
        let orthographic = Mat4::orthographic_rh_gl(
            0.0,
            output[0] as f32,
            0.0,
            output[1] as f32,
            -10000.0,
            10000.0,
        ) * transform;
        if self.perspective {
            let view = Mat4::look_at_rh(self.eye, self.center, self.up.normalize());
            View {
                scale,
                orthographic,
                perspective: Mat4::perspective_rh_gl(
                    self.fov.to_radians(),
                    output[0] as f32 / output[1] as f32,
                    self.near,
                    self.far,
                ) * view,
                eye: self.eye,
                basis: Mat3::from_mat4(view).transpose(),
                full_3d: true,
            }
        } else {
            let h = output[1] as f32 / scale[1];
            let w = output[0] as f32 / scale[0];
            let distance = if self.layer_fov > 0.0 {
                h / 2.0 / (self.layer_fov.to_radians() / 2.0).tan()
            } else {
                1000.0
            };
            let fov = 2.0 * (h / 2.0 / distance).atan();
            let eye = Vec3::new(size[0] / 2.0, size[1] / 2.0, distance);
            View {
                scale,
                orthographic,
                perspective: Mat4::perspective_rh_gl(fov, w / h, 1.0, distance + 20000.0)
                    * Mat4::from_translation(-eye),
                eye,
                basis: Mat3::IDENTITY,
                full_3d: false,
            }
        }
    }
}

pub use wallpaper_media::Fit;

struct Entity {
    node: usize,
    path: Option<Queue>,
}
impl Entity {
    fn new(assets: &Assets, node: usize, object: &Value) -> Result<Self> {
        let path = object["path"]
            .as_str()
            .filter(|file| !file.is_empty())
            .map(|file| {
                Queue::new(
                    &assets.json(file)?,
                    object["queuemode"] == "random",
                    0x57454341 ^ node as u64,
                )
            })
            .transpose()?;
        Ok(Self { node, path })
    }
    fn key_count(&self) -> usize {
        self.path.as_ref().map_or(0, Queue::key_count)
    }
}
pub(crate) struct Addition(Vec<Entity>);
pub(crate) struct Controller {
    settings: Option<usize>,
    entities: Vec<Entity>,
    recorded: Recorded,
    active: Option<usize>,
    time: f64,
    overridden: bool,
    fade_start: Option<f64>,
}
impl Controller {
    pub fn new(assets: &Assets, objects: &[Value]) -> Result<Self> {
        let settings = objects.iter().position(|object| object["__scene"] == true);
        let Some(settings_node) = settings else {
            return Ok(Self {
                settings,
                entities: Vec::new(),
                recorded: Recorded::default(),
                active: None,
                time: 0.,
                overridden: false,
                fade_start: None,
            });
        };
        let settings_value = &objects[settings_node];
        if !objects.iter().any(|object| object["camera"].is_string())
            && settings_value["cameraTransforms"]["paths"]
                .as_array()
                .is_none_or(Vec::is_empty)
        {
            return Ok(Self {
                settings,
                entities: Vec::new(),
                recorded: Recorded::default(),
                active: None,
                time: 0.,
                overridden: false,
                fade_start: None,
            });
        }
        let base = Pose::from_settings(settings_value)?;
        let mut entities = Vec::new();
        let mut keys = 0;
        for (node, object) in objects
            .iter()
            .enumerate()
            .filter(|(_, object)| object["camera"].is_string())
        {
            ensure!(entities.len() < 128, "camera entity budget exceeds 128");
            let entity = Entity::new(assets, node, object)?;
            keys += entity.key_count();
            ensure!(keys <= 200_000, "scene camera key budget exceeded");
            entities.push(entity);
        }
        let mut clips = Vec::new();
        if let Some(paths) = settings_value["cameraTransforms"]["paths"].as_array() {
            ensure!(paths.len() <= 64, "camera path asset budget exceeds 64");
            for path in paths {
                let value = if let Some(file) = path.as_str() {
                    assets.json(file)?
                } else {
                    path.clone()
                };
                if value["duration"].is_number() {
                    clips.push(value);
                } else {
                    clips.extend(
                        value
                            .as_array()
                            .or_else(|| value["paths"].as_array())
                            .context("recorded camera path list")?
                            .iter()
                            .cloned(),
                    );
                }
            }
        }
        let recorded = Recorded::new(&clips, base)?;
        Ok(Self {
            settings,
            entities,
            recorded,
            active: None,
            time: 0.,
            overridden: false,
            fade_start: None,
        })
    }
    pub fn present(&self) -> bool {
        !self.entities.is_empty() || self.recorded.present()
    }
    pub fn prepare_created(
        &self,
        assets: &Assets,
        objects: &[Value],
        nodes: &[usize],
    ) -> Result<Addition> {
        let entities = nodes
            .iter()
            .copied()
            .filter(|node| {
                objects[*node]["camera"].is_string() && objects[*node]["__destroyed"] != true
            })
            .filter(|node| !self.entities.iter().any(|entity| entity.node == *node))
            .map(|node| Entity::new(assets, node, &objects[node]))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            self.entities.len() + entities.len() <= 128,
            "camera entity budget exceeds 128"
        );
        ensure!(
            self.entities
                .iter()
                .chain(&entities)
                .map(Entity::key_count)
                .sum::<usize>()
                <= 200_000,
            "scene camera key budget exceeded"
        );
        Ok(Addition(entities))
    }
    pub fn commit_created(&mut self, addition: Addition) {
        self.entities.extend(addition.0);
    }
    pub fn release_destroyed(&mut self, objects: &[Value]) {
        self.entities
            .retain(|entity| objects[entity.node]["__destroyed"] != true);
    }
    pub fn animated(&self) -> bool {
        !self.overridden
            && self.active.map_or(self.recorded.present(), |node| {
                self.entities
                    .iter()
                    .find(|entity| entity.node == node)
                    .is_some_and(|entity| entity.path.as_ref().is_some_and(Queue::present))
            })
    }
    pub fn time(&self) -> f64 {
        self.time
    }
    pub fn reset(&mut self) {
        self.active = None;
        self.fade_start = None;
        for entity in &mut self.entities {
            if let Some(path) = &mut entity.path {
                path.reset();
            }
        }
    }
    pub fn fade_cover(&self) -> f32 {
        // Compatibility approximation, not an official duration/curve. See
        // CAMERA-TEX-NATIVE.md: the reproducible JS curtain uses 1s smoothstep.
        let Some(start) = self.fade_start else {
            return 0.;
        };
        let t = ((self.time - start) / 1.).clamp(0., 1.) as f32;
        1. - t * t * (3. - 2. * t)
    }
    pub fn update(&mut self, objects: &mut [Value], states: &[State], time: f64) -> Result<()> {
        if !self.present() {
            self.fade_start = None;
            return Ok(());
        }
        ensure!(time.is_finite() && time >= 0., "invalid camera time");
        let settings = self.settings.context("scene camera settings")?;
        let fade_enabled = objects[settings]["camerafade"].as_bool().unwrap_or(false);
        if time < self.time || !fade_enabled {
            self.fade_start = None;
        }
        self.overridden = objects[settings]["__cameraOverride"] == true;
        if self.overridden {
            self.fade_start = None;
            self.time = time;
            return Ok(());
        }
        let active = self
            .entities
            .iter()
            .rev()
            .find(|entity| states[entity.node].visible)
            .map(|entity| entity.node);
        let active_changed = active != self.active;
        if active_changed {
            self.fade_start = None;
            for entity in &mut self.entities {
                if let Some(path) = &mut entity.path {
                    path.reset();
                }
            }
            self.active = active;
        }
        let base = Pose::from_settings(&objects[settings])?;
        let mut pose = base;
        let mut transition = None;
        if let Some(node) = active {
            let world = states[node].transform;
            pose.eye = world.transform_point3(Vec3::ZERO);
            pose.center = pose.eye + world.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
            pose.up = world.transform_vector3(Vec3::Y).normalize_or_zero();
            pose.zoom = components(&objects[node]["zoom"], &[base.zoom], 1)?[0];
            let fov = components(&objects[node]["fov"], &[base.fov], 1)?[0];
            if fov > 0. {
                pose.fov = fov;
            }
            if let Some(path) = &mut self
                .entities
                .iter_mut()
                .find(|entity| entity.node == node)
                .unwrap()
                .path
            {
                path.set_random(objects[node]["queuemode"] == "random");
                if let Some(sample) = path.sample(time, pose)? {
                    pose = sample;
                }
                transition = path.transition;
            }
        } else if let Some(sample) = self.recorded.sample(time) {
            pose = sample;
            if !active_changed {
                transition = self.recorded.transition(self.time, time);
            }
        }
        pose.validate()?;
        let mut value = pose.value();
        value["__path"] = self.animated().into();
        let previous = objects[settings]["__cameraPose"].clone();
        objects[settings]["__cameraPose"] = value;
        if let Err(error) = Camera::prepare(&objects[settings]) {
            objects[settings]["__cameraPose"] = previous;
            return Err(error);
        }
        self.time = time;
        if fade_enabled && let Some(start) = transition {
            self.fade_start = Some(start);
        }
        Ok(())
    }
}

mod paths {
    //! Bounded authored camera playlists, sampled on the scene clock.
    use super::Pose;
    use crate::{animation::Channel, scene::bindings::components};
    use anyhow::{Context, Result, ensure};
    use rand_chacha::ChaCha8Rng;
    use rand_core::{Rng, SeedableRng};
    use serde_json::{Value, json};

    struct Clip {
        fps: f64,
        length: f64,
        mode: String,
        channels: [Option<Channel>; 5],
    }
    impl Clip {
        fn sample(&self, frame: f64, base: Pose) -> Result<Pose> {
            let frame = if self.length == 0. {
                0.
            } else {
                match self.mode.as_str() {
                    "loop" => frame.rem_euclid(self.length),
                    "mirror" => {
                        let t = frame.rem_euclid(self.length * 2.);
                        if t > self.length {
                            self.length * 2. - t
                        } else {
                            t
                        }
                    }
                    _ => frame.clamp(0., self.length),
                }
            };
            let mut pose = base;
            for (index, channel) in self.channels.iter().enumerate() {
                let Some(channel) = channel else { continue };
                let base = match index {
                    0 => base.eye.to_array().to_vec(),
                    1 => base.center.to_array().to_vec(),
                    2 => base.up.to_array().to_vec(),
                    3 => vec![base.fov],
                    _ => vec![base.zoom],
                };
                pose.set(index, &channel.sample(frame, &base)?);
            }
            pose.validate()?;
            Ok(pose)
        }
    }

    pub(super) struct Queue {
        clips: Vec<Clip>,
        random: bool,
        seed: u64,
        rng: ChaCha8Rng,
        index: usize,
        elapsed: f64,
        last: Option<f64>,
        base: Pose,
        pub transition: Option<f64>,
    }
    impl Queue {
        pub fn new(value: &Value, random: bool, seed: u64) -> Result<Self> {
            let clips = value
                .as_array()
                .or_else(|| value["paths"].as_array())
                .context("camera paths must be an array")?;
            ensure!(clips.len() <= 64, "camera clip budget exceeds 64");
            let mut keys = 0;
            let clips = clips
                .iter()
                .map(|clip| -> Result<Clip> {
                    let options = &clip["options"];
                    let fps = options["fps"].as_f64().unwrap_or(30.);
                    let length = options["length"].as_f64().unwrap_or(0.);
                    ensure!(
                        fps > 0. && fps <= 1000. && (0. ..=200_000.).contains(&length),
                        "invalid camera clip duration"
                    );
                    let mode = options["mode"].as_str().unwrap_or("single");
                    ensure!(
                        matches!(mode, "single" | "loop" | "mirror"),
                        "invalid camera clip mode"
                    );
                    let options = json!({"fps":fps,"length":length,"mode":mode});
                    let mut channels = [None, None, None, None, None];
                    for (index, name) in ["eye", "center", "up", "fov", "zoom"].iter().enumerate() {
                        if clip[*name].is_null() {
                            continue;
                        }
                        let channel =
                            Channel::new(&clip[*name], &options, if index < 3 { 3 } else { 1 })?;
                        keys += channel.key_count();
                        ensure!(keys <= 200_000, "camera key budget exceeded");
                        channels[index] = Some(channel);
                    }
                    Ok(Clip {
                        fps,
                        length,
                        mode: mode.into(),
                        channels,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Self {
                clips,
                random,
                seed,
                rng: ChaCha8Rng::seed_from_u64(seed),
                index: 0,
                elapsed: 0.,
                last: None,
                base: Pose::default(),
                transition: None,
            })
        }
        pub fn present(&self) -> bool {
            !self.clips.is_empty()
        }
        pub fn key_count(&self) -> usize {
            self.clips
                .iter()
                .flat_map(|clip| &clip.channels)
                .flatten()
                .map(Channel::key_count)
                .sum()
        }
        pub fn reset(&mut self) {
            self.last = None;
            self.transition = None;
            self.rng = ChaCha8Rng::seed_from_u64(self.seed);
        }
        fn select(&mut self, initial: bool) {
            self.index = if self.random {
                self.rng.next_u64() as usize % self.clips.len()
            } else if initial {
                0
            } else {
                (self.index + 1) % self.clips.len()
            };
            self.elapsed = 0.;
        }
        pub fn sample(&mut self, time: f64, base: Pose) -> Result<Option<Pose>> {
            self.transition = None;
            if !self.present() {
                return Ok(None);
            }
            if self.last.is_none_or(|last| time < last) {
                self.reset();
                self.base = base;
                self.select(true);
                self.last = Some(time);
                return self.clips[self.index].sample(0., self.base).map(Some);
            }
            let mut delta = time - self.last.unwrap();
            self.last = Some(time);
            let mut zeros = 0;
            let mut transitions = 0;
            while delta > 0. {
                let clip = &self.clips[self.index];
                let duration = clip.length / clip.fps;
                // Loop and Mirror stay on this path indefinitely (official camera docs).
                if clip.mode != "single" {
                    let period = duration * if clip.mode == "mirror" { 2. } else { 1. };
                    self.elapsed = if period > 0. {
                        (self.elapsed + delta).rem_euclid(period)
                    } else {
                        0.
                    };
                    break;
                }
                let remaining = (duration - self.elapsed).max(0.);
                if remaining > delta {
                    self.elapsed += delta;
                    break;
                }
                if remaining > 0. {
                    delta -= remaining;
                    zeros = 0;
                } else {
                    zeros += 1;
                    if zeros >= self.clips.len() {
                        break;
                    }
                }
                self.base = clip.sample(clip.length, self.base)?;
                transitions += 1;
                ensure!(
                    transitions <= 4096,
                    "camera queue transition budget exceeded"
                );
                self.select(false);
                self.transition = Some(time - delta);
            }
            self.clips[self.index]
                .sample(self.elapsed * self.clips[self.index].fps, self.base)
                .map(Some)
        }
        pub fn set_random(&mut self, random: bool) {
            self.random = random;
        }
    }

    struct Key {
        time: f64,
        pose: Pose,
    }
    struct RecordedClip {
        duration: f64,
        keys: Vec<Key>,
    }
    #[derive(Default)]
    pub(super) struct Recorded {
        clips: Vec<RecordedClip>,
        duration: f64,
    }
    impl Recorded {
        pub fn new(clips: &[Value], base: Pose) -> Result<Self> {
            ensure!(clips.len() <= 64, "recorded camera clip budget exceeds 64");
            let mut keys = 0;
            let clips = clips
                .iter()
                .map(|clip| -> Result<RecordedClip> {
                    let duration = clip["duration"].as_f64().context("camera clip duration")?;
                    ensure!(
                        duration > 0. && duration <= 86_400.,
                        "invalid recorded camera duration"
                    );
                    let values = clip["transforms"]
                        .as_array()
                        .context("recorded camera transforms")?;
                    keys += values.len();
                    ensure!(
                        !values.is_empty() && keys <= 20_000,
                        "recorded camera key budget exceeded"
                    );
                    let mut previous = -1.;
                    let keys = values
                        .iter()
                        .map(|value| -> Result<Key> {
                            let time = value["timestamp"].as_f64().context("camera timestamp")?;
                            ensure!(
                                time >= 0. && time > previous && time <= duration,
                                "invalid/non-increasing camera timestamp"
                            );
                            previous = time;
                            let mut pose = base;
                            for (index, name) in
                                ["eye", "center", "up", "fov", "zoom"].iter().enumerate()
                            {
                                if value[*name].is_null() {
                                    continue;
                                }
                                let defaults = match index {
                                    0 => base.eye.to_array().to_vec(),
                                    1 => base.center.to_array().to_vec(),
                                    2 => base.up.to_array().to_vec(),
                                    3 => vec![base.fov],
                                    _ => vec![base.zoom],
                                };
                                pose.set(
                                    index,
                                    &components(&value[*name], &defaults, defaults.len())?,
                                );
                            }
                            pose.validate()?;
                            Ok(Key { time, pose })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Ok(RecordedClip { duration, keys })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Self {
                duration: clips.iter().map(|clip| clip.duration).sum(),
                clips,
            })
        }
        pub fn present(&self) -> bool {
            !self.clips.is_empty()
        }
        /// The latest actual queue boundary, even when a frame skips several clips.
        pub fn transition(&self, from: f64, time: f64) -> Option<f64> {
            if !self.present() || time <= from {
                return None;
            }
            let local = time.rem_euclid(self.duration);
            let mut start = time - local;
            let mut offset = 0.;
            for clip in &self.clips {
                if local < offset + clip.duration {
                    break;
                }
                offset += clip.duration;
                start = time - local + offset;
            }
            (start > from && start > 0.).then_some(start)
        }
        pub fn sample(&self, time: f64) -> Option<Pose> {
            if !self.present() {
                return None;
            }
            let mut time = time.rem_euclid(self.duration);
            let mut clip = &self.clips[0];
            for candidate in &self.clips {
                clip = candidate;
                if time < clip.duration {
                    break;
                }
                time -= clip.duration;
            }
            let index = clip
                .keys
                .partition_point(|key| key.time <= time)
                .saturating_sub(1);
            let a = &clip.keys[index];
            let Some(b) = clip.keys.get(index + 1) else {
                return Some(a.pose);
            };
            let t = ((time - a.time) / (b.time - a.time)).clamp(0., 1.) as f32;
            Some(Pose {
                eye: a.pose.eye.lerp(b.pose.eye, t),
                center: a.pose.center.lerp(b.pose.center, t),
                up: a.pose.up.lerp(b.pose.up, t),
                fov: a.pose.fov + (b.pose.fov - a.pose.fov) * t,
                zoom: a.pose.zoom + (b.pose.zoom - a.pose.zoom) * t,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn perspective_layer_matches_orthographic_plane_fit_and_zoom_and_3d_depth() {
        for fit in [Fit::Cover, Fit::Contain, Fit::Stretch] {
            let camera = Camera::prepare(&json!({"cameraTransforms":{"zoom":1.2}})).unwrap();
            let view = camera.view([640.0, 360.0], [800, 800], fit);
            for point in [
                Vec3::ZERO,
                Vec3::new(320.0, 180.0, 0.0),
                Vec3::new(640.0, 360.0, 0.0),
            ] {
                let a = view.orthographic.project_point3(point);
                let b = view.perspective.project_point3(point);
                assert!((a - b).truncate().length() < 1e-5, "{a:?} {b:?}");
            }
        }
        let camera=Camera::prepare(&json!({"_perspective":true,"fov":90,"cameraTransforms":{"eye":"0 0 10","center":"0 0 0","up":"0 1 0"}})).unwrap();
        let view = camera.view([1.0; 2], [100, 100], Fit::Stretch);
        assert!(
            view.perspective.project_point3(Vec3::new(1.0, 0.0, 5.0)).x
                > view.perspective.project_point3(Vec3::new(1.0, 0.0, 0.0)).x
        );
        assert!(Camera::prepare(&json!({"nearz":2,"farz":1})).is_err());
        assert!(
            Camera::prepare(&json!({"_perspective":true,"cameraTransforms":{"eye":"0 0 0"}}))
                .is_err()
        );
    }
}

#[cfg(test)]
mod tests_paths {
    use super::*;
    use crate::{scene::bindings::Properties, scene::runtime::Runtime};
    use serde_json::json;

    fn channel(end: f32) -> Value {
        json!({"relative":true,"c0":[{"frame":0,"value":0},{"frame":2,"value":end}],"c1":[{"frame":0,"value":0}],"c2":[{"frame":0,"value":0}]})
    }
    #[test]
    fn queues_use_relative_clip_bases_fixed_randomness_and_rewind() {
        let path = json!({"paths":[
            {"options":{"fps":2,"length":2,"mode":"single"},"eye":channel(2.),"center":channel(2.)},
            {"options":{"fps":2,"length":2,"mode":"single"},"eye":channel(4.),"center":channel(4.)}
        ]});
        let base = Pose::default();
        let mut queue = paths::Queue::new(&path, false, 42).unwrap();
        for (time, x) in [
            (0., 0.),
            (0.5, 1.),
            (1., 2.),
            (1.5, 4.),
            (1.5, 4.),
            (2., 6.),
            (2.5, 7.),
            (0., 0.),
        ] {
            assert_eq!(queue.sample(time, base).unwrap().unwrap().eye.x, x);
        }
        let mut a = paths::Queue::new(&path, true, 42).unwrap();
        let mut b = paths::Queue::new(&path, true, 42).unwrap();
        for time in [0., 0.1, 0.5, 1., 3., 11., 0., 0.5] {
            assert_eq!(
                a.sample(time, base).unwrap().unwrap().value(),
                b.sample(time, base).unwrap().unwrap().value()
            );
        }
        let empty = json!({"paths":[]});
        assert!(!paths::Queue::new(&empty, false, 0).unwrap().present());
        let mut invalid = path;
        invalid["paths"][0]["eye"]["c0"][1]["frame"] = json!(0);
        assert!(paths::Queue::new(&invalid, false, 0).is_err());
    }
    #[test]
    fn queue_boundaries_are_exact_and_loop_mirror_do_not_switch_paths() {
        for random in [false, true] {
            let value = json!({"paths":[{"options":{"fps":2,"length":2}}, {"options":{"fps":2,"length":2}}]});
            let mut queue = paths::Queue::new(&value, random, 42).unwrap();
            for (time, transition) in [
                (0., None),
                (0.5, None),
                (1., Some(1.)),
                (1., None),
                (3.25, Some(3.)),
                (0., None),
                (0.5, None),
            ] {
                queue.sample(time, Pose::default()).unwrap();
                assert_eq!(queue.transition, transition);
            }
        }
        for (mode, values) in [("loop", [1., 0., 1.]), ("mirror", [1., 2., 1.])] {
            let value = json!({"paths":[{"options":{"fps":2,"length":2,"mode":mode},"eye":channel(2.)},{"options":{"fps":2,"length":2},"eye":channel(100.)}]});
            let mut queue = paths::Queue::new(&value, false, 0).unwrap();
            queue.sample(0., Pose::default()).unwrap();
            for (time, x) in [0.5, 1., 1.5].into_iter().zip(values) {
                assert_eq!(
                    queue.sample(time, Pose::default()).unwrap().unwrap().eye.x,
                    x
                );
                assert_eq!(queue.transition, None);
            }
        }
    }
    #[test]
    fn queue_mode_changes_apply_at_the_next_boundary_without_resetting_time() {
        let clips = (0..3).map(|x| json!({"options":{"fps":1,"length":1},"eye":{"c0":[{"frame":0,"value":x}],"c1":[{"frame":0,"value":0}],"c2":[{"frame":0,"value":1000}]}})).collect::<Vec<_>>();
        let value = json!({"paths":clips});
        let mut queue = paths::Queue::new(&value, false, 42).unwrap();
        assert_eq!(
            queue.sample(0., Pose::default()).unwrap().unwrap().eye.x,
            0.
        );
        queue.set_random(true);
        assert_eq!(
            queue.sample(0.5, Pose::default()).unwrap().unwrap().eye.x,
            0.
        );
        assert_eq!(queue.transition, None);
        let selected = queue.sample(1., Pose::default()).unwrap().unwrap().eye.x;
        assert_eq!(queue.transition, Some(1.));
        queue.set_random(false);
        assert_eq!(
            queue.sample(1.5, Pose::default()).unwrap().unwrap().eye.x,
            selected
        );
        assert_eq!(queue.transition, None);
        assert_eq!(
            queue.sample(2., Pose::default()).unwrap().unwrap().eye.x,
            (selected + 1.) % 3.
        );
        assert_eq!(queue.transition, Some(2.));
        assert_eq!(
            queue.sample(0., Pose::default()).unwrap().unwrap().eye.x,
            0.
        );
        assert_eq!(queue.transition, None);
    }
    #[test]
    fn fade_only_follows_boundaries_and_dynamic_disable_cancels_it() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("scene.json"), b"{}").unwrap();
        let assets =
            crate::assets::Assets::open(root.path(), &root.path().join("scene.json"), None)
                .unwrap();
        std::fs::write(
            root.path().join("path.json"),
            br#"{"paths":[{"options":{"fps":1,"length":2}},{"options":{"fps":1,"length":0.5}}]}"#,
        )
        .unwrap();
        let mut objects = vec![
            json!({"camera":"default","path":"path.json"}),
            json!({"__scene":true,"camerafade":true,"cameraTransforms":{}}),
        ];
        let states = objects
            .iter()
            .map(|o| crate::scene::local_state(o, [32.; 2], false).unwrap())
            .collect::<Vec<_>>();
        let mut camera = Controller::new(&assets, &objects).unwrap();
        for (time, cover) in [
            (0., 0.),
            (1., 0.),
            (2., 1.),
            (2.25, 0.84375),
            (2.25, 0.84375),
            (2.5, 1.),
            (3., 0.5),
            (3.5, 0.),
        ] {
            camera.update(&mut objects, &states, time).unwrap();
            assert_eq!(camera.fade_cover(), cover);
        }
        camera.update(&mut objects, &states, 4.5).unwrap();
        assert_eq!(camera.fade_cover(), 1.);
        objects[1]["camerafade"] = false.into();
        camera.update(&mut objects, &states, 4.75).unwrap();
        assert_eq!(camera.fade_cover(), 0.);
        objects[1]["camerafade"] = true.into();
        camera.update(&mut objects, &states, 4.875).unwrap();
        assert_eq!(camera.fade_cover(), 0.);
        camera.update(&mut objects, &states, 0.).unwrap();
        assert_eq!(camera.fade_cover(), 0.);
        camera.update(&mut objects, &states, 0.5).unwrap();
        assert_eq!(camera.fade_cover(), 0.);
        camera.update(&mut objects, &states, 2.).unwrap();
        assert_eq!(camera.fade_cover(), 1.);
        objects[0]["__destroyed"] = true.into();
        camera.release_destroyed(&objects);
        camera.update(&mut objects, &states, 2.25).unwrap();
        assert_eq!(
            camera.fade_cover(),
            0.,
            "removing the last camera clears its curtain"
        );
    }
    #[test]
    fn recorded_paths_hold_edges_interpolate_roll_and_loop_on_absolute_time() {
        let values = [
            json!({"duration":2,"transforms":[
            {"timestamp":0.5,"eye":"0 0 10","center":"0 0 0","up":"0 1 0","fov":50},
            {"timestamp":1.5,"eye":"2 0 10","center":"2 0 0","up":"1 0 0","fov":90}]}),
            json!({"duration":1,"transforms":[{"timestamp":0,"eye":"9 0 10","center":"9 0 0","up":"0 1 0"}]}),
        ];
        let recorded = paths::Recorded::new(&values, Pose::default()).unwrap();
        assert_eq!(recorded.sample(0.).unwrap().eye.x, 0.);
        assert_eq!(recorded.sample(0.5).unwrap().eye.x, 0.);
        let middle = recorded.sample(1.).unwrap();
        assert_eq!(middle.eye.x, 1.);
        assert_eq!(middle.up, Vec3::new(0.5, 0.5, 0.));
        assert_eq!(middle.fov, 70.);
        assert_eq!(recorded.sample(1.9).unwrap().eye.x, 2.);
        assert_eq!(recorded.sample(2.5).unwrap().eye.x, 9.);
        assert_eq!(recorded.sample(3.).unwrap().eye.x, 0.);
        for (from, time, boundary) in [
            (0., 1.9, None),
            (1.9, 2., Some(2.)),
            (2., 2., None),
            (2., 3., Some(3.)),
            (0., 8.25, Some(8.)),
            (3., 0., None),
        ] {
            assert_eq!(recorded.transition(from, time), boundary);
        }
        let mut invalid = values;
        invalid[0]["transforms"][1]["timestamp"] = json!(0.5);
        assert!(paths::Recorded::new(&invalid, Pose::default()).is_err());
    }
    #[test]
    fn adding_and_rejecting_cameras_preserves_the_running_path() {
        let root = tempfile::tempdir().unwrap();
        let mut pkg = 8u32.to_le_bytes().to_vec();
        pkg.extend(b"PKGV0001");
        pkg.extend(0u32.to_le_bytes());
        std::fs::write(root.path().join("scene.pkg"), pkg).unwrap();
        let assets =
            crate::assets::Assets::open(root.path(), &root.path().join("scene.pkg"), None).unwrap();
        std::fs::write(root.path().join("path.json"),serde_json::to_vec(&json!({"paths":[{"options":{"fps":2,"length":2},"eye":channel(2.),"center":channel(2.)}]})).unwrap()).unwrap();
        let mut objects = vec![
            json!({"camera":"default","path":"path.json","origin":[0,0,10]}),
            json!({"__scene":true,"cameraTransforms":{}}),
        ];
        let mut states = vec![
            crate::scene::local_state(&objects[0], [32.; 2], false).unwrap(),
            crate::scene::local_state(&objects[1], [32.; 2], false).unwrap(),
        ];
        let mut camera = Controller::new(&assets, &objects).unwrap();
        for (time, x) in [(0., 0.), (0.5, 1.)] {
            camera.update(&mut objects, &states, time).unwrap();
            assert_eq!(objects[1]["__cameraPose"]["eye"][0], json!(x));
        }
        objects.push(json!({"camera":"default","path":"missing.json","visible":false}));
        let mut state = states[0].clone();
        state.visible = false;
        states.push(state);
        assert!(camera.prepare_created(&assets, &objects, &[2]).is_err());
        camera.update(&mut objects, &states, 0.75).unwrap();
        assert_eq!(objects[1]["__cameraPose"]["eye"][0], json!(1.5));
        objects[2]["path"] = json!("path.json");
        let addition = camera.prepare_created(&assets, &objects, &[2]).unwrap();
        camera.commit_created(addition);
        camera.update(&mut objects, &states, 0.875).unwrap();
        assert_eq!(objects[1]["__cameraPose"]["eye"][0], json!(1.75));
        objects[2]["__destroyed"] = true.into();
        camera.release_destroyed(&objects);
        camera.update(&mut objects, &states, 1.).unwrap();
        assert_eq!(objects[1]["__cameraPose"]["eye"][0], json!(2.));
    }
    #[test]
    fn active_camera_uses_parent_world_visibility_properties_and_script_override() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("scene.pkg");
        let mut bytes = 8u32.to_le_bytes().to_vec();
        bytes.extend(b"PKGV0001");
        bytes.extend(0u32.to_le_bytes());
        std::fs::write(&package, bytes).unwrap();
        let assets = crate::assets::Assets::open(root.path(), &package, None).unwrap();
        let objects = [
            json!({"id":1,"origin":"4 0 0","angles":"0 0 1.57079632679"}),
            json!({"id":2,"camera":"default","parent":1,"origin":"1 0 10","zoom":{"value":2,"user":"zoom"}}),
            json!({"id":3,"camera":"default","origin":"0 0 10","zoom":3,"visible":{"value":false,"user":"second"}}),
            json!({"id":"settings","__scene":true,"origin":[0,0,0],"clearcolor":{"value":"0 0 0","script":"export function update(v){const camera=thisScene.getCameraTransforms();if(engine.runtime>2){camera.zoom=4;thisScene.setCameraTransforms(camera);}return v;}"},"cameraTransforms":{"eye":"0 0 1000","center":"0 0 0","up":"0 1 0","zoom":1}}),
        ];
        let mut runtime =
            Runtime::new(&assets, &objects, &Properties::new(), [32.; 2], None, true).unwrap();
        let pose = &runtime.objects[3]["__cameraPose"];
        assert!((pose["eye"][0].as_f64().unwrap() - 4.).abs() < 1e-5);
        assert!((pose["eye"][1].as_f64().unwrap() - 1.).abs() < 1e-5);
        assert!((pose["up"][0].as_f64().unwrap() + 1.).abs() < 1e-5);
        assert_eq!(pose["zoom"], json!(2.));
        runtime
            .properties(&Properties::from([("second".into(), json!(true))]))
            .unwrap();
        assert_eq!(runtime.objects[3]["__cameraPose"]["zoom"], json!(3.));
        runtime
            .properties(&Properties::from([("zoom".into(), json!(1.5))]))
            .unwrap();
        assert_eq!(runtime.objects[3]["__cameraPose"]["zoom"], json!(1.5));
        runtime.frame(&json!({"time":3,"delta":1,"timeOfDay":0.5,"screen":[32,32],"screenPointer":[0,0],"pointer":[0,0],"down":false,"focused":false,"hits":[],"cameraPose":runtime.objects[3]["__cameraPose"]}));
        assert_eq!(runtime.objects[3]["__cameraOverride"], true);
        assert_eq!(Camera::prepare(&runtime.objects[3]).unwrap().zoom, 4.);
        assert!(runtime.errors().is_empty(), "{:?}", runtime.errors());
        let old = runtime.objects.clone();
        assert!(runtime.apply(&json!([[1, ["zoom"], 0]])).is_err());
        assert_eq!(runtime.objects, old);
    }
}
