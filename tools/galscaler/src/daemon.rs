mod cover;
mod engine;
mod ipc;
mod native;
mod platform;
mod render;
mod scan;
mod stream;
mod web;

use anyhow::{Result, bail};
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
    /// 指定局域网监听地址，使用已保存的密码或 GALSCALER_PASSWORD。
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
    ipc::serve(paths, cli.listen).await
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
        .env_remove("GALSCALER_PASSWORD")
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
