//! Steam owns authentication and content transfer; discovery metadata has its own adapter.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    io::Read,
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use steamworks::{
    AppIDs, AppId, Client, ItemState, PublishedFileId, QueryHandle, UGCQueryType, UGCType,
    UserList, UserListOrder,
};

const APP: AppId = AppId(431960);
const WAIT: Duration = Duration::from_secs(20);
type Reply = Result<Value, String>;
struct Message {
    action: Value,
    reply: mpsc::Sender<Reply>,
}
static WORKER: OnceLock<mpsc::Sender<Message>> = OnceLock::new();

/// All SDK calls stay on one worker. Call from an asynchronous UI task.
pub fn execute(action: Value) -> Reply {
    validate_action(&action)?;
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Message>();
        thread::spawn(move || {
            let mut service = Steam::new();
            loop {
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(message) => {
                        let _ = message.reply.send(service.execute(message.action));
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => (),
                }
                service.tick();
            }
        });
        tx
    });
    let (tx, rx) = mpsc::channel();
    worker
        .send(Message { action, reply: tx })
        .map_err(|_| "Steam 服务已停止，请重新打开应用。".to_string())?;
    rx.recv_timeout(Duration::from_secs(90))
        .map_err(|_| "Steam 响应超时，请检查 Steam 客户端连接后重试。".to_string())?
}

fn validate_action(action: &Value) -> Result<(), String> {
    if action["text"]
        .as_str()
        .is_some_and(|text| text.len() > 1024 || text.contains('\0'))
    {
        return Err("搜索文字过长或包含无效字符。".into());
    }
    if action["page"]
        .as_u64()
        .is_some_and(|page| page == 0 || page > 10000)
    {
        return Err("页码必须在 1 到 10000 之间。".into());
    }
    Ok(())
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}
fn queue_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("misari/workshop-queue.json")
}
fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
        .join("misari/workshop-previews")
}
fn item_id(action: &Value) -> Result<PublishedFileId, String> {
    action["id"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| action["id"].as_u64())
        .filter(|id| *id > 0)
        .map(PublishedFileId)
        .ok_or_else(|| "创意工坊项目编号无效。".into())
}

#[derive(Clone, Serialize, Deserialize)]
struct Job {
    id: String,
    title: String,
    status: String,
    #[serde(default)]
    downloaded: u64,
    #[serde(default)]
    total: u64,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    account_id: String,
    #[serde(default)]
    needs_update: bool,
}

fn observe_completed(job: &mut Job, state: ItemState) -> bool {
    if !matches!(job.status.as_str(), "completed" | "update_available")
        || state.intersects(ItemState::DOWNLOADING | ItemState::DOWNLOAD_PENDING)
    {
        return false;
    }
    if job.needs_update {
        job.status = "update_available".into();
    } else if state.contains(ItemState::INSTALLED) {
        job.status = "completed".into();
    } else {
        job.status = "error".into();
        job.error = Some("本地下载内容已被移除，请重试以重新下载。".into());
    }
    true
}

fn read_jobs(path: &std::path::Path) -> Result<Vec<Job>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("队列文件超过 4 MiB".into());
    }
    let jobs: Vec<Job> = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if jobs.len() > 10000
        || jobs
            .iter()
            .any(|job| job.id.parse::<u64>().ok().is_none_or(|id| id == 0))
    {
        return Err("队列条目过多或包含无效项目编号".into());
    }
    Ok(jobs)
}

struct Steam {
    client: Option<Client>,
    error: Option<String>,
    jobs: Vec<Job>,
    items: BTreeMap<String, Value>,
    scope_cache: BTreeMap<String, (Instant, Vec<Value>)>,
    account_id: u64,
    favorites: BTreeSet<String>,
    favorites_loaded: bool,
    favorites_checked: Option<Instant>,
    downloads_paused: bool,
    read_only: bool,
    dirty: bool,
    queue_damaged: bool,
    last_save: Instant,
    last_init: Instant,
    download_results: mpsc::Receiver<steamworks::DownloadItemResult>,
    _download_callback: Option<steamworks::CallbackHandle>,
}

impl Steam {
    fn new() -> Self {
        let (tx, download_results) = mpsc::channel();
        let mut error = None;
        // main sets SteamAppId/SteamGameId before it starts any worker threads.
        let client = match Client::init() {
            Ok(client) => Some(client),
            Err(reason) => {
                error = Some(format!(
                    "请打开 Steam 并登录拥有 Wallpaper Engine 的账号：{reason}"
                ));
                None
            }
        };
        let callback = client.as_ref().map(|client| {
            client.register_callback(move |result: steamworks::DownloadItemResult| {
                let _ = tx.send(result);
            })
        });
        let (mut jobs, queue_damaged) = match read_jobs(&queue_path()) {
            Ok(jobs) => (jobs, false),
            Err(reason) => {
                error = Some(format!(
                    "原下载记录无法读取：{reason}。原文件保持完整，保存新任务时会先备份。"
                ));
                (Vec::new(), true)
            }
        };
        for job in &mut jobs {
            if matches!(job.status.as_str(), "downloading" | "updating") {
                job.status = "queued".into();
            }
        }
        let read_only = std::env::args().any(|arg| arg == "--diagnose")
            || std::env::var("MISARI_READ_ONLY").as_deref() == Ok("1");
        let current_account = client
            .as_ref()
            .map(|client| client.user().steam_id().raw().to_string());
        let downloads_paused = jobs.iter().any(|job| {
            job.status == "paused"
                && (job.account_id.is_empty() || Some(&job.account_id) == current_account.as_ref())
        });
        if downloads_paused
            && !read_only
            && let Some(client) = &client
        {
            client.ugc().suspend_downloads(true);
        }
        Self {
            client,
            error,
            jobs,
            items: BTreeMap::new(),
            scope_cache: BTreeMap::new(),
            account_id: 0,
            favorites: BTreeSet::new(),
            favorites_loaded: false,
            favorites_checked: None,
            downloads_paused,
            read_only,
            dirty: false,
            queue_damaged,
            last_save: Instant::now(),
            last_init: Instant::now(),
            download_results,
            _download_callback: callback,
        }
    }

    fn sdk(&self) -> Result<Client, String> {
        let client = self.client.clone().ok_or_else(|| {
            self.error
                .clone()
                .unwrap_or_else(|| "Steam 尚未连接".into())
        })?;
        if !client.user().logged_on() {
            return Err("Steam 当前离线。请在 Steam 中登录后重试。".into());
        }
        if !client.apps().is_subscribed_app(APP) {
            return Err(
                "当前 Steam 账号没有 Wallpaper Engine 使用许可。请切换到拥有该产品的账号。".into(),
            );
        }
        Ok(client)
    }

    fn execute(&mut self, action: Value) -> Reply {
        if self.read_only
            && !matches!(
                action["action"].as_str().unwrap_or("status"),
                "status" | "search" | "detail" | "explore"
            )
        {
            return Err("当前实例为只读验证模式，Steam 账号和下载状态不会被修改。".into());
        }
        if let Some(client) = &self.client {
            let account_id = client.user().steam_id().raw();
            if self.account_id != account_id {
                self.account_id = account_id;
                self.scope_cache.clear();
                self.favorites.clear();
                self.favorites_loaded = false;
                self.favorites_checked = None;
            }
        }
        match action["action"].as_str().unwrap_or("status") {
            "status" => self.status(),
            "explore" => self.explore(),
            "search" => {
                let mut result = if action["source"] == "discover" {
                    Discovery::search(&action)?
                } else {
                    self.search(&action)?
                };
                self.items.clear();
                if let Some(items) = result["items"].as_array_mut() {
                    for item in items {
                        self.decorate(item);
                        if let Some(id) = item["id"].as_str() {
                            self.items.insert(id.to_string(), item.clone());
                        }
                    }
                }
                Ok(result)
            }
            "detail" => {
                let id = item_id(&action)?;
                let client = self.sdk()?;
                let query = client
                    .ugc()
                    .query_item(id)
                    .map_err(|e| e.to_string())?
                    .set_return_long_description(true);
                let mut response = fetch(&client, query, 1)?;
                let mut item = response["items"]
                    .as_array_mut()
                    .and_then(|items| items.pop())
                    .ok_or("该项目已删除、设为私有，或不属于 Wallpaper Engine。")?;
                self.decorate(&mut item);
                self.items.insert(id.0.to_string(), item.clone());
                Ok(json!({"item":item}))
            }
            "subscribe" => {
                let client = self.sdk()?;
                let id = item_id(&action)?;
                let value = action["value"].as_bool().unwrap_or(true);
                let (tx, rx) = mpsc::channel();
                if value {
                    client.ugc().subscribe_item(id, move |result| {
                        let _ = tx.send(result.map(|_| ()).map_err(|e| e.to_string()));
                    });
                } else {
                    client.ugc().unsubscribe_item(id, move |result| {
                        let _ = tx.send(result.map(|_| ()).map_err(|e| e.to_string()));
                    });
                }
                wait(&client, rx)?;
                self.scope_cache.clear();
                if value {
                    self.enqueue(id, &action)?;
                } else if let Some(job) =
                    self.jobs.iter_mut().find(|job| job.id == id.0.to_string())
                {
                    job.status = "cancelled".into();
                    job.error = None;
                    self.save()?;
                }
                let mut status = self.status()?;
                status["message"] = json!(if value {
                    "已订阅，Steam 将下载内容并保持更新。"
                } else {
                    "已取消 Steam 订阅，本地已有文件保留。"
                });
                Ok(status)
            }
            "favorite" => {
                let client = self.sdk()?;
                let id = item_id(&action)?;
                let value = action["value"].as_bool().unwrap_or(true);
                favorite(&client, id, value)?;
                self.scope_cache.clear();
                if value {
                    self.favorites.insert(id.0.to_string());
                } else {
                    self.favorites.remove(&id.0.to_string());
                }
                let mut status = self.status()?;
                status["message"] = json!(if value {
                    "已添加到 Steam 收藏。"
                } else {
                    "已从 Steam 收藏移除。"
                });
                Ok(status)
            }
            "enqueue" | "retry" => {
                let id = item_id(&action)?;
                self.enqueue(id, &action)?;
                self.status()
            }
            "pause" => {
                let client = self.sdk()?;
                // The Steam SDK only offers application-wide transfer suspension.
                client.ugc().suspend_downloads(true);
                self.downloads_paused = true;
                for job in &mut self.jobs {
                    if matches!(job.status.as_str(), "queued" | "downloading" | "updating") {
                        job.status = "paused".into();
                    }
                }
                self.save()?;
                let mut status = self.status()?;
                status["message"] =
                    json!("已暂停全部 Wallpaper Engine 创意工坊下载；Steam 的其他游戏不受影响。");
                Ok(status)
            }
            "resume" => {
                let client = self.sdk()?;
                client.ugc().suspend_downloads(false);
                self.downloads_paused = false;
                for job in &mut self.jobs {
                    if job.status == "paused" {
                        job.status = "queued".into();
                    }
                }
                self.save()?;
                self.tick();
                let mut status = self.status()?;
                status["message"] = json!("已恢复全部 Wallpaper Engine 创意工坊下载。");
                Ok(status)
            }
            "cancel" => {
                let id = item_id(&action)?.0.to_string();
                let client = self.sdk()?;
                let state = client
                    .ugc()
                    .item_state(PublishedFileId(id.parse().unwrap()));
                if state.intersects(ItemState::DOWNLOADING | ItemState::DOWNLOAD_PENDING) {
                    return Err("Steam 无法单独取消正在传输的项目。可以暂停全部 Wallpaper Engine 下载，或在 Steam 中取消订阅。".into());
                }
                let job = self
                    .jobs
                    .iter_mut()
                    .find(|j| j.id == id)
                    .ok_or("队列中没有这个项目。")?;
                job.status = "cancelled".into();
                self.save()?;
                self.status()
            }
            _ => Err("未知的 Steam 操作。".into()),
        }
    }

    fn enqueue(&mut self, id: PublishedFileId, action: &Value) -> Result<(), String> {
        let _ = self.sdk()?;
        let key = id.0.to_string();
        let title = action["title"]
            .as_str()
            .or_else(|| self.items.get(&key).and_then(|v| v["title"].as_str()))
            .unwrap_or(&key)
            .to_string();
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == key) {
            job.status = if self.downloads_paused {
                "paused"
            } else {
                "queued"
            }
            .into();
            job.error = None;
            job.account_id = self.account_id.to_string();
        } else {
            if self.jobs.len() >= 10000 {
                return Err("下载队列已满，请清理已完成项目。".into());
            }
            self.jobs.push(Job {
                id: key,
                title,
                status: if self.downloads_paused {
                    "paused"
                } else {
                    "queued"
                }
                .into(),
                downloaded: 0,
                total: 0,
                error: None,
                account_id: self.account_id.to_string(),
                needs_update: false,
            });
        }
        self.save()?;
        self.tick();
        Ok(())
    }

    fn tick(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        client.run_callbacks();
        while let Ok(result) = self.download_results.try_recv() {
            if result.app_id != APP {
                continue;
            }
            if let Some(job) = self
                .jobs
                .iter_mut()
                .find(|j| j.id == result.published_file_id.0.to_string())
                && let Some(error) = result.error
            {
                job.status = "error".into();
                job.error = Some(error.to_string());
                self.dirty = true;
            }
        }
        let ugc = client.ugc();
        let active = self
            .jobs
            .iter()
            .any(|job| job.status == "downloading" || job.status == "updating");
        let mut may_start = !active
            && !self.downloads_paused
            && !self.read_only
            && client.user().logged_on()
            && client.apps().is_subscribed_app(APP);
        for job in &mut self.jobs {
            if matches!(job.status.as_str(), "cancelled" | "error") {
                continue;
            }
            if !job.account_id.is_empty()
                && job.account_id != client.user().steam_id().raw().to_string()
            {
                job.status = "paused".into();
                job.error = Some("此任务属于另一 Steam 账号。切换回原账号后恢复下载。".into());
                continue;
            }
            let Ok(id) = job.id.parse::<u64>() else {
                continue;
            };
            let id = PublishedFileId(id);
            let state = ugc.item_state(id);
            let old_update = job.needs_update;
            job.needs_update = state.contains(ItemState::NEEDS_UPDATE);
            self.dirty |= old_update != job.needs_update;
            let old_status = job.status.clone();
            if observe_completed(job, state) {
                self.dirty |= old_status != job.status;
                continue;
            }
            let previous = (job.status.clone(), job.downloaded, job.total);
            if let Some((done, total)) = ugc.item_download_info(id) {
                job.downloaded = done;
                job.total = total;
            }
            if state.contains(ItemState::INSTALLED)
                && !state.intersects(
                    ItemState::NEEDS_UPDATE | ItemState::DOWNLOADING | ItemState::DOWNLOAD_PENDING,
                )
            {
                job.status = "completed".into();
                // Steam marks an item as used when reading its installation info.
                // Read-only diagnostics retain observed/known transfer totals.
                if !self.read_only
                    && let Some(info) = ugc.item_install_info(id)
                {
                    job.total = info.size_on_disk;
                    job.downloaded = info.size_on_disk;
                }
            } else if self.downloads_paused || job.status == "paused" {
                job.status = "paused".into();
            } else if state.intersects(ItemState::DOWNLOADING | ItemState::DOWNLOAD_PENDING) {
                job.status = if state.contains(ItemState::NEEDS_UPDATE) {
                    "updating"
                } else {
                    "downloading"
                }
                .into();
            } else if may_start && job.status == "queued" {
                may_start = false;
                if ugc.download_item(id, false) {
                    job.status = "downloading".into();
                } else {
                    job.status = "error".into();
                    job.error = Some(
                        "Steam 未能开始下载。请检查登录状态、磁盘空间和项目访问权限，然后重试。"
                            .into(),
                    );
                }
            }
            self.dirty |= previous != (job.status.clone(), job.downloaded, job.total);
        }
        if self.dirty
            && !self.read_only
            && self.last_save.elapsed() > Duration::from_secs(2)
            && let Err(error) = self.save()
        {
            self.error = Some(error);
            self.last_save = Instant::now();
        }
    }

    fn save(&mut self) -> Result<(), String> {
        let path = queue_path();
        fs::create_dir_all(path.parent().unwrap()).map_err(|e| format!("无法保存下载队列：{e}"))?;
        if self.queue_damaged && path.exists() {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let backup = path.with_extension(format!("backup-{timestamp}.json"));
            fs::copy(&path, &backup)
                .map_err(|error| format!("无法备份原下载记录，已阻止覆盖：{error}"))?;
            self.error = Some(format!("原下载记录已备份至 {}。", backup.display()));
        }
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.jobs).map_err(|e| e.to_string())?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("下载记录超过 4 MiB，无法继续保存；原队列保持完整。".into());
        }
        fs::write(&temporary, bytes)
            .and_then(|_| fs::rename(&temporary, &path))
            .map_err(|e| format!("无法保存下载队列：{e}"))?;
        self.dirty = false;
        self.queue_damaged = false;
        self.last_save = Instant::now();
        Ok(())
    }

    fn decorate(&self, item: &mut Value) {
        let id = item["id"]
            .as_str()
            .and_then(|id| id.parse::<u64>().ok())
            .unwrap_or_default();
        let state = self
            .client
            .as_ref()
            .map(|c| c.ugc().item_state(PublishedFileId(id)))
            .unwrap_or(ItemState::NONE);
        item["subscribed"] = json!(state.contains(ItemState::SUBSCRIBED));
        item["downloaded"] = json!(state.contains(ItemState::INSTALLED));
        item["needs_update"] = json!(state.contains(ItemState::NEEDS_UPDATE));
        item["steam_favorite"] = json!(self.favorites.contains(&id.to_string()));
        if let Some(url) = item["preview_url"].as_str() {
            item["thumbnail"] = json!(cached_preview(id, url));
        }
    }

    fn status(&mut self) -> Reply {
        // Allow a user who logs into Steam after opening Misari to reconnect.
        if self.client.is_none() && self.last_init.elapsed() > Duration::from_secs(10) {
            self.last_init = Instant::now();
            if let Ok(client) = Client::init() {
                let (tx, rx) = mpsc::channel();
                self._download_callback = Some(client.register_callback(
                    move |result: steamworks::DownloadItemResult| {
                        let _ = tx.send(result);
                    },
                ));
                self.download_results = rx;
                self.client = Some(client);
                self.error = None;
            }
        }
        self.tick();
        if (!self.favorites_loaded
            || self
                .favorites_checked
                .is_some_and(|when| when.elapsed() >= Duration::from_secs(120)))
            && self.sdk().is_ok()
            && self
                .favorites_checked
                .is_none_or(|when| when.elapsed() >= Duration::from_secs(30))
        {
            self.favorites_checked = Some(Instant::now());
            match self.load_favorites() {
                Ok(()) => {
                    self.favorites_loaded = true;
                    if self
                        .error
                        .as_deref()
                        .is_some_and(|message| message.starts_with("Steam 收藏暂时"))
                    {
                        self.error = None;
                    }
                }
                Err(error) => {
                    self.favorites_loaded = false;
                    self.error = Some(format!("Steam 收藏暂时无法同步，将自动重试：{error}"));
                }
            }
        }
        let (account, subscriptions) = match &self.client {
            Some(client) => (
                json!({"name":client.friends().name(),"id":client.user().steam_id().raw().to_string(),"logged_in":client.user().logged_on(),
                    "owns_wallpaper_engine":client.apps().is_subscribed_app(APP),"app_installed":client.apps().is_app_installed(APP),
                    "error":if client.apps().is_subscribed_app(APP){self.error.clone()}else{Some("当前 Steam 账号没有 Wallpaper Engine 使用许可。".into())}}),
                client
                    .ugc()
                    .subscribed_items(false)
                    .iter()
                    .map(|id| id.0.to_string())
                    .collect::<Vec<_>>(),
            ),
            None => (
                json!({"name":"Steam 未连接","id":null,"logged_in":false,"error":self.error}),
                Vec::new(),
            ),
        };
        let mut item_states = BTreeMap::new();
        if let Some(client) = &self.client {
            let ugc = client.ugc();
            for id in self.items.keys().chain(subscriptions.iter()) {
                if let Ok(raw) = id.parse::<u64>() {
                    let state = ugc.item_state(PublishedFileId(raw));
                    item_states.insert(id.clone(), json!({"subscribed":state.contains(ItemState::SUBSCRIBED),
                        "downloaded":state.contains(ItemState::INSTALLED),"needs_update":state.contains(ItemState::NEEDS_UPDATE),
                        "steam_favorite":self.favorites.contains(id)}));
                }
            }
        }
        Ok(
            json!({"account":account,"jobs":self.jobs,"subscriptions":subscriptions,"favorites":self.favorites,"item_states":item_states,
            "downloads_paused":self.downloads_paused,"pause_scope":"wallpaper_engine","per_item_cancel":false}),
        )
    }

    fn load_favorites(&mut self) -> Result<(), String> {
        let client = self.sdk()?;
        let mut favorites = BTreeSet::new();
        for page in 1..=200 {
            let query = client
                .ugc()
                .query_user(
                    client.user().steam_id().account_id(),
                    UserList::Favorited,
                    UGCType::Items,
                    UserListOrder::CreationOrderDesc,
                    AppIDs::ConsumerAppId(APP),
                    page,
                )
                .map_err(|e| e.to_string())?;
            let result = fetch(&client, query, page)?;
            for item in result["items"].as_array().into_iter().flatten() {
                if let Some(id) = item["id"].as_str() {
                    favorites.insert(id.to_string());
                }
            }
            if page as u64 >= result["total_pages"].as_u64().unwrap_or(1) {
                break;
            }
        }
        self.favorites = favorites;
        Ok(())
    }

    fn search(&mut self, action: &Value) -> Reply {
        let client = self.sdk()?;
        if let Some(scope @ ("已订阅" | "Steam 收藏")) = action["scope"].as_str() {
            return self.search_user(&client, scope, action);
        }
        let page = action["page"].as_u64().unwrap_or(1) as u32;
        let sort_name = sort_name(action["sort"].as_str().unwrap_or("trend_week"));
        let sort = match sort_name {
            "most_recent" | "recent" => UGCQueryType::RankedByPublicationDate,
            "updated" => UGCQueryType::RankedByLastUpdatedDate,
            "top_rated" | "rating" => UGCQueryType::RankedByVote,
            "subscriptions" => UGCQueryType::RankedByTotalUniqueSubscriptions,
            "relevance" => UGCQueryType::RankedByTextSearch,
            _ => UGCQueryType::RankedByTrend,
        };
        let mut query = client
            .ugc()
            .query_all(sort, UGCType::Items, AppIDs::ConsumerAppId(APP), page)
            .map_err(|e| e.to_string())?
            .set_return_long_description(true)
            .set_allow_cached_response(60);
        if let Some(text) = action["text"].as_str().filter(|s| !s.is_empty()) {
            query = query.set_search_text(text);
        }
        if let Some(kind) = tag(action["kind"].as_str().unwrap_or("all")) {
            query = query.add_required_tag(kind);
        }
        if sort == UGCQueryType::RankedByTrend {
            query = query.set_ranked_by_trend_days(match sort_name {
                "trend_day" => 1,
                "trend_month" => 30,
                "trend_year" => 365,
                _ => 7,
            });
        }
        fetch(&client, query, page)
    }

    fn explore(&mut self) -> Reply {
        let mut response = Discovery::explore()?;
        response["items"] = json!([]);
        let mut ids = BTreeSet::new();
        let covers = response["collections"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|collection| {
                let id = collection["coversubmission"]
                    .as_str()
                    .and_then(|id| id.parse::<u64>().ok())
                    .or_else(|| collection["coversubmission"].as_u64())?;
                (id > 0 && ids.insert(id)).then_some(PublishedFileId(id))
            })
            .take(6)
            .collect::<Vec<_>>();
        if covers.is_empty() {
            return Ok(response);
        }
        let result = self.sdk().and_then(|client| {
            let query = client
                .ugc()
                .query_items(covers)
                .map_err(|error| error.to_string())?;
            fetch(&client, query, 1)
        });
        match result {
            Ok(mut results) => {
                if let Some(items) = results["items"].as_array_mut() {
                    for item in items {
                        self.decorate(item);
                        if let Some(collection) = response["collections"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|collection| {
                                collection["coversubmission"].as_str() == item["id"].as_str()
                            })
                        {
                            item["recommendation"] = collection.clone();
                        }
                        if let Some(id) = item["id"].as_str() {
                            self.items.insert(id.to_owned(), item.clone());
                        }
                    }
                }
                response["items"] = results["items"].take();
            }
            Err(error) => response["recommendations_error"] = json!(error),
        }
        Ok(response)
    }

    fn search_user(&mut self, client: &Client, scope: &str, action: &Value) -> Reply {
        let key = format!("{}:{scope}", client.user().steam_id().raw());
        if self
            .scope_cache
            .get(&key)
            .is_none_or(|(when, _)| when.elapsed() > Duration::from_secs(60))
        {
            let list = if scope == "已订阅" {
                UserList::Subscribed
            } else {
                UserList::Favorited
            };
            let deadline = Instant::now() + Duration::from_secs(45);
            let mut items = Vec::new();
            for page in 1..=200 {
                let query = client
                    .ugc()
                    .query_user(
                        client.user().steam_id().account_id(),
                        list,
                        UGCType::Items,
                        UserListOrder::SubscriptionDateDesc,
                        AppIDs::ConsumerAppId(APP),
                        page,
                    )
                    .map_err(|error| error.to_string())?
                    .set_return_long_description(true);
                let result = fetch(client, query, page)?;
                let total_pages = result["total_pages"].as_u64().unwrap_or(1);
                if total_pages > 200 {
                    return Err(
                        "Steam 个人列表超过 10000 个项目，请先在 Steam 中整理该列表。".into(),
                    );
                }
                items.extend(result["items"].as_array().into_iter().flatten().cloned());
                if page as u64 >= total_pages {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(
                        "Steam 个人列表同步时间较长，请稍后重试。没有显示不完整的搜索结果。".into(),
                    );
                }
            }
            if scope == "Steam 收藏" {
                self.favorites = items
                    .iter()
                    .filter_map(|item| item["id"].as_str().map(str::to_owned))
                    .collect();
                self.favorites_loaded = true;
                self.favorites_checked = Some(Instant::now());
            }
            self.scope_cache
                .insert(key.clone(), (Instant::now(), items));
        }
        let (_, items) = self.scope_cache.get(&key).unwrap();
        Ok(user_page(items, action))
    }
}

fn user_page(items: &[Value], action: &Value) -> Value {
    let text = action["text"].as_str().unwrap_or_default().to_lowercase();
    let required_kind = tag(action["kind"].as_str().unwrap_or_default());
    let mut items = items
        .iter()
        .filter(|item| {
            let matches_text = text.is_empty()
                || item["title"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&text)
                || item["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&text);
            let matches_kind = required_kind.is_none_or(|required| {
                item["tags"].as_array().into_iter().flatten().any(|tag| {
                    tag.as_str()
                        .is_some_and(|tag| tag.eq_ignore_ascii_case(required))
                })
            });
            matches_text && matches_kind
        })
        .cloned()
        .collect::<Vec<_>>();
    match sort_name(action["sort"].as_str().unwrap_or_default()) {
        "most_recent" | "updated" => {
            items.sort_by_key(|item| std::cmp::Reverse(item["updated"].as_u64().unwrap_or(0)))
        }
        "subscriptions" => items.sort_by_key(|item| {
            std::cmp::Reverse(item["subscription_count"].as_u64().unwrap_or(0))
        }),
        "trend_week" | "trend_day" | "trend_month" | "trend_year" => {
            items.sort_by_key(|item| std::cmp::Reverse(item["votes"].as_u64().unwrap_or(0)))
        }
        "top_rated" => items.sort_by(|left, right| {
            right["score"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&left["score"].as_f64().unwrap_or(0.0))
        }),
        _ => (),
    }
    let total = items.len();
    let total_pages = total.div_ceil(50).max(1);
    let page = (action["page"].as_u64().unwrap_or(1) as usize).min(total_pages);
    let items = items
        .into_iter()
        .skip((page - 1) * 50)
        .take(50)
        .collect::<Vec<_>>();
    json!({"items":items,"total":total,"page":page,"total_pages":total_pages,"source":"steam","scope":action["scope"]})
}

fn wait<T>(client: &Client, rx: mpsc::Receiver<Result<T, String>>) -> Result<T, String> {
    let deadline = Instant::now() + WAIT;
    loop {
        client.run_callbacks();
        match rx.try_recv() {
            Ok(value) => return value,
            Err(mpsc::TryRecvError::Disconnected) => return Err("Steam 操作意外结束".into()),
            _ => (),
        }
        if Instant::now() >= deadline {
            return Err("Steam 响应超时，请检查连接后重试。".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn fetch(client: &Client, query: QueryHandle, page: u32) -> Reply {
    let (tx, rx) = mpsc::channel();
    query.fetch(move |result|{
        let value=result.map_err(|e|e.to_string()).map(|results|{
            let items=(0..results.returned_results()).filter_map(|index|{
                let item=results.get(index)?;
                if item.consumer_app_id!=Some(APP){return None;}
                let kind=kind_from_tags(&item.tags);
                Some(json!({"id":item.published_file_id.0.to_string(),"title":item.title,"description":item.description,
                    "preview_url":results.preview_url(index),"kind":kind,"tags":item.tags,"author_id":item.owner.raw().to_string(),
                    "size":item.file_size,"created":item.time_created,"updated":item.time_updated,"votes":item.num_upvotes,"score":item.score,
                    "subscription_count":results.statistic(index, steamworks::UGCStatisticType::Subscriptions)}))
            }).collect::<Vec<_>>();
            json!({"items":items,"page":page,"total_pages":results.total_results().div_ceil(50).max(1),"total":results.total_results(),"source":"steam"})
        });let _=tx.send(value);
    });
    wait(client, rx)
}

fn tag(kind: &str) -> Option<&'static str> {
    match kind.to_lowercase().as_str() {
        "scene" | "we_scene" | "场景" => Some("Scene"),
        "video" | "视频" => Some("Video"),
        "web" | "we_web" | "网页" => Some("Web"),
        "application" | "we_application" | "应用" => Some("Application"),
        _ => None,
    }
}
fn sort_name(sort: &str) -> &str {
    match sort {
        "热门" => "trend_week",
        "最新" | "最新发布" => "most_recent",
        "评分" | "最高评分" | "评分最高" => "top_rated",
        "最多订阅" | "订阅最多" => "subscriptions",
        "最近更新" => "updated",
        "相关性" | "最相关" => "relevance",
        value => value,
    }
}
fn kind_from_tags(tags: &[String]) -> &'static str {
    if tags.iter().any(|t| t.eq_ignore_ascii_case("scene")) {
        "we_scene"
    } else if tags.iter().any(|t| t.eq_ignore_ascii_case("video")) {
        "video"
    } else if tags.iter().any(|t| t.eq_ignore_ascii_case("web")) {
        "we_web"
    } else if tags.iter().any(|t| t.eq_ignore_ascii_case("application")) {
        "we_application"
    } else {
        "unknown"
    }
}

fn favorite(client: &Client, id: PublishedFileId, add: bool) -> Result<(), String> {
    use steamworks::sys;
    // The typed SDK wrapper does not expose favorites. These are the SDK's
    // documented flat C functions, with their result checked before reporting success.
    unsafe {
        let ugc = sys::SteamAPI_SteamUGC_v021();
        let utils = sys::SteamAPI_SteamUtils_v010();
        if ugc.is_null() || utils.is_null() {
            return Err("Steam 收藏接口不可用。".into());
        }
        let call = if add {
            sys::SteamAPI_ISteamUGC_AddItemToFavorites(ugc, APP.0, id.0)
        } else {
            sys::SteamAPI_ISteamUGC_RemoveItemFromFavorites(ugc, APP.0, id.0)
        };
        if call == 0 {
            return Err("Steam 未接受收藏操作。".into());
        }
        let deadline = Instant::now() + WAIT;
        loop {
            let mut failed = false;
            if sys::SteamAPI_ISteamUtils_IsAPICallCompleted(utils, call, &mut failed) {
                if failed {
                    return Err("Steam 收藏同步失败，请稍后重试。".into());
                }
                let mut result =
                    std::mem::MaybeUninit::<sys::UserFavoriteItemsListChanged_t>::uninit();
                let ok = sys::SteamAPI_ISteamUtils_GetAPICallResult(
                    utils,
                    call,
                    result.as_mut_ptr().cast(),
                    std::mem::size_of::<sys::UserFavoriteItemsListChanged_t>() as i32,
                    3407,
                    &mut failed,
                );
                if !ok || failed {
                    return Err("无法读取 Steam 收藏结果。".into());
                }
                let result = result.assume_init();
                return if result.m_eResult == sys::EResult::k_EResultOK {
                    Ok(())
                } else {
                    Err(format!("Steam 收藏操作失败：{:?}", result.m_eResult))
                };
            }
            if Instant::now() >= deadline {
                return Err("Steam 收藏响应超时。".into());
            }
            client.run_callbacks();
            thread::sleep(Duration::from_millis(10));
        }
    }
}

struct Discovery;
impl Discovery {
    fn explore() -> Reply {
        let response = http_json("https://www.wallpaperengineapi.com/api/explore/v1")?;
        let collections = response["response"]["items"]
            .as_array()
            .ok_or("Wallpaper Engine 推荐服务返回了无法识别的数据。")?;
        Ok(json!({"collections":collections,"source":"discover"}))
    }

    fn search(action: &Value) -> Reply {
        let page = action["page"].as_u64().unwrap_or(1);
        let mut url =
            url::Url::parse("https://www.wallpaperengineapi.com/api/backupquery/v1").unwrap();
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("page", &page.to_string()).append_pair(
                "sort",
                sort_name(action["sort"].as_str().unwrap_or("top_rated")),
            );
            if let Some(text) = action["text"].as_str().filter(|s| !s.is_empty()) {
                query.append_pair("text", text);
            }
            if let Some(kind) = tag(action["kind"].as_str().unwrap_or("all")) {
                query.append_pair("requiredTags", kind);
            }
        }
        let response = http_json(url.as_str())?;
        let items=response["response"]["items"].as_array().ok_or("Wallpaper Engine 发现服务返回了无法识别的数据。")?
            .iter().filter_map(|item|{
                let id=item["i"].as_str().or_else(||item["publishedfileid"].as_str())?;
                if id.parse::<u64>().is_err(){return None;}
                let tags=item["tg"].as_array().into_iter().flatten().filter_map(|v|v.as_str().map(str::to_owned)).collect::<Vec<_>>();
                Some(json!({"id":id,"title":item["t"],"preview_url":item["p"],"description":"","kind":kind_from_tags(&tags),
                    "tags":tags,"author_id":item["c"],"size":item["s"],"updated":item["u"],"score":item["r"],"votes":item["v"]}))
            }).collect::<Vec<_>>();
        Ok(
            json!({"items":items,"page":page,"total_pages":response["response"]["totalpagecount"].as_u64().unwrap_or(1).max(1),"source":"discover","date":response["response"]["date"]}),
        )
    }
}

fn http_bytes(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let parsed = url::Url::parse(url).map_err(|e| e.to_string())?;
    if parsed.scheme() != "https" {
        return Err("预览链接必须使用 HTTPS。".into());
    }
    // Honor HTTPS-specific proxy settings before a generic ALL_PROXY. In
    // particular this avoids selecting an unsupported SOCKS transport when the
    // desktop already exposes an HTTP CONNECT proxy for HTTPS requests.
    let proxy = ["https_proxy", "HTTPS_PROXY"]
        .into_iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .and_then(|value| ureq::Proxy::new(&value).ok())
        })
        .or_else(ureq::Proxy::try_from_env);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(12)))
        .proxy(proxy)
        .build()
        .into();
    agent
        .get(url)
        .call()
        .map_err(|e| format!("在线服务暂时不可用：{e}"))?
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| format!("无法读取在线内容：{e}"))
}
fn http_json(url: &str) -> Reply {
    serde_json::from_slice(&http_bytes(url, 8 * 1024 * 1024)?)
        .map_err(|e| format!("在线服务返回了无效数据：{e}"))
}

fn preview_path(id: u64, url: &str) -> PathBuf {
    let mut hash = DefaultHasher::new();
    url.hash(&mut hash);
    cache_dir().join(format!("{id}-{:016x}.img", hash.finish()))
}

fn cached_preview(id: u64, url: &str) -> Option<PathBuf> {
    let output = preview_path(id, url);
    output
        .is_file()
        .then(|| crate::engine::thumbnail(output.to_str()?))
        .flatten()
}

/// Download previews outside the serialized Steam SDK worker. The UI merges
/// only thumbnail fields, since subscription/favorite state may have changed.
pub fn hydrate_thumbnails(mut items: Vec<Value>) -> Vec<Value> {
    prefetch(&items);
    for item in &mut items {
        if let (Some(id), Some(url)) = (
            item["id"].as_str().and_then(|id| id.parse::<u64>().ok()),
            item["preview_url"].as_str(),
        ) {
            item["thumbnail"] = json!(cached_preview(id, url));
        }
    }
    items
}

fn prefetch(items: &[Value]) {
    let work = items
        .iter()
        .take(100)
        .filter_map(|item| {
            Some((
                item["id"].as_str()?.parse::<u64>().ok()?,
                item["preview_url"].as_str()?.to_owned(),
            ))
        })
        .collect::<Vec<_>>();
    let cursor = AtomicUsize::new(0);
    let deadline = Instant::now() + Duration::from_secs(15);
    thread::scope(|scope| {
        for _ in 0..8.min(work.len()) {
            scope.spawn(|| {
                loop {
                    if Instant::now() >= deadline {
                        break;
                    }
                    let index = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some((id, url)) = work.get(index) else {
                        break;
                    };
                    let _ = preview(*id, url);
                }
            });
        }
    });
}

fn preview(id: u64, url: &str) -> Option<PathBuf> {
    // A workshop id is numeric; untrusted URLs never become filesystem names.
    let output = preview_path(id, url);
    if output.is_file() {
        return crate::engine::thumbnail(output.to_str()?);
    }
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    if ![
        "steamusercontent.com",
        "steamuserimages-a.akamaihd.net",
        "steamstatic.com",
    ]
    .iter()
    .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
    {
        return None;
    }
    let bytes = http_bytes(url, 8 * 1024 * 1024).ok()?;
    fs::create_dir_all(output.parent()?).ok()?;
    let temporary = output.with_extension("tmp");
    fs::write(&temporary, bytes).ok()?;
    fs::rename(&temporary, &output).ok()?;
    crate::engine::thumbnail(output.to_str()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_and_search_boundaries() {
        assert_eq!(item_id(&json!({"id":"3582127204"})).unwrap().0, 3582127204);
        assert!(item_id(&json!({"id":"../../file"})).is_err());
        assert!(validate_action(&json!({"page":0})).is_err());
        assert!(validate_action(&json!({"text":"a\u{0}b"})).is_err());
        assert_eq!(tag("we_scene"), Some("Scene"));
        assert_eq!(
            kind_from_tags(&["Customizable".into(), "Video".into()]),
            "video"
        );
    }

    #[test]
    fn personal_scope_filters_before_paginating_and_queue_roundtrips() {
        let items = (0..125).map(|index| json!({"id":index.to_string(),"title":format!("Rain {index}"),
            "tags":[if index < 60 {"Video"} else {"Scene"}],"updated":index,"score":index as f64 / 125.0}))
            .collect::<Vec<_>>();
        let page = user_page(
            &items,
            &json!({"kind":"视频","text":"RAIN","sort":"最新","page":2,"scope":"已订阅"}),
        );
        assert_eq!(page["total"], 60);
        assert_eq!(page["total_pages"], 2);
        assert_eq!(page["items"].as_array().unwrap().len(), 10);
        assert_eq!(page["items"][0]["id"], "9");
        let empty = user_page(&items, &json!({"text":"absent","page":99}));
        assert_eq!(empty["page"], 1);
        assert_eq!(empty["total"], 0);
        let legacy: Job = serde_json::from_value(json!({"id":"3582127204","title":"Saved task","status":"paused","downloaded":42,"total":100})).unwrap();
        assert!(legacy.account_id.is_empty());
        let restored: Job = serde_json::from_slice(&serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(
            (
                restored.downloaded,
                restored.total,
                restored.status.as_str()
            ),
            (42, 100, "paused")
        );
    }

    #[test]
    fn corrupt_queue_is_rejected_without_overwriting_source() {
        let path =
            std::env::temp_dir().join(format!("misari-corrupt-queue-{}.json", std::process::id()));
        fs::write(&path, b"[{broken").unwrap();
        assert!(read_jobs(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"[{broken");
        fs::write(
            &path,
            br#"[{"id":"../invalid","title":"x","status":"queued"}]"#,
        )
        .unwrap();
        assert!(read_jobs(&path).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn completed_tasks_report_updates_without_becoming_queued() {
        let mut job: Job = serde_json::from_value(
            json!({"id":"1","title":"Wallpaper","status":"completed","needs_update":true}),
        )
        .unwrap();
        assert!(observe_completed(
            &mut job,
            ItemState::INSTALLED | ItemState::NEEDS_UPDATE
        ));
        assert_eq!(job.status, "update_available");
        job.needs_update = false;
        assert!(observe_completed(&mut job, ItemState::INSTALLED));
        assert_eq!(job.status, "completed");
        assert!(observe_completed(&mut job, ItemState::NONE));
        assert_eq!(job.status, "error");
        assert!(job.error.is_some());
    }
}
