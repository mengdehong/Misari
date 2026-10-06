//! Parsed scalar/vector remaps shared by initializers and continuous operators.
use super::{Audio, Particle, ramp, scalar, vector};
use crate::audio::AudioSnapshot;
use anyhow::{Context, Result, bail, ensure};
use glam::Vec3;
use serde_json::Value;

#[derive(Clone)]
enum Input {
    Lifetime,
    Age,
    Time,
    Random,
    Size,
    Opacity,
    Color,
    Position,
    Velocity,
    Speed,
    Rotation,
    Angular,
    Life,
    Distance(usize),
    Audio(Audio),
}
#[derive(Clone, Copy)]
enum Output {
    Size,
    Opacity,
    Color,
    Position,
    Velocity,
    Speed,
    Rotation,
    Angular,
    Life,
}
#[derive(Clone, Copy)]
enum Operation {
    Assign,
    Multiply,
    Add,
    Subtract,
}
#[derive(Clone, Copy)]
enum Transform {
    Identity,
    Sine,
    Cosine,
    Simplex,
    Fbm,
    Smooth,
}
#[derive(Clone)]
pub(super) struct Remap {
    input: Input,
    output: Output,
    operation: Operation,
    transform: Transform,
    input_range: [Vec3; 2],
    output_range: [Vec3; 2],
    scale: Vec3,
    offset: Vec3,
    octaves: u8,
    flags: u32,
    blend: [f32; 4],
}
fn selection<'a>(v: &'a Value, key: &str, default: &'a str) -> Result<&'a str> {
    if v[key].is_null() {
        Ok(default)
    } else {
        v[key]
            .as_str()
            .with_context(|| format!("particle remap {key} must be a string"))
    }
}
impl Remap {
    pub(super) fn parse(v: &Value) -> Result<Self> {
        let input = match selection(v, "input", "lifetime")? {
            "lifetime" | "particlelifetime" => Input::Lifetime,
            "age" | "particleage" => Input::Age,
            "particlesystemtime" | "time" => Input::Time,
            "random" => Input::Random,
            "size" => Input::Size,
            "opacity" | "alpha" => Input::Opacity,
            "color" => Input::Color,
            "position" => Input::Position,
            "velocity" => Input::Velocity,
            "speed" => Input::Speed,
            "rotation" => Input::Rotation,
            "angularvelocity" => Input::Angular,
            "totallifetime" => Input::Life,
            "distancetocontrolpoint" => {
                Input::Distance(super::control_point_index(v, "inputcontrolpoint0")?)
            }
            "audio" => {
                let mut audio = Audio::parse(v, false)?;
                if audio.mode == 0 {
                    audio.mode = 3;
                }
                Input::Audio(audio)
            }
            name => bail!("unsupported particle remap input {name}"),
        };
        let output = match selection(v, "output", "size")? {
            "size" => Output::Size,
            "opacity" | "alpha" => Output::Opacity,
            "color" => Output::Color,
            "position" => Output::Position,
            "velocity" => Output::Velocity,
            "speed" => Output::Speed,
            "rotation" => Output::Rotation,
            "angularvelocity" => Output::Angular,
            "lifetime" => Output::Life,
            name => bail!("unsupported particle remap output {name}"),
        };
        let operation = match selection(v, "operation", "multiply")? {
            "remap" | "assign" => Operation::Assign,
            "multiply" => Operation::Multiply,
            "add" => Operation::Add,
            "subtract" => Operation::Subtract,
            name => bail!("unsupported particle remap operation {name}"),
        };
        let transform = match selection(v, "transformfunction", "none")? {
            "none" | "identity" => Transform::Identity,
            "sine" => Transform::Sine,
            "cosine" => Transform::Cosine,
            "simplexnoise" => Transform::Simplex,
            "fbmnoise" => Transform::Fbm,
            "smoothstep" => Transform::Smooth,
            name => bail!("unsupported particle remap transform {name}"),
        };
        let octaves = scalar(v, "transformoctaves", 4.)?;
        ensure!(
            (1.0..=8.).contains(&octaves) && octaves.fract() == 0.,
            "invalid remap noise octaves"
        );
        Ok(Self {
            input,
            output,
            operation,
            transform,
            input_range: [
                vector(v, "inputrangemin", Vec3::ZERO)?,
                vector(v, "inputrangemax", Vec3::ONE)?,
            ],
            output_range: [
                vector(v, "outputrangemin", Vec3::ZERO)?,
                vector(v, "outputrangemax", Vec3::ONE)?,
            ],
            scale: vector(v, "transforminputscale", Vec3::ONE)?,
            offset: vector(v, "transforminputoffset", Vec3::ZERO)?,
            octaves: octaves as u8,
            flags: if v["flags"].is_null() {
                3
            } else {
                super::super::animation::flags(v)?
            },
            blend: [
                scalar(v, "blendinstart", 0.)?,
                scalar(v, "blendinend", 0.)?,
                scalar(v, "blendoutstart", 1.)?,
                scalar(v, "blendoutend", 1.)?,
            ],
        })
    }
    pub(super) fn needs_audio(&self) -> bool {
        matches!(self.input, Input::Audio(_))
    }
    pub(super) fn apply(
        &self,
        p: &mut Particle,
        time: f32,
        points: &[Vec3; 8],
        audio: &AudioSnapshot,
        initial: bool,
    ) {
        let source = match &self.input {
            Input::Lifetime => Vec3::splat(p.age / p.life),
            Input::Age => Vec3::splat(p.age),
            Input::Time => Vec3::splat(time),
            Input::Random => p.random,
            Input::Size => Vec3::splat(p.size),
            Input::Opacity => Vec3::splat(p.color.w),
            Input::Color => p.color.truncate(),
            Input::Position => p.position,
            Input::Velocity => p.velocity,
            Input::Speed => Vec3::splat(p.velocity.length()),
            Input::Rotation => p.rotation,
            Input::Angular => p.angular,
            Input::Life => Vec3::splat(p.life),
            Input::Distance(cp) => Vec3::splat((p.position - points[*cp]).length()),
            Input::Audio(a) => Vec3::splat(a.response(audio)),
        };
        let [low, high] = self.input_range;
        let mut normalized = Vec3::from_array(std::array::from_fn(|axis| {
            let range = high[axis] - low[axis];
            if range.abs() > f32::EPSILON {
                (source[axis] - low[axis]) / range
            } else if source[axis] >= high[axis] {
                1.
            } else {
                0.
            }
        }));
        if self.flags & 1 != 0 {
            normalized = normalized.clamp(Vec3::ZERO, Vec3::ONE);
        }
        let argument = normalized * self.scale + self.offset;
        let transformed = match self.transform {
            Transform::Identity => argument,
            Transform::Sine => Vec3::from_array(argument.to_array().map(|x| (x.sin() + 1.) * 0.5)),
            Transform::Cosine => {
                Vec3::from_array(argument.to_array().map(|x| (x.cos() + 1.) * 0.5))
            }
            Transform::Simplex => {
                (super::super::noise::simplex_field(argument + p.random * 256.) + Vec3::ONE) * 0.5
            }
            Transform::Fbm => {
                (super::super::noise::fbm_field(argument + p.random * 256., self.octaves)
                    + Vec3::ONE)
                    * 0.5
            }
            Transform::Smooth => {
                let x = argument.clamp(Vec3::ZERO, Vec3::ONE);
                x * x * (Vec3::splat(3.) - 2. * x)
            }
        };
        let [low, high] = self.output_range;
        let mut value = low + (high - low) * transformed;
        if self.flags & 2 != 0 {
            value = value.clamp(low.min(high), low.max(high));
        }
        let current = match self.output {
            Output::Size => Vec3::splat(p.size),
            Output::Opacity => Vec3::splat(p.color.w),
            Output::Color => p.color.truncate(),
            Output::Position => p.position,
            Output::Velocity => p.velocity,
            Output::Speed => Vec3::splat(p.velocity.length()),
            Output::Rotation => p.rotation,
            Output::Angular => p.angular,
            Output::Life => Vec3::splat(p.life),
        };
        let value = match self.operation {
            Operation::Assign => value,
            Operation::Multiply => current * value,
            Operation::Add => current + value,
            Operation::Subtract => current - value,
        };
        let life = p.age / p.life;
        let weight = if initial {
            1.
        } else {
            ramp(life, self.blend[0], self.blend[1])
                * (1. - ramp(life, self.blend[2], self.blend[3]))
        };
        let value = current.lerp(value, weight);
        match self.output {
            Output::Size => p.size = value.x.max(0.),
            Output::Opacity => p.color.w = value.x.clamp(0., 1.),
            Output::Color => p.color = value.extend(p.color.w),
            Output::Position => p.position = value,
            Output::Velocity => p.velocity = value,
            Output::Speed => p.velocity = p.velocity.normalize_or_zero() * value.x,
            Output::Rotation => p.rotation = value,
            Output::Angular => p.angular = value,
            Output::Life => p.life = value.x.clamp(super::STEP as f32, 3600.),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::particles::simulation::System;
    use serde_json::json;

    fn particle() -> Particle {
        let mut system=System::new(&json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"sizerandom","min":10,"max":10},{"name":"alpharandom","min":0.5,"max":0.5},{"name":"velocityrandom","min":"4 0 0","max":"4 0 0"}]}),&Value::Null,19).unwrap();
        system.step_tick(0., &AudioSnapshot::default());
        system.particles.remove(0)
    }
    #[test]
    fn distance_scalar_and_vector_operations_clamps_and_lifetime_blend_have_real_values() {
        let mut p = particle();
        let audio = AudioSnapshot::default();
        let mut points = [Vec3::ZERO; 8];
        points[1] = Vec3::X * 16.;
        let mut settings = json!({"input":"distancetocontrolpoint","inputcontrolpoint0":1,"inputrangemax":32,"output":"color","outputrangemin":"1 0 0","outputrangemax":"0 0 1","operation":"remap"});
        Remap::parse(&settings)
            .unwrap()
            .apply(&mut p, 0., &points, &audio, true);
        assert_eq!(p.color, glam::Vec4::new(0.5, 0., 0.5, 0.5));
        p.position = Vec3::X * 64.;
        settings["flags"] = json!(0);
        settings["inputrangemax"] = json!(16);
        Remap::parse(&settings)
            .unwrap()
            .apply(&mut p, 0., &points, &audio, true);
        assert_eq!(p.color, glam::Vec4::new(-2., 0., 3., 0.5));
        settings["flags"] = json!(3);
        Remap::parse(&settings)
            .unwrap()
            .apply(&mut p, 0., &points, &audio, true);
        assert_eq!(p.color, glam::Vec4::new(0., 0., 1., 0.5));
        p.size = 10.;
        let mut settings = json!({"input":"size","inputrangemax":20,"output":"size","outputrangemin":0.5,"outputrangemax":2});
        Remap::parse(&settings)
            .unwrap()
            .apply(&mut p, 0., &points, &audio, true);
        assert_eq!(p.size, 12.5);
        settings["operation"] = json!("add");
        settings["outputrangemin"] = json!(5);
        settings["outputrangemax"] = json!(5);
        Remap::parse(&settings)
            .unwrap()
            .apply(&mut p, 0., &points, &audio, true);
        assert_eq!(p.size, 17.5);
        settings["operation"] = json!("subtract");
        Remap::parse(&settings)
            .unwrap()
            .apply(&mut p, 0., &points, &audio, true);
        assert_eq!(p.size, 12.5);
        let blend=Remap::parse(&json!({"output":"size","operation":"remap","outputrangemin":40,"outputrangemax":40,"blendinstart":0.2,"blendinend":0.4,"blendoutstart":0.6,"blendoutend":0.8})).unwrap();
        p.size = 10.;
        p.age = 0.3;
        blend.apply(&mut p, 0., &points, &audio, false);
        assert!((p.size - 25.).abs() < 0.00001);
        p.size = 10.;
        p.age = 0.9;
        blend.apply(&mut p, 0., &points, &audio, false);
        assert_eq!(p.size, 10.);
        let audio_remap = Remap::parse(
        &json!({"input":"audio","audioprocessingmode":1,"output":"opacity","operation":"remap"}),
    )
    .unwrap();
        assert!(audio_remap.needs_audio());
        let mut left = audio;
        left.bands[0].left.fill(1.);
        audio_remap.apply(&mut p, 0., &points, &left, true);
        assert_eq!(p.color.w, 1.);
        for invalid in [
            json!({"inputcontrolpoint0":8,"input":"distancetocontrolpoint"}),
            json!({"output":"external-path"}),
            json!({"transformfunction":"external-script"}),
            json!({"operation":true}),
            json!({"transformoctaves":10000}),
        ] {
            assert!(Remap::parse(&invalid).is_err());
        }
    }
    #[test]
    fn initial_remap_freezes_births_and_noise_operators_are_seeded_across_frame_rates() {
        let audio = AudioSnapshot::default();
        let definition = json!({"maxcount":4,"controlpoint":[{"id":1,"offset":"16 0 0"}],"emitter":[{"name":"boxrandom","instantaneous":1,"rate":4}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"remapinitialvalue","input":"distancetocontrolpoint","inputcontrolpoint0":1,"inputrangemax":32,"output":"color","outputrangemin":"1 0 0","outputrangemax":"0 0 1","operation":"remap"}]});
        let mut system = System::new(&definition, &Value::Null, 7).unwrap();
        system.advance(0., Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        assert_eq!(system.particles[0].color, glam::Vec4::new(0.5, 0., 0.5, 1.));
        system
            .set_overrides(&json!({"controlpoint1":"32 0 0"}))
            .unwrap();
        system.advance(0.25, Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
        assert_eq!(system.particles[0].color, glam::Vec4::new(0.5, 0., 0.5, 1.));
        assert_eq!(system.particles[1].color, glam::Vec4::new(0., 0., 1., 1.));
        for transform in ["simplexnoise", "fbmnoise", "sine"] {
            let definition = json!({"maxcount":8,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":4}],"initializer":[{"name":"lifetimerandom","min":2,"max":2}],"operator":[{"name":"remapvalue","input":"particlesystemtime","output":"velocity","operation":"remap","outputrangemin":"-10 -10 -10","outputrangemax":"10 10 10","transformfunction":transform,"transforminputscale":6,"flags":0},{"name":"movement"}]});
            let mut a = System::new(&definition, &Value::Null, 7).unwrap();
            let mut b = a.clone();
            for time in [0.1, 0.2, 0.3, 0.4, 0.5] {
                a.advance(time, Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
            }
            b.advance(0.5, Vec3::ZERO, glam::Mat4::IDENTITY, &audio);
            assert_eq!(a.particles, b.particles);
            assert!(
                a.particles
                    .iter()
                    .all(|p| p.velocity.is_finite() && p.velocity.abs().max_element() <= 10.)
            );
        }
    }
}
