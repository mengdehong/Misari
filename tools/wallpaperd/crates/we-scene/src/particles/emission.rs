//! Shared alpha-weighted layer bitmaps and UV-to-puppet bindings.
use anyhow::{Result, ensure};
use glam::{Mat4, Vec2, Vec3, Vec4};
use rand_chacha::ChaCha8Rng;
use rand_core::Rng;
use std::{collections::HashMap, rc::Rc};

pub(crate) const MAX_EDGE: u32 = 1024;
pub(crate) type Sources = Rc<HashMap<usize, Rc<Snapshot>>>;
pub(crate) struct Bitmap {
    pub size: [u32; 2],
    rgba: Vec<u8>,
    cumulative: Vec<u64>,
}
impl Bitmap {
    pub fn opaque(&self) -> bool {
        self.cumulative.last().is_some_and(|n| *n != 0)
    }
    pub fn new(size: [u32; 2], rgba: Vec<u8>) -> Result<Self> {
        ensure!(
            size.iter().all(|n| *n > 0 && *n <= MAX_EDGE),
            "particle emission bitmap exceeds limit"
        );
        ensure!(
            rgba.len() == size[0] as usize * size[1] as usize * 4,
            "invalid particle emission bitmap"
        );
        let mut total = 0;
        let cumulative = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                total += p[3] as u64;
                total
            })
            .collect();
        Ok(Self {
            size,
            rgba,
            cumulative,
        })
    }
    pub fn bytes(&self) -> usize {
        self.rgba.len() + self.cumulative.len() * 8
    }
    fn sample(&self, rng: &mut ChaCha8Rng) -> Option<(Vec2, Vec4)> {
        let total = *self.cumulative.last()?;
        if total == 0 {
            return None;
        }
        // Rejection avoids modulo bias even when a bitmap has just one opaque texel.
        let bound = total.wrapping_neg() % total;
        let mut number = rng.next_u64();
        while number < bound {
            number = rng.next_u64();
        }
        let i = self.cumulative.partition_point(|n| *n <= number % total);
        let random = |rng: &mut ChaCha8Rng| (rng.next_u32() >> 8) as f32 / 16_777_216.;
        let uv = Vec2::new(
            (i % self.size[0] as usize) as f32 + random(rng),
            (i / self.size[0] as usize) as f32 + random(rng),
        ) / Vec2::from_array(self.size.map(|n| n as f32));
        let rgba: [u8; 4] = self.rgba[i * 4..i * 4 + 4].try_into().unwrap();
        Some((uv, Vec4::from_array(rgba.map(|n| n as f32 / 255.))))
    }
}
const GRID: usize = 32;
pub(crate) struct UvMesh {
    triangles: Vec<([usize; 3], [Vec2; 3])>,
    cells: Vec<Vec<usize>>,
}
impl UvMesh {
    pub fn bytes(&self) -> usize {
        self.triangles.len() * std::mem::size_of::<([usize; 3], [Vec2; 3])>()
            + self
                .cells
                .iter()
                .map(|c| c.len() * std::mem::size_of::<usize>() + std::mem::size_of::<Vec<usize>>())
                .sum::<usize>()
    }
    pub fn new(model: &crate::mdl::Model) -> Result<Self> {
        let mut mesh = Self {
            triangles: Vec::new(),
            cells: vec![Vec::new(); GRID * GRID],
        };
        let mut offset = 0;
        let mut entries = 0;
        for geometry in &model.meshes {
            for indices in geometry.indices.as_chunks::<3>().0 {
                let uv = std::array::from_fn(|i| geometry.vertices[indices[i] as usize].uv);
                if (uv[1] - uv[0]).perp_dot(uv[2] - uv[0]).abs() < 1e-12 {
                    continue;
                }
                let id = mesh.triangles.len();
                mesh.triangles
                    .push((std::array::from_fn(|i| offset + indices[i] as usize), uv));
                let min = uv
                    .into_iter()
                    .fold(Vec2::ONE, Vec2::min)
                    .clamp(Vec2::ZERO, Vec2::ONE);
                let max = uv
                    .into_iter()
                    .fold(Vec2::ZERO, Vec2::max)
                    .clamp(Vec2::ZERO, Vec2::ONE);
                let cell = |n: f32| ((n * GRID as f32) as usize).min(GRID - 1);
                for y in cell(min.y)..=cell(max.y) {
                    for x in cell(min.x)..=cell(max.x) {
                        let list = &mut mesh.cells[y * GRID + x];
                        ensure!(
                            list.len() < 128 && entries < 1_048_576,
                            "particle emission UV lookup exceeds limit"
                        );
                        list.push(id);
                        entries += 1;
                    }
                }
            }
            offset += geometry.vertices.len();
        }
        Ok(mesh)
    }
    fn point(&self, uv: Vec2, positions: &[Vec3]) -> Option<Vec3> {
        let x = ((uv.x * GRID as f32) as usize).min(GRID - 1);
        let y = ((uv.y * GRID as f32) as usize).min(GRID - 1);
        for &id in &self.cells[y * GRID + x] {
            let (indices, [a, b, c]) = self.triangles[id];
            let d = (b - a).perp_dot(c - a);
            let v = (uv - a).perp_dot(c - a) / d;
            let w = (b - a).perp_dot(uv - a) / d;
            if v >= -1e-6 && w >= -1e-6 && v + w <= 1. + 1e-6 {
                return Some(
                    positions[indices[0]] * (1. - v - w)
                        + positions[indices[1]] * v
                        + positions[indices[2]] * w,
                );
            }
        }
        None
    }
}
#[derive(Clone)]
pub(crate) struct Pose {
    pub transform: Mat4,
    pub size: Vec2,
    pub positions: Option<Rc<[Vec3]>>,
}
pub(crate) struct Snapshot {
    pub bitmap: Rc<Bitmap>,
    pub mesh: Option<Rc<UvMesh>>,
    pub pose: Pose,
    pub previous: Option<(Pose, f32)>,
}
impl Snapshot {
    fn point(&self, uv: Vec2, pose: &Pose) -> Option<Vec3> {
        let local = match (&self.mesh, &pose.positions) {
            (Some(mesh), Some(positions)) => mesh.point(uv, positions)?,
            _ => Vec3::new((uv.x - 0.5) * pose.size.x, (0.5 - uv.y) * pose.size.y, 0.),
        };
        Some(pose.transform.transform_point3(local))
    }
    pub fn sample(&self, rng: &mut ChaCha8Rng) -> Option<(Vec3, Vec3, Vec4)> {
        // UV holes are not particle sources. A bounded retry also handles irregular puppets.
        for _ in 0..32 {
            let (uv, color) = self.bitmap.sample(rng)?;
            if let Some(point) = self.point(uv, &self.pose) {
                let velocity = self
                    .previous
                    .as_ref()
                    .and_then(|(pose, dt)| self.point(uv, pose).map(|old| (point - old) / *dt))
                    .unwrap_or(Vec3::ZERO);
                return Some((point, velocity, color));
            }
        }
        None
    }
}
pub(crate) struct Cache {
    pub source: String,
    pub bitmap: Rc<Bitmap>,
    pub mesh: Option<Rc<UvMesh>>,
    pub time: f32,
    pub snapshot: Rc<Snapshot>,
    pub captured: f32,
    pub periodic: bool,
}
impl Cache {
    pub fn bytes(&self) -> usize {
        let positions = &self.snapshot.pose.positions;
        let mut bytes = self.bitmap.bytes()
            + self.mesh.as_ref().map_or(0, |m| m.bytes())
            + positions
                .as_ref()
                .map_or(0, |p| p.len() * std::mem::size_of::<Vec3>());
        if let Some((pose, _)) = &self.snapshot.previous
            && let Some(previous) = &pose.positions
            && positions.as_ref().is_none_or(|p| !Rc::ptr_eq(p, previous))
        {
            bytes += previous.len() * std::mem::size_of::<Vec3>();
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::SeedableRng;
    #[test]
    fn audio_emission_requires_an_active_opaque_source_and_tracks_each_emitter() {
        use super::super::simulation::System;
        use serde_json::json;
        let definition = json!({"maxcount":4,"emitter":[{"name":"layerimage","rate":10,"audioprocessingmode":1},{"name":"boxrandom","instantaneous":1,"rate":0,"duration":0.1}]});
        let mut system = System::new(&definition, &json!(null), 0).unwrap();
        assert!(
            !system.needs_audio(),
            "an unrelated silent box must not keep an unconnected image emitter capturing"
        );
        let source = |alpha| {
            Rc::new(Snapshot {
                bitmap: Rc::new(Bitmap::new([1, 1], vec![255, 255, 255, alpha]).unwrap()),
                mesh: None,
                pose: Pose {
                    transform: Mat4::IDENTITY,
                    size: Vec2::ZERO,
                    positions: None,
                },
                previous: None,
            })
        };
        system.images = Rc::new(HashMap::from([(0, source(255))]));
        assert!(system.needs_audio());
        system.images = Rc::new(HashMap::from([(0, source(0))]));
        assert!(!system.needs_audio());
        assert!(
            system.image_demand(),
            "periodic refresh must remain possible while its current mask is transparent"
        );
        system.images = Rc::new(HashMap::from([(0, source(255))]));
        system.emitting = false;
        assert!(!system.needs_audio());
        assert!(!system.image_demand());
        system.forced = 1;
        assert!(
            system.needs_audio(),
            "explicit births still use the first emitter's audio"
        );
    }
    #[test]
    fn image_birth_is_absolute_and_motion_color_and_missing_sources_are_bounded() {
        use super::super::simulation::System;
        use serde_json::json;
        let definition = json!({"maxcount":4,"emitter":[{"name":"layerimage","flags":17,"speedmin":2,"speedmax":2,"instantaneous":4,"rate":0}],"initializer":[{"name":"lifetimerandom","min":1,"max":1},{"name":"alpharandom","min":0.25,"max":0.25}],"operator":[{"name":"movement"}]});
        let bitmap = Rc::new(Bitmap::new([1, 1], vec![10, 20, 30, 255]).unwrap());
        let pose = |x| Pose {
            transform: Mat4::from_translation(Vec3::new(x, -8., 0.)),
            size: Vec2::ZERO,
            positions: None,
        };
        let snapshot = Rc::new(Snapshot {
            bitmap,
            mesh: None,
            pose: pose(16.),
            previous: Some((pose(8.), 0.5)),
        });
        let mut system = System::new(&definition, &json!(null), 9).unwrap();
        system.images = Rc::new(HashMap::from([(0, snapshot)]));
        system.set_space(Mat4::from_translation(Vec3::new(4., -2., 0.)));
        let audio = crate::audio::AudioSnapshot::default();
        system.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
        assert_eq!(system.particles.len(), 4);
        for p in &system.particles {
            assert_eq!(p.position, Vec3::new(12., -6., 0.));
            assert_eq!(p.velocity, Vec3::X * 32.);
            assert_eq!(p.color, Vec4::new(10. / 255., 20. / 255., 30. / 255., 0.25));
        }
        let initial = system.particles.clone();
        system.advance(0.5, Vec3::ZERO, Mat4::IDENTITY, &audio);
        assert!(
            system
                .particles
                .iter()
                .all(|p| (p.position.x - 28.).abs() < 1e-4)
        );
        system.advance(0., Vec3::ZERO, Mat4::IDENTITY, &audio);
        assert_eq!(system.particles, initial);
        system.set_overrides(&json!({"colorn":"0.5 1 1"})).unwrap();
        assert!(
            system
                .particles
                .iter()
                .all(|p| p.color == Vec4::new(0.5, 1., 1., 0.25))
        );
        system.images = Default::default();
        system.advance(1., Vec3::ZERO, Mat4::IDENTITY, &audio);
        system.forced = 4;
        system.step_tick(0., &audio);
        assert!(system.particles.is_empty());
        assert!(system.events.born.is_empty());
        let mut invalid = definition;
        invalid["emitter"][0]["flags"] = json!(-1);
        assert!(System::new(&invalid, &json!(null), 0).is_err());
    }
    #[test]
    fn uv_grid_interpolates_current_vertices_without_traversing_the_whole_mesh() {
        let mut model = Rc::try_unwrap(crate::mdl::tests::fixture()).ok().unwrap();
        let geometry = &mut model.meshes[0];
        let template = geometry.vertices[0].clone();
        geometry.vertices = [Vec2::ZERO, Vec2::X, Vec2::Y, Vec2::ONE]
            .into_iter()
            .map(|uv| crate::mdl::Vertex {
                uv,
                position: Vec3::new(uv.x * 8., uv.y * 8., 0.),
                ..template.clone()
            })
            .collect();
        geometry.indices = vec![0, 2, 1, 1, 2, 3];
        let mesh = UvMesh::new(&model).unwrap();
        let positions = model.meshes[0]
            .vertices
            .iter()
            .map(|v| v.position + Vec3::X * 16.)
            .collect::<Vec<_>>();
        assert!(
            (mesh.point(Vec2::new(0.25, 0.5), &positions).unwrap() - Vec3::new(18., 4., 0.))
                .length()
                < 1e-5
        );
        assert!(
            (mesh.point(Vec2::new(0.75, 0.5), &positions).unwrap() - Vec3::new(22., 4., 0.))
                .length()
                < 1e-5
        );
        assert!(mesh.bytes() < 64 * 1024);
    }
    #[test]
    fn alpha_weighted_mask_color_motion_and_transparent_release() {
        let bitmap = Rc::new(
            Bitmap::new(
                [2, 2],
                vec![255, 0, 0, 0, 0, 255, 0, 255, 0, 0, 255, 0, 0, 0, 0, 0],
            )
            .unwrap(),
        );
        let pose = |x| Pose {
            transform: Mat4::from_translation(Vec3::X * x),
            size: Vec2::splat(8.),
            positions: None,
        };
        let snapshot = Snapshot {
            bitmap: bitmap.clone(),
            mesh: None,
            pose: pose(12.),
            previous: Some((pose(8.), 0.25)),
        };
        let mut rng = ChaCha8Rng::seed_from_u64(19);
        for _ in 0..256 {
            let (p, v, c) = snapshot.sample(&mut rng).unwrap();
            assert!(p.x >= 12. && p.x < 16. && p.y > 0. && p.y <= 4.);
            assert!((v - Vec3::X * 16.).length() < 1e-5);
            assert_eq!(c, Vec4::new(0., 1., 0., 1.));
        }
        let weak = Rc::downgrade(&bitmap);
        drop(bitmap);
        drop(snapshot);
        assert!(weak.upgrade().is_none());
        let empty = Bitmap::new([1, 1], vec![0; 4]).unwrap();
        assert!(empty.sample(&mut rng).is_none());
        assert!(Bitmap::new([MAX_EDGE + 1, 1], vec![]).is_err());
    }
}
