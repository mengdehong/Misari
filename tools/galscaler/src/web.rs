use crate::{
    api::{FRAME_LIMIT, Reply},
    cover,
    ipc::{Queue, call, dispatch},
    stream,
};
use anyhow::{Context, Result, anyhow, ensure};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{
        Path, Request, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::ErrorKind,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, process::Command, sync::watch, task::JoinHandle, time::timeout};

#[derive(Default, Deserialize, Serialize)]
struct Settings {
    enabled: bool,
    password: String,
}

type Authorization = Option<Arc<str>>;

pub struct Lan {
    path: PathBuf,
    name: String,
    settings: Settings,
    listen: Option<SocketAddr>,
    server: Option<Server>,
    error: Option<String>,
}

struct Server {
    address: SocketAddr,
    auth: watch::Sender<Authorization>,
    task: JoinHandle<()>,
}

impl Lan {
    pub async fn open(path: PathBuf, listen: Option<SocketAddr>, queue: &Queue) -> Result<Self> {
        let mut settings: Settings = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("无法读取局域网设置")?,
            Err(error) if error.kind() == ErrorKind::NotFound => Settings::default(),
            Err(error) => return Err(error).context("无法读取局域网设置"),
        };
        if listen.is_some() {
            if let Ok(password) = std::env::var("GALSCALER_PASSWORD") {
                validate_password(&password)?;
                settings.password = password;
            }
            settings.enabled = true;
        }
        let mut lan = Self {
            path,
            name: fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "GalScaler".into()),
            settings,
            listen,
            server: None,
            error: None,
        };
        if lan.settings.enabled {
            let start = async {
                ensure!(!lan.settings.password.trim().is_empty(), "请先设置连接密码");
                lan.bind().await
            };
            match start.await {
                Ok((listener, address)) => {
                    lan.server = Some(Server::start(
                        listener,
                        address,
                        &lan.name,
                        &lan.settings.password,
                        queue,
                    ));
                }
                Err(error) if listen.is_some() => return Err(error),
                Err(error) => lan.error = Some(format!("{error:#}")),
            }
        }
        Ok(lan)
    }

    pub fn enabled(&self) -> bool {
        self.server.is_some()
    }

    pub fn status(&self) -> Value {
        json!({
            "name": self.name,
            "enabled": self.enabled(),
            "address": self.server.as_ref().map(|server| format!("http://{}", server.address)),
            "has_password": !self.settings.password.is_empty(),
            "error": self.error,
        })
    }

    pub async fn set(
        &mut self,
        enabled: bool,
        password: Option<String>,
        queue: &Queue,
    ) -> Result<bool> {
        if let Some(password) = &password {
            validate_password(password)?;
        }
        let password = password.unwrap_or_else(|| self.settings.password.clone());
        if enabled {
            ensure!(!password.trim().is_empty(), "请先设置连接密码");
        }
        let bound = if enabled && self.server.is_none() {
            Some(self.bind().await?)
        } else {
            None
        };
        let changed = password != self.settings.password;
        let revoke = self.enabled() && (!enabled || changed);
        let settings = Settings { enabled, password };
        crate::model::save_json(&self.path, &settings).context("无法保存局域网设置")?;
        if !enabled {
            self.stop().await;
        } else if let Some((listener, address)) = bound {
            self.server = Some(Server::start(
                listener,
                address,
                &self.name,
                &settings.password,
                queue,
            ));
        } else if changed && let Some(server) = &self.server {
            server.auth.send_replace(authorization(&settings.password));
        }
        self.settings = settings;
        self.error = None;
        Ok(revoke)
    }

    async fn bind(&self) -> Result<(TcpListener, SocketAddr)> {
        let listen = match self.listen {
            Some(address) => address,
            None => SocketAddr::from((lan_ip().await?, 9771)),
        };
        let listener = TcpListener::bind(listen)
            .await
            .with_context(|| format!("无法监听 {listen}"))?;
        let mut address = listener.local_addr()?;
        if address.ip().is_unspecified() {
            address.set_ip(lan_ip().await?.into());
        }
        Ok((listener, address))
    }

    pub async fn stop(&mut self) {
        if let Some(server) = self.server.take() {
            server.auth.send_replace(None);
            server.task.abort();
            let _ = server.task.await;
        }
    }
}

impl Server {
    fn start(
        listener: TcpListener,
        address: SocketAddr,
        name: &str,
        password: &str,
        queue: &Queue,
    ) -> Self {
        let (auth, state) = watch::channel(authorization(password));
        let task = tokio::spawn(serve(
            listener,
            address,
            name.to_owned(),
            queue.clone(),
            state,
        ));
        Self {
            address,
            auth,
            task,
        }
    }
}

fn validate_password(password: &str) -> Result<()> {
    ensure!(
        (6..=64).contains(&password.chars().count())
            && !password.trim().is_empty()
            && !password.chars().any(char::is_control),
        "密码须为 6–64 个字符，且不能全为空白"
    );
    Ok(())
}

fn authorization(password: &str) -> Authorization {
    Some(format!("Basic {}", STANDARD.encode(format!("galscaler:{password}"))).into())
}

async fn lan_ip() -> Result<Ipv4Addr> {
    #[derive(Deserialize)]
    struct Route {
        dst: String,
        dev: Option<String>,
        prefsrc: Option<Ipv4Addr>,
        #[serde(default)]
        metric: u32,
        #[serde(default)]
        flags: Vec<String>,
    }
    let routes = timeout(Duration::from_secs(2), async {
        Command::new("ip")
            .args(["-4", "-j", "route", "show", "table", "main"])
            .kill_on_drop(true)
            .output()
            .await
    })
    .await?
    .context("无法读取本机网络，请检查 ip 命令")?;
    ensure!(routes.status.success(), "无法读取本机网络");
    let routes: Vec<Route> = serde_json::from_slice(&routes.stdout)?;
    routes
        .iter()
        .filter(|route| !route.flags.iter().any(|flag| flag == "linkdown"))
        .filter_map(|route| {
            let ip = route.prefsrc.filter(Ipv4Addr::is_private)?;
            let default_metric = routes
                .iter()
                .filter(|other| other.dst == "default" && other.dev == route.dev)
                .map(|other| other.metric)
                .min();
            Some((
                (
                    default_metric.is_none(),
                    default_metric.unwrap_or(route.metric),
                    route.metric,
                ),
                ip,
            ))
        })
        .min_by_key(|(priority, _)| *priority)
        .map(|(_, ip)| ip)
        .context("未找到局域网 IPv4 地址，请检查网络连接")
}

#[derive(Clone)]
struct Network {
    queue: Queue,
    auth: watch::Receiver<Authorization>,
}

impl Network {
    fn authorized(&self, headers: &HeaderMap) -> bool {
        self.auth.borrow().as_ref().is_some_and(|expected| {
            headers
                .get(header::AUTHORIZATION)
                .is_some_and(|header| bool::from(expected.as_bytes().ct_eq(header.as_bytes())))
        })
    }
}

async fn serve(
    listener: TcpListener,
    address: SocketAddr,
    name: String,
    queue: Queue,
    mut auth: watch::Receiver<Authorization>,
) {
    let _discovery = (|| {
        eprintln!("GalScaler Web 正在监听 {address}");
        if address.ip().is_loopback() {
            return None;
        }
        let mut publish = tokio::process::Command::new("avahi-publish-service");
        publish.args([
            "--",
            &name,
            "_galscaler._tcp",
            &address.port().to_string(),
            "version=1",
        ]);
        if !address.ip().is_unspecified() {
            publish.arg(format!("address={}", address.ip()));
        }
        match publish.kill_on_drop(true).spawn() {
            Ok(child) => Some(child),
            Err(error) => {
                eprintln!("无法发布 GalScaler mDNS 服务：{error}");
                None
            }
        }
    })();
    let app = Router::new()
        .route("/", get(page))
        .route("/api/v1/cover/{id}", get(game_cover))
        .route("/api/v1/rpc", post(rpc))
        .route("/api/v1/ws", get(upgrade))
        .route("/api/v1/stream/{id}", post(offer))
        .with_state(Network {
            queue,
            auth: auth.clone(),
        });
    if let Err(error) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = auth.wait_for(Option::is_none).await;
        })
        .await
    {
        eprintln!("局域网服务退出：{error:#}");
    }
}

async fn page() -> impl IntoResponse {
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src blob:; media-src blob:; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
            ),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Html(include_str!("../mobile/player.html")),
    )
}

fn response(status: StatusCode, reply: Reply) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], Json(reply)).into_response()
}

fn unauthorized() -> Response {
    response(
        StatusCode::UNAUTHORIZED,
        Reply::from_result(Err(anyhow!("密码不正确"))),
    )
}

async fn game_cover(
    State(network): State<Network>,
    Path(id): Path<u64>,
    headers: HeaderMap,
) -> Response {
    if !network.authorized(&headers) {
        return unauthorized();
    }
    let image = async {
        let reply = call(&network.queue, crate::api::Request::CoverPath { id }).await?;
        let path: PathBuf = serde_json::from_value(reply.into_result()?)?;
        tokio::task::spawn_blocking(move || cover::read(&path)).await?
    }
    .await;
    if !network.authorized(&headers) {
        return unauthorized();
    }
    match image {
        Ok((media_type, bytes)) => (
            [
                (header::CONTENT_TYPE, media_type),
                (header::CACHE_CONTROL, "no-store"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn rpc(State(network): State<Network>, request: Request) -> Response {
    if !network.authorized(request.headers()) {
        return unauthorized();
    }
    match timeout(
        Duration::from_secs(10),
        to_bytes(request.into_body(), FRAME_LIMIT),
    )
    .await
    {
        Ok(Ok(bytes)) => response(StatusCode::OK, dispatch(&network.queue, &bytes, None).await),
        Ok(Err(_)) => response(
            StatusCode::BAD_REQUEST,
            Reply::from_result(Err(anyhow!("无法读取请求，请求上限为 1 MiB"))),
        ),
        Err(_) => response(
            StatusCode::REQUEST_TIMEOUT,
            Reply::from_result(Err(anyhow!("读取请求超时"))),
        ),
    }
}

async fn upgrade(
    State(mut network): State<Network>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    network.auth.mark_unchanged();
    if !network.authorized(&headers) {
        return unauthorized();
    }
    ws.max_frame_size(FRAME_LIMIT)
        .max_message_size(FRAME_LIMIT)
        .on_upgrade(move |socket| websocket(socket, network))
}

async fn websocket(mut socket: WebSocket, mut network: Network) {
    loop {
        let message = tokio::select! {
            biased;
            _ = network.auth.changed() => break,
            message = socket.recv() => message,
        };
        let reply = match message {
            Some(Ok(Message::Text(text))) => dispatch(&network.queue, text.as_bytes(), None).await,
            Some(Ok(Message::Binary(bytes))) => dispatch(&network.queue, &bytes, None).await,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            _ => break,
        };
        let Ok(json) = serde_json::to_string(&reply) else {
            break;
        };
        if !matches!(
            timeout(
                Duration::from_secs(3),
                socket.send(Message::Text(json.into()))
            )
            .await,
            Ok(Ok(()))
        ) {
            break;
        }
    }
    let _ = timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
}

async fn offer(
    State(mut network): State<Network>,
    Path(id): Path<u64>,
    request: Request,
) -> Response {
    network.auth.mark_unchanged();
    if !network.authorized(request.headers()) {
        return unauthorized();
    }
    let result = tokio::select! {
        biased;
        _ = network.auth.changed() => return unauthorized(),
        result = async {
        let body = timeout(Duration::from_secs(5), to_bytes(request.into_body(), 65536)).await??;
        let offer = serde_json::from_slice(&body)?;
        let target = call(&network.queue, crate::api::Request::Stream { id })
            .await?
            .into_result()?;
        let path: std::path::PathBuf = serde_json::from_value(target)?;
        stream::signal(&path, offer).await
        } => result,
    };
    response(StatusCode::OK, Reply::from_result(result))
}
