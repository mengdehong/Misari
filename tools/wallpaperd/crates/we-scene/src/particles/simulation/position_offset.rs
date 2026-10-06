//! Bounded fractal displacement evaluated once when a particle is initialized.
use super::{Particle, scalar, vector};
use anyhow::{Result, ensure};
use glam::Vec3;
use serde_json::Value;

#[derive(Clone)]
pub(super) struct Offset {
    directions: Vec3,
    sign: Vec3,
    distance: f32,
    octaves: u8,
    scale: f32,
    time: f32,
}
impl Offset {
    pub(super) fn parse(v: &Value) -> Result<Self> {
        let octaves = scalar(v, "octaves", 4.)?;
        ensure!(
            (1.0..=8.0).contains(&octaves) && octaves.fract() == 0.,
            "particle noise requires 1 to 8 octaves"
        );
        Ok(Self {
            directions: vector(v, "directions", Vec3::new(1., 1., 0.))?,
            sign: vector(v, "sign", Vec3::ZERO)?,
            distance: scalar(v, "distance", 100.)?.max(0.),
            octaves: octaves as u8,
            scale: scalar(v, "scale", 0.01)?,
            time: scalar(v, "timescale", 1.)?,
        })
    }
    pub(super) fn value(&self, p: &Particle, time: f32) -> Vec3 {
        if self.distance == 0. || self.directions == Vec3::ZERO {
            return Vec3::ZERO;
        }
        let mut sample = p.random * 256. + p.position * self.scale + Vec3::splat(time * self.time);
        let mut amplitude = 1.;
        let mut weight = 0.;
        let mut offset = Vec3::ZERO;
        for _ in 0..self.octaves {
            offset += super::super::noise::field(sample) * amplitude;
            weight += amplitude;
            sample *= 2.;
            amplitude *= 0.5;
        }
        offset = (offset / weight).clamp_length_max(1.) * self.distance * self.directions;
        for axis in 0..3 {
            if self.sign[axis] != 0. {
                offset[axis] = offset[axis].abs() * self.sign[axis].signum();
            }
        }
        offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{audio::AudioSnapshot, particles::simulation::System};
    use serde_json::json;
    #[test]
    fn offsets_have_fixed_seed_axis_sign_distance_and_only_initialize_births() {
        let definition = json!({"maxcount":32,"emitter":[{"name":"boxrandom","instantaneous":8,"rate":8}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"positionoffsetrandom","directions":"1 0 1","sign":"1 0 -1","distance":8}]});
        let mut a = System::new(&definition, &Value::Null, 29).unwrap();
        let mut b = a.clone();
        let audio = AudioSnapshot::default();
        a.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        for p in &a.particles {
            assert!(p.position.is_finite() && p.position.length() <= 8.);
            assert!(p.position.x >= 0. && p.position.z <= 0.);
            assert_eq!(p.position.y, 0.);
        }
        assert!(a.particles.iter().any(|p| p.position.length() > 0.1));
        let initial: Vec<_> = a.particles.iter().map(|p| p.position).collect();
        for time in [0.1, 0.2, 0.3, 0.4, 0.5] {
            a.advance(time, Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        }
        b.advance(0.5, Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        assert_eq!(a.particles, b.particles);
        assert_eq!(
            &a.particles.iter().map(|p| p.position).collect::<Vec<_>>()[..initial.len()],
            initial
        );
        for invalid in [
            json!({"octaves":0}),
            json!({"octaves":1000}),
            json!({"octaves":1.5}),
            json!({"scale":"NaN"}),
        ] {
            assert!(Offset::parse(&invalid).is_err());
        }
    }
}
