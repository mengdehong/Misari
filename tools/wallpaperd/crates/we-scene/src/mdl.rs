//! WE model data stays independent of GLES and SceneScript handles.
mod animation;
mod collision;
pub(crate) use collision::Capsule;
mod parse;
mod physics;
mod render;
pub(crate) use animation::{Rig, validate_controls};
pub(crate) use render::Renderer;
#[cfg(test)]
pub(crate) mod tests;
use glam::{Mat4, Vec2, Vec3, Vec4};
pub(crate) use parse::parse;
#[derive(Clone)]
pub(crate) struct Vertex {
    pub position: Vec3,
    pub normal: Vec3,
    pub tangent: Vec4,
    pub uv: Vec2,
    pub uv2: Vec2,
    pub bones: [usize; 4],
    pub weights: Vec4,
}
pub(crate) struct Part {
    pub id: u32,
    pub start: usize,
    pub count: usize,
    pub offset: i32,
}
pub(crate) struct Geometry {
    pub materials: Vec<String>,
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub parts: Vec<Part>,
}
pub(crate) struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    pub local: Mat4,
    pub simulation: serde_json::Value,
}
#[derive(Clone)]
pub(crate) struct Key {
    pub translation: Vec3,
    pub angles: Vec3,
    pub scale: Vec3,
}
pub(crate) struct Clip {
    pub id: u32,
    pub name: String,
    pub mode: String,
    pub fps: f32,
    pub frames: usize,
    pub tracks: Vec<Vec<Key>>,
    pub events: Vec<(f32, String)>,
}
pub(crate) struct Attachment {
    pub name: String,
    pub bone: usize,
    pub matrix: Mat4,
}
pub(crate) struct Model {
    pub meshes: Vec<Geometry>,
    pub bones: Vec<Bone>,
    pub clips: Vec<Clip>,
    pub rest: Option<Vec<Mat4>>,
    pub attachments: Vec<Attachment>,
}
