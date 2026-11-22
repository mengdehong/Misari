use crate::{
    api::{Paths, Reply, Request, frame},
    cover,
    engine::Engine,
    platform::ge,
    web,
};
use anyhow::{Context, Result, anyhow, ensure};
use std::{
    fs::{self, OpenOptions},
    io::ErrorKind,
    net::SocketAddr,
    os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::{UnixListener, UnixStream},
    signal::unix::{SignalKind, signal},
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
    time::timeout,
};

pub type Queue = mpsc::Sender<(Request, oneshot::Sender<Reply>)>;

pub async fn dispatch(queue: &Queue, bytes: &[u8], local: Option<&Paths>) -> Reply {
    let result = async {
        let request = serde_json::from_slice(bytes).context("无效的 JSON 请求")?;
        ensure!(
            local.is_some()
                || matches!(
                    request,
                    Request::List
                        | Request::Status
                        | Request::GameMetadata { .. }
                        | Request::Log { .. }
                        | Request::SetPicture { .. }
                        | Request::Stop { .. }
                        | Request::LaunchStream { .. }
                ),
            "该操作仅支持本地连接"
        );
        if matches!(request, Request::GameMetadata { .. }) {
            let name: String = serde_json::from_value(call(queue, request).await?.into_result()?)?;
            let info = timeout(Duration::from_secs(11), cover::metadata(&name))
                .await
                .context("读取 VNDB 资料超时")??;
            return Ok(Reply::from_result(Ok(serde_json::to_value(info)?)));
        }
        if let Some(paths) = local {
            let value = match &request {
                Request::FindCovers { title } => Some(serde_json::to_value(
                    cover::candidates(&paths.data, title).await?,
                )?),
                Request::DownloadCover { url } => Some(serde_json::to_value(
                    cover::download(&paths.data, url).await?,
                )?),
                Request::CheckGeProton => Some(serde_json::to_value(ge::check(paths).await?)?),
                Request::InstallGeProton { version } => Some(serde_json::to_value(
                    ge::install(paths, version.as_deref()).await?,
                )?),
                _ => None,
            };
            if let Some(value) = value {
                return Ok(Reply::from_result(Ok(value)));
            }
        }
        call(queue, request).await
    }
    .await;
    result.unwrap_or_else(|error| Reply::from_result(Err(error)))
}

pub async fn call(queue: &Queue, request: Request) -> Result<Reply> {
    let (reply, receive) = oneshot::channel();
    queue
        .send((request, reply))
        .await
        .map_err(|_| anyhow!("引擎正在关闭"))?;
    receive.await.context("引擎正在关闭")
}

async fn serve_socket(
    mut stream: UnixStream,
    queue: Queue,
    paths: Paths,
    mut stop: watch::Receiver<()>,
) -> Result<()> {
    let request = tokio::select! {
        request = frame(&mut stream) => request,
        _ = stop.changed() => return Ok(()),
    };
    let reply = match request {
        Ok(bytes) => dispatch(&queue, &bytes, Some(&paths)).await,
        Err(error) => Reply::from_result(Err(error)),
    };
    let mut bytes = serde_json::to_vec(&reply)?;
    bytes.push(b'\n');
    timeout(Duration::from_secs(3), stream.write_all(&bytes)).await??;
    Ok(())
}

pub async fn serve(paths: Paths, listen: Option<SocketAddr>) -> Result<()> {
    paths.prepare()?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(paths.data.join("engine.lock"))?;
    lock.try_lock().context("这份游戏库已有引擎正在运行")?;
    let socket_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(paths.socket.with_added_extension("lock"))?;
    socket_lock.try_lock().context("该引擎 socket 已被占用")?;
    let mut engine = Engine::open(paths.clone(), false)?;
    engine.refresh().await?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    match UnixStream::connect(&paths.socket).await {
        Ok(_) => return Err(anyhow!("该引擎 socket 已被占用")),
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
            ensure!(
                fs::symlink_metadata(&paths.socket)?.file_type().is_socket(),
                "已有文件占用引擎 socket 路径"
            );
            fs::remove_file(&paths.socket)?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("无法检查引擎 socket"),
    }
    let (tx, mut rx) = mpsc::channel::<(Request, oneshot::Sender<Reply>)>(32);
    let (stopping, stop) = watch::channel(());
    let mut lan = web::Lan::open(paths.data.join("lan.json"), listen, &tx).await?;
    engine.streaming = lan.enabled();
    let listener = UnixListener::bind(&paths.socket).context("无法创建引擎 socket")?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    let mut clients = JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let result = loop {
        tokio::select! {
            connection = listener.accept() => {
                let (stream, _) = match connection {
                    Ok(connection) => connection,
                    Err(error) => break Err(error).context("无法接收本地连接"),
                };
                clients.spawn(serve_socket(stream, tx.clone(), paths.clone(), stop.clone()));
            }
            Some(_) = clients.join_next() => {}
            Some(result) = engine.cover_jobs.join_next() => {
                if let Ok((id, cover)) = result
                    && let Err(error) = engine.finish_cover(id, cover) {
                    eprintln!("游戏 {id} 的封面查找失败：{error:#}");
                }
            }
            Some((request, reply)) = rx.recv() => {
                let shutdown = matches!(request, Request::Shutdown);
                let response = match request {
                    Request::Lan => Ok(lan.status()),
                    Request::SetLan { enabled, password } => match lan.set(enabled, password, &tx).await {
                        Ok(revoke) => {
                            engine.streaming = lan.enabled();
                            if revoke {
                                engine.disconnect_streams().await;
                            }
                            Ok(lan.status())
                        }
                        Err(error) => Err(error),
                    },
                    request => engine.handle(request).await,
                };
                let stopped = shutdown && response.is_ok();
                let _ = reply.send(Reply::from_result(response));
                if stopped { break Ok(()); }
            }
            _ = tick.tick() => {
                if let Err(error) = engine.refresh().await { eprintln!("更新会话失败：{error:#}"); }
            }
            _ = terminate.recv() => { break engine.shutdown().await; }
            _ = interrupt.recv() => { break engine.shutdown().await; }
        }
    };
    drop(listener);
    drop(rx);
    stopping.send_replace(());
    lan.stop().await;
    let _ = timeout(Duration::from_secs(3), async {
        while clients.join_next().await.is_some() {}
    })
    .await;
    fs::remove_file(paths.socket)?;
    result
}
