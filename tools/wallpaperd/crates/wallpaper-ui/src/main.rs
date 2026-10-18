mod color_picker;
mod controls;
mod engine;
mod filters;
mod icons;
mod motion;
mod palette;
mod previews;
mod services;
mod view;

use filters::{Filters, Group};
use iced::{Element, Subscription, Task, Theme, keyboard, widget, window};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
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
            Self::Downloads => "下载",
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
    ToggleSidebar,
    ToggleFilters,
    ShowNavigation,
    FilterSection(Group),
    FilterTagSearch(String),
    EditFilter(Group, String, bool),
    ApplyFilters,
    ResetFilters,
    RemoveFilter(Group, String),
    ClearFilters,
    Sort(String),
    Page(i32),
    GridResized(iced::Size),
    GridScrolled(usize),
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
    DeleteWallpaper,
    CancelDeleteWallpaper,
    ConfirmDeleteWallpaper,
    Open(String),
    Import(bool),
    Picked(Result<Option<String>, String>),
    Command(&'static str, Value),
    CommandDone(Result<Value, String>),
    Opened(Result<Value, String>),
    Property(String, Value),
    EditColor(String),
    ColorChanged([f32; 3]),
    SaveColor,
    CloseColor,
    ThemeLoaded(u64, Theme, Option<widget::image::Handle>),
    ThemeFrame(Instant),
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
    ThumbnailLoaded(u64, String, Option<widget::image::Handle>),
    RemotePreviewDeadline(u64),
    SizeLoaded(u64, String, Option<u64>),
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
    Previous,
    PlayPlaylist(Option<String>),
    ToggleQueue,
    PlayQueued(String),
    ShowPlaybackSource,
    PlaybackMode(String),
    AutoRotation(bool),
    PlaybackInterval(u64),
    Dismiss,
    ExpireNotice(Instant),
    Key(keyboard::Key, keyboard::Modifiers),
    Screenshot(window::Screenshot),
}

struct Notice {
    error: bool,
    message: String,
    expires_at: Instant,
}
impl Notice {
    fn new(error: bool, message: impl Into<String>) -> Self {
        Self {
            error,
            message: message.into(),
            expires_at: Instant::now() + Duration::from_secs(if error { 8 } else { 3 }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssetSize {
    Loading,
    Loaded(Option<u64>),
}

#[derive(Default)]
enum RemoteLoad {
    #[default]
    Idle,
    Searching,
    Preparing(RemoteResults),
}

struct RemoteResults {
    revision: u64,
    preview_deadline: Option<Instant>,
    items: Vec<Value>,
    page: usize,
    pages: usize,
}

struct ThemeFade {
    from: iced::Color,
    target: Theme,
    started: Instant,
}

struct App {
    page: Page,
    catalog: Vec<Value>,
    filtered_catalog: std::cell::OnceCell<Vec<usize>>,
    state: Value,
    library: Value,
    displays: Value,
    selected: Option<String>,
    delete_confirmation: Option<String>,
    properties: Value,
    drafts: BTreeMap<String, String>,
    color_editor: Option<color_picker::Editor>,
    theme: Theme,
    theme_fade: Option<ThemeFade>,
    theme_preview: Option<widget::image::Handle>,
    ambient: Option<widget::image::Handle>,
    theme_epoch: u64,
    search: String,
    scope: String,
    filters: Filters,
    filter_draft: Filters,
    filters_open: bool,
    sidebar_collapsed: bool,
    filter_sections: std::collections::BTreeSet<Group>,
    filter_tag_search: String,
    sort: String,
    page_index: usize,
    page_size: usize,
    output: String,
    fit: String,
    loading: bool,
    polling: bool,
    busy: bool,
    notice: Option<Notice>,
    service: Value,
    remote: Vec<Value>,
    remote_load: RemoteLoad,
    displayed_remote_page: usize,
    grid_columns: usize,
    grid_row: usize,
    recommendations: Vec<Value>,
    previews: previews::Previews,
    asset_sizes: BTreeMap<String, AssetSize>,
    size_epoch: u64,
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
    queue_open: bool,
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
    Task::perform(blocking(engine::load), Message::Loaded)
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
            filtered_catalog: std::cell::OnceCell::new(),
            state: Value::Null,
            library: json!({}),
            displays: json!({}),
            selected: None,
            delete_confirmation: None,
            properties: Value::Null,
            drafts: BTreeMap::new(),
            color_editor: None,
            theme: palette::default_theme(),
            theme_fade: None,
            theme_preview: None,
            ambient: None,
            theme_epoch: 0,
            search: String::new(),
            scope: "全部作品".into(),
            filters: Filters::default(),
            filter_draft: Filters::default(),
            filters_open: false,
            sidebar_collapsed: false,
            filter_sections: [Group::Kind, Group::Feature, Group::Tag].into(),
            filter_tag_search: String::new(),
            sort: if matches!(page, Page::Discover | Page::Workshop) {
                "热门"
            } else {
                "名称 A–Z"
            }
            .into(),
            page_index: 0,
            page_size: 12,
            output: "全部显示器".into(),
            fit: "填充".into(),
            loading: true,
            polling: false,
            busy: false,
            notice: None,
            service: json!({}),
            remote: vec![],
            remote_load: RemoteLoad::Idle,
            displayed_remote_page: 1,
            grid_columns: 4,
            grid_row: 0,
            recommendations: vec![],
            previews: previews::Previews::default(),
            asset_sizes: BTreeMap::new(),
            size_epoch: 0,
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
            queue_open: false,
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
    fn selected_local_asset(&self) -> Option<&Value> {
        let item = self.selected_asset()?;
        if !self.is_remote() {
            return Some(item);
        }
        if item["downloaded"] != true {
            return None;
        }
        self.catalog.iter().find(|local| {
            engine::workshop_id(std::path::Path::new(strv(local, "path"))) == Some(strv(item, "id"))
        })
    }
    fn size_task(&mut self) -> Task<Message> {
        if self.loading {
            return Task::none();
        }
        let Some(path) = self
            .selected_local_asset()
            .map(|item| strv(item, "path").to_owned())
            .filter(|path| !path.is_empty())
        else {
            return Task::none();
        };
        if self.asset_sizes.contains_key(&path) {
            return Task::none();
        }
        self.asset_sizes.insert(path.clone(), AssetSize::Loading);
        let epoch = self.size_epoch;
        Task::perform(
            blocking(move || {
                let size = engine::asset_size(std::path::Path::new(&path));
                (path, size)
            }),
            move |(path, size)| Message::SizeLoaded(epoch, path, size),
        )
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
    fn playback_targets(&self) -> Vec<String> {
        self.state["outputs"]
            .as_object()
            .into_iter()
            .flat_map(|outputs| outputs.keys())
            .filter(|name| self.output == "全部显示器" || **name == self.output)
            .cloned()
            .collect()
    }
    fn playback_mixed(&self) -> bool {
        let targets = self.playback_targets();
        let Some(first) = targets.first() else {
            return false;
        };
        targets.iter().skip(1).any(|name| {
            [
                "current",
                "paused",
                "effective_paused",
                "pause_reasons",
                "mute",
                "volume",
                "backend",
            ]
            .iter()
            .any(|key| self.state["outputs"][name][key] != self.state["outputs"][first][key])
                || self.state["rotations"][name]["settings"]
                    != self.state["rotations"][first]["settings"]
        })
    }
    fn playback_source(&self) -> &str {
        let source = self.state["rotations"]
            .get(&self.output)
            .or_else(|| {
                self.state["rotations"]
                    .as_object()
                    .and_then(|m| m.values().next())
            })
            .map(|rotation| strv(&rotation["settings"], "source"))
            .unwrap_or("");
        if source.is_empty() { "library" } else { source }
    }
    fn playback_members(&self, name: &str) -> Vec<String> {
        array(&self.state["rotations"][name]["members"])
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect()
    }

    fn playback_settings(&self, name: &str) -> Value {
        if self.state["rotations"][name]["settings"].is_object() {
            self.state["rotations"][name]["settings"].clone()
        } else {
            json!({"source":"library","enabled":false,"mode":"ordered","interval_seconds":1800,
                "fit":"cover","transition":if motion::reduced() {"cut"} else {"fade"},"schedule":[]})
        }
    }
    fn change_playback_setting(&mut self, key: &str, value: Value) -> Task<Message> {
        let targets = self.playback_targets();
        let tasks = targets
            .into_iter()
            .filter_map(|name| {
                let mut settings = self
                    .commands
                    .iter()
                    .rev()
                    .chain(self.active_command.iter())
                    .find(|(method, params)| *method == "set_rotation" && params["output"] == name)
                    .map(|(_, params)| params["rotation"].clone())
                    .unwrap_or_else(|| self.playback_settings(&name));
                if settings["source"] == "single" {
                    return None;
                }
                settings[key] = value.clone();
                Some(self.command(
                    "set_rotation",
                    json!({"output":name,"rotation":settings,"restart":false}),
                ))
            })
            .collect::<Vec<_>>();
        Task::batch(tasks)
    }
    fn apply_wallpaper(&mut self, id: String, source: Option<String>) -> Task<Message> {
        let mut params = self.target(json!({"asset_id":id,"fit":match self.fit.as_str(){
            "适应"=>"contain","拉伸"=>"stretch",_=>"cover"
        },"transition":if motion::reduced() {"cut"} else {"fade"}}));
        if let Some(source) = source {
            params["source"] = json!(source);
        }
        self.command("apply", params)
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
        let indices = self.filtered_catalog.get_or_init(|| {
            let query = self.search.trim().to_lowercase();
            let hidden = array(&self.library["hidden"])
                .iter()
                .filter_map(Value::as_str)
                .collect::<std::collections::BTreeSet<_>>();
            let mut indices: Vec<_> = self
                .catalog
                .iter()
                .enumerate()
                .filter_map(|(index, v)| {
                    let id = strv(v, "id");
                    (!hidden.contains(id)
                        && self.filters.matches(v)
                        && (query.is_empty()
                            || self.local_title(v).to_lowercase().contains(&query)))
                    .then_some(index)
                })
                .collect();
            indices
                .sort_by_cached_key(|&index| self.local_title(&self.catalog[index]).to_lowercase());
            if self.sort == "名称 Z–A" {
                indices.reverse();
            }
            if self.sort == "类型" {
                indices.sort_by_key(|&index| strv(&self.catalog[index], "kind"));
            }
            indices
        });
        indices.iter().map(|&index| &self.catalog[index]).collect()
    }
    fn notify(&mut self, result: Result<Value, String>) -> bool {
        match result {
            Ok(value) => {
                if let Some(warning) = value["warning"].as_str() {
                    self.notice = Some(Notice::new(true, warning));
                } else if !self.notice.as_ref().is_some_and(|notice| notice.error) {
                    self.notice = Some(Notice::new(
                        false,
                        value["message"].as_str().unwrap_or("已保存"),
                    ));
                }
                true
            }
            Err(e) => {
                self.notice = Some(Notice::new(true, e));
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
            blocking(move || {
                if method == "trash_wallpaper" {
                    engine::trash_wallpaper(strv(&params, "asset_id"))
                } else {
                    engine::request(method, params)
                }
            }),
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
        Task::perform(blocking(move || engine::properties(params)), move |v| {
            Message::Properties(id.clone(), output.clone(), epoch, v)
        })
    }
    fn service_task(&mut self, mut action: Value) -> Task<Message> {
        if action.get("_revision").is_none() {
            action["_revision"] = json!(self.search_revision);
        }
        if self.service_busy {
            if matches!(action["action"].as_str(), Some("search" | "detail")) {
                self.service_commands
                    .retain(|v| v["action"] != action["action"]);
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
        self.remote_load = RemoteLoad::Searching;
        self.previews.cancel_pending();
        self.service_task(json!({"action":"search","text":self.search,"filters":self.filters,"scope":if self.page==Page::Workshop{self.scope.as_str()}else{"全部作品"},"sort":self.sort,"page":self.remote_page,"source":if self.page==Page::Discover && !self.filters.tag_groups().iter().any(|group| group.len() > 1){"discover"}else{"steam"}}))
    }
    fn remote_loading(&self) -> bool {
        !matches!(self.remote_load, RemoteLoad::Idle)
    }

    fn publish_remote_if_ready(&mut self) -> Task<Message> {
        let mut timer = None;
        let ready = match &mut self.remote_load {
            RemoteLoad::Preparing(next) if next.revision == self.search_revision => {
                let row = &next.items[..next.items.len().min(self.grid_columns.max(1))];
                let ready = row
                    .iter()
                    .filter(|item| matches!(self.previews.get(item), Some(Some(_))))
                    .count();
                // A slow thumbnail must not hold the whole query. Retain the old
                // grid until at least one image arrives rather than flashing blanks.
                let complete = ready * 3 >= row.len() * 2
                    || row.iter().all(|item| self.previews.finished(item))
                    || (ready > 0
                        && next
                            .preview_deadline
                            .is_some_and(|deadline| Instant::now() >= deadline));
                if !complete && ready > 0 && next.preview_deadline.is_none() {
                    let deadline = Instant::now() + Duration::from_millis(800);
                    next.preview_deadline = Some(deadline);
                    timer = Some((next.revision, deadline));
                }
                complete
            }
            _ => false,
        };
        if !ready {
            return match timer {
                Some((revision, deadline)) => Task::perform(
                    async move {
                        tokio::time::sleep_until(deadline.into()).await;
                        revision
                    },
                    Message::RemotePreviewDeadline,
                ),
                None => Task::none(),
            };
        }
        let RemoteLoad::Preparing(next) =
            std::mem::replace(&mut self.remote_load, RemoteLoad::Idle)
        else {
            unreachable!()
        };
        self.remote = next.items;
        self.remote_pages = next.pages;
        self.remote_page = next.page;
        self.displayed_remote_page = next.page;
        self.grid_row = 0;
        Task::batch([
            widget::operation::snap_to("wallpaper-grid", widget::operation::RelativeOffset::START),
            self.previews.load(self.preview_items()),
        ])
    }

    fn accept_service(&mut self, value: Value) {
        if let Some(states) = value["item_states"].as_object() {
            let pending = match &mut self.remote_load {
                RemoteLoad::Preparing(next) => next.items.as_mut_slice(),
                _ => &mut [],
            };
            for item in self
                .remote
                .iter_mut()
                .chain(&mut self.recommendations)
                .chain(pending)
            {
                if let Some(state) = states.get(strv(item, "id"))
                    && let (Some(item), Some(state)) = (item.as_object_mut(), state.as_object())
                {
                    item.extend(state.clone());
                }
            }
        }
        self.service = value;
    }
    fn preview_items(&self) -> Vec<Value> {
        let mut items: Vec<Value> = match self.page {
            Page::Library => {
                let filtered = self.filtered();
                let page = self
                    .page_index
                    .min(filtered.len().div_ceil(self.page_size).saturating_sub(1));
                filtered
                    .into_iter()
                    .skip(page * self.page_size)
                    .take(self.page_size)
                    .cloned()
                    .collect()
            }
            Page::Discover | Page::Workshop => {
                let (items, row) = match &self.remote_load {
                    RemoteLoad::Preparing(next) => (&next.items, 0),
                    RemoteLoad::Searching => {
                        return self
                            .remote
                            .iter()
                            .filter(|item| self.previews.get(item).is_some())
                            .cloned()
                            .collect();
                    }
                    RemoteLoad::Idle => (&self.remote, self.grid_row),
                };
                let first = row * self.grid_columns;
                items
                    .iter()
                    .skip(first)
                    .take(self.page_size + self.grid_columns)
                    .chain(
                        items
                            .iter()
                            .skip(first.saturating_sub(self.grid_columns))
                            .take(first.min(self.grid_columns)),
                    )
                    .cloned()
                    .collect()
            }
            Page::Playlists => array(&self.library["playlists"][&self.playlist]["members"])
                .iter()
                .filter_map(|id| self.catalog.iter().find(|item| item["id"] == *id).cloned())
                .collect(),
            _ => Vec::new(),
        };
        let mut priority = if matches!(self.page, Page::Discover | Page::Workshop) {
            let first_row = items.len().min(self.grid_columns);
            items.drain(..first_row).collect()
        } else {
            Vec::new()
        };
        if let Some(item) = self.selected_asset() {
            priority.push(item.clone());
        }
        if let Some(item) = self
            .catalog
            .iter()
            .find(|item| item["id"] == self.current_output()["current"])
        {
            priority.push(item.clone());
        }
        if self.page == Page::Discover {
            priority.extend(self.recommendations.iter().take(3).cloned());
        }
        priority.extend(items);
        priority
    }
    fn theme_task(&mut self) -> Task<Message> {
        let handle = self
            .catalog
            .iter()
            .find(|item| item["id"] == self.current_output()["current"])
            .and_then(|item| self.previews.get(item))
            .and_then(Clone::clone);
        if self.theme_preview == handle {
            return Task::none();
        }
        self.theme_preview = handle.clone();
        self.theme_epoch += 1;
        let epoch = self.theme_epoch;
        if let Some(handle) = handle {
            Task::perform(
                blocking(move || {
                    (
                        palette::from_preview(handle.clone()),
                        palette::ambient(&handle),
                    )
                }),
                move |(theme, ambient)| Message::ThemeLoaded(epoch, theme, ambient),
            )
        } else {
            self.theme = palette::default_theme();
            self.theme_fade = None;
            self.ambient = None;
            Task::none()
        }
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
        if matches!(
            &message,
            Message::Select(_)
                | Message::CloseDetail
                | Message::Output(_)
                | Message::Navigate(_)
                | Message::DeleteWallpaper
                | Message::ResetProperties
                | Message::LoadPreset(_)
        ) {
            self.color_editor = None;
        }
        if matches!(
            &message,
            Message::Loaded(Ok(_)) | Message::Refresh | Message::RefreshLibrary
        ) {
            self.size_epoch += 1;
            self.asset_sizes.clear();
        }
        if matches!(
            &message,
            Message::Navigate(_)
                | Message::Search(_)
                | Message::ApplyFilters
                | Message::RemoveFilter(..)
                | Message::ClearFilters
                | Message::Sort(_)
                | Message::Loaded(_)
        ) {
            self.filtered_catalog.take();
        }
        let refresh_previews = matches!(
            &message,
            Message::Navigate(_)
                | Message::Search(_)
                | Message::ApplyFilters
                | Message::RemoveFilter(..)
                | Message::ClearFilters
                | Message::Sort(_)
                | Message::Page(_)
                | Message::GridResized(_)
                | Message::Loaded(_)
                | Message::Select(_)
                | Message::CloseDetail
                | Message::Polled(_)
                | Message::CommandDone(_)
                | Message::ServiceDone(..)
                | Message::ExploreLoaded(_)
                | Message::PlaylistSelect(_)
                | Message::Refresh
                | Message::RefreshLibrary
                | Message::Output(_)
        );
        let task = self.update_inner(message);
        if self.color_editor.is_some() && !self.can_edit_properties() {
            self.color_editor = None;
        }
        if refresh_previews {
            Task::batch([
                task,
                self.previews.load(self.preview_items()),
                self.size_task(),
                self.theme_task(),
            ])
        } else {
            Task::batch([task, self.theme_task()])
        }
    }
    fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Navigate(page) => {
                self.search_revision += 1;
                self.remote_load = RemoteLoad::Idle;
                self.grid_row = 0;
                self.service_commands
                    .retain(|action| !matches!(strv(action, "action"), "search" | "detail"));
                self.page = page;
                self.page_index = 0;
                self.search.clear();
                self.filters = Filters::default();
                self.filter_draft = Filters::default();
                self.filter_tag_search.clear();
                self.filters_open = false;
                self.selected = None;
                self.delete_confirmation = None;
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

            Message::ToggleSidebar => self.sidebar_collapsed = !self.sidebar_collapsed,
            Message::ToggleFilters => self.filters_open = !self.filters_open,
            Message::ShowNavigation => {
                self.filters_open = false;
                self.sidebar_collapsed = false;
            }
            Message::FilterSection(group) => {
                if !self.filter_sections.remove(&group) {
                    self.filter_sections.insert(group);
                }
            }
            Message::FilterTagSearch(value) => self.filter_tag_search = value,
            Message::EditFilter(group, value, enabled) => {
                self.filter_draft.set(group, value, enabled)
            }
            Message::ResetFilters => self.filter_draft = Filters::default(),
            Message::ApplyFilters | Message::ClearFilters | Message::RemoveFilter(..) => {
                match message {
                    Message::ApplyFilters => self.filters = self.filter_draft.clone(),
                    Message::ClearFilters => {
                        self.filters = Filters::default();
                        self.filter_draft = Filters::default();
                    }
                    Message::RemoveFilter(group, value) => {
                        self.filters.set(group, value.clone(), false);
                        self.filter_draft.set(group, value, false);
                    }
                    _ => unreachable!(),
                }
                self.page_index = 0;
                self.grid_row = 0;
                if matches!(self.page, Page::Discover | Page::Workshop) {
                    self.remote_page = 1;
                    return self.search_task();
                }
                return widget::operation::scroll_to(
                    "wallpaper-grid",
                    widget::scrollable::AbsoluteOffset::<f32>::default(),
                );
            }

            Message::Sort(v) => {
                self.sort = v;
                self.page_index = 0;
                self.remote_page = 1;
                if matches!(self.page, Page::Discover | Page::Workshop) {
                    return self.search_task();
                }
            }

            Message::GridResized(size) => {
                self.grid_columns = view::grid_columns(size.width);
                let page_size = view::grid_page_size(size);
                if page_size != self.page_size {
                    let first = self.page_index * self.page_size;
                    self.page_size = page_size;
                    self.page_index = (first / page_size)
                        .min(self.filtered().len().div_ceil(page_size).saturating_sub(1));
                }
                return self.publish_remote_if_ready();
            }

            Message::GridScrolled(row) => {
                if row != self.grid_row {
                    self.grid_row = row;
                    return self.previews.load(self.preview_items());
                }
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
                    self.previews.retry_failed();
                    return self.search_task();
                }
                self.previews.clear();
                self.loading = true;
                return load_task();
            }

            Message::Loaded(result) => {
                self.loading = false;
                match result {
                    Ok(v) => {
                        self.previews.clear();
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
                    Err(e) => self.notice = Some(Notice::new(true, e)),
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
                        && self.previews.pending == 0
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
                        if !self.busy && self.library != self.state["library"] {
                            self.filtered_catalog.take();
                            self.library = self.state["library"].clone();
                        }

                        if self.output != "全部显示器"
                            && self.state["outputs"][&self.output].is_null()
                        {
                            self.output = "全部显示器".into();
                            self.notice =
                                Some(Notice::new(false, "显示器已断开，已保留它的壁纸设置。"));
                        }
                    }
                    Err(e) => {
                        if self.notice.is_none() {
                            self.notice = Some(Notice::new(true, e));
                        }
                    }
                }
            }

            Message::Select(id) => {
                self.delete_confirmation = None;
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
                        Err(e) => self.notice = Some(Notice::new(true, e)),
                    }
                }
            }

            Message::CloseDetail => {
                self.selected = None;
                self.delete_confirmation = None;
            }
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
                    let source = (self.page == Page::Playlists
                        && array(&self.library["playlists"][&self.playlist]["members"])
                            .iter()
                            .any(|member| member.as_str() == Some(&id)))
                    .then(|| self.playlist.clone());
                    return self.apply_wallpaper(id, source);
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

            Message::DeleteWallpaper => {
                if !self.busy
                    && !self.loading
                    && !self.is_remote()
                    && let Some(item) = self.selected_asset()
                {
                    self.delete_confirmation = Some(strv(item, "id").into());
                }
            }
            Message::CancelDeleteWallpaper => self.delete_confirmation = None,
            Message::ConfirmDeleteWallpaper => {
                if self.busy {
                    return Task::none();
                }
                if let Some(id) = self.delete_confirmation.take()
                    && self.selected.as_deref() == Some(&id)
                    && self.catalog.iter().any(|item| strv(item, "id") == id)
                {
                    self.notice = None;
                    return self.command("trash_wallpaper", json!({"asset_id":id}));
                }
            }

            Message::Open(path) => {
                return Task::perform(
                    async move { engine::open(&path).await.map(|_| json!({})) },
                    Message::Opened,
                );
            }
            Message::Import(directory) => {
                return Task::perform(engine::pick(directory), Message::Picked);
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
                Err(e) => self.notice = Some(Notice::new(true, e)),
            },
            Message::Command(method, params) => return self.command(method, params),
            Message::Opened(result) => {
                if let Err(error) = result {
                    self.notice = Some(Notice::new(true, error));
                }
            }

            Message::CommandDone(result) => {
                self.busy = false;
                let active = self.active_command.take();
                let deleting = active.as_ref().filter(|(m, _)| *m == "trash_wallpaper");
                if let Ok(reply) = &result {
                    if let Some((_, params)) = deleting {
                        let id = strv(params, "asset_id");
                        self.filtered_catalog.take();
                        self.catalog.retain(|item| strv(item, "id") != id);
                        if self.selected.as_deref() == Some(id) {
                            self.selected = None;
                            self.properties = Value::Null;
                            self.drafts.clear();
                            self.property_epoch += 1;
                        }
                        if let Some(service) = reply.get("service") {
                            self.accept_service(service.clone());
                        }
                    }
                    if self.commands.is_empty()
                        && let Some(library) = reply.get("library")
                        && self.library != *library
                    {
                        self.filtered_catalog.take();
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
                if deleting.is_some() && result.is_ok() {
                    self.page_index = self.page_index.min(
                        self.filtered()
                            .len()
                            .div_ceil(self.page_size)
                            .saturating_sub(1),
                    );
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

                if deleting.is_some() {
                    return Task::batch([load_task(), self.properties_task()]);
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

            Message::ThemeLoaded(epoch, theme, ambient) => {
                if epoch == self.theme_epoch {
                    self.ambient = ambient;
                    if motion::reduced() || self.theme.palette().primary == theme.palette().primary
                    {
                        self.theme = theme;
                        self.theme_fade = None;
                    } else {
                        self.theme_fade = Some(ThemeFade {
                            from: self.theme.palette().primary,
                            target: theme,
                            started: Instant::now(),
                        });
                    }
                }
            }
            Message::ThemeFrame(now) => {
                if let Some(fade) = &self.theme_fade {
                    let progress = now.saturating_duration_since(fade.started).as_secs_f32() / 0.24;
                    if progress >= 1.0 || motion::reduced() {
                        self.theme = self.theme_fade.take().unwrap().target;
                    } else {
                        let mut colors = fade.target.palette();
                        colors.primary = motion::linear_color(
                            fade.from,
                            colors.primary,
                            motion::smoothstep(progress),
                        );
                        colors.success = colors.primary;
                        self.theme = Theme::custom("Misari", colors);
                    }
                }
            }
            Message::EditColor(key) => {
                if self.can_edit_properties()
                    && array(&self.properties["definitions"]).iter().any(|def| {
                        strv(def, "key") == key && def["type"] == "color" && def["editable"] == true
                    })
                {
                    self.color_editor = Some(color_picker::Editor::new(
                        key.clone(),
                        &self.property_value(&key),
                    ));
                    return widget::operation::focus("color-field");
                }
            }
            Message::ColorChanged(hsv) => {
                if let Some(editor) = &mut self.color_editor {
                    editor.hsv = hsv.map(|v| {
                        if v.is_finite() {
                            v.clamp(0.0, 1.0)
                        } else {
                            0.0
                        }
                    });
                }
            }
            Message::SaveColor => {
                if let Some(editor) = self.color_editor.take()
                    && self.can_edit_properties()
                {
                    let focus = format!("color-swatch-{}", editor.key);
                    return self
                        .update(Message::Property(
                            editor.key,
                            json!(color_picker::rgb(editor.hsv)),
                        ))
                        .chain(widget::operation::focus(focus));
                }
            }
            Message::CloseColor => {
                if let Some(editor) = self.color_editor.take() {
                    return widget::operation::focus(format!("color-swatch-{}", editor.key));
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
                let value = if ty == "slider" {
                    match serde_json::from_str::<Value>(&value) {
                        Ok(v) => v,
                        Err(_) => {
                            self.notice = Some(Notice::new(true, "请输入有效数值。"));
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
                return Task::perform(engine::pick(directory), move |v| {
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
                    Err(e) => self.notice = Some(Notice::new(true, e)),
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
                            self.remote_load = RemoteLoad::Preparing(RemoteResults {
                                revision,
                                preview_deadline: None,
                                items: array(&v["items"]).to_vec(),
                                pages: v["total_pages"].as_u64().unwrap_or(1).max(1) as usize,
                                page: v["page"].as_u64().unwrap_or(self.remote_page as u64)
                                    as usize,
                            });
                            tasks.push(self.publish_remote_if_ready());
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
                        } else {
                            self.notice = Some(Notice::new(
                                false,
                                v["message"].as_str().unwrap_or("操作已完成"),
                            ));
                        }

                        if v.get("account").is_some() {
                            self.accept_service(v);
                        }
                    }
                    Err(e) if kind != "search" || revision == self.search_revision => {
                        if kind == "search" {
                            self.remote_load = RemoteLoad::Idle;
                        }
                        self.notice = Some(Notice::new(true, e));
                    }
                    Err(_) => {}
                }

                if let Some(action) = self.service_commands.pop_front() {
                    tasks.push(self.service_task(action));
                }
                return Task::batch(tasks);
            }

            Message::ExploreLoaded(result) => {
                if let Ok(value) = result {
                    self.recommendations = array(&value["items"]).to_vec();
                }
            }

            Message::SizeLoaded(epoch, path, size) => {
                if epoch == self.size_epoch {
                    self.asset_sizes.insert(path, AssetSize::Loaded(size));
                }
            }

            Message::RemotePreviewDeadline(revision) => {
                if revision == self.search_revision {
                    return self.publish_remote_if_ready();
                }
            }
            Message::ThumbnailLoaded(generation, source, handle) => {
                self.previews.accept(generation, source, handle);
                return self.publish_remote_if_ready();
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
                if !enabled {
                    let tasks = self
                        .playback_targets()
                        .into_iter()
                        .filter_map(|name| {
                            let mut settings = self.playback_settings(&name);
                            if settings["source"] != self.playlist {
                                return None;
                            }
                            settings["enabled"] = json!(false);
                            Some(self.command(
                                "set_rotation",
                                json!({"output":name,"rotation":settings,"restart":false}),
                            ))
                        })
                        .collect::<Vec<_>>();
                    return Task::batch(tasks);
                }
                let seconds = match self
                    .rotation_interval
                    .parse::<u64>()
                    .ok()
                    .and_then(|v| v.checked_mul(60))
                    .filter(|v| (10..=86400).contains(v))
                {
                    Some(v) => v,
                    None => {
                        self.notice = Some(Notice::new(true, "轮换周期需要是 1–1440 分钟。"));
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
,"interval_seconds":seconds,"fit":"cover","transition":if motion::reduced() { "cut" } else { "fade" },"schedule":schedule
}

}
)));
            }

            Message::Next | Message::Previous => {
                let previous = matches!(message, Message::Previous);
                let targets = self.playback_targets();
                let tasks = targets
                    .into_iter()
                    .filter_map(|name| {
                        if self.state["outputs"][&name]["current"]
                            .as_str()
                            .is_none_or(str::is_empty)
                            || self.playback_members(&name).is_empty()
                            || previous && self.state["rotations"][&name]["can_previous"] == false
                        {
                            return None;
                        }
                        Some(self.command(
                            if previous {
                                "rotation_previous"
                            } else {
                                "rotation_next"
                            },
                            json!({"output":name}),
                        ))
                    })
                    .collect::<Vec<_>>();
                return Task::batch(tasks);
            }
            Message::PlayPlaylist(id) => {
                let id = id.or_else(|| {
                    array(&self.library["playlists"][&self.playlist]["members"])
                        .iter()
                        .filter_map(Value::as_str)
                        .find(|id| {
                            self.catalog
                                .iter()
                                .any(|item| strv(item, "id") == *id && item["error"].is_null())
                                && !array(&self.library["hidden"])
                                    .iter()
                                    .any(|hidden| hidden.as_str() == Some(*id))
                        })
                        .map(String::from)
                });
                if let Some(id) = id {
                    return self.apply_wallpaper(id, Some(self.playlist.clone()));
                }
            }
            Message::ToggleQueue => self.queue_open = !self.queue_open,
            Message::PlayQueued(id) => {
                if !self.playback_mixed() {
                    let name = self
                        .playback_targets()
                        .into_iter()
                        .next()
                        .unwrap_or_default();
                    let settings = self.playback_settings(&name);
                    return self.command("apply", self.target(json!({
                        "asset_id":id,"source":self.playback_source(),"fit":settings["fit"],
                        "transition":if motion::reduced() { json!("cut") } else { settings["transition"].clone() }
                    })));
                }
            }
            Message::ShowPlaybackSource => {
                if !self.playback_mixed() {
                    let source = self.playback_source().to_owned();
                    let current = strv(self.current_output(), "current").to_owned();
                    self.queue_open = false;
                    if self.library["playlists"][&source].is_object() {
                        self.select_playlist(source);
                        let index = array(&self.library["playlists"][&self.playlist]["members"])
                            .iter()
                            .position(|id| id.as_str() == Some(&current))
                            .unwrap_or(0);
                        let navigate = self.update(Message::Navigate(Page::Playlists));
                        return navigate
                            .chain(widget::operation::focus(format!("playlist-item-{index}")))
                            .chain(controls::scroll_to_focused());
                    }
                    let navigate = self.update(Message::Navigate(Page::Library));
                    if !current.is_empty() {
                        self.page_index = self
                            .filtered()
                            .iter()
                            .position(|item| strv(item, "id") == current)
                            .unwrap_or(0)
                            / self.page_size;
                        return Task::batch([navigate, self.update(Message::Select(current))]);
                    }
                    return navigate;
                }
            }
            Message::PlaybackMode(mode) => {
                return self.change_playback_setting(
                    "mode",
                    json!(if mode == "随机挑选" {
                        "random"
                    } else {
                        "ordered"
                    }),
                );
            }
            Message::AutoRotation(enabled) => {
                return self.change_playback_setting("enabled", json!(enabled));
            }
            Message::PlaybackInterval(minutes) => {
                return self.change_playback_setting("interval_seconds", json!(minutes * 60));
            }

            Message::Dismiss => self.notice = None,
            Message::ExpireNotice(now) => {
                if self
                    .notice
                    .as_ref()
                    .is_some_and(|notice| now >= notice.expires_at)
                {
                    self.notice = None;
                }
            }
            Message::Key(key, modifiers) => match key {
                keyboard::Key::Named(keyboard::key::Named::Tab) => {
                    return if modifiers.shift() {
                        controls::focus_previous()
                    } else {
                        controls::focus_next()
                    };
                }
                keyboard::Key::Named(keyboard::key::Named::Escape) => {
                    if self.color_editor.is_some() {
                        return self.update(Message::CloseColor);
                    }
                    if self.queue_open {
                        self.queue_open = false;
                        return Task::none();
                    }
                    if self.filters_open {
                        self.filters_open = false;
                        return Task::none();
                    }
                    if self.delete_confirmation.take().is_none() {
                        self.selected = None;
                        self.notice = None;
                    }
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
                    let paused = !self
                        .playback_targets()
                        .iter()
                        .all(|name| self.state["outputs"][name]["paused"] == true);
                    return self.update(Message::Playback(json!({
                    "paused":paused
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
            if self.theme_fade.is_some() {
                window::frames().map(Message::ThemeFrame)
            } else {
                Subscription::none()
            },
            if self.notice.is_some() {
                iced::time::every(Duration::from_millis(250)).map(Message::ExpireNotice)
            } else {
                Subscription::none()
            },
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
    motion::set_reduced(engine::reduced_motion());
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
        .theme(|app: &App| app.theme.clone())
        .default_font(iced::Font::with_name("Noto Sans CJK SC"))
        .font(icons::FONT_BYTES)
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
