//! Improved Perlin noise, using the reference renderer's permutation and curl field.
use glam::Vec3;

pub(super) use simplex::{fbm_field, field as simplex_field};
const PERM: [u8; 256] = [
    151, 160, 137, 91, 90, 15, 131, 13, 201, 95, 96, 53, 194, 233, 7, 225, 140, 36, 103, 30, 69,
    142, 8, 99, 37, 240, 21, 10, 23, 190, 6, 148, 247, 120, 234, 75, 0, 26, 197, 62, 94, 252, 219,
    203, 117, 35, 11, 32, 57, 177, 33, 88, 237, 149, 56, 87, 174, 20, 125, 136, 171, 168, 68, 175,
    74, 165, 71, 134, 139, 48, 27, 166, 77, 146, 158, 231, 83, 111, 229, 122, 60, 211, 133, 230,
    220, 105, 92, 41, 55, 46, 245, 40, 244, 102, 143, 54, 65, 25, 63, 161, 1, 216, 80, 73, 209, 76,
    132, 187, 208, 89, 18, 169, 200, 196, 135, 130, 116, 188, 159, 86, 164, 100, 109, 198, 173,
    186, 3, 64, 52, 217, 226, 250, 124, 123, 5, 202, 38, 147, 118, 126, 255, 82, 85, 212, 207, 206,
    59, 227, 47, 16, 58, 17, 182, 189, 28, 42, 223, 183, 170, 213, 119, 248, 152, 2, 44, 154, 163,
    70, 221, 153, 101, 155, 167, 43, 172, 9, 129, 22, 39, 253, 19, 98, 108, 110, 79, 113, 224, 232,
    178, 185, 112, 104, 218, 246, 97, 228, 251, 34, 242, 193, 238, 210, 144, 12, 191, 179, 162,
    241, 81, 51, 145, 235, 249, 14, 239, 107, 49, 192, 214, 31, 181, 199, 106, 157, 184, 84, 204,
    176, 115, 121, 50, 45, 127, 4, 150, 254, 138, 236, 205, 93, 222, 114, 67, 29, 24, 72, 243, 141,
    128, 195, 78, 66, 215, 61, 156, 180,
];
fn perm(i: usize) -> usize {
    PERM[i & 255] as usize
}
fn grad(h: usize, x: f64, y: f64, z: f64) -> f64 {
    let h = h & 15;
    let u = if h < 8 { x } else { y };
    let v = if h < 4 {
        y
    } else if h == 12 || h == 14 {
        x
    } else {
        z
    };
    (if h & 1 == 0 { u } else { -u }) + (if h & 2 == 0 { v } else { -v })
}
fn noise(p: Vec3) -> f32 {
    let [x, y, z] = p.to_array().map(f64::from);
    let [xi, yi, zi] = [x, y, z].map(|v| v.floor() as i64 as usize & 255);
    let [x, y, z] = [x, y, z].map(|v| v - v.floor());
    let ease = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let [u, v, w] = [x, y, z].map(ease);
    let mix = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let a = perm(xi) + yi;
    let b = perm(xi + 1) + yi;
    let aa = perm(a) + zi;
    let ab = perm(a + 1) + zi;
    let ba = perm(b) + zi;
    let bb = perm(b + 1) + zi;
    mix(
        mix(
            mix(grad(perm(aa), x, y, z), grad(perm(ba), x - 1.0, y, z), u),
            mix(
                grad(perm(ab), x, y - 1.0, z),
                grad(perm(bb), x - 1.0, y - 1.0, z),
                u,
            ),
            v,
        ),
        mix(
            mix(
                grad(perm(aa + 1), x, y, z - 1.0),
                grad(perm(ba + 1), x - 1.0, y, z - 1.0),
                u,
            ),
            mix(
                grad(perm(ab + 1), x, y - 1.0, z - 1.0),
                grad(perm(bb + 1), x - 1.0, y - 1.0, z - 1.0),
                u,
            ),
            v,
        ),
        w,
    ) as f32
}
pub(super) fn field(p: Vec3) -> Vec3 {
    Vec3::new(
        noise(p),
        noise(p + Vec3::new(89.2, 33.1, 57.3)),
        noise(p + Vec3::new(100.3, 120.1, 142.2)),
    )
}
pub fn curl(p: Vec3) -> Vec3 {
    let e = 0.0001;
    let y = Vec3::new(89.2, 33.1, 57.3);
    let z = Vec3::new(100.3, 120.1, 142.2);
    // Curl uses two components of each derivative. Preserve the finite
    // differences and their rounding while omitting six unused noise samples.
    let derivative =
        |axis: Vec3, offset: Vec3| noise(p + axis * e + offset) - noise(p - axis * e + offset);
    Vec3::new(
        derivative(Vec3::Y, z) - derivative(Vec3::Z, y),
        derivative(Vec3::Z, Vec3::ZERO) - derivative(Vec3::X, z),
        derivative(Vec3::X, y) - derivative(Vec3::Y, Vec3::ZERO),
    ) / (2.0 * e)
}

mod simplex {
    //! Three-dimensional simplex noise, with fixed gradients and permutation.
    use glam::Vec3;
    const GRADIENTS: [Vec3; 12] = [
        Vec3::new(1., 1., 0.),
        Vec3::new(-1., 1., 0.),
        Vec3::new(1., -1., 0.),
        Vec3::new(-1., -1., 0.),
        Vec3::new(1., 0., 1.),
        Vec3::new(-1., 0., 1.),
        Vec3::new(1., 0., -1.),
        Vec3::new(-1., 0., -1.),
        Vec3::new(0., 1., 1.),
        Vec3::new(0., -1., 1.),
        Vec3::new(0., 1., -1.),
        Vec3::new(0., -1., -1.),
    ];
    fn value(p: Vec3) -> f32 {
        if !p.is_finite() {
            return 0.;
        }
        let cell = (p + Vec3::splat(p.element_sum() / 3.)).floor();
        let origin = p - cell + Vec3::splat(cell.element_sum() / 6.);
        let (one, two) = if origin.x >= origin.y {
            if origin.y >= origin.z {
                (Vec3::X, Vec3::new(1., 1., 0.))
            } else if origin.x >= origin.z {
                (Vec3::X, Vec3::new(1., 0., 1.))
            } else {
                (Vec3::Z, Vec3::new(1., 0., 1.))
            }
        } else if origin.y < origin.z {
            (Vec3::Z, Vec3::new(0., 1., 1.))
        } else if origin.x < origin.z {
            (Vec3::Y, Vec3::new(0., 1., 1.))
        } else {
            (Vec3::Y, Vec3::new(1., 1., 0.))
        };
        let indices = cell.to_array().map(|v| v as i64 as usize & 255);
        let corner = |offset: Vec3, point: Vec3| {
            let weight = 0.6 - point.length_squared();
            if weight <= 0. {
                return 0.;
            }
            let [x, y, z] = offset.to_array().map(|v| v as usize);
            let hash = super::perm(
                indices[0] + x + super::perm(indices[1] + y + super::perm(indices[2] + z)),
            );
            weight.powi(4) * GRADIENTS[hash % 12].dot(point)
        };
        (32. * (corner(Vec3::ZERO, origin)
            + corner(one, origin - one + Vec3::splat(1. / 6.))
            + corner(two, origin - two + Vec3::splat(1. / 3.))
            + corner(Vec3::ONE, origin - Vec3::splat(0.5))))
        .clamp(-1., 1.)
    }
    pub(in crate::particles) fn field(p: Vec3) -> Vec3 {
        Vec3::new(
            value(p),
            value(p + Vec3::new(89.2, 33.1, 57.3)),
            value(p + Vec3::new(100.3, 120.1, 142.2)),
        )
    }
    pub(in crate::particles) fn fbm_field(mut p: Vec3, octaves: u8) -> Vec3 {
        let mut sum = Vec3::ZERO;
        let mut amplitude = 1.;
        let mut weight = 0.;
        for _ in 0..octaves {
            sum += field(p) * amplitude;
            weight += amplitude;
            p *= 2.;
            amplitude *= 0.5;
        }
        sum / weight
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn simplex_is_continuous_at_rank_boundaries_and_fbm_is_bounded() {
            assert_eq!(value(Vec3::ZERO), 0.);
            assert_eq!(value(Vec3::ONE), 0.);
            for p in [
                Vec3::splat(0.25),
                Vec3::new(-0.3, 0.7, 0.7),
                Vec3::new(4.2, -3.1, 0.4),
            ] {
                let n = value(p);
                assert!(n.is_finite() && n.abs() <= 1.);
                assert!((value(p + Vec3::X * 0.0001) - n).abs() < 0.002);
                assert!((value(p - Vec3::X * 0.0001) - n).abs() < 0.002);
                let fbm = fbm_field(p, 8);
                assert!(fbm.is_finite() && fbm.abs().max_element() <= 1.);
            }
        }
    }
}

#[cfg(test)]
mod curl_tests {
    use super::*;

    #[test]
    fn omitted_noise_components_preserve_curl_exactly() {
        for i in -1000..=1000 {
            let p = Vec3::new(i as f32 * 0.017, i as f32 * -0.39, i as f32 * 2.7);
            let e = 0.0001;
            let dx = field(p + Vec3::X * e) - field(p - Vec3::X * e);
            let dy = field(p + Vec3::Y * e) - field(p - Vec3::Y * e);
            let dz = field(p + Vec3::Z * e) - field(p - Vec3::Z * e);
            let expected = Vec3::new(dy.z - dz.y, dz.x - dx.z, dx.y - dy.x) / (2. * e);
            assert_eq!(curl(p), expected, "{p:?}");
        }
    }
}
