//! Real compositor regression tests. Each run owns its XDG directories and processes.
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use image::{Rgba, RgbaImage};
use serde_json::Value;

#[allow(dead_code)]
#[path = "support/audio_server.rs"]
mod audio;
use audio::Process as OwnedProcess;
#[path = "support/scene.rs"]
mod scene;
use scene::rust_scene_fixture;

#[path = "nested/compositor.rs"]
mod compositor;
#[path = "nested/playback.rs"]
mod playback;
#[path = "nested/state.rs"]
mod state;
#[path = "nested/web.rs"]
mod web;

struct Rig {
    daemon: Option<Child>,
    compositor: Option<Child>,
    temp: Option<tempfile::TempDir>,
    env: Vec<(OsString, OsString)>,
    niri: OsString,
    screenshot: usize,
}

impl Rig {
    fn binary() -> OsString {
        std::env::var_os("WALLPAPERD_TEST_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_wallpaperd").into())
    }

    fn new() -> Self {
        let niri = std::env::var_os("WALLPAPERD_TEST_NIRI").expect("set WALLPAPERD_TEST_NIRI");
        let parent_runtime = std::env::var_os("XDG_RUNTIME_DIR").expect("Wayland session required");
        let parent_display = PathBuf::from(std::env::var_os("WAYLAND_DISPLAY").unwrap());
        let parent_display = Path::new(&parent_runtime).join(parent_display);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime = root.join("run");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let config = root.join("config");
        fs::create_dir_all(config.join("misari")).unwrap();
        fs::write(
            config.join("misari/wallpaperd.toml"),
            "pause_on_session = false\n",
        )
        .unwrap();
        let niri_config = root.join("niri.kdl");
        fs::write(
            &niri_config,
            "animations { off; }\nhotkey-overlay { skip-at-startup; }\n",
        )
        .unwrap();
        let mut env = vec![
            ("XDG_RUNTIME_DIR".into(), runtime.clone().into_os_string()),
            ("XDG_CONFIG_HOME".into(), config.into_os_string()),
            ("XDG_STATE_HOME".into(), root.join("state").into_os_string()),
            ("XDG_CACHE_HOME".into(), root.join("cache").into_os_string()),
            ("WAYLAND_DISPLAY".into(), parent_display.into_os_string()),
            (
                "PULSE_SERVER".into(),
                "unix:/nonexistent/wallpaperd-test-pulse".into(),
            ),
        ];
        let log = File::create(root.join("niri.log")).unwrap();
        let mut command = Command::new(&niri);
        command
            .args(["--config"])
            .arg(niri_config)
            .envs(env.iter().cloned())
            .env_remove("NIRI_SOCKET");
        let compositor = command
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut rig = Self {
            daemon: None,
            compositor: Some(compositor),
            temp: Some(temp),
            env: Vec::new(),
            niri,
            screenshot: 0,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let (display, socket) = loop {
            let sockets: Vec<_> = fs::read_dir(&runtime)
                .unwrap()
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_socket()))
                .collect();
            let display = sockets
                .iter()
                .find(|e| e.file_name().to_string_lossy().starts_with("wayland-"));
            let socket = sockets
                .iter()
                .find(|e| e.file_name().to_string_lossy().starts_with("niri."));
            if let (Some(display), Some(socket)) = (display, socket) {
                break (display.path(), socket.path());
            }
            assert!(
                rig.compositor
                    .as_mut()
                    .unwrap()
                    .try_wait()
                    .unwrap()
                    .is_none(),
                "nested niri exited"
            );
            assert!(Instant::now() < deadline, "nested niri did not start");
            thread::sleep(Duration::from_millis(20));
        };
        env.retain(|(key, _)| key != "WAYLAND_DISPLAY");
        env.extend([
            ("WAYLAND_DISPLAY".into(), display.into_os_string()),
            ("NIRI_SOCKET".into(), socket.into_os_string()),
        ]);
        rig.env = env;
        rig.start_daemon();
        rig
    }

    fn root(&self) -> &Path {
        self.temp.as_ref().unwrap().path()
    }

    fn start_daemon(&mut self) {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root().join("wallpaperd.log"))
            .unwrap();
        self.daemon = Some(
            Command::new(Self::binary())
                .arg("serve")
                .env("WAYLAND_DEBUG", "client")
                .envs(self.env.iter().cloned())
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = self.cli(&["status"]);
            if status.status.success() {
                let value: Value = serde_json::from_slice(&status.stdout).unwrap();
                if value["state"]["outputs"].get("winit").is_some() {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "wallpaperd did not start");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(Self::binary())
            .args(args)
            .envs(self.env.iter().cloned())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Value {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn set(&self, path: &Path, transition: &str, fps: u32) {
        self.ok(&[
            "set",
            path.to_str().unwrap(),
            "--transition",
            transition,
            "--fit",
            "stretch",
            "--fps",
            &fps.to_string(),
        ]);
    }

    fn shot(&mut self) -> RgbaImage {
        self.screenshot += 1;
        let path = self.root().join(format!("shot-{}.png", self.screenshot));
        let output = Command::new(&self.niri)
            .args([
                "msg",
                "action",
                "screenshot-screen",
                "--show-pointer",
                "false",
                "--path",
            ])
            .arg(&path)
            .envs(self.env.iter().cloned())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(image) = image::open(&path) {
                return image.into_rgba8();
            }
            assert!(Instant::now() < deadline, "screenshot did not finish");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn frames(&self) -> usize {
        fs::read_to_string(self.root().join("wallpaperd.log"))
            .unwrap()
            .lines()
            .filter(|line| line.contains("-> wl_surface") && line.contains(".frame("))
            .count()
    }

    fn niri_action(&self, args: &[&str]) {
        let output = Command::new(&self.niri)
            .args(["msg", "action"])
            .args(args)
            .envs(self.env.iter().cloned())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "niri {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn wait_state(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let snapshot = self.ok(&["status"])["state"].clone();
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "state did not converge: {snapshot}"
            );
            thread::sleep(Duration::from_millis(30));
        }
    }

    fn audio_server(&mut self) -> audio::Server {
        let server = audio::Server::new();
        self.env.retain(|(key, _)| key != "PULSE_SERVER");
        self.env
            .push(("PULSE_SERVER".into(), server.address().into()));
        self.daemon.as_mut().unwrap().kill().unwrap();
        self.daemon.take().unwrap().wait().unwrap();
        self.start_daemon();
        server
    }

    fn wait_corner(&mut self, rgb: [u8; 3]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let shot = self.shot();
            let pixel = shot.get_pixel(10, 10).0;
            if pixel[..3].iter().zip(rgb).all(|(a, b)| a.abs_diff(b) < 8) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "expected corner {rgb:?}, got {pixel:?}"
            );
            thread::sleep(Duration::from_millis(30));
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        for child in [&mut self.daemon, &mut self.compositor]
            .into_iter()
            .flatten()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        if (thread::panicking() || std::env::var_os("WALLPAPERD_TEST_KEEP").is_some())
            && let Some(temp) = self.temp.take()
        {
            eprintln!("wallpaperd test artifacts: {}", temp.keep().display());
        }
    }
}
