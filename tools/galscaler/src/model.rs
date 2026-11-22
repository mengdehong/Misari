use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub fn validate(self) -> Result<()> {
        ensure!(
            (1..=16384).contains(&self.width) && (1..=16384).contains(&self.height),
            "分辨率的宽高必须在 1–16384 之间"
        );
        Ok(())
    }
}

impl FromStr for Resolution {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (width, height) = value
            .split_once(['x', 'X', '×'])
            .context("分辨率格式应为 1280x720")?;
        let size = Self {
            width: width.parse().context("无效的画面宽度")?,
            height: height.parse().context("无效的画面高度")?,
        };
        size.validate()?;
        Ok(size)
    }
}

#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    /// 无滤镜，使用基础缩放。
    #[serde(alias = "nearest", alias = "fsr", alias = "nis")]
    Linear,
    #[default]
    Cunny,
    Anime4k,
}

impl Filter {
    pub fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Cunny => "cunny",
            Self::Anime4k => "anime4k",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Enhancement {
    pub enabled: bool,
    pub filter: Filter,
    pub source: Resolution,
    pub output: Resolution,
    pub fullscreen: bool,
    pub fps: u16,
}

impl Default for Enhancement {
    fn default() -> Self {
        Self {
            enabled: true,
            filter: Filter::Cunny,
            source: Resolution {
                width: 1280,
                height: 720,
            },
            output: Resolution {
                width: 2560,
                height: 1440,
            },
            fullscreen: false,
            fps: 60,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PictureOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Resolution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Resolution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<u16>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub proton: Option<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub enhancement: Enhancement,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Overrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proton: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<BTreeMap<String, String>>,
    pub enhancement: PictureOverrides,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Prefix {
    Shared,
    // Old library entries already own a per-game prefix.
    #[default]
    Independent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game<C = Overrides> {
    pub id: u64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<u64>,
    pub executable: PathBuf,
    pub cover: Option<PathBuf>,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub prefix: Prefix,
    #[serde(flatten)]
    pub settings: C,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, clap::Args)]
#[serde(default, deny_unknown_fields)]
pub struct Changes {
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub executable: Option<PathBuf>,
    #[arg(long)]
    pub proton: Option<String>,
    #[arg(long, value_enum)]
    pub prefix: Option<Prefix>,
    #[arg(long, conflicts_with = "clear_cover")]
    pub cover: Option<PathBuf>,
    #[arg(long)]
    pub clear_cover: bool,
    /// 开启实时增强；关闭时直接通过 umu/Proton 运行。
    #[arg(long)]
    pub enabled: Option<bool>,
    #[arg(long, value_enum)]
    pub filter: Option<Filter>,
    /// 游戏可见的虚拟屏幕尺寸，例如 1280x720。
    #[arg(long)]
    pub source: Option<Resolution>,
    /// 增强处理的目标像素尺寸；原生模式下窗口缩放不改变处理分辨率。
    #[arg(long)]
    pub output: Option<Resolution>,
    #[arg(long)]
    pub fullscreen: Option<bool>,
    #[arg(long)]
    pub fps: Option<u16>,
    /// Replace game arguments; pass an empty JSON array to clear them.
    #[arg(long, value_parser = json_arguments)]
    // Qualified Vec makes clap parse one JSON value rather than repeated flags.
    pub arguments: Option<std::vec::Vec<String>>,
    /// Replace environment overrides, e.g. '{"LC_ALL":"ja_JP.UTF-8"}'.
    #[arg(long, value_parser = json_environment)]
    pub environment: Option<BTreeMap<String, String>>,
    #[arg(skip)]
    pub overrides: Option<Overrides>,
}

fn json_arguments(value: &str) -> Result<Vec<String>, String> {
    serde_json::from_str(value).map_err(|e| e.to_string())
}

fn json_environment(value: &str) -> Result<BTreeMap<String, String>, String> {
    serde_json::from_str(value).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Library {
    next_id: u64,
    pub settings: Settings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_proton: Option<PathBuf>,
    pub games: Vec<Game>,
}

impl Default for Library {
    fn default() -> Self {
        Self {
            next_id: 1,
            settings: Settings::default(),
            shared_proton: None,
            games: Vec::new(),
        }
    }
}

pub fn file(path: &Path) -> Result<PathBuf> {
    let path =
        fs::canonicalize(path).with_context(|| format!("无法找到文件：{}", path.display()))?;
    ensure!(path.is_file(), "不是文件：{}", path.display());
    ensure!(path.to_str().is_some(), "文件路径必须是有效的 UTF-8");
    Ok(path)
}

pub fn executable(path: &Path) -> Result<PathBuf> {
    let path = file(path)?;
    ensure!(
        path.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe")),
        "请选择 Windows 游戏的 .exe 启动文件"
    );
    Ok(path)
}

fn local_cover(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let folder = dir.file_name()?.to_str()?;
    let stem = exe.file_stem()?.to_str()?;
    let files: Vec<_> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    let names = ["cover", "poster", folder, stem];
    for extensions in [&["png", "jpg", "jpeg", "webp"][..], &["ico"][..]] {
        for name in names {
            for extension in extensions {
                let name = format!("{name}.{extension}");
                if let Some(path) = files
                    .iter()
                    .find(|p| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(&name)))
                {
                    return Some(path.clone());
                }
            }
        }
    }
    let mut icons = files
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ico")));
    let icon = icons.next()?;
    icons.next().is_none().then_some(icon)
}

pub fn save_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = path.with_extension("json.new");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path).context("无法保存引擎数据")
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        self.enhancement.source.validate()?;
        self.enhancement.output.validate()?;
        ensure!(
            (1..=360).contains(&self.enhancement.fps),
            "帧率必须在 1–360 之间"
        );
        for (key, value) in &self.environment {
            ensure!(
                !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'),
                "无效的环境变量：{key}"
            );
            ensure!(
                !matches!(
                    key.as_str(),
                    "WINEPREFIX" | "PROTONPATH" | "GAMEID" | "PROTON_VERB"
                ),
                "{key} 由引擎管理，请使用对应的游戏配置"
            );
        }
        Ok(())
    }
}

impl Game {
    pub fn effective(&self, library: &Library) -> Game<Settings> {
        let defaults = &library.settings;
        let e = &self.settings.enhancement;
        let d = &defaults.enhancement;
        let proton = match (self.prefix, &library.shared_proton) {
            (Prefix::Shared, Some(proton)) => Some(proton),
            _ => defaults.proton.as_ref(),
        };
        Game {
            id: self.id,
            name: self.name.clone(),
            added_at: self.added_at,
            executable: self.executable.clone(),
            cover: self.cover.clone(),
            arguments: self.arguments.clone(),
            prefix: self.prefix,
            settings: Settings {
                proton: self.settings.proton.as_ref().or(proton).cloned(),
                environment: self
                    .settings
                    .environment
                    .as_ref()
                    .unwrap_or(&defaults.environment)
                    .clone(),
                enhancement: Enhancement {
                    enabled: e.enabled.unwrap_or(d.enabled),
                    filter: e.filter.unwrap_or(d.filter),
                    source: e.source.unwrap_or(d.source),
                    output: e.output.unwrap_or(d.output),
                    fullscreen: e.fullscreen.unwrap_or(d.fullscreen),
                    fps: e.fps.unwrap_or(d.fps),
                },
            },
        }
    }

    pub fn apply(&mut self, changes: Changes) -> Result<()> {
        if let Some(name) = changes.name {
            self.name = name.trim().into();
        }
        if let Some(path) = changes.executable {
            self.executable = executable(&path)?;
        }
        if let Some(cover) = changes.cover {
            self.cover = Some(file(&cover)?);
        }
        if changes.clear_cover {
            self.cover = None;
        }
        if let Some(arguments) = changes.arguments {
            self.arguments = arguments;
        }
        if let Some(prefix) = changes.prefix {
            self.prefix = prefix;
        }
        if let Some(overrides) = changes.overrides {
            self.settings = overrides;
        }
        if let Some(environment) = changes.environment {
            self.settings.environment = Some(environment);
        }
        let e = &mut self.settings.enhancement;
        e.enabled = changes.enabled.or(e.enabled);
        e.filter = changes.filter.or(e.filter);
        e.source = changes.source.or(e.source);
        e.output = changes.output.or(e.output);
        e.fullscreen = changes.fullscreen.or(e.fullscreen);
        e.fps = changes.fps.or(e.fps);
        self.validate()
    }
}

impl<C> Game<C> {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.name.trim().is_empty(), "游戏名称不能为空");
        ensure!(
            self.arguments.iter().all(|a| !a.contains('\0')),
            "启动参数不能包含空字符"
        );
        Ok(())
    }
}

impl Library {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Self::decode(&bytes).context("无法读取游戏库，原文件已保留"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("无法读取游戏库"),
        }
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
        let legacy = value.get("settings").is_none();
        let remove_sharpness = |settings: &mut serde_json::Value| {
            if let Some(enhancement) = settings
                .get_mut("enhancement")
                .and_then(serde_json::Value::as_object_mut)
            {
                enhancement.remove("sharpness");
            }
        };
        if let Some(settings) = value.get_mut("settings") {
            remove_sharpness(settings);
        }
        if let Some(games) = value
            .get_mut("games")
            .and_then(serde_json::Value::as_array_mut)
        {
            games.iter_mut().for_each(remove_sharpness);
        }
        let mut library: Self = serde_json::from_value(value)?;
        if legacy {
            library.migrate();
        }
        Ok(library)
    }

    fn migrate(&mut self) {
        // Promote a shared value only if a majority of the old library used it.
        fn promote<T: Clone + Ord>(
            games: &mut [Game],
            default: T,
            field: impl Fn(&mut Game) -> &mut Option<T>,
        ) -> T {
            let mut counts = BTreeMap::new();
            for game in games.iter_mut() {
                *counts
                    .entry(field(game).get_or_insert_with(|| default.clone()).clone())
                    .or_insert(0) += 1;
            }
            let value = counts
                .into_iter()
                .find(|&(_, count)| count >= 2 && count > games.len() / 2)
                .map_or(default, |(value, _)| value);
            for game in games {
                let slot = field(game);
                if slot.as_ref() == Some(&value) {
                    *slot = None;
                }
            }
            value
        }

        let e = &mut self.settings.enhancement;
        e.enabled = promote(&mut self.games, e.enabled, |g| {
            &mut g.settings.enhancement.enabled
        });
        e.filter = promote(&mut self.games, e.filter, |g| {
            &mut g.settings.enhancement.filter
        });
        e.source = promote(&mut self.games, e.source, |g| {
            &mut g.settings.enhancement.source
        });
        e.output = promote(&mut self.games, e.output, |g| {
            &mut g.settings.enhancement.output
        });
        e.fullscreen = promote(&mut self.games, e.fullscreen, |g| {
            &mut g.settings.enhancement.fullscreen
        });
        e.fps = promote(&mut self.games, e.fps, |g| &mut g.settings.enhancement.fps);

        let mut counts = BTreeMap::new();
        for game in &mut self.games {
            if let Some(path) = &game.settings.proton {
                *counts.entry(path.clone()).or_insert(0) += 1;
            }
            if game
                .settings
                .environment
                .as_ref()
                .is_some_and(BTreeMap::is_empty)
            {
                game.settings.environment = None;
            }
        }
        if let Some((path, _)) = counts
            .into_iter()
            .find(|&(_, count)| count >= 2 && count > self.games.len() / 2)
        {
            for game in &mut self.games {
                if game.settings.proton.as_ref() == Some(&path) {
                    game.settings.proton = None;
                }
            }
            self.settings.proton = Some(path);
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        save_json(path, self)
    }

    pub fn get(&self, id: u64) -> Result<&Game> {
        self.games
            .iter()
            .find(|g| g.id == id)
            .with_context(|| format!("游戏 {id} 不在库中"))
    }

    pub fn add(
        &mut self,
        path: &Path,
        name: Option<String>,
        proton: Option<PathBuf>,
    ) -> Result<Game> {
        let path = executable(path)?;
        self.unique(&path, None)?;
        let game = Game {
            id: self.next_id,
            added_at: Some(now()),
            name: name
                .unwrap_or_else(|| {
                    path.parent()
                        .unwrap()
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into()
                })
                .trim()
                .into(),
            cover: local_cover(&path),
            executable: path,
            arguments: Vec::new(),
            prefix: Prefix::Shared,
            settings: Overrides {
                proton,
                ..Default::default()
            },
        };
        game.validate()?;
        self.next_id += 1;
        self.games.push(game.clone());
        Ok(game)
    }

    pub fn unique(&self, path: &Path, except: Option<u64>) -> Result<()> {
        if let Some(game) = self
            .games
            .iter()
            .find(|g| g.executable == path && Some(g.id) != except)
        {
            bail!("启动文件已添加为游戏 {}（{}）", game.id, game.name);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_library_promotes_shared_defaults_and_preserves_exceptions() -> Result<()> {
        let bytes = br#"{
            "next_id": 4,
            "games": [
                {"id": 1, "name": "A", "executable": "/a.exe", "proton": "/proton/shared",
                 "enhancement": {"filter": "anime4k", "fps": 30}},
                {"id": 2, "name": "B", "executable": "/b.exe", "proton": "/proton/shared",
                 "enhancement": {"filter": "anime4k", "fullscreen": true}},
                {"id": 3, "name": "C", "executable": "/c.exe", "proton": "/proton/other",
                 "environment": {"LC_ALL": "ja_JP.UTF-8"}}
            ]
        }"#;
        let mut library = Library::decode(bytes)?;
        assert_eq!(library.settings.enhancement.filter, Filter::Anime4k);
        assert_eq!(
            library.settings.proton.as_deref(),
            Some(Path::new("/proton/shared"))
        );
        assert!(library.get(1)?.settings.proton.is_none());
        assert_eq!(library.get(1)?.prefix, Prefix::Independent);
        assert_eq!(
            library.get(1)?.effective(&library).settings.enhancement.fps,
            30
        );
        assert!(
            library
                .get(2)?
                .effective(&library)
                .settings
                .enhancement
                .fullscreen
        );
        let game = library.get(3)?.effective(&library);
        assert_eq!(game.settings.enhancement.filter, Filter::Cunny);
        assert_eq!(
            game.settings.proton.as_deref(),
            Some(Path::new("/proton/other"))
        );
        assert_eq!(game.settings.environment["LC_ALL"], "ja_JP.UTF-8");

        library.settings.enhancement.filter = Filter::Linear;
        let library = Library::decode(&serde_json::to_vec(&library)?)?;
        assert_eq!(
            library
                .get(1)?
                .effective(&library)
                .settings
                .enhancement
                .filter,
            Filter::Linear
        );
        assert_eq!(
            library
                .get(3)?
                .effective(&library)
                .settings
                .enhancement
                .filter,
            Filter::Cunny
        );
        Ok(())
    }

    #[test]
    fn retired_filters_load_as_unfiltered_native_scaling() -> Result<()> {
        let library = Library::decode(
            br#"{
            "settings": {"enhancement": {"filter": "fsr", "sharpness": 5}},
            "games": [
                {"id": 1, "name": "A", "executable": "/a.exe",
                 "enhancement": {"filter": "nis", "sharpness": 10}},
                {"id": 2, "name": "B", "executable": "/b.exe",
                 "enhancement": {"filter": "nearest"}}
            ]
        }"#,
        )?;
        assert_eq!(library.settings.enhancement.filter, Filter::Linear);
        for game in &library.games {
            assert_eq!(
                game.effective(&library).settings.enhancement.filter,
                Filter::Linear
            );
        }
        Ok(())
    }

    #[test]
    fn library_keeps_game_configuration_across_restarts() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let game_dir = dir.path().join("中文 游戏");
        fs::create_dir(&game_dir)?;
        fs::write(game_dir.join("game.EXE"), b"MZ")?;
        fs::write(game_dir.join("cover.ico"), b"icon")?;
        fs::write(game_dir.join("POSTER.PNG"), b"cover")?;
        let mut library = Library::default();
        let game = library.add(&game_dir.join("game.EXE"), None, None)?;
        assert_eq!(game.name, "中文 游戏");
        assert_eq!(game.cover, Some(game_dir.join("POSTER.PNG")));
        assert_eq!(game.prefix, Prefix::Shared);
        library.games[0].apply(Changes {
            source: Some("800x600".parse()?),
            output: Some("1920x1440".parse()?),
            arguments: Some(vec!["--title=含 空格".into()]),
            ..Default::default()
        })?;
        let path = dir.path().join("library.json");
        library.save(&path)?;
        let restored = Library::load(&path)?;
        let game = restored.get(1)?;
        assert_eq!(
            game.effective(&restored).settings.enhancement.source,
            Resolution {
                width: 800,
                height: 600
            }
        );
        assert_eq!(game.arguments, ["--title=含 空格"]);
        assert!(
            restored
                .clone()
                .add(&game_dir.join("game.EXE"), None, None)
                .is_err()
        );
        Ok(())
    }
}
