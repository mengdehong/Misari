//! Wallpaper Engine scene runtime in the caller's GLES context.
//! The context must be current during load, draw, property updates, and drop.
mod animation;
mod assets;
pub mod audio;
mod camera;
mod effect;
mod gpu;
mod lighting;
mod mdl;
mod model_bounds;
mod model_data;
mod particles;
mod postprocess;
mod render;
mod scene;
mod scene_media;
mod script;
mod shader;
mod text;

pub use camera::Fit;
pub use render::Renderer;
pub use scene::bindings::Properties;
pub use script::ScriptStorage;
