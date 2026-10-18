use serde_json::{Value, json};
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock, mpsc},
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

const REQUEST_LIMIT: usize = 64 * 1024;
const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(8);
static START: Mutex<()> = Mutex::new(());
static ACTIVATIONS: OnceLock<Mutex<mpsc::Receiver<String>>> = OnceLock::new();
const ACTIVATION_LIMIT: usize = 128;

fn activation_name(page: Option<String>) -> Result<String, String> {
    let page = page.unwrap_or_else(|| "open".into());
    match page.as_str() {
        "open" | "library" | "workshop" | "discover" | "downloads" | "playlists" | "displays"
        | "settings" => Ok(page),
        _ => Err("无法打开未知的应用页面。".into()),
    }
}

fn read_activation(reader: impl Read) -> Result<String, String> {
    let mut bytes = Vec::new();
    BufReader::new(reader.take(ACTIVATION_LIMIT as u64 + 1))
        .read_until(b'\n', &mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > ACTIVATION_LIMIT || bytes.last() != Some(&b'\n') {
        return Err("应用激活请求不完整或过长。".into());
    }
    let page: String = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    activation_name(Some(page))
}

/// Hold the returned file until process exit. Only its exclusive owner may
/// replace a stale activation socket or run a GUI download queue worker.
pub fn single_instance(page: Option<String>) -> Result<Option<fs::File>, String> {
    let page = activation_name(page)?;
    let directory = socket_path()
        .parent()
        .ok_or("无法定位应用运行目录")?
        .to_owned();
    fs::create_dir_all(&directory).map_err(|error| format!("无法创建应用运行目录：{error}"))?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join("wallpaper-ui.lock"))
        .map_err(|error| format!("无法打开应用锁：{error}"))?;
    lock.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    let socket = directory.join("wallpaper-ui.sock");
    match lock.try_lock() {
        Ok(()) => {
            if socket.exists() {
                fs::remove_file(&socket)
                    .map_err(|error| format!("无法清理旧的应用连接：{error}"))?;
            }
            let listener = UnixListener::bind(&socket)
                .map_err(|error| format!("无法创建应用连接：{error}"))?;
            fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
            let (tx, rx) = mpsc::sync_channel(32);
            ACTIVATIONS
                .set(Mutex::new(rx))
                .map_err(|_| "应用已在此进程初始化")?;
            thread::spawn(move || {
                for mut stream in listener.incoming().flatten() {
                    if stream
                        .set_read_timeout(Some(Duration::from_millis(500)))
                        .is_err()
                    {
                        continue;
                    }
                    if stream
                        .set_write_timeout(Some(Duration::from_millis(500)))
                        .is_err()
                    {
                        continue;
                    }
                    if let Ok(page) = read_activation(&stream)
                        && tx.try_send(page).is_ok()
                    {
                        let _ = stream.write_all(b"ok\n");
                    }
                }
            });
            Ok(Some(lock))
        }
        Err(fs::TryLockError::WouldBlock) => {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut stream = loop {
                match UnixStream::connect(&socket) {
                    Ok(stream) => break stream,
                    Err(error)
                        if Instant::now() < deadline
                            && matches!(
                                error.kind(),
                                std::io::ErrorKind::NotFound
                                    | std::io::ErrorKind::ConnectionRefused
                            ) =>
                    {
                        thread::sleep(Duration::from_millis(20))
                    }
                    Err(error) => return Err(format!("Misari 已经运行，但无法激活窗口：{error}")),
                }
            };
            stream
                .set_write_timeout(Some(Duration::from_millis(500)))
                .map_err(|error| error.to_string())?;
            stream
                .set_read_timeout(Some(Duration::from_millis(750)))
                .map_err(|error| error.to_string())?;
            let mut frame = serde_json::to_vec(&page).map_err(|error| error.to_string())?;
            frame.push(b'\n');
            stream
                .write_all(&frame)
                .map_err(|error| format!("无法激活现有窗口：{error}"))?;
            let mut ack = [0; 3];
            stream
                .read_exact(&mut ack)
                .map_err(|error| format!("现有窗口没有响应：{error}"))?;
            if ack != *b"ok\n" {
                return Err("现有窗口未接受激活请求。".into());
            }
            focus_window();
            Ok(None)
        }
        Err(fs::TryLockError::Error(error)) => Err(format!("无法锁定应用运行状态：{error}")),
    }
}

pub fn activation() -> Option<String> {
    ACTIVATIONS.get()?.lock().ok()?.try_recv().ok()
}

fn focus_window() {
    let Ok(bytes) = capture(
        Command::new("niri").args(["msg", "--json", "windows"]),
        Duration::from_secs(2),
    ) else {
        return;
    };
    let Ok(windows) = serde_json::from_slice::<Vec<Value>>(&bytes) else {
        return;
    };
    let Some(id) = windows
        .iter()
        .find(|window| window["app_id"] == "io.misari.wallpaper")
        .and_then(|window| window["id"].as_u64())
    else {
        return;
    };
    let _ = capture(
        Command::new("niri").args(["msg", "action", "focus-window", "--id", &id.to_string()]),
        Duration::from_secs(2),
    );
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/run"))
        .join("misari/wallpaperd.sock")
}

fn daemon() -> Option<PathBuf> {
    let sibling = std::env::current_exe().ok()?.with_file_name("wallpaperd");
    [sibling, home().join(".local/bin/wallpaperd")]
        .into_iter()
        .find(|path| path.is_file())
}

fn connect(start: bool) -> Result<UnixStream, String> {
    let path = socket_path();
    match UnixStream::connect(&path) {
        Ok(stream) => return Ok(stream),
        Err(error)
            if start
                && matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) => {}
        Err(error) => return Err(format!("无法连接壁纸服务：{error}")),
    }
    let _guard = START.lock().map_err(|_| "壁纸服务启动状态异常")?;
    if let Ok(stream) = UnixStream::connect(&path) {
        return Ok(stream);
    }
    let executable =
        daemon().ok_or("未找到壁纸服务。请重新安装 Misari，或将 wallpaperd 放在应用旁。")?;
    let mut child = Command::new(executable)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("无法启动壁纸服务：{error}"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(stream) = UnixStream::connect(&path) {
            thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(stream);
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "壁纸服务启动失败（{status}）。请在诊断中检查 Wayland 会话与播放组件。"
            ));
        }
        if Instant::now() >= deadline {
            thread::spawn(move || {
                let _ = child.wait();
            });
            return Err("壁纸服务启动超时。请稍后重试，或打开诊断查看会话状态。".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn frame(method: &str, params: Value) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(&json!({"api":1,"method":method,"params":params}))
        .map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if bytes.len() > REQUEST_LIMIT {
        return Err("操作数据超过 64 KiB，请缩小属性或播放列表后重试。".into());
    }
    Ok(bytes)
}

fn read_response(reader: impl Read) -> Result<Value, String> {
    let mut bytes = Vec::new();
    let mut reader = BufReader::new(reader.take(RESPONSE_LIMIT as u64 + 1));
    reader
        .read_until(b'\n', &mut bytes)
        .map_err(|error| format!("壁纸服务响应失败：{error}"))?;
    if bytes.len() > RESPONSE_LIMIT {
        return Err("壁纸服务响应超过 16 MiB。".into());
    }
    if bytes.last() != Some(&b'\n') {
        return Err("壁纸服务连接中断，未收到完整响应。".into());
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("壁纸服务返回无效数据：{error}"))?;
    if value["ok"] == false {
        return Err(value["error"]["message"]
            .as_str()
            .unwrap_or("壁纸服务未能完成操作")
            .into());
    }
    Ok(value)
}

/// Requests are bounded and must run outside the UI thread.
pub fn request(method: &str, params: Value) -> Result<Value, String> {
    exchange(method, params, true)
}

fn exchange(method: &str, params: Value, start: bool) -> Result<Value, String> {
    let bytes = frame(method, params)?;
    let mut stream = connect(start)?;
    stream
        .set_read_timeout(Some(if method == "apply" {
            Duration::from_secs(70)
        } else {
            TIMEOUT
        }))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .write_all(&bytes)
        .map_err(|error| format!("无法发送壁纸操作：{error}"))?;
    read_response(stream)
}

pub fn load() -> Result<Value, String> {
    let state = request("get", json!({}))?;
    let catalog = request("catalog", json!({}))?;
    let library = request("get_library", json!({}))?;
    let displays = capture(
        Command::new("niri").args(["msg", "--json", "outputs"]),
        Duration::from_secs(2),
    )
    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(|error| error.to_string()))
    .unwrap_or_else(|_| json!({}));
    Ok(
        json!({"state":state["state"],"catalog":catalog["assets"],"library":library["library"],"displays":displays}),
    )
}

pub fn open(target: &str) -> Result<(), String> {
    if target.is_empty() || target.chars().any(char::is_control) {
        return Err("无法打开空白或包含控制字符的位置。".into());
    }
    // Never pass user text to a shell, and never let it become an option.
    let argument = if target.starts_with('-') {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(target)
            .to_string_lossy()
            .into_owned()
    } else {
        target.to_owned()
    };
    let mut child = Command::new("xdg-open")
        .arg(argument)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("无法打开系统应用：{error}"))?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn pick(directory: bool) -> Result<Option<String>, String> {
    let mut zenity = Command::new("zenity");
    zenity.args(["--file-selection", "--title=导入壁纸"]);
    if directory {
        zenity.arg("--directory");
    }
    let output = match zenity.output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut kdialog = Command::new("kdialog");
            kdialog
                .arg(if directory {
                    "--getexistingdirectory"
                } else {
                    "--getopenfilename"
                })
                .arg(home());
            match kdialog.output() {
                Ok(output) => output,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let dialog = rfd::FileDialog::new()
                        .set_title("导入壁纸")
                        .set_directory(home());
                    let selected = if directory {
                        dialog.pick_folder()
                    } else {
                        dialog.pick_file()
                    };
                    return selected
                        .map(|path| {
                            path.into_os_string()
                                .into_string()
                                .map_err(|_| "所选路径包含无法识别的字符。".to_string())
                        })
                        .transpose();
                }
                Err(error) => return Err(format!("无法打开系统文件选择器：{error}")),
            }
        }
        Err(error) => return Err(format!("无法打开文件选择器：{error}")),
    };
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err("文件选择器未能打开，请检查桌面会话。".into());
    }
    let path =
        String::from_utf8(output.stdout).map_err(|_| "所选路径不是有效的 UTF-8".to_string())?;
    let path = path.trim_end_matches(['\r', '\n']);
    Ok((!path.is_empty()).then(|| path.to_owned()))
}

fn capture(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    let stdout = child.stdout.take().ok_or("命令未提供输出")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(RESPONSE_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(error) => break Err(error.to_string()),
            _ if Instant::now() >= deadline => break Err("命令响应超时".into()),
            _ => thread::sleep(Duration::from_millis(20)),
        }
    };
    if status.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let bytes = reader
        .join()
        .map_err(|_| "读取命令输出失败")?
        .map_err(|error| error.to_string())?;
    if !status?.success() {
        return Err("命令未能完成".into());
    }
    if bytes.len() > RESPONSE_LIMIT {
        return Err("命令输出过大".into());
    }
    Ok(bytes)
}

/// Cache a bounded first frame, including animated GIF previews.
pub fn thumbnail(path: &str) -> Option<PathBuf> {
    prepare_thumbnail(Path::new(path)).ok()
}

fn prepare_thumbnail(path: &Path) -> Result<PathBuf, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 * 1024 {
        return Err("预览文件过大".into());
    }
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    let mut hash = DefaultHasher::new();
    canonical.hash(&mut hash);
    metadata.len().hash(&mut hash);
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .hash(&mut hash);
    "misari-thumbnail-v1".hash(&mut hash);
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
        .join("misari/ui-thumbnails");
    let output = cache.join(format!("{:016x}.png", hash.finish()));
    if output.is_file() {
        return Ok(output);
    }
    let mut reader = image::ImageReader::open(&canonical)
        .map_err(|error| error.to_string())?
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| error.to_string())?
        .thumbnail(480, 320);
    fs::create_dir_all(&cache).map_err(|error| error.to_string())?;
    // A unique temporary path prevents partially written images reaching Iced.
    let temporary = cache.join(format!(
        ".{}-{}-{:?}.png",
        hash.finish(),
        std::process::id(),
        thread::current().id()
    ));
    image
        .save_with_format(&temporary, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    fs::rename(&temporary, &output).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        error.to_string()
    })?;
    Ok(output)
}

/// Read-only diagnostics deliberately do not auto-start or alter the daemon.
pub fn diagnose() -> Value {
    let response = exchange("get", json!({}), false);
    json!({"application":"Misari","socket":socket_path(),"daemon_executable":daemon(),
        "wayland_display":std::env::var("WAYLAND_DISPLAY").ok(),
        "session_type":std::env::var("XDG_SESSION_TYPE").ok(),
        "steam_installed":home().join("Share/Steam/steamapps/common/wallpaper_engine").is_dir(),
        "workshop_content":home().join("Share/Steam/steamapps/workshop/content/431960").is_dir(),
        "daemon":match response {Ok(value)=>value,Err(error)=>json!({"ok":false,"error":error})}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_boundaries_and_errors() {
        assert!(frame("get", json!({})).unwrap().ends_with(b"\n"));
        assert!(frame("set", json!({"text":"x".repeat(REQUEST_LIMIT)})).is_err());
        assert_eq!(
            read_response(&b"{\"ok\":true,\"state\":{}}\n"[..]).unwrap()["state"],
            json!({})
        );
        assert_eq!(
            read_response(&b"{\"ok\":false,\"error\":{\"message\":\"unavailable\"}}\n"[..])
                .unwrap_err(),
            "unavailable"
        );
        assert!(read_response(&b"{}"[..]).is_err());
        assert!(read_response(std::io::repeat(b'x').take(RESPONSE_LIMIT as u64 + 1)).is_err());
    }

    #[test]
    fn activation_frames_are_bounded_and_lock_lifetime_is_exclusive() {
        assert_eq!(read_activation(&b"\"workshop\"\n"[..]).unwrap(), "workshop");
        assert!(read_activation(&b"\"unknown-page\"\n"[..]).is_err());
        assert!(read_activation(&b"\"open\""[..]).is_err());
        assert!(read_activation(std::io::repeat(b'x').take(ACTIVATION_LIMIT as u64 + 1)).is_err());
        let path = std::env::temp_dir().join(format!("misari-ui-lock-test-{}", std::process::id()));
        let first = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let second = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        first.try_lock().unwrap();
        assert!(matches!(
            second.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(first);
        second.try_lock().unwrap();
        drop(second);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn thumbnail_scales_and_invalidates_when_source_changes() {
        let root =
            std::env::temp_dir().join(format!("misari-thumbnail-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("preview.gif");
        image::RgbaImage::from_pixel(960, 640, image::Rgba([10, 20, 30, 255]))
            .save(&source)
            .unwrap();
        let first = prepare_thumbnail(&source).unwrap();
        let preview = image::open(&first).unwrap();
        assert_eq!((preview.width(), preview.height()), (480, 320));
        fs::write(&source, b"invalid").unwrap();
        assert!(prepare_thumbnail(&source).is_err());
        let _ = fs::remove_file(first);
        let _ = fs::remove_dir_all(root);
    }
}
