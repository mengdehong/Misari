use super::{Proton, automatic_proton, protons};
use crate::api::Paths;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    env::consts::ARCH,
    fs::{self, OpenOptions},
    os::unix::fs::symlink,
    path::PathBuf,
};
use tokio::process::Command;

const REPOSITORY: &str = "GloriousEggroll/proton-ge-custom";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
}

pub(super) fn directory(paths: &Paths) -> PathBuf {
    paths.data.join("protons")
}

fn version(name: &str) -> Option<(u32, u32)> {
    let (major, rest) = name.strip_prefix("GE-Proton")?.split_once('-')?;
    let minor = rest.split('-').next()?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn release_version(tag: &str) -> Result<(u32, u32)> {
    let (major, minor) = version(tag).context("无效的 GE-Proton 版本")?;
    ensure!(
        tag == format!("GE-Proton{major}-{minor}"),
        "无效的 GE-Proton 版本"
    );
    Ok((major, minor))
}

fn curl() -> Command {
    let mut command = Command::new("curl");
    command.kill_on_drop(true).args([
        "--disable",
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "8",
        "--user-agent",
        concat!("GalScaler/", env!("CARGO_PKG_VERSION")),
    ]);
    command
}

async fn release(tag: Option<&str>) -> Result<Release> {
    let endpoint = if let Some(tag) = tag {
        release_version(tag)?;
        format!("tags/{tag}")
    } else {
        "latest".into()
    };
    let output = curl()
        .args(["--max-time", "15", "--max-filesize", "1048576"])
        .arg(format!(
            "https://api.github.com/repos/{REPOSITORY}/releases/{endpoint}"
        ))
        .output()
        .await
        .context("无法运行 curl")?;
    ensure!(
        output.status.success(),
        "无法读取 GE-Proton 发布信息：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let release: Release =
        serde_json::from_slice(&output.stdout).context("无效的 GE-Proton 发布信息")?;
    release_version(&release.tag_name)?;
    Ok(release)
}

impl Release {
    fn archive(&self) -> Result<&str> {
        let native = format!("{}-{ARCH}.tar.gz", self.tag_name);
        let legacy = format!("{}.tar.gz", self.tag_name);
        self.assets
            .iter()
            .find(|asset| asset.name == native)
            .or_else(|| {
                self.assets
                    .iter()
                    .find(|asset| ARCH == "x86_64" && asset.name == legacy)
            })
            .map(|asset| asset.name.as_str())
            .with_context(|| format!("{} 没有适用于 {ARCH} 的安装包", self.tag_name))
    }
}

pub async fn check(paths: &Paths) -> Result<Option<String>> {
    let release = release(None).await?;
    release.archive()?;
    let latest = release_version(&release.tag_name)?;
    let has_latest = protons(paths)
        .iter()
        .filter_map(|proton| version(&proton.name))
        .any(|installed| installed >= latest);
    Ok((!has_latest).then_some(release.tag_name))
}

pub async fn install(paths: &Paths, tag: Option<&str>) -> Result<Proton> {
    let root = directory(paths);
    fs::create_dir_all(&root).context("无法创建 GE-Proton 安装目录")?;
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".install.lock"))?;
    lock.try_lock().context("已有 GE-Proton 安装正在进行")?;

    let release = release(tag).await?;
    let archive = release.archive()?;
    let latest = release_version(&release.tag_name)?;
    let installed = protons(paths);
    if let Some(existing) = installed
        .iter()
        .find(|proton| version(&proton.name) == Some(latest))
    {
        return Ok(existing.clone());
    }
    let name = archive.strip_suffix(".tar.gz").unwrap().to_owned();
    let path = root.join(&name);
    let stage = tempfile::tempdir_in(&root)?;
    let download = stage.path().join("proton.tar.gz");
    let output = curl()
        .args([
            "--max-time",
            "3600",
            "--speed-time",
            "60",
            "--speed-limit",
            "1024",
            "--output",
        ])
        .arg(&download)
        .arg(format!(
            "https://github.com/{REPOSITORY}/releases/download/{}/{archive}",
            release.tag_name
        ))
        .output()
        .await
        .context("无法运行 curl")?;
    ensure!(
        output.status.success(),
        "下载 GE-Proton 失败：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );

    let unpacked = stage.path().join("runtime");
    fs::create_dir(&unpacked)?;
    let output = Command::new("tar")
        .kill_on_drop(true)
        .args(["-xzf"])
        .arg(&download)
        .arg("-C")
        .arg(&unpacked)
        .args(["--strip-components=1", "--no-same-owner"])
        .output()
        .await
        .context("无法运行 tar")?;
    ensure!(
        output.status.success(),
        "解压 GE-Proton 失败：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    ensure!(
        unpacked.join("proton").is_file(),
        "GE-Proton 安装包缺少启动程序"
    );

    let preferred = automatic_proton(paths, &installed)
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(version)
                .is_some()
        })
        .unwrap_or_else(|| path.clone());
    let link = stage.path().join("default");
    symlink(preferred, &link)?;
    fs::rename(link, root.join(".default")).context("无法保存自动选择的 Proton")?;
    fs::rename(unpacked, &path).context("无法安装 GE-Proton")?;
    Ok(Proton { name, path })
}
