//! Fixed, bounded audio input shared by scripts, shaders and particle systems.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spectrum {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub average: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioSnapshot {
    pub sequence: u64,
    pub available: bool,
    pub capturing: bool,
    pub device: Option<String>,
    /// In order: 16, 32, 64 bands. Values are normalized to 0..=1.
    pub bands: [Spectrum; 3],
}
impl Default for AudioSnapshot {
    fn default() -> Self {
        Self {
            sequence: 0,
            available: false,
            capturing: false,
            device: None,
            bands: [16, 32, 64].map(|count| Spectrum {
                left: vec![0.0; count],
                right: vec![0.0; count],
                average: vec![0.0; count],
            }),
        }
    }
}
impl AudioSnapshot {
    pub fn spectrum(&self, count: usize) -> Option<&Spectrum> {
        [16, 32, 64]
            .iter()
            .position(|&n| n == count)
            .map(|i| &self.bands[i])
    }
    pub fn valid(&self) -> bool {
        self.bands.iter().zip([16, 32, 64]).all(|(band, count)| {
            [&band.left, &band.right, &band.average]
                .into_iter()
                .all(|channel| {
                    channel.len() == count
                        && channel
                            .iter()
                            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
                })
        })
    }
}
