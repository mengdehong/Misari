//! Model-local bounds shared by culling and cursor rays, independent of GPU resources.
use glam::{Mat4, Vec3};

pub(crate) type Bounds = [[f32; 3]; 2];

pub(crate) fn corners(bounds: Bounds) -> impl Iterator<Item = Vec3> {
    (0..8).map(move |i| {
        Vec3::new(
            bounds[i & 1][0],
            bounds[(i >> 1) & 1][1],
            bounds[(i >> 2) & 1][2],
        )
    })
}

pub(crate) fn include(bounds: &mut Option<Bounds>, point: [f32; 3]) {
    let range = bounds.get_or_insert([point; 2]);
    for (axis, value) in point.into_iter().enumerate() {
        range[0][axis] = range[0][axis].min(value);
        range[1][axis] = range[1][axis].max(value);
    }
}

pub(crate) fn merge(bounds: &mut Option<Bounds>, other: Option<Bounds>) {
    if let Some([min, max]) = other {
        include(bounds, min);
        include(bounds, max);
    }
}

/// Reject only when all corners are outside the same homogeneous clip plane.
/// Keeping w until the plane test also handles boxes crossing the near plane.
pub(crate) fn outside(bounds: Bounds, transform: Mat4) -> bool {
    if !transform.is_finite() {
        return false;
    }
    let mut outside = [true; 6];
    for point in corners(bounds) {
        let p = transform * point.extend(1.0);
        if !p.is_finite() {
            return false;
        }
        for (reject, distance) in outside.iter_mut().zip([
            p.w + p.x,
            p.w - p.x,
            p.w + p.y,
            p.w - p.y,
            p.w + p.z,
            p.w - p.z,
        ]) {
            *reject &= distance < -1e-5 * p.abs().max_element().max(1.0);
        }
    }
    outside.into_iter().any(|plane| plane)
}

/// Unproject the visible near/far segment into model-local coordinates.
pub(crate) fn ray(transform: Mat4, pointer: [f32; 2]) -> Option<[Vec3; 2]> {
    if !transform.is_finite()
        || transform.determinant() == 0.0
        || pointer
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return None;
    }
    let inverse = transform.inverse();
    if !inverse.is_finite() {
        return None;
    }
    let point = |z| {
        let p = inverse * glam::Vec4::new(pointer[0] * 2. - 1., 1. - pointer[1] * 2., z, 1.);
        (p.is_finite() && p.w.abs() > f32::MIN_POSITIVE).then(|| p.truncate() / p.w)
    };
    let points = [point(-1.)?, point(1.)?];
    points.iter().all(|p| p.is_finite()).then_some(points)
}

pub(crate) fn hit(bounds: Bounds, [near, far]: [Vec3; 2]) -> Option<Vec3> {
    let direction = far - near;
    let (mut enter, mut leave) = (0f32, 1f32);
    for axis in 0..3 {
        if direction[axis].abs() <= f32::MIN_POSITIVE {
            if near[axis] < bounds[0][axis] || near[axis] > bounds[1][axis] {
                return None;
            }
        } else {
            let a = (bounds[0][axis] - near[axis]) / direction[axis];
            let b = (bounds[1][axis] - near[axis]) / direction[axis];
            enter = enter.max(a.min(b));
            leave = leave.min(a.max(b));
            if enter > leave {
                return None;
            }
        }
    }
    let point = near + direction * enter;
    point.is_finite().then_some(point)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rays_respect_projection_clip_range_degenerate_bounds_and_invalid_transforms() {
        let plane = [[-2., -2., 0.], [2., 2., 0.]];
        for projection in [
            Mat4::orthographic_rh_gl(-10., 10., -10., 10., 1., 100.),
            Mat4::perspective_rh_gl(90f32.to_radians(), 1., 1., 100.),
        ] {
            let transform = projection * Mat4::from_translation(Vec3::new(0., 0., -10.));
            let ray = ray(transform, [0.55, 0.5]).unwrap();
            let local = hit(plane, ray).unwrap();
            assert!((local - Vec3::X).length() < 0.001, "{local:?}");
            assert!(hit([[3., -2., 0.], [4., 2., 0.]], ray).is_none());
            assert!(
                hit([[-2., -2., 20.], [2., 2., 21.]], ray).is_none(),
                "behind camera"
            );
            assert!(
                hit([[-2., -2., -101.], [2., 2., -100.]], ray).is_none(),
                "beyond far plane"
            );
        }
        assert!(ray(Mat4::ZERO, [0.5; 2]).is_none());
        assert!(ray(Mat4::IDENTITY, [f32::NAN, 0.5]).is_none());
        assert!(ray(Mat4::IDENTITY, [1.1, 0.5]).is_none());
        let large = Mat4::orthographic_rh_gl(0., 32768., 0., 32768., -10000., 10000.);
        assert!(
            ray(large, [0.5; 2]).is_some(),
            "small valid projection determinant"
        );
    }
    #[test]
    fn crossing_clip_planes_is_conservative_under_affine_and_perspective_transforms() {
        let bounds = [[-1.; 3], [1.; 3]];
        let projection = Mat4::perspective_rh_gl(90f32.to_radians(), 1., 1., 100.);
        for (position, rejected) in [
            (Vec3::new(0., 0., -10.), false),
            (Vec3::new(0., 0., -1.), false),
            (Vec3::new(0., 0., 3.), true),
            (Vec3::new(20., 0., -10.), true),
            (Vec3::new(10., 0., -10.), false),
            (Vec3::new(0., 0., -100.), false),
            (Vec3::new(0., 0., -102.), true),
        ] {
            assert_eq!(
                outside(bounds, projection * Mat4::from_translation(position)),
                rejected,
                "{position:?}"
            );
        }
        let rotation = Mat4::from_rotation_z(std::f32::consts::FRAC_PI_4);
        let scale = Mat4::from_scale(Vec3::new(3., 0.1, 2.));
        assert!(!outside(
            bounds,
            Mat4::from_translation(Vec3::new(2., 0., 0.)) * rotation * scale
        ));
        assert!(outside(
            bounds,
            Mat4::from_translation(Vec3::new(4., 0., 0.)) * rotation * scale
        ));
    }
}
