use crate::{
    api::{Desktop, Paths},
    model::{Filter, Game, Prefix, Settings, file},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    env, fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::process::Command;

pub mod ge;

pub fn which(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|p| p.join(name))
        .find(|p| {
            p.is_file()
                && p.metadata()
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        })
}

#[derive(Debug, Clone, Serialize)]
pub struct Proton {
    pub name: String,
    pub path: PathBuf,
}

fn steam_roots() -> Vec<PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let share = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let mut roots: BTreeSet<_> = [
        share.join("Steam"),
        home.join(".steam/root"),
        home.join(".steam/steam"),
        home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
    ]
    .into_iter()
    .filter_map(|p| fs::canonicalize(p).ok())
    .collect();
    for root in roots.clone() {
        if let Ok(text) = fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            for line in text.lines() {
                let mut quoted = line.split('"');
                if quoted.nth(1) == Some("path")
                    && let Some(path) = quoted.nth(1)
                {
                    let path = path.replace("\\\\", "\\");
                    if let Ok(path) = fs::canonicalize(path) {
                        roots.insert(path);
                    }
                }
            }
        }
    }
    roots.into_iter().collect()
}

pub fn protons(paths: &Paths) -> Vec<Proton> {
    let mut installed = BTreeSet::new();
    for dir in steam_roots()
        .into_iter()
        .flat_map(|root| {
            [
                root.join("compatibilitytools.d"),
                root.join("steamapps/common"),
            ]
        })
        .chain(std::iter::once(ge::directory(paths)))
    {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !entry.file_name().as_encoded_bytes().starts_with(b".")
                    && path.join("proton").is_file()
                    && let Ok(path) = fs::canonicalize(path)
                {
                    installed.insert(path);
                }
            }
        }
    }
    let mut tools: Vec<_> = installed
        .into_iter()
        .map(|path| Proton {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            path,
        })
        .collect();
    tools.sort_by_cached_key(|p| {
        let version: Vec<u64> = p
            .name
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|n| n.parse().ok())
            .collect();
        std::cmp::Reverse((p.name.starts_with("GE-Proton"), version, p.name.clone()))
    });
    tools
}

pub fn resolve_proton(value: &str, paths: &Paths) -> Result<PathBuf> {
    let path = Path::new(value);
    if path.exists() {
        let path = fs::canonicalize(path)?;
        let dir = if path.is_file() && path.file_name().is_some_and(|n| n == "proton") {
            path.parent().unwrap().to_owned()
        } else {
            path
        };
        file(&dir.join("proton"))?;
        return Ok(dir);
    }
    let found: Vec<_> = protons(paths)
        .into_iter()
        .filter(|p| p.name == value)
        .collect();
    ensure!(found.len() <= 1, "存在多个同名 Proton，请指定完整目录路径");
    found
        .into_iter()
        .next()
        .map(|p| p.path)
        .with_context(|| format!("未找到 Proton：{value}；使用 runtimes 查看已安装版本"))
}

fn automatic_proton(paths: &Paths, installed: &[Proton]) -> Option<PathBuf> {
    fs::canonicalize(ge::directory(paths).join(".default"))
        .ok()
        .filter(|path| path.join("proton").is_file())
        .or_else(|| installed.first().map(|proton| proton.path.clone()))
}

pub fn selected_proton(configured: Option<&Path>, paths: &Paths) -> Result<PathBuf> {
    let proton = configured
        .map(Path::to_path_buf)
        .or_else(|| automatic_proton(paths, &protons(paths)))
        .context("未找到 Proton，请在全局设置中安装 GE-Proton 或选择已有版本")?;
    file(&proton.join("proton"))?;
    Ok(proton)
}

pub fn launch(
    game: &Game<Settings>,
    proton: &Path,
    paths: &Paths,
    scope: &str,
    log: &Path,
    desktop: Option<&Desktop>,
    streaming: bool,
) -> Result<Command> {
    game.validate()?;
    game.settings.validate()?;
    let exe = crate::model::executable(&game.executable)?;
    let umu = which("umu-run").context("未安装 umu-run（Arch: pacman -S umu-launcher）")?;
    let systemd = which("systemd-run").context("未安装 systemd-run，无法管理游戏进程组")?;
    let mut command = Command::new(systemd);
    command.args([
        "--user",
        "--scope",
        "--collect",
        "--quiet",
        "--unit",
        scope,
        "--property=KillMode=control-group",
        "--property=TimeoutStopSec=5s",
        "--",
    ]);
    if game.settings.enhancement.enabled {
        ensure!(
            desktop.is_none_or(|desktop| desktop
                .wayland_display
                .as_ref()
                .is_some_and(|p| p.is_absolute() && p.exists())),
            "未连接 Wayland 桌面，请在桌面会话中启动引擎"
        );
        command
            // Keep launches working when a rebuild replaces the binary on disk.
            .arg(format!("/proc/{}/exe", std::process::id()))
            .arg("render")
            .arg("--settings")
            .arg(serde_json::to_string(&game.settings.enhancement)?)
            .arg("--metrics")
            .arg(log.with_extension("json"));
        if streaming {
            command.arg("--stream").arg(log.with_extension("stream"));
        }
        if desktop.is_none() {
            command.arg("--headless");
        }
        command.arg("--");
    }
    command.arg(umu);
    if game.settings.enhancement.enabled {
        let helper = paths.data.join("client.exe");
        let temporary = paths.data.join("client.exe.new");
        fs::write(
            &temporary,
            include_bytes!(concat!(env!("OUT_DIR"), "/client.exe")),
        )?;
        fs::rename(temporary, &helper)?;
        command
            .arg(helper)
            .arg(format!("Z:{}", exe.to_string_lossy().replace('/', "\\")));
    } else {
        command.arg(exe);
    }
    command.args(&game.arguments);
    let prefix = match game.prefix {
        Prefix::Shared => paths.shared_prefix(),
        Prefix::Independent => paths.prefix(game.id),
    };
    command
        .envs(&game.settings.environment)
        .env_remove("GALSCALER_PASSWORD")
        .env("WINEPREFIX", prefix)
        .env("PROTONPATH", proton)
        .env("GAMEID", "umu-default")
        .env("PROTON_VERB", "waitforexitandrun")
        .current_dir(game.executable.parent().context("游戏启动目录无效")?)
        .stdin(Stdio::null());
    if game.settings.enhancement.enabled {
        // The Win32 helper associates client rectangles with Wine's X11 IDs.
        command.env("PROTON_ENABLE_WAYLAND", "0");
    }
    if let Some(desktop) = desktop {
        if let Some(value) = &desktop.wayland_display {
            command.env("WAYLAND_DISPLAY", value);
        }
        if let Some(value) = &desktop.display {
            command.env("DISPLAY", value);
        }
    }
    let output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(log)?;
    command.stdout(output.try_clone()?).stderr(output);
    Ok(command)
}

pub async fn stop_scope(scope: &str) -> Result<()> {
    let output = Command::new("systemctl")
        .args(["--user", "stop", "--no-block", scope])
        .output()
        .await
        .context("无法结束游戏进程组")?;
    // systemd returns 5 when a completed scope has already been collected.
    ensure!(
        output.status.success() || output.status.code() == Some(5),
        "无法结束会话：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

pub async fn scope_active(scope: &str) -> Result<bool> {
    let output = Command::new("systemctl")
        .args(["--user", "show", "--property=ActiveState", "--value", scope])
        .output()
        .await
        .context("无法查询游戏进程组")?;
    let state = String::from_utf8_lossy(&output.stdout);
    ensure!(
        !state.trim().is_empty(),
        "无法查询游戏进程组：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(matches!(
        state.trim(),
        "active" | "activating" | "deactivating" | "reloading"
    ))
}

pub async fn doctor(paths: &Paths, desktop: &Desktop) -> serde_json::Value {
    let systemd = Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .await
        .is_ok_and(|o| o.status.success());
    let runtimes = protons(paths);
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "data_dir": paths.data, "socket": paths.socket,
        "wayland": desktop.wayland_display,
        "umu_run": which("umu-run"),
        "systemd_user": systemd, "runtimes": runtimes,
        "automatic_proton": automatic_proton(paths, &runtimes),
        "ge_tools": which("curl").is_some() && which("tar").is_some(),
        "native": { "renderer": "Smithay/EGL/libplacebo", "cunny": "4x16 DS", "anime4k": "Restore CNN M + Upscale CNN x2 M" },
        "filters": [Filter::Linear, Filter::Anime4k, Filter::Cunny],
    })
}
