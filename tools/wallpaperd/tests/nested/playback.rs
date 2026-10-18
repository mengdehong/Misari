use super::*;

#[test]
#[ignore = "requires Wayland, private PipeWire/Pulse servers, parec, paplay and WALLPAPERD_TEST_NIRI"]
fn other_audio_scene_switch_preserves_mute_without_burst() {
    let mut rig = Rig::new();
    let server = rig.audio_server();
    let scene = rust_scene_fixture(rig.root());
    let tone = scene.join("tone.wav");
    let generated = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=80:sample_rate=48000:duration=4",
            "-y",
        ])
        .arg(&tone)
        .output()
        .unwrap();
    assert!(generated.status.success());
    let entry = scene.join("scene.json");
    let mut data: Value = serde_json::from_slice(&fs::read(&entry).unwrap()).unwrap();
    data["objects"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": 99, "sound": ["tone.wav"], "volume": 1, "playbackmode": "loop"
        }));
    fs::write(&entry, serde_json::to_vec(&data).unwrap()).unwrap();
    let shader = rig.root().join("silent.frag");
    fs::write(
        &shader,
        "void mainImage(out vec4 c,in vec2 p){c=vec4(0,0,0,1);}",
    )
    .unwrap();
    rig.set(&shader, "cut", 30);
    rig.ok(&["playback", "--mute", "false"]);
    // A silent external stream engages the real auto-mute policy without
    // contaminating the monitor recording with another application's PCM.
    let silence = rig.root().join("silence.f32");
    File::create(&silence)
        .unwrap()
        .set_len(48000 * 2 * 4 * 120)
        .unwrap();
    let mut external = server.play(&silence);
    rig.wait_state(|s| {
        s["audio_policy"]["other_audio_active"] == true
            && s["outputs"]["winit"]["effective_mute"] == true
    });
    let recording = rig.root().join("switch.f32");
    let mut monitor = OwnedProcess(
        Command::new("parec")
            .args([
                "--raw",
                "--format=float32le",
                "--rate=48000",
                "--channels=2",
                "--latency-msec=10",
                "--device=wallpaperd_test_a.monitor",
            ])
            .env("PULSE_SERVER", server.address())
            .stdout(File::create(&recording).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while server.source_outputs().as_array().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "PCM monitor did not start");
        thread::sleep(Duration::from_millis(10));
    }
    let scene_id = format!("we:{}", scene.display());
    for transition in ["cut", "fade"] {
        rig.set(&scene, transition, 30);
        rig.wait_state(|s| {
            s["outputs"]["winit"]["current"] == scene_id
                && s["outputs"]["winit"]["pending"].is_null()
        });
        thread::sleep(Duration::from_millis(100));
        rig.set(&shader, "cut", 30);
        rig.wait_state(|s| s["outputs"]["winit"]["pending"].is_null());
    }
    thread::sleep(Duration::from_millis(200));
    assert!(
        external.0.try_wait().unwrap().is_none(),
        "auto-mute fixture stopped early"
    );
    let pcm = fs::read(&recording).unwrap();
    let samples = pcm.as_chunks::<4>().0;
    assert!(samples.len() > 4800, "empty PCM recording");
    let peak = samples
        .iter()
        .map(|s| f32::from_le_bytes(*s).abs())
        .fold(0f32, f32::max);
    let audible = samples
        .iter()
        .filter(|s| f32::from_le_bytes(**s).abs() > 0.001)
        .count();
    eprintln!(
        "scene switches: peak={peak:.6}, audible_ms={:.2}",
        audible as f64 / 96.0
    );
    // Prove the same scene and monitor actually carry audio when auto-mute clears.
    drop(external);
    rig.wait_state(|s| s["outputs"]["winit"]["effective_mute"] == false);
    rig.set(&scene, "cut", 30);
    thread::sleep(Duration::from_millis(500));
    monitor.0.kill().unwrap();
    monitor.0.wait().unwrap();
    let control = fs::read(&recording).unwrap();
    let control_peak = control[pcm.len()..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|s| f32::from_le_bytes(*s).abs())
        .fold(0f32, f32::max);
    assert!(
        control_peak > 0.01,
        "unmuted positive control was silent: {control_peak}"
    );
    assert!(peak < 0.001, "auto-muted scene switch emitted audible PCM");
}

#[test]
#[ignore = "requires Wayland, private PipeWire/PulseAudio servers, paplay and WALLPAPERD_TEST_NIRI"]
fn other_audio_mutes_without_pausing_and_preserves_user_choice() {
    let mut rig = Rig::new();
    let _audio = rig.audio_server();
    let shader = rig.root().join("animated.frag");
    fs::write(
        &shader,
        "void mainImage(out vec4 c,in vec2 p) { c=vec4(0.2+0.2*sin(iTime),0.4,0.6,1.0); }",
    )
    .unwrap();
    rig.set(&shader, "cut", 20);
    rig.ok(&["playback", "--mute", "false", "--volume", "37"]);
    rig.wait_state(|state| {
        state["audio_policy"]["available"] == true
            && state["outputs"]["winit"]["effective_mute"] == false
    });
    let wave = rig.root().join("silence.wav");
    let samples = vec![0u8; 16000 * 4];
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36u32 + samples.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(8000u32.to_le_bytes());
    bytes.extend(16000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((samples.len() as u32).to_le_bytes());
    bytes.extend(samples);
    fs::write(&wave, bytes).unwrap();
    let mut own = OwnedProcess(
        Command::new("paplay")
            .args([
                "--device=wallpaperd_test_a",
                "--property=application.id=misari.wallpaperd",
            ])
            .arg(&wave)
            .envs(rig.env.iter().cloned())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let inputs = Command::new("pactl")
            .args(["--format=json", "list", "sink-inputs"])
            .envs(rig.env.iter().cloned())
            .output()
            .unwrap();
        let inputs: Value = serde_json::from_slice(&inputs.stdout).unwrap();
        if inputs.as_array().unwrap().iter().any(|input| {
            input["properties"]["application.id"] == "misari.wallpaperd" && input["corked"] == false
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "tagged playback stream did not start"
        );
        thread::sleep(Duration::from_millis(50));
    }
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        rig.ok(&["status"])["state"]["audio_policy"]["other_audio_active"],
        false
    );
    own.0.kill().unwrap();
    own.0.wait().unwrap();
    let mut sound = OwnedProcess(
        Command::new("paplay")
            .arg("--device=wallpaperd_test_a")
            .arg(&wave)
            .envs(rig.env.iter().cloned())
            .spawn()
            .unwrap(),
    );
    let state = rig.wait_state(|state| state["outputs"]["winit"]["effective_mute"] == true);
    assert_eq!(state["outputs"]["winit"]["mute"], false);
    assert!(
        state["outputs"]["winit"]["pause_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .all(|reason| reason == "frame_throttled")
    );
    assert_eq!(state["outputs"]["winit"]["volume"], 37);
    let frames = rig.frames();
    thread::sleep(Duration::from_millis(250));
    assert!(rig.frames() > frames, "auto-mute stopped image rendering");
    sound.0.kill().unwrap();
    sound.0.wait().unwrap();
    rig.wait_state(|state| state["outputs"]["winit"]["effective_mute"] == false);
    rig.ok(&["playback", "--mute", "true"]);
    let mut sound = OwnedProcess(
        Command::new("paplay")
            .arg("--device=wallpaperd_test_a")
            .arg(&wave)
            .envs(rig.env.iter().cloned())
            .spawn()
            .unwrap(),
    );
    rig.wait_state(|state| state["audio_policy"]["other_audio_active"] == true);
    sound.0.kill().unwrap();
    sound.0.wait().unwrap();
    let state = rig.wait_state(|state| {
        state["outputs"]["winit"]["mute_reasons"] == serde_json::json!(["user"])
    });
    assert_eq!(state["outputs"]["winit"]["effective_mute"], true);
    let saved: Value = serde_json::from_slice(
        &fs::read(rig.root().join("state/misari/wallpaperd/state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["outputs"]["winit"]["volume"], 37);
    assert_eq!(saved["outputs"]["winit"]["mute"], true);
    rig.daemon.as_mut().unwrap().kill().unwrap();
    rig.daemon.take().unwrap().wait().unwrap();
    rig.start_daemon();
    rig.wait_state(|state| {
        state["outputs"]["winit"]["current"] == format!("local:{}", shader.display())
            && state["outputs"]["winit"]["volume"] == 37
            && state["outputs"]["winit"]["mute"] == true
    });
}

#[test]
#[ignore = "requires headless Weston, ffmpeg and WALLPAPERD_TEST_NIRI"]
fn video_pixels_loop_pause_and_resume() {
    let mut rig = Rig::new();
    let video = rig.root().join("loop.mkv");
    let generated = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=red:s=64x64:r=20:d=1",
            "-f",
            "lavfi",
            "-i",
            "color=lime:s=64x64:r=20:d=1",
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1:a=0",
            "-c:v",
            "ffv1",
            "-colorspace",
            "smpte170m",
            "-y",
        ])
        .arg(&video)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    rig.set(&video, "cut", 20);
    rig.wait_corner([255, 0, 0]);
    rig.wait_corner([0, 255, 0]);
    rig.wait_corner([255, 0, 0]); // The decoder must loop, not freeze on the last frame.
    rig.ok(&["pause"]);
    thread::sleep(Duration::from_millis(200));
    let paused = rig.shot();
    let frames = rig.frames();
    thread::sleep(Duration::from_millis(300));
    assert_eq!(rig.shot(), paused);
    assert_eq!(rig.frames(), frames);
    rig.ok(&["resume"]);
    rig.wait_corner([0, 255, 0]);
}
