mod cover;
mod engine;
mod ipc;
mod native;
mod platform;
mod render;
mod scan;
mod stream;
mod web;

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use galscaler::{api, model};
use std::{
    ffi::OsString, fs::OpenOptions, net::SocketAddr, os::unix::fs::OpenOptionsExt, path::PathBuf,
    process::Stdio, time::Duration,
};
use tokio::{
    net::UnixStream,
    process::Command,
    time::{Instant, sleep},
};

#[derive(Parser)]
#[command(name = "galscalerd", version, about = "GalScaler 核心服务")]
struct Cli {
    #[arg(long, env = "GALSCALER_DATA_DIR")]
    data_dir: Option<PathBuf>,
    #[arg(long, env = "GALSCALER_SOCKET")]
    socket: Option<PathBuf>,
    /// 启用局域网 HTTP/WebSocket，需设置 GALSCALER_TOKEN。
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// 在后台启动，等待 Unix socket 可用后返回。
    #[arg(long, conflicts_with = "listen")]
    background: bool,
    #[command(subcommand)]
    worker: Option<Worker>,
}

#[derive(Subcommand)]
enum Worker {
    #[command(hide = true)]
    Render {
        #[arg(long)]
        settings: String,
        #[arg(long)]
        metrics: PathBuf,
        #[arg(long)]
        stream: Option<PathBuf>,
        #[arg(long, requires = "stream")]
        headless: bool,
        #[arg(last = true)]
        command: Vec<OsString>,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Worker::Render {
        settings,
        metrics,
        stream,
        headless,
        command,
    }) = cli.worker
    {
        return native::run(
            serde_json::from_str(&settings)?,
            metrics,
            stream,
            headless,
            command,
        );
    }
    let paths = api::Paths::new(cli.data_dir, cli.socket)?;
    if cli.background {
        return start(&paths).await;
    }
    let network = match cli.listen {
        Some(address) => {
            let token =
                std::env::var("GALSCALER_TOKEN").context("请为局域网接口设置 GALSCALER_TOKEN")?;
            ensure!(
                token.len() >= 32 && token.bytes().all(|byte| byte.is_ascii_graphic()),
                "GALSCALER_TOKEN 至少需要 32 个可见 ASCII 字符"
            );
            Some((address, token))
        }
        None => None,
    };
    ipc::serve(paths, network).await
}

async fn start(paths: &api::Paths) -> Result<()> {
    if UnixStream::connect(&paths.socket).await.is_ok() {
        return Ok(());
    }
    paths.prepare()?;
    let log_path = paths.data.join("engine.log");
    let log = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(&log_path)?;
    let mut child = Command::new("/proc/self/exe")
        .arg("--data-dir")
        .arg(&paths.data)
        .arg("--socket")
        .arg(&paths.socket)
        .env_remove("GALSCALER_TOKEN")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if UnixStream::connect(&paths.socket).await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "引擎启动失败（{:?}），请查看 {}",
                child.try_wait()?,
                log_path.display()
            );
        }
        sleep(Duration::from_millis(25)).await;
    }
}
