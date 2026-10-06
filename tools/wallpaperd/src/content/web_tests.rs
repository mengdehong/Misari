use super::*;
use crate::{domain::Fit, pixels::Size};
use std::{fs, thread, time::Instant};

fn web_browser_properties_pointer_resize_pause_execution_and_release() {
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("project.json"), r#"{"type":"web","file":"index.html","general":{"properties":{"tint":{"type":"color","value":"1 0 0"}}}}"#).unwrap();
    fs::write(project.path().join("index.html"), r#"<!doctype html><canvas id="c"></canvas><script>
    document.body.style.margin = '0';
    const ctx = c.getContext('2d');
    let tint = 'red', clicked = false, counter = 0, left = 0, right = 0;
    let enabled = false, metadata = false, playing = false, timeline = false;
    wallpaperRegisterAudioListener(samples => {
        if(samples.length === 128) { left = samples[0]; right = samples[64]; }
    });
    wallpaperRegisterMediaStatusListener(value => enabled = value.enabled);
    wallpaperRegisterMediaPropertiesListener(value => metadata = value.title === 'test song' && value.albumTitle === 'test album');
    wallpaperRegisterMediaPlaybackListener(value => playing = value.state === wallpaperMediaIntegration.PLAYBACK_PLAYING);
    wallpaperRegisterMediaTimelineListener(value => timeline = value.position === 12 && value.duration === 60);
    setInterval(() => ++counter, 50);
    c.addEventListener('mousedown', () => { clicked = true; });
    window.wallpaperPropertyListener = {applyUserProperties(p) {
        if(p.tint) tint = `rgb(${p.tint.value.split(' ').map(v=>Math.round(v*255)).join(',')})`;
    }};
    function draw() {
        c.width = innerWidth; c.height = innerHeight;
        ctx.fillStyle = tint; ctx.fillRect(0, 0, c.width, c.height / 2);
        ctx.fillStyle = clicked ? 'lime' : 'blue'; ctx.fillRect(c.width/2, 0, c.width/2, c.height/2);
        ctx.fillStyle = `rgb(${counter%200},0,0)`; ctx.fillRect(0,c.height/2,c.width,c.height/2);
        ctx.fillStyle = `rgb(${Math.round(left*255)},${enabled && metadata && playing && timeline ? 255 : 0},${Math.round(right*255)})`;
        ctx.fillRect(c.width/2,c.height/2,c.width/2,c.height/2);
        requestAnimationFrame(draw);
    }
    draw();
    </script>"#).unwrap();
    let mut gpu = Gpu::headless(Size::new(64, 64).unwrap()).unwrap();
    let selection = catalog::resolve(project.path().to_str().unwrap(), Fit::Cover).unwrap();
    let (wake, _reader) = UnixStream::pair().unwrap();
    let playback = Playback::default();
    let mut content = Content::load_with_properties(
        &gpu,
        &selection,
        playback,
        &wake,
        &crate::properties::Values::from([("tint".into(), json!([0, 1, 0]))]),
    )
    .unwrap();
    content.set_media(&crate::media::Snapshot {
        enabled: true,
        title: "test song".into(),
        album_title: "test album".into(),
        playback: crate::media::PlaybackState::Playing,
        position: 12.0,
        duration: 60.0,
        ..Default::default()
    });
    let mut audio = we_scene::audio::AudioSnapshot::default();
    audio.bands[2].left[0] = 0.125;
    audio.bands[2].right[0] = 0.25;
    let drive = |content: &mut Content, gpu: &Gpu, duration: Duration| {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            content.poll().unwrap();
            content.set_audio(&audio);
            content.render(gpu).unwrap();
            thread::sleep(Duration::from_millis(5));
        }
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    while !content.ready() {
        assert!(Instant::now() < deadline, "web first frame timed out");
        drive(&mut content, &gpu, Duration::from_millis(20));
    }
    let frame = content.export_frame(&gpu).unwrap();
    if std::env::var("WALLPAPERD_WEB_GPU").as_deref() == Ok("1") {
        let Engine::Web(web) = &content.engine else {
            panic!("expected Web engine")
        };
        assert_eq!(web.transport(), "dmabuf", "{}", web.diagnostics());
    }
    assert_eq!(
        frame.get_pixel(16, 16).0,
        [0, 255, 0, 255],
        "author property must reach the first frame"
    );
    assert_eq!(
        frame.get_pixel(48, 16).0,
        [0, 0, 255, 255],
        "BGRA and upper-left origin must be mapped correctly"
    );
    assert!(matches!(content.backend(), crate::domain::Backend::Cef));
    assert!(
        content.requires_audio(),
        "JS subscription must activate shared audio capture"
    );
    drive(&mut content, &gpu, Duration::from_millis(100));
    assert_eq!(
        content.export_frame(&gpu).unwrap().get_pixel(48, 48).0,
        [32, 255, 64, 255],
        "stereo audio bands and media callbacks must reach the page"
    );
    content.pointer(Some([0.75, 0.25]), Some(true)).unwrap();
    content.pointer(Some([0.75, 0.25]), Some(false)).unwrap();
    drive(&mut content, &gpu, Duration::from_millis(100));
    assert_eq!(
        content.export_frame(&gpu).unwrap().get_pixel(48, 16).0,
        [0, 255, 0, 255]
    );
    drive(&mut content, &gpu, Duration::from_millis(100));
    content
        .playback(Playback {
            paused: true,
            ..playback
        })
        .unwrap();
    drive(&mut content, &gpu, Duration::from_millis(100));
    let before = content.export_frame(&gpu).unwrap();
    drive(&mut content, &gpu, Duration::from_millis(600));
    assert_eq!(
        before,
        content.export_frame(&gpu).unwrap(),
        "pause must preserve pixels"
    );
    content.playback(playback).unwrap();
    drive(&mut content, &gpu, Duration::from_millis(100));
    let after = content.export_frame(&gpu).unwrap();
    let delta = after.get_pixel(16, 48)[0].wrapping_sub(before.get_pixel(16, 48)[0]);
    assert!(
        (1..=5).contains(&delta),
        "a paused JS timer kept running: {delta}"
    );
    content
        .set_properties(&crate::properties::Values::from([(
            "tint".into(),
            json!([1, 0, 0]),
        )]))
        .unwrap();
    drive(&mut content, &gpu, Duration::from_millis(100));
    assert_eq!(
        content.export_frame(&gpu).unwrap().get_pixel(16, 16).0,
        [255, 0, 0, 255]
    );
    gpu.resize(Size::new(96, 80).unwrap());
    content.resized();
    drive(&mut content, &gpu, Duration::from_millis(150));
    let frame = content.export_frame(&gpu).unwrap();
    assert_eq!(frame.dimensions(), (96, 80));
    assert_eq!(frame.get_pixel(24, 20).0, [255, 0, 0, 255]);
    gpu.resize(Size::new(32, 32).unwrap());
    content.resized();
    drive(&mut content, &gpu, Duration::from_millis(150));
    let frame = content.export_frame(&gpu).unwrap();
    assert_eq!(frame.dimensions(), (32, 32));
    assert_eq!(frame.get_pixel(8, 8).0, [255, 0, 0, 255]);
    drop(content);
}

fn web_browser_partial_upload_and_skipped_damage_keep_unchanged_pixels() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("project.json"),
        r#"{"type":"web","file":"index.html"}"#,
    )
    .unwrap();
    fs::write(project.path().join("index.html"), r#"<!doctype html><body style="margin:0;background:blue">
    <div id="a" style="position:absolute;left:7px;top:11px;width:9px;height:13px;background:red"></div>
    <div id="b" style="position:absolute;left:45px;top:39px;width:8px;height:9px;background:red"></div>
    <script>window.wallpaperPropertyListener={applyUserProperties(p){if(p.go?.value){
    setTimeout(()=>a.style.background='lime',200);
    setTimeout(()=>b.style.background='yellow',400);
    setTimeout(()=>a.style.background='magenta',800);
    }}};</script>"#).unwrap();
    let gpu = Gpu::headless(Size::new(64, 64).unwrap()).unwrap();
    let selection = catalog::resolve(project.path().to_str().unwrap(), Fit::Cover).unwrap();
    let (wake, _reader) = UnixStream::pair().unwrap();
    let mut content = Content::load(
        &gpu,
        &selection,
        Playback {
            fps: 60,
            ..Playback::default()
        },
        &wake,
    )
    .unwrap();
    let drive = |content: &mut Content, duration: Duration| {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            content.poll().unwrap();
            content.render(&gpu).unwrap();
            thread::sleep(Duration::from_millis(5));
        }
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    while !content.ready() {
        assert!(Instant::now() < deadline, "web first frame timed out");
        drive(&mut content, Duration::from_millis(20));
    }
    content
        .set_properties(&crate::properties::Values::from([(
            "go".into(),
            json!(true),
        )]))
        .unwrap();
    drive(&mut content, Duration::from_millis(150));
    // Consume neither of the two independent CSS paints. Uploading only the
    // last damage rectangle would leave the first square red.
    thread::sleep(Duration::from_millis(400));
    content.poll().unwrap();
    content.render(&gpu).unwrap();
    if std::env::var("WALLPAPERD_WEB_GPU").as_deref() == Ok("1") {
        // GPU transport holds one borrowed CEF frame during a stalled consumer.
        // Consume it before CEF can publish the renderer's newer state.
        drive(&mut content, Duration::from_millis(100));
    }
    let frame = content.export_frame(&gpu).unwrap();
    assert_eq!(frame.get_pixel(11, 17).0, [0, 255, 0, 255]);
    assert_eq!(frame.get_pixel(49, 43).0, [255, 255, 0, 255]);
    assert_eq!(frame.get_pixel(32, 32).0, [0, 0, 255, 255]);
    // The next paint is a single small rectangle with a nonzero x/y origin.
    drive(&mut content, Duration::from_millis(450));
    let frame = content.export_frame(&gpu).unwrap();
    assert_eq!(frame.get_pixel(11, 17).0, [255, 0, 255, 255]);
    assert_eq!(frame.get_pixel(49, 43).0, [255, 255, 0, 255]);
    assert_eq!(frame.get_pixel(32, 32).0, [0, 0, 255, 255]);
}

fn web_browser_video_and_webgl_share_correct_pixels() {
    let project = tempfile::tempdir().unwrap();
    let video = project.path().join("loop.webm");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=960x640:r=30",
            "-t",
            "0.4",
            "-an",
            "-c:v",
            "libvpx-vp9",
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
        ])
        .arg(&video)
        .status();
    let status = match status {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return;
        }
        status => status.unwrap(),
    };
    assert!(status.success());
    fs::write(
        project.path().join("project.json"),
        r#"{"type":"web","file":"index.html"}"#,
    )
    .unwrap();
    fs::write(project.path().join("index.html"), r#"<!doctype html><body style="margin:0;overflow:hidden">
    <video autoplay muted loop src="loop.webm" style="position:absolute;left:0;top:0;width:32px;height:64px;object-fit:fill"></video>
    <canvas id="c" width="32" height="64" style="position:absolute;left:32px;top:0"></canvas>
    <script>const gl=c.getContext('webgl');function paint(){gl.clearColor(0,0,1,1);gl.clear(gl.COLOR_BUFFER_BIT);requestAnimationFrame(paint)}paint();</script>"#).unwrap();
    let gpu = Gpu::headless(Size::new(64, 64).unwrap()).unwrap();
    let selection = catalog::resolve(project.path().to_str().unwrap(), Fit::Cover).unwrap();
    let (wake, _reader) = UnixStream::pair().unwrap();
    let mut content = Content::load(
        &gpu,
        &selection,
        Playback {
            fps: 60,
            ..Playback::default()
        },
        &wake,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        content.poll().unwrap();
        content.render(&gpu).unwrap();
        if content.ready() {
            let frame = content.export_frame(&gpu).unwrap();
            let red = frame.get_pixel(16, 32).0;
            let blue = frame.get_pixel(48, 32).0;
            if red[0] > 240 && red[1] < 40 && red[2] < 20 && blue == [0, 0, 255, 255] {
                break;
            }
        }
        assert!(Instant::now() < deadline, "{}", content.diagnostics());
        thread::sleep(Duration::from_millis(5));
    }
    if std::env::var("WALLPAPERD_WEB_GPU").as_deref() == Ok("1") {
        let Engine::Web(web) = &content.engine else {
            panic!("expected Web engine")
        };
        assert_eq!(web.transport(), "dmabuf", "{}", web.diagnostics());
    }
}

fn web_browser_gpu_pause_and_properties_work_without_presentation() {
    if std::env::var("WALLPAPERD_WEB_GPU").as_deref() != Ok("1") {
        return;
    }
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("project.json"),
        r#"{"type":"web","file":"index.html"}"#,
    )
    .unwrap();
    fs::write(project.path().join("index.html"),r#"<!doctype html><body style="margin:0;overflow:hidden"><canvas id="c" width="64" height="64"></canvas>
    <script>const ctx=c.getContext('2d');let tint='red',counter=0;setInterval(()=>counter++,50);
    wallpaperPropertyListener={applyUserProperties(p){if(p.tint)tint=p.tint.value}};
    function paint(){ctx.fillStyle=tint;ctx.fillRect(0,0,64,32);ctx.fillStyle=`rgb(${counter%200},0,0)`;ctx.fillRect(0,32,64,32);requestAnimationFrame(paint)}paint();</script>"#).unwrap();
    let gpu = Gpu::headless(Size::new(64, 64).unwrap()).unwrap();
    let selection = catalog::resolve(project.path().to_str().unwrap(), Fit::Cover).unwrap();
    let (wake, _reader) = UnixStream::pair().unwrap();
    let playback = Playback {
        fps: 60,
        ..Playback::default()
    };
    let mut content = Content::load(&gpu, &selection, playback, &wake).unwrap();
    let drive = |content: &mut Content, duration: Duration| {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            content.poll().unwrap();
            content.prepare_frame(Some(&gpu)).unwrap();
            thread::sleep(Duration::from_millis(5));
        }
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        drive(&mut content, Duration::from_millis(20));
        let Engine::Web(web) = &content.engine else {
            panic!("expected Web engine")
        };
        if web.ready() {
            break;
        }
        assert!(Instant::now() < deadline, "GPU first frame timed out");
    }
    content.render(&gpu).unwrap();
    let Engine::Web(web) = &content.engine else {
        panic!("expected Web engine")
    };
    assert_eq!(web.transport(), "dmabuf");
    content
        .playback(Playback {
            paused: true,
            ..playback
        })
        .unwrap();
    content
        .set_properties(&crate::properties::Values::from([(
            "tint".into(),
            json!("lime"),
        )]))
        .unwrap();
    // No presentation calls: reproduce a compositor withholding frame callbacks.
    drive(&mut content, Duration::from_millis(300));
    let first = content.export_frame(&gpu).unwrap();
    assert_eq!(first.get_pixel(16, 16).0, [0, 255, 0, 255]);
    drive(&mut content, Duration::from_millis(500));
    assert_eq!(
        first,
        content.export_frame(&gpu).unwrap(),
        "paused script kept running without presentation"
    );
    content.playback(playback).unwrap();
    drive(&mut content, Duration::from_millis(150));
    assert_ne!(first, content.export_frame(&gpu).unwrap());
}

#[test]
#[ignore = "requires Web-enabled wallpaperd, matching CEF and EGL/GLES 3"]
fn web_browser_regressions() {
    // CEF must be initialized and shut down on one thread. All browser lifetimes
    // below share that runtime, just as a real output worker does across switches.
    let _runtime = we_web::ShutdownGuard;
    web_browser_properties_pointer_resize_pause_execution_and_release();
    web_browser_partial_upload_and_skipped_damage_keep_unchanged_pixels();
    web_browser_video_and_webgl_share_correct_pixels();
    web_browser_gpu_pause_and_properties_work_without_presentation();
    crate::web::tests::web_browser_gpu_import_failure_restores_paused_properties_and_releases_fds();
    web_browser_candidates_keep_independent_properties_and_session_storage();
    web_browser_resource_boundary_reload_and_invalid_controls();
}

fn web_browser_candidates_keep_independent_properties_and_session_storage() {
    let project = tempfile::tempdir().unwrap();
    let entry = project.path().join("index.html");
    fs::write(&entry, r#"<!doctype html><body style="margin:0;background:red"><script>
    const count = Number(localStorage.count || 0) + 1; localStorage.count = count;
    wallpaperPropertyListener={applyUserProperties(p){document.body.style.background=`rgb(${p.tint.value},${count},0)`}};
    </script>"#).unwrap();
    let gpu = Gpu::headless(Size::new(64, 64).unwrap()).unwrap();
    let target = Target::new(gpu.gl.clone(), gpu.size).unwrap();
    let (wake, _reader) = UnixStream::pair().unwrap();
    let playback = Playback {
        fps: 60,
        ..Playback::default()
    };
    let mut old = crate::web::Web::load(
        project.path(),
        &entry,
        &gpu,
        playback,
        &crate::properties::Values::from([("tint".into(), json!(200))]),
        &wake,
    )
    .unwrap();
    let mut candidate = crate::web::Web::load(
        project.path(),
        &entry,
        &gpu,
        playback,
        &crate::properties::Values::from([("tint".into(), json!(100))]),
        &wake,
    )
    .unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    while !old.ready() || !candidate.ready() {
        for web in [&mut old, &mut candidate] {
            web.poll().unwrap();
            web.draw(&gpu, Some(target.fbo), false).unwrap();
        }
        assert!(Instant::now() < end, "simultaneous browsers stalled");
        thread::sleep(Duration::from_millis(5));
    }
    old.draw(&gpu, Some(target.fbo), true).unwrap();
    assert_eq!(
        gpu.read_image(&target).unwrap().get_pixel(32, 32).0,
        [200, 1, 0, 255]
    );
    candidate.draw(&gpu, Some(target.fbo), true).unwrap();
    assert_eq!(
        gpu.read_image(&target).unwrap().get_pixel(32, 32).0,
        [100, 1, 0, 255]
    );
    // Release an old browser while it may own a CEF GPU slot. The candidate must
    // continue receiving controls and frames on the same CEF UI thread.
    thread::sleep(Duration::from_millis(100));
    drop(old);
    candidate
        .properties(&crate::properties::Values::from([(
            "tint".into(),
            json!(50),
        )]))
        .unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        candidate.poll().unwrap();
        candidate.draw(&gpu, Some(target.fbo), false).unwrap();
        if gpu.read_image(&target).unwrap().get_pixel(32, 32).0 == [50, 1, 0, 255] {
            break;
        }
        assert!(
            Instant::now() < end,
            "dropping old browser blocked candidate"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn web_browser_resource_boundary_reload_and_invalid_controls() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("local.json"), r#"{"red":1}"#).unwrap();
    fs::write(root.path().join("outside.json"), r#"{"secret":1}"#).unwrap();
    std::os::unix::fs::symlink(
        root.path().join("outside.json"),
        project.join("escape.json"),
    )
    .unwrap();
    let entry = project.join("index.html");
    fs::write(&entry,r#"<!doctype html><body style="margin:0;background:red"><canvas id="c" width="64" height="64"></canvas><script>
    const count = Number(sessionStorage.count || 0)+1; sessionStorage.count=count;
    let escaped=false,local=false;
    window.wallpaperPropertyListener={async applyUserProperties(p){
      local = (await fetch('local.json').then(r=>r.json())).red === 1;
      try { await fetch('escape.json').then(r=>r.json()); escaped=true; } catch {}
      const gl=c.getContext('webgl'); gl.clearColor(escaped?1:0, count/255,local && p.blue.value?1:0,1);gl.clear(gl.COLOR_BUFFER_BIT);
    }};
    if(count===1)setTimeout(()=>location.reload(),400);
    </script>"#).unwrap();
    let gpu = Gpu::headless(Size::new(64, 64).unwrap()).unwrap();
    let target = Target::new(gpu.gl.clone(), gpu.size).unwrap();
    let (wake, _reader) = UnixStream::pair().unwrap();
    let mut web = crate::web::Web::load(
        &project,
        &entry,
        &gpu,
        Playback::default(),
        &crate::properties::Values::from([("blue".into(), json!(true))]),
        &wake,
    )
    .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        web.poll().unwrap();
        web.draw(&gpu, Some(target.fbo), false).unwrap();
        if web.ready() && gpu.read_image(&target).unwrap().get_pixel(32, 32).0 == [0, 2, 255, 255] {
            break;
        }
        assert!(
            Instant::now() < end,
            "reload/local fetch/symlink regression"
        );
        thread::sleep(Duration::from_millis(5));
    }
    // Invalid controls are now rejected at the library boundary, without killing
    // the worker or disturbing a valid browser.
    crate::web::tests::invalid_controls_leave_browser_alive(&mut web);
}
