use crate::model::{Changes, Filter, Resolution, Settings};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{env, fs, os::unix::fs::DirBuilderExt, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    time::timeout,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    List,
    CoverPath {
        id: u64,
    },
    Library,
    SetSettings {
        settings: Settings,
    },
    Lan,
    SetLan {
        enabled: bool,
        password: Option<String>,
    },
    Inspect {
        directory: PathBuf,
    },
    Add {
        executable: PathBuf,
        name: Option<String>,
        proton: Option<String>,
    },
    FindCovers {
        title: String,
    },
    DownloadCover {
        url: String,
    },
    Edit {
        id: u64,
        changes: Box<Changes>,
    },
    SetPicture {
        id: u64,
        enabled: Option<bool>,
        filter: Option<Filter>,
        output: Option<Resolution>,
        fps: Option<u16>,
    },
    Remove {
        id: u64,
    },
    Runtimes,
    Doctor {
        #[serde(default)]
        desktop: Option<Desktop>,
    },
    Launch {
        id: u64,
        #[serde(default)]
        desktop: Option<Desktop>,
    },
    LaunchStream {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        auto_output: Option<bool>,
    },
    Stream {
        id: u64,
    },
    Stop {
        id: u64,
    },
    Status,
    Log {
        id: u64,
        #[serde(default = "log_lines")]
        lines: usize,
    },
    Shutdown,
}

fn log_lines() -> usize {
    100
}

impl Request {
    pub fn use_client_desktop(&mut self) {
        if let Self::Launch { desktop, .. } | Self::Doctor { desktop } = self {
            desktop.get_or_insert_with(Desktop::current);
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct Reply {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Reply {
    pub fn from_result(response: Result<Value>) -> Self {
        match response {
            Ok(result) => Self {
                ok: true,
                result: Some(result),
                error: None,
            },
            Err(error) => Self {
                ok: false,
                result: None,
                error: Some(format!("{error:#}")),
            },
        }
    }

    pub fn into_result(self) -> Result<Value> {
        if self.ok {
            Ok(self.result.unwrap_or(Value::Null))
        } else {
            bail!("{}", self.error.as_deref().unwrap_or("引擎返回了无效响应"))
        }
    }
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub socket: PathBuf,
}

impl Paths {
    pub fn new(data: Option<PathBuf>, socket: Option<PathBuf>) -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME 未设置")?;
        let data = data.unwrap_or_else(|| {
            env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"))
                .join("galscaler")
        });
        let data = std::path::absolute(data)?;
        let socket = socket.unwrap_or_else(|| {
            env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| data.clone())
                .join("galscaler.sock")
        });
        Ok(Self {
            data,
            socket: std::path::absolute(socket)?,
        })
    }

    pub fn prepare(&self) -> Result<()> {
        for path in [
            self.data.clone(),
            self.data.join("prefixes"),
            self.data.join("logs"),
        ] {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
        }
        if let Some(parent) = self.socket.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        Ok(())
    }

    pub fn library(&self) -> PathBuf {
        self.data.join("library.json")
    }
    pub fn prefix(&self, id: u64) -> PathBuf {
        self.data.join("prefixes").join(id.to_string())
    }

    pub fn shared_prefix(&self) -> PathBuf {
        self.data.join("prefixes/shared")
    }
}

fn wayland_display() -> Option<PathBuf> {
    let display = PathBuf::from(env::var_os("WAYLAND_DISPLAY")?);
    let path = if display.is_absolute() {
        display
    } else {
        PathBuf::from(env::var_os("XDG_RUNTIME_DIR")?).join(display)
    };
    path.exists().then_some(path)
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Desktop {
    pub wayland_display: Option<PathBuf>,
    pub display: Option<String>,
}

impl Desktop {
    pub fn current() -> Self {
        Self {
            wayland_display: wayland_display(),
            display: env::var("DISPLAY").ok(),
        }
    }
}

pub const FRAME_LIMIT: usize = 1024 * 1024;

pub async fn frame(stream: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>> {
    frame_with_timeout(stream, Duration::from_secs(10)).await
}

pub async fn frame_with_timeout(
    stream: impl tokio::io::AsyncRead + Unpin,
    wait: Duration,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    timeout(
        wait,
        BufReader::new(stream.take(FRAME_LIMIT as u64 + 1)).read_until(b'\n', &mut bytes),
    )
    .await
    .context("引擎连接超时")??;
    ensure!(bytes.len() <= FRAME_LIMIT, "请求或响应超过 1 MiB");
    ensure!(bytes.last() == Some(&b'\n'), "JSON 消息必须以换行结束");
    Ok(bytes)
}
