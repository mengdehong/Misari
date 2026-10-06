use super::*;

pub(super) fn fixture(files: serde_json::Value) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let mut pkg = 8u32.to_le_bytes().to_vec();
    pkg.extend(b"PKGV0001");
    pkg.extend(0u32.to_le_bytes());
    std::fs::write(root.path().join("scene.pkg"), pkg).unwrap();
    white(root.path());
    for (name, value) in files.as_object().unwrap() {
        let path = root.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_json(path, value);
    }
    root
}
pub(super) fn white(root: &std::path::Path) {
    // Shipped util/white has 252/255 color and alpha in its lower mip levels.
    // Own a pure white fixture so color/alpha assertions test scene behavior.
    let mut white = b"TEXV0005\0TEXI0001\0".to_vec();
    for word in [0u32, 0, 32, 32, 32, 32, 0] {
        white.extend(word.to_le_bytes());
    }
    white.extend(b"TEXB0001\0");
    for word in [1u32, 1, 32, 32, 32 * 32 * 4] {
        white.extend(word.to_le_bytes());
    }
    white.extend(vec![255; 32 * 32 * 4]);
    std::fs::create_dir_all(root.join("materials/util")).unwrap();
    std::fs::write(root.join("materials/util/white.tex"), white).unwrap();
}
pub(super) fn renderer(root: &tempfile::TempDir, gpu: &Gpu) -> we_scene::Renderer {
    let common = crate::catalog::we_assets(&crate::store::WallpaperEngine::default()).unwrap();
    we_scene::Renderer::load(
        gpu.gl.clone(),
        root.path(),
        &root.path().join("scene.pkg"),
        Some(&common),
    )
    .unwrap()
}
pub(super) fn frame(
    renderer: &mut we_scene::Renderer,
    gpu: &Gpu,
    target: &Target,
    time: f32,
) -> image::RgbaImage {
    renderer
        .draw(
            [gpu.size.width, gpu.size.height],
            Some(target.fbo),
            we_scene::Fit::Stretch,
            time,
        )
        .unwrap();
    gpu.reset();
    gpu.read_image(target).unwrap()
}

pub(super) fn canvas(width: u32, height: u32) -> (Gpu, Target) {
    let gpu = Gpu::headless(crate::pixels::Size::new(width, height).unwrap()).unwrap();
    let target = Target::new(gpu.gl.clone(), gpu.size).unwrap();
    (gpu, target)
}

pub(super) fn write_json(path: impl AsRef<std::path::Path>, value: &serde_json::Value) {
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

pub(super) fn set_properties(renderer: &mut we_scene::Renderer, values: serde_json::Value) {
    renderer
        .set_properties(
            &values
                .as_object()
                .unwrap()
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        )
        .unwrap();
}

// Keep the shared render target explicit at load time; frames use that same target.
pub(super) struct RenderedScene<'a> {
    renderer: we_scene::Renderer,
    gpu: &'a Gpu,
    target: &'a Target,
}

pub(super) fn rendered<'a>(
    root: &tempfile::TempDir,
    gpu: &'a Gpu,
    target: &'a Target,
) -> RenderedScene<'a> {
    RenderedScene {
        renderer: renderer(root, gpu),
        gpu,
        target,
    }
}

impl RenderedScene<'_> {
    pub(super) fn frame(&mut self, time: f32) -> image::RgbaImage {
        frame(&mut self.renderer, self.gpu, self.target, time)
    }
}

impl std::ops::Deref for RenderedScene<'_> {
    type Target = we_scene::Renderer;
    fn deref(&self) -> &Self::Target {
        &self.renderer
    }
}

impl std::ops::DerefMut for RenderedScene<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.renderer
    }
}

pub(super) fn tex_header(header: [u32; 7], mip: [u32; 5]) -> Vec<u8> {
    let mut data = b"TEXV0005\0TEXI0001\0".to_vec();
    for word in header {
        data.extend(word.to_le_bytes());
    }
    data.extend(b"TEXB0001\0");
    for word in mip {
        data.extend(word.to_le_bytes());
    }
    data
}

pub(super) fn assert_gl_clean(gpu: &Gpu) {
    gpu.reset();
    assert_eq!(unsafe { gpu.gl.get_error() }, glow::NO_ERROR);
}
