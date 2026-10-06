//! The async control client processes the small catalog serially; no daemon job queue.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::{catalog, domain::Fit, pixels, store};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn fill(assets: &mut [Value]) {
    let cache = store::cache_path().with_file_name("thumbnails");
    for asset in assets {
        asset["thumbnail"] = match prepare(asset, &cache) {
            Ok(path) => json!(path),
            Err(error) => {
                eprintln!("wallpaperd thumbnail: {}: {error:#}", asset["id"]);
                Value::Null
            }
        };
    }
}

fn prepare(asset: &Value, cache: &Path) -> Result<Option<PathBuf>> {
    if let Some(preview) = asset["thumbnail"].as_str() {
        return pixels::thumbnail(Path::new(preview), cache).map(Some);
    }
    let kind = match asset["kind"].as_str() {
        Some("video") => catalog::Kind::Video,
        Some("shader") => catalog::Kind::Shader,
        _ => return Ok(None),
    };
    let selection = catalog::resolve(
        asset["id"].as_str().context("asset has no id")?,
        Fit::Stretch,
    )
    .map_err(|error| anyhow::anyhow!(error.message))?;
    let source = catalog::source(&selection).map_err(|error| anyhow::anyhow!(error.message))?;
    // Versions distinguish static image previews, video capture, and fixed shader uniforms.
    let version = if kind == catalog::Kind::Video { 4 } else { 3 };
    pixels::thumbnail_with(&source, cache, version, || {
        fs::create_dir_all(cache)?;
        let frame = cache.join(format!(".frame-{}.png", std::process::id()));
        let _ = fs::remove_file(&frame);
        let result = (|| {
            if kind == catalog::Kind::Video {
                video_frame(&source, &frame)?;
            } else {
                run(
                    Command::new(std::env::current_exe()?)
                        .arg("render-thumbnail")
                        .arg(&source)
                        .arg(&frame),
                    CAPTURE_TIMEOUT,
                )?;
            }
            image::open(&frame)
                .map(image::DynamicImage::into_rgba8)
                .context("reading captured thumbnail")
        })();
        let _ = fs::remove_file(frame);
        result
    })
    .map(Some)
}

fn video_frame(source: &Path, frame: &Path) -> Result<()> {
    let deadline = Instant::now() + CAPTURE_TIMEOUT;
    for position in ["1", "0"] {
        let _ = fs::remove_file(frame);
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("render-thumbnail")
            .arg(source)
            .arg(frame)
            .arg("--position")
            .arg(position);
        let result = run(
            &mut command,
            deadline.saturating_duration_since(Instant::now()),
        );
        if result.is_ok() && frame.is_file() {
            return Ok(());
        }
        if position == "0" || Instant::now() >= deadline {
            result?;
        }
        // Short videos/GIFs may have no frame at one second; retry their first frame.
    }
    anyhow::bail!("video contains no decodable frame")
}

pub fn capture_video(source: &Path, position: Duration) -> Result<image::RgbaImage> {
    let frame = wallpaper_media::extract_frame(source, position)?;
    let image = image::RgbaImage::from_raw(frame.width, frame.height, frame.pixels)
        .context("invalid captured RGBA frame")?;
    pixels::shrink_thumbnail(image)
}

fn run(command: &mut Command, timeout: Duration) -> Result<()> {
    anyhow::ensure!(!timeout.is_zero(), "thumbnail capture timed out");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting thumbnail capture")?;
    let deadline = Instant::now() + timeout;
    let result = (|| loop {
        if let Some(status) = child.try_wait()? {
            anyhow::ensure!(status.success(), "thumbnail capture exited with {status}");
            return Ok(());
        }
        anyhow::ensure!(Instant::now() < deadline, "thumbnail capture timed out");
        thread::sleep(Duration::from_millis(10));
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_timeout_terminates_the_child() {
        let started = Instant::now();
        let error = run(Command::new("sleep").arg("10"), Duration::from_millis(50)).unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
