use crate::{
    api::{Desktop, Paths, Request},
    cover,
    model::{Changes, Enhancement, Filter, Library, Prefix},
    platform, stream,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    process::Child,
    task::{AbortHandle, JoinSet},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Running,
    Stopping,
    Exited,
    Failed,
    Interrupted,
}

impl State {
    fn active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub game_id: u64,
    pub state: State,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub exit_code: Option<i32>,
    pub log: PathBuf,
    pub scope: String,
    pub error: Option<String>,
    #[serde(default)]
    pub prefix: Prefix,
    #[serde(default)]
    pub backend: String,
    #[serde(default)]
    pub metrics: Option<crate::render::Metrics>,
    #[serde(default)]
    pub streaming: bool,
}

struct Run {
    session: Session,
    child: Option<Child>,
}

pub struct Engine {
    pub paths: Paths,
    library: Library,
    runs: BTreeMap<u64, Run>,
    pub cover_jobs: JoinSet<(u64, Result<Option<PathBuf>>)>,
    pending_covers: BTreeMap<u64, AbortHandle>,
    pub streaming: bool,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn stream_picture(picture: &mut Enhancement, auto_output: bool) {
    if !picture.enabled {
        picture.enabled = true;
        picture.filter = Filter::Linear;
        picture.output = picture.source;
    }
    if auto_output {
        picture.output = stream::MAX_RESOLUTION;
    }
}

impl Engine {
    pub fn open(paths: Paths, streaming: bool) -> Result<Self> {
        paths.prepare()?;
        let library = Library::load(&paths.library())?;
        let sessions: Vec<Session> = match fs::read(paths.data.join("sessions.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("无法读取会话状态")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).context("无法读取会话状态"),
        };
        let runs = sessions
            .into_iter()
            .map(|session| {
                (
                    session.game_id,
                    Run {
                        session,
                        child: None,
                    },
                )
            })
            .collect();
        Ok(Self {
            paths,
            library,
            runs,
            cover_jobs: JoinSet::new(),
            pending_covers: BTreeMap::new(),
            streaming,
        })
    }

    fn cancel_cover(&mut self, id: u64) {
        if let Some(job) = self.pending_covers.remove(&id) {
            job.abort();
        }
    }

    pub fn finish_cover(&mut self, id: u64, result: Result<Option<PathBuf>>) -> Result<()> {
        if self.pending_covers.remove(&id).is_none() {
            return Ok(());
        }
        let Some(cover) = result? else {
            return Ok(());
        };
        let mut library = self.library.clone();
        let Some(game) = library.games.iter_mut().find(|game| game.id == id) else {
            return Ok(());
        };
        game.cover = Some(cover);
        library.save(&self.paths.library())?;
        self.library = library;
        Ok(())
    }

    fn save_sessions(&self) -> Result<()> {
        let sessions: Vec<_> = self.runs.values().map(|r| &r.session).collect();
        crate::model::save_json(&self.paths.data.join("sessions.json"), &sessions)
    }

    fn active(&self, id: u64) -> bool {
        self.runs.get(&id).is_some_and(|r| r.session.state.active())
    }

    fn edit(&mut self, id: u64, changes: Changes) -> Result<Value> {
        let original = self.library.get(id)?;
        let mut game = original.clone();
        let cover_changed = changes.cover.is_some() || changes.clear_cover;
        let proton = changes.proton.clone();
        game.apply(changes)?;
        if let Some(value) = proton {
            game.settings.proton = Some(platform::resolve_proton(&value, &self.paths)?);
        } else if let Some(path) = &game.settings.proton
            && game.settings.proton != original.settings.proton
        {
            game.settings.proton = Some(platform::resolve_proton(
                path.to_str().context("无效的 Proton 路径")?,
                &self.paths,
            )?);
        }
        let effective = game.effective(&self.library);
        effective.settings.validate()?;
        let cancel_cover =
            cover_changed || game.name != original.name || game.executable != original.executable;
        self.library.unique(&game.executable, Some(id))?;
        let mut library = self.library.clone();
        *library.games.iter_mut().find(|g| g.id == id).unwrap() = game.clone();
        library.save(&self.paths.library())?;
        self.library = library;
        if cancel_cover {
            self.cancel_cover(id);
        }
        Ok(serde_json::to_value(effective)?)
    }

    pub async fn handle(&mut self, request: Request) -> Result<Value> {
        match request {
            Request::Lan | Request::SetLan { .. } => unreachable!("handled by IPC"),
            Request::List => Ok(serde_json::to_value(
                self.library
                    .games
                    .iter()
                    .map(|g| g.effective(&self.library))
                    .collect::<Vec<_>>(),
            )?),
            Request::CoverPath { id } => Ok(json!(self.library.get(id)?.cover)),
            Request::Library => Ok(serde_json::to_value(&self.library)?),
            Request::SetSettings { mut settings } => {
                settings.validate()?;
                if let Some(path) = &settings.proton
                    && settings.proton != self.library.settings.proton
                {
                    settings.proton = Some(platform::resolve_proton(
                        path.to_str().context("无效的 Proton 路径")?,
                        &self.paths,
                    )?);
                }
                let mut library = self.library.clone();
                library.settings = settings;
                library.save(&self.paths.library())?;
                self.library = library;
                Ok(serde_json::to_value(&self.library.settings)?)
            }
            Request::Inspect { directory } => Ok(serde_json::to_value(
                tokio::task::spawn_blocking(move || crate::scan::inspect(&directory)).await??,
            )?),
            Request::Add {
                executable,
                name,
                proton,
            } => {
                let proton = proton
                    .as_deref()
                    .map(|value| platform::resolve_proton(value, &self.paths))
                    .transpose()?;
                let mut library = self.library.clone();
                let game = library.add(&executable, name, proton)?;
                library.save(&self.paths.library())?;
                self.library = library;
                if game.cover.as_ref().is_none_or(|path| {
                    path.extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("ico"))
                }) {
                    let id = game.id;
                    let title = game.name.clone();
                    let data = self.paths.data.clone();
                    let job = self
                        .cover_jobs
                        .spawn(async move { (id, cover::find_automatic(&data, &title).await) });
                    self.pending_covers.insert(id, job);
                }
                Ok(serde_json::to_value(game.effective(&self.library))?)
            }
            Request::FindCovers { .. } | Request::DownloadCover { .. } => {
                bail!("封面查找仅支持本地连接")
            }
            Request::Edit { id, changes } => self.edit(id, *changes),
            Request::SetPicture {
                id,
                enabled,
                filter,
                output,
                fps,
            } => self.edit(
                id,
                Changes {
                    enabled,
                    filter,
                    output,
                    fps,
                    ..Default::default()
                },
            ),
            Request::Remove { id } => {
                self.library.get(id)?;
                ensure!(!self.active(id), "请先结束游戏会话，再移出游戏库");
                let mut library = self.library.clone();
                library.games.retain(|g| g.id != id);
                library.save(&self.paths.library())?;
                self.library = library;
                self.cancel_cover(id);
                // Prefixes and game files contain user saves and are retained.
                Ok(json!({ "removed": id }))
            }
            Request::Runtimes => Ok(serde_json::to_value(platform::protons(&self.paths))?),
            Request::CheckGeProton | Request::InstallGeProton { .. } => {
                unreachable!("handled by local IPC")
            }
            Request::Doctor { desktop } => {
                Ok(platform::doctor(&self.paths, &desktop.unwrap_or_else(Desktop::current)).await)
            }
            request @ (Request::Launch { .. } | Request::LaunchStream { .. }) => {
                let (id, desktop, auto_output) = match request {
                    Request::Launch { id, desktop } => {
                        (id, Some(desktop.unwrap_or_else(Desktop::current)), false)
                    }
                    Request::LaunchStream { id, auto_output } => {
                        (id, None, auto_output.unwrap_or_default())
                    }
                    _ => unreachable!(),
                };
                ensure!(!self.active(id), "游戏 {id} 已有运行中的会话");
                let mut game = self.library.get(id)?.effective(&self.library);
                let shared = game.prefix == Prefix::Shared;
                if shared {
                    ensure!(
                        !self.runs.values().any(|run| {
                            run.session.prefix == Prefix::Shared && run.session.state.active()
                        }),
                        "共享环境正在使用，请先结束该游戏，或为此游戏选择独立环境"
                    );
                    if let Some(proton) = &self.library.shared_proton {
                        ensure!(
                            game.settings.proton.as_ref() == Some(proton),
                            "共享环境使用 {}，请使用该版本或为此游戏选择独立环境",
                            proton.display()
                        );
                    }
                }
                if desktop.is_none() {
                    ensure!(self.streaming, "请先开启局域网服务");
                    stream_picture(&mut game.settings.enhancement, auto_output);
                }
                let streaming = self.streaming && game.settings.enhancement.enabled;
                let proton =
                    platform::selected_proton(game.settings.proton.as_deref(), &self.paths)?;
                let started_at = now();
                let scope = format!("galscaler-{}-{id}-{started_at}.scope", std::process::id());
                let log = self
                    .paths
                    .data
                    .join("logs")
                    .join(format!("{id}-{started_at}.log"));
                let mut command = platform::launch(
                    &game,
                    &proton,
                    &self.paths,
                    &scope,
                    &log,
                    desktop.as_ref(),
                    streaming,
                )?;
                if shared && self.library.shared_proton.is_none() {
                    let mut library = self.library.clone();
                    library.shared_proton = Some(proton);
                    library.save(&self.paths.library())?;
                    self.library = library;
                }
                let session = Session {
                    game_id: id,
                    state: State::Starting,
                    started_at,
                    finished_at: None,
                    exit_code: None,
                    log,
                    scope,
                    error: None,
                    prefix: game.prefix,
                    backend: if game.settings.enhancement.enabled {
                        "native"
                    } else {
                        "direct"
                    }
                    .into(),
                    metrics: None,
                    streaming,
                };
                // Persist the scope before spawning so a restarted engine can recover it.
                self.runs.insert(
                    id,
                    Run {
                        session: session.clone(),
                        child: None,
                    },
                );
                self.save_sessions()?;
                match command.spawn() {
                    Ok(child) => self.runs.get_mut(&id).unwrap().child = Some(child),
                    Err(error) => {
                        let run = self.runs.get_mut(&id).unwrap();
                        run.session.state = State::Failed;
                        run.session.finished_at = Some(now());
                        run.session.error = Some(error.to_string());
                        self.save_sessions()?;
                        return Err(error).context("无法启动游戏");
                    }
                }
                Ok(serde_json::to_value(session)?)
            }
            Request::Stream { id } => {
                ensure!(self.streaming, "请先开启局域网服务");
                let run = self
                    .runs
                    .get(&id)
                    .filter(|run| matches!(run.session.state, State::Starting | State::Running))
                    .context("请先启动游戏")?;
                ensure!(run.session.streaming, "该会话未开启局域网串流");
                Ok(json!(run.session.log.with_extension("stream")))
            }
            Request::Stop { id } => {
                ensure!(self.active(id), "游戏 {id} 当前没有运行中的会话");
                let run = self.runs.get_mut(&id).unwrap();
                platform::stop_scope(&run.session.scope).await?;
                run.session.state = State::Stopping;
                let session = run.session.clone();
                self.save_sessions()?;
                Ok(serde_json::to_value(session)?)
            }
            Request::Status => Ok(json!({
                "pid": std::process::id(), "data_dir": self.paths.data,
                "games": self.library.games.len(),
                "sessions": self.runs.values().map(|r| &r.session).collect::<Vec<_>>(),
            })),
            Request::Log { id, lines } => {
                ensure!((1..=1000).contains(&lines), "日志行数必须在 1–1000 之间");
                let run = self.runs.get(&id).context("这款游戏还没有启动记录")?;
                let mut file = fs::File::open(&run.session.log).context("无法读取游戏日志")?;
                let start = file.metadata()?.len().saturating_sub(64 * 1024);
                file.seek(SeekFrom::Start(start))?;
                let mut bytes = Vec::new();
                file.take(64 * 1024).read_to_end(&mut bytes)?;
                let text = String::from_utf8_lossy(&bytes);
                let mut tail: Vec<_> = text.lines().rev().take(lines).collect();
                tail.reverse();
                Ok(json!({ "path": run.session.log, "text": tail.join("\n") }))
            }
            Request::Shutdown => {
                self.shutdown().await?;
                Ok(json!({ "stopped": true }))
            }
        }
    }

    pub async fn disconnect_streams(&self) {
        let mut streams = JoinSet::new();
        for run in self.runs.values() {
            if run.session.streaming && run.session.state.active() {
                streams.spawn(crate::stream::disconnect(
                    run.session.log.with_extension("stream"),
                ));
            }
        }
        while streams.join_next().await.is_some() {}
    }

    pub async fn refresh(&mut self) -> Result<()> {
        let mut changed = false;
        for run in self.runs.values_mut().filter(|r| r.session.state.active()) {
            if run.session.backend == "native"
                && let Ok(bytes) = fs::read(run.session.log.with_extension("json"))
            {
                let metrics: crate::render::Metrics = serde_json::from_slice(&bytes)?;
                if run.session.metrics.as_ref() != Some(&metrics) {
                    run.session.metrics = Some(metrics);
                    changed = true;
                }
            }
            if run.session.state == State::Stopping {
                platform::stop_scope(&run.session.scope).await?;
            }
            if let Some(child) = &mut run.child
                && let Some(status) = child.try_wait()?
            {
                let requested = run.session.state == State::Stopping;
                run.session.exit_code = status.code();
                if !status.success() && !requested {
                    run.session.error = Some(format!("启动程序退出：{status}；请查看游戏日志"));
                }
                run.session.state = State::Stopping;
                run.child = None;
                // Wine/container descendants can outlive the launcher's main process.
                platform::stop_scope(&run.session.scope).await?;
                changed = true;
            }
            if run.session.state == State::Starting || run.child.is_none() {
                if platform::scope_active(&run.session.scope).await? {
                    if run.session.state == State::Starting
                        && (run.session.backend != "native" || run.session.metrics.is_some())
                    {
                        run.session.state = State::Running;
                        changed = true;
                    }
                } else if run.child.is_none() {
                    run.session.state = if run.session.state == State::Stopping {
                        if run.session.error.is_some() {
                            State::Failed
                        } else {
                            State::Exited
                        }
                    } else {
                        State::Interrupted
                    };
                    run.session.finished_at = Some(now());
                    if run.session.backend == "native" {
                        for extension in ["client", "client.tmp", "stream"] {
                            let _ = fs::remove_file(run.session.log.with_extension(extension));
                        }
                    }
                    if run.session.state == State::Interrupted {
                        run.session.error = Some("会话在引擎离线期间结束，无法取得退出码".into());
                    }
                    changed = true;
                }
            }
        }
        if changed {
            self.save_sessions()?;
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        for run in self.runs.values_mut().filter(|r| r.session.state.active()) {
            platform::stop_scope(&run.session.scope).await?;
            run.session.state = State::Stopping;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while self.runs.values().any(|r| r.session.state.active()) {
            self.refresh().await?;
            if tokio::time::Instant::now() >= deadline {
                bail!("仍有游戏进程未退出，引擎将保留会话记录供恢复");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.save_sessions()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_output_keeps_the_game_mode_and_uses_the_stream_ceiling() {
        let original = Enhancement {
            enabled: false,
            filter: Filter::Anime4k,
            ..Enhancement::default()
        };

        let mut auto = original.clone();
        stream_picture(&mut auto, true);
        assert!(auto.enabled);
        assert_eq!(auto.filter, Filter::Linear);
        assert_eq!(auto.source, original.source);
        assert_eq!(auto.output, stream::MAX_RESOLUTION);

        let mut enhanced = Enhancement {
            enabled: true,
            ..original.clone()
        };
        stream_picture(&mut enhanced, true);
        assert_eq!(enhanced.filter, Filter::Anime4k);
        assert_eq!(enhanced.output, stream::MAX_RESOLUTION);

        let mut legacy = original;
        stream_picture(&mut legacy, false);
        assert_eq!(legacy.output, legacy.source);
    }

    #[tokio::test]
    async fn edits_are_transactional_and_removing_a_game_retains_its_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let exe = dir.path().join("游戏.exe");
        fs::write(&exe, b"MZ")?;
        let paths = Paths {
            data: dir.path().join("data"),
            socket: dir.path().join("engine.sock"),
        };
        let mut engine = Engine::open(paths.clone(), false)?;
        engine
            .handle(Request::Add {
                executable: exe.clone(),
                name: Some("游戏".into()),
                proton: None,
            })
            .await?;
        engine
            .handle(Request::Edit {
                id: 1,
                changes: Box::new(Changes {
                    name: Some("新版名称".into()),
                    ..Default::default()
                }),
            })
            .await?;
        assert!(
            engine
                .handle(Request::Edit {
                    id: 1,
                    changes: Box::new(Changes {
                        name: Some("不应保存".into()),
                        fps: Some(0),
                        ..Default::default()
                    }),
                })
                .await
                .is_err()
        );
        engine
            .handle(Request::SetPicture {
                id: 1,
                enabled: None,
                filter: Some(Filter::Anime4k),
                output: Some("1920x1080".parse()?),
                fps: None,
            })
            .await?;
        assert!(
            engine
                .handle(Request::SetPicture {
                    id: 1,
                    enabled: Some(false),
                    filter: None,
                    output: None,
                    fps: Some(0),
                })
                .await
                .is_err()
        );
        drop(engine);
        let mut engine = Engine::open(paths.clone(), false)?;
        let games = engine.handle(Request::List).await?;
        assert_eq!(games[0]["name"], "新版名称");
        assert_eq!(games[0]["enhancement"]["filter"], "anime4k");
        assert_eq!(
            games[0]["enhancement"]["output"],
            json!({ "width": 1920, "height": 1080 })
        );
        assert_eq!(games[0]["enhancement"]["enabled"], true);
        assert_eq!(engine.library.get(1)?.settings.enhancement.fps, None);
        let prefix = paths.prefix(1);
        fs::create_dir_all(&prefix)?;
        fs::write(prefix.join("save.dat"), b"save")?;
        engine.handle(Request::Remove { id: 1 }).await?;
        assert!(exe.exists());
        assert_eq!(fs::read(prefix.join("save.dat"))?, b"save");
        assert_eq!(engine.handle(Request::List).await?, json!([]));
        Ok(())
    }
}
