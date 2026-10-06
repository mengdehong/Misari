//! Bone capsules are fitted once in bind space and follow the instance pose.
use super::Model;
use glam::{DMat3, Mat4, Vec3};
use std::rc::Rc;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Capsule {
    pub a: Vec3,
    pub b: Vec3,
    pub radius: f32,
}
struct Binding {
    bone: Option<usize>,
    shape: Capsule,
}
pub(super) struct Colliders {
    bindings: Vec<Binding>,
    cached: Option<(Mat4, Rc<[Capsule]>)>,
}
impl Colliders {
    pub fn new(model: &Model, world: &[Mat4]) -> Self {
        let mut points = vec![Vec::new(); model.bones.len() + 1];
        let inverse = world
            .iter()
            .map(|m| {
                if m.determinant().abs() > 1e-12 {
                    m.inverse()
                } else {
                    Mat4::IDENTITY
                }
            })
            .collect::<Vec<_>>();
        for vertex in model.meshes.iter().flat_map(|m| &m.vertices) {
            let slot = (0..4)
                .max_by(|a, b| vertex.weights[*a].total_cmp(&vertex.weights[*b]))
                .unwrap();
            let bone = (vertex.weights[slot] > 0.).then_some(vertex.bones[slot]);
            let index = bone.unwrap_or(model.bones.len());
            let position = bone.map_or(vertex.position, |i| {
                inverse[i].transform_point3(vertex.position)
            });
            points[index].push(position);
        }
        let bindings = points
            .iter()
            .enumerate()
            .filter_map(|(index, p)| {
                fit(p).map(|shape| Binding {
                    bone: (index < model.bones.len()).then_some(index),
                    shape,
                })
            })
            .collect();
        Self {
            bindings,
            cached: None,
        }
    }
    pub fn invalidate(&mut self) {
        self.cached = None;
    }
    pub fn snapshot(&mut self, transform: Mat4, world: &[Mat4]) -> Rc<[Capsule]> {
        if let Some((previous, cached)) = &self.cached
            && *previous == transform
        {
            return cached.clone();
        }
        let shapes = self
            .bindings
            .iter()
            .map(|binding| {
                let matrix = transform * binding.bone.map_or(Mat4::IDENTITY, |i| world[i]);
                Capsule {
                    a: matrix.transform_point3(binding.shape.a),
                    b: matrix.transform_point3(binding.shape.b),
                    radius: binding.shape.radius * max_scale(matrix),
                }
            })
            .collect::<Vec<_>>();
        let shapes: Rc<[Capsule]> = shapes.into();
        self.cached = Some((transform, shapes.clone()));
        shapes
    }
}
fn fit(points: &[Vec3]) -> Option<Capsule> {
    if points.is_empty() {
        return None;
    }
    let min = points
        .iter()
        .copied()
        .fold(Vec3::splat(f32::INFINITY), Vec3::min);
    let max = points
        .iter()
        .copied()
        .fold(Vec3::splat(f32::NEG_INFINITY), Vec3::max);
    let center = (min + max) * 0.5;
    let axis = (0..3)
        .max_by(|a, b| (max[*a] - min[*a]).total_cmp(&(max[*b] - min[*b])))
        .unwrap();
    let mut a = center;
    let mut b = center;
    a[axis] = min[axis];
    b[axis] = max[axis];
    let radius = points.iter().map(|p| distance(*p, a, b)).fold(0., f32::max);
    a[axis] = (a[axis] + radius).min(center[axis]);
    b[axis] = (b[axis] - radius).max(center[axis]);
    let radius = points.iter().map(|p| distance(*p, a, b)).fold(0., f32::max);
    (radius > 1e-6).then_some(Capsule { a, b, radius })
}
fn distance(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let delta = b - a;
    let t = if delta.length_squared() > 1e-12 {
        (p - a).dot(delta) / delta.length_squared()
    } else {
        0.
    };
    p.distance(a.lerp(b, t.clamp(0., 1.)))
}
// Largest singular value: unlike a maximum column length, this also contains
// the capsule radius when a parent introduces shear through nonuniform scale.
fn max_scale(transform: Mat4) -> f32 {
    let m = DMat3::from_mat4(transform.as_dmat4());
    let a = m.transpose() * m;
    let off = a.x_axis.y * a.x_axis.y + a.x_axis.z * a.x_axis.z + a.y_axis.z * a.y_axis.z;
    let maximum = if off < 1e-24 {
        a.x_axis.x.max(a.y_axis.y).max(a.z_axis.z)
    } else {
        let q = (a.x_axis.x + a.y_axis.y + a.z_axis.z) / 3.;
        let spread = (a.x_axis.x - q).powi(2)
            + (a.y_axis.y - q).powi(2)
            + (a.z_axis.z - q).powi(2)
            + 2. * off;
        let p = (spread / 6.).sqrt();
        let determinant = ((a - DMat3::IDENTITY * q) / p).determinant() * 0.5;
        let angle = determinant.clamp(-1., 1.).acos() / 3.;
        q + 2. * p * angle.cos()
    };
    maximum.max(0.).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Quat, Vec4};
    #[test]
    fn sphere_fit_and_sheared_radius_bound_are_correct() {
        let points = [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z];
        let shape = fit(&points).unwrap();
        assert_eq!(shape.a, Vec3::ZERO);
        assert_eq!(shape.b, Vec3::ZERO);
        assert_eq!(shape.radius, 1.);
        let matrix = Mat4::from_scale_rotation_translation(
            Vec3::new(2., 1., 0.5),
            Quat::from_rotation_z(0.7),
            Vec3::ONE,
        );
        assert!((max_scale(matrix) - 2.).abs() < 1e-6);
        let shear = Mat4::from_cols(Vec4::X, Vec4::new(1., 1., 0., 0.), Vec4::Z, Vec4::W);
        assert!((max_scale(shear) - 1.618034).abs() < 1e-6);
        assert!(fit(&[]).is_none());
    }
    #[test]
    fn cached_bind_capsules_follow_bones_and_instance_transform_and_release() {
        let model = super::super::tests::fixture();
        let mut model = Rc::try_unwrap(model).ok().unwrap();
        model.meshes[0].vertices.clear();
        for p in [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z] {
            model.meshes[0].vertices.push(super::super::Vertex {
                position: Vec3::new(7., 0., 0.) + p,
                normal: Vec3::Z,
                tangent: Vec4::X,
                uv: glam::Vec2::ZERO,
                uv2: glam::Vec2::ZERO,
                bones: [0; 4],
                weights: Vec4::X,
            });
        }
        let mut rig = super::super::Rig::new(Rc::new(model)).unwrap();
        let transform = Mat4::from_translation(Vec3::Y * 4.);
        let first = rig.collision_capsules(transform);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].a, Vec3::new(7., 4., 0.));
        assert_eq!(first[0].radius, 1.);
        assert!(Rc::ptr_eq(&first, &rig.collision_capsules(transform)));
        rig.advance(0.5,&serde_json::json!({"__boneOverrides":{"0":Mat4::from_translation(Vec3::X*4.).to_cols_array()}})).unwrap();
        let changed = rig.collision_capsules(transform);
        assert_eq!(changed[0].a, Vec3::new(9., 4., 0.));
        assert!(!Rc::ptr_eq(&first, &changed));
        let weak = Rc::downgrade(&changed);
        drop(changed);
        drop(rig);
        assert!(weak.upgrade().is_none());
    }
}
