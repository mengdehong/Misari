//! Ordered control-point paths and constraints retain birth offsets as points move.
use super::{Overrides, Particle, Range, cp, scalar, vector};
use crate::scene::bindings::components;
use anyhow::Result;
use glam::{Quat, Vec2, Vec3};
use serde_json::Value;
use std::f32::consts::TAU;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Path {
    pub start: usize,
    pub end: usize,
    pub fraction: f32,
    pub offset: Vec3,
    pub axis: Vec3,
}
#[derive(Clone)]
pub(super) struct Around {
    count: f32,
    cp: usize,
    bounds: Vec2,
    speed: Range,
    axis: Quat,
    mirror: bool,
    flags: u32,
    sequence: u64,
}
#[derive(Clone)]
pub(super) struct Between {
    count: f32,
    start: usize,
    end: usize,
    bounds: Vec2,
    arc: f32,
    direction: Vec3,
    mirror: bool,
    flags: u32,
    sequence: u64,
}
fn bounds(v: &Value) -> Result<Vec2> {
    let b = components(&v["bounds"], &[0., 1.], 2)?;
    Ok(Vec2::new(b[0], b[1]))
}
fn fraction(sequence: u64, count: f32, mirror: bool, inclusive: bool) -> f32 {
    if count <= 1. {
        return 0.;
    }
    let count = count as f64;
    let denominator = if inclusive || mirror {
        (count - 1.).max(1.)
    } else {
        count
    };
    if mirror {
        let phase = (sequence as f64 / denominator).rem_euclid(2.);
        if phase <= 1. {
            phase as f32
        } else {
            (2. - phase) as f32
        }
    } else {
        (sequence as f64 % count / denominator).min(1.) as f32
    }
}
impl Around {
    pub(super) fn parse(v: &Value) -> Result<Self> {
        Ok(Self {
            count: scalar(v, "count", 1.)?.clamp(1., super::MAX_PARTICLES as f32),
            cp: cp(v)?,
            bounds: bounds(v)?,
            speed: Range {
                min: vector(v, "speedmin", Vec3::ZERO)?,
                max: vector(v, "speedmax", Vec3::splat(100.))?,
                exponent: Vec3::ONE,
            },
            axis: Quat::from_rotation_arc(
                Vec3::Z,
                vector(v, "axis", Vec3::Z)?.normalize_or(Vec3::Z),
            ),
            mirror: v["limitbehavior"] == "mirror",
            flags: super::super::animation::flags(v)?,
            sequence: 0,
        })
    }
    pub(super) fn apply(
        &mut self,
        p: &mut Particle,
        points: &[Vec3; 8],
        overrides: &Overrides,
        rng: &mut rand_chacha::ChaCha8Rng,
    ) {
        let count = (self.count
            * if self.flags & 1 != 0 {
                overrides.count
            } else {
                1.
            })
        .max(1.);
        let t = fraction(self.sequence, count, self.mirror, false);
        self.sequence = self.sequence.wrapping_add(1);
        let rotation = self.axis
            * Quat::from_rotation_z(-TAU * (self.bounds.x + (self.bounds.y - self.bounds.x) * t));
        let speed = self.speed.sample(rng);
        // Order the emitter's radius in the axis plane, retaining its axial
        // offset. The emitter still determines how far births are from the CP.
        let offset = self.axis.inverse() * (p.position - points[self.cp]);
        p.position =
            points[self.cp] + rotation * Vec3::new(0., offset.truncate().length(), offset.z);
        p.velocity = rotation * speed * overrides.speed;
    }
    pub(super) fn reset(&mut self, periodic: bool) {
        if !periodic || self.flags & 2 != 0 {
            self.sequence = 0;
        }
    }
}
impl Between {
    pub(super) fn parse(v: &Value) -> Result<Self> {
        Ok(Self {
            count: scalar(v, "count", 100.)?.clamp(1., super::MAX_PARTICLES as f32),
            start: super::control_point_index(v, "controlpointstart")?,
            end: if v["controlpointend"].is_null() {
                1
            } else {
                super::control_point_index(v, "controlpointend")?
            },
            bounds: bounds(v)?,
            arc: scalar(v, "arcamount", 0.)?,
            direction: vector(v, "arcdirection", Vec3::Y)?.normalize_or(Vec3::Y),
            mirror: v["limitbehavior"] == "mirror",
            flags: super::super::animation::flags(v)?,
            sequence: 0,
        })
    }
    pub(super) fn apply(&mut self, p: &mut Particle, points: &[Vec3; 8], overrides: &Overrides) {
        let count = (self.count
            * if self.flags & 1 != 0 {
                overrides.count
            } else {
                1.
            })
        .max(1.);
        let mut t = fraction(self.sequence, count, self.mirror, true);
        self.sequence = self.sequence.wrapping_add(1);
        if self.flags & 4 != 0 {
            t = t * t * (3. - 2. * t);
        }
        let fraction = self.bounds.x + (self.bounds.y - self.bounds.x) * t;
        let delta = points[self.end] - points[self.start];
        let mut position = points[self.start] + delta * fraction;
        if self.flags & 32 != 0 {
            position += self.direction * self.arc * delta.length() * 4. * t * (1. - t);
        }
        if self.flags & (8 | 16) != 0 {
            let edge = (4. * t * (1. - t)).clamp(0., 1.);
            if self.flags & 8 != 0 {
                p.velocity *= edge;
            }
            if self.flags & 16 != 0 {
                p.size *= edge;
            }
        }
        p.position = position;
        p.path = Some(Path {
            start: self.start,
            end: self.end,
            fraction,
            offset: Vec3::ZERO,
            axis: delta.normalize_or(Vec3::X),
        });
    }
    pub(super) fn reset(&mut self, periodic: bool) {
        if !periodic || self.flags & 2 != 0 {
            self.sequence = 0;
        }
    }
}
#[derive(Clone)]
pub(super) enum Constraint {
    Radius {
        cp: usize,
        distance: f32,
        strength: Option<f32>,
    },
    Between {
        start: usize,
        end: usize,
    },
}
impl Constraint {
    pub(super) fn radius(v: &Value) -> Result<Self> {
        Ok(Self::Radius {
            cp: cp(v)?,
            distance: scalar(v, "distance", 256.)?.max(0.),
            strength: if v["variablestrength"].is_null() {
                None
            } else {
                Some(scalar(v, "variablestrength", 1.)?.max(0.))
            },
        })
    }
    pub(super) fn between(v: &Value) -> Result<Self> {
        Ok(Self::Between {
            start: super::control_point_index(v, "controlpointstart")?,
            end: if v["controlpointend"].is_null() {
                1
            } else {
                super::control_point_index(v, "controlpointend")?
            },
        })
    }
    pub(super) fn apply(&self, p: &mut Particle, points: &[Vec3; 8], dt: f32) {
        match *self {
            Self::Radius {
                cp,
                distance,
                strength,
            } => {
                let direction = (p.position - points[cp]).normalize_or(Vec3::X);
                let target = points[cp] + direction * distance;
                let weight = strength.map_or(1., |s| 1. - (-s * dt).exp());
                p.position = p.position.lerp(target, weight);
            }
            Self::Between { start, end } => {
                let axis = (points[end] - points[start]).normalize_or(Vec3::X);
                let path = p.path.get_or_insert(Path {
                    start,
                    end,
                    fraction: p.random.x,
                    offset: Vec3::ZERO,
                    axis,
                });
                if path.start != start || path.end != end {
                    *path = Path {
                        start,
                        end,
                        fraction: p.random.x,
                        offset: Vec3::ZERO,
                        axis,
                    };
                }
                let rotation = Quat::from_rotation_arc(path.axis, axis);
                p.position =
                    points[start].lerp(points[end], path.fraction) + rotation * path.offset;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{audio::AudioSnapshot, particles::simulation::System};
    use serde_json::json;
    #[test]
    fn between_uses_authored_points_bounds_mirror_and_keeps_birth_noise_on_moving_paths() {
        let value = json!({"maxcount":3,"controlpoint":[{"id":1,"offset":"10 0 0"},{"id":2,"offset":"20 0 0"}],"emitter":[{"name":"boxrandom","instantaneous":3,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"mapsequencebetweencontrolpoints","count":3,"controlpointstart":1,"controlpointend":2,"bounds":"0.25 0.75"},{"name":"positionoffsetrandom","directions":"0 1 0","distance":2}],"operator":[{"name":"maintaindistancebetweencontrolpoints","controlpointstart":1,"controlpointend":2}]});
        let audio = AudioSnapshot::default();
        let mut system = System::new(&value, &Value::Null, 17).unwrap();
        system.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        let initial = system.particles.clone();
        for (p, x) in initial.iter().zip([12.5, 15., 17.5]) {
            assert!((p.position.x - x).abs() < 0.00001);
            assert_eq!(p.position.z, 0.);
        }
        system
            .set_overrides(&json!({"controlpoint1":"10 10 0","controlpoint2":"10 20 0"}))
            .unwrap();
        system.advance(0.1, Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        for (p, birth) in system.particles.iter().zip(&initial) {
            let expected = Vec3::new(10. - birth.position.y, birth.position.x, 0.);
            assert!(
                (p.position - expected).length() < 0.00001,
                "path offset was lost: {:?}, expected {expected:?}",
                p.position
            );
        }
        let mut value = value;
        value["operator"] = json!([]);
        value["initializer"].as_array_mut().unwrap().pop();
        value["initializer"][1]["limitbehavior"] = json!("mirror");
        value["emitter"][0]["instantaneous"] = json!(5);
        value["maxcount"] = json!(5);
        let mut mirror = System::new(&value, &Value::Null, 17).unwrap();
        mirror.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        assert_eq!(
            mirror
                .particles
                .iter()
                .map(|p| p.position.x)
                .collect::<Vec<_>>(),
            [12.5, 15., 17.5, 15., 12.5]
        );
    }
    #[test]
    fn around_orders_birth_radius_and_velocity_in_clockwise_sequence() {
        let mut value = json!({"maxcount":4,"controlpoint":[{"id":1,"offset":"10 0 0"}],"emitter":[{"name":"boxrandom","controlpoint":1,"origin":"2 0 0","instantaneous":4,"rate":0}],"initializer":[{"name":"mapsequencearoundcontrolpoint","controlpoint":1,"count":4,"speedmin":"4 0 0","speedmax":"4 0 0"}]});
        let audio = AudioSnapshot::default();
        let mut system = System::new(&value, &Value::Null, 7).unwrap();
        system.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        for (slot, (p, velocity)) in system
            .particles
            .iter()
            .zip([Vec3::X * 4., -Vec3::Y * 4., -Vec3::X * 4., Vec3::Y * 4.])
            .enumerate()
        {
            assert!((p.velocity - velocity).length() < 0.00001);
            let expected =
                Vec3::X * 10. + Quat::from_rotation_z(-TAU * slot as f32 / 4.) * Vec3::Y * 2.;
            assert!(p.position.abs_diff_eq(expected, 0.00001));
        }
        value["initializer"][0]["count"] = json!(3.02);
        let mut fractional = System::new(&value, &Value::Null, 7).unwrap();
        fractional.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        let expected = Quat::from_rotation_z(-TAU / 3.02) * Vec3::X * 4.;
        assert!((fractional.particles[1].velocity - expected).length() < 0.00001);
        value["initializer"][0]["axis"] = json!("1 0 0");
        let mut axis = System::new(&value, &Value::Null, 7).unwrap();
        axis.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        assert!(
        axis.particles
            .iter()
            .all(|p| p.velocity.x.abs() < 0.00001 && (p.velocity.length() - 4.).abs() < 0.00001)
    );
    }
    #[test]
    fn sequenced_petals_keep_birth_radius_rotation_and_outward_velocity() {
        let value = json!({"maxcount":5,
        "controlpoint":[{"id":1,"offset":"10 20 30"}],
        "emitter":[{"name":"sphererandom","controlpoint":1,"distancemin":200,"distancemax":300,"instantaneous":5,"rate":0}],
        "initializer":[
            {"name":"velocityrandom","min":"50 50 50","max":"50 50 50"},
            {"name":"mapsequencearoundcontrolpoint","controlpoint":1,"count":5,"speedmin":"0 100 0","speedmax":"0 100 0"}
        ],"operator":[{"name":"movement","gravity":"0 50 0"}]});
        let mut system = System::new(&value, &Value::Null, 7).unwrap();
        system.advance(
            0.,
            Vec3::ZERO,
            glam::Mat4::IDENTITY,
            &AudioSnapshot::default(),
        );
        for (slot, p) in system.particles.iter().enumerate() {
            let offset = p.position - Vec3::new(10., 20., 30.);
            assert!((200. ..=300.).contains(&offset.length()));
            let direction = Quat::from_rotation_z(-TAU * slot as f32 / 5.) * Vec3::Y;
            assert!(
                offset
                    .truncate()
                    .normalize()
                    .abs_diff_eq(direction.truncate(), 0.0001)
            );
            let expected = Quat::from_rotation_z(-TAU * slot as f32 / 5.) * Vec3::Y * 100.;
            assert!(p.velocity.abs_diff_eq(expected, 0.0001));
            assert_eq!(p.rotation, Vec3::ZERO);
        }
        let velocities = system
            .particles
            .iter()
            .map(|p| p.velocity)
            .collect::<Vec<_>>();
        system.step_tick(super::super::STEP as f32, &AudioSnapshot::default());
        for (p, velocity) in system.particles.iter().zip(velocities) {
            assert!(
                p.velocity
                    .abs_diff_eq(velocity + Vec3::Y * 50. * super::super::STEP as f32, 0.0001)
            );
        }
    }
    #[test]
    fn count_modifiers_periodic_restart_and_variable_radius_strength_are_bounded() {
        let mut value = json!({"maxcount":8,"controlpoint":[{"id":1,"offset":"8 0 0"}],"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"mapsequencebetweencontrolpoints","count":3,"flags":3}]});
        let audio = AudioSnapshot::default();
        let mut system = System::new(&value, &json!({"count":2}), 7).unwrap();
        system.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        system.forced = 1;
        system.step_tick(0., &audio);
        assert!((system.particles[1].position.x - 1.6).abs() < 0.00001);
        system.restart_emission();
        system.step_tick(0., &audio);
        assert_eq!(system.particles[2].position, Vec3::ZERO);
        value["initializer"] = json!([]);
        value["emitter"][0]["origin"] = json!("1 0 0");
        value["operator"] = json!([{"name":"maintaindistancetocontrolpoint","distance":8}]);
        let mut radius = System::new(&value, &Value::Null, 7).unwrap();
        radius.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        assert_eq!(radius.particles[0].position, Vec3::X * 8.);
        value["operator"][0]["variablestrength"] = json!(5);
        let mut soft = System::new(&value, &Value::Null, 7).unwrap();
        soft.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        assert_eq!(soft.particles[0].position, Vec3::X);
        soft.advance(0.1, Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        assert!(soft.particles[0].position.x > 1. && soft.particles[0].position.x < 8.);
    }
}
