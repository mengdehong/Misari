//! Shared output fit. Coordinates entering engines are normalized, top-left based.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    #[default]
    Cover,
    Contain,
    Stretch,
}

impl Fit {
    /// Pixels per content unit. Callers validate positive, finite dimensions.
    pub fn scale(self, source: [f32; 2], output: [f32; 2]) -> [f32; 2] {
        let [x, y] = [output[0] / source[0], output[1] / source[1]];
        match self {
            Self::Cover => [x.max(y); 2],
            Self::Contain => [x.min(y); 2],
            Self::Stretch => [x, y],
        }
    }

    /// Centered texture coordinates, including contain borders and cover cropping.
    pub fn texture_scale(self, source: [f32; 2], output: [f32; 2]) -> [f32; 2] {
        if self == Self::Stretch {
            return [1.; 2];
        }
        let scale = self.scale(source, output);
        [
            output[0] / (source[0] * scale[0]),
            output[1] / (source[1] * scale[1]),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitted_extent_and_texture_coordinates_are_inverse() {
        for source in [[1920., 1080.], [10., 100.], [100., 10.]] {
            for output in [[640., 480.], [1000., 1000.]] {
                for fit in [Fit::Cover, Fit::Contain, Fit::Stretch] {
                    let scale = fit.scale(source, output);
                    let uv = fit.texture_scale(source, output);
                    for axis in 0..2 {
                        assert!(
                            (source[axis] * scale[axis] * uv[axis] - output[axis]).abs() < 0.001
                        );
                    }
                    match fit {
                        Fit::Cover => assert!(uv.iter().all(|v| *v <= 1.00001)),
                        Fit::Contain => assert!(uv.iter().all(|v| *v >= 0.99999)),
                        Fit::Stretch => assert_eq!(uv, [1.; 2]),
                    }
                }
            }
        }
    }
}
