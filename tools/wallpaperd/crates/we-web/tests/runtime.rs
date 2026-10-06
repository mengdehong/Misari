//! Run on the real application thread and reuse this executable for Chromium.
use serde_json::json;
use std::{
    fs,
    os::unix::net::UnixStream,
    thread,
    time::{Duration, Instant},
};

fn main() {
    if let Some(code) = we_web::execute_process() {
        std::process::exit(code);
    }
    if !std::env::args().any(|arg| arg == "--ignored") {
        println!("CEF runtime check skipped; pass --ignored to run it");
        return;
    }
    assert!(we_web::runtime_directory().is_some());
    assert!(
        !fs::read_to_string("/proc/self/maps")
            .unwrap()
            .contains("/libcef.so"),
        "locating the Web runtime loaded CEF before any browser was needed"
    );
    // No application threads exist before CEF initialization in this test runner.
    unsafe {
        std::env::set_var(
            "WALLPAPERD_WEB_SUBPROCESS",
            std::env::current_exe().unwrap(),
        );
    }
    let _runtime = we_web::ShutdownGuard;
    let root = tempfile::tempdir().unwrap();
    let entry = root.path().join("index.html");
    fs::write(&entry,r#"<!doctype html><body style="margin:0;background:red"><div id="box" style="position:absolute;width:8px;height:8px"></div><script>
    let tick=0;setInterval(()=>box.style.background=(++tick%2?"blue":"green"),50);
    const count=Number(localStorage.count||0)+1;localStorage.count=count;
    wallpaperPropertyListener={applyUserProperties(p){document.body.style.background=`rgb(${p.tint.value},${count},0)`}};
    </script>"#).unwrap();
    let (wake, _reader) = UnixStream::pair().unwrap();
    let browser = |tint| {
        let browser =
            we_web::Renderer::load(root.path(), &entry, [64, 64], false, "headless", &wake)
                .unwrap();
        browser
            .send(
                json!({"size":{"width":64,"height":64},"properties":{"tint":tint},
            "playback":{"paused":false,"mute":true,"volume":100,"fps":60}}),
                false,
            )
            .unwrap();
        browser
    };
    let first = browser(200);
    let second = browser(100);
    let wait = |browser: &we_web::Renderer, tint| {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let update = browser.poll();
            assert!(update.error.is_none(), "{:?}", update.error);
            let pixel = browser
                .with_pixels(|pixels| {
                    pixels.data[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 4].to_vec()
                })
                .unwrap();
            if pixel == Some(vec![0, 1, tint, 255]) {
                break;
            }
            assert!(
                Instant::now() < end,
                "browser state/storage not isolated: {pixel:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    };
    wait(&first, 200);
    wait(&second, 100);
    // Force paints to miss the mmap write lock; the next successful paint must
    // restore a complete image, including all damage dropped during the lock.
    second.poll();
    second
        .with_pixels(|pixels| {
            assert_eq!(pixels.size, [64, 64]);
            thread::sleep(Duration::from_millis(350));
        })
        .unwrap()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let update = second.poll();
        assert!(update.error.is_none(), "{:?}", update.error);
        if update.frame {
            assert_eq!(update.damage.unwrap().1, [0, 0, 64, 64]);
            break;
        }
        assert!(Instant::now() < deadline, "dropped paint did not recover");
        thread::sleep(Duration::from_millis(1));
    }
    assert!(second.send(json!([]), false).is_err());
    assert!(
        second
            .send(json!({"big":"x".repeat(65536)}), false)
            .is_err()
    );
    drop(first);
    wait(&second, 100);
    drop(second);
    let third = browser(50);
    wait(&third, 50);
    drop(third);
    frame_pacing_with_frequent_controls(&wake);
    resized_frame_releases_unused_pages(&wake);
    println!("CEF runtime: contexts, controls, reuse, frame pacing and resize memory passed");
}

fn frame_pacing_with_frequent_controls(wake: &UnixStream) {
    let root = tempfile::tempdir().unwrap();
    let entry = root.path().join("index.html");
    fs::write(&entry, r#"<!doctype html><body style="margin:0"><canvas id="c" width="64" height="64"></canvas><script>
    const ctx=c.getContext('2d');let tick=0;
    function paint(){ctx.fillStyle=`rgb(${++tick%255},40,80)`;ctx.fillRect(0,0,64,64);requestAnimationFrame(paint)}paint();
    </script>"#).unwrap();
    let browser =
        we_web::Renderer::load(root.path(), &entry, [64, 64], false, "headless", wake).unwrap();
    let mut state = json!({"size":{"width":64,"height":64},"properties":{},
        "playback":{"paused":false,"mute":true,"volume":100,"fps":60}});
    browser.send(state.clone(), false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(browser.poll().error.is_none());
        if browser.with_pixels(|p| p.sequence).unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "animation did not start");
        thread::sleep(Duration::from_millis(5));
    }
    for limit in [60, 30, 15] {
        state["playback"]["fps"] = limit.into();
        browser.send(state.clone(), false).unwrap();
        thread::sleep(Duration::from_millis(300));
        let first = browser.with_pixels(|p| p.sequence).unwrap().unwrap();
        let start = Instant::now();
        let mut control = start;
        while start.elapsed() < Duration::from_secs(2) {
            assert!(browser.poll().error.is_none());
            if control.elapsed() >= Duration::from_millis(10) {
                // Audio and pointer updates carry the unchanged playback state too.
                browser.send(state.clone(), false).unwrap();
                control = Instant::now();
            }
            thread::sleep(Duration::from_millis(1));
        }
        let last = browser.with_pixels(|p| p.sequence).unwrap().unwrap();
        let fps = (last - first) as f64 / start.elapsed().as_secs_f64();
        println!("CEF animation with frequent controls: {fps:.1} FPS (limit {limit})");
        assert!(
            fps >= f64::from(limit) * 0.8 && fps <= f64::from(limit) + 1.5,
            "unchanged playback controls disturbed frame pacing: {fps:.1} FPS (limit {limit})"
        );
    }
}

fn resized_frame_releases_unused_pages(wake: &UnixStream) {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    let entry = root.path().join("index.html");
    fs::write(
        &entry,
        "<!doctype html><body style='margin:0;background:lime'>",
    )
    .unwrap();
    let mut browser =
        we_web::Renderer::load(root.path(), &entry, [64, 64], false, "headless", wake).unwrap();
    let mut frame = None;
    for size in [[1024, 1024], [64, 64]] {
        browser.resize(size).unwrap();
        browser
            .send(
                json!({"size":{"width":size[0],"height":size[1]},"properties":{},
            "playback":{"paused":false,"mute":true,"volume":100,"fps":60}}),
                false,
            )
            .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(browser.poll().error.is_none());
            if browser.with_pixels(|p| p.size == size).unwrap() == Some(true) {
                break;
            }
            assert!(
                Instant::now() < end,
                "browser resize did not paint {size:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
        if frame.is_none() {
            frame = fs::read_dir("/proc/self/fd").unwrap().find_map(|entry| {
                let path = entry.ok()?.path();
                let name = fs::read_link(&path).ok()?;
                (name.to_string_lossy().contains("memfd:we-web-frame-mmap")
                    && path.metadata().ok()?.len() == 16 + 1024 * 1024 * 4)
                    .then(|| fs::File::open(path).unwrap())
            });
            assert!(frame.as_ref().unwrap().metadata().unwrap().blocks() * 512 >= 4 * 1024 * 1024);
        }
    }
    let allocated = frame.unwrap().metadata().unwrap().blocks() * 512;
    println!("CEF shrunk frame allocation: {allocated} bytes");
    assert!(
        allocated < 128 * 1024,
        "resized browser retained its large frame: {allocated} bytes"
    );
}
