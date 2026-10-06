use std::{
    collections::BTreeSet,
    fs,
    os::unix::net::UnixStream,
    thread,
    time::{Duration, Instant},
};

use glow::HasContext;
use serde_json::json;

use crate::{
    catalog,
    content::Content,
    domain::{Fit, Playback, Transition},
    graphics::Gpu,
    pixels::Size,
};

fn usage() -> serde_json::Value {
    let status = fs::read_to_string("/proc/self/smaps_rollup").unwrap();
    let kib = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    let mut clients = BTreeSet::new();
    let mut vram_kib = 0u64;
    let mut gtt_kib = 0u64;
    for entry in fs::read_dir("/proc/self/fdinfo").unwrap().flatten() {
        let Ok(info) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let field = |key: &str| {
            info.lines()
                .find_map(|line| line.strip_prefix(key))
                .map(str::trim)
        };
        let Some(client) = field("drm-client-id:") else {
            continue;
        };
        if !clients.insert((
            field("drm-pdev:").unwrap_or("").to_owned(),
            client.to_owned(),
        )) {
            continue;
        }
        let amount = |key: &str| {
            field(key)
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0)
        };
        vram_kib += amount("drm-memory-vram:");
        gtt_kib += amount("drm-memory-gtt:");
    }
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the complete object on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    let cpu_seconds = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1_000_000.0;
    json!({
        "cpu_seconds": cpu_seconds,
        "rss_mib": kib("Rss:") as f64 / 1024.0,
        "pss_mib": kib("Pss:") as f64 / 1024.0,
        "vram_mib": vram_kib as f64 / 1024.0,
        "gtt_mib": gtt_kib as f64 / 1024.0,
    })
}

#[test]
#[ignore = "requires Wayland, EGL/GLES 3, libmpv and WALLPAPERD_BENCH_ASSET"]
fn profile_video_resources() {
    profile_resources(false);
}

#[test]
#[ignore = "requires Wayland, Rust scene assets and WALLPAPERD_BENCH_ASSET pointing to a project"]
fn profile_we_scene_resources() {
    profile_resources(true);
}

#[test]
#[ignore = "requires EGL/GLES 3; measures 4K transition composition"]
fn profile_transition_composition() {
    let gpu = Gpu::headless(Size::new(3840, 2160).unwrap()).unwrap();
    let old = gpu
        .image(
            image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 0, 0, 255])),
            Fit::Stretch,
        )
        .unwrap();
    let new = gpu
        .image(
            image::RgbaImage::from_pixel(64, 64, image::Rgba([0, 0, 255, 255])),
            Fit::Stretch,
        )
        .unwrap();
    let mut timings = serde_json::Map::new();
    for kind in [
        Transition::Fade,
        Transition::Disc,
        Transition::Honeycomb,
        Transition::Spiral,
        Transition::Stripes,
    ] {
        for _ in 0..3 {
            gpu.present(&new, Some(&old), kind, 0.5).unwrap();
        }
        // SAFETY: the pbuffer context is current; finish includes completed GPU work.
        unsafe {
            gpu.gl.finish();
        }
        let mut samples = Vec::new();
        for frame in 0..30 {
            let start = Instant::now();
            gpu.present(&new, Some(&old), kind, (frame as f32 + 0.5) / 30.0)
                .unwrap();
            unsafe {
                gpu.gl.finish();
            }
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        assert_eq!(unsafe { gpu.gl.get_error() }, glow::NO_ERROR);
        samples.sort_by(f64::total_cmp);
        timings.insert(format!("{kind:?}"), json!({"median_ms": samples[15], "mean_ms": samples.iter().sum::<f64>() / samples.len() as f64}));
    }
    println!(
        "{}",
        json!({"renderer": unsafe { gpu.gl.get_parameter_string(glow::RENDERER) }, "resolution": [3840, 2160], "frames_per_effect": 30, "effects": timings})
    );
}

fn drive(
    content: &mut Content,
    gpu: &Gpu,
    direct: bool,
    duration: Duration,
    fps: Option<u32>,
) -> (usize, f64, Vec<f64>) {
    let start = Instant::now();
    let period = fps.map(|fps| Duration::from_secs_f64(1. / fps as f64));
    let mut next = start;
    let mut frames = 0;
    let mut timings = Vec::new();
    while start.elapsed() < duration {
        if let Some(period) = period {
            thread::sleep(next.saturating_duration_since(Instant::now()));
            next += period;
            if start.elapsed() >= duration {
                break;
            }
        }
        content.poll().unwrap();
        let rendering = Instant::now();
        let changed = if direct {
            content.render_direct(gpu, false)
        } else {
            content.render(gpu)
        }
        .unwrap();
        if changed {
            if !direct {
                gpu.present(content.target(), None, crate::domain::Transition::Cut, 1.0)
                    .unwrap();
            }
            // The desktop swap flushes GL; the benchmark pbuffer requires an explicit flush.
            unsafe { gpu.gl.flush() };
            content.report_swap();
            frames += 1;
            timings.push(rendering.elapsed().as_secs_f64() * 1000.);
        }
        if period.is_none() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    (frames, start.elapsed().as_secs_f64(), timings)
}
fn profile_resources(scene: bool) {
    let path = std::env::var("WALLPAPERD_BENCH_ASSET").expect("set WALLPAPERD_BENCH_ASSET");
    let selection = catalog::resolve(&path, Fit::Cover).unwrap();
    if scene {
        assert_eq!(
            catalog::kind(catalog::path(&selection)),
            Some(catalog::Kind::WeScene)
        );
    }
    // Supplying the existing display permits hardware device discovery; no desktop surface is mapped.
    let connection = wayland_client::Connection::connect_to_env()
        .expect("Wayland session required for hardware device discovery");
    let mut gpu = Gpu::headless(Size::new(3840, 2160).unwrap()).unwrap();
    gpu.native_display = connection.backend().display_ptr().cast();
    let (wake, _reader) = UnixStream::pair().unwrap();
    wake.set_nonblocking(true).unwrap();
    let fps = std::env::var("WALLPAPERD_BENCH_FPS").ok().map(|value| {
        let fps = value.parse::<u32>().expect("WALLPAPERD_BENCH_FPS integer");
        assert!((1..=240).contains(&fps), "benchmark FPS must be in 1..=240");
        fps
    });
    let playback = Playback {
        fps: fps.unwrap_or(30),
        ..Playback::default()
    };
    let mut content = Content::load(&gpu, &selection, playback, &wake).unwrap();
    let direct = std::env::var("WALLPAPERD_BENCH_DIRECT").as_deref() != Ok("false");
    let loading = Instant::now();
    while !content.ready() {
        drive(&mut content, &gpu, direct, Duration::from_millis(100), fps);
        assert!(
            loading.elapsed() < Duration::from_secs(60),
            "content did not become ready within the loading budget"
        );
    }
    drive(
        &mut content,
        &gpu,
        direct,
        Duration::from_secs(if scene { 3 } else { 1 }),
        fps,
    );
    let before = usage();
    let (frames, seconds, mut timings) =
        drive(&mut content, &gpu, direct, Duration::from_secs(6), fps);
    timings.sort_by(f64::total_cmp);
    let after = usage();
    assert!(frames > 30, "content did not keep presenting frames");
    println!(
        "{}",
        json!({
            "asset": selection.asset_id, "direct": direct, "backend":content.backend(), "fps_limit":fps, "frames": frames, "seconds": seconds,
            "fps": frames as f64 / seconds,
            "render_ms_mean":timings.iter().sum::<f64>()/timings.len() as f64,
            "render_ms_p95":timings[(timings.len()*95/100).min(timings.len()-1)],
            "cpu_percent": 100.0 * (after["cpu_seconds"].as_f64().unwrap() - before["cpu_seconds"].as_f64().unwrap()) / seconds,
            "before": before, "after": after,
        })
    );
    if scene {
        content
            .playback(Playback {
                paused: true,
                ..playback
            })
            .unwrap();
        drive(&mut content, &gpu, direct, Duration::from_millis(200), fps);
        let before = usage();
        let (frames, seconds, _) = drive(&mut content, &gpu, direct, Duration::from_secs(3), fps);
        let after = usage();
        assert_eq!(frames, 0, "paused scene produced frames");
        println!(
            "{}",
            json!({"paused":true, "frames":frames, "seconds":seconds,
            "cpu_percent": 100.0 * (after["cpu_seconds"].as_f64().unwrap() - before["cpu_seconds"].as_f64().unwrap()) / seconds,
            "before":before, "after":after})
        );
    }
}
