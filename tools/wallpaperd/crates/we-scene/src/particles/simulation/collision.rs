//! Swept point collisions keep authored geometry local and wallpaper bounds global.
use super::{Particle, cp, scalar, vector};
use anyhow::{Result, ensure};
use glam::{Mat4, Vec2, Vec3};
use serde_json::Value;
use std::{collections::HashMap, rc::Rc};

pub(in crate::particles) type Models = Rc<HashMap<usize, Rc<[crate::mdl::Capsule]>>>;

#[derive(Clone, Copy)]
enum Behavior {
    Bounce,
    Slide,
    Stop,
    Delete,
}
#[derive(Clone)]
enum Shape {
    Plane {
        normal: Vec3,
        distance: f32,
    },
    Quad {
        normal: Vec3,
        forward: Vec3,
        origin: Vec3,
        half_size: Vec2,
    },
    Sphere {
        origin: Vec3,
        radius: f32,
    },
    Bounds,
    Model(usize),
}
#[derive(Clone)]
pub(super) struct Collision {
    shape: Shape,
    behavior: Behavior,
    bounce: f32,
    point: Option<usize>,
    stop_rotation: bool,
}
pub(super) struct Context<'a> {
    pub points: [Vec3; 8],
    pub to_world: Mat4,
    pub to_local: Mat4,
    pub world_particles: bool,
    pub bounds: Option<Vec2>,
    pub models: &'a Models,
}
#[derive(Clone, Copy)]
struct Hit {
    time: f32,
    normal: Vec3,
    point: Vec3,
}
impl Collision {
    pub(super) fn parse(v: &Value) -> Result<Self> {
        let flags = super::super::animation::flags(v)?;
        let normal = || -> Result<Vec3> {
            let n = vector(v, "plane", Vec3::Y)?;
            ensure!(
                n.length_squared() > 1e-12,
                "zero particle collision plane normal"
            );
            Ok(n.normalize())
        };
        let shape = match v["name"].as_str() {
            Some("collisionplane") => Shape::Plane {
                normal: normal()?,
                distance: scalar(v, "distance", 0.)?,
            },
            Some("collisionquad") => {
                let normal = normal()?;
                let forward = vector(v, "forward", Vec3::Z)?;
                let forward = forward - normal * forward.dot(normal);
                ensure!(
                    forward.length_squared() > 1e-12,
                    "parallel particle collision quad axes"
                );
                let size = crate::scene::bindings::components(&v["size"], &[100., 100.], 2)?;
                ensure!(
                    size.iter().all(|n| *n >= 0.),
                    "negative particle collision quad size"
                );
                Shape::Quad {
                    normal,
                    forward: forward.normalize(),
                    origin: vector(v, "origin", Vec3::ZERO)?,
                    half_size: Vec2::from_array([size[0], size[1]]) * 0.5,
                }
            }
            Some("collisionsphere") => {
                let radius = scalar(v, "radius", 50.)?;
                ensure!(radius >= 0., "negative particle collision sphere radius");
                Shape::Sphere {
                    origin: vector(v, "origin", Vec3::ZERO)?,
                    radius,
                }
            }
            Some("collisionbounds") => Shape::Bounds,
            Some("collisionmodel") => Shape::Model(0),
            _ => anyhow::bail!("invalid particle collision shape"),
        };
        Ok(Self {
            shape,
            behavior: match v["collisionbehavior"].as_str().unwrap_or("bounce") {
                "bounce" => Behavior::Bounce,
                "slide" => Behavior::Slide,
                "stop" => Behavior::Stop,
                "delete" => Behavior::Delete,
                other => anyhow::bail!("unknown particle collision behavior {other}"),
            },
            bounce: scalar(v, "bouncefactor", 1.)?.max(0.),
            point: if flags & 1 != 0 || !v["controlpoint"].is_null() {
                Some(cp(v)?)
            } else {
                None
            },
            stop_rotation: flags & 2 != 0 || v["stoprotation"] == true,
        })
    }
    pub(super) fn model(v: &Value, index: usize) -> Result<Self> {
        let mut collision = Self::parse(v)?;
        collision.shape = Shape::Model(index);
        Ok(collision)
    }
    fn respond(&self, velocity: Vec3, normal: Vec3) -> Vec3 {
        let inward = velocity.dot(normal).min(0.);
        match self.behavior {
            Behavior::Bounce => velocity - normal * inward * (1. + self.bounce),
            Behavior::Slide => velocity - normal * inward,
            Behavior::Stop | Behavior::Delete => Vec3::ZERO,
        }
    }
    pub(super) fn apply(&self, p: &mut Particle, previous: Vec3, context: &Context) -> bool {
        let bounds = matches!(self.shape, Shape::Bounds);
        let world = bounds || matches!(self.shape, Shape::Model(_));
        let (transform, inverse) = if world {
            if context.world_particles {
                (Mat4::IDENTITY, Mat4::IDENTITY)
            } else {
                (context.to_world, context.to_local)
            }
        } else if context.world_particles {
            (context.to_local, context.to_world)
        } else {
            (Mat4::IDENTITY, Mat4::IDENTITY)
        };
        let mut previous = transform.transform_point3(previous);
        let mut position = transform.transform_point3(p.position);
        let mut velocity = transform.transform_vector3(p.velocity);
        let offset = self.point.map_or(Vec3::ZERO, |i| context.points[i]);
        let mut collided = false;
        // Two axis contacts suffice for the rectangular wallpaper; process the
        // remaining sweep after each contact rather than tunnelling through it.
        for _ in 0..if bounds { 2 } else { 1 } {
            let hit = match self.shape {
                Shape::Plane { normal, distance } => plane(
                    previous,
                    position,
                    offset + normal * distance,
                    normal,
                    false,
                ),
                Shape::Quad {
                    normal,
                    forward,
                    origin,
                    half_size,
                } => {
                    let origin = origin + offset;
                    plane(previous, position, origin, normal, true).filter(|hit| {
                        let delta = hit.point - origin;
                        delta.dot(forward.cross(normal)).abs() <= half_size.x
                            && delta.dot(forward).abs() <= half_size.y
                    })
                }
                Shape::Sphere { origin, radius } => {
                    sphere(previous, position, origin + offset, radius)
                }
                Shape::Bounds => context.bounds.and_then(|size| {
                    [
                        (Vec3::ZERO, Vec3::X),
                        (Vec3::X * size.x, -Vec3::X),
                        (Vec3::ZERO, Vec3::Y),
                        (Vec3::Y * size.y, -Vec3::Y),
                    ]
                    .into_iter()
                    .filter_map(|(origin, normal)| plane(previous, position, origin, normal, false))
                    .min_by(|a, b| a.time.total_cmp(&b.time))
                }),
                Shape::Model(index) => context.models.get(&index).and_then(|shapes| {
                    shapes
                        .iter()
                        .filter_map(|shape| capsules::capsule(previous, position, shape))
                        .min_by(|a, b| a.time.total_cmp(&b.time))
                }),
            };
            let Some(hit) = hit else {
                break;
            };
            collided = true;
            let contact = hit.point;
            position = contact + self.respond(position - contact, hit.normal);
            previous = contact;
            velocity = self.respond(velocity, hit.normal);
            if self.stop_rotation {
                p.angular = Vec3::ZERO;
            }
            if matches!(self.behavior, Behavior::Delete | Behavior::Stop) {
                break;
            }
        }
        if collided {
            p.position = inverse.transform_point3(position);
            p.velocity = inverse.transform_vector3(velocity);
        }
        collided && matches!(self.behavior, Behavior::Delete)
    }
}
fn plane(
    previous: Vec3,
    position: Vec3,
    origin: Vec3,
    normal: Vec3,
    one_side: bool,
) -> Option<Hit> {
    let start = (previous - origin).dot(normal);
    let end = (position - origin).dot(normal);
    if end > 0. || end == 0. && start <= 0. || one_side && start < 0. {
        return None;
    }
    let time = if start <= 0. {
        0.
    } else {
        start / (start - end)
    };
    let point = previous.lerp(position, time);
    Some(Hit {
        time,
        normal,
        point: point - normal * (point - origin).dot(normal),
    })
}
fn sphere(previous: Vec3, position: Vec3, origin: Vec3, radius: f32) -> Option<Hit> {
    if radius == 0. {
        return None;
    }
    let delta = position - previous;
    let start = previous - origin;
    let c = start.length_squared() - radius * radius;
    let time = if c < 0. {
        0.
    } else {
        let a = delta.length_squared();
        if a < 1e-12 {
            return None;
        }
        let b = start.dot(delta);
        if b >= 0. {
            return None;
        }
        let discriminant = b * b - a * c;
        if discriminant < 0. {
            return None;
        }
        let time = (-b - discriminant.sqrt()) / a;
        if !(0. ..=1.).contains(&time) {
            return None;
        }
        time
    };
    let normal =
        (previous.lerp(position, time) - origin).normalize_or((-delta).normalize_or(Vec3::X));
    Some(Hit {
        time,
        normal,
        point: origin + normal * radius,
    })
}

mod capsules {
    use super::*;

    pub(super) fn capsule(
        previous: Vec3,
        position: Vec3,
        shape: &crate::mdl::Capsule,
    ) -> Option<Hit> {
        let axis = shape.b - shape.a;
        let length = axis.length_squared();
        if length < 1e-12 {
            return sphere(previous, position, shape.a, shape.radius);
        }
        let closest = shape.a + axis * ((previous - shape.a).dot(axis) / length).clamp(0., 1.);
        if previous.distance_squared(closest) < shape.radius * shape.radius {
            let normal =
                (previous - closest).normalize_or((previous - position).normalize_or(Vec3::X));
            return Some(Hit {
                time: 0.,
                normal,
                point: closest + normal * shape.radius,
            });
        }
        let delta = position - previous;
        let start = previous - shape.a;
        let radial_start = start - axis * start.dot(axis) / length;
        let radial_delta = delta - axis * delta.dot(axis) / length;
        let a = radial_delta.length_squared();
        let b = radial_start.dot(radial_delta);
        let c = radial_start.length_squared() - shape.radius * shape.radius;
        let discriminant = b * b - a * c;
        let cylinder = if a > 1e-12 && discriminant >= 0. {
            let time = (-b - discriminant.sqrt()) / a;
            let point = previous.lerp(position, time);
            let axial = (point - shape.a).dot(axis) / length;
            if (0. ..=1.).contains(&time) && (0. ..=1.).contains(&axial) {
                Some(Hit {
                    time,
                    normal: (point - shape.a - axis * axial).normalize(),
                    point,
                })
            } else {
                None
            }
        } else {
            None
        };
        [
            cylinder,
            sphere(previous, position, shape.a, shape.radius),
            sphere(previous, position, shape.b, shape.radius),
        ]
        .into_iter()
        .flatten()
        .min_by(|a, b| a.time.total_cmp(&b.time))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn sweep_detects_cylinder_caps_and_initial_penetration() {
            let shape = crate::mdl::Capsule {
                a: -Vec3::Z * 2.,
                b: Vec3::Z * 2.,
                radius: 1.,
            };
            let hit = capsule(-Vec3::X * 5., Vec3::X * 5., &shape).unwrap();
            assert!((hit.time - 0.4).abs() < 1e-6);
            assert_eq!(hit.normal, -Vec3::X);
            assert_eq!(hit.point, -Vec3::X);
            let hit = capsule(Vec3::new(-5., 0., 2.5), Vec3::new(5., 0., 2.5), &shape).unwrap();
            assert!((hit.normal.z - 0.5).abs() < 1e-6);
            assert!(capsule(Vec3::new(-5., 0., 4.), Vec3::new(5., 0., 4.), &shape).is_none());
            let hit = capsule(Vec3::X * 0.5, Vec3::X * 0.6, &shape).unwrap();
            assert_eq!(hit.time, 0.);
            assert_eq!(hit.point, Vec3::X);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{audio::AudioSnapshot, particles::simulation::System};
    use glam::Quat;
    use serde_json::json;

    fn system(operator: Value) -> System {
        System::new(&json!({"maxcount":1,"emitter":[{"name":"boxrandom","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2}],"operator":[operator]}), &Value::Null, 7).unwrap()
    }
    fn context(models: &Models) -> Context<'_> {
        Context {
            points: [Vec3::ZERO; 8],
            to_world: Mat4::IDENTITY,
            to_local: Mat4::IDENTITY,
            world_particles: false,
            bounds: Some(Vec2::splat(32.)),
            models,
        }
    }
    fn particle() -> Particle {
        let mut system = system(json!({"name":"movement"}));
        system.advance(0., Vec3::ZERO, Mat4::IDENTITY, &AudioSnapshot::default());
        system.particles.remove(0)
    }
    #[test]
    fn sweeps_bounce_slide_stop_delete_and_stop_rotation_without_tunnelling() {
        let models = Models::default();
        let previous = Vec3::new(-8., 4., 0.);
        for (behavior, expected, deleted) in [
            ("bounce", Vec3::new(4., 2., 0.), false),
            ("slide", Vec3::new(4., 0., 0.), false),
            ("stop", Vec3::ZERO, false),
            ("delete", Vec3::ZERO, true),
        ] {
            let collision = Collision::parse(&json!({"name":"collisionplane","bouncefactor":0.5,"collisionbehavior":behavior,"flags":2})).unwrap();
            let mut p = particle();
            p.position = previous + Vec3::new(8., -8., 0.);
            p.velocity = Vec3::new(4., -4., 0.);
            p.angular = Vec3::ONE;
            assert_eq!(
                collision.apply(&mut p, previous, &context(&models)),
                deleted
            );
            assert!(
                (p.velocity - expected).length() < 1e-6,
                "{behavior}: {:?}",
                p.velocity
            );
            assert_eq!(p.angular, Vec3::ZERO);
            let expected_position = match behavior {
                "bounce" => Vec3::new(0., 2., 0.),
                "slide" => Vec3::ZERO,
                _ => Vec3::new(-4., 0., 0.),
            };
            assert!((p.position - expected_position).length() < 1e-6);
        }
        let collision = Collision::parse(&json!({"name":"collisionsphere","radius":1})).unwrap();
        let mut p = particle();
        p.position = Vec3::X * 5.;
        p.velocity = Vec3::X * 10.;
        assert!(!collision.apply(&mut p, -Vec3::X * 5., &context(&models)));
        assert!((p.position.x + 7.).abs() < 1e-6);
        assert_eq!(p.velocity, -Vec3::X * 10.);
    }
    #[test]
    fn quad_is_one_sided_finite_oriented_and_control_point_relative() {
        let models = Models::default();
        let collision = Collision::parse(
        &json!({"name":"collisionquad","size":"4 8","controlpoint":3,"collisionbehavior":"slide"}),
    )
    .unwrap();
        let mut context = context(&models);
        context.points[3] = Vec3::Y * 10.;
        for (x, z, from, to, hit) in [
            (1., 3., 12., 8., true),
            (3., 0., 12., 8., false),
            (0., 5., 12., 8., false),
            (0., 0., 8., 12., false),
        ] {
            let mut p = particle();
            p.position = Vec3::new(x, to, z);
            p.velocity = Vec3::new(2., -4., 0.);
            collision.apply(&mut p, Vec3::new(x, from, z), &context);
            assert_eq!(p.velocity.y == 0., hit);
            if hit {
                assert_eq!(p.position.y, 10.);
            }
        }
        assert!(Collision::parse(&json!({"name":"collisionplane","plane":"0 0 0"})).is_err());
        assert!(Collision::parse(&json!({"name":"collisionquad","plane":"0 0 1"})).is_err());
        assert!(Collision::parse(&json!({"name":"collisionsphere","radius":-1})).is_err());
        assert!(
            Collision::parse(&json!({"name":"collisionbounds","collisionbehavior":"unexpected"}))
                .is_err()
        );
    }
    #[test]
    fn bounds_and_local_shapes_preserve_rotated_scaled_world_space_coordinates() {
        let models = Models::default();
        let bounds = Collision::parse(&json!({"name":"collisionbounds"})).unwrap();
        let to_world = Mat4::from_scale_rotation_translation(
            Vec3::splat(2.),
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            Vec3::new(16., -16., 0.),
        );
        let to_local = to_world.inverse();
        for world_particles in [false, true] {
            let ctx = Context {
                to_world,
                to_local,
                world_particles,
                ..context(&models)
            };
            let transform = if world_particles {
                Mat4::IDENTITY
            } else {
                to_local
            };
            let mut p = particle();
            let previous = transform.transform_point3(Vec3::new(31., 31., 0.));
            p.position = transform.transform_point3(Vec3::new(35., 35., 0.));
            p.velocity = transform.transform_vector3(Vec3::new(4., 4., 0.));
            bounds.apply(&mut p, previous, &ctx);
            let actual = if world_particles {
                p.position
            } else {
                to_world.transform_point3(p.position)
            };
            let velocity = if world_particles {
                p.velocity
            } else {
                to_world.transform_vector3(p.velocity)
            };
            assert!(
                (actual - Vec3::new(29., 29., 0.)).length() < 1e-5,
                "{actual:?}"
            );
            assert!(
                (velocity - Vec3::new(-4., -4., 0.)).length() < 1e-5,
                "{velocity:?}"
            );
        }
        let collision =
            Collision::parse(&json!({"name":"collisionplane","collisionbehavior":"slide"}))
                .unwrap();
        let ctx = Context {
            to_world,
            to_local,
            world_particles: true,
            ..context(&models)
        };
        let mut p = particle();
        p.position = to_world.transform_point3(-Vec3::Y);
        p.velocity = to_world.transform_vector3(-Vec3::Y * 4.);
        collision.apply(&mut p, to_world.transform_point3(Vec3::Y), &ctx);
        assert!((p.position - to_world.transform_point3(Vec3::ZERO)).length() < 1e-5);
        assert!(p.velocity.length() < 1e-5);
    }
    #[test]
    fn delete_dispatches_final_death_once_and_fixed_steps_are_output_fps_independent() {
        let audio = AudioSnapshot::default();
        let value = json!({"maxcount":1,"emitter":[{"name":"boxrandom","origin":"0 1 0","instantaneous":1,"rate":0}],"initializer":[{"name":"lifetimerandom","min":2,"max":2},{"name":"velocityrandom","min":"0 -20 0","max":"0 -20 0"}],"operator":[{"name":"movement"},{"name":"collisionplane","collisionbehavior":"delete"}]});
        let mut system = System::new(&value, &Value::Null, 7).unwrap();
        system.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
        let mut deaths = 0;
        for _ in 0..20 {
            system.step_tick(super::super::STEP as f32, &audio);
            deaths += system.events.died.len();
            for event in &system.events.died {
                assert!(event.values.position.y.abs() < 1e-6);
            }
        }
        assert_eq!(deaths, 1);
        assert!(system.particles.is_empty());
        let mut value = value;
        value["operator"][1]["collisionbehavior"] = json!("bounce");
        let run = |fps: u32| {
            let mut s = System::new(&value, &Value::Null, 7).unwrap();
            for n in 0..=fps {
                s.advance(n as f64 / fps as f64, Vec3::ZERO, Mat4::IDENTITY, &audio);
            }
            s.particles
        };
        assert_eq!(run(24), run(60));
    }
}
