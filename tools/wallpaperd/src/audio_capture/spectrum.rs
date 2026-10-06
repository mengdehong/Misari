use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;
use we_scene::audio::AudioSnapshot;

pub const RATE: u32 = 48_000;
const SIZE: usize = 4096;
const SCENE_SIZE: usize = 2048;
const HOP: usize = 1024;

// FFT-bin boundaries calibrated with official WE stereo tone probes. Bass
// retains individual bins; the upper bands widen toward the 16 kHz cutoff.
const SCENE_UPPER_EDGES: [usize; 33] = [
    33, 44, 52, 56, 60, 72, 76, 92, 100, 108, 120, 136, 152, 164, 168, 184, 200, 216, 232, 248,
    264, 288, 320, 344, 376, 400, 432, 472, 504, 536, 576, 616, 684,
];

#[derive(Clone, Copy)]
pub(super) enum Response {
    Scene,
    // Media checks measure physical PCM gain, independently of visual response.
    #[cfg(test)]
    Pcm,
}

fn set_bands(snapshot: &mut AudioSnapshot, left: &[f32; 64], right: &[f32; 64]) {
    for spectrum in &mut snapshot.bands {
        let group = 64 / spectrum.left.len();
        for i in 0..spectrum.left.len() {
            let range = i * group..(i + 1) * group;
            spectrum.left[i] = left[range.clone()].iter().copied().fold(0., f32::max);
            spectrum.right[i] = right[range].iter().copied().fold(0., f32::max);
            spectrum.average[i] = (spectrum.left[i] + spectrum.right[i]) * 0.5;
        }
    }
}

pub(super) struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    window: [f32; SIZE],
    gain: f32,
    ring: [[f32; SIZE]; 2],
    position: usize,
    samples: usize,
    work: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    smoothed: [[f32; 64]; 2],
    ranges: [(usize, usize); 64],
    response: Response,
}
impl Analyzer {
    pub fn new(response: Response) -> Self {
        let size = match response {
            Response::Scene => SCENE_SIZE,
            #[cfg(test)]
            Response::Pcm => SIZE,
        };
        let fft = FftPlanner::new().plan_fft_forward(size);
        let window = [1.; SIZE];
        #[cfg(test)]
        let window = if matches!(response, Response::Pcm) {
            std::array::from_fn(|i| {
                0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (SIZE - 1) as f32).cos()
            })
        } else {
            window
        };
        let ranges = std::array::from_fn(|i| {
            if i < 32 {
                (i + 1, i + 2)
            } else {
                (SCENE_UPPER_EDGES[i - 32], SCENE_UPPER_EDGES[i - 31])
            }
        });
        #[cfg(test)]
        let ranges = if matches!(response, Response::Pcm) {
            std::array::from_fn(|i| {
                let bin = |edge: usize| {
                    let hz = 20. * 1000_f32.powf(edge as f32 / 64.);
                    (hz * SIZE as f32 / RATE as f32).floor() as usize
                };
                let lo = bin(i).clamp(1, SIZE / 2);
                (lo, bin(i + 1).max(lo + 1).min(SIZE / 2 + 1))
            })
        } else {
            ranges
        };
        let gain = match response {
            Response::Scene => 2. / size as f32,
            #[cfg(test)]
            Response::Pcm => 2. / window.iter().sum::<f32>(),
        };
        Self {
            scratch: vec![Complex::default(); fft.get_inplace_scratch_len()],
            fft,
            gain,
            window,
            ring: [[0.0; SIZE]; 2],
            position: 0,
            samples: 0,
            work: vec![Complex::default(); size],
            smoothed: [[0.0; 64]; 2],
            ranges,
            response,
        }
    }

    pub fn push(
        &mut self,
        frames: impl IntoIterator<Item = [f32; 2]>,
        mut publish: impl FnMut(AudioSnapshot),
    ) {
        let size = self.fft.len();
        for frame in frames {
            for (channel, value) in frame.into_iter().enumerate() {
                self.ring[channel][self.position] = if value.is_finite() {
                    value.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
            }
            self.position = (self.position + 1) % size;
            self.samples += 1;
            if self.samples >= size && (self.samples - size).is_multiple_of(HOP) {
                publish(self.transform());
                // Keep the counter bounded while preserving hop alignment.
                self.samples = size;
            }
        }
    }

    fn transform(&mut self) -> AudioSnapshot {
        let dt = HOP as f32 / RATE as f32;
        let size = self.fft.len();
        let peak = self
            .ring
            .iter()
            .flat_map(|c| &c[..size])
            .copied()
            .map(f32::abs)
            .fold(0., f32::max);
        let mut magnitudes = [[0.; 64]; 2];
        for (channel, bands) in magnitudes.iter_mut().enumerate() {
            let mean = match self.response {
                Response::Scene => 0.,
                #[cfg(test)]
                Response::Pcm => self.ring[channel].iter().sum::<f32>() / SIZE as f32,
            };
            for i in 0..size {
                self.work[i] = Complex::new(
                    (self.ring[channel][(self.position + i) % size] - mean) * self.window[i],
                    0.0,
                );
            }
            self.fft
                .process_with_scratch(&mut self.work, &mut self.scratch);
            for (i, &(lo, hi)) in self.ranges.iter().enumerate() {
                bands[i] = self.work[lo..hi]
                    .iter()
                    .enumerate()
                    .map(|(offset, v)| {
                        let weight = match self.response {
                            Response::Scene => {
                                let hz = (lo + offset) as f32 * RATE as f32 / size as f32;
                                (hz.max(120.) / 200.).sqrt()
                            }
                            #[cfg(test)]
                            Response::Pcm => 1.,
                        };
                        v.norm() * self.gain * weight
                    })
                    .fold(0.0_f32, f32::max);
            }
        }
        // A shared reference preserves stereo balance; its floor keeps quiet
        // tones below peak normalization instead of amplifying them to full bars.
        // ponytail: weighting and the floor approximate official output; refine
        // them with mixed-audio probes if playback transients diverge.
        let level = magnitudes.iter().flatten().copied().fold(0.0009, f32::max);
        for (channel, bands) in magnitudes.iter().enumerate() {
            for (i, &magnitude) in bands.iter().enumerate() {
                let (target, attack, release) = match self.response {
                    Response::Scene => (
                        if peak > 0.0001 {
                            (magnitude * 3. / level).clamp(0., 1.)
                        } else {
                            0.
                        },
                        0.030,
                        0.140,
                    ),
                    #[cfg(test)]
                    Response::Pcm => (magnitude.clamp(0., 1.), 0.02, 0.15),
                };
                let previous = &mut self.smoothed[channel][i];
                // Smooth visual values so attack and release stay independent
                // of the input volume and coarse-band resolution.
                let factor = 1.0 - (-dt / if target > *previous { attack } else { release }).exp();
                *previous += (target - *previous) * factor;
                if *previous < 0.000001 {
                    *previous = 0.0;
                }
            }
        }
        let mut snapshot = AudioSnapshot {
            available: true,
            capturing: true,
            ..Default::default()
        };
        set_bands(&mut snapshot, &self.smoothed[0], &self.smoothed[1]);
        snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scene_gain_keeps_quiet_playback_below_peak_normalization() {
        let mut analyzer = Analyzer::new(Response::Scene);
        let mut actual = AudioSnapshot::default();
        let tone = |amplitude| {
            (0..RATE as usize * 4).map(move |i| {
                let sine = (std::f32::consts::TAU * 60. * i as f32 / RATE as f32).sin();
                [amplitude * sine, amplitude * 0.2 * sine]
            })
        };
        analyzer.push(tone(0.03), |s| actual = s);
        analyzer.push(std::iter::repeat_n([0., 0.], RATE as usize / 2), |s| {
            actual = s
        });
        analyzer.push(tone(0.0003), |s| actual = s);
        // Both cold quiet playback and a loud-to-quiet transition retain the
        // same smaller bar in official WE, rather than normalizing to full height.
        let quiet = actual.bands[2].average[2];
        assert!((0.28..0.52).contains(&quiet), "quiet transition: {quiet}");
        analyzer.push(tone(0.0003), |s| actual = s);
        assert!((actual.bands[2].average[2] - quiet).abs() < 0.015);
        let mut cold = Analyzer::new(Response::Scene);
        cold.push(tone(0.0003), |s| actual = s);
        assert!((actual.bands[2].average[2] - quiet).abs() < 0.015);
    }
    #[test]
    fn scene_tones_match_official_frequency_peaks_gain_and_coarser_bands() {
        // Captured from official WE with identical stereo tones on a private
        // output monitor: 64-band peak and quiet left/right amplitudes.
        let cases = [
            (60., 2, 0.64, 0.13),
            (125., 4, 0.64, 0.13),
            (250., 10, 0.83, 0.17),
            (440., 18, 1.00, 0.20),
            (1000., 32, 1.00, 0.31),
            (6000., 51, 1.00, 0.60),
            (12000., 60, 1.00, 0.60),
        ];
        for (hz, band, left, right) in cases {
            for amplitude in [0.0003, 0.03] {
                let mut analyzer = Analyzer::new(Response::Scene);
                let mut actual = AudioSnapshot::default();
                analyzer.push(
                    (0..RATE as usize).map(|i| {
                        let sine = (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin();
                        [amplitude * sine, amplitude * 0.2 * sine]
                    }),
                    |s| actual = s,
                );
                let peak = actual.bands[2]
                    .right
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .unwrap()
                    .0;
                assert_eq!(peak, band, "{hz}Hz official frequency mapping");
                let (left, right) = if amplitude > 0.001 {
                    (1., 0.60)
                } else {
                    (left, right)
                };
                assert!(
                    (actual.bands[2].left[band] - left).abs() < 0.12,
                    "{hz}Hz/{amplitude}: left gain {:?}, official left={left}",
                    actual.bands[2]
                );
                assert!(
                    (actual.bands[2].right[band] - right).abs() < 0.12,
                    "{hz}Hz stereo gain: {:?}, official right={right}",
                    actual.bands[2]
                );
                for spectrum in &actual.bands {
                    assert_eq!(
                        spectrum.left.iter().copied().fold(0., f32::max),
                        actual.bands[2].left[band]
                    );
                    for i in 0..spectrum.left.len() {
                        assert_eq!(
                            spectrum.average[i],
                            (spectrum.left[i] + spectrum.right[i]) * 0.5
                        );
                    }
                }
                assert!(actual.valid());
                analyzer.push(std::iter::repeat_n([0., 0.], RATE as usize * 3), |s| {
                    actual = s
                });
                assert!(actual.bands[2].left.iter().all(|v| *v == 0.));
            }
        }
    }
    #[test]
    fn coarser_spectra_retain_stereo_peaks_without_average_attenuation() {
        let mut snapshot = AudioSnapshot::default();
        let mut left = [0.; 64];
        let mut right = [0.; 64];
        left[13] = 0.6;
        right[29] = 0.9;
        left[40] = 0.;
        set_bands(&mut snapshot, &left, &right);
        assert!(snapshot.valid());
        let quiet = snapshot.bands[2].left[13];
        assert!((0.3..0.7).contains(&quiet), "quiet audio response: {quiet}");
        assert!(snapshot.bands[2].right[29] > quiet);
        assert_eq!(snapshot.bands[2].left[40], 0.);
        assert_eq!(snapshot.bands[1].left[6], quiet);
        assert_eq!(snapshot.bands[0].left[3], quiet);
    }
    #[test]
    fn fixed_pcm_channels_bands_gain_chunking_silence_and_nonfinite() {
        let frames = (0..RATE as usize)
            .map(|i| {
                let phase = std::f32::consts::TAU * i as f32 / RATE as f32;
                [(phase * 750.0).sin() * 0.8, (phase * 6000.0).sin() * 0.4]
            })
            .collect::<Vec<_>>();
        let mut whole = Analyzer::new(Response::Pcm);
        let mut expected = AudioSnapshot::default();
        whole.push(frames.iter().copied(), |s| expected = s);
        let mut split = Analyzer::new(Response::Pcm);
        let mut actual = AudioSnapshot::default();
        for chunk in frames.chunks(137) {
            split.push(chunk.iter().copied(), |s| actual = s);
        }
        assert_eq!(actual, expected);
        assert!(actual.valid());
        let peak = |v: &[f32]| {
            v.iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap()
                .0
        };
        for band in &actual.bands {
            assert!(peak(&band.left) < peak(&band.right));
            for i in 0..band.left.len() {
                assert_eq!(band.average[i], (band.left[i] + band.right[i]) * 0.5);
            }
        }
        assert!((actual.bands[2].left[peak(&actual.bands[2].left)] - 0.8).abs() < 0.02);
        assert!((actual.bands[2].right[peak(&actual.bands[2].right)] - 0.4).abs() < 0.02);
        split.push(
            std::iter::repeat_n([f32::NAN, f32::INFINITY], RATE as usize * 3),
            |s| actual = s,
        );
        assert!(actual.valid());
        assert!(actual.bands[2].average.iter().all(|v| *v == 0.0));
    }
}
