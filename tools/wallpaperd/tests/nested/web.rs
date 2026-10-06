use super::*;

#[test]
#[ignore = "requires headless Weston, niri and Web-enabled wallpaperd"]
fn web_project_properties_snapshots_failed_switch_restore_and_release() {
    let mut rig = Rig::new();
    let project = rig.root().join("web project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("project.json"), r#"{"type":"web","file":"index.html","title":"Web probe","general":{"properties":{"tint":{"type":"color","value":"1 0 0"},"padding":{"type":"textinput","value":""},"listen":{"type":"bool","value":false},"useDefault":{"type":"bool","value":true},"customimage":{"type":"file"}}}}"#).unwrap();
    fs::write(project.join("index.html"), r#"<!doctype html><body style="margin:0;background:red"><script>
    window.wallpaperPropertyListener={applyUserProperties(p){
    if(p.useDefault?.value) p.customimage.value='';
    if(p.tint)
    document.body.style.background=`rgb(${p.tint.value.split(' ').map(v=>Math.round(v*255)).join(',')})`;
    if(p.listen?.value) setTimeout(()=>wallpaperRegisterAudioListener(()=>document.body.style.background='magenta'),300);
    }};
    </script>"#).unwrap();
    let path = project.to_str().unwrap();
    assert_eq!(
        rig.ok(&["status"])["state"]["backends"]["cef"]["available"],
        true
    );
    let daemon_pid = rig.daemon.as_ref().unwrap().id();
    // Larger than a pipe buffer: the worker must wake to complete a partial control write.
    let properties = serde_json::json!({"tint":[0,1,0],"padding":"x".repeat(30000)}).to_string();
    rig.ok(&["set", path, "--properties", &properties]);
    rig.wait_corner([0, 255, 0]);
    let status = rig.ok(&["status"]);
    assert_eq!(status["state"]["outputs"]["winit"]["backend"], "cef");
    if std::env::var("WALLPAPERD_WEB_DIRECT").as_deref() == Ok("1")
        && std::env::var("WALLPAPERD_WEB_GPU").as_deref() == Ok("1")
    {
        assert!(
            fs::read_to_string(rig.root().join("wallpaperd.log"))
                .unwrap()
                .contains("direct Wayland DMA-BUF presentation"),
            "direct mode never submitted a DMA-BUF to Wayland"
        );
    }
    assert_eq!(
        status["state"]["outputs"]["winit"]["properties_available"],
        true
    );
    assert_eq!(
        status["state"]["outputs"]["winit"]["properties"]["customimage"],
        ""
    );
    assert!(
        !rig.cli(&["set-properties", path, r#"{"customimage":"/etc/passwd"}"#])
            .status
            .success()
    );
    rig.ok(&["pause"]);
    rig.ok(&["set-properties", path, r#"{"tint":[0,0,1]}"#]);
    rig.wait_corner([0, 0, 255]);
    let snapshot = rig.ok(&["snapshot", "--output", "winit"]);
    let png = image::open(snapshot["path"].as_str().unwrap())
        .unwrap()
        .to_rgba8();
    assert_eq!(
        png.get_pixel(png.width() / 2, png.height() / 2).0,
        [0, 0, 255, 255]
    );
    assert!(
        !rig.cli(&["set-properties", path, r#"{"tint":[2,0,0]}"#])
            .status
            .success()
    );
    // Direct buffers must also support owned snapshots for cross-engine fades.
    let image = rig.root().join("web-transition.png");
    RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255]))
        .save(&image)
        .unwrap();
    rig.set(&image, "fade", 60);
    rig.wait_corner([255, 0, 0]);
    rig.set(&project, "fade", 60);
    rig.wait_corner([0, 0, 255]);
    let missing = rig.root().join("broken-web");
    fs::create_dir(&missing).unwrap();
    fs::write(
        missing.join("project.json"),
        r#"{"type":"web","file":"missing.html"}"#,
    )
    .unwrap();
    assert!(
        !rig.cli(&["set", missing.to_str().unwrap()])
            .status
            .success()
    );
    rig.wait_corner([0, 0, 255]);
    // A dead in-process browser host must release its Chromium process group;
    // the daemon then restores accepted Web content in a fresh output worker.
    assert!(
        !fs::read_to_string(format!("/proc/{daemon_pid}/maps"))
            .unwrap()
            .contains("/libcef.so"),
        "playing Web content loaded CEF outside its output worker"
    );
    let children = process_children(daemon_pid);
    let worker = *children
        .iter()
        .find(|pid| {
            fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|args| {
                args.split(|&byte| byte == 0)
                    .any(|arg| arg == b"render-worker")
            })
        })
        .expect("output worker missing");
    let profile = process_profile(worker);
    assert!(profile.is_dir(), "CEF profile was not created");
    let mut descendants = process_children(worker);
    let mut cursor = 0;
    while cursor < descendants.len() {
        descendants.extend(process_children(descendants[cursor]));
        cursor += 1;
    }
    assert!(!descendants.is_empty(), "Chromium subprocesses missing");
    assert_eq!(unsafe { libc::kill(worker as i32, libc::SIGKILL) }, 0);
    rig.wait_state(|state| {
        state["outputs"]["winit"]["renderer"] == "running"
            && state["outputs"]["winit"]["backend"] == "cef"
            && state["outputs"]["winit"]["paused"] == true
            && process_children(daemon_pid)
                .iter()
                .any(|&pid| pid != worker)
    });
    rig.wait_corner([0, 0, 255]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while descendants.iter().any(|pid| process_running(*pid)) {
        assert!(
            Instant::now() < deadline,
            "crashed worker retained Chromium children: {descendants:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!profile.exists(), "crashed worker retained its CEF profile");
    // A second consecutive host failure must stop rather than restart indefinitely.
    let recovered_worker = *process_children(daemon_pid).first().unwrap();
    let recovered_profile = process_profile(recovered_worker);
    assert_eq!(
        unsafe { libc::kill(recovered_worker as i32, libc::SIGKILL) },
        0
    );
    rig.wait_state(|state| {
        state["outputs"]["winit"]["renderer"] == "stopped"
            && state["outputs"]["winit"]["error"]["code"] == "renderer_failed"
    });
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        rig.ok(&["status"])["state"]["outputs"]["winit"]["renderer"],
        "stopped"
    );
    assert!(
        !recovered_profile.exists(),
        "failed worker retained its CEF profile"
    );
    // Restore project identity, accepted properties and paused intent after a daemon restart.
    rig.daemon.as_mut().unwrap().kill().unwrap();
    rig.daemon.take().unwrap().wait().unwrap();
    rig.start_daemon();
    rig.wait_corner([0, 0, 255]);
    rig.wait_state(|state| {
        state["outputs"]["winit"]["paused"] == true
            && state["outputs"]["winit"]["pending"].is_null()
            && state["outputs"]["winit"]["backend"] == "cef"
    });
    rig.ok(&["resume"]);
    rig.ok(&["set-properties", path, r#"{"listen":true}"#]);
    rig.wait_corner([255, 0, 255]);
    let profile = process_profile(
        *process_children(rig.daemon.as_ref().unwrap().id())
            .first()
            .unwrap(),
    );
    rig.ok(&["release"]);
    rig.wait_state(|state| state["outputs"]["winit"]["current"].is_null());
    assert!(
        !profile.exists(),
        "released worker retained its CEF profile"
    );
}

fn process_profile(pid: u32) -> PathBuf {
    fs::read(format!("/proc/{pid}/environ"))
        .unwrap()
        .split(|&byte| byte == 0)
        .find_map(|value| value.strip_prefix(b"WALLPAPERD_WEB_PROFILE="))
        .map(|path| PathBuf::from(String::from_utf8(path.to_vec()).unwrap()))
        .expect("daemon-owned CEF profile missing")
}

fn process_children(parent: u32) -> Vec<u32> {
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse().ok()?;
            let stat = fs::read_to_string(entry.path().join("stat")).ok()?;
            let (_, fields) = stat.rsplit_once(") ")?;
            (fields.split_whitespace().nth(1)?.parse::<u32>().ok()? == parent).then_some(pid)
        })
        .collect()
}
fn process_running(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, fields)| !fields.starts_with("Z "))
    })
}
