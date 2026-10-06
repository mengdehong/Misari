mod animation {
    //! Sprite sequence speed and blending follow the particle system, not texture timing.
    use super::simulation::Particle;
    use anyhow::{Result, ensure};
    use serde_json::Value;

    pub(super) fn flags(value: &Value) -> Result<u32> {
        if value["flags"].is_null() {
            return Ok(0);
        }
        let flags = value["flags"]
            .as_f64()
            .filter(|n| (0. ..=u32::MAX as f64).contains(n) && n.fract() == 0.);
        flags
            .map(|n| n as u32)
            .ok_or_else(|| anyhow::anyhow!("invalid particle flags"))
    }

    #[derive(Clone, Copy)]
    pub(super) struct Animation {
        random: bool,
        sequence: f32,
        pub blend: bool,
    }
    impl Animation {
        pub fn parse(value: &Value) -> Result<Self> {
            let random = match value["animationmode"].as_str().unwrap_or("sequence") {
                "sequence" => false,
                "randomframe" => true,
                other => anyhow::bail!("unknown particle animation mode {other}"),
            };
            let sequence =
                crate::scene::bindings::components(&value["sequencemultiplier"], &[1.], 1)?[0];
            ensure!(
                (0. ..=10000.).contains(&sequence),
                "invalid particle sequence multiplier"
            );
            Ok(Self {
                random,
                sequence,
                blend: !random && flags(value)? & 2 == 0,
            })
        }
        pub fn frame(self, particle: &Particle, count: usize) -> (usize, usize, f32) {
            let phase = if self.random {
                particle.random_frame(count) as f32
            } else {
                (particle.age / particle.life * self.sequence).rem_euclid(1.) * count as f32
            };
            let current = (phase.floor() as usize).min(count - 1);
            (
                current,
                (current + 1).min(count - 1),
                if self.blend { phase.fract() } else { 0. },
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;
        #[test]
        fn negative_and_unbounded_sprite_settings_are_rejected() {
            for value in [
                json!({"flags":-1}),
                json!({"flags":4294967296u64}),
                json!({"sequencemultiplier":-1}),
                json!({"sequencemultiplier":10001}),
                json!({"animationmode":"unknown"}),
            ] {
                assert!(Animation::parse(&value).is_err(), "{value}");
            }
        }
    }
}
mod connections {
    //! Layer connections name source objects; snapshots are shared by all child instances.
    use anyhow::{Context, Result, ensure};
    use serde_json::Value;
    use std::{
        collections::{HashMap, HashSet},
        rc::Rc,
    };
    pub(super) struct Connections {
        models: Vec<(usize, String)>,
        images: Vec<(usize, String)>,
    }
    impl Connections {
        pub fn parse(object: &Value) -> Result<Self> {
            let mut models = Vec::new();
            let mut images = Vec::new();
            let mut seen = HashSet::new();
            let entries = object["dependencies"]
                .as_array()
                .map_or(&[][..], Vec::as_slice);
            ensure!(entries.len() <= 64, "too many particle layer connections");
            for entry in entries {
                if entry["type"] != "collisionmodel" && entry["type"] != "emitterimage" {
                    continue;
                }
                let index = entry["index"]
                    .as_u64()
                    .context("invalid particle model connection index")?
                    as usize;
                ensure!(
                    index < 64 && seen.insert((entry["type"].to_string(), index)),
                    "duplicate or invalid particle model connection"
                );
                let id = &entry["id"];
                ensure!(
                    id.is_number() || id.is_string(),
                    "invalid particle model connection id"
                );
                if entry["type"] == "collisionmodel" {
                    models.push((index, id.to_string()));
                } else {
                    images.push((index, id.to_string()));
                }
            }
            Ok(Self { models, images })
        }
        pub fn has_models(&self) -> bool {
            !self.models.is_empty()
        }
        pub fn image_requests(&self) -> impl Iterator<Item = &(usize, String)> {
            self.images.iter()
        }
        pub fn images(
            &self,
            sources: &HashMap<String, Rc<super::emission::Snapshot>>,
        ) -> super::emission::Sources {
            Rc::new(
                self.images
                    .iter()
                    .filter_map(|(index, id)| {
                        sources.get(id).map(|source| (*index, source.clone()))
                    })
                    .collect(),
            )
        }
        pub fn models(
            &self,
            sources: &HashMap<String, Rc<[crate::mdl::Capsule]>>,
        ) -> super::simulation::Models {
            Rc::new(
                self.models
                    .iter()
                    .filter_map(|(index, id)| {
                        sources.get(id).map(|capsules| (*index, capsules.clone()))
                    })
                    .collect(),
            )
        }
    }
}
use crate::{
    assets::{AssetKey, Assets},
    gpu::{Frame, Mesh, Pass, Texture},
    scene::State,
    scene::bindings::{Properties, resolve},
};
use anyhow::{Context, Result, ensure};
use glam::{Mat4, Vec3, Vec4};
use glow::HasContext;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

pub(crate) mod emission;
mod family;
mod history {
    //! Fixed simulation-time samples; drawing never changes a particle's trail.
    use anyhow::{Result, ensure};
    use glam::Vec3;
    use serde_json::Value;
    use std::collections::VecDeque;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(super) struct Config {
        pub duration: f64,
        pub segments: usize,
    }
    impl Config {
        pub fn parse(value: &Value) -> Result<Self> {
            let duration = crate::scene::bindings::components(&value["length"], &[1.], 1)?[0];
            let segments = crate::scene::bindings::components(&value["segments"], &[4.], 1)?[0];
            ensure!(
                (1e-6..=3600.).contains(&duration),
                "invalid rope trail length"
            );
            ensure!(
                (2. ..=64.).contains(&segments) && segments.fract() == 0.,
                "invalid rope trail segments"
            );
            Ok(Self {
                duration: duration as f64,
                segments: segments as usize,
            })
        }
        fn interval(self) -> f64 {
            self.duration / self.segments as f64
        }
    }
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(super) struct Sample {
        pub time: f64,
        pub position: Vec3,
    }
    #[derive(Clone, Debug, PartialEq)]
    pub(super) struct History {
        config: Config,
        samples: VecDeque<Sample>,
        previous: Sample,
        next: u64,
    }
    impl History {
        pub fn translate(&mut self, offset: Vec3) {
            for sample in &mut self.samples {
                sample.position += offset;
            }
            self.previous.position += offset;
        }
        pub fn new(config: Config, position: Vec3) -> Self {
            let initial = Sample { time: 0., position };
            let mut samples = VecDeque::with_capacity(config.segments + 2);
            samples.push_back(initial);
            Self {
                config,
                samples,
                previous: initial,
                next: 1,
            }
        }
        pub fn record_before_frame(&mut self, time: f64, position: Vec3, remaining: f64) {
            // Earlier samples expire before catch-up finishes. Keep the last
            // position for interpolation, plus two sample intervals at the boundary.
            if remaining > self.config.duration + 2. * self.config.interval() {
                self.previous = Sample { time, position };
            } else {
                self.record(time, position);
            }
        }
        pub fn record(&mut self, time: f64, position: Vec3) {
            if time <= self.previous.time {
                return;
            }
            let interval = self.config.interval();
            // Most 120 Hz simulation ticks do not cross a trail sample.
            // Avoid rounding and pruning work until a sample is actually due.
            if time / interval + 1e-9 >= self.next as f64 {
                // A large simulation-rate step skips expired samples, never allocates its past.
                let first = ((time - self.config.duration).max(0.) / interval).floor() as u64;
                self.next = self.next.max(first);
                let last = (time / interval + 1e-9).floor() as u64;
                for index in self.next..=last {
                    let sample_time = index as f64 * interval;
                    let fraction = ((sample_time - self.previous.time)
                        / (time - self.previous.time))
                        .clamp(0., 1.) as f32;
                    self.samples.push_back(Sample {
                        time: sample_time,
                        position: self.previous.position.lerp(position, fraction),
                    });
                }
                self.next = last.saturating_add(1).max(self.next);
                while self.samples.len() > self.config.segments + 2 {
                    self.samples.pop_front();
                }
            }
            while self.samples.len() > 2 && self.samples[1].time <= time - self.config.duration {
                self.samples.pop_front();
            }
            self.previous = Sample { time, position };
        }
        pub fn points(&self, out: &mut Vec<Sample>) {
            out.clear();
            let start = (self.previous.time - self.config.duration).max(0.);
            if let Some(first) = self.samples.front()
                && first.time < start
                && let Some(second) = self.samples.get(1)
            {
                let fraction =
                    ((start - first.time) / (second.time - first.time)).clamp(0., 1.) as f32;
                out.push(Sample {
                    time: start,
                    position: first.position.lerp(second.position, fraction),
                });
            }
            out.extend(
                self.samples
                    .iter()
                    .copied()
                    // Simulation steps and positions use f32. Tick rounding can put
                    // `previous` just beyond a sample without a distinct endpoint.
                    .filter(|p| p.time >= start && (p.time as f32) < self.previous.time as f32),
            );
            out.push(self.previous);
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn record_reference(history: &mut History, time: f64, position: Vec3) {
            if time <= history.previous.time {
                return;
            }
            let interval = history.config.interval();
            // A large simulation-rate step skips expired samples, never allocates its past.
            let first = ((time - history.config.duration).max(0.) / interval).floor() as u64;
            history.next = history.next.max(first);
            let last = (time / interval + 1e-9).floor() as u64;
            for index in history.next..=last {
                let sample_time = index as f64 * interval;
                let fraction = ((sample_time - history.previous.time)
                    / (time - history.previous.time))
                    .clamp(0., 1.) as f32;
                history.samples.push_back(Sample {
                    time: sample_time,
                    position: history.previous.position.lerp(position, fraction),
                });
            }
            history.next = last.saturating_add(1).max(history.next);
            while history.samples.len() > history.config.segments + 2 {
                history.samples.pop_front();
            }
            while history.samples.len() > 2
                && history.samples[1].time <= time - history.config.duration
            {
                history.samples.pop_front();
            }
            history.previous = Sample { time, position };
        }
        #[test]
        fn sparse_trail_samples_match_full_recording_at_boundaries_and_large_steps() {
            for duration in [0.07, 0.5, 30.] {
                let mut actual = History::new(
                    Config {
                        duration,
                        segments: 16,
                    },
                    Vec3::ZERO,
                );
                let mut expected = actual.clone();
                let mut times = (1..=2000)
                    .map(|tick| tick as f64 * f64::from((5. / 120.) as f32))
                    .collect::<Vec<_>>();
                let boundary = duration / 16.;
                times.extend([
                    1000.,
                    1000. + boundary - 1e-10,
                    1000. + boundary,
                    1000. + boundary + 1e-10,
                ]);
                for time in times {
                    let position = Vec3::new((time as f32).sin(), time as f32, 0.);
                    actual.record(time, position);
                    record_reference(&mut expected, time, position);
                    assert_eq!(actual, expected, "duration={duration}, time={time}");
                }
            }
        }
        #[test]
        fn tick_rounding_does_not_duplicate_the_trail_endpoint() {
            let mut history = History::new(
                Config {
                    duration: 0.5,
                    segments: 8,
                },
                Vec3::ZERO,
            );
            let mut time = 0.;
            for _ in 0..60 {
                time += f64::from((1. / 120.) as f32);
                history.record(time, Vec3::new(time as f32 * 32., 0., 0.));
            }
            let mut points = Vec::new();
            history.points(&mut points);
            assert!(
                points
                    .windows(2)
                    .all(|p| (p[1].time as f32) > p[0].time as f32),
                "nearly coincident endpoint samples create overlapping ribbon quads: {points:?}"
            );
        }
        #[test]
        fn history_follows_turns_clips_duration_and_bounds_large_steps() {
            let mut history = History::new(
                Config {
                    duration: 1.,
                    segments: 4,
                },
                Vec3::ZERO,
            );
            for tick in 1..=120 {
                let time = tick as f64 / 120.;
                let position = if time <= 0.5 {
                    Vec3::new(time as f32 * 8., 0., 0.)
                } else {
                    Vec3::new(4., (time as f32 - 0.5) * 8., 0.)
                };
                history.record(time, position);
            }
            let mut points = Vec::new();
            history.points(&mut points);
            assert_eq!(points.len(), 5);
            assert_eq!(points[0].position, Vec3::ZERO);
            assert_eq!(points[2].position, Vec3::new(4., 0., 0.));
            assert_eq!(points[4].position, Vec3::new(4., 4., 0.));
            history.record(1.1, Vec3::new(4., 4.8, 0.));
            history.points(&mut points);
            assert!((points[0].time - 0.1).abs() < 1e-9);
            assert!((points[0].position.x - 0.8).abs() < 1e-5);
            history.record(1000., Vec3::ONE);
            history.points(&mut points);
            assert!(points.len() <= 6);
            assert!(history.samples.len() <= 6);
            assert!(
                points
                    .iter()
                    .all(|p| p.time >= 999. && p.position.is_finite())
            );
        }
    }
}
pub(crate) mod instances {
    //! The WE shader input is shared by sprite and ribbon instances.
    use super::simulation::Particle;
    use crate::assets::SpriteFrame;
    use glam::{Vec3, Vec4};

    pub(crate) const FLOATS: usize = 65;

    pub(crate) struct Layout {
        pub attributes: Vec<(&'static str, usize, usize)>,
        pub floats: usize,
        ranges: Vec<std::ops::Range<usize>>,
    }
    impl Layout {
        pub fn new(mut used: impl FnMut(&str) -> bool) -> Self {
            let attributes = [
                ("a_Position", 3, 0),
                ("a_ParticleRotationSize", 4, 3),
                ("a_Color", 4, 7),
                ("a_TexCoordVec4C1", 4, 11),
                ("a_ParticleUVRange", 2, 15),
                ("a_ParticleFrame0", 4, 17),
                ("a_ParticleFrame1", 4, 21),
                ("a_ParticleFrame2", 4, 25),
                ("a_ParticleFrameMix", 1, 29),
                ("a_RopeEnd", 4, 30),
                ("a_RopeEndColor", 4, 34),
                ("a_InstanceRow0", 4, 38),
                ("a_InstanceRow1", 4, 42),
                ("a_InstanceRow2", 4, 46),
                ("a_RopePrevious", 3, 50),
                ("a_RopeAfter", 3, 53),
                ("a_ParticleEye", 3, 50),
                ("a_ParticleRight", 3, 56),
                ("a_ParticleUp", 3, 59),
                ("a_ParticleForward", 3, 62),
            ]
            .into_iter()
            .filter(|(name, _, _)| used(name))
            .collect::<Vec<_>>();
            let mut needed = [false; FLOATS];
            // Keep positions even for a constant shader, so an empty attribute
            // set still has a nonzero stride and preserves the instance count.
            needed[..3].fill(true);
            for &(_, count, offset) in &attributes {
                needed[offset..offset + count].fill(true);
            }
            let mut floats = 0;
            let mut offsets = [0; FLOATS];
            let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
            for (index, used) in needed.into_iter().enumerate() {
                offsets[index] = floats;
                if used {
                    floats += 1;
                    if let Some(range) = ranges.last_mut().filter(|r| r.end == index) {
                        range.end += 1;
                    } else {
                        ranges.push(index..index + 1);
                    }
                }
            }
            Self {
                attributes: attributes
                    .into_iter()
                    .map(|(name, count, offset)| (name, count, offsets[offset]))
                    .collect(),
                floats,
                ranges,
            }
        }
        pub(super) fn append(&self, out: &mut Vec<f32>, data: &[f32; FLOATS]) {
            for range in &self.ranges {
                out.extend_from_slice(&data[range.clone()]);
            }
        }
    }

    pub(super) fn data(
        p: &Particle,
        end: Option<&Particle>,
        color: Vec4,
        frames: &[SpriteFrame],
        animation: super::animation::Animation,
        placement: super::orientation::Placement,
    ) -> [f32; FLOATS] {
        let mut data = [0.; FLOATS];
        data[0..3].copy_from_slice(&p.position.to_array());
        // WE sprite and rope shaders receive half the simulated particle size.
        data[3..7].copy_from_slice(&[p.rotation.x, p.rotation.y, p.rotation.z, p.size * 0.5]);
        data[7..11].copy_from_slice(&(p.color * color).to_array());
        data[11..14].copy_from_slice(&p.velocity.to_array());
        data[14] = p.age / p.life;
        data[15..17].copy_from_slice(&end.map_or([0.0, 1.0], |b| [p.age / p.life, b.age / b.life]));
        if frames.is_empty() {
            data[17..30].copy_from_slice(&[
                0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0,
            ]);
        } else {
            let (current, next, mix) = animation.frame(p, frames.len());
            let a = &frames[current];
            let b = &frames[next];
            data[17..19].copy_from_slice(&a.origin);
            data[19..21].copy_from_slice(&a.u);
            data[21..23].copy_from_slice(&a.v);
            data[23..25].copy_from_slice(&b.origin);
            data[25..27].copy_from_slice(&b.u);
            data[27..29].copy_from_slice(&b.v);
            data[29] = mix;
        }
        let b = end.unwrap_or(p);
        data[30..33].copy_from_slice(&b.position.to_array());
        data[33] = b.size * 0.5;
        data[34..38].copy_from_slice(&(b.color * color).to_array());
        for axis in 0..3 {
            data[38 + axis * 4..42 + axis * 4].copy_from_slice(&[
                placement.matrix.x_axis[axis],
                placement.matrix.y_axis[axis],
                placement.matrix.z_axis[axis],
                placement.matrix.w_axis[axis],
            ]);
        }
        data[50..53].copy_from_slice(&placement.eye.to_array());
        data[53..56].copy_from_slice(&b.position.to_array());
        data[56..65].copy_from_slice(&placement.basis.to_cols_array());
        data
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn ribbon(
        out: &mut Vec<f32>,
        layout: &Layout,
        mut data: [f32; FLOATS],
        start: &super::ribbon::Point,
        end: &super::ribbon::Point,
        previous: Vec3,
        after: Vec3,
        uv: [f32; 2],
        color: Vec4,
    ) {
        data[0..3].copy_from_slice(&start.position.to_array());
        data[6] = start.size * 0.5;
        data[7..11].copy_from_slice(&(start.color * color).to_array());
        data[15..17].copy_from_slice(&uv);
        data[30..33].copy_from_slice(&end.position.to_array());
        data[33] = end.size * 0.5;
        data[34..38].copy_from_slice(&(end.color * color).to_array());
        data[50..53].copy_from_slice(&previous.to_array());
        data[53..56].copy_from_slice(&after.to_array());
        layout.append(out, &data);
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn compact_layout_keeps_attribute_values_aliases_and_instance_boundaries() {
            let data = std::array::from_fn::<_, FLOATS, _>(|i| i as f32);
            for active in [
                vec![],
                vec![
                    ("a_Position", 3, 0),
                    ("a_Color", 4, 7),
                    ("a_ParticleUp", 3, 59),
                ],
                vec![
                    ("a_RopePrevious", 3, 50),
                    ("a_ParticleEye", 3, 50),
                    ("a_RopeEnd", 4, 30),
                ],
            ] {
                let layout = Layout::new(|name| active.iter().any(|(n, _, _)| *n == name));
                let mut packed = Vec::new();
                layout.append(&mut packed, &data);
                layout.append(&mut packed, &data);
                assert_eq!(packed.len(), layout.floats * 2);
                assert!(layout.floats < FLOATS);
                for (name, count, source) in active {
                    let (_, _, offset) = layout
                        .attributes
                        .iter()
                        .find(|(n, _, _)| *n == name)
                        .unwrap();
                    for base in [0, layout.floats] {
                        assert_eq!(
                            &packed[base + offset..base + offset + count],
                            &data[source..source + count]
                        );
                    }
                }
            }
            let full = Layout::new(|_| true);
            let mut packed = Vec::new();
            full.append(&mut packed, &data);
            assert_eq!(full.floats, FLOATS);
            assert_eq!(packed, data);
            let aliases = Layout::new(|name| matches!(name, "a_RopePrevious" | "a_ParticleEye"));
            assert_eq!(aliases.floats, 6);
            assert_eq!(aliases.attributes[0].2, aliases.attributes[1].2);
        }
    }
}
mod noise;
mod orientation {
    //! Billboard axes are computed once per system instance, including child transforms.
    use anyhow::{Result, ensure};
    use glam::{Mat3, Mat4, Vec3};
    use serde_json::Value;

    #[derive(Clone, Copy)]
    enum Mode {
        Screen,
        Upright,
        Fixed(Vec3),
    }
    #[derive(Clone, Copy)]
    pub(super) struct Orientation {
        mode: Mode,
        world: bool,
    }
    #[derive(Clone, Copy)]
    pub(super) struct Placement {
        pub matrix: Mat4,
        pub basis: Mat3,
        pub eye: Vec3,
    }
    impl Orientation {
        pub fn parse(value: &Value) -> Result<Self> {
            let mode = match value["orientation"].as_str().unwrap_or("screen") {
                "screen" => Mode::Screen,
                "upright" => Mode::Upright,
                "fixed" => {
                    let axis = Vec3::from_slice(&crate::scene::bindings::components(
                        &value["axis"],
                        &[0., 0., 1.],
                        3,
                    )?);
                    ensure!(axis.length_squared() > 1e-12, "invalid fixed particle axis");
                    Mode::Fixed(axis.normalize())
                }
                other => anyhow::bail!("unknown particle orientation {other}"),
            };
            Ok(Self {
                mode,
                world: super::animation::flags(value)? & 1 != 0 || value["worldspace"] == true,
            })
        }
        pub fn placement(
            self,
            model: Mat4,
            instance: Mat4,
            view: crate::camera::View,
        ) -> Placement {
            let world = model * instance;
            let linear = Mat3::from_mat4(world);
            if linear.determinant().abs() < 1e-12 {
                return Placement {
                    matrix: instance,
                    basis: Mat3::IDENTITY,
                    eye: Vec3::ZERO,
                };
            }
            let preferred_up = if self.world {
                Vec3::Y
            } else {
                linear * Vec3::Y
            };
            let fallback_up = if self.world {
                Vec3::Z
            } else {
                linear * Vec3::Z
            };
            let basis = match self.mode {
                // 2D sprites rotate and mirror with their authored layer/child transform.
                Mode::Screen if !view.full_3d && !self.world => linear,
                Mode::Screen => view.basis,
                Mode::Upright => {
                    let up = preferred_up.normalize();
                    let forward = view.basis.z_axis - up * view.basis.z_axis.dot(up);
                    let forward = if forward.length_squared() > 1e-12 {
                        forward
                    } else {
                        view.basis.y_axis - up * view.basis.y_axis.dot(up)
                    };
                    let right = up.cross(forward).normalize();
                    Mat3::from_cols(right, up, right.cross(up).normalize())
                }
                Mode::Fixed(axis) => {
                    let forward = if self.world { axis } else { linear * axis }.normalize();
                    let right = preferred_up.cross(forward);
                    let right = if right.length_squared() > 1e-12 {
                        right
                    } else {
                        fallback_up.cross(forward)
                    }
                    .normalize();
                    Mat3::from_cols(right, forward.cross(right).normalize(), forward)
                }
            };
            let inverse = linear.inverse();
            Placement {
                matrix: instance,
                basis: Mat3::from_cols(
                    (inverse * basis.x_axis).normalize(),
                    (inverse * basis.y_axis).normalize(),
                    (inverse * basis.z_axis).normalize(),
                ),
                eye: world.inverse().transform_point3(view.eye),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;
        #[test]
        fn screen_fixed_upright_and_world_axes_include_parent_and_child_rotation() {
            let camera=crate::camera::Camera::prepare(&json!({"_perspective":true,"cameraTransforms":{"eye":[10,0,0],"center":[0,0,0],"up":[0,1,0]}})).unwrap();
            let view = camera.view([1.; 2], [32; 2], crate::Fit::Stretch);
            let model = Mat4::from_rotation_y(0.7) * Mat4::from_scale(Vec3::new(1., -1., 1.));
            let child = Mat4::from_rotation_x(0.4);
            let axes = |value: Value| {
                let p = Orientation::parse(&value)
                    .unwrap()
                    .placement(model, child, view);
                let m = Mat3::from_mat4(model * child);
                Mat3::from_cols(
                    (m * p.basis.x_axis).normalize(),
                    (m * p.basis.y_axis).normalize(),
                    (m * p.basis.z_axis).normalize(),
                )
            };
            assert!(axes(json!({})).abs_diff_eq(view.basis, 1e-5));
            let local = axes(json!({"orientation":"fixed","axis":[0,0,1]}));
            assert!(
                local
                    .z_axis
                    .abs_diff_eq((model * child).transform_vector3(Vec3::Z).normalize(), 1e-5)
            );
            assert!(
                axes(json!({"orientation":"fixed","axis":[0,1,0],"flags":1}))
                    .z_axis
                    .abs_diff_eq(Vec3::Y, 1e-5)
            );
            assert!(
                axes(json!({"orientation":"upright","flags":1}))
                    .y_axis
                    .abs_diff_eq(Vec3::Y, 1e-5)
            );
            assert!(Orientation::parse(&json!({"orientation":"fixed","axis":[0,0,0]})).is_err());
            let overhead=crate::camera::Camera::prepare(&json!({"_perspective":true,"cameraTransforms":{"eye":[0,10,0],"center":[0,0,0],"up":[0,0,1]}})).unwrap().view([1.;2],[32;2],crate::Fit::Stretch);
            let p = Orientation::parse(&json!({"orientation":"upright","flags":1}))
                .unwrap()
                .placement(model, child, overhead);
            assert!(p.basis.is_finite());
        }
    }
}
mod ribbon {
    //! WE rope shader spline geometry and UVs, evaluated without geometry shaders.
    use super::{history, simulation::Particle};
    use anyhow::{Result, ensure};
    use glam::{Vec3, Vec4};
    use serde_json::Value;

    #[derive(Clone, Copy)]
    pub(super) struct Config {
        pub subdivision: usize,
        pub scale: f32,
        pub scrolling: bool,
        smoothing: bool,
        fade_alpha: bool,
        fade_size: bool,
    }
    impl Config {
        pub fn parse(value: &Value, trail: bool) -> Result<Self> {
            let subdivision = crate::scene::bindings::components(
                &value["subdivision"],
                &[if trail { 1. } else { 4. }],
                1,
            )?[0];
            let scale = crate::scene::bindings::components(&value["uvscale"], &[1.], 1)?[0];
            ensure!(
                (0. ..=64.).contains(&subdivision) && subdivision.fract() == 0.,
                "invalid rope subdivision"
            );
            ensure!(scale > 0. && scale <= 10000., "invalid rope UV scale");
            let flag = |name: &str, default| -> Result<bool> {
                if value[name].is_null() {
                    Ok(default)
                } else {
                    value[name]
                        .as_bool()
                        .ok_or_else(|| anyhow::anyhow!("invalid rope {name}"))
                }
            };
            Ok(Self {
                subdivision: subdivision as usize,
                scale,
                scrolling: flag("uvscrolling", false)?,
                smoothing: flag("uvsmoothing", true)?,
                fade_alpha: trail && flag("fadealpha", false)?,
                fade_size: trail && flag("fadesize", false)?,
            })
        }
        pub fn steps(self) -> usize {
            self.subdivision + 1
        }
    }
    #[derive(Clone, Copy, Debug)]
    pub(super) struct Point {
        pub position: Vec3,
        pub size: f32,
        pub color: Vec4,
        phase: f32,
        life: f32,
        uv: f32,
    }
    impl Point {
        fn particle(p: &Particle) -> Self {
            Self {
                position: p.position,
                size: p.size,
                color: p.color,
                phase: p.age / p.life,
                life: p.life,
                uv: 0.,
            }
        }
    }
    #[derive(Default)]
    pub(super) struct Scratch {
        source: Vec<Point>,
        samples: Vec<history::Sample>,
        pub points: Vec<Point>,
    }
    // The installed WE geometry shader uses cubic Bezier controls at 0.15 of
    // neighboring tangents and smoothstep parameter spacing (TRAILSUBDIVISION + 1).
    fn curve(points: [Vec3; 4], t: f32) -> Vec3 {
        let [a, b, c, d] = points;
        let before = b + (c - a) * 0.15;
        let after = c + (b - d) * 0.15;
        let u = 1. - t;
        b * (u * u * u) + before * (3. * t * u * u) + after * (3. * t * t * u) + c * (t * t * t)
    }

    impl Scratch {
        pub fn chain(&mut self, particles: &[Particle], config: Config) {
            self.source.clear();
            self.source.extend(particles.iter().map(Point::particle));
            self.subdivide(config);
        }
        pub fn trail(&mut self, particle: &Particle, history: usize, config: Config) {
            particle.trails[history].points(&mut self.samples);
            self.source.clear();
            let duration = self.samples.last().map_or(1., |p| p.time)
                - self.samples.first().map_or(0., |p| p.time);
            for sample in &self.samples {
                let mut point = Point::particle(particle);
                point.position = sample.position;
                point.phase = (sample.time / duration.max(1e-9)) as f32;
                self.source.push(point);
            }
            self.subdivide(config);
        }
        fn subdivide(&mut self, config: Config) {
            self.points.clear();
            if self.source.len() < 2 {
                return;
            }
            for i in 0..self.source.len() - 1 {
                let a = self.source[i.saturating_sub(1)];
                let b = self.source[i];
                let c = self.source[i + 1];
                let d = self.source[(i + 2).min(self.source.len() - 1)];
                for sub in 0..config.steps() {
                    let t = sub as f32 / config.steps() as f32;
                    let t = t * t * (3. - 2. * t);
                    self.points.push(Point {
                        position: curve([a.position, b.position, c.position, d.position], t),
                        size: b.size + (c.size - b.size) * t,
                        color: b.color.lerp(c.color, t),
                        phase: b.phase + (c.phase - b.phase) * t,
                        life: b.life,
                        uv: 0.,
                    });
                }
            }
            self.points.push(*self.source.last().unwrap());
            let smoothing = config.smoothing
                && !config.scrolling
                && self.source.iter().all(|p| p.life == self.source[0].life);
            let length: f32 = self
                .points
                .windows(2)
                .map(|p| p[0].position.distance(p[1].position))
                .sum();
            let mut distance = 0.;
            let segments = self.points.len() - 1;
            for i in 0..self.points.len() {
                if i > 0 {
                    distance += self.points[i - 1]
                        .position
                        .distance(self.points[i].position);
                }
                let point = &mut self.points[i];
                let progress = if smoothing && length > 1e-8 {
                    distance / length
                } else {
                    i as f32 / segments as f32
                };
                point.uv = config.scale
                    * if config.scrolling {
                        point.phase
                    } else {
                        1. - progress
                    };
                let fade = (std::f32::consts::PI * progress).sin().max(0.);
                if config.fade_alpha {
                    point.color.w *= fade;
                }
                if config.fade_size {
                    point.size *= fade;
                }
            }
        }
        pub fn segments(
            &self,
        ) -> impl Iterator<Item = (usize, &Point, &Point, Vec3, Vec3, [f32; 2])> {
            self.points
                .windows(2)
                .enumerate()
                .filter(|(_, p)| p[0].position.distance_squared(p[1].position) > 1e-12)
                .map(|(i, p)| {
                    (
                        i,
                        &p[0],
                        &p[1],
                        self.points[i.saturating_sub(1)].position,
                        self.points[(i + 2).min(self.points.len() - 1)].position,
                        [p[0].uv, p[1].uv],
                    )
                })
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;
        #[test]
        fn curved_subdivision_uv_arc_length_scroll_and_bounds() {
            let mut scratch = Scratch {
                source: [
                    Vec3::ZERO,
                    Vec3::new(4., 0., 0.),
                    Vec3::new(4., 4., 0.),
                    Vec3::new(8., 4., 0.),
                ]
                .into_iter()
                .enumerate()
                .map(|(i, position)| Point {
                    position,
                    size: 2. + i as f32,
                    color: Vec4::splat(i as f32 / 3.),
                    phase: 1. - i as f32 / 4.,
                    life: 2.,
                    uv: 0.,
                })
                .collect(),
                ..Default::default()
            };
            let config = Config::parse(&json!({"subdivision":4,"uvscale":2}), false).unwrap();
            scratch.subdivide(config);
            assert_eq!(scratch.points.len(), 16);
            assert_eq!(scratch.points[5].position, Vec3::new(4., 0., 0.));
            assert!(
                scratch.points[2].position.y < -0.1,
                "a straight chain is not a spline"
            );
            assert_eq!(scratch.points[0].uv, 2.);
            assert_eq!(scratch.points[15].uv, 0.);
            let arc_total: f32 = scratch
                .points
                .windows(2)
                .map(|p| p[0].position.distance(p[1].position))
                .sum();
            let arc_first: f32 = scratch.points[..6]
                .windows(2)
                .map(|p| p[0].position.distance(p[1].position))
                .sum();
            assert!((scratch.points[5].uv - 2. * (1. - arc_first / arc_total)).abs() < 1e-5);
            scratch.source[1].life = 3.;
            scratch.subdivide(config);
            assert!((scratch.points[5].uv - 4. / 3.).abs() < 1e-5);
            let scrolling = Config::parse(&json!({"uvscrolling":true,"uvscale":2}), false).unwrap();
            scratch.subdivide(scrolling);
            assert_eq!(scratch.points[5].uv, 1.5);
            assert!(Config::parse(&json!({"subdivision":65}), false).is_err());
            assert!(Config::parse(&json!({"subdivision":1.5}), false).is_err());
        }
    }
}
mod simulation;
pub(crate) const INSTANCE_FLOATS: usize = instances::FLOATS;

#[derive(Clone, Copy)]
enum Render {
    Sprite,
    Trail {
        length: f32,
        max: f32,
        min: f32,
    },
    Rope(ribbon::Config),
    RopeTrail {
        config: ribbon::Config,
        history: usize,
        segments: usize,
    },
}
struct Draw {
    node: usize,
    pass: Pass,
    mesh: Mesh,
    render: Render,
    instances: Vec<f32>,
    layout: instances::Layout,
    ribbon: ribbon::Scratch,
    blend: String,
    animation: animation::Animation,
    orientation: orientation::Orientation,
    perspective: bool,
    world_space: bool,
}
pub(crate) struct Renderer {
    family: family::Family,
    draws: Vec<Draw>,
    pub state: State,
    pub index: usize,
    created_at: f32,
    perspective: bool,
    connections: connections::Connections,
}
impl Renderer {
    pub fn resources(&self, usage: &mut crate::gpu::Usage) {
        for draw in &self.draws {
            usage.pass(&draw.pass);
            usage.mesh(&draw.mesh);
            // Reserve the reflected layout, not unused shader attributes.
            let instances = self.family.node_capacity(draw.node) * draw.steps();
            usage.bytes += (instances as u64 * draw.layout.floats as u64 * 4)
                .saturating_sub(draw.mesh.instance_byte_size());
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn load(
        gl: Rc<glow::Context>,
        assets: &Assets,
        file: &str,
        object: &Value,
        index: usize,
        state: State,
        properties: &Properties,
        textures: &mut HashMap<AssetKey, Rc<Texture>>,
        budget: &mut u64,
    ) -> Result<Self> {
        let object = resolve(object, properties)?;
        let (family, definitions) =
            family::Family::load(assets, file, &object, properties, index as u64 + 0x57455)?;
        let mut draws = Vec::new();
        for (node, definition) in definitions.iter().enumerate() {
            draws.extend(Draw::load(
                gl.clone(),
                assets,
                definition,
                properties,
                textures,
                budget,
                node,
            )?);
            ensure!(draws.len() <= 1024, "too many particle material draws");
        }
        let renderer = Self {
            connections: connections::Connections::parse(&object)?,
            perspective: animation::flags(&definitions[0])? & 4 != 0,
            family,
            draws,
            state,
            index,
            created_at: object["__createdAt"].as_f64().unwrap_or(0.) as f32,
        };
        ensure!(
            renderer.capacity() <= 100_000,
            "particle draw capacity exceeds 100000"
        );
        Ok(renderer)
    }
    pub fn capacity(&self) -> usize {
        self.family.capacity().max(
            self.draws
                .iter()
                .map(|d| self.family.node_capacity(d.node) * d.steps())
                .sum(),
        )
    }
    pub fn changed(&self, properties: &Properties) -> Result<bool> {
        self.draws.iter().try_fold(false, |changed, draw| {
            Ok(changed || draw.pass.changed(properties)?)
        })
    }
    pub fn prepare_properties(&self, properties: &Properties) -> Result<Vec<Vec<f32>>> {
        self.draws
            .iter()
            .map(|d| d.pass.prepare(properties))
            .collect::<Result<Vec<_>>>()
            .map(|values| values.into_iter().flatten().collect())
    }
    pub fn apply_properties(&mut self, values: Vec<Vec<f32>>) {
        let mut values = values.into_iter();
        for draw in &mut self.draws {
            draw.pass
                .apply(values.by_ref().take(draw.pass.value_count()).collect());
        }
    }
    pub fn initialized(&self) -> bool {
        self.family.initialized()
    }
    pub fn audio(&self) -> bool {
        self.family.audio()
            || self.family.is_playing() && self.draws.iter().any(|d| d.pass.requires_audio())
    }
    pub fn pointer(&self) -> bool {
        self.family.pointer() || self.draws.iter().any(|d| d.pass.accepts_pointer())
    }
    pub fn ready(&self) -> bool {
        self.family.ready()
    }
    pub fn perspective(&self) -> bool {
        self.state.perspective || self.perspective
    }
    pub fn set_bounds(&mut self, size: [f32; 2]) {
        self.family.set_bounds(size);
    }
    pub fn has_model_connections(&self) -> bool {
        self.connections.has_models()
    }
    pub fn set_models(&mut self, sources: &HashMap<String, Rc<[crate::mdl::Capsule]>>) {
        self.family.set_models(self.connections.models(sources));
    }
    pub fn image_requests(&self) -> impl Iterator<Item = (&str, bool)> {
        self.connections
            .image_requests()
            .map(|(index, id)| (id.as_str(), self.family.image_periodic(*index)))
    }
    pub fn image_demand(&self) -> bool {
        self.family.image_demand()
    }
    pub fn set_images(&mut self, sources: &HashMap<String, Rc<emission::Snapshot>>) {
        if self.connections.image_requests().next().is_none() {
            return;
        }
        self.family.set_images(self.connections.images(sources));
    }
    pub fn advance(
        &mut self,
        time: f32,
        pointer: Vec3,
        object: &Value,
        audio: &crate::audio::AudioSnapshot,
        host: bool,
        view: crate::camera::View,
    ) -> Result<Vec<Value>> {
        let mut changes = Vec::new();
        if let Some(commands) = object["__particle"]["commands"].as_array()
            && !commands.is_empty()
        {
            self.family.control(&object["__particle"]["commands"])?;
            changes.push(json!([self.index, ["__particle", "commands"], []]));
        }
        self.family.set_overrides(&object["instanceoverride"])?;
        ensure!(
            self.capacity() <= 100_000,
            "particle draw capacity exceeds 100000"
        );
        let inverse = if self.state.transform.determinant().abs() > 1e-12 {
            self.state.transform.inverse()
        } else {
            Mat4::IDENTITY
        };
        // Particle definitions and scene transforms share WE's Y-up coordinates.
        let local = inverse.transform_point3(pointer);
        self.family.advance(
            (time - self.created_at).max(0.) as f64,
            local,
            inverse,
            self.state.transform,
            audio,
        );
        if host {
            for (key, value) in [
                ("playing", json!(self.family.is_playing())),
                ("live", json!(self.family.live())),
            ] {
                if object["__particle"][key] != value {
                    changes.push(json!([self.index, ["__particle", key], value]));
                }
            }
        }
        if !self.family.ready() {
            return Ok(changes);
        }
        for draw in &mut self.draws {
            let node = &self.family.nodes[draw.node];
            draw.instances.clear();
            let frames = draw.pass.textures[0]
                .as_ref()
                .map_or(&[][..], |texture| texture.sequence_frames(0));
            for system in &node.instances {
                let color = self.state.color * node.color * system.system.color_multiplier();
                let placement = if draw.world_space {
                    draw.orientation
                        .placement(Mat4::IDENTITY, Mat4::IDENTITY, view)
                } else {
                    draw.orientation
                        .placement(self.state.transform, system.matrix, view)
                };
                match draw.render {
                    Render::Rope(config) => {
                        draw.ribbon.chain(&system.system.particles, config);
                        let mut previous_particle = None;
                        let mut data = [0.; INSTANCE_FLOATS];
                        for (i, start, end, previous, after, uv) in draw.ribbon.segments() {
                            let index = i / config.steps();
                            if previous_particle != Some(index) {
                                data = instances::data(
                                    &system.system.particles[index],
                                    None,
                                    color,
                                    frames,
                                    draw.animation,
                                    placement,
                                );
                                previous_particle = Some(index);
                            }
                            instances::ribbon(
                                &mut draw.instances,
                                &draw.layout,
                                data,
                                start,
                                end,
                                previous,
                                after,
                                uv,
                                color,
                            );
                        }
                    }
                    Render::RopeTrail {
                        config, history, ..
                    } => {
                        for particle in &system.system.particles {
                            draw.ribbon.trail(particle, history, config);
                            let data = instances::data(
                                particle,
                                None,
                                color,
                                frames,
                                draw.animation,
                                placement,
                            );
                            for (_, start, end, previous, after, uv) in draw.ribbon.segments() {
                                instances::ribbon(
                                    &mut draw.instances,
                                    &draw.layout,
                                    data,
                                    start,
                                    end,
                                    previous,
                                    after,
                                    uv,
                                    color,
                                );
                            }
                        }
                    }
                    _ => {
                        for p in &system.system.particles {
                            let data =
                                instances::data(p, None, color, frames, draw.animation, placement);
                            draw.layout.append(&mut draw.instances, &data);
                        }
                    }
                }
            }
            draw.mesh.upload_particles(&draw.instances)?;
        }
        Ok(changes)
    }
    pub fn references(&self) -> impl Iterator<Item = &str> {
        self.draws
            .iter()
            .flat_map(|d| d.pass.references.iter().filter_map(Option::as_deref))
    }
    pub fn draw<'a>(
        &'a self,
        gl: &glow::Context,
        frame: &Frame<'_>,
        scene: impl Fn(&str) -> Result<Option<&'a Texture>>,
        view: crate::camera::View,
    ) -> Result<()> {
        for draw in &self.draws {
            let matrix = view.matrix(self.perspective() || draw.perspective);
            let transform = if draw.world_space {
                Mat4::IDENTITY
            } else {
                self.state.transform
            };
            let model = transform;
            let mut frame = *frame;
            frame.projection = matrix * transform;
            draw.draw(gl, matrix * model, &frame, &scene, model, view)?;
        }
        Ok(())
    }
}

pub(crate) fn validate_controls(object: &Value) -> Result<()> {
    if !object["__particle"].is_null() {
        family::Family::validate_commands(&object["__particle"]["commands"])?;
        simulation::validate_overrides(&object["instanceoverride"])?;
    }
    Ok(())
}

impl Draw {
    fn steps(&self) -> usize {
        match self.render {
            Render::Rope(config) => config.steps(),
            Render::RopeTrail {
                config, segments, ..
            } => (segments + 1) * config.steps(),
            _ => 1,
        }
    }
    pub(super) fn load(
        gl: Rc<glow::Context>,
        assets: &Assets,
        definition: &Value,
        properties: &Properties,
        textures: &mut HashMap<AssetKey, Rc<Texture>>,
        budget: &mut u64,
        node: usize,
    ) -> Result<Vec<Self>> {
        let renderers = definition["renderer"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![json!({"name":"sprite"})]);
        ensure!(renderers.len() <= 16, "too many particle renderers");
        if renderers.is_empty() {
            return Ok(Vec::new());
        }
        let material = assets.json(
            definition["material"]
                .as_str()
                .context("particle material")?,
        )?;
        let passes = material["passes"]
            .as_array()
            .context("particle material passes")?;
        ensure!(
            !passes.is_empty() && passes.len() <= 16,
            "invalid particle material pass count"
        );
        let mut draws = Vec::new();
        let mut history = 0;
        let animation = animation::Animation::parse(definition)?;
        for renderer in renderers {
            let orientation = orientation::Orientation::parse(&renderer)?;
            let render = match renderer["name"].as_str().unwrap_or("sprite") {
                "sprite" => Render::Sprite,
                "rope" => Render::Rope(ribbon::Config::parse(&renderer, false)?),
                "ropetrail" => {
                    let result = Render::RopeTrail {
                        config: ribbon::Config::parse(&renderer, true)?,
                        history,
                        segments: history::Config::parse(&renderer)?.segments,
                    };
                    history += 1;
                    result
                }
                "spritetrail" => Render::Trail {
                    length: crate::scene::bindings::components(&renderer["length"], &[0.05], 1)?[0],
                    max: crate::scene::bindings::components(&renderer["maxlength"], &[10.0], 1)?[0],
                    min: crate::scene::bindings::components(&renderer["minlength"], &[0.0], 1)?[0],
                },
                other => anyhow::bail!("unsupported particle renderer {other}"),
            };
            for source in passes {
                let mut spec = source.clone();

                let rope = matches!(render, Render::Rope(_) | Render::RopeTrail { .. });

                if rope {
                    spec["shader"] = json!("genericropeparticle");
                }
                if spec["combos"].is_null() {
                    spec["combos"] = json!({});
                }
                spec["combos"]["TRAILRENDERER"] =
                    json!(matches!(render, Render::Trail { .. }) as i32);
                let blend = crate::gpu::blend_name(&spec)?;
                let pass = Pass::load(
                    gl.clone(),
                    assets,
                    &spec,
                    properties,
                    &HashSet::new(),
                    textures,
                    budget,
                    crate::shader::MaterialDomain::Particle {
                        rope,
                        frame_blend: animation.blend,
                    },
                )?;
                let (mesh, layout) = Mesh::particles(gl.clone(), &pass)?;
                draws.push(Self {
                    node,
                    pass,
                    mesh,
                    render,
                    instances: Vec::new(),
                    layout,
                    ribbon: ribbon::Scratch::default(),
                    blend,
                    animation,
                    orientation,
                    perspective: animation::flags(definition)? & 4 != 0,
                    world_space: animation::flags(definition)? & 1 != 0,
                });
            }
        }
        Ok(draws)
    }
    pub(super) fn draw<'a>(
        &'a self,
        gl: &glow::Context,
        matrix: Mat4,
        frame: &Frame<'_>,
        scene: &impl Fn(&str) -> Result<Option<&'a Texture>>,
        model: Mat4,
        view: crate::camera::View,
    ) -> Result<()> {
        unsafe {
            if self.blend == "normal" {
                if frame.composite {
                    gl.enable(glow::BLEND);
                    gl.blend_func_separate(glow::SRC_ALPHA, glow::ZERO, glow::ONE, glow::ZERO);
                } else {
                    gl.disable(glow::BLEND);
                }
            } else {
                gl.enable(glow::BLEND);
                gl.blend_func_separate(
                    glow::SRC_ALPHA,
                    if self.blend == "additive" {
                        glow::ONE
                    } else {
                        glow::ONE_MINUS_SRC_ALPHA
                    },
                    glow::ONE,
                    glow::ONE_MINUS_SRC_ALPHA,
                );
            }
        }
        self.pass.vector3("g_ViewRight", view.basis.x_axis);
        self.pass.vector3("g_ViewUp", view.basis.y_axis);
        self.pass.vector3("g_EyePosition", view.eye);
        self.pass.matrix4(
            "g_ModelMatrixInverse",
            if model.determinant().abs() > 1e-12 {
                model.inverse()
            } else {
                Mat4::IDENTITY
            },
        );
        self.pass.matrix4("g_ModelMatrix", model);
        self.pass
            .vector4("g_RenderVar1", Vec4::new(1.0, 1.0, 1.0, 1.0));
        if let Some(texture) = &self.pass.textures[0]
            && let Some(frame) = texture.frames.first()
        {
            self.pass.vector4(
                "g_RenderVar1",
                Vec4::new(
                    1.0,
                    1.0,
                    texture.sequence_frames(0).len() as f32,
                    frame.ratio,
                ),
            );
        }
        if let Render::Trail { length, max, min } = self.render {
            self.pass
                .vector4("g_RenderVar0", Vec4::new(length, max, min, 0.0));
        }
        let mut inputs = [None; 8];
        for (slot, name) in self.pass.references.iter().enumerate() {
            if let Some(name) = name {
                inputs[slot] = scene(name)?;
            }
        }
        self.pass
            .draw(&self.mesh, matrix, frame, &inputs, Vec4::ONE);
        Ok(())
    }
}
