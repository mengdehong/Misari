//! WE HSV and normalized RGB palettes. Alpha and instance color stay independent.
use super::{lerp, random, scalar};
use crate::scene::bindings::components;
use anyhow::{Context, Result, ensure};
use glam::Vec3;
use rand_chacha::ChaCha8Rng;
use serde_json::Value;

#[derive(Clone)]
pub(super) enum Random {
    Hsv { min: Vec3, max: Vec3, steps: u32 },
    List { colors: Vec<Vec3>, noise: Vec3 },
}
impl Random {
    pub(super) fn hsv(v: &Value) -> Result<Self> {
        let steps = scalar(v, "huesteps", 0.)?;
        ensure!(
            (0.0..=65536.).contains(&steps) && steps.fract() == 0.,
            "invalid particle hue steps"
        );
        Ok(Self::Hsv {
            min: Vec3::new(
                scalar(v, "huemin", 0.)?,
                scalar(v, "saturationmin", 1.)?,
                scalar(v, "valuemin", 1.)?,
            ),
            max: Vec3::new(
                scalar(v, "huemax", 1.)?,
                scalar(v, "saturationmax", 1.)?,
                scalar(v, "valuemax", 1.)?,
            ),
            steps: steps as u32,
        })
    }
    pub(super) fn list(v: &Value) -> Result<Self> {
        let entries = v["colors"].as_array().context("particle color list")?;
        ensure!(
            !entries.is_empty() && entries.len() <= 10,
            "particle palette requires 1 to 10 colors"
        );
        let mut colors = Vec::with_capacity(entries.len());
        for color in entries {
            let color = components(color, &[1.; 3], 3)?;
            colors.push(Vec3::new(color[0], color[1], color[2]));
        }
        Ok(Self::List {
            colors,
            noise: Vec3::new(
                scalar(v, "huenoise", 0.)?,
                scalar(v, "saturationnoise", 0.)?,
                scalar(v, "valuenoise", 0.)?,
            ),
        })
    }
    pub(super) fn sample(&self, rng: &mut ChaCha8Rng) -> Vec3 {
        match self {
            Self::Hsv { min, max, steps } => {
                let hue = random(rng);
                let hue = if *steps == 0 {
                    hue
                } else {
                    (hue * *steps as f32).floor() / *steps as f32
                };
                rgb(Vec3::new(
                    lerp(min.x, max.x, hue),
                    lerp(min.y, max.y, random(rng)),
                    lerp(min.z, max.z, random(rng)),
                ))
            }
            Self::List { colors, noise } => {
                let color = colors[(random(rng) * colors.len() as f32) as usize];
                if *noise == Vec3::ZERO {
                    return color;
                }
                let offset =
                    Vec3::from_array(std::array::from_fn(|i| (2. * random(rng) - 1.) * noise[i]));
                rgb(hsv(color) + offset)
            }
        }
    }
}
fn rgb(value: Vec3) -> Vec3 {
    let h = value.x.rem_euclid(1.) * 6.;
    let s = value.y.clamp(0., 1.);
    let v = value.z.max(0.);
    let c = v * s;
    let x = c * (1. - (h.rem_euclid(2.) - 1.).abs());
    let color = match h.floor() as u8 {
        0 => Vec3::new(c, x, 0.),
        1 => Vec3::new(x, c, 0.),
        2 => Vec3::new(0., c, x),
        3 => Vec3::new(0., x, c),
        4 => Vec3::new(x, 0., c),
        _ => Vec3::new(c, 0., x),
    };
    color + Vec3::splat(v - c)
}
fn hsv(color: Vec3) -> Vec3 {
    let v = color.max_element();
    let c = v - color.min_element();
    if c <= f32::EPSILON || v <= 0. {
        return Vec3::new(0., 0., v);
    }
    let h = if v == color.x {
        ((color.y - color.z) / c).rem_euclid(6.)
    } else if v == color.y {
        (color.z - color.x) / c + 2.
    } else {
        (color.x - color.y) / c + 4.
    };
    Vec3::new(h / 6., c / v, v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::SeedableRng;
    use serde_json::json;
    #[test]
    fn hsv_roundtrip_quantized_hues_and_palette_noise_are_seeded_and_bounded() {
        for color in [
            Vec3::X,
            Vec3::Y,
            Vec3::Z,
            Vec3::ONE,
            Vec3::ZERO,
            Vec3::new(0.2, 0.5, 0.9),
            Vec3::new(2., 0.2, 0.),
        ] {
            assert!((rgb(hsv(color)) - color).length() < 0.000001);
        }
        let six = Random::hsv(&json!({"huesteps":6})).unwrap();
        let mut a = ChaCha8Rng::seed_from_u64(42);
        let mut b = a.clone();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..256 {
            let color = six.sample(&mut a);
            assert_eq!(color, six.sample(&mut b));
            assert_eq!(color.min_element(), 0.);
            assert_eq!(color.max_element(), 1.);
            assert!(color.to_array().iter().all(|c| *c == 0. || *c == 1.));
            seen.insert(color.to_array().map(f32::to_bits));
        }
        assert_eq!(seen.len(), 6);
        let palette = Random::list(&json!({"colors":["1 0 0","0 1 0"]})).unwrap();
        for _ in 0..64 {
            assert!([Vec3::X, Vec3::Y].contains(&palette.sample(&mut a)));
        }
        let gray = Random::list(
            &json!({"colors":["0.5 0.5 0.5"],"saturationnoise":1,"valuenoise":0.25,"huenoise":1}),
        )
        .unwrap();
        for _ in 0..64 {
            let color = gray.sample(&mut a);
            assert!(color.is_finite() && color.min_element() >= 0. && color.max_element() <= 0.75);
        }
        for invalid in [
            json!({"colors":[]}),
            json!({"colors":vec!["1 1 1";11]}),
            json!({"colors":["NaN 0 0"]}),
        ] {
            assert!(Random::list(&invalid).is_err());
        }
        assert!(Random::hsv(&json!({"huesteps":1.5})).is_err());
    }
}
