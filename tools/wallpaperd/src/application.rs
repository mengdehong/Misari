//! Opt-in executable compatibility: one private display and process group per worker.
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use parking_lot::Mutex;
use serde_json::json;

use crate::{
    domain::Playback,
    graphics::{Gpu, Target},
    pixels::Size,
};

fn executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

pub fn capability() -> serde_json::Value {
    let missing: Vec<_> = [
        "gamescope",
        "Xwayland",
        "wine",
        "wineserver",
        "ffmpeg",
        "xwininfo",
    ]
    .into_iter()
    .filter(|name| executable(name).is_none())
    .collect();
    let enabled = crate::store::Store::load()
        .map(|store| store.policy.allow_applications.unwrap_or(false))
        .unwrap_or(false);
    let reason = if !missing.is_empty() {
        format!("缺少应用兼容组件：{}", missing.join("、"))
    } else if !enabled {
        "应用壁纸会运行 Windows 程序；请在设置中启用应用兼容后端。".into()
    } else {
        "通过独立 Wine 与无界面显示器播放；应用音频、交互和属性暂不支持。".into()
    };
    json!({"available":enabled && missing.is_empty(),"enabled":enabled,"installed":missing.is_empty(),"reason":reason,
        "content":["we_application"],"pause":true,"stop":true,"audio":false,"properties":false,"input":false,
        "synchronized_playback":false,"max_capture_fps":30,"max_capture_size":[1920,1080]})
}

struct Process(Child);
impl Process {
    fn spawn(command: &mut Command, log: &File) -> Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?);
        // A private session makes every signal below local to this instance.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(Self(command.spawn()?))
    }
    fn signal(&self, signal: i32) -> Result<()> {
        if unsafe { libc::kill(-(self.0.id() as i32), signal) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        Ok(())
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.signal(libc::SIGCONT);
        let _ = self.signal(libc::SIGKILL);
        let _ = self.0.wait();
    }
}

struct WinePrefix(PathBuf);
impl WinePrefix {
    fn signal(&self, signal: i32) {
        // Wine can detach helpers. Match this unique private prefix, never a shared server.
        use std::os::unix::ffi::OsStrExt;
        let mut marker = b"WINEPREFIX=".to_vec();
        marker.extend_from_slice(self.0.as_os_str().as_bytes());
        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<i32>().ok())
                else {
                    continue;
                };
                if let Ok(environment) = fs::read(entry.path().join("environ"))
                    && environment
                        .split(|byte| *byte == 0)
                        .any(|value| value == marker)
                {
                    unsafe {
                        libc::kill(pid, signal);
                    }
                }
            }
        }
    }
}
impl Drop for WinePrefix {
    fn drop(&mut self) {
        self.signal(libc::SIGCONT);
        self.signal(libc::SIGKILL);
    }
}

#[derive(Default)]
struct Frame {
    pixels: Option<Vec<u8>>,
    error: Option<String>,
}

pub struct Application {
    capture: Process,
    program: Process,
    display: Process,
    frames: Arc<Mutex<Frame>>,
    reader: Option<thread::JoinHandle<()>>,
    directory: PathBuf,
    prefix: Option<WinePrefix>,
    size: Size,
    texture: Option<Target>,
    fit: crate::domain::Fit,
    paused: bool,
    ready: bool,
}

impl Application {
    pub fn load(
        path: &Path,
        gpu: &Gpu,
        playback: Playback,
        wake: &UnixStream,
        fit: crate::domain::Fit,
    ) -> Result<Self> {
        let capability = capability();
        ensure!(
            capability["available"] == true,
            "{}",
            capability["reason"]
                .as_str()
                .unwrap_or("应用兼容后端不可用")
        );
        Self::launch(path, gpu.size, playback, wake, fit, false)
    }

    fn launch(
        path: &Path,
        size: Size,
        playback: Playback,
        wake: &UnixStream,
        fit: crate::domain::Fit,
        native: bool,
    ) -> Result<Self> {
        let directory = crate::store::socket_path()
            .parent()
            .context("runtime directory missing")?
            .join(format!("application-{}", std::process::id()));
        // A worker can prepare a replacement while its old application is still visible.
        let directory = directory.with_extension(format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir_all(&directory)?;
        let result = Self::start(path, size, playback, wake, fit, native, &directory);
        if let Err(error) = result {
            let logs = crate::store::cache_path()
                .parent()
                .unwrap()
                .join("application-logs");
            let _ = fs::create_dir_all(&logs);
            let diagnostic = logs.join(format!("worker-{}.log", std::process::id()));
            let copied = File::open(directory.join("runtime.log"))
                .and_then(|file| {
                    let mut bytes = Vec::new();
                    file.take(64 * 1024).read_to_end(&mut bytes)?;
                    fs::write(&diagnostic, bytes)
                })
                .is_ok();
            let _ = fs::remove_dir_all(&directory);
            return Err(if copied {
                error.context(format!("应用诊断日志：{}", diagnostic.display()))
            } else {
                error
            });
        }
        result
    }

    fn start(
        path: &Path,
        size: Size,
        playback: Playback,
        wake: &UnixStream,
        fit: crate::domain::Fit,
        native: bool,
        directory: &Path,
    ) -> Result<Self> {
        let log = File::create(directory.join("runtime.log"))?;
        let width = size.width.clamp(16, 1920);
        let height = size.height.clamp(16, 1080);
        let display_file = directory.join("display");
        let mut display_command = Command::new("gamescope");
        display_command
            .args([
                "--backend",
                "headless",
                "-W",
                &width.to_string(),
                "-H",
                &height.to_string(),
                "-w",
                &width.to_string(),
                "-h",
                &height.to_string(),
                "--xwayland-count",
                "1",
                "--",
                "/bin/sh",
                "-c",
                "printf '%s' \"$DISPLAY\" > \"$1\"; exec sleep infinity",
                "misari-display",
            ])
            .arg(&display_file)
            .env_remove("WAYLAND_DEBUG");
        let mut display =
            Process::spawn(&mut display_command, &log).context("启动独立应用显示器失败")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let display_name = loop {
            if let Ok(name) = fs::read_to_string(&display_file)
                && name.starts_with(':')
                && name.trim().len() > 1
            {
                break name.trim().to_owned();
            }
            ensure!(
                display.0.try_wait()?.is_none(),
                "独立应用显示器启动失败；检查 gamescope 与显卡驱动"
            );
            ensure!(Instant::now() < deadline, "独立应用显示器启动超时");
            thread::sleep(Duration::from_millis(50));
        };
        let prefix = (!native).then(|| WinePrefix(directory.join("wine")));
        let mut command = if native {
            Command::new(path)
        } else {
            let mut command = Command::new("wine");
            command.arg(path);
            command
        };
        command
            .current_dir(path.parent().context("application directory missing")?)
            .env("DISPLAY", &display_name)
            .env_remove("WAYLAND_DISPLAY")
            .env("WINEDEBUG", "-all")
            .env(
                "WINEDLLOVERRIDES",
                "winemenubuilder.exe=d;winepulse.drv=d;winealsa.drv=d",
            )
            .env(
                "PULSE_SERVER",
                "unix:/nonexistent/misari-application-audio-disabled",
            );
        if let Some(prefix) = &prefix {
            command.env("WINEPREFIX", &prefix.0);
        }
        let mut program = Process::spawn(&mut command, &log).context("启动应用失败")?;
        let deadline = Instant::now() + Duration::from_secs(35);
        let (window, window_size) = loop {
            ensure!(display.0.try_wait()?.is_none(), "应用显示器意外退出");
            // Wine may hand off to another process in the same prefix; wait for its window.
            let exited = program.0.try_wait()?;
            if let Some(window) = find_window(&display_name)? {
                break window;
            }
            if let Some(status) = exited {
                ensure!(status.success(), "应用启动失败：{status}");
            }
            ensure!(
                Instant::now() < deadline,
                "应用在 35 秒内没有创建可显示的窗口"
            );
            thread::sleep(Duration::from_millis(100));
        };
        let factor = (1920.0 / window_size.width as f64)
            .min(1080.0 / window_size.height as f64)
            .min(1.0);
        let size = Size::new(
            (window_size.width as f64 * factor).max(1.0) as u32,
            (window_size.height as f64 * factor).max(1.0) as u32,
        )?;
        let mut capture_command = Command::new("ffmpeg");
        capture_command.args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "x11grab",
            "-draw_mouse",
            "0",
            "-framerate",
            "30",
            "-window_id",
            &window.to_string(),
            "-i",
            &display_name,
            "-vf",
            &format!("scale={}:{}", size.width, size.height),
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "pipe:1",
        ]);
        capture_command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log.try_clone()?);
        unsafe {
            capture_command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut capture = Process(capture_command.spawn().context("启动应用画面捕获失败")?);
        let mut stdout = capture
            .0
            .stdout
            .take()
            .context("capture stdout unavailable")?;
        let frames = Arc::new(Mutex::new(Frame::default()));
        let shared = frames.clone();
        let mut wake = wake.try_clone()?;
        let reader = thread::Builder::new()
            .name("application-capture".into())
            .spawn(move || {
                loop {
                    let mut pixels = vec![0; size.bytes()];
                    if let Err(error) = stdout.read_exact(&mut pixels) {
                        shared.lock().error = Some(format!("应用画面捕获结束：{error}"));
                        let _ = wake.write_all(&[1]);
                        break;
                    }
                    shared.lock().pixels = Some(pixels);
                    let _ = wake.write_all(&[1]);
                }
            })?;
        let mut app = Self {
            capture,
            program,
            display,
            frames,
            reader: Some(reader),
            directory: directory.to_path_buf(),
            prefix,
            size,
            texture: None,
            fit,
            paused: false,
            ready: false,
        };
        // Keep capture running until a real frame exists, even when loading paused.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.poll()?;
            if app.frames.lock().pixels.is_some() {
                break;
            }
            ensure!(Instant::now() < deadline, "应用画面捕获超时");
            thread::sleep(Duration::from_millis(25));
        }
        app.playback(playback)?;
        Ok(app)
    }

    pub fn poll(&mut self) -> Result<()> {
        if let Some(error) = &self.frames.lock().error {
            bail!("{error}");
        }
        ensure!(self.display.0.try_wait()?.is_none(), "应用显示器已退出");
        ensure!(self.capture.0.try_wait()?.is_none(), "应用画面捕获已退出");
        Ok(())
    }
    pub fn playback(&mut self, playback: Playback) -> Result<()> {
        if self.paused != playback.paused {
            let signal = if playback.paused {
                libc::SIGSTOP
            } else {
                libc::SIGCONT
            };
            self.program.signal(signal)?;
            if let Some(prefix) = &self.prefix {
                prefix.signal(signal);
            }
            self.capture.signal(signal)?;
            self.paused = playback.paused;
        }
        Ok(())
    }
    pub fn needs_render(&self) -> bool {
        self.frames.lock().pixels.is_some()
    }
    pub fn draw(
        &mut self,
        gpu: &Gpu,
        target: Option<glow::NativeFramebuffer>,
        force: bool,
    ) -> Result<bool> {
        let pixels = self.frames.lock().pixels.take();
        let changed = pixels.is_some();
        if let Some(pixels) = pixels {
            if self.texture.is_none() {
                self.texture = Some(Target::new(gpu.gl.clone(), self.size)?);
            }
            self.texture.as_ref().unwrap().upload(&pixels);
            self.ready = true;
        }
        if let Some(texture) = &self.texture
            && (changed || force)
        {
            gpu.draw_raster(texture, target, self.fit, false);
            return Ok(true);
        }
        Ok(false)
    }
    pub fn ready(&self) -> bool {
        self.ready
    }
}

impl Drop for Application {
    fn drop(&mut self) {
        let _ = self.capture.signal(libc::SIGCONT);
        let _ = self.capture.signal(libc::SIGKILL);
        let _ = self.capture.0.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let _ = self.program.signal(libc::SIGCONT);
        let _ = self.program.signal(libc::SIGKILL);
        drop(self.prefix.take());
        let _ = self.display.signal(libc::SIGKILL);
        let _ = self.display.0.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn find_window(display: &str) -> Result<Option<(u32, Size)>> {
    let output = Command::new("xwininfo")
        .args(["-display", display, "-root", "-tree"])
        .env("LC_ALL", "C")
        .output()?;
    let tree = String::from_utf8_lossy(&output.stdout);
    let mut candidates = Vec::new();
    for line in tree.lines().filter(|line| {
        let line = line.to_ascii_lowercase();
        ![
            "steamcompmgr",
            "wine system tray",
            "wine configuration",
            "wineboot",
            "explorer.exe",
        ]
        .iter()
        .any(|name| line.contains(name))
    }) {
        let mut fields = line.split_whitespace();
        let Some(id) = fields
            .next()
            .and_then(|id| id.strip_prefix("0x"))
            .and_then(|id| u32::from_str_radix(id, 16).ok())
        else {
            continue;
        };
        for field in fields {
            let geometry = field.split(['+', '-']).next().unwrap_or("");
            let Some((width, height)) = geometry.split_once('x') else {
                continue;
            };
            if let (Ok(width), Ok(height)) = (width.parse::<u32>(), height.parse::<u32>())
                && width >= 16
                && height >= 16
                && width <= 8192
                && height <= 8192
            {
                candidates.push((id, Size::new(width, height)?));
            }
        }
    }
    candidates.sort_by_key(|(_, size)| std::cmp::Reverse(size.width as u64 * size.height as u64));
    for (id, size) in candidates {
        let output = Command::new("xwininfo")
            .args(["-display", display, "-id", &id.to_string()])
            .env("LC_ALL", "C")
            .output()?;
        if String::from_utf8_lossy(&output.stdout).contains("Map State: IsViewable") {
            return Ok(Some((id, size)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_prefix_signals_do_not_touch_other_processes() {
        let root = tempfile::tempdir().unwrap();
        let prefix = WinePrefix(root.path().join("private"));
        let log = File::create(root.path().join("log")).unwrap();
        let owned = Process::spawn(
            Command::new("sleep").arg("60").env("WINEPREFIX", &prefix.0),
            &log,
        )
        .unwrap();
        let other = Process::spawn(Command::new("sleep").arg("60"), &log).unwrap();
        prefix.signal(libc::SIGSTOP);
        let state = |process: &Process| {
            fs::read_to_string(format!("/proc/{}/status", process.0.id()))
                .unwrap()
                .lines()
                .find(|line| line.starts_with("State:"))
                .unwrap()
                .to_owned()
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        while !state(&owned).contains('T') && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(state(&owned).contains('T'));
        assert!(!state(&other).contains('T'));
        prefix.signal(libc::SIGCONT);
    }

    #[test]
    #[ignore = "requires gamescope, Xwayland, ffmpeg, xwininfo, cc and Vulkan; runs only a generated native X11 window"]
    fn synthetic_window_capture_pause_resume_failure_and_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("synthetic.c");
        let binary = root.path().join("synthetic");
        fs::write(
            &source,
            r#"
#include <X11/Xlib.h>
#include <unistd.h>
int main() {
    Display *d = XOpenDisplay(0); if (!d) return 2;
    Window w = XCreateSimpleWindow(d, DefaultRootWindow(d), 0, 0, 320, 240, 0, 0, 0x12ab34);
    XStoreName(d, w, "Misari synthetic test"); XMapWindow(d, w); XFlush(d);
    for (;;) { XClearWindow(d, w); XFlush(d); usleep(10000); }
}
"#,
        )
        .unwrap();
        assert!(
            Command::new("cc")
                .arg(&source)
                .args(["-lX11", "-o"])
                .arg(&binary)
                .status()
                .unwrap()
                .success()
        );
        let (wake, _reader) = UnixStream::pair().unwrap();
        wake.set_nonblocking(true).unwrap();
        let mut app = Application::launch(
            &binary,
            Size::new(320, 240).unwrap(),
            Playback::default(),
            &wake,
            crate::domain::Fit::Cover,
            true,
        )
        .unwrap();
        let pixels = app.frames.lock().pixels.take().unwrap();
        assert_eq!(&pixels[..4], &[0x12, 0xab, 0x34, 255]);
        app.frames.lock().pixels = Some(pixels);
        let gpu = Gpu::headless(Size::new(64, 64).unwrap()).unwrap();
        let target = Target::new(gpu.gl.clone(), gpu.size).unwrap();
        assert!(app.draw(&gpu, Some(target.fbo), false).unwrap());
        assert!(app.ready());
        assert_eq!(
            gpu.read_image(&target).unwrap().get_pixel(32, 32).0,
            [0x12, 0xab, 0x34, 255]
        );
        let pids = [app.capture.0.id(), app.program.0.id(), app.display.0.id()];
        let directory = app.directory.clone();
        app.playback(Playback {
            paused: true,
            ..Default::default()
        })
        .unwrap();
        for pid in &pids[..2] {
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
                if status
                    .lines()
                    .any(|line| line.starts_with("State:") && line.contains('T'))
                {
                    break;
                }
                assert!(Instant::now() < deadline, "application did not pause");
                thread::sleep(Duration::from_millis(10));
            }
        }
        app.frames.lock().pixels = None;
        app.playback(Playback::default()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.frames.lock().pixels.is_none() {
            app.poll().unwrap();
            assert!(Instant::now() < deadline, "capture did not resume");
            thread::sleep(Duration::from_millis(20));
        }
        app.program.signal(libc::SIGKILL).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.poll().is_ok() {
            assert!(
                Instant::now() < deadline,
                "closed window did not produce a failure"
            );
            thread::sleep(Duration::from_millis(20));
        }
        drop(app);
        for pid in pids {
            assert!(
                !PathBuf::from(format!("/proc/{pid}")).exists(),
                "owned process survived release"
            );
        }
        assert!(!directory.exists());
    }

    #[test]
    #[ignore = "requires Wine development tools, gamescope and Vulkan; compiles a local Win32 test window, never runs workshop content"]
    fn synthetic_win32_window_uses_private_wine_and_releases_it() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("synthetic.c");
        let binary = root.path().join("synthetic.exe");
        fs::write(&source, r#"
#include <windows.h>
static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM w, LPARAM l) {
    return DefWindowProcA(window, message, w, l);
}
int WINAPI WinMain(HINSTANCE instance, HINSTANCE previous, LPSTR command, int show) {
    WNDCLASSA klass = {0}; klass.lpfnWndProc = procedure; klass.hInstance = instance;
    klass.lpszClassName = "MisariSynthetic"; klass.hbrBackground = CreateSolidBrush(RGB(18,171,52));
    if (!RegisterClassA(&klass)) return 2;
    HWND window = CreateWindowA(klass.lpszClassName, "Misari synthetic Win32", WS_POPUP | WS_VISIBLE,
        0, 0, 320, 240, 0, 0, instance, 0);
    if (!window) return 3;
    MSG message; while (GetMessageA(&message, 0, 0, 0) > 0) { TranslateMessage(&message); DispatchMessageA(&message); }
    return 0;
}
"#).unwrap();
        assert!(
            Command::new("winegcc")
                .arg("-mwindows")
                .arg(&source)
                .args(["-lgdi32", "-o"])
                .arg(&binary)
                .status()
                .unwrap()
                .success()
        );
        let (wake, _reader) = UnixStream::pair().unwrap();
        wake.set_nonblocking(true).unwrap();
        // winegcc emits a local Winelib module using the real Win32 window APIs.
        let module = binary.with_extension("exe.so");
        let mut app = Application::launch(
            &module,
            Size::new(320, 240).unwrap(),
            Playback::default(),
            &wake,
            crate::domain::Fit::Cover,
            false,
        )
        .unwrap();
        let prefix = app.prefix.as_ref().unwrap().0.clone();
        assert!(prefix.starts_with(&app.directory));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.poll().unwrap();
            if app
                .frames
                .lock()
                .pixels
                .as_ref()
                .is_some_and(|pixels| pixels.as_chunks::<4>().0.contains(&[18, 171, 52, 255]))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Wine window produced no expected pixels"
            );
            thread::sleep(Duration::from_millis(50));
        }
        app.playback(Playback {
            paused: true,
            ..Default::default()
        })
        .unwrap();
        app.playback(Playback::default()).unwrap();
        let pids = [app.capture.0.id(), app.program.0.id(), app.display.0.id()];
        drop(app);
        assert!(!prefix.exists());
        for pid in pids {
            assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
        }
    }
}
