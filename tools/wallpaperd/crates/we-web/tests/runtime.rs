//! Run on the real application thread and reuse this executable for Chromium.
use serde_json::json;
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
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
            we_web::Renderer::load(root.path(), &entry, [64, 64], 60, false, "headless", &wake)
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
    second
        .send(json!({"properties":{"tint":75}}), false)
        .unwrap();
    second
        .send(
            json!({"pointer":{"focused":true,"x":0.5,"y":0.5,"down":false}}),
            false,
        )
        .unwrap();
    wait(&second, 75);
    second
        .send(json!({"properties":{"tint":100}}), false)
        .unwrap();
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
    frame_pacing_with_frequent_controls(&wake, "multi-browser");
    drop(third);
    frame_pacing_with_frequent_controls(&wake, "first-browser");
    paused_unchanged_property_refreezes(&wake);
    stalled_revision_reports_error(&wake);
    unresponsive_renderer_is_replaced(&wake);
    resized_frame_releases_unused_pages(&wake);
    println!(
        "CEF runtime: contexts, controls, reuse, frame pacing, hang recovery and resize memory passed"
    );
}

fn stalled_revision_reports_error(wake: &UnixStream) {
    let check = |script: &str, paused, label: &str| {
        let root = tempfile::tempdir().unwrap();
        let entry = root.path().join("index.html");
        fs::write(
            &entry,
            format!(
                r#"<!doctype html><body style="margin:0;background:red"><script>{script}</script>"#
            ),
        )
        .unwrap();
        let browser =
            we_web::Renderer::load(root.path(), &entry, [64, 64], 60, false, "headless", wake)
                .unwrap();
        browser
            .send(
                json!({"size":{"width":64,"height":64},"properties":{"value":0},
                    "playback":{"paused":false,"mute":true,"volume":100,"fps":60}}),
                false,
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let update = browser.poll();
            assert!(
                update.error.is_none(),
                "{label} initial: {:?}",
                update.error
            );
            if browser.with_pixels(|_| ()).unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "{label} initial frame missing");
            thread::sleep(Duration::from_millis(5));
        }
        browser
            .send(
                json!({"properties":{"value":1},
                    "playback":{"paused":paused,"mute":true,"volume":100,"fps":60}}),
                true,
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(7);
        loop {
            if let Some(error) = browser.poll().error {
                assert!(
                    error.contains("revision did not become ready"),
                    "{label}: unexpected error: {error}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{label}: revision deadline did not fire"
            );
            thread::sleep(Duration::from_millis(5));
        }
    };

    check(
        "document.addEventListener('visibilitychange',e=>e.stopImmediatePropagation(),true);",
        true,
        "blocked visibility event",
    );
    check(
        "wallpaperPropertyListener={applyUserProperties(p){if(p.value?.value===1)while(true){}}};",
        false,
        "hung property callback",
    );
}

fn unresponsive_renderer_is_replaced(wake: &UnixStream) {
    let root = tempfile::tempdir().unwrap();
    let entry = root.path().join("index.html");
    fs::write(
        &entry,
        r#"<!doctype html><style>html,body{width:100%;height:100%;margin:0}body{background:red}</style><body><script>
        document.addEventListener('click',()=>{
          document.body.style.background='blue';
          setTimeout(()=>{while(true){}},500);
        });
        </script>"#,
    )
    .unwrap();
    let load = |entry: &std::path::Path| {
        let browser =
            we_web::Renderer::load(root.path(), entry, [64, 64], 60, false, "headless", wake)
                .unwrap();
        browser
            .send(
                json!({"size":{"width":64,"height":64},"properties":{},
                    "playback":{"paused":false,"mute":true,"volume":100,"fps":60}}),
                false,
            )
            .unwrap();
        browser
    };
    let wait_for_pixel = |browser: &we_web::Renderer, expected, label: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let update = browser.poll();
            assert!(update.error.is_none(), "{label}: {:?}", update.error);
            let pixel = browser
                .with_pixels(|pixels| pixels.data[(32 * 64 + 32) * 4..][..4].to_vec())
                .unwrap();
            if pixel.as_deref() == Some(expected) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{label} frame did not arrive: {pixel:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    };

    let hung = load(&entry);
    wait_for_pixel(&hung, &[0, 0, 255, 255], "hang test initial");
    for down in [true, false] {
        hung.send(
            json!({"pointer":{"focused":true,"x":0.5,"y":0.5,"down":down}}),
            true,
        )
        .unwrap();
    }
    wait_for_pixel(&hung, &[255, 0, 0, 255], "hang test click handler");
    thread::sleep(Duration::from_millis(750));
    hung.send(
        json!({"pointer":{"focused":true,"x":0.75,"y":0.5,"down":false}}),
        true,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if let Some(error) = hung.poll().error {
            assert!(
                error.contains("unresponsive"),
                "unexpected hung renderer error: {error}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "hung renderer was not terminated by CEF's responsiveness monitor"
        );
        thread::sleep(Duration::from_millis(10));
    }
    drop(hung);

    let replacement_entry = root.path().join("replacement.html");
    fs::write(
        &replacement_entry,
        "<!doctype html><body style='margin:0;background:lime'>",
    )
    .unwrap();
    let replacement = load(&replacement_entry);
    wait_for_pixel(
        &replacement,
        &[0, 255, 0, 255],
        "replacement after renderer hang",
    );
}

fn paused_unchanged_property_refreezes(wake: &UnixStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let stopped = Arc::new(AtomicBool::new(false));
    let server = {
        let requests = requests.clone();
        let stopped = stopped.clone();
        thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buffer = [0; 1024];
                        let _ = stream.read(&mut buffer);
                        requests.fetch_add(1, Ordering::Relaxed);
                        let _ = stream.write_all(
                            b"HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accepting timer probe: {error}"),
                }
            }
        })
    };
    let root = tempfile::tempdir().unwrap();
    let entry = root.path().join("index.html");
    fs::write(
        &entry,
        format!(
            r#"<!doctype html><body style="margin:0;background:rgb(10,0,0)"><script>
    let tick=0;setInterval(()=>fetch('http://{address}/'+(++tick),{{mode:'no-cors'}}),10);
    wallpaperPropertyListener={{applyUserProperties(p){{
	      if(p.noop) document.body.style.background=p.noop.value===2?'black':'rgb(10,0,0)';
    }}}};
    </script>"#
        ),
    )
    .unwrap();
    let browser =
        we_web::Renderer::load(root.path(), &entry, [64, 64], 60, false, "headless", wake).unwrap();
    let state = |noop, paused, fps| {
        json!({"size":{"width":64,"height":64},
        "properties":{"noop":noop},
        "playback":{"paused":paused,"mute":true,"volume":100,"fps":fps}})
    };
    browser.send(state(0, false, 60), false).unwrap();
    let pixel = |browser: &we_web::Renderer, black: bool, label: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let update = browser.poll();
            assert!(update.error.is_none(), "{:?}", update.error);
            let red = browser
                .with_pixels(|p| p.data[(32 * 64 + 32) * 4 + 2])
                .unwrap();
            if red.is_some_and(|red| (red == 0) == black) {
                break red.unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "{label} frame did not arrive: {red:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
    };
    pixel(&browser, false, "initial");
    let deadline = Instant::now() + Duration::from_secs(5);
    while requests.load(Ordering::Relaxed) < 10 {
        assert!(Instant::now() < deadline, "page timer did not start");
        thread::sleep(Duration::from_millis(5));
    }
    // Supersede paused revisions while their visibility barriers are in flight.
    // Every control is an edge so the transport cannot collapse the sequence.
    for noop in [1, 2, 1, 2] {
        browser.send(state(noop, true, 60), true).unwrap();
    }
    browser.send(state(2, false, 60), true).unwrap();
    pixel(&browser, true, "rapid paused revisions resumed");
    browser.send(state(0, false, 60), false).unwrap();
    pixel(&browser, false, "rapid revision reset");
    let started = Instant::now();
    browser.send(state(2, true, 1), false).unwrap();
    pixel(&browser, true, "one-fps paused property");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "one-fps paused property waited for playback-rate rAF"
    );
    // Pausing and changing author state without changing a pixel must still
    // complete the apply/refreeze lifecycle.
    browser.send(state(1, true, 1), false).unwrap();
    thread::sleep(Duration::from_millis(150));
    let before = requests.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(500));
    let advanced = requests.load(Ordering::Relaxed) - before;
    println!("CEF paused unchanged-property timer requests: {advanced}");
    assert!(
        advanced <= 2,
        "unchanged paused property left page timers running: {advanced} requests"
    );
    browser
        .with_pixels(|_| {
            browser.send(state(2, false, 60), false).unwrap();
            thread::sleep(Duration::from_millis(150));
        })
        .unwrap()
        .unwrap();
    pixel(&browser, true, "resumed property");
    let deadline = Instant::now() + Duration::from_secs(5);
    while requests.load(Ordering::Relaxed) <= before + advanced + 2 {
        assert!(
            Instant::now() < deadline,
            "resumed page timer did not restart"
        );
        thread::sleep(Duration::from_millis(5));
    }
    drop(browser);
    stopped.store(true, Ordering::Release);
    server.join().unwrap();
}

fn frame_pacing_with_frequent_controls(wake: &UnixStream, context: &str) {
    let root = tempfile::tempdir().unwrap();
    let entry = root.path().join("index.html");
    fs::write(&entry, r#"<!doctype html><body style="margin:0"><canvas id="c" width="64" height="64"></canvas><script>
    const ctx=c.getContext('2d');let tick=0;
    function paint(){ctx.fillStyle=`rgb(${++tick%255},40,80)`;ctx.fillRect(0,0,64,64);requestAnimationFrame(paint)}paint();
    </script>"#).unwrap();
    let browser =
        we_web::Renderer::load(root.path(), &entry, [64, 64], 60, false, "headless", wake).unwrap();
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
        let mut last_pixel = None;
        let mut unique_frames = 0;
        while start.elapsed() < Duration::from_secs(2) {
            assert!(browser.poll().error.is_none());
            if let Some(pixel) = browser.with_pixels(|p| p.data[..4].to_vec()).unwrap()
                && last_pixel.as_ref() != Some(&pixel)
            {
                unique_frames += 1;
                last_pixel = Some(pixel);
            }
            if control.elapsed() >= Duration::from_millis(10) {
                // Keep covering redundant full-state controls as well as deltas.
                browser.send(state.clone(), false).unwrap();
                browser.send(json!({"audio":[0.25,0.5]}), false).unwrap();
                browser
                    .send(
                        json!({"pointer":{"focused":true,"x":0.5,"y":0.5,"down":false}}),
                        false,
                    )
                    .unwrap();
                control = Instant::now();
            }
            thread::sleep(Duration::from_millis(1));
        }
        let last = browser.with_pixels(|p| p.sequence).unwrap().unwrap();
        let fps = (last - first) as f64 / start.elapsed().as_secs_f64();
        println!(
            "CEF {context} animation: {fps:.1} paints/s, {unique_frames} unique JS frames (limit {limit})"
        );
        assert!(
            fps >= f64::from(limit) * 0.8 && fps <= f64::from(limit) + 1.5,
            "unchanged playback controls disturbed frame pacing: {fps:.1} FPS (limit {limit})"
        );
        assert!(
            unique_frames >= limit as usize * 8 / 5 && unique_frames <= limit as usize * 11 / 5,
            "CEF {context} delivered {unique_frames} unique JS frames in two seconds (limit {limit})"
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
        we_web::Renderer::load(root.path(), &entry, [64, 64], 60, false, "headless", wake).unwrap();
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
