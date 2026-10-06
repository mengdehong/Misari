//! Per-instance spring/rigid bone constraints from compiled MDLS metadata.
//! The field names are shared by the official samurai physics sample.
use super::*;
use anyhow::{Result, ensure};
use glam::{EulerRot, Quat};
use serde_json::Value;

#[derive(Clone)]
struct Constraint {
    spring: bool,
    rotation: bool,
    translation: bool,
    rs: f32,
    rf: f32,
    ri: f32,
    ts: f32,
    tf: f32,
    ti: f32,
    gravity: Vec3,
    tip: Vec3,
    mass: f32,
    limit_angle: Option<(Vec3, Vec3)>,
    limit_distance: Option<f32>,
}
#[derive(Clone)]
struct Motion {
    position: Vec3,
    rotation: Quat,
    velocity: Vec3,
    angular: Vec3,
    anchor: Vec3,
    anchor_velocity: Vec3,
    command: u64,
    reset_command: u64,
    command_time: Option<f64>,
    direction: Vec3,
    impulse_angle: Vec3,
    reset: bool,
}
#[derive(Clone)]
pub(super) struct Physics {
    constraints: Vec<Option<Constraint>>,
    motion: Vec<Option<Motion>>,
    pub active: bool,
}
fn number(v: &Value, key: &str, default: f32) -> Result<f32> {
    let n = crate::scene::bindings::components(&v[key], &[default], 1)?[0];
    ensure!((0. ..=1e6).contains(&n), "invalid bone physics {key}");
    Ok(n)
}
fn vector(v: &Value, key: &str, default: Vec3) -> Result<Vec3> {
    let value = Vec3::from_slice(&crate::scene::bindings::components(
        &v[key],
        &default.to_array(),
        3,
    )?);
    ensure!(
        value.abs().max_element() <= 1e6,
        "excessive bone physics {key}"
    );
    Ok(value)
}
impl Physics {
    pub fn new(model: &Model) -> Result<Self> {
        let constraints = model
            .bones
            .iter()
            .map(|bone| {
                let v = &bone.simulation;
                if v["se"] != true && v["re"] != true {
                    return Ok(None);
                }
                let rotation = v["r"] == true;
                let translation = v["t"] == true;
                if !rotation && !translation {
                    return Ok(None);
                }
                let tip = vector(v, "tp", Vec3::X * 100.)?;
                let size = number(v, "s", 0.)?;
                let axis = vector(v, "a", tip.normalize_or(Vec3::X))?.normalize_or(Vec3::X);
                let limit_angle = if v["la"] == true {
                    let min = vector(v, "lamin", Vec3::splat(-std::f32::consts::PI))?;
                    let max = vector(v, "lamax", Vec3::splat(std::f32::consts::PI))?;
                    ensure!(min.cmple(max).all(), "invalid bone physics angle limits");
                    Some((min, max))
                } else {
                    None
                };
                Ok(Some(Constraint {
                    spring: v["se"] == true,
                    rotation,
                    translation,
                    rs: number(v, "rs", 200.)?,
                    rf: number(v, "rf", 10.)?,
                    ri: number(v, "ri", 30.)?,
                    ts: number(v, "ts", 200.)?,
                    tf: number(v, "tf", 10.)?,
                    ti: number(v, "ti", 30.)?,
                    gravity: if v["ge"] == true {
                        vector(v, "gd", -Vec3::Y)?
                    } else {
                        Vec3::ZERO
                    },
                    tip: axis
                        * if size > 0. {
                            size
                        } else {
                            tip.length().max(0.01)
                        },
                    mass: number(v, "m", 20.)?,
                    limit_angle,
                    limit_distance: if v["lt"] == true {
                        Some(number(v, "ltmax", 100.)?)
                    } else {
                        None
                    },
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let active = constraints.iter().any(Option::is_some);
        Ok(Self {
            motion: if active {
                vec![None; constraints.len()]
            } else {
                Vec::new()
            },
            constraints: if active { constraints } else { Vec::new() },
            active,
        })
    }
    pub fn reset(&mut self) {
        for state in self.motion.iter_mut().flatten() {
            state.reset = true;
        }
    }
    pub fn apply(
        &mut self,
        dt: f32,
        space: Mat4,
        local: &mut [Mat4],
        order: &[usize],
        model: &Model,
        object: &Value,
    ) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        ensure!(space.is_finite(), "non-finite bone physics layer transform");
        let mut world = vec![Mat4::IDENTITY; local.len()];
        for &i in order {
            let parent = model.bones[i].parent.map_or(space, |p| world[p]);
            let target = parent * local[i];
            let Some(c) = &self.constraints[i] else {
                world[i] = target;
                continue;
            };
            if parent.determinant().abs() <= 1e-12 || target.determinant().abs() <= 1e-12 {
                // Zero scale can temporarily hide a layer or an animated bone.
                // Preserve its authored pose and restart when it is invertible.
                if let Some(state) = &mut self.motion[i] {
                    state.reset = true;
                }
                world[i] = target;
                continue;
            }
            let (_, rotation, position) = target.to_scale_rotation_translation();
            let command = &object["__bonePhysics"][i.to_string()];
            let revision = command["revision"].as_u64().unwrap_or(0);
            let state = self.motion[i].get_or_insert(Motion {
                position,
                rotation,
                velocity: Vec3::ZERO,
                angular: Vec3::ZERO,
                anchor: position,
                anchor_velocity: Vec3::ZERO,
                command: 0,
                reset_command: 0,
                command_time: None,
                direction: Vec3::ZERO,
                impulse_angle: Vec3::ZERO,
                reset: false,
            });
            let reset_command = command["resetRevision"].as_u64().unwrap_or(0);
            let reset = state.reset || reset_command != state.reset_command;
            if reset {
                state.position = position;
                state.rotation = rotation;
                state.velocity = Vec3::ZERO;
                state.angular = Vec3::ZERO;
                state.anchor = position;
                state.anchor_velocity = Vec3::ZERO;
                state.reset = false;
                if reset_command != state.reset_command {
                    state.command_time = None;
                }
                state.reset_command = reset_command;
            }
            if revision != state.command {
                let time = command["time"].as_f64();
                let direction = vector(command, "direction", Vec3::ZERO)?;
                let angle = vector(command, "angular", Vec3::ZERO)?;
                let same = time.is_some() && time == state.command_time;
                state.velocity += direction - if same { state.direction } else { Vec3::ZERO };
                state.angular += angle
                    - if same {
                        state.impulse_angle
                    } else {
                        Vec3::ZERO
                    };
                state.direction = direction;
                state.impulse_angle = angle;
                state.command_time = time;
                state.command = revision;
            }
            let velocity = if dt > 0. {
                (position - state.anchor) / dt
            } else {
                state.anchor_velocity
            };
            let acceleration = if dt > 0. {
                (velocity - state.anchor_velocity) / dt
            } else {
                Vec3::ZERO
            };
            // Bounded semi-implicit integration is an internal solver choice.
            // More than 250ms is treated as a seek, not unbounded catch-up.
            let steps = (dt * 120.).ceil().max(1.) as usize;
            let h = dt / steps as f32;
            for _ in 0..steps {
                if c.translation {
                    let force = if c.spring {
                        (position - state.position) * c.ts
                    } else {
                        Vec3::ZERO
                    };
                    state.velocity += (force + c.gravity * c.mass) / (1. + c.ti) * h;
                    state.velocity *= (-c.tf * h).exp();
                    state.position += state.velocity * h;
                } else {
                    state.position = position;
                }
                if c.rotation {
                    let mut error = rotation * state.rotation.conjugate();
                    if error.w < 0. {
                        error = -error;
                    }
                    let error = error.to_scaled_axis();
                    let tip = state.rotation * c.tip;
                    let lever = tip.length_squared().max(0.0001);
                    let torque = tip.cross(c.gravity * c.mass - acceleration) / lever;
                    let spring = if c.spring { error * c.rs } else { Vec3::ZERO };
                    state.angular += (spring + torque) / (1. + c.ri) * h;
                    state.angular *= (-c.rf * h).exp();
                    state.rotation =
                        (Quat::from_scaled_axis(state.angular * h) * state.rotation).normalize();
                } else {
                    state.rotation = rotation;
                }
            }
            if dt > 0. {
                state.anchor = position;
                state.anchor_velocity = velocity;
            }
            let target_rotation = target.to_scale_rotation_translation().1;
            let inverse = parent.inverse();
            let pose = local[i];
            let (scale, base_rotation, base_position) = pose.to_scale_rotation_translation();
            let mut relative = (target_rotation.conjugate() * state.rotation).normalize();
            if let Some((min, max)) = c.limit_angle {
                let (z, y, x) = relative.to_euler(EulerRot::ZYX);
                let angles = Vec3::new(x, y, z);
                let limited = angles.clamp(min, max);
                if limited != angles {
                    state.angular = Vec3::ZERO;
                }
                relative = Quat::from_euler(EulerRot::ZYX, limited.z, limited.y, limited.x);
                state.rotation = (target_rotation * relative).normalize();
            }
            let mut origin = inverse.transform_point3(state.position);
            if let Some(limit) = c.limit_distance {
                let delta = origin - base_position;
                if delta.length() > limit {
                    origin = base_position + delta.normalize() * limit;
                    state.position = parent.transform_point3(origin);
                    state.velocity = Vec3::ZERO;
                }
            }
            local[i] = Mat4::from_scale_rotation_translation(
                scale,
                (base_rotation * relative).normalize(),
                origin,
            );
            world[i] = parent * local[i];
            ensure!(
                world[i].is_finite() && state.velocity.is_finite() && state.angular.is_finite(),
                "bone physics overflows"
            );
        }
        Ok(())
    }
}
pub(super) fn validate_commands(object: &Value) -> Result<()> {
    let Some(commands) = object["__bonePhysics"].as_object() else {
        ensure!(
            object["__bonePhysics"].is_null(),
            "invalid bone physics commands"
        );
        return Ok(());
    };
    let bones = object["__model"]["bones"].as_array().map_or(0, Vec::len);
    ensure!(
        commands.len() <= bones,
        "bone physics command budget exceeded"
    );
    for (key, v) in commands {
        ensure!(
            key.parse::<usize>().ok().is_some_and(|n| n < bones),
            "unknown physics bone"
        );
        ensure!(
            v["revision"].as_u64().is_some(),
            "invalid physics command revision"
        );
        ensure!(
            v["resetRevision"].is_null() || v["resetRevision"].as_u64().is_some(),
            "invalid physics reset revision"
        );
        ensure!(
            v["time"].as_f64().is_some_and(f64::is_finite),
            "invalid physics command time"
        );
        for key in ["direction", "angular"] {
            let value = vector(v, key, Vec3::ZERO)?;
            ensure!(
                value.abs().max_element() <= 1e6,
                "excessive bone physics impulse"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::rc::Rc;
    fn model(constraint: Value) -> Rc<Model> {
        let mut model = super::super::tests::fixture();
        let model_mut = Rc::get_mut(&mut model).unwrap();
        model_mut.clips.clear();
        model_mut.bones[0].simulation = constraint;
        model
    }
    #[test]
    fn spring_rigid_impulses_world_motion_limits_reset_and_rewind() {
        let mut rig = Rig::new(model(
            json!({"se":true,"t":true,"ts":200,"ti":0,"tf":2,"lt":true,"ltmax":3}),
        ))
        .unwrap();
        let mut object = json!({"__bonePhysics":{"0":{"revision":1,"time":0,"direction":[20,0,0],"angular":[0,0,0]}}});
        rig.advance(0., &object).unwrap();
        rig.advance(0.1, &object).unwrap();
        let x = rig.local[0].w_axis.x;
        assert!(x > 2.1 && x <= 5., "spring impulse: {x}");
        let vertices = rig.vertices(0);
        let emission = rig.emission_positions();
        assert!(emission[0].abs_diff_eq(Vec3::from_slice(&vertices), 1e-5));
        assert!(!rig.advance(0.1, &object).unwrap());
        assert_eq!(rig.vertices(0), vertices);
        object["__bonePhysics"]["0"] = json!({"revision":2,"resetRevision":2,"time":0.1,"direction":[0,0,0],"angular":[0,0,0]});
        rig.advance(0.1, &object).unwrap();
        assert_eq!(rig.local[0].w_axis.x, 2.);
        rig.advance(0., &object).unwrap();
        assert_eq!(rig.local[0].w_axis.x, 2.);

        let mut rigid = Rig::new(model(json!({"re":true,"t":true,"tf":0}))).unwrap();
        let none = json!({});
        rigid.advance(0., &none).unwrap();
        let space = Mat4::from_scale_rotation_translation(
            Vec3::new(2., 3., 1.),
            Quat::IDENTITY,
            Vec3::new(10., 0., 0.),
        );
        rigid.advance_in_space(0.1, &none, space).unwrap();
        let bone = space * rigid.world[0];
        assert!(
            (bone.w_axis.x - 7.).abs() < 1e-5,
            "rigid must retain world position"
        );
        assert!(rigid.animated());

        let mut angular=Rig::new(model(json!({"re":true,"r":true,"rf":0,"ri":0,"la":true,"lamin":[0,0,-0.2],"lamax":[0,0,0.2]}))).unwrap();
        let impulse = json!({"__bonePhysics":{"0":{"revision":1,"time":0,"direction":[0,0,0],"angular":[0,0,10]}}});
        angular.advance(0., &impulse).unwrap();
        angular.advance(0.1, &impulse).unwrap();
        let (_, _, z) = angular.local[0]
            .to_scale_rotation_translation()
            .1
            .to_euler(EulerRot::XYZ);
        assert!((z - 0.2).abs() < 1e-5);
    }
    #[test]
    fn accumulated_same_frame_impulses_apply_each_increment_once() {
        let mut rig = Rig::new(model(json!({"re":true,"t":true,"tf":0,"ti":0}))).unwrap();
        let mut object = json!({"__bonePhysics":{"0":{"revision":1,"time":0,"direction":[10,0,0],"angular":[0,0,0]}}});
        rig.advance(0., &object).unwrap();
        object["__bonePhysics"]["0"]["revision"] = json!(2);
        object["__bonePhysics"]["0"]["direction"] = json!([20, 0, 0]);
        rig.advance(0., &object).unwrap();
        rig.advance(0.1, &object).unwrap();
        assert!(
            (rig.local[0].w_axis.x - 4.).abs() < 1e-5,
            "same-frame impulse was applied twice"
        );
    }
    #[test]
    fn gravity_and_parent_animation_feed_rotation_and_bad_constraints_fail() {
        let m = model(
            json!({"re":true,"r":true,"rf":1,"ri":0,"ge":true,"gd":[0,-1,0],"m":20,"tp":[5,0,0]}),
        );
        let mut rig = Rig::new(m).unwrap();
        rig.advance(0., &json!({})).unwrap();
        rig.advance(0.1, &json!({})).unwrap();
        assert!(rig.local[0].x_axis.y < 0., "gravity did not rotate the tip");
        let m = model(json!({"se":true,"r":true,"rf":0,"ri":0}));
        let mut physics = Physics::new(&m).unwrap();
        let mut local = m.bones.iter().map(|b| b.local).collect::<Vec<_>>();
        physics
            .apply(0., Mat4::IDENTITY, &mut local, &[1, 0], &m, &json!({}))
            .unwrap();
        physics.motion[0].as_mut().unwrap().rotation = -Quat::IDENTITY;
        physics
            .apply(0.1, Mat4::IDENTITY, &mut local, &[1, 0], &m, &json!({}))
            .unwrap();
        assert_eq!(
            physics.motion[0].as_ref().unwrap().angular,
            Vec3::ZERO,
            "equivalent quaternion caused a full-turn spring force"
        );
        let m = model(json!({"re":true,"r":true,"rf":0,"ri":0,"tp":[5,0,0]}));
        let mut rig = Rig::new(m.clone()).unwrap();
        let mut independent = Rig::new(m).unwrap();
        rig.advance(0., &json!({})).unwrap();
        independent.advance(0., &json!({})).unwrap();
        let moved = Mat4::from_translation(Vec3::Y * 10.);
        rig.advance_in_space(0., &json!({}), moved).unwrap();
        rig.advance_in_space(0.1, &json!({}), moved).unwrap();
        independent.advance(0.1, &json!({})).unwrap();
        assert!(
            rig.local[0].x_axis.y < 0.,
            "script-time parent motion lost its inertial force"
        );
        assert_eq!(
            independent.local[0].x_axis,
            Vec4::X,
            "shared MDL shares physics state"
        );
        rig.advance_in_space(0.1, &json!({}), Mat4::from_scale(Vec3::ZERO))
            .unwrap();
        rig.advance_in_space(0.2, &json!({}), moved).unwrap();
        assert_eq!(
            rig.local[0].x_axis,
            Vec4::X,
            "singular transform did not restart at its authored pose"
        );
        assert!(Rig::new(model(json!({"se":true,"t":true,"tf":-1}))).is_err());
        assert!(
            Rig::new(model(
                json!({"re":true,"r":true,"la":true,"lamin":[1,0,0],"lamax":[0,0,0]})
            ))
            .is_err()
        );
    }
}
