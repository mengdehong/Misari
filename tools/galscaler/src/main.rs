use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use galscaler::{
    api::{FRAME_LIMIT, Paths, Reply, Request, frame, frame_with_timeout},
    model::Changes,
};
use std::{
    io::{self, Read},
    net::SocketAddr,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::Command,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, net::UnixStream};

#[derive(Parser)]
#[command(
    version,
    about = "Galgame 启动与实时画质增强引擎；命令自动连接后台服务，输出 JSON"
)]
struct Cli {
    #[arg(long, global = true, env = "GALSCALER_DATA_DIR")]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true, env = "GALSCALER_SOCKET")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// 从标准输入读取一个 JSON 请求。
    Request,
    /// 在前台运行 galscalerd。默认只监听本地 Unix socket。
    Serve {
        #[arg(long)]
        listen: Option<SocketAddr>,
    },
    /// 查看游戏库。
    List,
    /// 识别游戏目录的引擎和启动入口。
    Inspect { directory: PathBuf },
    /// 添加 Windows 游戏，默认使用文件夹名称和全局 Proton 设置。
    Add {
        executable: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        proton: Option<String>,
    },
    /// 修改游戏配置，下次启动时生效。
    Edit {
        id: u64,
        #[command(flatten)]
        changes: Box<Changes>,
    },
    /// 移出游戏库，保留游戏文件、兼容环境和存档。
    Remove { id: u64 },
    /// 列出本机 Steam 与 GE-Proton 版本，也可在配置中使用自定义路径。
    Runtimes,
    /// 检查 Wayland、umu 和运行环境。
    Doctor,
    /// 启动游戏并立即返回会话；用 status 和 log 查看后续结果。
    Launch { id: u64 },
    /// 结束游戏会话及其全部 Wine/容器子进程。
    Stop { id: u64 },
    /// 查看引擎和游戏会话状态。
    Status,
    /// 查看最近一次启动的日志。
    Log {
        id: u64,
        #[arg(long, default_value_t = 100)]
        lines: usize,
    },
    /// 结束全部游戏会话并关闭引擎。
    Shutdown,
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
    let paths = Paths::new(cli.data_dir, cli.socket)?;
    let mut request = match cli.command {
        Action::Request => {
            let mut input = String::new();
            std::io::stdin()
                .take(FRAME_LIMIT as u64 + 1)
                .read_to_string(&mut input)?;
            ensure!(input.len() <= FRAME_LIMIT, "请求超过 1 MiB");
            serde_json::from_str(&input)?
        }
        Action::Serve { listen } => {
            let mut daemon = daemon_command(&paths)?;
            if let Some(address) = listen {
                daemon.arg("--listen").arg(address.to_string());
            }
            return Err(daemon.exec()).context("无法运行 galscalerd");
        }
        Action::List => Request::List,
        Action::Inspect { directory } => Request::Inspect {
            directory: std::path::absolute(directory)?,
        },
        Action::Add {
            executable,
            name,
            proton,
        } => Request::Add {
            executable: std::path::absolute(executable)?,
            name,
            proton: proton.map(absolute_proton).transpose()?,
        },
        Action::Edit { id, mut changes } => {
            changes.executable = changes.executable.map(std::path::absolute).transpose()?;
            changes.cover = changes.cover.map(std::path::absolute).transpose()?;
            changes.proton = changes.proton.map(absolute_proton).transpose()?;
            Request::Edit { id, changes }
        }
        Action::Remove { id } => Request::Remove { id },
        Action::Runtimes => Request::Runtimes,
        Action::Doctor => Request::Doctor { desktop: None },
        Action::Launch { id } => Request::Launch { id, desktop: None },
        Action::Stop { id } => Request::Stop { id },
        Action::Status => Request::Status,
        Action::Log { id, lines } => Request::Log { id, lines },
        Action::Shutdown => Request::Shutdown,
    };
    request.use_client_desktop();
    let result = call(&paths, &request).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn daemon_command(paths: &Paths) -> Result<Command> {
    let executable = if let Some(path) = std::env::var_os("GALSCALER_DAEMON") {
        PathBuf::from(path)
    } else {
        let sibling = std::env::current_exe()?.with_file_name("galscalerd");
        if sibling.is_file() {
            sibling
        } else {
            "galscalerd".into()
        }
    };
    let mut command = Command::new(executable);
    command
        .arg("--data-dir")
        .arg(&paths.data)
        .arg("--socket")
        .arg(&paths.socket);
    Ok(command)
}

async fn call(paths: &Paths, request: &Request) -> Result<serde_json::Value> {
    let mut bytes = serde_json::to_vec(request)?;
    ensure!(bytes.len() < FRAME_LIMIT, "请求超过 1 MiB");
    bytes.push(b'\n');
    let mut stream = match UnixStream::connect(&paths.socket).await {
        Ok(stream) => stream,
        Err(error)
            if !matches!(request, Request::Shutdown)
                && matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
        {
            let status = daemon_command(paths)?
                .arg("--background")
                .status()
                .context("无法运行 galscalerd")?;
            ensure!(status.success(), "galscalerd 启动失败");
            UnixStream::connect(&paths.socket)
                .await
                .context("无法连接 GalScaler 引擎")?
        }
        Err(error) => return Err(error).context("无法连接 GalScaler 引擎"),
    };
    stream.write_all(&bytes).await?;
    let reply = if matches!(request, Request::FindCovers { .. }) {
        frame_with_timeout(stream, Duration::from_secs(30)).await?
    } else {
        frame(stream).await?
    };
    serde_json::from_slice::<Reply>(&reply)?.into_result()
}

fn absolute_proton(value: String) -> Result<String> {
    if std::path::Path::new(&value).exists() {
        Ok(std::path::absolute(value)?.to_string_lossy().into())
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_accepts_json_arguments_and_environment_without_shell_expansion() {
        let cli = Cli::try_parse_from([
            "galscaler",
            "edit",
            "7",
            "--arguments",
            r#"["--title=中文 游戏", "$HOME"]"#,
            "--environment",
            r#"{"LC_ALL":"ja_JP.UTF-8"}"#,
        ])
        .unwrap();
        let Action::Edit { id, changes } = cli.command else {
            panic!("expected edit")
        };
        assert_eq!(id, 7);
        assert_eq!(changes.arguments.unwrap(), ["--title=中文 游戏", "$HOME"]);
        assert_eq!(changes.environment.unwrap()["LC_ALL"], "ja_JP.UTF-8");
    }
}
