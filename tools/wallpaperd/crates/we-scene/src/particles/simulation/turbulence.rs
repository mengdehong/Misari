//! Turbulent birth direction and continuous force have distinct WE parameters.
use super::{Audio, Particle, lerp, scalar, vector};
use crate::audio::AudioSnapshot;
use anyhow::{Result, ensure};
use glam::{Quat, Vec3};
use serde_json::Value;
use std::f32::consts::PI;

#[derive(Clone)]
struct Field {
    time: f32,
    speed: [f32; 2],
    phase: [f32; 2],
    audio: Audio,
}
impl Field {
    fn parse(v: &Value, speed: [f32; 2], time: f32, phase_max: f32) -> Result<Self> {
        Ok(Self {
            time: scalar(v, "timescale", time)?,
            speed: [
                scalar(v, "speedmin", speed[0])?,
                scalar(v, "speedmax", speed[1])?,
            ],
            phase: [
                scalar(v, "phasemin", 0.)?,
                scalar(v, "phasemax", phase_max)?,
            ],
            audio: Audio::parse(v, false)?,
        })
    }
    fn phase(&self, p: &Particle, audio: &AudioSnapshot) -> f32 {
        // WE audio response modifies phase, not the velocity's magnitude. A
        // zero phase remains unaffected even when the monitor becomes loud.
        lerp(self.phase[0], self.phase[1], p.random.x) * self.audio.response(audio)
    }
    fn speed(&self, p: &Particle) -> f32 {
        lerp(self.speed[0], self.speed[1], p.random.y)
    }
    fn needs_audio(&self) -> bool {
        self.audio.mode != 0 && self.phase != [0.; 2] && self.speed != [0.; 2]
    }
}

#[derive(Clone)]
pub(super) struct Velocity {
    field: Field,
    spread: f32,
    offset: Quat,
    forward: Vec3,
    tangent: Vec3,
}
impl Velocity {
    pub(super) fn parse(v: &Value) -> Result<Self> {
        let forward = vector(v, "forward", Vec3::Y)?.normalize_or_zero();
        let normal = vector(v, "right", Vec3::Z)?.normalize_or_zero();
        ensure!(
            forward != Vec3::ZERO && normal != Vec3::ZERO,
            "zero turbulent direction"
        );
        let normal = (normal - forward * normal.dot(forward)).normalize_or_zero();
        ensure!(
            normal != Vec3::ZERO,
            "turbulent normal is parallel to forward"
        );
        Ok(Self {
            field: Field::parse(v, [100., 250.], 1., 0.1)?,
            spread: scalar(v, "scale", 1.)?.clamp(0., 2.) * PI * 0.5,
            offset: Quat::from_axis_angle(normal, scalar(v, "offset", 0.)?),
            forward,
            tangent: normal.cross(forward),
        })
    }
    pub(super) fn needs_audio(&self) -> bool {
        self.spread != 0. && self.field.needs_audio()
    }
    pub(super) fn value(&self, p: &Particle, time: f32, audio: &AudioSnapshot) -> Vec3 {
        let direction = if self.spread == 0. {
            self.forward
        } else {
            let phase = self.field.phase(p, audio);
            let sample = p.position * 0.1
                + Vec3::splat(time * self.field.time)
                + Vec3::new(phase, phase * 0.7, phase * 1.3);
            let noise = super::super::noise::curl(sample).normalize_or(self.forward);
            let cosine = noise.dot(self.forward).clamp(-1., 1.);
            if cosine.acos() > self.spread {
                let tangent = (noise - self.forward * cosine).normalize_or(self.tangent);
                self.forward * self.spread.cos() + tangent * self.spread.sin()
            } else {
                noise
            }
        };
        self.offset * direction * self.field.speed(p)
    }
}

#[derive(Clone)]
pub(super) struct Force {
    field: Field,
    scale: f32,
    mask: Vec3,
}
impl Force {
    pub(super) fn parse(v: &Value) -> Result<Self> {
        let mut field = Field::parse(v, [500., 1000.], 0.01, 0.)?;
        if v["audioprocessingfrequencyend"].is_null() {
            field.audio.frequency[1] = 15;
        }
        Ok(Self {
            field,
            scale: scalar(v, "scale", 0.005)? * 2.,
            mask: vector(v, "mask", Vec3::new(1., 1., 0.))?,
        })
    }
    pub(super) fn needs_audio(&self) -> bool {
        self.scale != 0. && self.mask != Vec3::ZERO && self.field.needs_audio()
    }
    pub(super) fn value(&self, p: &Particle, time: f32, audio: &AudioSnapshot) -> Vec3 {
        if self.mask == Vec3::ZERO || self.field.speed == [0.; 2] {
            return Vec3::ZERO;
        }
        let sample = (p.position + Vec3::X * (self.field.phase(p, audio) + time * self.field.time))
            * self.scale;
        super::super::noise::curl(sample).normalize_or_zero() * self.field.speed(p) * self.mask
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::particles::simulation::System;
    use serde_json::json;

    fn particle() -> Particle {
        let definition = json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2}]});
        let mut system = System::new(&definition, &json!({}), 19).unwrap();
        system.step_tick(0., &AudioSnapshot::default());
        system.particles.remove(0)
    }

    #[test]
    fn turbulent_birth_defaults_nonzero_speed_and_zero_spread_has_authored_direction() {
        let mut p = particle();
        let default = Velocity::parse(&json!({})).unwrap();
        let silent = AudioSnapshot::default();
        for time in [0., 0.25, 1., 9.] {
            p.position = Vec3::splat(time * 13.);
            let velocity = default.value(&p, time, &silent);
            assert!(velocity.is_finite());
            assert!((99.999..=250.001).contains(&velocity.length()));
            assert!(velocity.dot(Vec3::Y) >= -0.001);
        }
        let zero = Velocity::parse(&json!({"scale":0,"speedmin":16,"speedmax":16,"forward":"2 0 0","right":"0 0 3","audioprocessingmode":1})).unwrap();
        assert!(!zero.needs_audio());
        assert!((zero.value(&p, 100., &silent) - Vec3::X * 16.).length() < 0.0001);
        let offset = Velocity::parse(
            &json!({"scale":0,"speedmin":16,"speedmax":16,"offset":std::f32::consts::FRAC_PI_2}),
        )
        .unwrap();
        assert!((offset.value(&p, 0., &silent) + Vec3::X * 16.).length() < 0.0001);
        let normal = Velocity::parse(&json!({"scale":0,"speedmin":16,"speedmax":16,"forward":"1 0 0","right":"0 1 0","offset":std::f32::consts::FRAC_PI_2})).unwrap();
        assert!((normal.value(&p, 0., &silent) + Vec3::Z * 16.).length() < 0.0001);
        for value in [
            json!({"forward":"0 0 0"}),
            json!({"right":"0 1 0"}),
            json!({"right":"0 0 0"}),
            json!({"audioprocessingmode":1.5}),
        ] {
            assert!(Velocity::parse(&value).is_err());
        }
    }

    #[test]
    fn stereo_audio_changes_phase_without_muting_births_or_zero_phase_force() {
        let mut p = particle();
        p.position = Vec3::new(2., 3., 4.);
        p.random = Vec3::splat(0.5);
        let silent = AudioSnapshot::default();
        let mut left = silent.clone();
        left.bands[0].left.fill(1.);
        left.bands[0].average.fill(0.5);
        let mut right = silent.clone();
        right.bands[0].right.fill(1.);
        right.bands[0].average.fill(0.5);
        let settings = json!({"scale":2,"speedmin":32,"speedmax":32,"phasemin":3,"phasemax":3,"audioprocessingmode":1});
        let birth = Velocity::parse(&settings).unwrap();
        assert!(birth.needs_audio());
        let quiet = birth.value(&p, 0.7, &silent);
        let loud = birth.value(&p, 0.7, &left);
        assert!((quiet.length() - 32.).abs() < 0.0001);
        assert!((loud.length() - 32.).abs() < 0.0001);
        assert!((quiet - loud).length() > 1.);
        assert_eq!(quiet, birth.value(&p, 0.7, &right));
        let mut settings = settings;
        settings["audioprocessingmode"] = json!(2);
        let swapped = Velocity::parse(&settings).unwrap();
        assert_eq!(quiet, swapped.value(&p, 0.7, &left));
        assert_eq!(loud, swapped.value(&p, 0.7, &right));

        let mut settings =
            json!({"scale":0.3,"speedmin":40,"speedmax":40,"mask":"1 1 1","audioprocessingmode":1});
        let no_phase = Force::parse(&settings).unwrap();
        assert!(!no_phase.needs_audio());
        assert_eq!(
            no_phase.value(&p, 0.7, &silent),
            no_phase.value(&p, 0.7, &left)
        );
        assert!((no_phase.value(&p, 0.7, &silent).length() - 40.).abs() < 0.0001);
        settings["phasemax"] = json!(4);
        let phase = Force::parse(&settings).unwrap();
        assert!(phase.needs_audio());
        assert!((phase.value(&p, 0.7, &silent) - phase.value(&p, 0.7, &left)).length() > 1.);
        settings["mask"] = json!("0 0 0");
        let disabled = Force::parse(&settings).unwrap();
        assert!(!disabled.needs_audio());
        assert_eq!(disabled.value(&p, 0.7, &left), Vec3::ZERO);
        let default = Force::parse(&json!({})).unwrap().value(&p, 0., &silent);
        assert_eq!(default.z, 0.);
        assert!(default.length() > 100.);
    }

    #[test]
    fn turbulent_system_is_deterministic_across_steps_and_audio_only_affects_new_births() {
        let definition = json!({"maxcount":16,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":4}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"turbulentvelocityrandom","scale":2,"speedmin":8,"speedmax":8,"phasemin":3,"phasemax":3,"audioprocessingmode":1}],"operator":[{"name":"movement"}]});
        let mut a = System::new(&definition, &json!({}), 31).unwrap();
        let mut b = a.clone();
        let silent = AudioSnapshot::default();
        let mut loud = silent.clone();
        loud.bands[0].left.fill(1.);
        a.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &silent);
        b.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &silent);
        let original = a.particles[0].velocity;
        for time in [0.1, 0.2, 0.3, 0.4, 0.5] {
            a.advance(time, Vec3::ZERO, glam::Mat4::IDENTITY, &loud);
        }
        b.advance(0.5, Vec3::ZERO, glam::Mat4::IDENTITY, &loud);
        assert_eq!(a.particles, b.particles);
        assert_eq!(a.particles[0].velocity, original);
        assert!(a.particles.len() >= 2);
        assert_ne!(a.particles[1].velocity, original);
        let mut quiet = System::new(&definition, &json!({}), 31).unwrap();
        quiet.advance(0.5, Vec3::ZERO, glam::Mat4::IDENTITY, &silent);
        assert_ne!(a.particles[1].velocity, quiet.particles[1].velocity);
    }
}
