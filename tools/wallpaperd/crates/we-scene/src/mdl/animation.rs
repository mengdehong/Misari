//! Per-instance animation clocks, parent-local poses, and CPU skinning.
use super::*;
use anyhow::{Context, Result, ensure};
use glam::{DMat4, Mat3, Quat};
use serde_json::{Value, json};
use std::{collections::HashMap, rc::Rc};

struct Clock {
    clip: u32,
    frame: f64,
    time: f64,
    revision: u64,
}
pub(crate) fn validate_controls(object: &Value) -> Result<()> {
    super::physics::validate_commands(object)?;
    if let Some(layers) = object["animationlayers"].as_array() {
        ensure!(layers.len() <= 128, "model exceeds 128 animation layers");
        for layer in layers {
            let rate = crate::scene::bindings::components(&layer["rate"], &[1.0], 1)?[0];
            let blend = crate::scene::bindings::components(&layer["blend"], &[1.0], 1)?[0];
            ensure!(
                rate.abs() <= 128.0 && (0.0..=1.0).contains(&blend),
                "invalid skeletal animation rate/blend"
            );
            for key in ["__frame", "__time", "__order"] {
                if let Some(v) = layer.get(key) {
                    ensure!(
                        v.as_f64().is_some_and(|n| n.is_finite() && n.abs() <= 1e12),
                        "invalid skeletal animation {key}"
                    );
                }
            }
            let blend_time = layer["blendtime"].as_f64().unwrap_or(0.0);
            ensure!(
                (0.0..=3600.0).contains(&blend_time),
                "invalid animation blend time"
            );
            crate::scene::visible(&layer["visible"])?;
        }
    } else {
        ensure!(
            object["animationlayers"].is_null(),
            "animation layers must be an array"
        );
    }
    if let Some(overrides) = object["__boneOverrides"].as_object() {
        ensure!(overrides.len() <= 1024, "bone override budget exceeded");
        for value in overrides.values() {
            ensure!(
                value.as_array().is_some_and(|a| a.len() == 16
                    && a.iter()
                        .all(|v| v.as_f64().is_some_and(|x| x.is_finite() && x.abs() <= 1e12))),
                "invalid bone override matrix"
            );
        }
    }
    Ok(())
}
pub(crate) struct Rig {
    pub model: Rc<Model>,
    order: Vec<usize>,
    inverse: Vec<DMat4>,
    pub local: Vec<Mat4>,
    pub world: Vec<Mat4>,
    skin: Vec<Mat4>,
    normals: Vec<Mat3>,
    clocks: HashMap<String, Clock>,
    pub events: Vec<Value>,
    pub layers: Vec<Value>,
    moving: bool,
    last: Option<(f32, Value)>,
    last_space: Option<Mat4>,
    colliders: super::collision::Colliders,
    emission_positions: Option<Rc<[Vec3]>>,
    physics: super::physics::Physics,
}
impl Model {
    pub fn metadata(&self) -> Value {
        json!({
            "bones":self.bones.iter().map(|b|json!({"name":b.name,"parent":b.parent,"local":b.local.to_cols_array(),"simulation":b.simulation})).collect::<Vec<_>>(),
            "clips":self.clips.iter().map(|c|json!({"id":c.id,"name":c.name,"mode":c.mode,"fps":c.fps,"frames":c.frames,"duration":c.frames as f32/c.fps})).collect::<Vec<_>>(),
            "attachments":self.attachments.iter().map(|a|json!({"name":a.name,"bone":a.bone,"matrix":a.matrix.to_cols_array()})).collect::<Vec<_>>(),
            "rest":self.rest.as_ref().map(|r|r.iter().map(|m|m.to_cols_array()).collect::<Vec<_>>())
            ,"parts":self.meshes.iter().map(|m|m.parts.iter().map(|p|json!({"id":p.id,"start":p.start,"count":p.count,"offset":p.offset})).collect::<Vec<_>>()).collect::<Vec<_>>()
        })
    }
}
impl Rig {
    pub fn new(model: Rc<Model>) -> Result<Self> {
        let physics = super::physics::Physics::new(&model)?;
        let mut order = Vec::with_capacity(model.bones.len());
        let mut seen = vec![false; model.bones.len()];
        for index in 0..model.bones.len() {
            let mut chain = Vec::new();
            let mut next = Some(index);
            while let Some(i) = next {
                if seen[i] {
                    break;
                }
                chain.push(i);
                seen[i] = true;
                next = model.bones[i].parent;
            }
            order.extend(chain.into_iter().rev());
        }
        let local = model.bones.iter().map(|b| b.local).collect::<Vec<_>>();
        let mut world = vec![DMat4::IDENTITY; local.len()];
        for &i in &order {
            world[i] =
                model.bones[i].parent.map_or(DMat4::IDENTITY, |p| world[p]) * local[i].as_dmat4();
        }
        Ok(Self {
            colliders: super::collision::Colliders::new(
                &model,
                &world.iter().map(|m| m.as_mat4()).collect::<Vec<_>>(),
            ),
            inverse: world.iter().map(|m| m.inverse()).collect(),
            world: world.iter().map(|m| m.as_mat4()).collect(),
            skin: vec![Mat4::IDENTITY; local.len()],
            normals: vec![Mat3::IDENTITY; local.len()],
            moving: !model.clips.is_empty(),
            model,
            order,
            local,
            clocks: HashMap::new(),
            events: Vec::new(),
            layers: Vec::new(),
            last: None,
            last_space: None,
            emission_positions: None,
            physics,
        })
    }
    pub fn animated(&self) -> bool {
        self.moving || self.physics.active
    }
    pub fn collision_capsules(&mut self, transform: Mat4) -> Rc<[super::Capsule]> {
        self.colliders.snapshot(transform, &self.world)
    }
    pub fn attachment_matrices(&self) -> Vec<Mat4> {
        self.model
            .attachments
            .iter()
            .map(|a| self.world[a.bone] * a.matrix)
            .collect()
    }
    #[cfg(test)]
    pub fn advance(&mut self, time: f32, object: &Value) -> Result<bool> {
        self.advance_in_space(time, object, Mat4::IDENTITY)
    }
    pub fn advance_in_space(&mut self, time: f32, object: &Value, space: Mat4) -> Result<bool> {
        self.events.clear();
        let same_inputs = self.last.as_ref().is_some_and(|(_, inputs)| {
            inputs.is_array()
                && inputs[0] == object["animationlayers"]
                && inputs[1] == object["__boneOverrides"]
                && inputs[2] == object["__bonePhysics"]
                && (!self.physics.active || self.last_space == Some(space))
        });
        if self
            .last
            .as_ref()
            .is_some_and(|(t, _)| (*t == time || time >= *t && !self.animated()) && same_inputs)
        {
            // A later resume starts at the held frame, including when no GPU upload is needed.
            for clock in self.clocks.values_mut() {
                clock.time = time as f64;
            }
            self.last.as_mut().unwrap().0 = time;
            return Ok(false);
        }
        let rewind = self.last.as_ref().is_some_and(|(t, _)| time < *t);
        if rewind {
            self.clocks.clear();
        }
        let dt = self.last.as_ref().map_or(0., |(t, _)| time - *t);
        if rewind || dt > 0.25 {
            self.physics.reset();
        }
        let defaults;
        let layers = if let Some(layers) = object["animationlayers"].as_array() {
            layers
        } else {
            defaults = self
                .model
                .clips
                .first()
                .map(|c| vec![json!({"animation":c.id})])
                .unwrap_or_default();
            &defaults
        };
        ensure!(layers.len() <= 128, "model exceeds 128 animation layers");
        let mut layers = layers.iter().enumerate().collect::<Vec<_>>();
        layers.sort_by(|(a, x), (b, y)| {
            x["__order"]
                .as_f64()
                .unwrap_or(*a as f64)
                .total_cmp(&y["__order"].as_f64().unwrap_or(*b as f64))
        });
        self.layers.clear();
        self.moving = false;
        let mut sampled = Vec::new();
        let mut active = std::collections::HashSet::new();
        for (index, layer) in layers {
            if layer["__destroyed"].as_bool() == Some(true) {
                continue;
            }
            let clip = self.model.clips.iter().find(|c| {
                layer["animation"].as_u64() == Some(c.id as u64)
                    || layer["animation"].as_str() == Some(c.name.as_str())
            });
            let Some(clip) = clip else {
                continue;
            };
            let key = layer
                .get("__key")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| layer.get("id").map(Value::to_string))
                .unwrap_or_else(|| index.to_string());
            active.insert(key.clone());
            let rate = crate::scene::bindings::components(&layer["rate"], &[1.0], 1)?[0];
            let blend = crate::scene::bindings::components(&layer["blend"], &[1.0], 1)?[0];
            ensure!(
                rate.abs() <= 128.0 && (0.0..=1.0).contains(&blend),
                "invalid skeletal animation rate/blend"
            );
            let revision = layer["__revision"].as_u64().unwrap_or(0);
            let stopped = layer["playing"].as_bool() == Some(false);
            let paused = layer["paused"]
                .as_bool()
                .unwrap_or_else(|| layer["startpaused"].as_bool().unwrap_or(false));
            let frame = layer["__frame"].as_f64().unwrap_or({
                if paused || stopped {
                    0.0
                } else {
                    time as f64 * clip.fps as f64 * rate as f64
                }
            });
            ensure!(
                frame.is_finite() && frame.abs() <= 1e12,
                "invalid skeletal animation frame"
            );
            let anchor = layer["__time"].as_f64().unwrap_or(time as f64);
            ensure!(
                anchor.is_finite() && anchor.abs() <= 1e12,
                "invalid skeletal clock anchor"
            );
            let commanded = frame
                + if paused || stopped {
                    0.0
                } else {
                    (time as f64 - anchor).max(0.0) * clip.fps as f64 * rate as f64
                };
            let mode = if layer["__single"].as_bool() == Some(true) {
                "single"
            } else {
                &clip.mode
            };
            let clock = self.clocks.entry(key.clone()).or_insert(Clock {
                clip: clip.id,
                frame: commanded,
                time: time as f64,
                revision,
            });
            let mut previous = clock.frame;
            if clock.revision != revision || clock.clip != clip.id {
                previous = frame;
                clock.frame = commanded;
                clock.revision = revision;
                clock.clip = clip.id;
            } else if !paused && !stopped {
                clock.frame += (time as f64 - clock.time).max(0.0) * clip.fps as f64 * rate as f64;
            }
            clock.time = time as f64;
            let running = !paused
                && !stopped
                && rate != 0.0
                && (mode != "single"
                    || (rate > 0.0 && clock.frame < clip.frames as f64)
                    || (rate < 0.0 && clock.frame > 0.0));
            self.moving |= running;
            self.layers
                .push(json!({"key":key,"revision":revision,"frame":clock.frame,"playing":running}));
            if clock.frame != previous && !rewind && !stopped {
                for (frame, name) in &clip.events {
                    for _ in 0..crossings(
                        previous,
                        clock.frame,
                        *frame as f64,
                        clip.frames as f64,
                        mode,
                    )
                    .min(128 - self.events.len())
                    {
                        self.events.push(json!({"key":key,"revision":revision,"name":name,"frame":frame,"animation":layer["name"].as_str().unwrap_or(&clip.name),"ended":false}));
                    }
                }
                let end = if rate < 0.0 { 0.0 } else { clip.frames as f64 };
                for _ in 0..crossings(previous, clock.frame, end, clip.frames as f64, mode)
                    .min(128 - self.events.len())
                {
                    self.events.push(json!({"key":key,"revision":revision,"frame":end,"animation":layer["name"].as_str().unwrap_or(&clip.name),"ended":true}));
                }
            }
            if stopped || !crate::scene::visible(&layer["visible"])? {
                continue;
            }
            let blend_time = layer["blendtime"].as_f64().unwrap_or(0.0);
            ensure!(
                blend_time.is_finite() && (0.0..=3600.0).contains(&blend_time),
                "invalid animation blend time"
            );
            let mut blend = blend;
            if blend_time > 0.0 {
                if layer["blendin"].as_bool() == Some(true) {
                    blend *= (clock.frame / clip.fps as f64 / blend_time).clamp(0.0, 1.0) as f32;
                }
                if layer["blendout"].as_bool() == Some(true) && mode == "single" {
                    blend *= ((clip.frames as f64 - clock.frame) / clip.fps as f64 / blend_time)
                        .clamp(0.0, 1.0) as f32;
                }
            }
            sampled.push((
                clip,
                clock.frame,
                blend,
                layer["additive"].as_bool().unwrap_or(false),
                mode,
            ));
        }
        self.clocks.retain(|k, _| active.contains(k));
        let mut local = Vec::with_capacity(self.model.bones.len());
        let mut world = vec![DMat4::IDENTITY; self.model.bones.len()];
        for (index, bone) in self.model.bones.iter().enumerate() {
            let mut pose = Key::from_matrix(bone.local);
            let mut delta = Key {
                translation: Vec3::ZERO,
                angles: Vec3::ZERO,
                scale: Vec3::ZERO,
            };
            let mut weight = 0.0_f32;
            let mut base_set = false;
            let mut replaced = false;
            for (clip, frame, blend, additive, mode) in &sampled {
                let Some(track) = clip.tracks.get(index).filter(|t| !t.is_empty()) else {
                    continue;
                };
                let sample = sample(track, *frame, mode);
                if *additive {
                    let reference = if clip.mode == "single" {
                        track.last().unwrap()
                    } else {
                        &track[0]
                    };
                    if !replaced && !base_set {
                        pose = reference.clone();
                        base_set = true;
                    }
                    delta.translation += (sample.translation - reference.translation) * *blend;
                    delta.angles += angle_delta(sample.angles - reference.angles) * *blend;
                    delta.scale += (sample.scale - reference.scale) * *blend;
                    weight += *blend;
                } else {
                    pose = pose.lerp(&sample, *blend);
                    replaced = true;
                }
            }
            let factor = 1.0 / weight.max(1.0);
            pose.translation += delta.translation * factor;
            pose.angles += delta.angles * factor;
            pose.scale += delta.scale * factor;
            let override_value = &object["__boneOverrides"][index.to_string()];
            let matrix = if let Some(values) = override_value.as_array() {
                ensure!(values.len() == 16, "invalid bone override matrix");
                let mut v = [0.0; 16];
                for (x, y) in v.iter_mut().zip(values) {
                    *x = y.as_f64().context("bone matrix value")? as f32;
                }
                Mat4::from_cols_array(&v)
            } else {
                pose.matrix()
            };
            ensure!(matrix.is_finite(), "non-finite bone pose");
            local.push(matrix);
        }
        self.physics.apply(
            if dt > 0.25 { 0. } else { dt.max(0.) },
            space,
            &mut local,
            &self.order,
            &self.model,
            object,
        )?;
        for &i in &self.order {
            world[i] = self.model.bones[i]
                .parent
                .map_or(DMat4::IDENTITY, |p| world[p])
                * local[i].as_dmat4();
        }
        let skin = world
            .iter()
            .zip(&self.inverse)
            .map(|(w, b)| (*w * *b).as_mat4())
            .collect::<Vec<_>>();
        ensure!(
            skin.iter().all(|m| m.is_finite()),
            "skeletal transform overflows"
        );
        self.local = local;
        self.world = world.iter().map(|m| m.as_mat4()).collect();
        self.skin = skin;
        self.normals.clear();
        self.normals.extend(self.skin.iter().map(|m| {
            let m = Mat3::from_mat4(*m);
            if m.determinant().abs() > 1e-12 {
                m.inverse().transpose()
            } else {
                m
            }
        }));
        self.colliders.invalidate();
        self.emission_positions = None;
        if same_inputs {
            self.last.as_mut().unwrap().0 = time;
        } else {
            self.last = Some((
                time,
                json!([
                    object["animationlayers"],
                    object["__boneOverrides"],
                    object["__bonePhysics"],
                ]),
            ));
        }
        self.last_space = Some(space);
        Ok(true)
    }
    /// GPU material/target rebuilds retain per-instance simulation and consumed
    /// command revisions. Force one pose/upload update at the retained clock.
    pub fn inherit_physics(&mut self, previous: &Self) {
        if self.physics.active
            && self.model.bones.len() == previous.model.bones.len()
            && self
                .model
                .bones
                .iter()
                .zip(&previous.model.bones)
                .all(|(a, b)| {
                    a.name == b.name
                        && a.parent == b.parent
                        && a.local == b.local
                        && a.simulation == b.simulation
                })
        {
            self.physics = previous.physics.clone();
            self.last = previous.last.as_ref().map(|(time, _)| (*time, Value::Null));
        }
    }
    pub fn emission_positions(&mut self) -> Rc<[Vec3]> {
        if let Some(positions) = &self.emission_positions {
            return positions.clone();
        }
        let positions: Rc<[Vec3]> = self
            .model
            .meshes
            .iter()
            .flat_map(|g| &g.vertices)
            .map(|v| {
                let mut point = Vec3::ZERO;
                let mut total = 0.;
                for i in 0..4 {
                    if v.weights[i] > 0.
                        && let Some(skin) = self.skin.get(v.bones[i])
                    {
                        point += skin.transform_point3(v.position) * v.weights[i];
                        total += v.weights[i];
                    }
                }
                if total > 0. {
                    point / total
                } else {
                    v.position
                }
            })
            .collect::<Vec<_>>()
            .into();
        self.emission_positions = Some(positions.clone());
        positions
    }
    pub fn release_emission_positions(&mut self) {
        self.emission_positions = None;
    }
    pub fn vertices(&self, mesh: usize) -> Vec<f32> {
        let mut out = Vec::new();
        self.append_vertices(mesh, &mut out);
        out
    }
    pub fn append_vertices(&self, mesh: usize, out: &mut Vec<f32>) {
        let geometry = &self.model.meshes[mesh];
        out.reserve(geometry.vertices.len() * 14);
        for v in &geometry.vertices {
            let (mut position, mut n, mut t, mut total) = (Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, 0.0);
            for slot in 0..4 {
                let weight = v.weights[slot];
                if weight <= 0.0 {
                    continue;
                }
                if let Some(skin) = self.skin.get(v.bones[slot]) {
                    position += skin.transform_point3(v.position) * weight;
                    n += self.normals[v.bones[slot]] * v.normal * weight;
                    t += skin.transform_vector3(v.tangent.truncate()) * weight;
                    total += weight;
                }
            }
            if total > 0.0 {
                position /= total;
                n = n.normalize_or_zero();
                t = t.normalize_or_zero();
            } else {
                position = v.position;
                n = v.normal;
                t = v.tangent.truncate();
            }
            out.extend(position.to_array());
            out.extend(n.to_array());
            out.extend(t.extend(v.tangent.w).to_array());
            out.extend(v.uv.to_array());
            out.extend(v.uv2.to_array());
        }
    }
}
impl Key {
    fn from_matrix(m: Mat4) -> Self {
        let (scale, rotation, translation) = m.to_scale_rotation_translation();
        let (x, y, z) = rotation.to_euler(crate::scene::EULER_ORDER);
        Self {
            translation,
            angles: Vec3::new(x, y, z),
            scale,
        }
    }
    fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(
            self.scale,
            Quat::from_euler(
                crate::scene::EULER_ORDER,
                self.angles.x,
                self.angles.y,
                self.angles.z,
            ),
            self.translation,
        )
    }
    fn lerp(&self, b: &Self, t: f32) -> Self {
        Self {
            translation: self.translation.lerp(b.translation, t),
            angles: self.angles + angle_delta(b.angles - self.angles) * t,
            scale: self.scale.lerp(b.scale, t),
        }
    }
}
fn angle_delta(v: Vec3) -> Vec3 {
    v.map(|a| (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI)
}
fn sample(track: &[Key], frame: f64, mode: &str) -> Key {
    let span = (track.len() - 1) as f64;
    if span == 0.0 {
        return track[0].clone();
    }
    let frame = match mode {
        "loop" => frame.rem_euclid(span),
        "mirror" | "pingpong" => {
            let f = frame.rem_euclid(2.0 * span);
            if f > span { 2.0 * span - f } else { f }
        }
        _ => frame.clamp(0.0, span),
    };
    let first = frame.floor() as usize;
    let last = (first + 1).min(track.len() - 1);
    track[first].lerp(&track[last], (frame - first as f64) as f32)
}
fn crossings(from: f64, to: f64, event: f64, span: f64, mode: &str) -> usize {
    let count = |event: f64, period: f64| {
        if to >= from {
            ((to - event).div_euclid(period) - (from - event).div_euclid(period)).clamp(0.0, 128.0)
                as usize
        } else {
            (((from - event) / period).ceil() - ((to - event) / period).ceil()).clamp(0.0, 128.0)
                as usize
        }
    };
    match mode {
        "loop" if span > 0.0 => count(event, span),
        "mirror" | "pingpong" if span > 0.0 => {
            let first = count(event, 2.0 * span);
            first
                + if event > 0.0 && event < span {
                    count(2.0 * span - event, 2.0 * span)
                } else {
                    0
                }
        }
        _ => usize::from(if to >= from {
            from < event && to >= event
        } else {
            to <= event && from > event
        }),
    }
}
