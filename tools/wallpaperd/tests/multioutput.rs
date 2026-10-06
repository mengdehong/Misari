//! Two real layer-shell outputs, one daemon monitor, and isolated Pulse/Wayland servers.
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

#[path = "support/audio_server.rs"]
mod audio;
#[path = "support/scene.rs"]
mod scene;

use audio::Process;
struct Rig {
    root: tempfile::TempDir,
    env: Vec<(String, OsString)>,
    daemon: Option<Process>,
    compositor: Option<Process>,
}
impl Rig {
    fn new(server: &audio::Server) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut env = Vec::new();
        for (key, subdir) in [
            ("XDG_RUNTIME_DIR", "runtime"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
        ] {
            let path = root.path().join(subdir);
            fs::create_dir(&path).unwrap();
            env.push((key.into(), path.into_os_string()));
        }
        env.extend([
            ("WLR_BACKENDS".into(), "headless".into()),
            ("WLR_HEADLESS_OUTPUTS".into(), "2".into()),
            ("WLR_RENDERER".into(), "gles2".into()),
            ("PULSE_SERVER".into(), server.address().into()),
            (
                "DBUS_SESSION_BUS_ADDRESS".into(),
                format!("unix:path={}/no-bus", root.path().display()).into(),
            ),
        ]);
        let config = root.path().join("config");
        fs::create_dir(config.join("labwc")).unwrap();
        fs::write(config.join("labwc/rc.xml"), "<labwc_config/>").unwrap();
        fs::write(config.join("labwc/autostart"), "").unwrap();
        fs::create_dir(config.join("misari")).unwrap();
        fs::write(config.join("misari/wallpaperd.toml"),
            "pause_on_session = false\nmute_on_other_audio = false\nmedia_integration = false\n[wallpaper_engine]\nscene_backend = \"rust\"\n").unwrap();
        let mut rig = Self {
            root,
            env,
            daemon: None,
            compositor: None,
        };
        let compositor = rig.start("labwc", &["-C", config.join("labwc").to_str().unwrap()]);
        rig.compositor = Some(compositor);
        let display = rig.wait(|| {
            fs::read_dir(rig.root.path().join("runtime"))
                .unwrap()
                .filter_map(Result::ok)
                .find_map(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    (name.starts_with("wayland-") && !name.ends_with(".lock")).then_some(name)
                })
        });
        rig.env.push(("WAYLAND_DISPLAY".into(), display.into()));
        rig.daemon = Some(rig.start(env!("CARGO_BIN_EXE_wallpaperd"), &["serve"]));
        rig.wait(|| {
            let reply = rig.cli(&["status"]);
            reply
                .status
                .success()
                .then(|| serde_json::from_slice::<Value>(&reply.stdout).unwrap())
                .filter(|s| {
                    s["state"]["outputs"]
                        .as_object()
                        .is_some_and(|o| o.len() == 2)
                })
        });
        rig
    }
    fn command(&self, program: &str) -> Command {
        let mut c = Command::new(program);
        for key in [
            "WAYLAND_DISPLAY",
            "WAYLAND_SOCKET",
            "DISPLAY",
            "NIRI_SOCKET",
        ] {
            c.env_remove(key);
        }
        c.envs(self.env.iter().cloned());
        c
    }
    fn start(&self, program: &str, args: &[&str]) -> Process {
        let log = File::options()
            .create(true)
            .append(true)
            .open(self.root.path().join("process.log"))
            .unwrap();
        Process(
            self.command(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        )
    }
    fn cli(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_wallpaperd"))
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let o = self.cli(args);
        assert!(
            o.status.success(),
            "wallpaperd {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice(&o.stdout).unwrap()
    }
    fn wait<T>(&self, mut condition: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(value) = condition() {
                return value;
            }
            assert!(Instant::now() < deadline, "two-output state timed out");
            thread::sleep(Duration::from_millis(35));
        }
    }
    fn spectrum(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        self.wait(|| {
            let s = self.ok(&["status"])["state"]["audio_spectrum"].clone();
            predicate(&s).then_some(s)
        })
    }
    fn pixel(&self, output: &str) -> [u8; 4] {
        let snapshot = self.ok(&["snapshot", "--output", output]);
        let image = image::open(snapshot["path"].as_str().unwrap())
            .unwrap()
            .to_rgba8();
        image.get_pixel(image.width() / 2, image.height() / 2).0
    }
    fn channel(&self, output: &str, dominant: usize) {
        self.wait(|| {
            let pixel = self.pixel(output);
            (pixel[dominant] >= 96 && pixel[1 - dominant] <= 32 && pixel[2] <= 8).then_some(())
        });
    }
    fn worker(&self, output: &str) -> u32 {
        let daemon = self.daemon.as_ref().unwrap().0.id();
        let children =
            fs::read_to_string(format!("/proc/{daemon}/task/{daemon}/children")).unwrap();
        children
            .split_whitespace()
            .find_map(|id| {
                let args = fs::read(format!("/proc/{id}/cmdline")).ok()?;
                let args: Vec<_> = args.split(|b| *b == 0).collect();
                (args.contains(&b"render-worker".as_slice()) && args.contains(&output.as_bytes()))
                    .then(|| id.parse().unwrap())
            })
            .expect("output worker not found")
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        if thread::panicking() && self.daemon.is_some() {
            eprintln!(
                "last daemon state: {}",
                String::from_utf8_lossy(&self.cli(&["status"]).stdout)
            );
        }
        self.daemon.take();
        self.compositor.take();
        let log = fs::read_to_string(self.root.path().join("process.log")).unwrap_or_default();
        if thread::panicking() {
            eprintln!("two-output process log:\n{log}");
        }
        if let Some(dir) = std::env::var_os("WALLPAPERD_TEST_LOG_DIR") {
            fs::create_dir_all(&dir).unwrap();
            fs::write(Path::new(&dir).join("multioutput-process.log"), log).unwrap();
        }
    }
}
struct Stopped(u32);
impl Stopped {
    fn new(pid: u32) -> Self {
        assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGSTOP) }, 0);
        Self(pid)
    }
}
impl Drop for Stopped {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0 as i32, libc::SIGCONT);
        }
    }
}
fn pcm(root: &Path, name: &str, amplitudes: [f32; 2]) -> PathBuf {
    let path = root.join(name);
    let mut file = File::create(&path).unwrap();
    let mut second = Vec::with_capacity(48000 * 8);
    for n in 0..48000 {
        for (frequency, amplitude) in [750., 6000.].into_iter().zip(amplitudes) {
            second.extend(
                (amplitude * (std::f32::consts::TAU * frequency * n as f32 / 48000.).sin())
                    .to_le_bytes(),
            );
        }
    }
    for _ in 0..60 {
        file.write_all(&second).unwrap();
    }
    path
}

#[test]
#[ignore = "requires labwc, EGL/GLES, pipewire, wireplumber, pactl and paplay"]
fn shared_monitor_two_outputs_pause_slow_worker_reconnect_errors_and_release() {
    let mut server = audio::Server::new();
    let rig = Rig::new(&server);
    let project = scene::rust_scene_fixture(rig.root.path());
    let scene_path = project.join("scene.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&scene_path).unwrap()).unwrap();
    value["objects"][0]["effects"] = json!([]);
    value["objects"][0]["color"] = json!({"value":[0,0,0],"script":
        "const audio=engine.registerAudioBuffers(64);export function update(v){if(engine.userProperties.fault)while(true){};let left=0,right=0;for(let i=0;i<64;i++){left=Math.max(left,audio.left[i]);right=Math.max(right,audio.right[i]);}return new Vec3(left,right,0);}"});
    fs::write(scene_path, serde_json::to_vec(&value).unwrap()).unwrap();
    let metadata = project.join("project.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&metadata).unwrap()).unwrap();
    value["general"]["properties"]["fault"] = json!({"type":"bool","value":false});
    fs::write(metadata, serde_json::to_vec(&value).unwrap()).unwrap();
    let asset = project.to_str().unwrap();
    rig.ok(&[
        "set", asset, "--fit", "stretch", "--fps", "30", "--mute", "true",
    ]);
    let state = rig.ok(&["status"])["state"].clone();
    for output in ["HEADLESS-1", "HEADLESS-2"] {
        assert_eq!(state["outputs"][output]["backend"], "rust_scene");
        assert_eq!(state["outputs"][output]["effective_mute"], true);
    }
    // Scene buffers have a dB response, rather than linear PCM amplitudes.
    // Avoid saturation and check channel ownership without fixing that response curve.
    let first = pcm(server.root.path(), "first.f32", [0.01, 0.0001]);
    let second = pcm(server.root.path(), "second.f32", [0.0001, 0.01]);
    let playback = server.play(&first);
    for output in ["HEADLESS-1", "HEADLESS-2"] {
        rig.channel(output, 0);
    }
    assert_eq!(
        server.source_outputs().as_array().unwrap().len(),
        1,
        "one shared monitor for both outputs"
    );
    let sequence = rig.spectrum(|s| s["capturing"] == true)["sequence"]
        .as_u64()
        .unwrap();
    let stopped = Stopped::new(rig.worker("HEADLESS-1"));
    drop(playback);
    let playback = server.play(&second);
    rig.channel("HEADLESS-2", 1);
    let advanced = rig.spectrum(|s| s["sequence"].as_u64().unwrap() > sequence + 20);
    assert_eq!(server.source_outputs().as_array().unwrap().len(), 1);
    drop(stopped);
    rig.channel("HEADLESS-1", 1);
    println!(
        "two actual outputs share one monitor; blocked worker skipped more than {} snapshots",
        advanced["sequence"].as_u64().unwrap() - sequence
    );
    rig.ok(&["pause", "--output", "HEADLESS-1"]);
    let frozen = rig.pixel("HEADLESS-1");
    drop(playback);
    let playback = server.play(&first);
    rig.channel("HEADLESS-2", 0);
    assert_eq!(rig.pixel("HEADLESS-1"), frozen);
    assert_eq!(server.source_outputs().as_array().unwrap().len(), 1);
    rig.ok(&["pause", "--output", "HEADLESS-2"]);
    rig.spectrum(|s| s["capturing"] == false);
    rig.wait(|| {
        server
            .source_outputs()
            .as_array()
            .unwrap()
            .is_empty()
            .then_some(())
    });
    rig.ok(&["resume"]);
    for output in ["HEADLESS-1", "HEADLESS-2"] {
        rig.channel(output, 0);
    }
    assert_eq!(server.source_outputs().as_array().unwrap().len(), 1);
    drop(playback);
    server.disconnect();
    rig.spectrum(|s| s["available"] == false);
    for output in ["HEADLESS-1", "HEADLESS-2"] {
        rig.wait(|| (rig.pixel(output)[..3] == [0, 0, 0]).then_some(()));
    }
    server.restart();
    let playback = server.play(&second);
    for output in ["HEADLESS-1", "HEADLESS-2"] {
        rig.channel(output, 1);
    }
    assert_eq!(server.source_outputs().as_array().unwrap().len(), 1);
    println!("pause/resume/reconnect passed; checking one output script budget");
    rig.ok(&[
        "set-properties",
        asset,
        r#"{"fault":true}"#,
        "--output",
        "HEADLESS-1",
    ]);
    rig.wait(|| {
        let state = rig.ok(&["status"])["state"].clone();
        state["outputs"]["HEADLESS-1"]["diagnostics"]
            .as_str()
            .filter(|s| s.contains("SceneScript"))
            .map(|_| ())
    });
    drop(playback);
    let _playback = server.play(&first);
    rig.channel("HEADLESS-2", 0);
    let shader = project.join("shaders/probe.frag");
    fs::write(shader, b"#error broken candidate").unwrap();
    assert!(
        !rig.cli(&["set", asset, "--output", "HEADLESS-2"])
            .status
            .success()
    );
    rig.channel("HEADLESS-2", 0);
    let pids = [rig.worker("HEADLESS-1"), rig.worker("HEADLESS-2")];
    rig.ok(&["release"]);
    rig.spectrum(|s| s["capturing"] == false);
    rig.wait(|| {
        server
            .source_outputs()
            .as_array()
            .unwrap()
            .is_empty()
            .then_some(())
    });
    rig.wait(|| {
        pids.iter()
            .all(|pid| !Path::new(&format!("/proc/{pid}")).exists())
            .then_some(())
    });
    println!(
        "pause, resume, reconnect, script timeout, failed switch and both worker releases passed"
    );
}
