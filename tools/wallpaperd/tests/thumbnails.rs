//! Exercise the public CLI against a private catalog; no live desktop settings are changed.
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::Path,
    process::Command,
    thread,
};

use serde_json::{Value, json};

fn query(root: &Path, assets: &Value, thumbnails: bool) -> Value {
    let runtime = root.join("runtime");
    fs::create_dir_all(runtime.join("misari")).unwrap();
    let socket = runtime.join("misari/wallpaperd.sock");
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    let assets = assets.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["api"], 1);
        assert_eq!(request["method"], "catalog");
        writeln!(stream, "{}", json!({"ok":true,"assets":assets})).unwrap();
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_wallpaperd"));
    command
        .arg("list")
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("LIBGL_ALWAYS_SOFTWARE", "1")
        .env("PATH", "/nonexistent")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY");
    if thumbnails {
        command.arg("--thumbnails");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "thumbnail CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().unwrap();
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["assets"].clone()
}

fn assert_pixel(image: &image::RgbImage, x: u32, y: u32, expected: [u8; 3]) {
    let actual = image[(x, y)].0;
    assert!(
        actual
            .iter()
            .zip(expected)
            .all(|(a, b)| a.abs_diff(b) <= 12),
        "pixel ({x},{y}): {actual:?}, expected {expected:?}"
    );
}

#[test]
#[ignore = "requires libmpv, ffmpeg for fixtures and EGL/GLES 3 (surfaceless)"]
fn list_generates_video_shader_previews_and_reuses_cache() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let video = root.join("视频 ' $(touch unwanted).mkv");
    let output = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=red:s=1280x720:r=4:d=1",
            "-f",
            "lavfi",
            "-i",
            "color=lime:s=1280x720:r=4:d=1",
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1:a=0",
            "-c:v",
            "ffv1",
            // lavfi's color source uses BT.601, which must be tagged for HD media.
            "-colorspace",
            "smpte170m",
            "-y",
        ])
        .arg(&video)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shader = root.join("colors.frag");
    fs::write(&shader, "void mainImage(out vec4 c, in vec2 p) { c=vec4(step(iResolution.y*0.5,p.y),iTime*0.25,iMouse.x/iResolution.x,1.0); }").unwrap();
    let bad = root.join("bad.frag");
    fs::write(&bad, "not a shader").unwrap();
    let broken_video = root.join("broken.mp4");
    fs::write(&broken_video, "not a video").unwrap();
    let asset = |path: &Path, kind: &str| json!({"id":format!("local:{}",path.display()),"path":path,"kind":kind,"thumbnail":null});
    let assets = json!([
        asset(&bad, "shader"),
        asset(&broken_video, "video"),
        asset(&video, "video"),
        asset(&shader, "shader"),
    ]);
    assert_eq!(
        query(root, &assets, false),
        assets,
        "plain list must keep its protocol and avoid generation"
    );
    assert!(!root.join("cache").exists());
    let generated = query(root, &assets, true);
    assert!(generated[0]["thumbnail"].is_null());
    assert!(generated[1]["thumbnail"].is_null());
    let thumbnail = |index: usize| {
        image::open(generated[index]["thumbnail"].as_str().unwrap())
            .unwrap()
            .into_rgb8()
    };
    let image = thumbnail(2);
    assert_eq!(image.dimensions(), (640, 360));
    assert_pixel(&image, 320, 180, [0, 255, 0]); // One second, not the red opening frame.
    let image = thumbnail(3);
    assert_eq!(image.dimensions(), (640, 360));
    assert_pixel(&image, 320, 30, [255, 128, 128]);
    assert_pixel(&image, 320, 330, [0, 128, 128]); // Upright GL readback.
    let path = Path::new(generated[2]["thumbnail"].as_str().unwrap());
    let modified = fs::metadata(path).unwrap().modified().unwrap();
    assert_eq!(
        query(root, &assets, true),
        generated,
        "cached video must retain its generated preview"
    );
    assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
    fs::write(
        &shader,
        "void mainImage(out vec4 c, in vec2 p) { c=vec4(1.0,0.0,0.0,1.0); }",
    )
    .unwrap();
    let updated = query(root, &assets, true);
    assert_ne!(updated[3]["thumbnail"], generated[3]["thumbnail"]);
    assert_pixel(
        &image::open(updated[3]["thumbnail"].as_str().unwrap())
            .unwrap()
            .into_rgb8(),
        320,
        180,
        [255, 0, 0],
    );
    let cache = root.join("cache/misari/wallpaperd/thumbnails");
    assert!(fs::read_dir(cache).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')
    }));
    assert!(!root.join("unwanted").exists());
}
