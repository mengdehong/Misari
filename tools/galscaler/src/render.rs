use crate::model::Filter;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use smithay::{
    backend::{
        allocator::Fourcc,
        egl::EGLContext,
        renderer::{
            Offscreen,
            gles::{GlesRenderer, GlesTexture},
        },
    },
    utils::{Logical, Physical, Point, Rectangle, Size},
};
use std::{
    ffi::{CString, c_char, c_void},
    ptr::NonNull,
};

unsafe extern "C" {
    fn gs_gpu_create(shaders: *const *const c_char, count: i32) -> *mut c_void;
    fn gs_gpu_destroy(gpu: *mut c_void);
    fn gs_gpu_render(
        gpu: *mut c_void,
        input: u32,
        iw: i32,
        ih: i32,
        output: u32,
        ow: i32,
        oh: i32,
        acquire: *const c_void,
        release: *mut *const c_void,
    ) -> bool;
    fn gs_gpu_stats(
        gpu: *mut c_void,
        passes: *mut i32,
        neural: *mut i32,
        steps: *mut i32,
        gpu_ms: *mut f64,
    );
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub backend: String,
    pub filter: String,
    pub source: [i32; 2],
    pub enhanced: [i32; 2],
    pub display: [i32; 2],
    pub frames: u64,
    pub passes: i32,
    pub neural_passes: i32,
    #[serde(default)]
    pub upscale_steps: i32,
    pub gpu_ms: Option<f64>,
}

/// The same transform drives presentation, black bars and pointer coordinates.
#[derive(Debug, Clone, Copy)]
pub struct View {
    pub source: Rectangle<i32, Logical>,
    pub target: Rectangle<i32, Physical>,
}

pub fn fit(source: Size<i32, Logical>, target: Size<i32, Physical>) -> Rectangle<i32, Physical> {
    let scale = (target.w as f64 / source.w as f64).min(target.h as f64 / source.h as f64);
    let size = Size::from((
        ((source.w as f64 * scale).round() as i32).max(1),
        ((source.h as f64 * scale).round() as i32).max(1),
    ));
    Rectangle::new(
        ((target.w - size.w) / 2, (target.h - size.h) / 2).into(),
        size,
    )
}

impl View {
    pub fn new(source: Rectangle<i32, Logical>, display: Size<i32, Physical>) -> Self {
        Self {
            source,
            target: fit(source.size, display),
        }
    }

    pub fn pointer(self, point: Point<f64, Physical>) -> Option<Point<f64, Logical>> {
        let target = self.target.to_f64();
        if !target.contains(point) {
            return None;
        }
        Some(
            (
                self.source.loc.x as f64
                    + (point.x - target.loc.x) * self.source.size.w as f64 / target.size.w,
                self.source.loc.y as f64
                    + (point.y - target.loc.y) * self.source.size.h as f64 / target.size.h,
            )
                .into(),
        )
    }
}

struct Gpu {
    context: EGLContext,
    handle: NonNull<c_void>,
}

impl Gpu {
    fn new(renderer: &GlesRenderer, filter: Filter) -> Result<Self> {
        let shaders: &[&str] = match filter {
            Filter::Cunny => &[include_str!("shaders/cunny.glsl")],
            Filter::Anime4k => &[
                include_str!("shaders/anime4k-restore.glsl"),
                include_str!("shaders/anime4k-upscale.glsl"),
            ],
            Filter::Linear => &[],
        };
        let shaders: Vec<_> = shaders
            .iter()
            .map(|s| CString::new(*s))
            .collect::<Result<_, _>>()?;
        let pointers: Vec<_> = shaders.iter().map(|s| s.as_ptr()).collect();
        let context =
            EGLContext::new_shared(renderer.egl_context().display(), renderer.egl_context())?;
        // All GPU calls and destruction stay on this compositor thread.
        unsafe {
            context.make_current()?;
        }
        let handle =
            NonNull::new(unsafe { gs_gpu_create(pointers.as_ptr(), pointers.len() as i32) })
                .context("无法初始化原生 shader 处理链路，详情见会话日志")?;
        Ok(Self { context, handle })
    }

    fn run(
        &self,
        renderer: &mut GlesRenderer,
        input: &GlesTexture,
        source: Size<i32, Logical>,
        output: &GlesTexture,
        size: Size<i32, Physical>,
    ) -> Result<(i32, i32, i32, f64)> {
        let fence = renderer.with_context(|gl| unsafe {
            let fence = gl.FenceSync(0x9117, 0); // GL_SYNC_GPU_COMMANDS_COMPLETE
            gl.Flush();
            fence.cast::<c_void>()
        })?;
        let mut release = std::ptr::null();
        let (mut passes, mut neural, mut steps, mut gpu_ms) = (0, 0, 0, 0.0);
        unsafe {
            self.context.make_current()?;
            ensure!(
                gs_gpu_render(
                    self.handle.as_ptr(),
                    input.tex_id(),
                    source.w,
                    source.h,
                    output.tex_id(),
                    size.w,
                    size.h,
                    fence,
                    &mut release
                ),
                "GPU 图像处理失败，禁止静默退回普通缩放；请查看会话日志"
            );
            gs_gpu_stats(
                self.handle.as_ptr(),
                &mut passes,
                &mut neural,
                &mut steps,
                &mut gpu_ms,
            );
        }
        renderer.with_context(|gl| unsafe {
            gl.WaitSync(release as *mut _, 0, u64::MAX);
            gl.DeleteSync(release as *mut _);
        })?;
        Ok((passes, neural, steps, gpu_ms))
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // If the context cannot be made current, leave its driver resources to
        // context destruction instead of issuing GL commands on another context.
        if unsafe { self.context.make_current() }.is_ok() {
            unsafe {
                gs_gpu_destroy(self.handle.as_ptr());
            }
        }
    }
}

pub struct Pipeline {
    gpu: Gpu,
    pub scene: GlesTexture,
    pub enhanced: GlesTexture,
    pub source_size: Size<i32, Logical>,
    pub enhanced_size: Size<i32, Physical>,
    pub metrics: Metrics,
}

impl Pipeline {
    pub fn new(
        renderer: &mut GlesRenderer,
        source: Size<i32, Logical>,
        enhanced: Size<i32, Physical>,
        filter: Filter,
    ) -> Result<Self> {
        let scene = renderer.create_buffer(Fourcc::Abgr8888, (source.w, source.h).into())?;
        let texture = renderer.create_buffer(Fourcc::Abgr8888, (enhanced.w, enhanced.h).into())?;
        let gpu = Gpu::new(renderer, filter)?;
        Ok(Self {
            gpu,
            scene,
            enhanced: texture,
            source_size: source,
            enhanced_size: enhanced,
            metrics: Metrics {
                backend: "native".into(),
                filter: filter.name().into(),
                source: [source.w, source.h],
                enhanced: [enhanced.w, enhanced.h],
                ..Default::default()
            },
        })
    }

    pub fn resize(&mut self, renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> Result<()> {
        self.enhanced = renderer.create_buffer(Fourcc::Abgr8888, (size.w, size.h).into())?;
        self.enhanced_size = size;
        self.metrics.enhanced = [size.w, size.h];
        Ok(())
    }

    pub fn process(&mut self, renderer: &mut GlesRenderer) -> Result<()> {
        let (passes, neural_passes, upscale_steps, gpu_ms) = self.gpu.run(
            renderer,
            &self.scene,
            self.source_size,
            &self.enhanced,
            self.enhanced_size,
        )?;
        self.metrics.frames += 1;
        self.metrics.passes = passes;
        self.metrics.neural_passes = neural_passes;
        self.metrics.upscale_steps = upscale_steps;
        self.metrics.gpu_ms = (gpu_ms >= 0.0).then_some(gpu_ms);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "需要可用的 EGL GPU；验证超过两倍的实际超分链路"]
    fn neural_upscaling_covers_target_before_downsampling() -> Result<()> {
        use smithay::backend::egl::{EGLDevice, EGLDisplay};
        let device = EGLDevice::enumerate()?.next().context("没有 EGL 设备")?;
        let display = unsafe { EGLDisplay::new(device)? };
        let mut renderer = unsafe { GlesRenderer::new(EGLContext::new(&display)?)? };
        for (filter, passes) in [(Filter::Cunny, 42), (Filter::Anime4k, 34)] {
            let mut pipeline =
                Pipeline::new(&mut renderer, (13, 11).into(), (39, 33).into(), filter)?;
            pipeline.process(&mut renderer)?;
            assert_eq!(pipeline.metrics.enhanced, [39, 33]);
            assert_eq!(pipeline.metrics.upscale_steps, 2);
            assert_eq!(pipeline.metrics.neural_passes, passes, "{filter:?}");
        }
        Ok(())
    }

    #[test]
    #[ignore = "需要可用的 EGL GPU；显式运行以验证真实 shader"]
    fn cunny_matches_reference_convolution_and_pixel_shuffle() -> Result<()> {
        use smithay::backend::{
            egl::{EGLDevice, EGLDisplay},
            renderer::{Bind, ExportMem, Frame, Renderer},
        };
        use smithay::utils::Transform;

        let device = EGLDevice::enumerate()?.next().context("没有 EGL 设备")?;
        let display = unsafe { EGLDisplay::new(device)? };
        let mut renderer = unsafe { GlesRenderer::new(EGLContext::new(&display)?)? };
        let mut pipeline = Pipeline::new(
            &mut renderer,
            (13, 11).into(),
            (26, 22).into(),
            Filter::Cunny,
        )?;
        {
            let mut target = renderer.bind(&mut pipeline.scene)?;
            let mut frame = renderer.render(&mut target, (13, 11).into(), Transform::Normal)?;
            for y in 0..11 {
                for x in 0..13 {
                    let color = [
                        ((x * 17 + y * 9) % 256) as f32 / 255.0,
                        ((x * 5 + y * 23) % 256) as f32 / 255.0,
                        ((x * 29 + y * 3) % 256) as f32 / 255.0,
                        1.0,
                    ];
                    frame.clear(
                        color.into(),
                        &[Rectangle::new((x, y).into(), (1, 1).into())],
                    )?;
                }
            }
            let _done = frame.finish()?;
        }
        pipeline.process(&mut renderer)?;
        assert_eq!(pipeline.metrics.neural_passes, 21);
        let mapping = {
            let target = renderer.bind(&mut pipeline.enhanced)?;
            renderer.copy_framebuffer(
                &target,
                Rectangle::from_size((26, 22).into()),
                Fourcc::Abgr8888,
            )?
        };
        let pixels = renderer.map_texture(&mapping)?.to_vec();
        // Independent CPU evaluation of the original 4x16 DS weights, with
        // bilinear RGB + luma residual. Odd dimensions expose padding and flips.
        let expected = [
            (0, 0, [0, 0, 0]),
            (1, 0, [5, 2, 8]),
            (0, 1, [2, 6, 1]),
            (1, 1, [7, 8, 9]),
            (12, 10, [140, 138, 181]),
            (13, 10, [148, 139, 194]),
            (12, 11, [145, 150, 183]),
            (13, 11, [152, 151, 196]),
            (24, 20, [26, 22, 109]),
            (25, 21, [44, 40, 128]),
            (4, 17, [104, 199, 76]),
            (21, 3, [187, 82, 47]),
        ];
        for (x, y, rgb) in expected {
            let offset = (y * 26 + x) * 4;
            for c in 0..3 {
                assert!(
                    (i32::from(pixels[offset + c]) - rgb[c]).abs() <= 3,
                    "pixel ({x},{y}) channel {c}: GPU {}, reference {}",
                    pixels[offset + c],
                    rgb[c]
                );
            }
        }
        Ok(())
    }

    #[test]
    fn letterboxing_and_mouse_coordinates_share_one_transform() {
        let view = View::new(
            Rectangle::new((100, 50).into(), (800, 600).into()),
            (2560, 1440).into(),
        );
        assert_eq!(
            view.target,
            Rectangle::new((320, 0).into(), (1920, 1440).into())
        );
        assert_eq!(
            view.pointer((1280.0, 720.0).into()),
            Some((500.0, 350.0).into())
        );
        assert_eq!(
            view.pointer((320.0, 0.0).into()),
            Some((100.0, 50.0).into())
        );
        assert!(view.pointer((319.0, 720.0).into()).is_none());
        assert!(view.pointer((2240.0, 720.0).into()).is_none());
    }
}
