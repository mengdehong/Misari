//! WE particle simulation. Fixed steps and seeds are independent of output FPS.
mod boids;
mod collision;
mod colors;
mod event_values;
mod position_offset;
mod positions;
mod remap;
mod turbulence;
mod vortex;
use crate::{audio::AudioSnapshot, scene::bindings::components, scene::numbers};
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec2, Vec3, Vec4};
use rand_chacha::ChaCha8Rng;
use rand_core::{Rng, SeedableRng};
use serde_json::Value;
use std::f32::consts::TAU;
use turbulence::{Force, Velocity};

pub const MAX_PARTICLES: usize = 20_000;
pub const STEP: f64 = 1.0 / 120.0;
pub(super) use collision::Models;
pub(super) use event_values::{Event, Values};
pub(super) fn validate_overrides(value: &Value) -> Result<()> {
    Overrides::parse(value)?;
    for i in 0..8 {
        if !value[format!("controlpoint{i}")].is_null() {
            vector(value, &format!("controlpoint{i}"), Vec3::ZERO)?;
        }
    }
    Ok(())
}
#[derive(Clone, Default)]
pub(super) struct Events {
    pub born: Vec<Event>,
    pub died: Vec<Event>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Particle {
    pub id: u64,
    pub position: Vec3,
    pub velocity: Vec3,
    pub rotation: Vec3,
    pub angular: Vec3,
    pub color: Vec4,
    pub size: f32,
    pub age: f32,
    age_exact: f64,
    pub life: f32,
    base_color: Vec4,
    base_size: f32,
    random: Vec3,
    osc_phase_cos: Vec3,
    osc_position: Vec3,
    pub(super) trails: Vec<super::history::History>,
    path: Option<positions::Path>,
}
impl Particle {
    pub fn random_frame(&self, count: usize) -> usize {
        (self.random.x * count as f32) as usize
    }
}
#[derive(Clone)]
pub struct System {
    emitters: Vec<Emitter>,
    initializers: Vec<Init>,
    operators: Vec<Op>,
    osc_phase: Option<[f32; 2]>,
    control: [ControlPoint; 8],
    base_control: [ControlPoint; 8],
    point_driven: [bool; 8],
    point_received: [bool; 8],
    point_velocity: [Vec3; 8],
    previous_points: [Vec3; 8],
    point_sample_time: Option<f64>,
    pub(super) motion: bool,
    motion_space: Mat4,
    space_velocity: Mat4,
    pub points: [Vec3; 8],
    pub(super) world_space: bool,
    flags: u32,
    pub(super) space: Mat4,
    pub(super) space_inverse: Mat4,
    pub particles: Vec<Particle>,
    pub max: usize,
    overrides: Overrides,
    rng: ChaCha8Rng,
    seed: u64,
    serial: u64,
    frame_time: Option<f64>,
    pub tick: u64,
    started: bool,
    pub audio: bool,
    pub(super) emission_audio: bool,
    initializer_audio: bool,
    pub(super) simulation_audio: bool,
    pub pointer: bool,
    override_value: Value,
    pub(super) start: f32,
    pub(super) elapsed: f64,
    pub(super) emitting: bool,
    pub(super) forced: usize,
    pub(super) events: Events,
    pub(super) event_mask: u8,
    pub(super) inherit_values: bool,
    event_source: Option<Values>,
    pub(super) histories: Vec<super::history::Config>,
    pub(super) bounds: Option<glam::Vec2>,
    pub(super) models: Models,
    pub(super) images: super::emission::Sources,
}
impl System {
    pub(super) fn image_periodic(&self, index: usize) -> bool {
        self.emitters.iter().any(|e| {
            e.image
                .as_ref()
                .is_some_and(|image| image.index == index && image.periodic)
        })
    }
    pub(super) fn inherit_source(&mut self, parent: &System, values: Values) {
        if self.inherit_values {
            self.event_source = Some(if parent.world_space {
                values
            } else {
                values.transformed(parent.space)
            });
        }
    }
    pub(super) fn set_space(&mut self, space: Mat4) {
        if self.space == space {
            return;
        }
        let previous = self.space;
        self.space = space;
        let determinant = space.determinant();
        self.space_inverse = if determinant.is_finite() && determinant.abs() > 1e-12 {
            let inverse = space.inverse();
            if inverse.is_finite() {
                inverse
            } else {
                Mat4::IDENTITY
            }
        } else {
            Mat4::IDENTITY
        };
        for (index, point) in self.control.iter().enumerate() {
            if self.point_received[index] && point.flags & 8 == 0 {
                self.points[index] = self
                    .space_inverse
                    .transform_point3(previous.transform_point3(self.points[index]));
            }
        }
    }
    pub(super) fn local_position(&self, position: Vec3) -> Vec3 {
        if self.world_space {
            self.space_inverse.transform_point3(position)
        } else {
            position
        }
    }
    pub fn initialized(&self) -> bool {
        self.started
    }
    pub(super) fn color_multiplier(&self) -> Vec4 {
        // WE also multiplies the simulated RGB by the instance color at draw
        // time. Alpha already participates in initialization and fading.
        self.overrides.color.unwrap_or(Vec3::ONE).extend(1.)
    }
    pub fn set_overrides(&mut self, v: &Value) -> Result<()> {
        if self.override_value == *v {
            return Ok(());
        }
        let ov = Overrides::parse(v)?.enabled(self.flags);
        let mut control = self.base_control;
        for (i, point) in control.iter_mut().enumerate() {
            if !v[format!("controlpoint{i}")].is_null() {
                point.offset = vector(v, &format!("controlpoint{i}"), point.offset)?;
            }
        }
        self.control = control;
        if self.overrides.color != ov.color {
            for p in &mut self.particles {
                p.color = ov
                    .color
                    .unwrap_or(p.base_color.truncate())
                    .extend(p.color.w);
            }
        }
        self.overrides = ov;
        self.override_value = v.clone();
        Ok(())
    }
    #[cfg(test)]
    pub fn advance(
        &mut self,
        time: f64,
        pointer: Vec3,
        world_to_local: glam::Mat4,
        audio: &AudioSnapshot,
    ) -> bool {
        self.update_points(pointer, world_to_local, Vec3::ZERO);
        let target = ((time.max(0.0) + self.start as f64) / STEP).floor() as u64;
        if target < self.tick {
            self.reset();
        }
        self.begin_frame(time);
        if !self.started {
            self.step(0.0, audio, 0.);
            self.started = true;
        }
        // A late join catches up over bounded batches without dropping timeline state.
        let count = target.saturating_sub(self.tick).min(240);
        for _ in 0..count {
            self.step(STEP as f32 * self.overrides.rate, audio, 0.);
            self.tick += 1;
        }
        self.tick == target
    }
    pub(super) fn fresh(&self, seed: u64) -> Self {
        let mut value = self.clone();
        value.seed = seed;
        value.reset();
        value
    }
    pub(super) fn needs_audio(&self) -> bool {
        let source = |e: &Emitter| {
            (e.audio.mode != 0 || self.initializer_audio) && self.emission_source_ready(e)
        };
        self.audio
            && (self.simulation_audio && !self.particles.is_empty()
                || self.max > 0
                    && self.emitting
                    && self
                        .emitters
                        .iter()
                        .any(|e| self.emitter_active(e) && source(e))
                || self.forced > 0 && self.emitters.first().is_some_and(source))
    }
    fn emitter_active(&self, emitter: &Emitter) -> bool {
        (emitter.duration <= 0. || self.elapsed < (emitter.delay + emitter.duration) as f64)
            && (!emitter.bursted && emitter.burst > 0 || emitter.rate > 0.)
    }
    fn emission_source_ready(&self, emitter: &Emitter) -> bool {
        emitter.image.as_ref().is_none_or(|image| {
            self.images
                .get(&image.index)
                .is_some_and(|source| source.bitmap.opaque())
        })
    }
    pub(super) fn image_demand(&self) -> bool {
        self.max > 0
            && self.emitters.iter().any(|e| {
                e.image.is_some()
                    && (self.forced > 0
                        || self.emitting
                            && (e.duration <= 0. || self.elapsed < (e.delay + e.duration) as f64)
                            && (!e.bursted && e.burst > 0 || e.rate > 0.))
            })
    }
    pub(super) fn has_emission(&self) -> bool {
        self.emitting && self.max > 0 && self.emitters.iter().any(|e| self.emitter_active(e))
    }
    pub(super) fn restart_emission(&mut self) {
        self.emitting = true;
        self.elapsed = 0.;
        for e in &mut self.emitters {
            e.bursted = false;
            e.frame_emitted = false;
            e.fractional = 0.;
            e.periodic_left = 0.;
            e.periodic_on = false;
        }
        for initializer in &mut self.initializers {
            initializer.reset_sequence(true);
        }
    }
    #[cfg(test)]
    pub(super) fn step_tick(&mut self, dt: f32, audio: &AudioSnapshot) {
        self.step_tick_to(dt, audio, 0);
    }
    pub(super) fn step_tick_to(&mut self, dt: f32, audio: &AudioSnapshot, remaining: u64) {
        let scaled_dt = dt * self.overrides.rate;
        self.step(scaled_dt, audio, remaining as f64 * scaled_dt as f64);
        self.started = true;
        self.tick += u64::from(dt > 0.0);
    }
    pub(super) fn begin_frame(&mut self, time: f64) {
        if self.frame_time != Some(time) {
            self.frame_time = Some(time);
            for emitter in &mut self.emitters {
                emitter.frame_emitted = false;
            }
        }
    }
    fn reset(&mut self) {
        self.rng = ChaCha8Rng::seed_from_u64(self.seed);
        self.serial = 0;
        self.frame_time = None;
        self.tick = 0;
        self.started = false;
        self.elapsed = 0.0;
        self.emitting = true;
        self.forced = 0;
        self.events = Events::default();
        self.event_source = None;
        self.point_received.fill(false);
        self.point_velocity.fill(Vec3::ZERO);
        self.point_sample_time = None;
        self.space_velocity = Mat4::ZERO;
        self.particles.clear();
        for initializer in &mut self.initializers {
            initializer.reset_sequence(false);
        }
        for e in &mut self.emitters {
            e.bursted = false;
            e.frame_emitted = false;
            e.fractional = 0.0;
            e.periodic_left = 0.0;
            e.periodic_on = false;
        }
    }
    fn step(&mut self, dt: f32, audio: &AudioSnapshot, remaining: f64) {
        let points = if self.world_space {
            self.points.map(|p| self.space.transform_point3(p))
        } else {
            self.points
        };
        let world_to_sim = if self.world_space {
            Mat4::IDENTITY
        } else {
            self.space_inverse
        };
        self.events.born.clear();
        self.events.died.clear();
        if self.event_mask & 2 != 0 {
            self.events.died.extend(
                self.particles
                    .iter()
                    .filter(|p| p.age_exact + dt as f64 >= p.life as f64)
                    .map(Event::from),
            );
        }
        self.particles
            .retain(|p| p.age_exact + (dt as f64) < p.life as f64);
        let sim_time = self.elapsed as f32;
        let event_source = self.event_source.map(|values| {
            if self.world_space {
                values
            } else {
                values.transformed(self.space_inverse)
            }
        });
        let first_born = self.serial;
        let forced = std::mem::take(&mut self.forced).min(self.max - self.particles.len());
        if !self.emitters.is_empty() {
            for _ in 0..forced {
                if let Some(particle) = self.spawn(0, audio) {
                    self.particles.push(particle);
                }
            }
        }
        for index in 0..if self.emitting {
            self.emitters.len()
        } else {
            0
        } {
            let e = &mut self.emitters[index];
            if sim_time < e.delay || (e.duration > 0.0 && sim_time >= e.delay + e.duration) {
                continue;
            }
            if e.flags & 4 != 0 {
                e.periodic_left -= dt;
                if e.periodic_left <= 0.0 {
                    e.periodic_on = !e.periodic_on;
                    let range = if e.periodic_on {
                        e.periodic_duration
                    } else {
                        e.periodic_delay
                    };
                    e.periodic_left =
                        lerp(range[0], range[1], random(&mut self.rng)).max(STEP as f32);
                    if e.periodic_on {
                        for initializer in &mut self.initializers {
                            initializer.reset_sequence(true);
                        }
                    }
                }
                if !e.periodic_on {
                    continue;
                }
            }
            let response = e.audio.response(audio);
            let mut count = 0;
            if !e.bursted {
                count = (e.burst as f32 * response).round() as usize;
                e.bursted = true;
            }
            e.fractional +=
                dt as f64 * e.rate as f64 * self.overrides.count as f64 * response as f64;
            let n = e.fractional.floor() as usize;
            e.fractional -= n as f64;
            count += n;
            if e.flags & 2 != 0 {
                count = count.min(usize::from(!e.frame_emitted));
                e.frame_emitted |= count > 0;
            }
            for _ in 0..count.min(self.max - self.particles.len()) {
                if let Some(p) = self.spawn(index, audio) {
                    self.particles.push(p);
                }
            }
        }
        let collisions = collision::Context {
            points: self.points,
            to_world: self.space,
            to_local: self.space_inverse,
            world_particles: self.world_space,
            bounds: self.bounds,
            models: &self.models,
        };
        // These inputs are shared by every particle in this tick. Preserve the
        // original multiplication order rather than combining dt and speed.
        for operator in &mut self.operators {
            match operator {
                Op::Movement {
                    gravity,
                    drag,
                    world,
                    impulse,
                    damping,
                } => {
                    let gravity = if *world {
                        world_to_sim.transform_vector3(*gravity)
                    } else {
                        *gravity
                    };
                    *impulse = gravity * dt * self.overrides.speed;
                    *damping = (1.0 - *drag * dt).max(0.0);
                }
                Op::Angular {
                    force,
                    drag,
                    impulse,
                    damping,
                } => {
                    *impulse = *force * dt * self.overrides.speed;
                    *damping = (1.0 - *drag * dt).max(0.0);
                }
                Op::Boids(boids) => boids.prepare(&self.particles),
                _ => {}
            }
        }
        for (index, p) in self.particles.iter_mut().enumerate() {
            let previous = p.position;
            p.age_exact += dt as f64;
            p.age = p.age_exact as f32;
            p.color = p.base_color;
            // Instance RGB replaces initializer output; retain the authored base
            // so removing a live override restores it without respawning particles.
            if let Some(color) = self.overrides.color {
                p.color = color.extend(p.color.w);
            }
            p.size = p.base_size;
            let life = (p.age / p.life).clamp(0.0, 1.0);
            let mut osc_position = Vec3::ZERO;
            for o in &self.operators {
                match o {
                    Op::Movement {
                        impulse, damping, ..
                    } => {
                        p.position += p.velocity * dt;
                        p.velocity += *impulse;
                        p.velocity *= *damping;
                    }
                    Op::Angular {
                        impulse, damping, ..
                    } => {
                        p.rotation += p.angular * dt * self.overrides.speed;
                        p.angular += *impulse;
                        p.angular *= *damping;
                        if !(p.rotation.cmpge(Vec3::ZERO) & p.rotation.cmplt(Vec3::splat(TAU)))
                            .all()
                        {
                            p.rotation = p.rotation.rem_euclid(Vec3::splat(TAU));
                        }
                    }
                    Op::Fade { inside, outside } => {
                        // Native operators give fade-in priority when intervals overlap.
                        if life <= *inside {
                            p.color.w *= lifetime_progress(life, 0.0, *inside);
                        } else if life > *outside {
                            p.color.w *= 1.0 - lifetime_progress(life, *outside, 1.0);
                        }
                    }
                    Op::Change {
                        field,
                        time,
                        start,
                        end,
                    } => {
                        let factor = start.lerp(*end, lifetime_progress(life, time.x, time.y));
                        match field {
                            0 => p.size *= factor.x,
                            1 => p.color.w *= factor.x,
                            _ => p.color = (p.color.truncate() * factor).extend(p.color.w),
                        }
                    }
                    Op::Turbulence(n) => {
                        p.velocity += n.value(p, sim_time, audio) * dt * self.overrides.speed
                    }
                    Op::Inherit(field) => field.apply(p, event_source),
                    Op::Remap(remap) => remap.apply(p, sim_time, &points, audio, false),
                    Op::Constraint(constraint) => constraint.apply(p, &points, dt),
                    Op::Boids(boids) => boids.apply(p, index, dt * self.overrides.speed),
                    Op::Collision(collision) => {
                        if collision.apply(p, previous, &collisions) {
                            if self.event_mask & 2 != 0 {
                                self.events.died.push(Event::from(&*p));
                            }
                            p.life = 0.;
                            break;
                        }
                    }
                    Op::OscAlpha(o) => p.color.w *= o.value(p, 0),
                    Op::OscSize(o) => p.size *= o.value(p, 0),
                    Op::OscPosition(o) => {
                        osc_position +=
                            Vec3::from_array(std::array::from_fn(|i| o.position(p, i))) * o.mask
                    }
                    Op::Attract {
                        cp,
                        origin,
                        scale,
                        threshold,
                    } => {
                        let delta = points[*cp] + *origin - p.position;
                        let dist = delta.length();
                        if dist > 0.0 && dist < *threshold {
                            let falloff = 1.0 - dist / *threshold;
                            p.velocity +=
                                delta / dist * *scale * falloff * dt * self.overrides.speed;
                        }
                    }
                    Op::Vortex(vortex) => {
                        p.velocity += vortex.value(p, &points, audio) * dt * self.overrides.speed
                    }
                    Op::Cap { speed, time } => {
                        let n = p.velocity.length();
                        if n > *speed && n > 0.0 {
                            p.velocity = p
                                .velocity
                                .lerp(p.velocity * (*speed / n), ramp(life, time.x, time.y));
                        }
                    }
                    Op::Reduce {
                        cp,
                        distance,
                        inner,
                    } => {
                        let r = (p.position - points[*cp]).length();
                        if r < distance.y {
                            p.velocity *=
                                lerp(*inner, 1.0, ramp(r, distance.x, distance.y)).max(0.0);
                        }
                    }
                }
            }
            p.position += osc_position - p.osc_position;
            p.osc_position = osc_position;
        }
        // Bad authored forces cannot poison GLES or grow immortal particle state.
        self.particles.retain(|p| {
            p.position.is_finite()
                && p.velocity.is_finite()
                && p.rotation.is_finite()
                && p.color.is_finite()
                && p.size.is_finite()
                && p.life.is_finite()
                && p.life > 0.0
        });
        if self.event_mask & 1 != 0 {
            self.events.born.extend(
                self.particles
                    .iter()
                    .filter(|p| p.id >= first_born)
                    .map(Event::from),
            );
        }
        for particle in &mut self.particles {
            for history in &mut particle.trails {
                history.record_before_frame(particle.age_exact, particle.position, remaining);
            }
        }
        self.elapsed += dt as f64;
        if self.emitters.iter().all(|e| {
            (e.duration > 0.0 && self.elapsed >= (e.delay + e.duration) as f64)
                || (e.rate == 0.0 && e.bursted)
        }) {
            self.emitting = false;
        }
    }
    fn spawn(&mut self, index: usize, audio: &AudioSnapshot) -> Option<Particle> {
        let e = &self.emitters[index];
        let rng = &mut self.rng;
        let source = if let Some(image) = &e.image {
            Some(self.images.get(&image.index)?.sample(rng)?)
        } else {
            None
        };
        let direction = if e.sphere {
            let z = lerp(-1.0, 1.0, random(rng));
            let angle = random(rng) * TAU;
            if e.directions.z == 0. {
                Vec3::new(angle.cos(), angle.sin(), 0.)
            } else {
                let radius = (1.0 - z * z).sqrt();
                Vec3::new(radius * angle.cos(), radius * angle.sin(), z)
            }
        } else {
            Vec3::ZERO
        };
        let mut displacement = if e.sphere {
            let min = e.min.x.max(0.);
            let max = e.max.x.max(min);
            let radius = if e.directions.z == 0. {
                lerp(min * min, max * max, random(rng)).sqrt()
            } else {
                lerp(min.powi(3), max.powi(3), random(rng)).cbrt()
            };
            direction * radius
        } else {
            Vec3::from_array(std::array::from_fn(|axis| {
                let d = lerp(e.min[axis], e.max[axis], random(rng));
                if random(rng) < 0.5 { -d } else { d }
            }))
        };
        displacement *= e.directions;
        let mut fallback_direction = direction * e.directions;
        for axis in 0..3 {
            if e.sign[axis] != 0.0 {
                displacement[axis] = displacement[axis].abs() * e.sign[axis].signum();
                fallback_direction[axis] = fallback_direction[axis].abs() * e.sign[axis].signum();
            }
        }
        let mut p = Particle {
            id: self.serial,
            position: self.points[e.cp] + e.origin + displacement,
            velocity: displacement.normalize_or(fallback_direction.normalize_or_zero())
                * lerp(e.speed[0], e.speed[1], random(rng))
                * self.overrides.speed,
            rotation: Vec3::ZERO,
            angular: Vec3::ZERO,
            color: Vec3::ONE.extend(self.overrides.alpha),
            size: self.overrides.size,
            age: 0.0,
            age_exact: 0.0,
            life: self.overrides.life,
            base_color: Vec4::ONE,
            base_size: 1.0,
            random: Vec3::new(random(rng), random(rng), random(rng)),
            osc_phase_cos: Vec3::ZERO,
            osc_position: Vec3::ZERO,
            trails: Vec::new(),
            path: None,
        };
        if let Some(phase) = self.osc_phase {
            p.osc_phase_cos = Vec3::from_array(std::array::from_fn(|i| {
                lerp(phase[0], phase[1], p.random[i]).cos()
            }));
        }
        self.serial = self.serial.wrapping_add(1);
        if let (Some(image), Some((position, velocity, _))) = (&e.image, source) {
            p.position = self.space_inverse.transform_point3(position)
                + self.points[e.cp]
                + e.origin
                + image.offset.sample(rng);
            if image.motion {
                p.velocity = self.space_inverse.transform_vector3(velocity)
                    * lerp(e.speed[0], e.speed[1], random(rng))
                    * self.overrides.speed;
            }
        }
        let event_source = self
            .event_source
            .map(|values| values.transformed(self.space_inverse));
        for i in &mut self.initializers {
            match i {
                Init::Life(r) => p.life = r.sample(rng).x.max(STEP as f32) * self.overrides.life,
                // Size initializers multiply in authored order, starting from size 1.
                Init::Size(r) => p.size *= r.sample(rng).x.max(0.0),
                Init::Color(r) => {
                    // RGB endpoints describe one gradient, not three independent ranges.
                    p.color = (r.min.lerp(r.max, random(rng)) / 255.0).extend(p.color.w);
                }
                Init::Palette(c) => p.color = c.sample(rng).extend(p.color.w),
                Init::PositionOffset(offset) => {
                    p.position += offset.value(&p, self.elapsed as f32);
                }
                Init::Inherit(field) => field.apply(&mut p, event_source),
                Init::Remap(remap) => {
                    remap.apply(&mut p, self.elapsed as f32, &self.points, audio, true)
                }
                Init::Alpha(r) => p.color.w = r.sample(rng).x * self.overrides.alpha,
                Init::Velocity(r) => p.velocity += r.sample(rng) * self.overrides.speed,
                Init::Rotation(r) => p.rotation = r.sample(rng),
                Init::Angular(r) => p.angular = r.sample(rng),
                Init::Turbulent(n) => {
                    p.velocity += n.value(&p, self.elapsed as f32, audio) * self.overrides.speed
                }
                Init::InheritVelocity { cp, weight } => {
                    p.velocity += self
                        .space_inverse
                        .transform_vector3(self.point_velocity[*cp])
                        * weight.sample(rng).x
                        * self.overrides.speed;
                }
                Init::Around(around) => around.apply(&mut p, &self.points, &self.overrides, rng),
                Init::Between(between) => between.apply(&mut p, &self.points, &self.overrides),
            }
        }
        if e.image.as_ref().is_some_and(|image| image.copy)
            && let Some((_, _, color)) = source
        {
            p.color = (p.color.truncate() * color.truncate()).extend(p.color.w);
        }
        p.base_color = p.color;
        p.life = p.life.clamp(STEP as f32, 3600.0);
        p.base_size = p.size;
        if let Some(path) = &mut p.path {
            path.offset =
                p.position - self.points[path.start].lerp(self.points[path.end], path.fraction);
        }
        if self.world_space {
            // Convert once at birth. Existing positions, velocities and rope
            // histories remain in the common simulation space as the layer moves.
            p.position = self.space.transform_point3(p.position);
            p.velocity = self.space.transform_vector3(p.velocity);
            if let Some(path) = &mut p.path {
                path.offset = self.space.transform_vector3(path.offset);
                path.axis = self
                    .space
                    .transform_vector3(path.axis)
                    .normalize_or(Vec3::X);
            }
        }
        p.trails = self
            .histories
            .iter()
            .map(|c| super::history::History::new(*c, p.position))
            .collect();
        Some(p)
    }

    pub fn new(v: &Value, overrides: &Value, seed: u64) -> Result<Self> {
        let flags = super::animation::flags(v)?;
        let ov = Overrides::parse(overrides)?.enabled(flags);
        let max = scalar(v, "maxcount", 100.0)?.clamp(0.0, MAX_PARTICLES as f32) as usize;
        let mut emitters = Vec::new();
        let mut images = 0;
        for e in entries(v, "emitter", 32)? {
            let name = e["name"].as_str().context("particle emitter name")?;
            ensure!(
                ["boxrandom", "sphererandom", "layerimage"].contains(&name),
                "unsupported particle emitter {name}"
            );
            let rate = scalar(e, "rate", 5.0)?;
            ensure!(rate >= 0.0, "negative particle rate");
            emitters.push(Emitter {
                sphere: name == "sphererandom",
                image: if name == "layerimage" {
                    let flags = super::animation::flags(e)?;
                    let image = ImageEmitter {
                        index: images,
                        copy: e["copylayercolor"].as_bool().unwrap_or(flags & 1 != 0),
                        periodic: e["updateemissionbitmap"]
                            .as_bool()
                            .unwrap_or(flags & 8 != 0),
                        motion: e["inheritlayermotion"].as_bool().unwrap_or(flags & 16 != 0),
                        offset: Range {
                            min: vector(e, "offsetmin", Vec3::ZERO)?,
                            max: vector(e, "offsetmax", Vec3::ZERO)?,
                            exponent: Vec3::ONE,
                        },
                    };
                    images += 1;
                    Some(image)
                } else {
                    None
                },
                origin: vector(e, "origin", Vec3::ZERO)?,
                directions: vector(e, "directions", Vec3::new(1., 1., 0.))?,
                min: vector(e, "distancemin", Vec3::ZERO)?,
                max: vector(e, "distancemax", Vec3::ZERO)?,
                sign: vector(e, "sign", Vec3::ZERO)?,
                speed: [scalar(e, "speedmin", 0.0)?, scalar(e, "speedmax", 0.0)?],
                rate,
                burst: scalar(e, "instantaneous", 0.0)?.clamp(0.0, MAX_PARTICLES as f32) as usize,
                cp: cp(e)?,
                flags: super::animation::flags(e)?,
                delay: scalar(e, "delay", 0.0)?.max(0.0),
                duration: scalar(e, "duration", 0.0)?.max(0.0),
                periodic_duration: [
                    scalar(e, "minperiodicduration", 1.0)?,
                    scalar(e, "maxperiodicduration", 1.0)?,
                ],
                periodic_delay: [
                    scalar(e, "minperiodicdelay", 1.0)?,
                    scalar(e, "maxperiodicdelay", 1.0)?,
                ],
                periodic_left: 0.0,
                periodic_on: false,
                fractional: 0.0,
                bursted: false,
                frame_emitted: false,
                audio: Audio::parse(e, true)?,
            });
        }
        let mut initializers = Vec::new();
        for i in entries(v, "initializer", 64)? {
            let name = i["name"].as_str().context("particle initializer name")?;
            initializers.push(match name {
                "lifetimerandom" => Init::Life(Range::scalar(i, 1.0, 1.0)?),
                "sizerandom" => Init::Size(Range::scalar(i, 20.0, 20.0)?),
                "alpharandom" => Init::Alpha(Range::scalar(i, 1.0, 1.0)?),
                "colorrandom" => Init::Color(Range::vector(i, Vec3::ZERO, Vec3::splat(255.0))?),
                "velocityrandom" => Init::Velocity(Range::vector(
                    i,
                    Vec3::new(-32.0, -32.0, 0.0),
                    Vec3::new(32.0, 32.0, 0.0),
                )?),
                "rotationrandom" => {
                    Init::Rotation(Range::vector(i, Vec3::ZERO, Vec3::new(0.0, 0.0, TAU))?)
                }
                "angularvelocityrandom" => Init::Angular(Range::vector(
                    i,
                    Vec3::new(0.0, 0.0, -5.0),
                    Vec3::new(0.0, 0.0, 5.0),
                )?),
                "turbulentvelocityrandom" => Init::Turbulent(Velocity::parse(i)?),
                "hsvcolorrandom" => Init::Palette(colors::Random::hsv(i)?),
                "colorlist" => Init::Palette(colors::Random::list(i)?),
                "inheritinitialvaluefromevent" => Init::Inherit(event_values::Field::parse(i)?),
                "remapinitialvalue" => Init::Remap(remap::Remap::parse(i)?),
                "positionoffsetrandom" => Init::PositionOffset(position_offset::Offset::parse(i)?),
                "inheritcontrolpointvelocity" => Init::InheritVelocity {
                    cp: cp(i)?,
                    weight: Range::scalar(i, 1., 1.)?,
                },
                "mapsequencearoundcontrolpoint" => Init::Around(positions::Around::parse(i)?),
                "mapsequencebetweencontrolpoints" => Init::Between(positions::Between::parse(i)?),
                _ => anyhow::bail!("unsupported particle initializer {name}"),
            });
        }
        let mut operators = Vec::new();
        let mut model_index = 0;
        for o in entries(v, "operator", 64)? {
            let name = o["name"].as_str().context("particle operator name")?;
            operators.push(match name {
                "movement" => Op::Movement {
                    gravity: vector(o, "gravity", Vec3::ZERO)?,
                    drag: scalar(o, "drag", 0.0)?,
                    world: super::animation::flags(o)? & 1 != 0 || o["worldspace"] == true,
                    impulse: Vec3::ZERO,
                    damping: 1.,
                },
                "angularmovement" => Op::Angular {
                    force: vector(o, "force", Vec3::ZERO)?,
                    drag: scalar(o, "drag", 0.0)?,
                    impulse: Vec3::ZERO,
                    damping: 1.,
                },
                "alphafade" => Op::Fade {
                    inside: scalar(o, "fadeintime", 0.5)?,
                    outside: scalar(o, "fadeouttime", 0.5)?,
                },
                "sizechange" | "alphachange" | "colorchange" => Op::Change {
                    field: match name {
                        "sizechange" => 0,
                        "alphachange" => 1,
                        _ => 2,
                    },
                    time: Vec2::new(scalar(o, "starttime", 0.0)?, scalar(o, "endtime", 1.0)?),
                    start: vector(
                        o,
                        "startvalue",
                        if name == "colorchange" {
                            Vec3::ZERO
                        } else {
                            Vec3::ONE
                        },
                    )?,
                    end: vector(o, "endvalue", Vec3::ZERO)?,
                },
                "turbulence" => Op::Turbulence(Force::parse(o)?),
                "boids" => Op::Boids(boids::Boids::parse(o)?),
                "collisionmodel" => {
                    let result = Op::Collision(collision::Collision::model(o, model_index)?);
                    model_index += 1;
                    result
                }
                "inheritvaluefromevent" => Op::Inherit(event_values::Field::parse(o)?),
                "remapvalue" => Op::Remap(remap::Remap::parse(o)?),
                "maintaindistancetocontrolpoint" => {
                    Op::Constraint(positions::Constraint::radius(o)?)
                }
                "maintaindistancebetweencontrolpoints" => {
                    Op::Constraint(positions::Constraint::between(o)?)
                }
                "collisionplane" | "collisionquad" | "collisionsphere" | "collisionbounds" => {
                    Op::Collision(collision::Collision::parse(o)?)
                }
                "oscillatealpha" => Op::OscAlpha(Osc::parse(o, false)?),
                "oscillatesize" => Op::OscSize(Osc::parse(o, true)?),
                "oscillateposition" => Op::OscPosition(Osc::parse(o, false)?),
                "controlpointattract" => Op::Attract {
                    cp: cp(o)?,
                    origin: vector(o, "origin", Vec3::ZERO)?,
                    scale: scalar(o, "scale", 0.0)?,
                    threshold: scalar(o, "threshold", 0.0)?,
                },
                "vortex" | "vortex_v2" => Op::Vortex(vortex::Vortex::parse(o)?),
                "capvelocity" => Op::Cap {
                    speed: scalar(o, "maxspeed", 0.0)?.max(0.0),
                    time: Vec2::new(
                        scalar(o, "blendinstart", 0.0)?,
                        scalar(o, "blendinend", 1.0)?,
                    ),
                },
                "reducemovementnearcontrolpoint" => Op::Reduce {
                    cp: cp(o)?,
                    distance: Vec2::new(
                        scalar(o, "distanceinner", 0.0)?,
                        scalar(o, "distanceouter", 0.0)?,
                    ),
                    inner: scalar(o, "reductioninner", 0.0)? / 1000.0,
                },
                _ => anyhow::bail!("unsupported particle operator {name}"),
            });
        }
        // A particle's random phase never changes. Cache one shared phase range
        // inline, with no per-particle allocation; other authored ranges retain
        // the original calculation.
        let osc_phase = operators.iter().find_map(|op| match op {
            Op::OscPosition(o) => Some(o.phase),
            _ => None,
        });
        for op in &mut operators {
            if let Op::OscPosition(o) = op {
                o.cached_phase = Some(o.phase) == osc_phase;
            }
        }
        let control = parse_control_points(v)?;
        let pointer = control.iter().any(|point| point.flags & 1 != 0);
        let initializer_audio = initializers.iter().any(|i| {
            matches!(i,Init::Turbulent(n) if n.needs_audio())
                || matches!(i,Init::Remap(r) if r.needs_audio())
        });
        let emission_audio = emitters.iter().any(|e| e.audio.mode != 0) || initializer_audio;
        let simulation_audio = operators.iter().any(|o| {
            matches!(o,Op::Turbulence(n) if n.needs_audio())
                | matches!(o,Op::Vortex(v) if v.needs_audio())
                | matches!(o,Op::Remap(r) if r.needs_audio())
        });
        let start = scalar(v, "starttime", 0.0)?.max(0.0);
        ensure!(start <= 3600.0, "particle prewarm exceeds one hour");
        let motion = initializers
            .iter()
            .any(|i| matches!(i, Init::InheritVelocity { .. }));
        let inherit_values = initializers.iter().any(|i| matches!(i, Init::Inherit(_)))
            || operators.iter().any(|o| matches!(o, Op::Inherit(_)));
        let mut system = Self {
            emitters,
            initializers,
            operators,
            osc_phase,
            control,
            base_control: control,
            point_driven: [false; 8],
            point_received: [false; 8],
            point_velocity: [Vec3::ZERO; 8],
            previous_points: [Vec3::ZERO; 8],
            point_sample_time: None,
            motion,
            motion_space: glam::Mat4::IDENTITY,
            space_velocity: glam::Mat4::ZERO,
            points: [Vec3::ZERO; 8],
            world_space: flags & 1 != 0,
            flags,
            space: glam::Mat4::IDENTITY,
            space_inverse: glam::Mat4::IDENTITY,
            particles: Vec::with_capacity(max),
            max,
            overrides: ov,
            rng: ChaCha8Rng::seed_from_u64(seed),
            seed,
            serial: 0,
            frame_time: None,
            tick: 0,
            started: false,
            audio: emission_audio || simulation_audio,
            emission_audio,
            initializer_audio,
            simulation_audio,
            pointer,
            override_value: Value::Null,
            start,
            elapsed: 0.0,
            emitting: true,
            forced: 0,
            events: Events::default(),
            event_mask: 3,
            inherit_values,
            event_source: None,
            bounds: None,
            models: Default::default(),
            images: Default::default(),
            histories: entries(v, "renderer", 16)?
                .iter()
                .filter(|r| r["name"] == "ropetrail")
                .map(super::history::Config::parse)
                .collect::<Result<Vec<_>>>()?,
        };
        system.set_overrides(overrides)?;
        Ok(system)
    }

    pub(in crate::particles) fn drive_points(&mut self, start: Option<usize>) {
        if let Some(start) = start {
            self.point_driven[start..].fill(true);
        }
    }
    pub(in crate::particles) fn update_points(
        &mut self,
        pointer: Vec3,
        world_to_local: Mat4,
        origin: Vec3,
    ) {
        for (i, point) in self.control.iter().enumerate() {
            // If a parent has expired, retain its last copied values until the
            // child finishes. An invalid parent never resets a live force target.
            if self.point_received[i] {
                continue;
            }
            self.points[i] = if point.flags & 1 != 0 {
                pointer + point.offset
            } else if point.flags & 2 != 0 {
                world_to_local.transform_point3(point.offset)
            } else {
                point.offset + origin
            };
        }
    }
    pub(in crate::particles) fn inherit_points(&mut self, parent: &Self, start: Option<usize>) {
        let transform = self.space_inverse * parent.space;
        for (i, point) in self.control.iter().enumerate() {
            if point.flags & 4 == 0 {
                continue;
            }
            let position = parent.points[point.parent];
            self.points[i] = if point.flags & 8 != 0 {
                position
            } else {
                transform.transform_point3(position)
            } + point.offset;
            self.point_received[i] = true;
        }
        if let Some(start) = start {
            for (index, particle) in (start..8).zip(&parent.particles) {
                let world = if parent.world_space {
                    particle.position
                } else {
                    parent.space.transform_point3(particle.position)
                };
                self.points[index] = self.space_inverse.transform_point3(world);
                self.point_received[index] = true;
            }
        }
    }
    pub(in crate::particles) fn inherit_point_motion(
        &mut self,
        parent: &Self,
        start: Option<usize>,
    ) {
        if !self.motion {
            return;
        }
        for (index, point) in self.control.iter().enumerate() {
            if point.flags & 4 == 0 {
                continue;
            }
            let velocity = parent.point_velocity[point.parent];
            self.point_velocity[index] = if point.flags & 8 != 0 {
                let motion =
                    (parent.space_velocity * parent.points[point.parent].extend(1.)).truncate();
                self.space
                    .transform_vector3(parent.space_inverse.transform_vector3(velocity - motion))
                    + (self.space_velocity * self.points[index].extend(1.)).truncate()
            } else {
                velocity
            };
        }
        if let Some(start) = start {
            for (index, particle) in (start..8).zip(&parent.particles) {
                self.point_velocity[index] = parent.particle_velocity(particle);
            }
        }
    }
    pub(in crate::particles) fn seed_motion(&mut self, parent: &Self, matrix: Mat4) {
        self.space_velocity = parent.space_velocity * matrix;
    }
    pub(in crate::particles) fn particle_velocity(&self, particle: &Particle) -> Vec3 {
        if self.world_space {
            particle.velocity
        } else {
            self.space.transform_vector3(particle.velocity)
                + (self.space_velocity * particle.position.extend(1.)).truncate()
        }
    }
    pub(in crate::particles) fn point_motion(&mut self, time: f64) {
        if !self.motion {
            return;
        }
        let positions = self.points.map(|p| self.space.transform_point3(p));
        if let Some(previous) = self.point_sample_time {
            let delta = (time - previous) as f32;
            if delta >= 1e-6 {
                self.space_velocity = (self.space - self.motion_space) * delta.recip();
                if !self.space_velocity.is_finite() {
                    self.space_velocity = Mat4::ZERO;
                }
                for (index, position) in positions.iter().enumerate() {
                    if !self.point_received[index] {
                        let velocity = (*position - self.previous_points[index]) / delta;
                        self.point_velocity[index] = if velocity.is_finite() {
                            velocity
                        } else {
                            Vec3::ZERO
                        };
                    }
                }
            }
        }
        self.previous_points = positions;
        self.point_sample_time = Some(time);
        self.motion_space = self.space;
    }
    pub(in crate::particles) fn follow_point_motion(&mut self, parent: &Self, particle: &Particle) {
        if !self.motion {
            return;
        }
        let velocity = parent.particle_velocity(particle);
        let parent_origin_velocity = (parent.space_velocity * Vec3::ZERO.extend(1.)).truncate();
        for (index, point) in self.control.iter().enumerate() {
            if point.flags & 7 == 0 && !self.point_driven[index] {
                self.point_velocity[index] = velocity
                    + (self.space_velocity * point.offset.extend(1.)).truncate()
                    - parent_origin_velocity;
            }
        }
    }
    pub(in crate::particles) fn freeze_point_sources(&mut self) {
        for (index, received) in self.point_received.iter().enumerate() {
            if *received {
                self.point_velocity[index] = Vec3::ZERO;
            }
        }
    }
}

fn scalar(v: &Value, key: &str, default: f32) -> Result<f32> {
    Ok(components(&v[key], &[default], 1)?[0])
}
fn vector(v: &Value, key: &str, default: Vec3) -> Result<Vec3> {
    let a = components(&v[key], &default.to_array(), 3)?;
    Ok(Vec3::new(a[0], a[1], a[2]))
}
fn cp(v: &Value) -> Result<usize> {
    let n = scalar(v, "controlpoint", 0.0)?;
    ensure!(
        (0.0..8.0).contains(&n) && n.fract() == 0.0,
        "invalid particle control point"
    );
    Ok(n as usize)
}
fn entries<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a [Value]> {
    let a = v[key].as_array().map_or(&[][..], Vec::as_slice);
    ensure!(a.len() <= max, "too many particle {key} entries");
    Ok(a)
}
fn random(rng: &mut ChaCha8Rng) -> f32 {
    (rng.next_u32() >> 8) as f32 / 16777216.0
}
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
fn lifetime_progress(life: f32, start: f32, end: f32) -> f32 {
    if life <= start {
        0.0
    } else if life > end {
        1.0
    } else {
        (life - start) / (end - start)
    }
}
fn ramp(x: f32, a: f32, b: f32) -> f32 {
    if a == b {
        if x >= b { 1.0 } else { 0.0 }
    } else {
        ((x - a) / (b - a)).clamp(0.0, 1.0)
    }
}
#[derive(Clone)]
struct Range {
    min: Vec3,
    max: Vec3,
    exponent: Vec3,
}
impl Range {
    fn scalar(v: &Value, min: f32, max: f32) -> Result<Self> {
        let exponent = scalar(v, "exponent", 1.0)?;
        ensure!(exponent > 0.0, "invalid random exponent");
        Ok(Self {
            min: Vec3::splat(scalar(v, "min", min)?),
            max: Vec3::splat(scalar(v, "max", max)?),
            exponent: Vec3::splat(exponent),
        })
    }
    fn vector(v: &Value, min: Vec3, max: Vec3) -> Result<Self> {
        // WE VecRandom scalars affect X only; ordinary emitter vectors broadcast.
        let parse = |key: &str, default: Vec3| -> Result<Vec3> {
            let a = numbers(&v[key], &default.to_array())?;
            match a.as_slice() {
                [x] => Ok(Vec3::new(*x, 0.0, 0.0)),
                [x, y, z] => Ok(Vec3::new(*x, *y, *z)),
                _ => anyhow::bail!("invalid particle vector range"),
            }
        };
        let exponent = if v["exponent"].is_null() {
            Vec3::ONE
        } else {
            let a = numbers(&v["exponent"], &[1.0; 3])?;
            match a.as_slice() {
                [x] => Vec3::splat(*x),
                [x, y, z] => Vec3::new(*x, *y, *z),
                _ => anyhow::bail!("invalid random exponent"),
            }
        };
        ensure!(exponent.min_element() > 0.0, "invalid random exponent");
        Ok(Self {
            min: parse("min", min)?,
            max: parse("max", max)?,
            exponent,
        })
    }
    fn sample(&self, rng: &mut ChaCha8Rng) -> Vec3 {
        Vec3::from_array(std::array::from_fn(|i| {
            lerp(self.min[i], self.max[i], random(rng).powf(self.exponent[i]))
        }))
    }
}
#[derive(Clone, Default)]
struct Audio {
    mode: u8,
    bounds: Vec2,
    exponent: f32,
    frequency: [usize; 2],
}
impl Audio {
    fn parse(v: &Value, emitter: bool) -> Result<Self> {
        let mode = scalar(v, "audioprocessingmode", 0.0)?;
        ensure!(
            (0.0..=3.0).contains(&mode) && mode.fract() == 0.0,
            "invalid particle audio channel"
        );
        let bounds = components(
            &v["audioprocessingbounds"],
            if emitter { &[0.8, 1.0] } else { &[0.0, 1.0] },
            2,
        )?;
        let exponent = scalar(
            v,
            "audioprocessingexponent",
            if emitter { 2.0 } else { 1.0 },
        )?;
        ensure!(exponent > 0.0, "invalid particle audio exponent");
        Ok(Self {
            mode: mode as u8,
            bounds: Vec2::new(bounds[0], bounds[1]),
            exponent,
            frequency: [
                scalar(v, "audioprocessingfrequencystart", 0.0)?
                    .round()
                    .clamp(0.0, 15.0) as usize,
                scalar(v, "audioprocessingfrequencyend", 1.0)?
                    .round()
                    .clamp(0.0, 15.0) as usize,
            ],
        })
    }
    fn response(&self, s: &AudioSnapshot) -> f32 {
        if self.mode == 0 {
            return 1.0;
        }
        let b = &s.bands[0];
        let data = match self.mode {
            1 => &b.left,
            2 => &b.right,
            _ => &b.average,
        };
        let [lo, hi] = self.frequency;
        let hi = hi.max(lo);
        let mean = data[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32;
        let k = ramp(
            mean,
            self.bounds.x,
            self.bounds.y.max(self.bounds.x + f32::EPSILON),
        );
        (k * k * (3.0 - 2.0 * k)).powf(self.exponent)
    }
}
#[derive(Clone)]
struct Emitter {
    sphere: bool,
    image: Option<ImageEmitter>,
    origin: Vec3,
    directions: Vec3,
    min: Vec3,
    max: Vec3,
    sign: Vec3,
    speed: [f32; 2],
    rate: f32,
    burst: usize,
    cp: usize,
    flags: u32,
    delay: f32,
    duration: f32,
    periodic_duration: [f32; 2],
    periodic_delay: [f32; 2],
    periodic_left: f32,
    periodic_on: bool,
    fractional: f64,
    bursted: bool,
    frame_emitted: bool,
    audio: Audio,
}
#[derive(Clone)]
struct ImageEmitter {
    pub index: usize,
    pub copy: bool,
    pub periodic: bool,
    pub motion: bool,
    pub offset: Range,
}
#[derive(Clone)]
enum Init {
    Life(Range),
    Size(Range),
    Color(Range),
    Palette(colors::Random),
    PositionOffset(position_offset::Offset),
    Inherit(event_values::Field),
    Remap(remap::Remap),
    Alpha(Range),
    Velocity(Range),
    Rotation(Range),
    Angular(Range),
    Turbulent(Velocity),
    InheritVelocity { cp: usize, weight: Range },
    Around(positions::Around),
    Between(positions::Between),
}
impl Init {
    fn reset_sequence(&mut self, periodic: bool) {
        match self {
            Self::Around(value) => value.reset(periodic),
            Self::Between(value) => value.reset(periodic),
            _ => {}
        }
    }
}
#[derive(Clone)]
struct Osc {
    frequency: [f32; 2],
    scale: [f32; 2],
    phase: [f32; 2],
    mask: Vec3,
    cached_phase: bool,
}
impl Osc {
    fn parse(v: &Value, size: bool) -> Result<Self> {
        let low = scalar(v, "frequencymin", 0.0)?;
        let high = scalar(v, "frequencymax", 10.0)?;
        Ok(Self {
            frequency: [low, if high == 0.0 { low } else { high }],
            scale: [
                scalar(v, "scalemin", if size { 0.8 } else { 0.0 })?,
                scalar(v, "scalemax", if size { 1.2 } else { 1.0 })?,
            ],
            phase: [scalar(v, "phasemin", 0.0)?, scalar(v, "phasemax", TAU)?],
            mask: vector(v, "mask", Vec3::new(1.0, 1.0, 0.0))?,
            cached_phase: false,
        })
    }
    fn position(&self, p: &Particle, axis: usize) -> f32 {
        let frequency = lerp(self.frequency[0], self.frequency[1], p.random[axis]);
        let phase = lerp(self.phase[0], self.phase[1], p.random[(axis + 1) % 3]);
        let amplitude = lerp(self.scale[0], self.scale[1], p.random[(axis + 2) % 3]);
        let angle = p.age * frequency + phase;
        // Omit trigonometry on masked axes only when its result is finite.
        // Overflow must still reach the particle's existing invalid-state guard.
        if self.mask[axis] == 0.0
            && angle.is_finite()
            && phase.is_finite()
            && amplitude.abs() <= f32::MAX / 2.0
        {
            return 0.0;
        }
        // Position scale is an amplitude, unlike alpha/size interpolation endpoints.
        let phase_cos = if self.cached_phase {
            p.osc_phase_cos[(axis + 1) % 3]
        } else {
            phase.cos()
        };
        amplitude * (angle.cos() - phase_cos)
    }
    fn value(&self, p: &Particle, axis: usize) -> f32 {
        let t = (p.age * lerp(self.frequency[0], self.frequency[1], p.random[axis])
            + lerp(self.phase[0], self.phase[1], p.random[(axis + 1) % 3]))
        .sin()
            * 0.5
            + 0.5;
        lerp(self.scale[0], self.scale[1], t)
    }
}
#[derive(Clone)]
enum Op {
    Movement {
        gravity: Vec3,
        drag: f32,
        world: bool,
        impulse: Vec3,
        damping: f32,
    },
    Angular {
        force: Vec3,
        drag: f32,
        impulse: Vec3,
        damping: f32,
    },
    Fade {
        inside: f32,
        outside: f32,
    },
    Change {
        field: u8,
        time: Vec2,
        start: Vec3,
        end: Vec3,
    },
    Turbulence(Force),
    Inherit(event_values::Field),
    Remap(remap::Remap),
    Constraint(positions::Constraint),
    Collision(collision::Collision),
    Boids(boids::Boids),
    OscAlpha(Osc),
    OscSize(Osc),
    OscPosition(Osc),
    Attract {
        cp: usize,
        origin: Vec3,
        scale: f32,
        threshold: f32,
    },
    Vortex(vortex::Vortex),
    Cap {
        speed: f32,
        time: Vec2,
    },
    Reduce {
        cp: usize,
        distance: Vec2,
        inner: f32,
    },
}
#[derive(Clone)]
struct Overrides {
    count: f32,
    size: f32,
    life: f32,
    speed: f32,
    rate: f32,
    alpha: f32,
    color: Option<Vec3>,
}
impl Overrides {
    fn enabled(mut self, flags: u32) -> Self {
        if flags & 8 != 0 {
            self.color = None;
        }
        if flags & 16 != 0 {
            self.count = 1.;
        }
        if flags & 32 != 0 {
            self.life = 1.;
        }
        if flags & 64 != 0 {
            self.size = 1.;
        }
        if flags & 128 != 0 {
            self.speed = 1.;
        }
        self
    }
    fn parse(v: &Value) -> Result<Self> {
        let value = Self {
            count: scalar(v, "count", 1.0)?,
            size: scalar(v, "size", 1.0)?,
            life: scalar(v, "lifetime", 1.0)?,
            speed: scalar(v, "speed", 1.0)?,
            rate: scalar(v, "rate", 1.0)?,
            alpha: scalar(v, "alpha", 1.0)?,
            color: if !v["colorn"].is_null() {
                Some(vector(v, "colorn", Vec3::ONE)?)
            } else if !v["color"].is_null() {
                Some(vector(v, "color", Vec3::splat(255.0))? / 255.0)
            } else {
                None
            },
        };
        ensure!(
            value.count >= 0.0
                && value.size >= 0.0
                && value.life > 0.0
                && (0.0..=128.0).contains(&value.rate),
            "invalid particle instance multiplier"
        );
        Ok(value)
    }
}

#[derive(Clone, Copy, Default)]
struct ControlPoint {
    pub offset: Vec3,
    pub flags: u32,
    parent: usize,
}
fn parse_control_points(value: &Value) -> Result<[ControlPoint; 8]> {
    let mut points = [ControlPoint::default(); 8];
    let mut seen = [false; 8];
    for point in entries(value, "controlpoint", 8)? {
        let id = control_point_index(point, "id")?;
        ensure!(!seen[id], "duplicate particle control point {id}");
        seen[id] = true;
        points[id] = ControlPoint {
            offset: vector(point, "offset", Vec3::ZERO)?,
            flags: super::animation::flags(point)? | u32::from(point["locktopointer"] == true),
            parent: control_point_index(point, "parentcontrolpoint")?,
        };
    }
    Ok(points)
}
pub(in crate::particles) fn control_point_index(value: &Value, name: &str) -> Result<usize> {
    let index = scalar(value, name, 0.)?;
    ensure!(
        (0. ..8.).contains(&index) && index.fract() == 0.,
        "invalid particle {name}"
    );
    Ok(index as usize)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod control_point_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn inherited_velocity_uses_sample_time_transforms_and_holds_one_latest_input() {
        for world in [0, 1] {
            let mut system=System::new(&json!({"flags":world,"maxcount":3,"controlpoint":[{"id":1,"locktopointer":true}],"emitter":[{"name":"boxrandom","controlpoint":1,"instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"inheritcontrolpointvelocity","controlpoint":1,"min":0.5,"max":0.5}]}),&Value::Null,7).unwrap();
            let space = Mat4::from_translation(Vec3::new(10., 20., 0.))
                * Mat4::from_rotation_z(0.7)
                * Mat4::from_scale(Vec3::new(2., 1., 1.));
            system.set_space(space);
            system.update_points(Vec3::ZERO, space.inverse(), Vec3::ZERO);
            system.point_motion(0.);
            system.step_tick(0., &crate::audio::AudioSnapshot::default());
            assert_eq!(system.particles[0].velocity, Vec3::ZERO);
            system.update_points(Vec3::new(4., 6., 0.), space.inverse(), Vec3::ZERO);
            system.point_motion(0.5);
            let velocity = system.point_velocity[1];
            assert!(velocity.abs_diff_eq(space.transform_vector3(Vec3::new(8., 12., 0.)), 1e-4));
            for _ in 0..30 {
                system.point_motion(0.5);
                assert_eq!(system.point_velocity[1], velocity);
            }
            system.forced = 1;
            system.step_tick(0., &crate::audio::AudioSnapshot::default());
            let inherited = if world != 0 {
                system.particles[1].velocity
            } else {
                space.transform_vector3(system.particles[1].velocity)
            };
            assert!(inherited.abs_diff_eq(velocity * 0.5, 1e-4));
            system.point_motion(1.);
            assert_eq!(system.point_velocity[1], Vec3::ZERO);
            system.forced = 1;
            system.step_tick(0., &crate::audio::AudioSnapshot::default());
            assert_eq!(system.particles[2].velocity, Vec3::ZERO);
            let reset = system.fresh(7);
            assert!(reset.point_sample_time.is_none());
            assert_eq!(reset.point_velocity, [Vec3::ZERO; 8]);
        }
        let mut parent = System::new(
            &json!({"initializer":[{"name":"inheritcontrolpointvelocity"}]}),
            &Value::Null,
            7,
        )
        .unwrap();
        parent.set_space(Mat4::IDENTITY);
        parent.point_motion(0.);
        parent.set_space(Mat4::from_translation(Vec3::new(4., 0., 0.)));
        parent.point_motion(0.5);
        let mut child=System::new(&json!({"controlpoint":[{"id":0,"flags":12}],"initializer":[{"name":"inheritcontrolpointvelocity"}]}),&Value::Null,7).unwrap();
        let scale = Mat4::from_scale(Vec3::new(2., 1., 1.));
        child.set_space(scale);
        child.point_motion(0.);
        child.set_space(parent.space * scale);
        child.inherit_points(&parent, None);
        child.point_motion(0.5);
        child.inherit_point_motion(&parent, None);
        assert!(
            child.point_velocity[0].abs_diff_eq(Vec3::new(8., 0., 0.), 1e-5),
            "raw copies must not multiply the parent's world motion by the child scale"
        );
    }
    #[test]
    fn parent_indices_flags_duplicates_and_pointer_alias_are_validated() {
        let points = parse_control_points(
            &json!({"controlpoint":[{"id":2,"locktopointer":true,"offset":"1 2 3"}]}),
        )
        .unwrap();
        assert_eq!(points[2].flags, 1);
        assert_eq!(points[2].offset, Vec3::new(1., 2., 3.));
        for point in [
            json!({"id":8}),
            json!({"id":1.5}),
            json!({"parentcontrolpoint":8}),
            json!({"flags":-1}),
            json!({"flags":1.5}),
        ] {
            assert!(parse_control_points(&json!({"controlpoint":[point]})).is_err());
        }
        assert!(parse_control_points(&json!({"controlpoint":[{"id":0},{"id":0}]})).is_err());
    }
}
