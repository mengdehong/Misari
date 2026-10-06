//! Private PipeWire/Pulse fixture shared by daemon and playback checks.
use std::{
    fs::File,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(crate) struct Process(pub(crate) Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
pub(crate) struct Server {
    pub(crate) root: tempfile::TempDir,
    env: Vec<(String, String)>,
    processes: Vec<Process>,
    pulse: Option<Process>,
}
impl Server {
    pub(crate) fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().to_str().unwrap().to_owned();
        let env = vec![
            ("XDG_RUNTIME_DIR".into(), runtime.clone()),
            ("PIPEWIRE_RUNTIME_DIR".into(), runtime.clone()),
            (
                "PULSE_SERVER".into(),
                format!("unix:{runtime}/pulse/native"),
            ),
        ];
        let mut server = Self {
            root,
            env,
            processes: vec![],
            pulse: None,
        };
        server.processes.push(server.spawn("pipewire", &[]));
        server
            .processes
            .push(server.spawn("wireplumber", &["--profile", "policy"]));
        server.restart();
        server
    }
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.envs(self.env.iter().map(|(k, v)| (k, v)));
        command
    }
    fn spawn(&self, program: &str, args: &[&str]) -> Process {
        let log = File::options()
            .create(true)
            .append(true)
            .open(self.root.path().join("audio.log"))
            .unwrap();
        Process(
            self.command(program)
                .args(args)
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        )
    }
    pub(crate) fn pactl(&self, args: &[&str]) -> String {
        let output = self.command("pactl").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "pactl {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    pub(crate) fn restart(&mut self) {
        self.pulse = Some(self.spawn("pipewire-pulse", &[]));
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if self
                .command("pactl")
                .arg("info")
                .output()
                .unwrap()
                .status
                .success()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "private pulse server startup timed out"
            );
            thread::sleep(Duration::from_millis(30));
        }
        self.pactl(&[
            "load-module",
            "module-null-sink",
            "sink_name=wallpaperd_test_a",
            "channels=2",
        ]);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if self
                .command("pactl")
                .args(["set-default-sink", "wallpaperd_test_a"])
                .output()
                .unwrap()
                .status
                .success()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "private audio policy startup timed out"
            );
            thread::sleep(Duration::from_millis(30));
        }
    }
    pub(crate) fn source_outputs(&self) -> serde_json::Value {
        serde_json::from_str(&self.pactl(&["--format=json", "list", "source-outputs"])).unwrap()
    }
    pub(crate) fn address(&self) -> String {
        self.env[2].1.clone()
    }
    pub(crate) fn disconnect(&mut self) {
        self.pulse.take();
    }
    pub(crate) fn play(&self, file: &Path) -> Process {
        Process(
            self.command("paplay")
                .args([
                    "--raw",
                    "--format=float32le",
                    "--rate=48000",
                    "--channels=2",
                    "--device=wallpaperd_test_a",
                ])
                .arg(file)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.pulse.take();
        while self.processes.pop().is_some() {}
    }
}
