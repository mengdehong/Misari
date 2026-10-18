mod controls;
mod engine;
mod services;
mod view;

use iced::{Element, Subscription, Task, Theme, keyboard, widget, window};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Library,
    Discover,
    Workshop,
    Downloads,
    Playlists,
    Displays,
    Settings,
}
impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Library => "我的壁纸",
            Self::Discover => "发现灵感",
            Self::Workshop => "创意工坊",
            Self::Downloads => "下载与更新",
            Self::Playlists => "播放列表",
            Self::Displays => "显示器",
            Self::Settings => "设置",
        }
    }
}
#[derive(Clone, Debug)]
enum Message {
    Navigate(Page),
    Search(String),
    SearchNow,
    Scope(String),
    Filter(String),
    Sort(String),
    Favorites(bool),
    Page(i32),
    Loaded(Result<Value, String>),
    Refresh,
    RefreshLibrary,
    Tick,
    Polled(Result<Value, String>),
    Select(String),
    Properties(String, String, u64, Result<Value, String>),
    CloseDetail,
    Output(String),
    Fit(String),
    Apply,
    Playback(Value),
    Stop,
    Favorite,
    Open(String),
    Import(bool),
    Picked(Result<Option<String>, String>),
    Command(&'static str, Value),
    CommandDone(Result<Value, String>),
    Opened(Result<Value, String>),
    Property(String, Value),
    PropertyText(String, String),
    CommitProperty(String),
    PickProperty(String, bool),
    PropertyPicked(String, String, String, Result<Option<String>, String>),
    ResetProperties,
    PresetName(String),
    SavePreset,
    LoadPreset(String),
    DeletePreset(String),
    Service(Value),
    ServiceDone(String, u64, Result<Value, String>),
    ServicePoll(Result<Value, String>),
    ExploreLoaded(Result<Value, String>),
    ThumbnailsLoaded(Vec<Value>),
    PlaylistSelect(String),
    PlaylistName(String),
    CreatePlaylist,
    RenamePlaylist,
    DeletePlaylist,
    PlaylistAdd,
    PlaylistRemove(usize),
    PlaylistMove(usize, i32),
    PlaylistSaveProperties,
    RotationRandom(bool),
    RotationInterval(String),
    ScheduleStart(String),
    ScheduleEnd(String),
    ScheduleEnabled(bool),
    StartRotation(bool),
    Next,
    Dismiss,
    Key(keyboard::Key, keyboard::Modifiers),
    Screenshot(window::Screenshot),
}

struct App {
    page: Page,
    catalog: Vec<Value>,
    state: Value,
    library: Value,
    displays: Value,
    selected: Option<String>,
    properties: Value,
    drafts: BTreeMap<String, String>,
    search: String,
    scope: String,
    filter: String,
    sort: String,
    favorites_only: bool,
    page_index: usize,
    output: String,
    fit: String,
    loading: bool,
    polling: bool,
    busy: bool,
    notice: Option<(bool, String)>,
    service: Value,
    remote: Vec<Value>,
    recommendations: Vec<Value>,
    thumbnail_requests: usize,
    remote_page: usize,
    remote_pages: usize,
    service_busy: bool,
    service_polling: bool,
    search_revision: u64,
    property_epoch: u64,
    commands: VecDeque<(&'static str, Value)>,
    active_command: Option<(&'static str, Value)>,
    service_commands: VecDeque<Value>,
    playlist: String,
    playlist_name: String,
    preset_name: String,
    rotation_random: bool,
    rotation_interval: String,
    schedule_enabled: bool,
    schedule_start: String,
    schedule_end: String,
    screenshot: Option<String>,
    screenshot_tick: u8,
}
fn strv<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("background task panicked")
}
fn load_task() -> Task<Message> {
    Task::perform(
        blocking(|| {
            let mut value = engine::load()?;
            if let Some(items) = value["catalog"].as_array_mut() {
                for item in items {
                    if let Some(path) = item["thumbnail"].as_str()
                        && let Some(preview) = engine::thumbnail(path)
                    {
                        item["thumbnail"] = json!(preview);
                    }
                }
            }
            Ok(value)
        }),
        Message::Loaded,
    )
}
impl App {
    fn new() -> (Self, Task<Message>) {
        let args: Vec<_> = std::env::args().collect();
        let option = |key: &str| args.windows(2).find(|p| p[0] == key).map(|p| p[1].clone());
        let page = match option("--page").as_deref() {
            Some("workshop") => Page::Workshop,
            Some("discover") => Page::Discover,
            Some("downloads") => Page::Downloads,
            Some("playlists") => Page::Playlists,
            Some("displays") => Page::Displays,
            Some("settings") => Page::Settings,
            _ => Page::Library,
        };
        let app = Self {
            page,
            catalog: vec![],
            state: Value::Null,
            library: json!({}),
            displays: json!({}),
            selected: None,
            properties: Value::Null,
            drafts: BTreeMap::new(),
            search: String::new(),
            scope: "全部作品".into(),
            filter: "全部类型".into(),
            sort: if matches!(page, Page::Discover | Page::Workshop) {
                "热门"
            } else {
                "名称 A–Z"
            }
            .into(),
            favorites_only: false,
            page_index: 0,
            output: "全部显示器".into(),
            fit: "填充".into(),
            loading: true,
            polling: false,
            busy: false,
            notice: None,
            service: json!({}),
            remote: vec![],
            recommendations: vec![],
            thumbnail_requests: 0,
            remote_page: 1,
            remote_pages: 1,
            service_busy: false,
            service_polling: false,
            search_revision: 0,
            property_epoch: 0,
            commands: VecDeque::new(),
            active_command: None,
            service_commands: VecDeque::new(),
            playlist: String::new(),
            playlist_name: String::new(),
            preset_name: String::new(),
            rotation_random: false,
            rotation_interval: "30".into(),
            schedule_enabled: false,
            schedule_start: "08:00".into(),
            schedule_end: "23:00".into(),
            screenshot: option("--screenshot"),
            screenshot_tick: 0,
        };
        (
            app,
            Task::batch([
                load_task(),
                Task::perform(
                    blocking(|| services::execute(json!({"action":"status"}))),
                    Message::ServicePoll,
                ),
            ]),
        )
    }
    fn target(&self, mut params: Value) -> Value {
        if self.output != "全部显示器" {
            params["output"] = json!(self.output);
        }
        params
    }
    fn selected_asset(&self) -> Option<&Value> {
        let id = self.selected.as_deref()?;
        self.catalog
            .iter()
            .chain(self.remote.iter())
            .chain(self.recommendations.iter())
            .find(|v| strv(v, "id") == id)
    }
    fn is_remote(&self) -> bool {
        self.selected
            .as_deref()
            .is_some_and(|v| !v.starts_with("we:") && !v.starts_with("local:"))
    }
    fn current_output(&self) -> &Value {
        self.state["outputs"]
            .get(&self.output)
            .or_else(|| {
                self.state["outputs"]
                    .as_object()
                    .and_then(|m| m.values().next())
            })
            .unwrap_or(&Value::Null)
    }
    fn prop_state(&self) -> &Value {
        self.properties["outputs"]
            .get(&self.output)
            .or_else(|| {
                self.properties["outputs"]
                    .as_object()
                    .and_then(|m| m.values().next())
            })
            .unwrap_or(&self.properties)
    }
    fn property_value(&self, key: &str) -> Value {
        self.prop_state()["values"]
            .get(key)
            .or_else(|| self.properties["values"].get(key))
            .cloned()
            .unwrap_or(Value::Null)
    }
    fn can_edit_properties(&self) -> bool {
        let Some(id) = self.selected.as_deref() else {
            return false;
        };
        let Some(outputs) = self.state["outputs"].as_object() else {
            return false;
        };
        let targets: Vec<_> = outputs
            .iter()
            .filter(|(name, _)| self.output == "全部显示器" || **name == self.output)
            .collect();
        !targets.is_empty()
            && targets.iter().all(|(_, v)| {
                v["current"] == id && v["properties_available"] == true && v["pending"].is_null()
            })
    }
    fn favorite(&self, id: &str) -> bool {
        array(&self.library["favorites"])
            .iter()
            .any(|v| v.as_str() == Some(id))
    }
    fn local_title(&self, v: &Value) -> String {
        self.library["titles"][strv(v, "id")]
            .as_str()
            .unwrap_or_else(|| {
                let s = strv(v, "name");
                if s.is_empty() { strv(v, "title") } else { s }
            })
            .into()
    }
    fn filtered(&self) -> Vec<&Value> {
        let mut values: Vec<_> = self
            .catalog
            .iter()
            .filter(|v| {
                let id = strv(v, "id");
                !array(&self.library["hidden"])
                    .iter()
                    .any(|v| v.as_str() == Some(id))
                    && (!self.favorites_only || self.favorite(id))
                    && (self.filter == "全部类型"
                        || view::kind_label(strv(v, "kind")) == self.filter)
                    && (self.search.trim().is_empty()
                        || self
                            .local_title(v)
                            .to_lowercase()
                            .contains(&self.search.trim().to_lowercase()))
            })
            .collect();
        values.sort_by_cached_key(|v| self.local_title(v).to_lowercase());
        if self.sort == "名称 Z–A" {
            values.reverse();
        }
        if self.sort == "类型" {
            values.sort_by_key(|v| strv(v, "kind"));
        }
        values
    }
    fn notify(&mut self, result: Result<Value, String>) -> bool {
        match result {
            Ok(_) => {
                if !self.notice.as_ref().is_some_and(|(e, _)| *e) {
                    self.notice = Some((false, "已保存".into()));
                }
                true
            }
            Err(e) => {
                self.notice = Some((true, e));
                false
            }
        }
    }
    fn command(&mut self, method: &'static str, params: Value) -> Task<Message> {
        if self.busy {
            if matches!(method, "set_properties" | "set_playback")
                && let Some((_, queued)) = self.commands.iter_mut().rev().find(|(m, p)| {
                    *m == method
                        && p["output"] == params["output"]
                        && p["asset_id"] == params["asset_id"]
                        && p.get("preset").is_none()
                        && params.get("preset").is_none()
                })
            {
                if method == "set_properties" {
                    if let (Some(to), Some(from)) = (
                        queued["properties"].as_object_mut(),
                        params["properties"].as_object(),
                    ) {
                        to.extend(from.clone());
                    }
                } else if let (Some(to), Some(from)) = (queued.as_object_mut(), params.as_object())
                {
                    to.extend(from.clone());
                }
                return Task::none();
            }
            self.commands.push_back((method, params));
            return Task::none();
        }
        self.busy = true;
        self.active_command = Some((method, params.clone()));
        Task::perform(
            blocking(move || engine::request(method, params)),
            Message::CommandDone,
        )
    }
    fn properties_task(&self) -> Task<Message> {
        let Some(id) = self.selected.clone().filter(|v| v.starts_with("we:")) else {
            return Task::none();
        };
        let params = self.target(json!({"asset_id":id}));
        let output = self.output.clone();
        let epoch = self.property_epoch;
        Task::perform(
            blocking(move || engine::request("get_properties", params)),
            move |v| Message::Properties(id.clone(), output.clone(), epoch, v),
        )
    }
    fn service_task(&mut self, mut action: Value) -> Task<Message> {
        if action.get("_revision").is_none() {
            action["_revision"] = json!(self.search_revision);
        }
        if self.service_busy {
            if action["action"] == "search" {
                self.service_commands.retain(|v| v["action"] != "search");
            }
            self.service_commands.push_back(action);
            return Task::none();
        }
        self.service_busy = true;
        let kind = strv(&action, "action").to_owned();
        let revision = action["_revision"].as_u64().unwrap_or(0);
        Task::perform(blocking(move || services::execute(action)), move |v| {
            Message::ServiceDone(kind.clone(), revision, v)
        })
    }
    fn search_task(&mut self) -> Task<Message> {
        self.search_revision += 1;
        self.service_task(json!({"action":"search","text":self.search,"kind":self.filter,"scope":self.scope,"sort":self.sort,"page":self.remote_page,"source":if self.page==Page::Discover{"discover"}else{"steam"}}))
    }
    fn accept_service(&mut self, value: Value) {
        if let Some(states) = value["item_states"].as_object() {
            for item in self.remote.iter_mut().chain(&mut self.recommendations) {
                if let Some(state) = states.get(strv(item, "id"))
                    && let (Some(item), Some(state)) = (item.as_object_mut(), state.as_object())
                {
                    item.extend(state.clone());
                }
            }
        }
        self.service = value;
    }
    fn thumbnails_task(&mut self, items: Vec<Value>) -> Task<Message> {
        if items.is_empty() {
            return Task::none();
        }
        self.thumbnail_requests += 1;
        Task::perform(
            blocking(move || services::hydrate_thumbnails(items)),
            Message::ThumbnailsLoaded,
        )
    }
    fn select_playlist(&mut self, id: String) {
        self.playlist = id;
        self.playlist_name = strv(&self.library["playlists"][&self.playlist], "name").into();
        let rotation = self.state["rotations"]
            .get(&self.output)
            .or_else(|| {
                self.state["rotations"]
                    .as_object()
                    .and_then(|m| m.values().next())
            })
            .cloned()
            .unwrap_or(Value::Null);
        self.rotation_random = strv(&rotation["settings"], "mode") == "random";
        self.rotation_interval = (rotation["settings"]["interval_seconds"]
            .as_u64()
            .unwrap_or(1800)
            / 60)
            .to_string();
        let schedule = array(&rotation["settings"]["schedule"]);
        self.schedule_enabled = !schedule.is_empty();
        if let Some(s) = schedule.first() {
            self.schedule_start = strv(s, "start").into();
            self.schedule_end = strv(s, "end").into();
        }
    }
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Navigate(page) => {
                self.page = page;
                self.page_index = 0;
                self.search.clear();
                self.filter = "全部类型".into();
                self.selected = None;
                self.notice = None;
                if matches!(page, Page::Discover | Page::Workshop) {
                    self.sort = "热门".into();
                    self.remote_page = 1;
                    let search = self.search_task();
                    if page == Page::Discover && self.recommendations.is_empty() {
                        return Task::batch([
                            search,
                            Task::perform(
                                blocking(|| {
                                    services::execute(json!({
                                    "action":"explore"
                                    }
                                    ))
                                }),
                                Message::ExploreLoaded,
                            ),
                        ]);
                    }
                    return search;
                }
                self.sort = "名称 A–Z".into();
            }

            Message::Search(s) => {
                self.search = s;
                self.page_index = 0;
            }

            Message::Scope(scope) => {
                self.scope = scope;
                self.remote_page = 1;
                return self.search_task();
            }

            Message::SearchNow => {
                if matches!(self.page, Page::Discover | Page::Workshop) {
                    self.remote_page = 1;
                    return self.search_task();
                }
            }

            Message::Filter(v) => {
                self.filter = v;
                self.page_index = 0;
                if matches!(self.page, Page::Discover | Page::Workshop) {
                    self.remote_page = 1;
                    return self.search_task();
                }
            }

            Message::Sort(v) => {
                self.sort = v;
                self.page_index = 0;
                if matches!(self.page, Page::Discover | Page::Workshop) {
                    return self.search_task();
                }
            }

            Message::Favorites(v) => {
                self.favorites_only = v;
                self.page_index = 0;
            }

            Message::Page(delta) => {
                if matches!(self.page, Page::Discover | Page::Workshop) {
                    self.remote_page = (self.remote_page as i32 + delta).max(1) as usize;
                    return self.search_task();
                } else {
                    self.page_index = (self.page_index as i32 + delta).max(0) as usize;
                }
            }

            Message::Refresh => {
                if matches!(self.page, Page::Discover | Page::Workshop) {
                    return self.search_task();
                }
                self.loading = true;
                return load_task();
            }

            Message::Loaded(result) => {
                self.loading = false;
                match result {
                    Ok(v) => {
                        self.state = v["state"].clone();
                        self.catalog = array(&v["catalog"]).to_vec();
                        self.library = v["library"].clone();
                        self.displays = v["displays"].clone();
                        if self.playlist.is_empty()
                            && let Some(id) = self.library["playlists"]
                                .as_object()
                                .and_then(|m| m.keys().next())
                                .cloned()
                        {
                            self.select_playlist(id);
                        }

                        if self.selected.is_none() && self.page == Page::Library {
                            self.selected =
                                self.current_output()["current"].as_str().map(String::from);
                            return self.properties_task();
                        }

                        if matches!(self.page, Page::Discover | Page::Workshop)
                            && self.remote.is_empty()
                        {
                            let search = self.search_task();
                            if self.page == Page::Discover && self.recommendations.is_empty() {
                                return Task::batch([
                                    search,
                                    Task::perform(
                                        blocking(|| services::execute(json!({"action":"explore"}))),
                                        Message::ExploreLoaded,
                                    ),
                                ]);
                            }
                            return search;
                        }
                    }
                    Err(e) => self.notice = Some((true, e)),
                }
            }

            Message::RefreshLibrary => {
                self.loading = true;
                return load_task();
            }

            Message::Tick => {
                if let Some(page) = engine::activation()
                    && let Some(page) = match page.as_str() {
                        "library" => Some(Page::Library),
                        "workshop" => Some(Page::Workshop),
                        "discover" => Some(Page::Discover),
                        "downloads" => Some(Page::Downloads),
                        "playlists" => Some(Page::Playlists),
                        "displays" => Some(Page::Displays),
                        "settings" => Some(Page::Settings),
                        _ => None,
                    }
                {
                    return self.update(Message::Navigate(page));
                }

                let mut tasks = vec![];

                if !self.polling && !self.loading {
                    self.polling = true;
                    tasks.push(Task::perform(
                        blocking(|| engine::request("get", json!({}))),
                        Message::Polled,
                    ));
                }

                if !self.service_polling && !self.service_busy {
                    self.service_polling = true;
                    tasks.push(Task::perform(
                        blocking(|| {
                            services::execute(json!({
                            "action":"status"
                            }
                            ))
                        }),
                        Message::ServicePoll,
                    ));
                }

                if self.screenshot.is_some() {
                    self.screenshot_tick = self.screenshot_tick.saturating_add(1);
                    if self.screenshot_tick >= 4
                        && !self.loading
                        && !self.service_busy
                        && self.thumbnail_requests == 0
                    {
                        tasks.push(
                            window::oldest()
                                .and_then(window::screenshot)
                                .map(Message::Screenshot),
                        );
                    }
                }

                return Task::batch(tasks);
            }

            Message::Polled(result) => {
                self.polling = false;
                match result {
                    Ok(v) => {
                        self.state = v["state"].clone();
                        if !self.busy {
                            self.library = self.state["library"].clone();
                        }

                        if self.output != "全部显示器"
                            && self.state["outputs"][&self.output].is_null()
                        {
                            self.output = "全部显示器".into();
                            self.notice =
                                Some((false, "显示器已断开，已保留它的壁纸设置。".into()));
                        }
                    }
                    Err(e) => {
                        if self.notice.is_none() {
                            self.notice = Some((true, e));
                        }
                    }
                }
            }

            Message::Select(id) => {
                self.property_epoch += 1;
                self.selected = Some(id.clone());
                self.properties = Value::Null;
                self.drafts.clear();
                self.preset_name.clear();
                if self.is_remote() {
                    return self.service_task(json!({
                    "action":"detail","id":id
                    }
                    ));
                }
                return self.properties_task();
            }

            Message::Properties(id, output, epoch, result) => {
                if self.selected.as_deref() == Some(&id)
                    && self.output == output
                    && self.property_epoch == epoch
                {
                    match result {
                        Ok(v) => {
                            self.properties = v;
                            self.drafts.clear();
                            for def in array(&self.properties["definitions"]) {
                                let key = strv(def, "key");
                                let value = self.property_value(key);
                                let s = value
                                    .as_str()
                                    .map(String::from)
                                    .unwrap_or_else(|| value.to_string());
                                self.drafts.insert(key.into(), s);
                            }
                        }
                        Err(e) => self.notice = Some((true, e)),
                    }
                }
            }

            Message::CloseDetail => self.selected = None,
            Message::Output(v) => {
                self.output = v;
                self.property_epoch += 1;
                self.fit = match strv(self.current_output(), "fit") {
                    "contain" => "适应",
                    "stretch" => "拉伸",
                    _ => "填充",
                }
                .into();
                self.select_playlist(self.playlist.clone());
                return self.properties_task();
            }

            Message::Fit(v) => self.fit = v,
            Message::Apply => {
                if let Some(id) = self.selected.clone() {
                    return self.command(
                        "apply",
                        self.target(json!({
                        "asset_id":id,"fit":match self.fit.as_str(){
                        "适应"=>"contain","拉伸"=>"stretch",_=>"cover"
                        }
                        ,"transition":"fade"
                        }
                        )),
                    );
                }
            }

            Message::Playback(mut patch) => {
                patch = self.target(patch);
                return self.command("set_playback", patch);
            }

            Message::Stop => return self.command("release", self.target(json!({}))),
            Message::Favorite => {
                if let Some(id) = self.selected.clone() {
                    let favorite = !self.favorite(&id);
                    return self.command(
                        "edit_library",
                        json!({
                        "action":"favorite","asset_id":id,"favorite":favorite
                        }
                        ),
                    );
                }
            }

            Message::Open(path) => {
                return Task::perform(
                    blocking(move || engine::open(&path).map(|_| json!({}))),
                    Message::Opened,
                );
            }
            Message::Import(directory) => {
                return Task::perform(blocking(move || engine::pick(directory)), Message::Picked);
            }
            Message::Picked(result) => match result {
                Ok(Some(path)) => {
                    return self.command(
                        "edit_library",
                        json!({
                        "action":"import","path":path
                        }
                        ),
                    );
                }
                Ok(None) => {}
                Err(e) => self.notice = Some((true, e)),
            },
            Message::Command(method, params) => return self.command(method, params),
            Message::Opened(result) => {
                if let Err(error) = result {
                    self.notice = Some((true, error));
                }
            }

            Message::CommandDone(result) => {
                self.busy = false;
                let active = self.active_command.take();
                if let Ok(reply) = &result {
                    if self.commands.is_empty()
                        && let Some(library) = reply.get("library")
                    {
                        self.library = library.clone();
                    }

                    if let Some(id) = reply["created"].as_str() {
                        self.select_playlist(id.into());
                    } else if !self.library["playlists"]
                        .as_object()
                        .is_some_and(|lists| lists.contains_key(&self.playlist))
                        && let Some(id) = self.library["playlists"]
                            .as_object()
                            .and_then(|m| m.keys().next())
                            .cloned()
                    {
                        self.select_playlist(id);
                    } else if self.library["playlists"]
                        .as_object()
                        .is_some_and(|lists| lists.is_empty())
                    {
                        self.playlist.clear();
                        self.playlist_name.clear();
                    }
                }
                let quiet = active
                    .as_ref()
                    .is_some_and(|(m, _)| matches!(*m, "set_playback" | "set_properties"));
                let ok = if result.is_ok() && quiet {
                    true
                } else {
                    self.notify(result)
                };
                if let Some((method, params)) = self.commands.pop_front() {
                    return self.command(method, params);
                }

                if ok {
                    let refresh = if active.as_ref().is_some_and(|(method, params)| {
                        *method == "edit_library" && params["action"] == "import"
                    }) {
                        load_task()
                    } else {
                        Task::perform(
                            blocking(|| engine::request("get", json!({}))),
                            Message::Polled,
                        )
                    };
                    return Task::batch([refresh, self.properties_task()]);
                } else {
                    return self.properties_task();
                }
            }

            Message::Property(key, value) => {
                if let Some(id) = self.selected.clone() {
                    self.property_epoch += 1;
                    if let Some(outputs) = self.properties["outputs"].as_object_mut() {
                        for (name, output) in outputs {
                            if self.output == "全部显示器" || *name == self.output {
                                output["values"][&key] = value.clone();
                            }
                        }
                    }
                    self.drafts.insert(
                        key.clone(),
                        value
                            .as_str()
                            .map(String::from)
                            .unwrap_or_else(|| value.to_string()),
                    );
                    return self.command(
                        "set_properties",
                        self.target(json!({
                        "asset_id":id,"properties":{
                        key:value
                        }

                        }
                        )),
                    );
                }
            }

            Message::PropertyText(key, value) => {
                self.drafts.insert(key, value);
            }

            Message::CommitProperty(key) => {
                let value = self.drafts.get(&key).cloned().unwrap_or_default();
                let ty = array(&self.properties["definitions"])
                    .iter()
                    .find(|d| strv(d, "key") == key)
                    .map(|d| strv(d, "type"))
                    .unwrap_or("");
                let value = if matches!(ty, "slider" | "color") {
                    match serde_json::from_str::<Value>(&value) {
                        Ok(v) => v,
                        Err(_) => {
                            self.notice=Some((true,"请输入有效数值；颜色使用三个 0–1 的 RGB 通道，例如 [0.2, 0.5, 1.0]。".into()));
                            return Task::none();
                        }
                    }
                } else {
                    json!(value)
                };
                return self.update(Message::Property(key, value));
            }

            Message::PickProperty(key, directory) => {
                let id = self.selected.clone().unwrap_or_default();
                let output = self.output.clone();
                return Task::perform(blocking(move || engine::pick(directory)), move |v| {
                    Message::PropertyPicked(key.clone(), id.clone(), output.clone(), v)
                });
            }
            Message::PropertyPicked(key, id, output, result) => {
                if self.selected.as_deref() != Some(&id) || self.output != output {
                    return Task::none();
                }
                match result {
                    Ok(Some(path)) => return self.update(Message::Property(key, json!(path))),
                    Ok(None) => {}
                    Err(e) => self.notice = Some((true, e)),
                }
            }

            Message::ResetProperties => {
                if let Some(id) = self.selected.clone() {
                    let properties: serde_json::Map<_, _> = array(&self.properties["definitions"])
                        .iter()
                        .filter(|d| d["editable"] == true)
                        .map(|d| (strv(d, "key").into(), Value::Null))
                        .collect();
                    return self.command(
                        "set_properties",
                        self.target(json!({
                        "asset_id":id,"properties":properties
                        }
                        )),
                    );
                }
            }

            Message::PresetName(s) => self.preset_name = s,
            Message::SavePreset => {
                if let Some(id) = self.selected.clone() {
                    return self.command("edit_library",json!({
"action":"save_preset","asset_id":id,"name":self.preset_name.trim(),"properties":self.prop_state()["values"]
}
));
                }
            }

            Message::LoadPreset(name) => {
                if let Some(id) = self.selected.clone() {
                    return self.command(
                        "set_properties",
                        self.target(json!({
                        "asset_id":id,"preset":name
                        }
                        )),
                    );
                }
            }

            Message::DeletePreset(name) => {
                if let Some(id) = self.selected.clone() {
                    return self.command(
                        "edit_library",
                        json!({
                        "action":"delete_preset","asset_id":id,"name":name
                        }
                        ),
                    );
                }
            }

            Message::Service(action) => return self.service_task(action),
            Message::ServiceDone(kind, revision, result) => {
                self.service_busy = false;
                let mut tasks = Vec::new();
                match result {
                    Ok(v) => {
                        if kind == "search" && revision == self.search_revision {
                            self.remote = array(&v["items"]).to_vec();
                            self.remote_pages =
                                v["total_pages"].as_u64().unwrap_or(1).max(1) as usize;
                            self.remote_page =
                                v["page"].as_u64().unwrap_or(self.remote_page as u64) as usize;
                            tasks.push(self.thumbnails_task(self.remote.clone()));
                        } else if kind == "search" {
                        } else if kind == "detail" {
                            let item = &v["item"];
                            for old in self
                                .remote
                                .iter_mut()
                                .chain(&mut self.recommendations)
                                .filter(|a| a["id"] == item["id"])
                            {
                                *old = item.clone();
                            }
                            tasks.push(self.thumbnails_task(vec![item.clone()]));
                        } else {
                            self.notice =
                                Some((false, v["message"].as_str().unwrap_or("操作已完成").into()));
                        }

                        if v.get("account").is_some() {
                            self.accept_service(v);
                        }
                    }
                    Err(e) => self.notice = Some((true, e)),
                }

                if let Some(action) = self.service_commands.pop_front() {
                    tasks.push(self.service_task(action));
                }
                return Task::batch(tasks);
            }

            Message::ExploreLoaded(result) => {
                if let Ok(value) = result {
                    self.recommendations = array(&value["items"]).to_vec();
                    return self.thumbnails_task(self.recommendations.clone());
                }
            }

            Message::ThumbnailsLoaded(items) => {
                self.thumbnail_requests = self.thumbnail_requests.saturating_sub(1);
                for item in items {
                    for current in self
                        .remote
                        .iter_mut()
                        .chain(&mut self.recommendations)
                        .filter(|current| {
                            current["id"] == item["id"]
                                && current["preview_url"] == item["preview_url"]
                        })
                    {
                        if item["thumbnail"].is_string() {
                            current["thumbnail"] = item["thumbnail"].clone();
                            current["preview_failed"] = json!(false);
                        } else if !current["thumbnail"].is_string() {
                            current["preview_failed"] = json!(true);
                        }
                    }
                }
            }

            Message::ServicePoll(result) => {
                self.service_polling = false;
                match result {
                    Ok(v) => {
                        let finished = |v: &Value| {
                            array(&v["jobs"])
                                .iter()
                                .filter(|j| matches!(strv(j, "status"), "completed" | "installed"))
                                .map(|j| strv(j, "id").to_owned())
                                .collect::<Vec<_>>()
                        };
                        let changed = finished(&v) != finished(&self.service);
                        self.accept_service(v);
                        if changed {
                            return load_task();
                        }
                    }
                    Err(e) => {
                        self.service["account"]["error"] = json!(e);
                    }
                }
            }

            Message::PlaylistSelect(id) => self.select_playlist(id),
            Message::PlaylistName(s) => self.playlist_name = s,
            Message::CreatePlaylist => {
                return self.command(
                    "edit_library",
                    json!({
                    "action":"create_playlist","name":self.playlist_name.trim()
                    }
                    ),
                );
            }
            Message::RenamePlaylist => {
                return self.command(
                    "edit_library",
                    json!({
                    "action":"rename_playlist","id":self.playlist,"name":self.playlist_name.trim()
                    }
                    ),
                );
            }
            Message::DeletePlaylist => {
                if self.busy || self.playlist.is_empty() {
                    return Task::none();
                }
                let id = self.playlist.clone();
                return self.command(
                    "edit_library",
                    json!({
                    "action":"delete_playlist","id":id
                    }
                    ),
                );
            }

            Message::PlaylistAdd => {
                if let Some(id) = self.selected.clone() {
                    let mut members =
                        array(&self.library["playlists"][&self.playlist]["members"]).to_vec();
                    if !members.iter().any(|v| v == &id) {
                        members.push(json!(id));
                    }
                    self.library["playlists"][&self.playlist]["members"] = json!(members);
                    return self.command(
                        "edit_library",
                        json!({
                        "action":"playlist_members","id":self.playlist,"members":members
                        }
                        ),
                    );
                }
            }

            Message::PlaylistRemove(index) => {
                let mut members =
                    array(&self.library["playlists"][&self.playlist]["members"]).to_vec();
                if index < members.len() {
                    members.remove(index);
                }
                self.library["playlists"][&self.playlist]["members"] = json!(members);
                return self.command(
                    "edit_library",
                    json!({
                    "action":"playlist_members","id":self.playlist,"members":members
                    }
                    ),
                );
            }

            Message::PlaylistMove(index, delta) => {
                let mut members =
                    array(&self.library["playlists"][&self.playlist]["members"]).to_vec();
                let next = index as i32 + delta;
                if index < members.len() && next >= 0 && (next as usize) < members.len() {
                    members.swap(index, next as usize);
                }
                self.library["playlists"][&self.playlist]["members"] = json!(members);
                return self.command(
                    "edit_library",
                    json!({
                    "action":"playlist_members","id":self.playlist,"members":members
                    }
                    ),
                );
            }

            Message::PlaylistSaveProperties => {
                if let Some(id) = self.selected.clone() {
                    return self.command("edit_library",json!({
"action":"playlist_properties","id":self.playlist,"asset_id":id,"properties":self.prop_state()["values"]
}
));
                }
            }

            Message::RotationRandom(v) => self.rotation_random = v,
            Message::RotationInterval(v) => self.rotation_interval = v,
            Message::ScheduleStart(v) => self.schedule_start = v,
            Message::ScheduleEnd(v) => self.schedule_end = v,
            Message::ScheduleEnabled(v) => self.schedule_enabled = v,
            Message::StartRotation(enabled) => {
                let seconds = match self
                    .rotation_interval
                    .parse::<u64>()
                    .ok()
                    .and_then(|v| v.checked_mul(60))
                    .filter(|v| (10..=86400).contains(v))
                {
                    Some(v) => v,
                    None => {
                        self.notice = Some((true, "轮换周期需要是 1–1440 分钟。".into()));
                        return Task::none();
                    }
                };
                let schedule = if self.schedule_enabled {
                    json!([{
                    "start":self.schedule_start,"end":self.schedule_end,"days":[1,2,3,4,5,6,7]
                    }
                    ])
                } else {
                    json!([])
                };
                return self.command("set_rotation",self.target(json!({
"rotation":{
"enabled":enabled,"source":self.playlist,"mode":if self.rotation_random{
"random"
}
else{
"ordered"
}
,"interval_seconds":seconds,"fit":"cover","transition":"fade","schedule":schedule
}

}
)));
            }

            Message::Next => {
                let output = if self.output == "全部显示器" {
                    self.state["outputs"]
                        .as_object()
                        .and_then(|m| m.keys().next())
                        .cloned()
                } else {
                    Some(self.output.clone())
                };
                if let Some(output) = output {
                    return self.command(
                        "rotation_next",
                        json!({
                        "output":output
                        }
                        ),
                    );
                }
            }

            Message::Dismiss => self.notice = None,
            Message::Key(key, modifiers) => match key {
                keyboard::Key::Named(keyboard::key::Named::Tab) => {
                    return if modifiers.shift() {
                        controls::focus_previous()
                    } else {
                        controls::focus_next()
                    };
                }
                keyboard::Key::Named(keyboard::key::Named::Escape) => {
                    self.selected = None;
                    self.notice = None;
                }
                keyboard::Key::Character(ref c)
                    if modifiers.control()
                        && matches!(c.as_str(), "1" | "2" | "3" | "4" | "5" | "6" | "7") =>
                {
                    let i = c.parse::<usize>().unwrap() - 1;
                    return self.update(Message::Navigate(
                        [
                            Page::Library,
                            Page::Discover,
                            Page::Workshop,
                            Page::Downloads,
                            Page::Playlists,
                            Page::Displays,
                            Page::Settings,
                        ][i],
                    ));
                }
                keyboard::Key::Character(ref c) if modifiers.control() && c.as_str() == "i" => {
                    return self.update(Message::Import(modifiers.shift()));
                }
                keyboard::Key::Character(ref c) if modifiers.control() && c.as_str() == "p" => {
                    return self.update(Message::Playback(json!({
                    "paused":self.current_output()["paused"]!=true
                    }
                    )));
                }
                keyboard::Key::Character(c) if modifiers.control() && c.as_str() == "f" => {
                    return widget::operation::focus("search");
                }
                keyboard::Key::Character(c) if modifiers.control() && c.as_str() == "r" => {
                    return self.update(Message::Refresh);
                }
                _ => {}
            },

            Message::Screenshot(shot) => {
                if let Some(path) = self.screenshot.take() {
                    if let Err(e) = image::save_buffer(
                        &path,
                        &shot.rgba,
                        shot.size.width,
                        shot.size.height,
                        image::ColorType::Rgba8,
                    ) {
                        eprintln!("screenshot: {e}");
                    } else {
                        println!("screenshot: {path}");
                    }
                }
            }
        }

        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_secs(2)).map(|_| Message::Tick),
            keyboard::listen().filter_map(|e| match e {
                keyboard::Event::KeyPressed { key, modifiers, .. } => {
                    Some(Message::Key(key, modifiers))
                }
                _ => None,
            }),
        ])
    }
    fn view(&self) -> Element<'_, Message> {
        view::view(self)
    }
}
fn main() -> iced::Result {
    // SAFETY: initialization runs before Iced, Tokio or the Steam worker creates threads.
    unsafe {
        std::env::set_var("SteamAppId", "431960");
        std::env::set_var("SteamGameId", "431960");
    }

    if std::env::args().any(|v| v == "--diagnose") {
        println!("{}",serde_json::to_string_pretty(&json!({"engine":engine::diagnose(),"steam":services::execute(json!({"action":"status"}))})).unwrap());
        return Ok(());
    }
    let args: Vec<_> = std::env::args().collect();
    let page = args
        .windows(2)
        .find(|p| p[0] == "--page")
        .map(|p| p[1].clone());
    let _instance = match engine::single_instance(page) {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(()),
        Err(error) => {
            rfd::MessageDialog::new()
                .set_title("无法打开 Misari 壁纸")
                .set_description(error)
                .set_level(rfd::MessageLevel::Error)
                .show();
            return Ok(());
        }
    };
    iced::application(App::new, App::update, App::view)
        .title("Misari · 壁纸")
        .theme(Theme::custom(
            "Misari",
            iced::theme::Palette {
                background: view::BG,
                text: view::TEXT,
                primary: view::ACCENT,
                success: view::ACCENT,
                warning: iced::Color::from_rgb8(231, 181, 111),
                danger: iced::Color::from_rgb8(240, 129, 136),
            },
        ))
        .default_font(iced::Font::with_name("Noto Sans CJK SC"))
        .window(window::Settings {
            size: iced::Size::new(1440.0, 960.0),
            min_size: Some(iced::Size::new(1080.0, 720.0)),
            platform_specific: window::settings::PlatformSpecific {
                application_id: "io.misari.wallpaper".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .subscription(App::subscription)
        .run()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rapid_edits_coalesce_without_losing_other_properties() {
        let (mut app, _) = App::new();
        let _ = app.command(
            "set_properties",
            json!({"asset_id":"we:/a","output":"A","properties":{"speed":1}}),
        );
        for n in 2..20 {
            let _ = app.command(
                "set_properties",
                json!({"asset_id":"we:/a","output":"A","properties":{"speed":n}}),
            );
        }
        let _ = app.command(
            "set_properties",
            json!({"asset_id":"we:/a","output":"A","properties":{"color":[1,0,0]}}),
        );
        let _ = app.command(
            "set_properties",
            json!({"asset_id":"we:/a","output":"B","properties":{"speed":5}}),
        );
        assert_eq!(app.commands.len(), 2);
        assert_eq!(
            app.commands[0].1["properties"],
            json!({"speed":19,"color":[1,0,0]})
        );
        assert_eq!(app.commands[1].1["output"], "B");
    }
    #[test]
    fn late_properties_and_file_dialog_cannot_write_another_output() {
        let (mut app, _) = App::new();
        app.selected = Some("we:/a".into());
        app.output = "B".into();
        app.property_epoch = 3;
        let _ = app.update(Message::Properties(
            "we:/a".into(),
            "A".into(),
            3,
            Ok(json!({"values":{"x":1}})),
        ));
        assert!(app.properties.is_null());
        let _ = app.update(Message::PropertyPicked(
            "file".into(),
            "we:/a".into(),
            "A".into(),
            Ok(Some("/tmp/photo.png".into())),
        ));
        assert!(!app.busy);
        let _ = app.update(Message::Properties(
            "we:/a".into(),
            "B".into(),
            2,
            Ok(json!({"values":{"x":2}})),
        ));
        assert!(app.properties.is_null());
    }
    #[test]
    fn stale_search_does_not_overwrite_current_results() {
        let (mut app, _) = App::new();
        app.search_revision = 2;
        app.remote = vec![json!({"id":"new"})];
        let _ = app.update(Message::ServiceDone(
            "search".into(),
            1,
            Ok(json!({"items":[{"id":"old"}],"page":1,"total_pages":2})),
        ));
        assert_eq!(app.remote[0]["id"], "new");
    }
    #[test]
    fn recommendation_actions_refresh_both_detail_and_buttons() {
        let (mut app, _) = App::new();
        app.remote = vec![json!({"id":"42","steam_favorite":false})];
        app.recommendations = app.remote.clone();
        app.accept_service(json!({"item_states":{"42":{"subscribed":true,"steam_favorite":true}}}));
        assert_eq!(app.recommendations[0]["steam_favorite"], true);
        let _ = app.update(Message::ServiceDone(
            "detail".into(),
            0,
            Ok(json!({"item":{"id":"42","title":"Refreshed","subscribed":true}})),
        ));
        assert_eq!(app.recommendations[0]["title"], "Refreshed");
        assert_eq!(app.remote[0], app.recommendations[0]);
    }
    #[test]
    fn deleting_a_playlist_cannot_queue_an_empty_or_duplicate_delete() {
        let (mut app, _) = App::new();
        app.library =
            json!({"playlists":{"a":{"name":"A","members":[]},"b":{"name":"B","members":[]}}});
        app.select_playlist("a".into());
        let _ = app.update(Message::DeletePlaylist);
        let _ = app.update(Message::DeletePlaylist);
        assert!(app.commands.is_empty());
        assert_eq!(app.playlist, "a");
        let _ = app.update(Message::CommandDone(Ok(
            json!({"library":{"playlists":{"b":{"name":"B","members":[]}}}}),
        )));
        assert_eq!(app.playlist, "b");
        let _ = app.update(Message::DeletePlaylist);
        let _ = app.update(Message::CommandDone(Err("Unavailable".into())));
        assert_eq!(app.playlist, "b");
    }
}
