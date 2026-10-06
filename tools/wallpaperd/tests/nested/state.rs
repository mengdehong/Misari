use super::*;

#[test]
#[ignore = "requires headless Weston and WALLPAPERD_TEST_NIRI"]
fn library_import_rotation_and_source_safety() {
    use serde_json::json;
    let mut rig = Rig::new();
    let red = rig.root().join("Zebra ' $(literal).png");
    let blue = rig.root().join("aqua.png");
    RgbaImage::from_pixel(32, 32, Rgba([255, 0, 0, 255]))
        .save(&red)
        .unwrap();
    RgbaImage::from_pixel(32, 32, Rgba([0, 0, 255, 255]))
        .save(&blue)
        .unwrap();
    let edit = |rig: &Rig, value: Value| rig.ok(&["library-edit", &value.to_string()]);
    edit(&rig, json!({"action":"import","path":red}));
    edit(&rig, json!({"action":"import","path":blue}));
    edit(&rig, json!({"action":"import","path":red}));
    let catalog = rig.ok(&["list"]);
    let assets = catalog["assets"].as_array().unwrap();
    let id = |path: &Path| {
        assets
            .iter()
            .find(|a| a["path"] == path.to_str().unwrap())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let red_id = id(&red);
    let blue_id = id(&blue);
    assert_eq!(assets.iter().filter(|a| a["id"] == red_id).count(), 1);
    let playlist = edit(&rig, json!({"action":"create_playlist","name":"工作"}))["created"]
        .as_str()
        .unwrap()
        .to_owned();
    edit(
        &rig,
        json!({"action":"playlist_members","id":playlist,"members":[red_id,blue_id]}),
    );
    let library = rig.ok(&["library"])["library"].clone();
    assert!(
        !rig.cli(&[
            "library-edit",
            &json!({"action":"playlist_members","id":playlist,"members":[red_id,red_id]})
                .to_string()
        ])
        .status
        .success()
    );
    assert_eq!(
        rig.ok(&["library"])["library"],
        library,
        "invalid edits must be atomic"
    );
    rig.ok(&["set", &red_id, "--output", "winit"]);
    let rotation = json!({"source":playlist,"enabled":true,"mode":"ordered","interval_seconds":10,"transition":"cut"});
    rig.ok(&["rotation", &rotation.to_string(), "--output", "winit"]);
    rig.wait_state(|s| {
        s["outputs"]["winit"]["current"] == blue_id && s["outputs"]["winit"]["pending"].is_null()
    });
    rig.wait_corner([0, 0, 255]);
    // No panel or client owns this timer; the daemon advances after the CLI exits.
    rig.wait_state(|s| {
        s["outputs"]["winit"]["current"] == red_id && s["outputs"]["winit"]["pending"].is_null()
    });
    rig.wait_corner([255, 0, 0]);
    edit(&rig, json!({"action":"delete_playlist","id":playlist}));
    assert_eq!(
        rig.ok(&["status"])["state"]["rotations"]["winit"]["settings"]["enabled"],
        false
    );
    edit(&rig, json!({"action":"remove_source","path":red}));
    assert!(
        red.exists() && blue.exists(),
        "management never deletes source files"
    );
    assert!(
        !rig.ok(&["list"])["assets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == red_id)
    );

    rig.daemon.as_mut().unwrap().kill().unwrap();
    rig.daemon.take().unwrap().wait().unwrap();
    rig.start_daemon();
    assert!(red.exists() && blue.exists());
    assert!(
        !rig.ok(&["list"])["assets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == red_id),
        "removed sources stay excluded after restart"
    );
}

#[test]
#[ignore = "requires headless Weston and WALLPAPERD_TEST_NIRI"]
fn rust_properties_graph_rollback_pause_and_restore() {
    let mut rig = Rig::new();
    let project = rust_scene_fixture(rig.root());
    let asset = project.to_str().unwrap();
    rig.ok(&["set", asset, "--fit", "stretch", "--fps", "20"]);
    rig.wait_corner([255, 0, 0]);
    let state = rig.ok(&["status"])["state"]["outputs"]["winit"].clone();
    assert_eq!(state["backend"], "rust_scene");
    assert_eq!(state["properties_available"], true);
    // The first candidate frame precedes the playback/resume repaint. Wait
    // for a quiet interval instead of assuming both swaps finish in 150 ms.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut frames = rig.frames();
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < Duration::from_millis(400) {
        assert!(
            Instant::now() < deadline,
            "static effect graph never settled"
        );
        thread::sleep(Duration::from_millis(20));
        let current = rig.frames();
        if current != frames {
            frames = current;
            quiet_since = Instant::now();
        }
    }
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        frames,
        rig.frames(),
        "static effect graphs must stop committing"
    );
    rig.ok(&[
        "set-properties",
        asset,
        r#"{"strength":0.5,"tint":[0,0,1]}"#,
    ]);
    rig.wait_corner([0, 0, 128]);
    let shader = project.join("shaders/tone.frag");
    let original = fs::read(&shader).unwrap();
    fs::write(&shader, b"#error broken replacement").unwrap();
    rig.ok(&["set-properties", asset, r#"{"tint":[1,0,0]}"#]);
    rig.wait_corner([128, 0, 0]); // Uniform edits reuse the compiled shader.
    let failed = rig.cli(&["set-properties", asset, r#"{"style":true,"strength":0.2}"#]);
    assert!(!failed.status.success());
    rig.wait_corner([128, 0, 0]);
    assert_eq!(
        rig.ok(&["properties", asset])["outputs"]["winit"]["values"]["style"],
        false
    );
    fs::write(&shader, original).unwrap();
    rig.ok(&["set-properties", asset, r#"{"style":true}"#]);
    rig.wait_corner([0, 128, 0]);
    rig.ok(&["pause"]);
    thread::sleep(Duration::from_millis(100));
    let paused = rig.shot();
    let frames = rig.frames();
    let snapshot = rig.ok(&["snapshot"]);
    rig.ok(&["set-properties", asset, r#"{"style":false,"strength":1}"#]);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(rig.shot(), paused);
    assert_eq!(rig.frames(), frames);
    assert_eq!(rig.ok(&["snapshot"])["path"], snapshot["path"]);
    rig.ok(&["resume"]);
    rig.wait_corner([255, 0, 0]);
    rig.daemon.as_mut().unwrap().kill().unwrap();
    rig.daemon.take().unwrap().wait().unwrap();
    rig.start_daemon();
    rig.wait_corner([255, 0, 0]);
    assert_eq!(
        rig.ok(&["status"])["state"]["outputs"]["winit"]["backend"],
        "rust_scene"
    );
    let state = rig.root().join("state/misari/wallpaperd/state.json");
    let backup = state.with_extension("backup");
    fs::rename(&state, &backup).unwrap();
    fs::create_dir(&state).unwrap();
    let failed = rig.cli(&["set-properties", asset, r#"{"strength":0.5}"#]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("save_failed"));
    rig.wait_corner([128, 0, 0]);
    fs::remove_dir(&state).unwrap();
    fs::rename(&backup, &state).unwrap();
}
