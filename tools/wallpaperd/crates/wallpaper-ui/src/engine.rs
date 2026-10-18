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
    let mut catalog = request("catalog", json!({}))?;
    if let Some(assets) = catalog["assets"].as_array_mut() {
        for asset in assets {
            enrich_filter_metadata(asset);
        }
    }
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

/// Read metadata once on the loading worker, never while building the grid.
fn enrich_filter_metadata(asset: &mut Value) {
    let path = Path::new(crate::strv(asset, "path"));
    if path.is_dir() {
        let project = fs::File::open(path.join("project.json"))
            .ok()
            .and_then(|file| {
                let mut bytes = Vec::new();
                file.take(1024 * 1024 + 1).read_to_end(&mut bytes).ok()?;
                (bytes.len() <= 1024 * 1024).then_some(())?;
                serde_json::from_slice::<Value>(
                    bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes),
                )
                .ok()
            });
        if let Some(project) = project {
            let metadata = json!({"kind":asset["kind"],"project":project});
            asset["tags"] = json!(crate::filters::metadata_tags(&metadata));
        }
    } else if asset["kind"] == "image"
        && let Ok((width, height)) = image::image_dimensions(path)
    {
        asset["tags"] = json!([crate::filters::resolution_tag(width, height)]);
    }
}

/// Keep project translations at the presentation boundary; property keys and values stay intact.
pub fn properties(params: Value) -> Result<Value, String> {
    let mut snapshot = request("get_properties", params.clone())?;
    if let Some(path) = params["asset_id"]
        .as_str()
        .and_then(|id| id.strip_prefix("we:"))
    {
        let path = Path::new(path).join("project.json");
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= 8 * 1024 * 1024)
            && let Ok(bytes) = fs::read(path)
            && let Ok(project) = serde_json::from_slice::<Value>(&bytes)
        {
            snapshot["localization"] = project["general"]["localization"].clone();
        }
    }
    Ok(snapshot)
}

/// Sum regular file sizes without following symlinks outside the project.
pub fn asset_size(path: &Path) -> Option<u64> {
    let mut pending = vec![path.to_owned()];
    let mut total = 0_u64;
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).ok()?;
        if metadata.is_file() {
            total = total.checked_add(metadata.len())?;
        } else if metadata.is_dir() {
            for entry in fs::read_dir(&path).ok()? {
                pending.push(entry.ok()?.path());
            }
        }
    }
    Some(total)
}

/// Only a catalog asset's canonical file or complete WE project can be trashed.
pub fn wallpaper_path(asset: &Value) -> Result<&Path, String> {
    let id = asset["id"].as_str().ok_or("壁纸编号无效。")?;
    let path = Path::new(asset["path"].as_str().ok_or("壁纸路径无效。")?);
    let valid = if let Some(source) = id.strip_prefix("local:") {
        Path::new(source) == path && path.is_file()
    } else if let Some(source) = id.strip_prefix("we:") {
        Path::new(source) == path && path.is_dir() && path.join("project.json").is_file()
    } else {
        false
    };
    if !valid
        || !path.is_absolute()
        || path.parent().is_none()
        || path.canonicalize().map_err(|e| e.to_string())? != path
    {
        return Err("壁纸路径已变化，请刷新资源库后重试。".into());
    }
    Ok(path)
}

pub fn workshop_id(path: &Path) -> Option<&str> {
    let id = path.file_name()?.to_str()?;
    if !id.bytes().all(|byte| byte.is_ascii_digit()) || id.parse::<u64>().ok()? == 0 {
        return None;
    }
    let app = path.parent()?;
    let content = app.parent()?;
    let workshop = content.parent()?;
    let steamapps = workshop.parent()?;
    (app.file_name()? == "431960"
        && content.file_name()? == "content"
        && workshop.file_name()? == "workshop"
        && steamapps.file_name()? == "steamapps")
        .then_some(id)
}

fn trash_command(path: &Path) -> Command {
    let mut command = Command::new("gio");
    command.args(["trash", "--"]).arg(path);
    command
}

pub fn move_to_trash(path: &Path) -> Result<(), String> {
    capture(&mut trash_command(path), Duration::from_secs(30))
        .map(|_| ())
        .map_err(|error| format!("无法移到系统回收站，请检查 gio 和文件权限：{error}"))
}

pub fn trash_wallpaper(asset_id: &str) -> Result<Value, String> {
    trash_wallpaper_with(asset_id, request, crate::services::execute)
}

fn trash_wallpaper_with(
    asset_id: &str,
    mut request: impl FnMut(&str, Value) -> Result<Value, String>,
    trash: impl FnOnce(Value) -> Result<Value, String>,
) -> Result<Value, String> {
    let catalog = request("catalog", json!({}))?;
    let asset = crate::array(&catalog["assets"])
        .iter()
        .find(|item| item["id"] == asset_id)
        .ok_or("壁纸已不在资源库中，请刷新后重试。")?;
    let path = wallpaper_path(asset)?;
    let state = request("get", json!({}))?;
    let hidden = crate::array(&state["state"]["library"]["hidden"])
        .iter()
        .any(|id| id == asset_id);
    // Exclude it from every rotation before stopping playback or moving files.
    let mut reply = request(
        "edit_library",
        json!({"action":"hide","asset_id":asset_id,"hidden":true}),
    )?;
    let result = (|| {
        let state = request("get", json!({}))?;
        if let Some(outputs) = state["state"]["outputs"].as_object() {
            for (name, output) in outputs {
                if output["current"] == asset_id || output["pending"] == asset_id {
                    request("release", json!({"output":name}))?;
                }
            }
        }
        trash(json!({"action":"trash_wallpaper","id":asset_id,"path":path}))
    })();
    match result {
        Ok(service) => {
            // Refresh rotation candidates before restoring visibility. Restored
            // files and future subscriptions should appear in the library again.
            let refreshed = request("catalog", json!({})).and_then(|_| {
                request(
                    "edit_library",
                    json!({"action":"hide","asset_id":asset_id,"hidden":hidden}),
                )
            });
            match refreshed {
                Ok(value) => reply = value,
                Err(error) => {
                    reply["warning"] = json!(format!("壁纸已移到回收站，但刷新资源库失败：{error}"))
                }
            }
            reply["message"] = service["message"].clone();
            if let Some(warning) = service.get("warning") {
                reply["warning"] = json!(match reply["warning"].as_str() {
                    Some(previous) =>
                        format!("{}；{previous}", warning.as_str().unwrap_or_default()),
                    None => warning.as_str().unwrap_or_default().into(),
                });
            }
            if service.get("account").is_some() {
                reply["service"] = service;
            }
            Ok(reply)
        }
        Err(error) => {
            if !hidden
                && let Err(rollback) = request(
                    "edit_library",
                    json!({"action":"hide","asset_id":asset_id,"hidden":false}),
                )
            {
                return Err(format!("{error}；恢复资源库显示失败：{rollback}"));
            }
            Err(error)
        }
    }
}

pub async fn open(target: &str) -> Result<(), String> {
    if target.is_empty() || target.chars().any(char::is_control) {
        return Err("无法打开空白或包含控制字符的位置。".into());
    }
    use ashpd::desktop::open_uri::{OpenDirectoryRequest, OpenFileRequest};
    let uri = url::Url::parse(target).ok();
    let result = if let Some(uri) = uri.as_ref().filter(|uri| uri.scheme() != "file") {
        OpenFileRequest::default().send_uri(uri).await
    } else {
        let path = match uri {
            Some(uri) => uri.to_file_path().map_err(|_| "无法识别本地文件位置。")?,
            None => PathBuf::from(target),
        };
        let file = fs::File::open(&path).map_err(|error| format!("无法定位文件：{error}"))?;
        // OpenDirectory reveals this item in its parent through FileManager1,
        // with the desktop portal providing a fallback for other file managers.
        OpenDirectoryRequest::default().send(&file).await
    };
    portal_response(result.and_then(|request| request.response())).map(|_| ())
}

pub async fn pick(directory: bool) -> Result<Option<String>, String> {
    let result = ashpd::desktop::file_chooser::SelectedFiles::open_file()
        .title("导入壁纸")
        .accept_label("选择")
        .directory(directory)
        .multiple(false)
        .current_folder(home())
        .map_err(|error| format!("无法设置文件选择位置：{error}"))?
        .send()
        .await
        .and_then(|request| request.response());
    portal_response(result)?
        .map(|selected| {
            selected
                .uris()
                .first()
                .ok_or("文件选择器未返回所选路径。")?
                .to_file_path()
                .map_err(|_| "所选位置不是本地文件。")?
                .into_os_string()
                .into_string()
                .map_err(|_| "所选路径包含无法识别的字符。".to_owned())
        })
        .transpose()
}

fn portal_response<T>(result: Result<T, ashpd::Error>) -> Result<Option<T>, String> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => Ok(None),
        Err(error) => Err(format!("无法调用系统文件服务：{error}")),
    }
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

pub fn reduced_motion() -> bool {
    if let Ok(value) = std::env::var("MISARI_REDUCED_MOTION") {
        return matches!(value.as_str(), "1" | "true");
    }
    capture(
        Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "enable-animations"]),
        Duration::from_millis(200),
    )
    .is_ok_and(|output| output.trim_ascii() == b"false")
}

/// Cache a bounded first frame, including animated GIF previews.
pub fn thumbnail(path: &str) -> Option<PathBuf> {
    prepare_thumbnail(Path::new(path)).ok()
}

/// Iced can lay out decoded handles without opening or decoding files itself.
pub fn thumbnail_handle(path: &Path) -> Option<iced::widget::image::Handle> {
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(480);
    limits.max_image_height = Some(320);
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?.into_rgba8();
    Some(iced::widget::image::Handle::from_rgba(
        image.width(),
        image.height(),
        image.into_raw(),
    ))
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
    fn local_filter_metadata_comes_from_project_files_and_image_headers() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("project.json"), r#"{"tags":["Landscape"],"contentrating":"Everyone","general":{"supportsaudioprocessing":true,"properties":{"speed":{}}}}"#).unwrap();
        let mut asset = json!({"path":directory.path(),"kind":"we_scene"});
        enrich_filter_metadata(&mut asset);
        let tags = crate::filters::metadata_tags(&asset);
        for tag in [
            "Scene",
            "Landscape",
            "Everyone",
            "Audio responsive",
            "Customizable",
        ] {
            assert!(tags.contains(tag));
        }
        let path = directory.path().join("image.png");
        image::RgbaImage::new(16, 9).save(&path).unwrap();
        let mut asset = json!({"path":path,"kind":"image"});
        enrich_filter_metadata(&mut asset);
        assert_eq!(asset["tags"], json!(["Other resolution"]));
        fs::write(directory.path().join("project.json"), "invalid json").unwrap();
        let mut asset = json!({"path":directory.path(),"kind":"we_scene"});
        enrich_filter_metadata(&mut asset);
        assert!(!crate::filters::metadata_tags(&asset).contains("Everyone"));
    }

    #[test]
    fn asset_sizes_cover_files_and_complete_projects_without_symlink_loops() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        fs::create_dir_all(project.join("assets")).unwrap();
        fs::write(project.join("project.json"), b"{}").unwrap();
        let file = project.join("assets/video.mp4");
        fs::write(&file, b"video").unwrap();
        fs::write(directory.path().join("outside"), b"not part of the project").unwrap();
        std::os::unix::fs::symlink(directory.path(), project.join("assets/link")).unwrap();
        assert_eq!(asset_size(&file), Some(5));
        assert_eq!(asset_size(&project), Some(7));
        assert_eq!(asset_size(&project.join("missing")), None);
        fs::write(project.join("empty"), b"").unwrap();
        assert_eq!(asset_size(&project.join("empty")), Some(0));
    }

    #[test]
    fn trash_preserves_wallpaper_files_and_complete_projects() {
        // /tmp can be an internal tmpfs mount that GIO deliberately cannot trash.
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let root = root.path().canonicalize().unwrap();
        let data = root.join("data");
        let file = root.join("壁纸 ' $(echo unsafe).jpg");
        let sibling = root.join("keep.jpg");
        fs::write(&file, b"wallpaper").unwrap();
        fs::write(&sibling, b"keep").unwrap();
        let asset = json!({"id":format!("local:{}", file.display()),"path":file});
        assert_eq!(wallpaper_path(&asset).unwrap(), file);
        assert!(wallpaper_path(&json!({"id":"42","path":file})).is_err());
        assert!(wallpaper_path(&json!({"id":asset["id"],"path":sibling})).is_err());
        let project = root.join("project");
        fs::create_dir_all(project.join("assets")).unwrap();
        fs::write(project.join("project.json"), b"{}").unwrap();
        fs::write(project.join("assets/scene.pkg"), b"scene").unwrap();
        assert!(
            wallpaper_path(&json!({"id":format!("we:{}", project.display()),"path":project}))
                .is_ok()
        );
        assert!(
            wallpaper_path(&json!({"id":format!("we:{}", root.display()),"path":root})).is_err()
        );

        for path in [&file, &project] {
            capture(
                trash_command(path)
                    .env("XDG_DATA_HOME", &data)
                    .env("GIO_USE_VFS", "local"),
                Duration::from_secs(5),
            )
            .unwrap();
            assert!(!path.exists());
        }
        let trashed = data.join("Trash/files");
        assert_eq!(
            fs::read(trashed.join(file.file_name().unwrap())).unwrap(),
            b"wallpaper"
        );
        assert_eq!(
            fs::read(trashed.join("project/assets/scene.pkg")).unwrap(),
            b"scene"
        );
        assert_eq!(fs::read(sibling).unwrap(), b"keep");
        assert_eq!(fs::read_dir(data.join("Trash/info")).unwrap().count(), 2);
    }

    #[test]
    fn workshop_ids_only_match_wallpaper_engine_installations() {
        assert_eq!(
            workshop_id(Path::new("/Steam/steamapps/workshop/content/431960/42")),
            Some("42")
        );
        for path in [
            "/Pictures/42",
            "/Steam/steamapps/workshop/content/123/42",
            "/Steam/steamapps/workshop/content/431960/0",
            "/Steam/steamapps/workshop/content/431960/+42",
            "/Steam/steamapps/workshop/content/431960/42/scene.pkg",
        ] {
            assert_eq!(workshop_id(Path::new(path)), None);
        }
    }

    #[test]
    fn deletion_stops_affected_outputs_and_restores_visibility_on_success_or_failure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .canonicalize()
            .unwrap()
            .join("wallpaper.jpg");
        fs::write(&path, b"wallpaper").unwrap();
        let id = format!("local:{}", path.display());
        for fails in [false, true] {
            let mut calls = Vec::new();
            let reply = trash_wallpaper_with(
                &id,
                |method, params| {
                    calls.push((method.to_owned(), params.clone()));
                    Ok(match method {
                        "catalog" => json!({"assets":[{"id":id,"path":path}]}),
                        "get" => json!({"state":{"library":{"hidden":[]},"outputs":{
                            "A":{"current":id},"B":{"pending":id},"C":{"current":"local:/other"}
                        }}}),
                        "edit_library" => {
                            json!({"library":{"hidden":if params["hidden"] == true {vec![id.clone()]} else {vec![]}}})
                        }
                        "release" => json!({}),
                        _ => panic!("unexpected request: {method}"),
                    })
                },
                |action| {
                    assert_eq!(action["action"], "trash_wallpaper");
                    assert_eq!(action["id"], id);
                    if fails {
                        Err("trash unavailable".into())
                    } else {
                        Ok(json!({"message":"deleted"}))
                    }
                },
            );
            assert_eq!(reply.is_err(), fails);
            assert_eq!(calls[2].1["hidden"], true, "hide before stopping playback");
            assert_eq!(
                calls
                    .iter()
                    .filter(|(method, _)| method == "release")
                    .map(|(_, params)| crate::strv(params, "output"))
                    .collect::<Vec<_>>(),
                ["A", "B"]
            );
            assert_eq!(calls.last().unwrap().1["hidden"], false);
            if !fails {
                assert_eq!(
                    calls[calls.len() - 2].0,
                    "catalog",
                    "refresh rotations before unhiding"
                );
            }
        }
    }

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
