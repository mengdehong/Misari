use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::domain::{Fit, Playback, Selection, Transition};

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct SavedOutput {
    pub selection: Option<Selection>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub mute: Option<bool>,
    #[serde(default)]
    pub fps: Option<u32>,
    #[serde(default)]
    pub volume: Option<u32>,
    #[serde(default)]
    pub released: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, crate::properties::Values>,
    #[serde(default)]
    pub rotation: crate::library::Rotation,
}

impl SavedOutput {
    pub fn playback(&self, defaults: Playback) -> Playback {
        Playback {
            paused: self.paused,
            mute: self.mute.unwrap_or(defaults.mute),
            fps: self
                .fps
                .filter(|v| (1..=240).contains(v))
                .unwrap_or(defaults.fps),
            volume: self.volume.filter(|v| *v <= 100).unwrap_or(defaults.volume),
        }
    }

    pub fn accept_playback(&mut self, playback: Playback) {
        self.paused = playback.paused;
        self.mute = Some(playback.mute);
        self.fps = Some(playback.fps);
        self.volume = Some(playback.volume);
    }

    pub fn accept_properties(&mut self, asset_id: &str, overrides: crate::properties::Values) {
        if overrides.is_empty() {
            self.properties.remove(asset_id);
        } else {
            self.properties.insert(asset_id.into(), overrides);
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Store {
    #[serde(default)]
    pub outputs: BTreeMap<String, SavedOutput>,
    #[serde(default)]
    pub library: crate::library::Library,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(default)]
    pub asset_dirs: Vec<PathBuf>,
    pub default: Option<PathBuf>,
    pub fit: Fit,
    pub transition: Transition,
    pub outputs: BTreeMap<String, OutputConfig>,
    pub playback: Playback,
    pub pause_on_session: Option<bool>,
    pub pause_on_fullscreen: Option<bool>,
    pub mute_on_other_audio: Option<bool>,
    pub media_integration: Option<bool>,
    pub wallpaper_engine: WallpaperEngine,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    pub default: Option<PathBuf>,
    pub fit: Option<Fit>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct WallpaperEngine {
    pub assets: Option<PathBuf>,
    pub scene_backend: SceneBackend,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneBackend {
    #[default]
    Auto,
    Rust,
    Native,
}

pub fn cache_path() -> PathBuf {
    xdg_dir("XDG_CACHE_HOME", ".cache").join("misari/wallpaperd/we")
}

fn xdg_dir(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback)
        })
}

pub fn socket_path() -> PathBuf {
    xdg_dir("XDG_RUNTIME_DIR", ".local/run").join("misari/wallpaperd.sock")
}

pub fn state_path() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("misari/wallpaperd/state.json")
}

pub fn config_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("misari/wallpaperd.toml")
}

pub fn expand_home(path: PathBuf) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    path
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("invalid {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn parse(text: &str) -> Result<Self> {
        let mut config: Self = toml::from_str(text)?;
        config
            .playback
            .validate()
            .map_err(|e| anyhow::anyhow!(e.message))?;
        for path in config
            .asset_dirs
            .iter_mut()
            .chain(config.default.iter_mut())
            .chain(config.wallpaper_engine.assets.iter_mut())
            .chain(
                config
                    .outputs
                    .values_mut()
                    .filter_map(|output| output.default.as_mut()),
            )
        {
            *path = expand_home(std::mem::take(path));
        }
        Ok(config)
    }

    pub fn fit_for(&self, output: &str) -> Fit {
        self.outputs
            .get(output)
            .and_then(|output| output.fit)
            .unwrap_or(self.fit)
    }

    pub fn wallpaper_for(&self, output: &str) -> Option<&std::path::Path> {
        self.outputs
            .get(output)
            .and_then(|output| output.default.as_deref())
            .or(self.default.as_deref())
    }
}

impl Store {
    pub fn load() -> Result<Self> {
        let path = state_path();
        match fs::read(&path) {
            Ok(data) => {
                serde_json::from_slice(&data).with_context(|| format!("invalid {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = state_path();
        let parent = path.parent().context("state path has no parent")?;
        fs::create_dir_all(parent)?;
        let tmp = parent.join(format!(".state-{}.tmp", std::process::id()));
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_expands_home_in_all_paths() {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME must be set for this test"));
        let config = Config::parse(
            r#"
asset_dirs = ["~/Pictures/Wallpapers", "~", "~//Pictures", "/absolute", "relative", "~other/Pictures", "relative/~/Pictures", "$HOME/Pictures"]
default = "~/Pictures/default.jpg"
[wallpaper_engine]
assets = "~/.local/share/Steam/assets"
[outputs."DP-1"]
default = "~/Pictures/portrait.jpg"
fit = "contain"
"#,
        )
        .unwrap();
        assert_eq!(
            config.asset_dirs,
            vec![
                home.join("Pictures/Wallpapers"),
                home.clone(),
                home.join("Pictures"),
                PathBuf::from("/absolute"),
                PathBuf::from("relative"),
                PathBuf::from("~other/Pictures"),
                PathBuf::from("relative/~/Pictures"),
                PathBuf::from("$HOME/Pictures"),
            ]
        );
        assert_eq!(config.default, Some(home.join("Pictures/default.jpg")));
        assert_eq!(
            config.wallpaper_engine.assets,
            Some(home.join(".local/share/Steam/assets"))
        );
        assert_eq!(
            config.outputs["DP-1"].default,
            Some(home.join("Pictures/portrait.jpg"))
        );
    }

    #[test]
    fn wallpaper_defaults_inherit_and_reject_invalid_values() {
        let config = Config::parse(
            r#"
default = "/global.jpg"
fit = "stretch"
transition = "fade"
[outputs."DP-1"]
default = "/portrait.jpg"
fit = "contain"
[outputs."DP-2"]
"#,
        )
        .unwrap();
        assert_eq!(config.fit_for("DP-1"), Fit::Contain);
        for name in ["DP-2", "unknown"] {
            assert_eq!(config.fit_for(name), Fit::Stretch);
            assert_eq!(
                config.wallpaper_for(name),
                Some(std::path::Path::new("/global.jpg"))
            );
        }
        assert_eq!(
            config.wallpaper_for("DP-1"),
            Some(std::path::Path::new("/portrait.jpg"))
        );
        assert_eq!(config.transition, Transition::Fade);
        let defaults = Config::parse("").unwrap();
        assert_eq!(defaults.fit, Fit::Cover);
        assert_eq!(defaults.transition, Transition::Cut);
        assert!(defaults.wallpaper_for("DP-1").is_none());
        for text in [
            "fit = 'invalid'",
            "transition = 'invalid'",
            "[outputs.'DP-1']\nfit = 'invalid'",
            "[outputs.'DP-1']\ntransition = 'fade'",
        ] {
            assert!(
                Config::parse(text).is_err(),
                "accepted invalid config: {text}"
            );
        }
    }

    #[test]
    fn legacy_scene_config_accepts_unused_library_and_preserves_backend_selection() {
        for (backend, native) in [("auto", false), ("rust", false), ("native", true)] {
            let text = format!(
                r#"[wallpaper_engine]
scene_backend = "{backend}"
library = "/missing/legacy.so"
assets = "/assets"
"#
            );
            let config: Config = toml::from_str(&text).unwrap();
            assert_eq!(
                matches!(config.wallpaper_engine.scene_backend, SceneBackend::Native),
                native
            );
            assert_eq!(
                config.wallpaper_engine.assets.as_deref(),
                Some(std::path::Path::new("/assets"))
            );
        }
        assert!(
            toml::from_str::<Config>(
                r#"[wallpaper_engine]
scene_backend = "unknown"
"#
            )
            .is_err()
        );
    }
}
