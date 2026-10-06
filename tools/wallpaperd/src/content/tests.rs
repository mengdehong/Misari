use super::*;
use crate::{domain::Fit, pixels::Size};
use std::{process::Command, thread, time::Instant};

fn selection(path: &std::path::Path) -> Selection {
    Selection {
        asset_id: format!("local:{}", path.display()),
        fit: Fit::Stretch,
    }
}

fn pixel(gpu: &Gpu, target: &Target, x: i32, y: i32) -> [u8; 4] {
    target.bind();
    let pixel = surface_pixel(gpu, x, y);
    gpu.reset();
    pixel
}

fn surface_pixel(gpu: &Gpu, x: i32, y: i32) -> [u8; 4] {
    let mut pixel = [0u8; 4];
    // SAFETY: four bytes hold one RGBA pixel of the live framebuffer.
    unsafe {
        gpu.gl.read_pixels(
            x,
            y,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixel)),
        );
    }
    pixel
}

#[test]
fn shader_clock_stops_and_resumes_without_including_pause_time() {
    let mut clock = Clock::new(false);
    thread::sleep(Duration::from_millis(5));
    clock.pause(true);
    let stopped = clock.elapsed();
    thread::sleep(Duration::from_millis(5));
    assert_eq!(clock.elapsed(), stopped);
    clock.pause(false);
    thread::sleep(Duration::from_millis(5));
    assert!(clock.elapsed() > stopped);
}

#[test]
#[ignore = "requires EGL/GLES 3, libmpv and ffmpeg"]
fn shared_playback_shader_and_video_late_join_pause_resume_and_loop() {
    let gpu = Gpu::headless(Size::new(32, 32).unwrap()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let shader = temp.path().join("timeline.frag");
    std::fs::write(
        &shader,
        "void mainImage(out vec4 c,in vec2 p){c=vec4(fract(iTime/4.),0.,0.,1.);}",
    )
    .unwrap();
    let movie = temp.path().join("timeline.mkv");
    assert!(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "nullsrc=size=32x32:rate=30,geq=r='255*T/4':g=0:b=0",
                "-t",
                "4",
                "-c:v",
                "ffv1",
                "-y"
            ])
            .arg(&movie)
            .status()
            .unwrap()
            .success()
    );
    let playback = Playback::default();
    let paused = Playback {
        paused: true,
        ..playback
    };
    let (wake, _reader) = UnixStream::pair().unwrap();
    wake.set_nonblocking(true).unwrap();
    let drive_pair = |first: &mut Content, second: &mut Content, duration: Duration| {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            for content in [&mut *first, &mut *second] {
                content.poll().unwrap();
                if content.render(&gpu).unwrap() {
                    content.report_swap();
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
    };
    let red = |content: &Content| pixel(&gpu, content.target(), 16, 16)[0];
    for path in [&shader, &movie] {
        let selection = selection(path);
        let mut clock = wallpaper_media::clock::Timeline::new(wallpaper_media::clock::now(), true);
        let mut first = Content::load_synchronized(
            &gpu,
            &selection,
            playback,
            &wake,
            &Default::default(),
            Some(clock),
        )
        .unwrap();
        drive(&mut first, &gpu, Duration::from_millis(1100)).unwrap();
        let before_join = red(&first);
        assert!(
            before_join > 20,
            "{}: first renderer did not advance: {before_join}",
            path.display()
        );
        let mut second = Content::load_synchronized(
            &gpu,
            &selection,
            playback,
            &wake,
            &Default::default(),
            Some(clock),
        )
        .unwrap();
        drive_pair(&mut first, &mut second, Duration::from_millis(900));
        let ready_deadline = Instant::now() + Duration::from_secs(2);
        while !second.ready() {
            assert!(
                Instant::now() < ready_deadline,
                "late join did not align before its startup deadline"
            );
            drive_pair(&mut first, &mut second, Duration::from_millis(5));
        }
        let assert_synced = |stage: &str,
                             clock: wallpaper_media::clock::Timeline,
                             first: &Content,
                             second: &Content| {
            let state = |content: &Content| match &content.engine {
                Engine::Video(video) => format!(
                    "position {:?}, seek {:?}, pending {}",
                    video.position(),
                    video.seek_target(),
                    video.pending_seek()
                ),
                _ => "shader".into(),
            };
            let (a, b) = (red(first), red(second));
            assert!(
                a.abs_diff(b) <= 12,
                "{} {stage} playback diverged: {a} vs {b}; {} / {}; clock {:.3}",
                path.display(),
                state(first),
                state(second),
                clock.position(wallpaper_media::clock::now()) as f64 / 1e9
            );
        };
        assert_synced("joined", clock, &first, &second);
        first.playback(paused).unwrap();
        let frozen = red(&first);
        drive_pair(&mut first, &mut second, Duration::from_millis(550));
        assert_eq!(red(&first), frozen, "a paused renderer must keep its frame");
        first.playback(playback).unwrap();
        drive_pair(&mut first, &mut second, Duration::from_millis(600));
        assert_synced("resumed", clock, &first, &second);
        clock.set_running(wallpaper_media::clock::now(), false);
        for content in [&mut first, &mut second] {
            content.playback(paused).unwrap();
            content.synchronize(clock).unwrap();
        }
        let frozen = (red(&first), red(&second));
        drive_pair(&mut first, &mut second, Duration::from_millis(550));
        assert_eq!((red(&first), red(&second)), frozen);
        clock.set_running(wallpaper_media::clock::now(), true);
        for content in [&mut first, &mut second] {
            content.synchronize(clock).unwrap();
            content.playback(playback).unwrap();
        }
        drive_pair(&mut first, &mut second, Duration::from_millis(1500));
        assert_synced("loop", clock, &first, &second);
        assert!(red(&first) < 120, "loop did not wrap the shared timeline");
    }
}

#[test]
#[ignore = "requires EGL/GLES 3; run with --ignored on a graphics-capable host"]
fn gpu_geometric_transitions_endpoints_geometry_and_opacity() {
    use crate::domain::Transition;
    for (width, height) in [(96, 64), (64, 96), (192, 64)] {
        let gpu = Gpu::headless(Size::new(width, height).unwrap()).unwrap();
        let selection = selection(std::path::Path::new("image.png"));
        let old = Content::image(
            &gpu,
            image::RgbaImage::from_pixel(width, height, image::Rgba([255, 0, 0, 255])),
            &selection,
        )
        .unwrap();
        let new = Content::image(
            &gpu,
            image::RgbaImage::from_pixel(width, height, image::Rgba([0, 0, 255, 255])),
            &selection,
        )
        .unwrap();
        for kind in [
            Transition::Disc,
            Transition::Honeycomb,
            Transition::Spiral,
            Transition::Stripes,
        ] {
            let mut previous_coverage = 0;
            for progress in [0.0, 0.15, 0.35, 0.5, 0.85, 1.0] {
                gpu.present(new.target(), Some(old.target()), kind, progress)
                    .unwrap();
                let mut frame = vec![0u8; (width * height * 4) as usize];
                // SAFETY: frame holds every RGBA pixel of the current pbuffer.
                unsafe {
                    gpu.gl.read_pixels(
                        0,
                        0,
                        width as i32,
                        height as i32,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(&mut frame)),
                    );
                }
                let pixels = frame.as_chunks::<4>().0;
                let mut coverage = 0;
                for pixel in pixels {
                    assert_eq!(pixel[1], 0, "{kind:?}: unexpected color");
                    assert_eq!(pixel[3], 255, "{kind:?}: transparent frame");
                    assert!((254..=256).contains(&(u32::from(pixel[0]) + u32::from(pixel[2]))));
                    coverage += u64::from(pixel[2]);
                    if progress == 0.0 {
                        assert_eq!(*pixel, [255, 0, 0, 255]);
                    }
                    if progress == 1.0 {
                        assert_eq!(*pixel, [0, 0, 255, 255]);
                    }
                }
                assert!(
                    coverage >= previous_coverage,
                    "{kind:?}: reveal moved backwards"
                );
                previous_coverage = coverage;
                if progress == 0.5 {
                    assert!(
                        pixels.iter().any(|pixel| pixel[0] > 240),
                        "{kind:?}: lost old frame"
                    );
                    assert!(
                        pixels.iter().any(|pixel| pixel[2] > 240),
                        "{kind:?}: no reveal"
                    );
                    if kind == Transition::Disc {
                        let center = surface_pixel(&gpu, width as i32 / 2, height as i32 / 2);
                        assert_eq!(center, [0, 0, 255, 255]);
                    }
                }
                if kind == Transition::Disc && progress == 0.35 {
                    // Compare across the circle's edge, including portrait/ultrawide.
                    for radius in 0..(width.min(height) / 2) as i32 {
                        assert_eq!(
                            surface_pixel(&gpu, width as i32 / 2 + radius, height as i32 / 2),
                            surface_pixel(&gpu, width as i32 / 2, height as i32 / 2 + radius)
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "requires EGL/GLES 3; run with --ignored on a graphics-capable host"]
fn gpu_image_shader_pause_resize_and_blend() {
    let mut gpu = Gpu::headless(Size::new(8, 8).unwrap()).unwrap();
    let source = image::RgbaImage::from_fn(8, 8, |_, y| {
        if y < 4 {
            image::Rgba([255, 0, 0, 255])
        } else {
            image::Rgba([0, 0, 255, 255])
        }
    });
    let image =
        Content::image(&gpu, source, &selection(std::path::Path::new("image.png"))).unwrap();
    assert_eq!(pixel(&gpu, image.target(), 4, 6), [255, 0, 0, 255]);
    assert_eq!(pixel(&gpu, image.target(), 4, 1), [0, 0, 255, 255]);
    gpu.present(image.target(), None, crate::domain::Transition::Cut, 1.0)
        .unwrap();
    assert_eq!(surface_pixel(&gpu, 4, 6), [255, 0, 0, 255]);
    assert_eq!(surface_pixel(&gpu, 4, 1), [0, 0, 255, 255]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("green.frag");
    std::fs::write(
        &path,
        "void mainImage(out vec4 color, in vec2 p) { color = vec4(0.,1.,0.,1.); }",
    )
    .unwrap();
    let (wake, _) = UnixStream::pair().unwrap();
    let mut shader = Content::load(
        &gpu,
        &selection(&path),
        Playback {
            paused: true,
            ..Playback::default()
        },
        &wake,
    )
    .unwrap();
    assert!(shader.render(&gpu).unwrap());
    assert!(!shader.render(&gpu).unwrap());
    assert_eq!(pixel(&gpu, shader.target(), 4, 4), [0, 255, 0, 255]);
    gpu.present(
        shader.target(),
        Some(image.target()),
        crate::domain::Transition::Fade,
        0.5,
    )
    .unwrap();
    let mut blended = [0u8; 4];
    unsafe {
        gpu.gl.read_pixels(
            4,
            6,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut blended)),
        );
    }
    assert!((126..=129).contains(&blended[0]) && (126..=129).contains(&blended[1]));
    gpu.resize(Size::new(12, 6).unwrap());
    shader.resized();
    assert!(shader.render(&gpu).unwrap());
    assert_eq!(shader.target().size, gpu.size);
    shader.playback(Playback::default()).unwrap();
    assert!(shader.render_direct(&gpu, false).unwrap());
    assert!(shader.target.is_none());
    assert_eq!(surface_pixel(&gpu, 4, 4), [0, 255, 0, 255]);
    shader
        .playback(Playback {
            paused: true,
            ..Playback::default()
        })
        .unwrap();
    assert!(!shader.needs_render());
    assert!(!shader.render_direct(&gpu, false).unwrap());
    shader.snapshot(&gpu).unwrap();
    std::fs::write(
        &path,
        "void mainImage(out vec4 color, in vec2 p) { this is broken; }",
    )
    .unwrap();
    assert!(Content::load(&gpu, &selection(&path), Playback::default(), &wake).is_err());
    assert_eq!(pixel(&gpu, shader.target(), 4, 4), [0, 255, 0, 255]);
}

fn drive(content: &mut Content, gpu: &Gpu, timeout: Duration) -> Result<usize> {
    drive_frames(content, gpu, timeout, false)
}

fn drive_frames(
    content: &mut Content,
    gpu: &Gpu,
    timeout: Duration,
    direct: bool,
) -> Result<usize> {
    let start = Instant::now();
    let mut frames = 0;
    while start.elapsed() < timeout {
        content.poll()?;
        if if direct {
            content.render_direct(gpu, false)?
        } else {
            content.render(gpu)?
        } {
            frames += 1;
            content.report_swap();
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(frames)
}

#[test]
#[ignore = "requires EGL/GLES 3, libmpv and ffmpeg"]
fn video_fps_limit_does_not_duplicate_native_frames() {
    let gpu = Gpu::headless(Size::new(32, 32).unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native-rate.mkv");
    assert!(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=32x32:rate=12",
                "-t",
                "2",
                "-c:v",
                "ffv1",
                "-y",
            ])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let (wake, _reader) = UnixStream::pair().unwrap();
    wake.set_nonblocking(true).unwrap();
    let mut video = Content::load(&gpu, &selection(&path), Playback::default(), &wake).unwrap();
    drive(&mut video, &gpu, Duration::from_millis(400)).unwrap();
    assert!(video.ready());
    let frames = drive(&mut video, &gpu, Duration::from_millis(1200)).unwrap();
    assert!(frames >= 8, "native video stopped presenting: {frames}");
    assert!(
        frames <= 18,
        "12 FPS video was upsampled to the 30 FPS limit: {frames}"
    );
}

#[test]
#[ignore = "requires EGL/GLES 3 and libmpv"]
fn direct_transparent_gif_and_contain_borders_are_opaque_black() {
    let gpu = Gpu::headless(Size::new(32, 32).unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transparent.gif");
    let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&path).unwrap());
    encoder
        .set_repeat(image::codecs::gif::Repeat::Infinite)
        .unwrap();
    for _ in 0..2 {
        let frame = image::RgbaImage::from_fn(32, 16, |x, _| {
            image::Rgba(if x < 16 {
                [0, 0, 0, 0]
            } else {
                [255, 0, 0, 255]
            })
        });
        encoder
            .encode_frame(image::Frame::from_parts(
                frame,
                0,
                0,
                image::Delay::from_numer_denom_ms(100, 1),
            ))
            .unwrap();
    }
    drop(encoder);
    let (wake, _reader) = UnixStream::pair().unwrap();
    wake.set_nonblocking(true).unwrap();
    let mut selection = selection(&path);
    selection.fit = Fit::Contain;
    let mut gif = Content::load(&gpu, &selection, Playback::default(), &wake).unwrap();
    assert!(drive_frames(&mut gif, &gpu, Duration::from_millis(400), true).unwrap() > 0);
    gpu.reset();
    assert_eq!(surface_pixel(&gpu, 8, 0), [0, 0, 0, 255]);
    assert_eq!(surface_pixel(&gpu, 8, 16), [0, 0, 0, 255]);
    let red = surface_pixel(&gpu, 24, 16);
    assert!(
        red[0] > 240 && red[1] < 10 && red[2] < 10 && red[3] == 255,
        "{red:?}"
    );
}

#[test]
#[ignore = "requires EGL/GLES 3, libmpv and ffmpeg"]
fn video_first_frame_loop_pause_resume_and_failed_candidate() {
    let gpu = Gpu::headless(Size::new(32, 32).unwrap()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("video.mkv");
    assert!(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=red:size=32x32:rate=15,drawbox=x=0:y=16:w=32:h=16:color=blue:t=fill",
                "-t",
                "0.8",
                "-c:v",
                "ffv1",
                "-y"
            ])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let (wake, _reader) = UnixStream::pair().unwrap();
    wake.set_nonblocking(true).unwrap();
    let mut video = Content::load(
        &gpu,
        &selection(&path),
        Playback {
            paused: true,
            ..Playback::default()
        },
        &wake,
    )
    .unwrap();
    assert!(!video.ready());
    assert!(drive(&mut video, &gpu, Duration::from_millis(800)).unwrap() >= 1);
    assert!(video.ready());
    assert_eq!(
        drive(&mut video, &gpu, Duration::from_millis(200)).unwrap(),
        0
    );
    let top = pixel(&gpu, video.target(), 16, 24);
    let bottom = pixel(&gpu, video.target(), 16, 4);
    assert!(top[0] > 200 && top[2] < 40, "top: {top:?}");
    assert!(bottom[2] > 200 && bottom[0] < 40, "bottom: {bottom:?}");
    video.playback(Playback::default()).unwrap();
    assert!(drive_frames(&mut video, &gpu, Duration::from_millis(1600), true).unwrap() >= 10);
    assert!(
        video.target.is_none(),
        "steady playback must not retain an intermediate texture"
    );
    gpu.reset();
    assert_eq!(surface_pixel(&gpu, 16, 24), top);
    assert_eq!(surface_pixel(&gpu, 16, 4), bottom);
    video
        .playback(Playback {
            paused: true,
            ..Playback::default()
        })
        .unwrap();
    drive_frames(&mut video, &gpu, Duration::from_millis(100), true).unwrap();
    assert!(!video.needs_render());
    assert_eq!(
        drive_frames(&mut video, &gpu, Duration::from_millis(200), true).unwrap(),
        0
    );
    video.snapshot(&gpu).unwrap();
    let broken = dir.path().join("broken.mp4");
    std::fs::write(&broken, b"not a video").unwrap();
    let mut candidate =
        Content::load(&gpu, &selection(&broken), Playback::default(), &wake).unwrap();
    assert!(drive(&mut candidate, &gpu, Duration::from_secs(2)).is_err());
    assert_eq!(pixel(&gpu, video.target(), 16, 24), top);
}
