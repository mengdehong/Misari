//! Bounded event snapshots survive parent death; live sources refresh separately.
use super::Particle;
use anyhow::{Result, bail};
use glam::{Mat4, Vec3, Vec4};
use serde_json::Value;

#[derive(Clone, Copy)]
pub(in crate::particles) struct Event {
    pub id: u64,
    pub values: Values,
}
#[derive(Clone, Copy)]
pub(in crate::particles) struct Values {
    pub position: Vec3,
    velocity: Vec3,
    rotation: Vec3,
    angular: Vec3,
    color: Vec4,
    size: f32,
    life: f32,
}
impl Event {
    pub(in crate::particles) fn from(p: &Particle) -> Self {
        Self {
            id: p.id,
            values: Values::from(p),
        }
    }
}
impl Values {
    pub(in crate::particles) fn from(p: &Particle) -> Self {
        Self {
            position: p.position,
            velocity: p.velocity,
            rotation: p.rotation,
            angular: p.angular,
            color: p.color,
            size: p.size,
            life: p.life,
        }
    }
    pub(super) fn transformed(mut self, matrix: Mat4) -> Self {
        self.position = matrix.transform_point3(self.position);
        self.velocity = matrix.transform_vector3(self.velocity);
        self
    }
}
#[derive(Clone, Copy)]
pub(in crate::particles) enum Field {
    Color,
    Opacity,
    Size,
    Position,
    Velocity,
    Speed,
    Rotation,
    Angular,
    Life,
}
impl Field {
    pub(super) fn parse(v: &Value) -> Result<Self> {
        let input = if v["input"].is_null() {
            "color"
        } else {
            v["input"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("particle event input must be a string"))?
        };
        Ok(match input {
            "color" => Self::Color,
            "opacity" | "alpha" => Self::Opacity,
            "size" => Self::Size,
            "position" => Self::Position,
            "velocity" => Self::Velocity,
            "speed" => Self::Speed,
            "rotation" => Self::Rotation,
            "angularvelocity" => Self::Angular,
            "lifetime" => Self::Life,
            _ => bail!("unsupported particle event input {input}"),
        })
    }
    pub(super) fn apply(self, p: &mut Particle, values: Option<Values>) {
        let Some(values) = values else { return };
        match self {
            Self::Color => p.color = values.color.truncate().extend(p.color.w),
            Self::Opacity => p.color.w = values.color.w,
            Self::Size => p.size = values.size,
            Self::Position => p.position = values.position,
            Self::Velocity => p.velocity = values.velocity,
            Self::Speed => p.velocity = p.velocity.normalize_or_zero() * values.velocity.length(),
            Self::Rotation => p.rotation = values.rotation,
            Self::Angular => p.angular = values.angular,
            Self::Life => p.life = values.life,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{audio::AudioSnapshot, particles::simulation::System};
    use serde_json::json;
    #[test]
    fn color_is_rgb_opacity_is_separate_and_invalid_event_selectors_are_rejected() {
        let definition =
            json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}]});
        let mut system = System::new(&definition, &Value::Null, 7).unwrap();
        system.step_tick(0., &AudioSnapshot::default());
        let mut p = system.particles.remove(0);
        p.color = Vec4::new(1., 0., 0., 0.25);
        let source = Values::from(&p);
        p.color = Vec4::new(0., 1., 0., 0.5);
        Field::Color.apply(&mut p, Some(source));
        assert_eq!(p.color, Vec4::new(1., 0., 0., 0.5));
        Field::Opacity.apply(&mut p, Some(source));
        assert_eq!(p.color, Vec4::new(1., 0., 0., 0.25));
        let previous = p.clone();
        Field::Color.apply(&mut p, None);
        assert_eq!(p, previous);
        assert!(Field::parse(&json!({"input":true})).is_err());
        assert!(Field::parse(&json!({"input":"outside-object"})).is_err());
    }
}
