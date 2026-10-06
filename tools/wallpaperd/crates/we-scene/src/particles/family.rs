//! Bounded child instances, scheduled by their own fixed-step birth clocks.

use super::simulation::{Event, STEP, System, Values};
use crate::{
    assets::{Assets, normalize},
    audio::AudioSnapshot,
    scene::bindings::{Properties, components, resolve},
    scene::local_state,
};
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3, Vec4};
use serde_json::{Value, json};
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashSet},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Root,
    Static,
    Follow,
    Spawn,
    Death,
}
pub(super) struct Instance {
    pub id: u64,
    pub system: System,
    pub matrix: Mat4,
    origin: Vec3,
    binding: Option<(u64, Option<u64>)>,
    owner: Option<u64>,
    event_particle: Option<u64>,
    clock_start: i128,
}
pub(super) struct Node {
    prototype: System,
    pub instances: Vec<Instance>,
    parent: Option<usize>,
    kind: Kind,
    max_instances: usize,
    probability: f32,
    matrix: Mat4,
    pub color: Vec4,
    overrides: Value,
    inherit_overrides: bool,
    control_point_start: Option<usize>,
}
pub(super) struct Family {
    pub nodes: Vec<Node>,
    seed: u64,
    serial: u64,
    tick: u64,
    input_time: f64,
    started: bool,
    complete: bool,
    emitting: bool,
    pending_step: bool,
    schedule: BinaryHeap<Reverse<(i128, usize, u64)>>,
}
fn hash(mut n: u64) -> u64 {
    n = (n ^ (n >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    n = (n ^ (n >> 27)).wrapping_mul(0x94d049bb133111eb);
    n ^ (n >> 31)
}
fn inherited_overrides(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(values) = value.as_object_mut() {
        // Layer instance multipliers apply to each child; its own flags can
        // disable individual overrides. Control points follow their bindings.
        values.retain(|key, _| {
            matches!(
                key.as_str(),
                "alpha" | "color" | "colorn" | "count" | "size" | "lifetime" | "speed" | "rate"
            )
        });
    }
    value
}
impl Family {
    pub fn set_images(&mut self, images: super::emission::Sources) {
        for node in &mut self.nodes {
            node.prototype.images = images.clone();
            for instance in &mut node.instances {
                instance.system.images = images.clone();
            }
        }
    }
    pub fn image_periodic(&self, index: usize) -> bool {
        self.nodes
            .iter()
            .any(|node| node.prototype.image_periodic(index))
    }
    pub fn image_demand(&self) -> bool {
        self.nodes.iter().any(|node| {
            node.instances
                .iter()
                .any(|instance| instance.system.image_demand())
                || self.emitting && node.parent.is_some() && node.prototype.image_demand()
        })
    }
    pub fn set_models(&mut self, models: super::simulation::Models) {
        for node in &mut self.nodes {
            node.prototype.models = models.clone();
            for instance in &mut node.instances {
                instance.system.models = models.clone();
            }
        }
    }
    pub fn set_bounds(&mut self, size: [f32; 2]) {
        let bounds = Some(glam::Vec2::from_array(size));
        if self.nodes[0].prototype.bounds == bounds {
            return;
        }
        for node in &mut self.nodes {
            node.prototype.bounds = bounds;
            for instance in &mut node.instances {
                instance.system.bounds = bounds;
            }
        }
    }
    pub fn load(
        assets: &Assets,
        file: &str,
        object: &Value,
        properties: &Properties,
        seed: u64,
    ) -> Result<(Self, Vec<Value>)> {
        let mut nodes = Vec::new();
        let mut definitions = Vec::new();
        let mut stack = HashSet::new();
        Self::load_node(
            assets,
            file,
            object,
            properties,
            None,
            0,
            seed,
            &mut nodes,
            &mut definitions,
            &mut stack,
        )?;
        ensure!(
            nodes
                .iter()
                .map(|n| n.prototype.max * n.max_instances)
                .sum::<usize>()
                <= 100_000,
            "particle family capacity exceeds 100000"
        );
        ensure!(
            nodes.iter().map(|n| n.max_instances).sum::<usize>() <= 4096,
            "particle family instance capacity exceeds 4096"
        );
        for node in &mut nodes {
            node.prototype.event_mask = 0;
        }
        for index in (0..nodes.len()).rev() {
            if let Some(parent) = nodes[index].parent {
                nodes[parent].prototype.event_mask |= match nodes[index].kind {
                    Kind::Spawn | Kind::Follow => 1,
                    Kind::Death => 2,
                    _ => 0,
                };
            }
            if nodes[index].prototype.motion
                && let Some(parent) = nodes[index].parent
            {
                nodes[parent].prototype.motion = true;
            }
        }
        for node in &mut nodes {
            for instance in &mut node.instances {
                instance.system.motion = node.prototype.motion;
                instance.system.event_mask = node.prototype.event_mask;
            }
        }
        Ok((
            Self {
                nodes,
                seed,
                serial: 1,
                tick: 0,
                input_time: 0.,
                started: false,
                complete: false,
                emitting: true,
                pending_step: false,
                schedule: BinaryHeap::from([Reverse((0, 0, 0))]),
            },
            definitions,
        ))
    }
    #[allow(clippy::too_many_arguments)]
    fn load_node(
        assets: &Assets,
        file: &str,
        spec: &Value,
        properties: &Properties,
        parent: Option<usize>,
        depth: usize,
        seed: u64,
        nodes: &mut Vec<Node>,
        definitions: &mut Vec<Value>,
        stack: &mut HashSet<String>,
    ) -> Result<()> {
        ensure!(
            depth <= 8 && nodes.len() < 128,
            "particle children exceed depth/system budget"
        );
        let file = normalize(file)?;
        ensure!(stack.insert(file.clone()), "particle child cycle: {file}");
        let definition = resolve(&assets.json(&file)?, properties)?;
        let spec = resolve(spec, properties)?;
        let kind = if parent.is_none() {
            Kind::Root
        } else {
            match spec["type"].as_str().unwrap_or("static") {
                "static" => Kind::Static,
                "eventfollow" => Kind::Follow,
                "eventspawn" => Kind::Spawn,
                "eventdeath" => Kind::Death,
                other => anyhow::bail!("unknown particle child type {other}"),
            }
        };
        let inherit = spec["instanceoverride"].is_null() && parent.is_some();
        let overrides = if inherit {
            inherited_overrides(&nodes[parent.unwrap()].overrides)
        } else {
            spec["instanceoverride"].clone()
        };
        let control_point_start = if parent.is_some() && super::animation::flags(&spec)? & 1 != 0 {
            Some(super::simulation::control_point_index(
                &spec,
                "controlpointstartindex",
            )?)
        } else {
            None
        };
        let mut prototype = System::new(&definition, &overrides, hash(seed ^ nodes.len() as u64))?;
        prototype.drive_points(control_point_start);
        let max_instances = if kind == Kind::Root {
            1
        } else if kind == Kind::Static {
            nodes[parent.unwrap()].max_instances
        } else {
            components(&spec["maxcount"], &[20.0], 1)?[0].clamp(0.0, 256.0) as usize
        };
        let probability = components(&spec["probability"], &[1.0], 1)?[0];
        ensure!(
            (0.0..=1.0).contains(&probability),
            "invalid particle child probability"
        );
        let state = if kind == Kind::Root {
            local_state(&json!({}), [0.0; 2], true)?
        } else {
            local_state(&spec, [0.0; 2], true)?
        };
        let matrix = state.transform;
        ensure!(
            matrix.determinant().abs() > 1e-12,
            "singular particle child transform"
        );
        let instances = if kind == Kind::Root {
            vec![Instance {
                id: 0,
                system: prototype.fresh(hash(seed)),
                matrix: Mat4::IDENTITY,
                origin: Vec3::ZERO,
                binding: None,
                owner: None,
                event_particle: None,
                clock_start: 0,
            }]
        } else {
            vec![]
        };
        let index = nodes.len();
        let color = state.color * parent.map_or(Vec4::ONE, |i| nodes[i].color);
        nodes.push(Node {
            prototype,
            instances,
            parent,
            kind,
            max_instances,
            probability,
            matrix,
            color,
            overrides,
            inherit_overrides: inherit,
            control_point_start,
        });
        definitions.push(definition.clone());
        let children = definition["children"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        ensure!(children.len() <= 64, "too many particle children");
        for child in children {
            let file = child["particle"]
                .as_str()
                .or_else(|| child["name"].as_str())
                .context("particle child file")?;
            Self::load_node(
                assets,
                file,
                child,
                properties,
                Some(index),
                depth + 1,
                seed,
                nodes,
                definitions,
                stack,
            )?;
        }
        stack.remove(&file);
        Ok(())
    }
    pub fn capacity(&self) -> usize {
        self.nodes
            .iter()
            .map(|n| n.prototype.max * n.max_instances)
            .sum()
    }
    pub fn node_capacity(&self, index: usize) -> usize {
        self.nodes[index].prototype.max * self.nodes[index].max_instances
    }
    pub fn audio(&self) -> bool {
        self.nodes.iter().any(|node| {
            node.instances.iter().any(|i| i.system.needs_audio())
                || self.emitting
                    && node.prototype.emission_audio
                    && node.parent.is_some_and(|parent| {
                        self.nodes[parent]
                            .instances
                            .iter()
                            .any(|i| i.system.has_emission() || !i.system.particles.is_empty())
                    })
        })
    }
    pub fn pointer(&self) -> bool {
        self.nodes.iter().any(|n| n.prototype.pointer)
    }
    pub fn initialized(&self) -> bool {
        self.started
    }
    pub fn ready(&self) -> bool {
        self.complete
    }
    pub fn set_overrides(&mut self, value: &Value) -> Result<()> {
        if self.nodes[0].overrides == *value {
            return Ok(());
        }
        let mut prepared: Vec<(Value, System)> = Vec::with_capacity(self.nodes.len());
        for (i, node) in self.nodes.iter().enumerate() {
            let overrides = if i == 0 {
                value.clone()
            } else if node.inherit_overrides {
                inherited_overrides(&prepared[node.parent.unwrap()].0)
            } else {
                node.overrides.clone()
            };
            let mut prototype = node.prototype.clone();
            prototype.set_overrides(&overrides)?;
            prepared.push((overrides, prototype));
        }
        ensure!(
            self.nodes
                .iter()
                .zip(&prepared)
                .map(|(n, (_, s))| n.max_instances * s.max)
                .sum::<usize>()
                <= 100_000,
            "particle family capacity exceeds 100000"
        );
        for (node, (overrides, prototype)) in self.nodes.iter_mut().zip(prepared) {
            node.prototype = prototype;
            for instance in &mut node.instances {
                instance.system.set_overrides(&overrides)?;
            }
            node.overrides = overrides;
        }
        Ok(())
    }
    pub fn advance(
        &mut self,
        time: f64,
        pointer: Vec3,
        world_to_local: Mat4,
        root_to_world: Mat4,
        audio: &AudioSnapshot,
    ) {
        let target = ((time.max(0.0) + self.nodes[0].prototype.start as f64) / STEP).floor() as u64;
        if target < self.tick {
            self.reset(true);
        }
        self.tick = target;
        self.input_time = time;
        let deadline = Instant::now() + Duration::from_millis(4);
        if self.pending_step {
            let pending = self
                .nodes
                .iter()
                .enumerate()
                .flat_map(|(index, node)| node.instances.iter().map(move |i| (index, i.id)))
                .collect::<Vec<_>>();
            for (index, id) in pending {
                self.step_instance(index, id, 0., pointer, world_to_local, root_to_world, audio);
            }
            self.schedule = self
                .nodes
                .iter()
                .enumerate()
                .flat_map(|(index, node)| {
                    node.instances
                        .iter()
                        .map(move |i| Reverse((i.next_tick(), index, i.id)))
                })
                .collect();
            self.pending_step = false;
        }
        // Event nodes can own many independently ticking systems. Budget their
        // capacity, so a cheap child warm-up does not hold the entire family.
        let step_budget = 240
            * self
                .nodes
                .iter()
                .map(|node| node.max_instances)
                .sum::<usize>();
        for _ in 0..step_budget {
            let Some(&Reverse((clock, index, id))) = self.schedule.peek() else {
                break;
            };
            if clock > target as i128 {
                break;
            }
            self.schedule.pop();
            if let Some(instance) = self.nodes[index].instances.iter().find(|i| i.id == id)
                && instance.next_tick() == clock
            {
                let dt = if instance.system.initialized() {
                    STEP as f32
                } else {
                    0.
                };
                self.step_instance(index, id, dt, pointer, world_to_local, root_to_world, audio);
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        self.started = self.nodes[0].instances[0].system.initialized();
        self.complete = self
            .schedule
            .peek()
            .is_none_or(|Reverse((clock, _, _))| *clock > target as i128);
    }
    #[allow(clippy::too_many_arguments)]
    fn step_instance(
        &mut self,
        index: usize,
        id: u64,
        dt: f32,
        pointer: Vec3,
        world_to_local: Mat4,
        root_to_world: Mat4,
        audio: &AudioSnapshot,
    ) {
        let (parents, current) = self.nodes.split_at_mut(index);
        let (node, children) = current.split_first_mut().unwrap();
        let Some(position) = node.instances.iter().position(|i| i.id == id) else {
            return;
        };
        let instance = &mut node.instances[position];
        instance.system.begin_frame(self.input_time);
        let mut owner_present = true;
        if let Some((owner, particle)) = instance.binding {
            let parent = &parents[node.parent.unwrap()];
            let parent = parent.instances.iter().find(|i| i.id == owner);
            owner_present = parent.is_some();
            let position = parent.and_then(|i| {
                particle.map_or(Some(i.origin), |id| {
                    i.system
                        .particles
                        .iter()
                        .find(|p| p.id == id)
                        .map(|p| i.system.local_position(p.position))
                })
            });
            if let Some(position) = position {
                let origin = node.matrix.inverse().transform_vector3(position);
                // Local follow systems carry existing particles with their owner.
                // Only world-space births and histories stay at their old positions.
                if !instance.system.world_space && origin != instance.origin {
                    let offset = origin - instance.origin;
                    for particle in &mut instance.system.particles {
                        particle.position += offset;
                        for history in &mut particle.trails {
                            history.translate(offset);
                        }
                    }
                }
                instance.origin = origin;
            } else {
                instance.system.emitting = false;
            }
        }
        let inverse = instance.matrix.inverse();
        instance.system.set_space(root_to_world * instance.matrix);
        instance.system.update_points(
            inverse.transform_point3(pointer),
            inverse * world_to_local,
            instance.origin,
        );
        if let Some(parent) = node.parent.and_then(|parent| {
            parents[parent]
                .instances
                .iter()
                .find(|i| Some(i.id) == instance.owner)
        }) {
            instance
                .system
                .inherit_points(&parent.system, node.control_point_start);
        } else if instance.owner.is_some() {
            instance.system.freeze_point_sources();
        }
        instance.system.point_motion(self.input_time);
        if let Some(parent) = node.parent.and_then(|parent| {
            parents[parent]
                .instances
                .iter()
                .find(|i| Some(i.id) == instance.owner)
        }) {
            instance
                .system
                .inherit_point_motion(&parent.system, node.control_point_start);
        }
        if let Some((owner, Some(particle))) = instance.binding
            && let Some(parent) = parents[node.parent.unwrap()]
                .instances
                .iter()
                .find(|i| i.id == owner)
            && let Some(particle) = parent.system.particles.iter().find(|p| p.id == particle)
        {
            instance
                .system
                .follow_point_motion(&parent.system, particle);
        }
        if let (Some(owner), Some(particle)) = (instance.owner, instance.event_particle)
            && let Some(parent) = parents[node.parent.unwrap()]
                .instances
                .iter()
                .find(|i| i.id == owner)
            && let Some(particle) = parent.system.particles.iter().find(|p| p.id == particle)
        {
            instance
                .system
                .inherit_source(&parent.system, Values::from(particle));
        }
        let remaining = (self.tick as i128 - instance.next_tick()).max(0) as u64;
        instance.system.step_tick_to(dt, audio, remaining);
        // Dispatch before removing an exhausted parent, including its final death event.
        for (offset, child) in children.iter_mut().enumerate() {
            if child.parent != Some(index) {
                continue;
            }
            let child_index = index + 1 + offset;
            let seed = self.seed ^ (child_index as u64).wrapping_mul(0x9e3779b97f4a7c15);
            if child.kind == Kind::Static {
                if let Some((clock, id)) = child.spawn(
                    instance,
                    None,
                    instance.origin,
                    seed,
                    &mut self.serial,
                    self.emitting,
                ) {
                    self.schedule.push(Reverse((clock, child_index, id)));
                }
            } else {
                let events = if child.kind == Kind::Death {
                    &instance.system.events.died
                } else {
                    &instance.system.events.born
                };
                for event in events {
                    if let Some((clock, id)) = child.spawn(
                        instance,
                        Some(event),
                        instance.system.local_position(event.values.position),
                        seed,
                        &mut self.serial,
                        self.emitting,
                    ) {
                        self.schedule.push(Reverse((clock, child_index, id)));
                    }
                }
            }
        }
        if node.kind == Kind::Root
            || node.kind == Kind::Static && owner_present
            || instance.system.emitting
            || !instance.system.particles.is_empty()
        {
            self.schedule
                .push(Reverse((instance.next_tick(), index, id)));
        } else {
            node.instances.remove(position);
        }
    }

    pub fn validate_commands(commands: &Value) -> Result<()> {
        let entries = commands
            .as_array()
            .context("invalid particle command queue")?;
        ensure!(entries.len() <= 128, "particle command budget exceeded");
        for command in entries {
            match command[0].as_str() {
                Some("play" | "pause" | "stop") => {}
                Some("emit") => ensure!(
                    command[1].as_u64().is_some_and(|n| n <= 20_000),
                    "invalid forced particle count"
                ),
                _ => anyhow::bail!("unknown particle command"),
            }
        }
        Ok(())
    }
    pub fn control(&mut self, commands: &Value) -> Result<()> {
        Self::validate_commands(commands)?;
        for command in commands.as_array().unwrap() {
            match command[0].as_str().unwrap() {
                "play" => {
                    if !self.is_playing() {
                        self.reset(false);
                    }
                    for node in &mut self.nodes {
                        for instance in &mut node.instances {
                            if self.emitting && !instance.system.has_emission() {
                                instance.system.restart_emission();
                            }
                            instance.system.emitting = true;
                        }
                    }
                    self.emitting = true;
                    self.pending_step = true;
                }
                "pause" => {
                    self.emitting = false;
                    for node in &mut self.nodes {
                        for instance in &mut node.instances {
                            instance.system.emitting = false;
                        }
                    }
                }
                "stop" => {
                    self.emitting = false;
                    self.reset(false);
                }
                "emit" => {
                    let root = &mut self.nodes[0].instances[0].system;
                    root.forced = root
                        .forced
                        .saturating_add(command[1].as_u64().unwrap() as usize)
                        .min(root.max);
                    self.pending_step = true;
                }
                _ => unreachable!(),
            }
        }
        Ok(())
    }
    pub(super) fn reset(&mut self, rewind: bool) {
        for node in &mut self.nodes {
            node.instances.clear();
        }
        if rewind {
            self.emitting = true;
            self.tick = 0;
            self.serial = 1;
        }
        let root = &mut self.nodes[0];
        let mut system = root.prototype.fresh(hash(self.seed));
        system.emitting = self.emitting;
        root.instances.push(Instance {
            id: 0,
            system,
            matrix: Mat4::IDENTITY,
            origin: Vec3::ZERO,
            binding: None,
            owner: None,
            event_particle: None,
            clock_start: self.tick as i128,
        });
        self.started = false;
        self.pending_step = false;
        self.complete = false;
        self.schedule = BinaryHeap::from([Reverse((self.tick as i128, 0, 0))]);
    }
    pub fn is_playing(&self) -> bool {
        self.nodes.iter().flat_map(|n| &n.instances).any(|i| {
            i.system.has_emission() || !i.system.particles.is_empty() || i.system.forced > 0
        })
    }
    pub fn live(&self) -> usize {
        self.nodes
            .iter()
            .flat_map(|n| &n.instances)
            .map(|i| i.system.particles.len())
            .sum()
    }
}
impl Instance {
    fn next_tick(&self) -> i128 {
        self.clock_start + self.system.tick as i128 + i128::from(self.system.initialized())
    }
}
impl Node {
    fn spawn(
        &mut self,
        parent: &Instance,
        event: Option<&Event>,
        position: Vec3,
        seed: u64,
        serial: &mut u64,
        emitting: bool,
    ) -> Option<(i128, u64)> {
        if self.instances.len() >= self.max_instances
            || self.kind == Kind::Static
                && self
                    .instances
                    .iter()
                    .any(|i| i.binding == Some((parent.id, None)))
        {
            return None;
        }
        let particle = event.map(|e| e.id);
        let seed = hash(seed ^ parent.id ^ particle.unwrap_or(0));
        if ((seed >> 40) as f32 / 16777216.0) >= self.probability {
            return None;
        }
        let mut system = self.prototype.fresh(seed);
        system.emitting = emitting;
        system.set_space(parent.system.space * self.matrix);
        system.inherit_points(&parent.system, self.control_point_start);
        system.seed_motion(&parent.system, self.matrix);
        system.inherit_point_motion(&parent.system, self.control_point_start);
        if let Some(event) = event {
            system.inherit_source(&parent.system, event.values);
        }
        let event_particle = particle.filter(|_| system.inherit_values);
        self.instances.push(Instance {
            id: *serial,
            system,
            matrix: parent.matrix * self.matrix,
            origin: self.matrix.inverse().transform_vector3(position),
            binding: matches!(self.kind, Kind::Follow | Kind::Static)
                .then_some((parent.id, particle)),
            owner: Some(parent.id),
            event_particle,
            clock_start: parent.clock_start + parent.system.tick as i128
                - (self.prototype.start as f64 / STEP).floor() as i128,
        });
        *serial = serial.wrapping_add(1);
        let instance = self.instances.last().unwrap();
        Some((instance.next_tick(), instance.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn fixture() -> (tempfile::TempDir, Assets) {
        let root = tempfile::tempdir().unwrap();
        let mut package = 8u32.to_le_bytes().to_vec();
        package.extend(b"PKGV0001");
        package.extend(0u32.to_le_bytes());
        std::fs::write(root.path().join("scene.pkg"), package).unwrap();
        let parent = json!({"maxcount":2,"emitter":[{"name":"boxrandom","instantaneous":2,"rate":0}],"initializer":[{"name":"lifetimerandom","min":0.25,"max":0.25},{"name":"velocityrandom","min":"24 0 0","max":"24 0 0"}],"operator":[{"name":"movement"}],"children":[
            {"name":"child.json","type":"eventfollow","maxcount":2,"origin":"0 4 0","scale":"2 1 1"},
            {"name":"burst.json","type":"eventspawn"},{"name":"burst.json","type":"eventdeath"},
            {"name":"child.json","type":"static","origin":"8 0 0"}
        ]});
        let child = json!({"maxcount":12,"emitter":[{"name":"boxrandom","rate":24}],"initializer":[{"name":"lifetimerandom","min":0.25,"max":0.25}]});
        let burst = json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":0.25,"max":0.25}]});
        for (name, value) in [
            ("parent.json", parent),
            ("child.json", child),
            ("burst.json", burst),
        ] {
            std::fs::write(root.path().join(name), serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let assets = Assets::open(root.path(), &root.path().join("scene.pkg"), None).unwrap();
        (root, assets)
    }
    pub(super) fn advance(family: &mut Family, time: f64) {
        loop {
            family.advance(
                time,
                Vec3::ZERO,
                Mat4::IDENTITY,
                Mat4::IDENTITY,
                &AudioSnapshot::default(),
            );
            if family.ready() {
                break;
            }
        }
    }
    fn snapshot(family: &Family) -> Vec<Vec<(u64, Mat4, Vec<super::super::simulation::Particle>)>> {
        family
            .nodes
            .iter()
            .map(|n| {
                n.instances
                    .iter()
                    .map(|i| (i.id, i.matrix, i.system.particles.clone()))
                    .collect()
            })
            .collect()
    }
    #[test]
    fn final_death_seeds_copied_points_before_parent_release_and_keeps_world_targets() {
        let (root, assets) = fixture();
        for (name, value) in [
            (
                "last-root.json",
                json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":0.125,"max":0.125}],"children":[{"name":"last-child.json","type":"eventspawn"}]}),
            ),
            (
                "last-child.json",
                json!({"maxcount":1,"controlpoint":[{"id":1,"offset":"16 0 0"}],"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":0.125,"max":0.125}],"children":[{"name":"last-death.json","type":"eventdeath"}]}),
            ),
            (
                "last-death.json",
                json!({"flags":1,"starttime":0.25,"maxcount":4,"controlpoint":[{"id":0,"flags":4,"parentcontrolpoint":1}],"emitter":[{"name":"boxrandom","instantaneous":1,"rate":4,"duration":0.5}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"inheritcontrolpointvelocity","controlpoint":0,"min":1,"max":1}]}),
            ),
        ] {
            std::fs::write(root.path().join(name), serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let mut family = Family::load(&assets, "last-root.json", &json!({}), &Properties::new(), 7)
            .unwrap()
            .0;
        let update = |family: &mut Family, time, space: Mat4| {
            loop {
                family.advance(
                    time,
                    Vec3::ZERO,
                    space.inverse(),
                    space,
                    &AudioSnapshot::default(),
                );
                if family.ready() {
                    break;
                }
            }
        };
        update(
            &mut family,
            0.25,
            Mat4::from_translation(Vec3::new(10., 20., 0.)),
        );
        assert!(family.nodes[1].instances.is_empty());
        let particles = &family.nodes[2].instances[0].system.particles;
        assert!(!particles.is_empty());
        assert!(
            particles
                .iter()
                .all(|p| p.position.abs_diff_eq(Vec3::new(26., 20., 0.), 1e-4))
        );
        update(
            &mut family,
            0.5,
            Mat4::from_translation(Vec3::new(30., 40., 0.)),
        );
        assert!(
            family.nodes[2].instances[0]
                .system
                .particles
                .iter()
                .all(|p| p.position.abs_diff_eq(Vec3::new(26., 20., 0.), 1e-4))
        );
        update(
            &mut family,
            2.,
            Mat4::from_translation(Vec3::new(30., 40., 0.)),
        );
        assert!(family.nodes[2].instances.is_empty());
    }

    #[test]
    fn parent_control_points_transform_raw_copy_and_particle_driven_slots_keep_child_defaults() {
        let (root, assets) = fixture();
        for (name, value) in [
            (
                "copy-root.json",
                json!({"flags":1,"maxcount":2,"controlpoint":[{"id":1,"offset":"10 20 0"}],"emitter":[{"name":"boxrandom","instantaneous":2,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"mapsequencearoundcontrolpoint","count":2,"speedmin":"5 0 0","speedmax":"5 0 0"}],"children":[{"name":"copy-child.json","type":"static","origin":"4 6 0","angles":"0 0 0.7","scale":"2 1 1","flags":1,"controlpointstartindex":3}]}),
            ),
            (
                "copy-child.json",
                json!({"maxcount":1,"controlpoint":[{"id":0,"flags":4,"parentcontrolpoint":1},{"id":1,"flags":12,"parentcontrolpoint":1}],"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1}]}),
            ),
        ] {
            std::fs::write(root.path().join(name), serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let mut family = Family::load(
            &assets,
            "copy-root.json",
            &json!({"instanceoverride":{"count":2,"controlpoint1":[20,40,0]}}),
            &Properties::new(),
            7,
        )
        .unwrap()
        .0;
        let space = Mat4::from_translation(Vec3::new(100., 200., 0.));
        let update = |family: &mut Family, time| {
            loop {
                family.advance(
                    time,
                    Vec3::ZERO,
                    space.inverse(),
                    space,
                    &AudioSnapshot::default(),
                );
                if family.ready() {
                    break;
                }
            }
        };
        update(&mut family, 0.);
        let check = |family: &Family, expected: Vec3| {
            let parent = &family.nodes[0].instances[0].system;
            let child = &family.nodes[1].instances[0].system;
            assert_eq!(
                child.max, 1,
                "child capacity must retain its authored count"
            );
            assert!(
                child
                    .space
                    .transform_point3(child.points[0])
                    .abs_diff_eq(parent.space.transform_point3(expected), 1e-4)
            );
            assert!(
                child.points[1].abs_diff_eq(expected, 1e-4),
                "raw copy must not inherit the parent's offset override twice"
            );
            for (index, particle) in parent.particles.iter().enumerate() {
                assert!(
                    child
                        .space
                        .transform_point3(child.points[3 + index])
                        .abs_diff_eq(particle.position, 1e-4)
                );
            }
        };
        check(&family, Vec3::new(20., 40., 0.));
        family
            .set_overrides(&json!({"count":2,"controlpoint1":[30,50,0]}))
            .unwrap();
        update(&mut family, 0.1);
        check(&family, Vec3::new(30., 50., 0.));
        let before = snapshot(&family);
        update(&mut family, 0.1);
        assert_eq!(snapshot(&family), before);
        update(&mut family, 2.);
        family.control(&json!([["stop", 0]])).unwrap();
        assert_eq!(family.nodes[1].instances.len(), 0);
        let mut bad =
            json!({"children":[{"name":"copy-child.json","flags":1,"controlpointstartindex":8}]});
        std::fs::write(
            root.path().join("bad-copy.json"),
            serde_json::to_vec(&bad).unwrap(),
        )
        .unwrap();
        assert!(Family::load(&assets, "bad-copy.json", &json!({}), &Properties::new(), 7).is_err());
        bad["children"][0]["controlpointstartindex"] = json!(1.5);
        std::fs::write(
            root.path().join("bad-copy.json"),
            serde_json::to_vec(&bad).unwrap(),
        )
        .unwrap();
        assert!(Family::load(&assets, "bad-copy.json", &json!({}), &Properties::new(), 7).is_err());
    }

    #[test]
    fn local_follow_prewarm_moves_with_parent_but_world_births_stay_put() {
        let (root, assets) = fixture();
        for world in [false, true] {
            for (name, value) in [
                (
                    "follow-root.json",
                    json!({"maxcount":1,
                    "emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],
                    "initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"velocityrandom","min":"12 0 0","max":"12 0 0"}],
                    "operator":[{"name":"movement"}],
                    "children":[{"name":"follow-child.json","type":"eventfollow","origin":"2 0 0"}]}),
                ),
                (
                    "follow-child.json",
                    json!({"flags":u32::from(world),"starttime":0.25,"maxcount":1,
                    "emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],
                    "initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"velocityrandom","min":"0 4 0","max":"0 4 0"}],
                    "operator":[{"name":"movement"}],"renderer":[{"name":"ropetrail","length":0.25,"segments":4}]}),
                ),
            ] {
                std::fs::write(root.path().join(name), serde_json::to_vec(&value).unwrap())
                    .unwrap();
            }
            let mut family = Family::load(
                &assets,
                "follow-root.json",
                &json!({}),
                &Properties::new(),
                7,
            )
            .unwrap()
            .0;
            advance(&mut family, 0.);
            advance(&mut family, 0.25);
            let instance = &family.nodes[1].instances[0];
            let particle = &instance.system.particles[0];
            let position = if world {
                particle.position
            } else {
                instance.matrix.transform_point3(particle.position)
            };
            assert!(
                (position.x - if world { 2. } else { 5. }).abs() < 1e-4,
                "prewarmed local glow was left at its birth origin: {position:?}, world={world}"
            );
            let mut history = Vec::new();
            particle.trails[0].points(&mut history);
            for point in history {
                let point = if world {
                    point.position
                } else {
                    instance.matrix.transform_point3(point.position)
                };
                assert!(
                    (point.x - position.x).abs() < 1e-4,
                    "local trail must move with its parent"
                );
            }
        }
    }
    #[test]
    fn world_child_follows_a_world_parent_without_dragging_previous_births() {
        let (root, assets) = fixture();
        for (name, value) in [
            (
                "world-root.json",
                json!({"flags":1,"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"velocityrandom","min":"12 0 0","max":"12 0 0"}],"operator":[{"name":"movement"}],"children":[{"name":"world-child.json","type":"eventfollow","origin":"2 0 0"}]}),
            ),
            (
                "world-child.json",
                json!({"flags":1,"maxcount":10,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":12}],"initializer":[{"name":"lifetimerandom","min":1,"max":1}]}),
            ),
        ] {
            std::fs::write(root.path().join(name), serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let mut family = Family::load(
            &assets,
            "world-root.json",
            &json!({}),
            &Properties::new(),
            7,
        )
        .unwrap()
        .0;
        let advance = |family: &mut Family, time, space: Mat4| {
            loop {
                family.advance(
                    time,
                    Vec3::ZERO,
                    space.inverse(),
                    space,
                    &AudioSnapshot::default(),
                );
                if family.ready() {
                    break;
                }
            }
        };
        advance(
            &mut family,
            0.,
            Mat4::from_translation(Vec3::new(10., 20., 0.)),
        );
        assert_eq!(
            family.nodes[1].instances[0].system.particles[0].position,
            Vec3::new(12., 20., 0.)
        );
        advance(
            &mut family,
            0.25,
            Mat4::from_translation(Vec3::new(30., 40., 0.)),
        );
        assert!(
            family.nodes[0].instances[0].system.particles[0]
                .position
                .abs_diff_eq(Vec3::new(13., 20., 0.), 1e-4)
        );
        let particles = &family.nodes[1].instances[0].system.particles;
        assert_eq!(particles.len(), 4);
        assert_eq!(particles[0].position, Vec3::new(12., 20., 0.));
        assert!(
            particles
                .iter()
                .all(|p| p.position.x <= 15.001 && (p.position.y - 20.).abs() < 1e-4)
        );
        advance(
            &mut family,
            1.5,
            Mat4::from_translation(Vec3::new(30., 40., 0.)),
        );
        assert!(
            !family.nodes[1].instances.is_empty(),
            "child particles must outlive their parent"
        );
        advance(
            &mut family,
            2.,
            Mat4::from_translation(Vec3::new(30., 40., 0.)),
        );
        assert!(family.nodes[1].instances.is_empty());
    }
    #[test]
    fn children_prewarm_from_their_birth_clock_and_nested_events_are_frame_rate_independent() {
        let (root, assets) = fixture();
        for (name, value) in [
            (
                "warm-root.json",
                json!({"maxcount":1,"children":[{"name":"warm-child.json","type":"static"}]}),
            ),
            (
                "warm-child.json",
                json!({"starttime":0.25,"maxcount":8,"emitter":[{"name":"boxrandom","rate":8}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"velocityrandom","min":"12 0 0","max":"12 0 0"}],"operator":[{"name":"movement"}],"children":[{"name":"warm-event.json","type":"eventspawn","maxcount":8}]}),
            ),
            (
                "warm-event.json",
                json!({"starttime":0.125,"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"velocityrandom","min":"24 0 0","max":"24 0 0"}],"operator":[{"name":"movement"}]}),
            ),
        ] {
            std::fs::write(root.path().join(name), serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let load = || {
            Family::load(
                &assets,
                "warm-root.json",
                &json!({}),
                &Properties::new(),
                17,
            )
            .unwrap()
            .0
        };
        let mut initial = load();
        advance(&mut initial, 0.);
        assert!(initial.nodes[0].instances[0].system.particles.is_empty());
        assert_eq!(initial.nodes[1].instances[0].system.particles.len(), 2);
        assert_eq!(initial.nodes[2].instances.len(), 2);
        let ages = initial.nodes[2]
            .instances
            .iter()
            .map(|i| i.system.particles[0].age)
            .collect::<Vec<_>>();
        assert!((ages[0] - 0.25).abs() < 1e-6, "first past birth: {ages:?}");
        assert!(
            (ages[1] - 0.125).abs() < 1e-6,
            "second past birth: {ages:?}"
        );
        let first = snapshot(&initial);
        let mut at_24 = load();
        let mut at_60 = load();
        for frame in 0..=12 {
            advance(&mut at_24, frame as f64 / 24.);
        }
        for frame in 0..=30 {
            advance(&mut at_60, frame as f64 / 60.);
        }
        let mut late = load();
        advance(&mut late, 0.5);
        assert_eq!(snapshot(&at_24), snapshot(&at_60));
        assert_eq!(snapshot(&at_24), snapshot(&late));
        let held = snapshot(&late);
        advance(&mut late, 0.5);
        assert_eq!(snapshot(&late), held);
        advance(&mut late, 0.);
        assert_eq!(snapshot(&late), first);
        assert_eq!(
            late.schedule.len(),
            late.nodes.iter().map(|n| n.instances.len()).sum::<usize>()
        );
    }

    #[test]
    fn continuous_follow_children_prewarm_without_skipping_display_frames() {
        let (root, assets) = fixture();
        for (name, value) in [
            (
                "continuous-root.json",
                json!({"maxcount":120,"emitter":[{"name":"boxrandom","rate":40}],
                    "initializer":[{"name":"lifetimerandom","min":5,"max":8}],
                    "children":[{"name":"continuous-child.json","type":"eventfollow","maxcount":40}]}),
            ),
            (
                "continuous-child.json",
                json!({"starttime":3,"maxcount":8,"emitter":[{"name":"boxrandom","rate":1}],
                    "initializer":[{"name":"lifetimerandom","min":3,"max":5}]}),
            ),
        ] {
            std::fs::write(root.path().join(name), serde_json::to_vec(&value).unwrap()).unwrap();
        }
        // The vortex's rate and child warm-up create several 360-tick jobs
        // per display frame, even though there are only two definitions.
        let mut family = Family::load(
            &assets,
            "continuous-root.json",
            &json!({"instanceoverride":{"rate":3.35}}),
            &Properties::new(),
            17,
        )
        .unwrap()
        .0;
        advance(&mut family, 0.);
        for frame in 1..=90 {
            family.advance(
                frame as f64 / 30.,
                Vec3::ZERO,
                Mat4::IDENTITY,
                Mat4::IDENTITY,
                &AudioSnapshot::default(),
            );
            assert!(
                family.ready(),
                "particle display frame {frame} was held for child prewarm"
            );
        }
        assert!(family.nodes[1].instances.len() > 1);
    }
    #[test]
    fn exhausted_event_parents_deliver_final_death_and_large_child_prewarm_is_bounded() {
        let (root, assets) = fixture();
        for (name, value) in [
            (
                "event-root.json",
                json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":0.125,"max":0.125}],"children":[{"name":"event-child.json","type":"eventspawn"}]}),
            ),
            (
                "event-child.json",
                json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":0.125,"max":0.125}],"children":[{"name":"event-death.json","type":"eventdeath"}]}),
            ),
            (
                "event-death.json",
                json!({"starttime":0.25,"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1}]}),
            ),
        ] {
            std::fs::write(root.path().join(name), serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let mut family = Family::load(
            &assets,
            "event-root.json",
            &json!({}),
            &Properties::new(),
            17,
        )
        .unwrap()
        .0;
        advance(&mut family, 0.125);
        assert!(family.nodes[1].instances.is_empty());
        assert_eq!(
            family.nodes[2].instances.len(),
            1,
            "final death event was lost on parent removal"
        );
        assert!((family.nodes[2].instances[0].system.particles[0].age - 0.25).abs() < 1e-6);
        advance(&mut family, 1.);
        assert_eq!(family.live(), 0);
        assert!(family.nodes[2].instances.is_empty());
        assert_eq!(family.schedule.len(), 1);
        std::fs::write(
        root.path().join("event-child.json"),
        br#"{"starttime":3600,"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":3600,"max":3600}]}"#,
    )
    .unwrap();
        let mut warm = Family::load(
            &assets,
            "event-root.json",
            &json!({}),
            &Properties::new(),
            17,
        )
        .unwrap()
        .0;
        warm.advance(
            0.,
            Vec3::ZERO,
            Mat4::IDENTITY,
            Mat4::IDENTITY,
            &AudioSnapshot::default(),
        );
        assert!(!warm.ready());
        let ticks = warm.nodes[1].instances[0].system.tick;
        assert!((1..=9840).contains(&ticks));
        warm.advance(
            0.,
            Vec3::ZERO,
            Mat4::IDENTITY,
            Mat4::IDENTITY,
            &AudioSnapshot::default(),
        );
        assert!(warm.nodes[1].instances[0].system.tick > ticks);
        assert!(!warm.ready());
        assert_eq!(warm.schedule.len(), 2);
    }
    #[test]
    fn playback_commands_pause_emission_force_bursts_stop_restart_and_validate_atomically() {
        let (root, assets) = fixture();
        std::fs::write(root.path().join("control.json"), br#"{"maxcount":4,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":16}],"initializer":[{"name":"lifetimerandom","min":0.25,"max":0.25},{"name":"velocityrandom","min":"24 0 0","max":"24 0 0"}],"operator":[{"name":"movement"}]}"#).unwrap();
        let mut family = Family::load(&assets, "control.json", &json!({}), &Properties::new(), 17)
            .unwrap()
            .0;
        advance(&mut family, 0.);
        assert_eq!(family.live(), 1);
        assert!(family.is_playing());
        assert!(
            family
                .control(&json!([["pause", 0], ["emit", 20001]]))
                .is_err()
        );
        assert!(
            family.emitting,
            "invalid queue must not partially pause emission"
        );
        family.control(&json!([["pause", 0]])).unwrap();
        advance(&mut family, 0.125);
        assert_eq!(family.live(), 1);
        assert!((family.nodes[0].instances[0].system.particles[0].position.x - 3.).abs() < 1e-5);
        advance(&mut family, 0.5);
        assert!(!family.is_playing());
        family.control(&json!([["emit", 3]])).unwrap();
        advance(&mut family, 0.5);
        assert_eq!(family.live(), 3, "forced particles bypass paused emission");
        assert!(!family.emitting);
        family.control(&json!([["stop", 0], ["emit", 20]])).unwrap();
        advance(&mut family, 0.5);
        assert_eq!(family.live(), 4, "forced emission respects the pool bound");
        family.control(&json!([["stop", 0]])).unwrap();
        assert!(!family.is_playing());
        family.control(&json!([["play", 0]])).unwrap();
        advance(&mut family, 0.5);
        assert_eq!(family.live(), 1, "play restarts the stopped initial burst");
        advance(&mut family, 0.625);
        assert_eq!(family.live(), 3);
        family.control(&json!([["stop", 0]])).unwrap();
        advance(&mut family, 0.);
        assert_eq!(
            family.live(),
            1,
            "rewinding starts a fresh deterministic timeline"
        );
    }
    #[test]
    fn audio_demand_tracks_emission_live_behaviors_and_stop() {
        let (root, assets) = fixture();
        let definition = json!({"maxcount":4,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":16,"audioprocessingmode":1,"audioprocessingbounds":"0 1"}],"initializer":[{"name":"lifetimerandom","min":1,"max":1}]});
        std::fs::write(
            root.path().join("audio.json"),
            serde_json::to_vec(&definition).unwrap(),
        )
        .unwrap();
        let load = || {
            Family::load(&assets, "audio.json", &json!({}), &Properties::new(), 17)
                .unwrap()
                .0
        };
        let mut family = load();
        assert!(
            family.audio(),
            "an audio-gated emitter needs monitor data before its first tick"
        );
        family.control(&json!([["pause", 0], ["emit", 1]])).unwrap();
        advance(&mut family, 0.);
        assert_eq!(family.live(), 1);
        assert!(
            !family.audio(),
            "a paused emitter does not need FFT for static live particles"
        );
        family.control(&json!([["play", 0]])).unwrap();
        assert!(family.audio());
        family.control(&json!([["stop", 0]])).unwrap();
        assert!(!family.audio());
        let mut definition = definition;
        definition["operator"] =
            json!([{"name":"turbulence","audioprocessingmode":1,"phasemax":1}]);
        std::fs::write(
            root.path().join("audio.json"),
            serde_json::to_vec(&definition).unwrap(),
        )
        .unwrap();
        let mut live = load();
        live.control(&json!([["pause", 0], ["emit", 1]])).unwrap();
        advance(&mut live, 0.);
        assert!(
            live.audio(),
            "audio-bound behavior still responds while emission is paused"
        );
        live.control(&json!([["stop", 0]])).unwrap();
        assert!(!live.audio());
    }
    #[test]
    fn children_inherit_layer_multipliers_and_keep_existing_particle_birth_values() {
        let (root, assets) = fixture();
        let mut child = json!({"maxcount":4,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":2}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"sizerandom","min":4,"max":4},{"name":"velocityrandom","min":"8 0 0","max":"8 0 0"},{"name":"colorrandom","min":"0 255 0","max":"0 255 0"}]});
        std::fs::write(
            root.path().join("child.json"),
            serde_json::to_vec(&child).unwrap(),
        )
        .unwrap();
        let parent = json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2}],"children":[{"name":"child.json","type":"static"},{"name":"child.json","type":"eventfollow"}]});
        std::fs::write(
            root.path().join("override-parent.json"),
            serde_json::to_vec(&parent).unwrap(),
        )
        .unwrap();
        let overrides = json!({"alpha":0.5,"colorn":[0.5,0.5,0.5],"count":2,"size":3,"lifetime":4,"speed":5,"rate":2});
        let mut family = Family::load(
            &assets,
            "override-parent.json",
            &json!({"instanceoverride":overrides}),
            &Properties::new(),
            7,
        )
        .unwrap()
        .0;
        advance(&mut family, 0.);
        for node in &family.nodes[1..] {
            let system = &node.instances[0].system;
            let p = &system.particles[0];
            assert_eq!(system.max, 4);
            assert_eq!(p.size, 12.);
            assert_eq!(p.life, 8.);
            assert_eq!(p.velocity.x, 40.);
            assert_eq!(p.color, Vec4::new(0.5, 0.5, 0.5, 0.5));
        }
        family.set_overrides(&json!({"alpha":0.5,"colorn":[1,0,0],"count":3,"size":7,"lifetime":8,"speed":9,"rate":4})).unwrap();
        advance(&mut family, 0.1);
        for node in &family.nodes[1..] {
            let system = &node.instances[0].system;
            let p = &system.particles[0];
            assert_eq!(system.max, 4);
            assert_eq!(p.size, 12.);
            assert_eq!(p.life, 8.);
            assert_eq!(p.color, Vec4::new(1., 0., 0., 0.5));
            assert!((p.age - 0.4).abs() < 1e-5, "child simulation rate");
            assert_eq!(system.particles.len(), 3, "child emission count and rate");
            let born = &system.particles[1];
            assert_eq!(born.size, 28.);
            assert_eq!(born.life, 16.);
            assert_eq!(born.velocity.x, 72.);
        }
        child["flags"] = json!(8 | 16 | 32 | 64 | 128);
        std::fs::write(
            root.path().join("child.json"),
            serde_json::to_vec(&child).unwrap(),
        )
        .unwrap();
        let mut disabled = Family::load(
            &assets,
            "override-parent.json",
            &json!({"instanceoverride":overrides}),
            &Properties::new(),
            7,
        )
        .unwrap()
        .0;
        advance(&mut disabled, 0.);
        for node in &disabled.nodes[1..] {
            let system = &node.instances[0].system;
            let p = &system.particles[0];
            assert_eq!(system.max, 4);
            assert_eq!(p.size, 4.);
            assert_eq!(p.life, 2.);
            assert_eq!(p.velocity.x, 8.);
            assert_eq!(p.color, Vec4::new(0., 1., 0., 0.5));
            assert_eq!(system.color_multiplier(), Vec4::ONE);
        }
    }

    #[test]
    fn children_fixed_ticks_events_follow_transforms_pause_rewind_and_release() {
        let (root, assets) = fixture();
        // A trail that leaves old births behind explicitly uses world space.
        let file = root.path().join("child.json");
        let mut child: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        child["flags"] = json!(1);
        std::fs::write(file, serde_json::to_vec(&child).unwrap()).unwrap();
        let load = || {
            Family::load(&assets, "parent.json", &json!({}), &Properties::new(), 17)
                .unwrap()
                .0
        };
        let mut a = load();
        let mut b = load();
        advance(&mut a, 0.0);
        assert_eq!(a.nodes[1].instances.len(), 2);
        assert_eq!(a.nodes[2].instances.len(), 2);
        assert!(
            a.nodes[3].instances.is_empty(),
            "death child started before parent died"
        );
        advance(&mut a, 0.125);
        let instance = &a.nodes[1].instances[0];
        assert!(instance.system.particles.len() >= 2);
        let world: Vec<_> = instance
            .system
            .particles
            .iter()
            .map(|p| p.position)
            .collect();
        assert!(
            world[0].x < world[1].x,
            "follow emission moved the old trail with its parent"
        );
        assert!(world.iter().all(|p| (p.y - 4.0).abs() < 1e-5));
        for frame in 0..61 {
            advance(&mut a, frame as f64 / 60.0);
        }
        for frame in 0..31 {
            advance(&mut b, frame as f64 / 30.0);
        }
        assert_eq!(snapshot(&a), snapshot(&b));
        assert!(
            a.nodes[1].instances.is_empty(),
            "dead parent retained follow instances"
        );
        assert!(
            a.nodes[2].instances.is_empty() && a.nodes[3].instances.is_empty(),
            "finished event instance retained state"
        );
        let paused = snapshot(&a);
        advance(&mut a, 1.0);
        assert_eq!(paused, snapshot(&a));
        advance(&mut a, 0.0);
        let mut initial = load();
        advance(&mut initial, 0.0);
        assert_eq!(snapshot(&a), snapshot(&initial));
        advance(&mut a, 0.25);
        assert!(
            !a.nodes[3].instances.is_empty(),
            "parent death did not trigger child"
        );
    }
    #[test]
    fn cycles_escape_and_instance_budgets_are_rejected_and_overrides_are_transactional() {
        let (root, assets) = fixture();
        std::fs::write(
            root.path().join("cycle.json"),
            br#"{"children":[{"name":"cycle.json"}]}"#,
        )
        .unwrap();
        assert!(Family::load(&assets, "cycle.json", &json!({}), &Properties::new(), 0).is_err());
        assert!(
            Family::load(&assets, "../parent.json", &json!({}), &Properties::new(), 0).is_err()
        );
        let mut family = Family::load(&assets, "parent.json", &json!({}), &Properties::new(), 0)
            .unwrap()
            .0;
        family.nodes[1].max_instances = 4;
        let capacity = family.capacity();
        assert!(family.set_overrides(&json!({"count":-1})).is_err());
        assert_eq!(capacity, family.capacity());
        assert!(
            family
                .set_overrides(&json!({"controlpoint0":"NaN 0 0"}))
                .is_err()
        );
        assert_eq!(capacity, family.capacity());
        let mut empty = json!({"maxcount":0,"children":vec![json!({"name":"empty-child.json","type":"eventspawn","maxcount":256});16]});
        std::fs::write(root.path().join("empty-child.json"), br#"{"maxcount":0}"#).unwrap();
        std::fs::write(
            root.path().join("empty-family.json"),
            serde_json::to_vec(&empty).unwrap(),
        )
        .unwrap();
        let error = Family::load(
            &assets,
            "empty-family.json",
            &json!({}),
            &Properties::new(),
            0,
        )
        .err()
        .unwrap();
        assert!(
            format!("{error:#}").contains("instance capacity"),
            "zero particle pools bypassed the instance budget: {error:#}"
        );
        empty["children"].as_array_mut().unwrap().pop();
        std::fs::write(
            root.path().join("empty-family.json"),
            serde_json::to_vec(&empty).unwrap(),
        )
        .unwrap();
        assert!(
            Family::load(
                &assets,
                "empty-family.json",
                &json!({}),
                &Properties::new(),
                0
            )
            .is_ok()
        );
    }
}

#[cfg(test)]
mod event_values_tests {
    use super::tests::{advance, fixture};
    use super::*;

    fn write(root: &tempfile::TempDir, name: &str, value: &Value) {
        std::fs::write(root.path().join(name), serde_json::to_vec(value).unwrap()).unwrap();
    }
    fn particle(family: &Family, node: usize) -> &super::super::simulation::Particle {
        &family.nodes[node].instances[0].system.particles[0]
    }

    #[test]
    fn event_birth_color_is_frozen_live_operator_updates_and_death_keeps_last_snapshot() {
        let (root, assets) = fixture();
        write(
            &root,
            "source.json",
            &json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":0.125,"max":0.125},{"name":"sizerandom","min":8,"max":8},{"name":"colorrandom","min":"255 255 255","max":"255 255 255"}],"operator":[{"name":"colorchange","startvalue":"1 0 0","endvalue":"0 0 1","endtime":0.5}],"children":[{"name":"birth.json","type":"eventspawn"},{"name":"live.json","type":"eventfollow"},{"name":"death.json","type":"eventdeath"}]}),
        );
        let child = json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"sizerandom","min":4,"max":4},{"name":"inheritinitialvaluefromevent"},{"name":"inheritinitialvaluefromevent","input":"size"}]});
        write(&root, "birth.json", &child);
        write(&root, "death.json", &child);
        let mut live = child;
        live["operator"] = json!([{"name":"inheritvaluefromevent"}]);
        write(&root, "live.json", &live);
        let mut family = Family::load(&assets, "source.json", &Value::Null, &Properties::new(), 19)
            .unwrap()
            .0;
        advance(&mut family, 0.);
        assert_eq!(particle(&family, 1).color, Vec4::new(1., 0., 0., 1.));
        assert_eq!(particle(&family, 2).color, Vec4::new(1., 0., 0., 1.));
        assert_eq!(particle(&family, 1).size, 8.);
        advance(&mut family, 0.075);
        assert_eq!(particle(&family, 1).color, Vec4::new(1., 0., 0., 1.));
        assert_eq!(particle(&family, 2).color, Vec4::new(0., 0., 1., 1.));
        advance(&mut family, 0.125);
        assert!(family.nodes[0].instances[0].system.particles.is_empty());
        assert_eq!(particle(&family, 3).color, Vec4::new(0., 0., 1., 1.));
        advance(&mut family, 0.25);
        assert_eq!(particle(&family, 2).color, Vec4::new(0., 0., 1., 1.));
        assert_eq!(particle(&family, 3).color, Vec4::new(0., 0., 1., 1.));
        advance(&mut family, 1.2);
        assert!(family.nodes.iter().skip(1).all(|n| n.instances.is_empty()));
        assert!(
            Family::load(&assets, "source.json", &Value::Null, &Properties::new(), 19)
                .unwrap()
                .0
                .nodes[0]
                .prototype
                .event_mask
                != 0
        );
    }

    #[test]
    fn event_world_velocity_and_position_are_converted_once_for_birth_and_live_operations() {
        let (root, assets) = fixture();
        for parent_world in [0, 1] {
            for child_world in [0, 1] {
                write(
                    &root,
                    "world-source.json",
                    &json!({"flags":parent_world,"maxcount":1,"emitter":[{"name":"boxrandom","origin":"2 0 0","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":0.25,"max":0.25},{"name":"velocityrandom","min":"4 0 0","max":"4 0 0"}],"children":[{"name":"world-inherit.json","type":"eventspawn","scale":"3 2 1","angles":"0 0 0.5"}]}),
                );
                write(
                    &root,
                    "world-inherit.json",
                    &json!({"flags":child_world,"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"inheritinitialvaluefromevent","input":"velocity"},{"name":"inheritinitialvaluefromevent","input":"position"}],"operator":[{"name":"inheritvaluefromevent","input":"position"},{"name":"inheritvaluefromevent","input":"velocity"}]}),
                );
                let mut family = Family::load(
                    &assets,
                    "world-source.json",
                    &Value::Null,
                    &Properties::new(),
                    19,
                )
                .unwrap()
                .0;
                let update = |family: &mut Family, time, space: Mat4| {
                    loop {
                        family.advance(
                            time,
                            Vec3::ZERO,
                            space.inverse(),
                            space,
                            &AudioSnapshot::default(),
                        );
                        if family.ready() {
                            break;
                        }
                    }
                };
                let space = Mat4::from_translation(Vec3::new(8., 4., 0.))
                    * Mat4::from_rotation_z(0.7)
                    * Mat4::from_scale(Vec3::new(2., 1., 1.));
                let common = |family: &Family, node: usize| {
                    let system = &family.nodes[node].instances[0].system;
                    let p = &system.particles[0];
                    if system.world_space {
                        (p.position, p.velocity)
                    } else {
                        (
                            system.space.transform_point3(p.position),
                            system.space.transform_vector3(p.velocity),
                        )
                    }
                };
                update(&mut family, 0., space);
                let (pp, pv) = common(&family, 0);
                let (cp, cv) = common(&family, 1);
                assert!((pp - cp).length() < 0.00001);
                assert!((pv - cv).length() < 0.00001);
                let moved = Mat4::from_translation(Vec3::new(32., 2., 0.)) * space;
                update(&mut family, 0.125, moved);
                let (pp, pv) = common(&family, 0);
                let (cp, cv) = common(&family, 1);
                assert!((pp - cp).length() < 0.00001);
                assert!((pv - cv).length() < 0.00001);
                update(&mut family, 0.3, moved);
                let last = common(&family, 1);
                update(
                    &mut family,
                    0.5,
                    Mat4::from_translation(Vec3::splat(100.)) * moved,
                );
                let held = common(&family, 1);
                assert!((last.0 - held.0).length() < 0.00005);
                assert!((last.1 - held.1).length() < 0.00005);
            }
        }
        write(
            &root,
            "static-only.json",
            &json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"children":[{"name":"burst.json","type":"static"}]}),
        );
        let mut family = Family::load(
            &assets,
            "static-only.json",
            &Value::Null,
            &Properties::new(),
            1,
        )
        .unwrap()
        .0;
        advance(&mut family, 0.);
        assert_eq!(family.nodes[0].prototype.event_mask, 0);
        assert!(family.nodes[0].instances[0].system.events.born.is_empty());
    }
}
