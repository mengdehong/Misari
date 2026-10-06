//! Flocking reads one immutable step snapshot, with bounded dense-cell sampling.
use super::{Particle, scalar};
use anyhow::{Result, ensure};
use glam::Vec3;
use serde_json::Value;
use std::collections::HashMap;

const CELL_SAMPLES: usize = 64;
const CANDIDATES: usize = 256;
type CellKey = [i64; 3];
#[derive(Clone)]
pub(super) struct Boids {
    neighbor: f32,
    separation: f32,
    cohesion_factor: f32,
    alignment_factor: f32,
    separation_factor: f32,
    max_speed: Option<f32>,
    grid: HashMap<CellKey, Vec<usize>>,
    forces: Vec<Vec3>,
    #[cfg(test)]
    visits: usize,
}
fn key(position: Vec3, width: f32) -> CellKey {
    position
        .to_array()
        .map(|v| (v as f64 / width as f64).floor() as i64)
}
fn hash(mut n: u64) -> u64 {
    n = (n ^ (n >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    n = (n ^ (n >> 27)).wrapping_mul(0x94d049bb133111eb);
    n ^ (n >> 31)
}
impl Boids {
    pub(super) fn parse(v: &Value) -> Result<Self> {
        let neighbor = scalar(v, "neighborthreshold", 50.)?;
        let separation = scalar(v, "separationthreshold", 25.)?;
        let max_speed = scalar(v, "maxspeed", 100.)?;
        ensure!(
            neighbor >= 0. && separation >= 0. && max_speed >= 0.,
            "negative particle boids threshold/speed"
        );
        Ok(Self {
            neighbor,
            separation,
            cohesion_factor: scalar(v, "cohesionfactor", 1.)?,
            alignment_factor: scalar(v, "alignmentfactor", 1.)?,
            separation_factor: scalar(v, "separationfactor", 1.)?,
            max_speed: (super::super::animation::flags(v)? & 1 != 0 || v["clampspeed"] == true)
                .then_some(max_speed),
            grid: HashMap::new(),
            forces: Vec::new(),
            #[cfg(test)]
            visits: 0,
        })
    }
    pub(super) fn prepare(&mut self, particles: &[Particle]) {
        self.forces.clear();
        self.forces.resize(particles.len(), Vec3::ZERO);
        #[cfg(test)]
        {
            self.visits = 0;
        }
        let width = self.neighbor.max(self.separation);
        if width == 0. {
            return;
        }
        self.grid.clear();
        for (i, p) in particles.iter().enumerate() {
            self.grid.entry(key(p.position, width)).or_default().push(i);
        }
        for (i, p) in particles.iter().enumerate() {
            let center = key(p.position, width);
            let mut position = Vec3::ZERO;
            let mut velocity = Vec3::ZERO;
            let mut separation = Vec3::ZERO;
            let mut neighbors = 0.;
            let mut visited = 0;
            let salt = hash(p.id ^ p.random.x.to_bits() as u64);
            // Rotate cell traversal per particle to avoid privileging one side
            // when the candidate budget is reached in a very dense population.
            for cell in 0..27 {
                let offset = (cell + salt as usize % 27) % 27;
                let key = std::array::from_fn(|axis| {
                    center[axis].saturating_add((offset / 3usize.pow(axis as u32) % 3) as i64 - 1)
                });
                let Some(indices) = self.grid.get(&key) else {
                    continue;
                };
                let count = indices.len().min(CELL_SAMPLES);
                let start = hash(salt ^ offset as u64) as usize % indices.len();
                for sample in 0..count {
                    if visited == CANDIDATES {
                        break;
                    }
                    let j = indices[(start + sample * indices.len() / count) % indices.len()];
                    visited += 1;
                    if i == j {
                        continue;
                    }
                    let q = &particles[j];
                    let delta = p.position - q.position;
                    let distance = delta.length_squared();
                    if distance <= self.neighbor * self.neighbor {
                        neighbors += 1.;
                        position += q.position;
                        velocity += q.velocity;
                    }
                    if distance <= self.separation * self.separation {
                        if distance > 1e-8 {
                            separation += delta / distance;
                        } else {
                            // Coincident particles separate symmetrically and
                            // deterministically without consuming the birth RNG.
                            let pair = hash(p.id.min(q.id) ^ hash(p.id.max(q.id)));
                            let direction = Vec3::from_array(std::array::from_fn(|axis| {
                                if pair & (1 << axis) == 0 { 1. } else { -1. }
                            }))
                            .normalize();
                            separation += direction * if p.id < q.id { 1. } else { -1. };
                        }
                    }
                }
                if visited == CANDIDATES {
                    break;
                }
            }
            let shared = if neighbors > 0. {
                (position / neighbors - p.position) * self.cohesion_factor
                    + (velocity / neighbors - p.velocity) * self.alignment_factor
            } else {
                Vec3::ZERO
            };
            self.forces[i] = shared + separation * self.separation_factor;
            #[cfg(test)]
            {
                self.visits += visited;
            }
        }
    }
    pub(super) fn apply(&self, p: &mut Particle, index: usize, dt: f32) {
        p.velocity += self.forces[index] * dt;
        if let Some(limit) = self.max_speed {
            p.velocity = p.velocity.clamp_length_max(limit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{audio::AudioSnapshot, particles::simulation::System};
    use glam::Mat4;
    use serde_json::json;
    fn particles(count: usize) -> Vec<Particle> {
        let mut s=System::new(&json!({"maxcount":count,"emitter":[{"name":"boxrandom","instantaneous":count,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2}]}),&Value::Null,7).unwrap();
        s.advance(0., Vec3::ZERO, Mat4::IDENTITY, &AudioSnapshot::default());
        s.particles
    }
    #[test]
    fn distinct_neighborhood_separation_alignment_cohesion_and_speed_clamp() {
        let mut particles = particles(3);
        particles[0].position = Vec3::ZERO;
        particles[1].position = Vec3::X * 2.;
        particles[2].position = Vec3::X * 30.;
        particles[0].velocity = Vec3::Y;
        particles[1].velocity = Vec3::Y * 3.;
        for (field, expected) in [
            ("cohesionfactor", Vec3::X * 2.),
            ("alignmentfactor", Vec3::Y * 2.),
            ("separationfactor", -Vec3::X * 0.5),
        ] {
            let mut v = json!({"neighborthreshold":10,"separationthreshold":4,"cohesionfactor":0,"alignmentfactor":0,"separationfactor":0});
            v[field] = json!(1);
            let mut boids = Boids::parse(&v).unwrap();
            boids.prepare(&particles);
            assert!(
                (boids.forces[0] - expected).length() < 1e-6,
                "{field}: {:?}",
                boids.forces[0]
            );
            assert_eq!(boids.forces[2], Vec3::ZERO);
        }
        let mut clamp=Boids::parse(&json!({"clampspeed":true,"maxspeed":2,"cohesionfactor":0,"alignmentfactor":0,"separationfactor":0})).unwrap();
        clamp.prepare(&particles);
        clamp.apply(&mut particles[1], 1, 1.);
        assert_eq!(particles[1].velocity, Vec3::Y * 2.);
        assert!(Boids::parse(&json!({"neighborthreshold":-1})).is_err());
    }
    #[test]
    fn immutable_snapshot_has_no_particle_order_bias_and_dense_work_is_bounded() {
        let mut particles = particles(20);
        for (i, p) in particles.iter_mut().enumerate() {
            p.position = Vec3::X * i as f32;
            p.velocity = Vec3::Y * i as f32;
        }
        let mut config = Boids::parse(&json!({})).unwrap();
        config.prepare(&particles);
        let expected = config.forces.clone();
        particles.reverse();
        config.prepare(&particles);
        for (a, b) in config.forces.iter().zip(expected.iter().rev()) {
            assert!((*a - *b).length() < 1e-5);
        }
        let mut dense = particles.clone();
        dense.resize_with(20_000, || particles[0].clone());
        for (i, p) in dense.iter_mut().enumerate() {
            p.id = i as u64;
            p.position = Vec3::ZERO;
        }
        config.prepare(&dense);
        assert!(config.visits <= dense.len() * CANDIDATES);
        assert!(config.forces.iter().all(|f| f.is_finite()));
        assert_eq!(config.grid.len(), 1);
    }
    #[test]
    fn seeded_fixed_step_flocking_matches_output_rates_and_rewind() {
        let value = json!({"maxcount":50,"emitter":[{"name":"sphererandom","instantaneous":50,"rate":0,"distancemax":10,"speedmin":2,"speedmax":2}],"initializer":[{"name":"lifetimerandom","min":2,"max":2}],"operator":[{"name":"movement"},{"name":"boids","neighborthreshold":20,"clampspeed":true,"maxspeed":10}]});
        let audio = AudioSnapshot::default();
        let run = |fps: u32| {
            let mut s = System::new(&value, &Value::Null, 7).unwrap();
            for n in 0..=fps {
                while !s.advance(n as f64 / fps as f64, Vec3::ZERO, Mat4::IDENTITY, &audio) {}
            }
            s
        };
        let mut fast = run(60);
        let slow = run(24);
        assert_eq!(fast.particles, slow.particles);
        let mut fresh = System::new(&value, &Value::Null, 7).unwrap();
        fresh.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
        fast.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
        assert_eq!(fast.particles, fresh.particles);
    }
}
