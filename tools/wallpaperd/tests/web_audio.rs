//! Browser PCM is played on a private Pulse server, independent of the user's session.
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    os::{fd::AsRawFd, unix::net::UnixStream},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
#[allow(dead_code)]
#[path = "support/audio_server.rs"]
mod audio_server;

use audio_server::Process;

fn main() {
    if let Some(code) = we_web::execute_process() {
        std::process::exit(code);
    }
    if !std::env::args().any(|arg| arg == "--ignored") {
        println!("Web audio check skipped; pass --ignored to run it");
        return;
    }
    web_audio_volume_mute_pause_own_stream_and_release();
}
fn web_audio_volume_mute_pause_own_stream_and_release() {
    let server = audio_server::Server::new();
    let root = server.root.path();
    let project = root.join("project");
    fs::create_dir(&project).unwrap();
    let entry = project.join("index.html");
    fs::write(
        &entry,
        r#"<!doctype html><body style="background:red"><script>
    const audio = new AudioContext();
    const oscillator = audio.createOscillator(), gain = audio.createGain();
    gain.gain.value = 0.2;
    oscillator.connect(gain).connect(audio.destination);
    oscillator.start(); audio.resume();
    </script>"#,
    )
    .unwrap();
    // This runner has not created any application threads yet. Its Chromium
    // children reuse this same dispatch entry and inherit the private Pulse server.
    unsafe {
        std::env::set_var("PULSE_SERVER", server.address());
        std::env::set_var("PULSE_PROP", "application.id=misari.wallpaperd");
        std::env::set_var(
            "WALLPAPERD_WEB_SUBPROCESS",
            std::env::current_exe().unwrap(),
        );
    }
    let _runtime = we_web::ShutdownGuard;
    let (wake, _reader) = UnixStream::pair().unwrap();
    let browser =
        we_web::Renderer::load(&project, &entry, [64, 64], false, "headless", &wake).unwrap();
    let send = |browser: &we_web::Renderer, volume: u32, mute: bool, paused: bool| {
        browser
            .send(
                json!({"size":{"width":64,"height":64},"properties":{},
            "playback":{"paused":paused,"mute":mute,"fps":30,"volume":volume}}),
                false,
            )
            .unwrap();
    };
    send(&browser, 100, false, false);
    let mut monitor = Process(
        Command::new("parec")
            .args([
                "--raw",
                "--format=float32le",
                "--rate=48000",
                "--channels=2",
                "--latency-msec=20",
                "--device=wallpaperd_test_a.monitor",
            ])
            .env("PULSE_SERVER", server.address())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut output = monitor.0.stdout.take().unwrap();
    // SAFETY: fcntl changes flags on this live child pipe and touches no Rust memory.
    let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
    assert!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0
    );
    let rms = |output: &mut std::process::ChildStdout| {
        let deadline = Instant::now() + Duration::from_millis(600);
        let settled = Instant::now() + Duration::from_millis(300);
        let mut sum = 0.0f64;
        let mut count = 0;
        while Instant::now() < deadline {
            let mut bytes = [0; 16384];
            match output.read(&mut bytes) {
                Ok(n) => {
                    if Instant::now() >= settled {
                        for sample in bytes[..n].as_chunks::<4>().0 {
                            let value = f32::from_le_bytes(*sample) as f64;
                            sum += value * value;
                            count += 1;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                value => panic!("monitor stopped: {value:?}"),
            }
            thread::sleep(Duration::from_millis(5));
        }
        (count > 1000).then(|| (sum / count as f64).sqrt())
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let full = loop {
        let level = rms(&mut output).unwrap_or(0.0);
        if level > 0.05 {
            break level;
        }
        assert!(
            Instant::now() < deadline,
            "Web Audio did not play: {}",
            browser.poll().error.unwrap_or_default()
        );
    };
    let streams: Value =
        serde_json::from_str(&server.pactl(&["--format=json", "list", "sink-inputs"])).unwrap();
    assert_eq!(
        streams
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["corked"] == false)
            .count(),
        1,
        "CEF must not play a duplicate audio stream: {streams}"
    );
    for stream in streams.as_array().unwrap() {
        assert_eq!(stream["properties"]["application.id"], "misari.wallpaperd");
    }
    send(&browser, 25, false, false);
    let quarter = rms(&mut output).expect("monitor produced no samples");
    assert!(
        (quarter / full - 0.25).abs() < 0.08,
        "volume scaling: {quarter} / {full}"
    );
    send(&browser, 100, true, false);
    assert!(
        rms(&mut output).expect("mute monitor stopped") < 0.001,
        "mute left audible PCM"
    );
    send(&browser, 100, false, true);
    assert!(
        rms(&mut output).expect("pause monitor stopped") < 0.001,
        "pause left audible PCM"
    );
    send(&browser, 100, false, false);
    assert!(
        rms(&mut output).expect("resume monitor stopped") > full * 0.8,
        "resume did not restore audio"
    );
    drop(browser);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let streams: Value =
            serde_json::from_str(&server.pactl(&["--format=json", "list", "sink-inputs"])).unwrap();
        if streams.as_array().unwrap().is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "release retained an audio stream: {streams}"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let streams: Value =
        serde_json::from_str(&server.pactl(&["--format=json", "list", "sink-inputs"])).unwrap();
    assert!(
        streams.as_array().unwrap().is_empty(),
        "release retained an audio stream"
    );
}
