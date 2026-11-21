use crate::{
    api::{FRAME_LIMIT, Reply},
    ipc::{Queue, call, dispatch},
    stream,
};
use anyhow::anyhow;
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
use std::{sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::watch, time::timeout};

#[derive(Clone)]
struct Network {
    token: Arc<str>,
    queue: Queue,
    stop: watch::Receiver<()>,
}

impl Network {
    fn authorized(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.as_bytes().strip_prefix(b"Bearer "))
            .is_some_and(|token| bool::from(self.token.as_bytes().ct_eq(token)))
    }
}

pub async fn serve(
    listener: TcpListener,
    token: String,
    queue: Queue,
    mut stop: watch::Receiver<()>,
) {
    let _discovery = listener.local_addr().ok().and_then(|address| {
        eprintln!("GalScaler Web 正在监听 {address}");
        if address.ip().is_loopback() {
            return None;
        }
        let mut publish = tokio::process::Command::new("avahi-publish-service");
        publish.args([
            "GalScaler",
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
    });
    let app = Router::new()
        .route("/", get(page))
        .route("/api/v1/rpc", post(rpc))
        .route("/api/v1/ws", get(upgrade))
        .route("/api/v1/stream/{id}", post(offer))
        .with_state(Network {
            token: token.into(),
            queue,
            stop: stop.clone(),
        });
    if let Err(error) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = stop.changed().await;
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
                "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; media-src blob:; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
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
        Reply::from_result(Err(anyhow!("访问令牌无效"))),
    )
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
    State(network): State<Network>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
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
            _ = network.stop.changed() => break,
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

async fn offer(State(network): State<Network>, Path(id): Path<u64>, request: Request) -> Response {
    if !network.authorized(request.headers()) {
        return unauthorized();
    }
    let result = async {
        let body = timeout(Duration::from_secs(5), to_bytes(request.into_body(), 65536)).await??;
        let offer = serde_json::from_slice(&body)?;
        let target = call(&network.queue, crate::api::Request::Stream { id })
            .await?
            .into_result()?;
        let path: std::path::PathBuf = serde_json::from_value(target)?;
        stream::signal(&path, offer).await
    }
    .await;
    response(StatusCode::OK, Reply::from_result(result))
}
