//! Axis, cylindrical and hollow-ring vortex forces in the simulation space.
use super::{Audio, Particle, cp, lerp, ramp, scalar, vector};
use crate::audio::AudioSnapshot;
use anyhow::Result;
use glam::{Vec2, Vec3};
use serde_json::Value;

#[derive(Clone)]
pub(super) struct Vortex {
    cp: usize,
    axis: Vec3,
    offset: Vec3,
    distance: Vec2,
    speed: Vec2,
    flags: u32,
    center: f32,
    radius: f32,
    width: f32,
    pull_distance: f32,
    pull: f32,
    audio: Audio,
}
impl Vortex {
    pub(super) fn parse(v: &Value) -> Result<Self> {
        Ok(Self {
            cp: cp(v)?,
            axis: vector(v, "axis", Vec3::Z)?.normalize_or(Vec3::Z),
            offset: vector(v, "offset", Vec3::ZERO)?,
            distance: Vec2::new(
                scalar(v, "distanceinner", 500.)?,
                scalar(v, "distanceouter", 650.)?,
            ),
            speed: Vec2::new(
                scalar(v, "speedinner", 2500.)?,
                scalar(v, "speedouter", 0.)?,
            ),
            flags: super::super::animation::flags(v)?,
            center: scalar(v, "centerforce", 1.)?,
            radius: scalar(v, "ringradius", 300.)?.max(0.),
            width: scalar(v, "ringwidth", 50.)?.max(0.),
            pull_distance: scalar(v, "ringpulldistance", 50.)?.max(0.),
            pull: scalar(v, "ringpullforce", 10.)?,
            audio: Audio::parse(v, false)?,
        })
    }
    pub(super) fn needs_audio(&self) -> bool {
        self.audio.mode != 0 && (self.speed != Vec2::ZERO || self.pull != 0. || self.center != 0.)
    }
    pub(super) fn value(&self, p: &Particle, points: &[Vec3; 8], audio: &AudioSnapshot) -> Vec3 {
        let mut radial = p.position - points[self.cp] - self.offset;
        if self.flags & 1 != 0 {
            radial -= self.axis * radial.dot(self.axis);
        }
        let distance = radial.length();
        // WE's signed vortex speeds turn along radial × axis. Reversing the
        // operands makes negative speeds rotate clockwise in the rendered image.
        let tangent = radial.cross(self.axis).normalize_or_zero();
        if distance <= f32::EPSILON || tangent == Vec3::ZERO {
            return Vec3::ZERO;
        }
        let direction = radial / distance;
        let mut pull = Vec3::ZERO;
        let speed = if self.flags & 4 != 0 {
            let inner = (self.radius - self.width * 0.5).max(0.);
            let outer = self.radius + self.width * 0.5;
            if distance < inner {
                0.
            } else if distance <= outer {
                lerp(self.speed.x, self.speed.y, ramp(distance, inner, outer))
            } else if self.pull_distance > 0. && distance < outer + self.pull_distance {
                let weight = ramp(distance, outer, outer + self.pull_distance);
                pull = -direction * self.pull * weight;
                self.speed.y * (1. - weight)
            } else {
                0.
            }
        } else {
            lerp(
                self.speed.x,
                self.speed.y,
                ramp(distance, self.distance.x, self.distance.y),
            )
        };
        if self.flags & 2 != 0 {
            pull -= direction * self.center;
        }
        (tangent * speed + pull) * self.audio.response(audio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::particles::simulation::System;
    use serde_json::json;
    #[test]
    fn cylinders_ignore_axial_distance_rings_have_hollow_centers_and_audio_is_stereo() {
        let mut system = System::new(
            &json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}]}),
            &Value::Null,
            7,
        )
        .unwrap();
        let silent = AudioSnapshot::default();
        system.step_tick(0., &silent);
        let p = &mut system.particles[0];
        p.position = Vec3::new(10., 0., 100.);
        let points = [Vec3::ZERO; 8];
        let mut config =
            json!({"flags":1,"distanceinner":10,"distanceouter":20,"speedinner":40,"speedouter":0});
        assert_eq!(
            Vortex::parse(&config).unwrap().value(p, &points, &silent),
            -Vec3::Y * 40.
        );
        config["flags"] = json!(0);
        assert_eq!(
            Vortex::parse(&config).unwrap().value(p, &points, &silent),
            Vec3::ZERO
        );
        config = json!({"flags":5,"ringradius":10,"ringwidth":4,"ringpulldistance":4,"ringpullforce":8,"speedinner":40,"speedouter":20});
        p.position = Vec3::X * 5.;
        let ring = Vortex::parse(&config).unwrap();
        assert_eq!(ring.value(p, &points, &silent), Vec3::ZERO);
        p.position = Vec3::X * 10.;
        assert_eq!(ring.value(p, &points, &silent), -Vec3::Y * 30.);
        p.position = Vec3::X * 14.;
        assert_eq!(ring.value(p, &points, &silent), Vec3::new(-4., -10., 0.));
        config["audioprocessingmode"] = json!(1);
        let stereo = Vortex::parse(&config).unwrap();
        assert!(stereo.needs_audio());
        assert_eq!(stereo.value(p, &points, &silent), Vec3::ZERO);
        let mut audio = silent;
        audio.bands[0].right.fill(1.);
        assert_eq!(stereo.value(p, &points, &audio), Vec3::ZERO);
        audio.bands[0].left.fill(1.);
        assert_eq!(stereo.value(p, &points, &audio), Vec3::new(-4., -10., 0.));
    }
}
