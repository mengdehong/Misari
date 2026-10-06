use crate::{
    domain::{AssetRef, Error, Fit, Selection},
    library::Library,
    store::{Config, WallpaperEngine},
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
};

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp"];
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "m4v", "avi", "ogv", "mpeg", "mpg", "gif",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Image,
    Video,
    Shader,
    WeScene,
    WeWeb,
    WeApplication,
}

#[derive(Serialize)]
pub struct Asset {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub kind: Kind,
    pub thumbnail: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<Project>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

impl Asset {
    /// Match the panel's title: WE titles are authored; local files omit the extension.
    pub fn display_name(&self) -> &str {
        if self.project.is_some() {
            &self.name
        } else {
            self.name
                .rsplit_once('.')
                .filter(|(_, ext)| !ext.is_empty())
                .map_or(&self.name, |(name, _)| name)
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Project {
    #[serde(default)]
    pub title: String,
    pub r#type: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub preview: String,
}
impl Project {
    fn read(root: &Path) -> Result<Self, Error> {
        read_project(root)
    }
    fn kind(&self) -> Result<Kind, Error> {
        match self.r#type.trim().to_ascii_lowercase().as_str() {
            "video" => Ok(Kind::Video),
            "scene" => Ok(Kind::WeScene),
            "web" => Ok(Kind::WeWeb),
            "application" => Ok(Kind::WeApplication),
            other => Err(Error::new(
                "unsupported",
                format!("unsupported WE project type: {other}"),
            )),
        }
    }
    fn source(&self, root: &Path) -> Result<PathBuf, Error> {
        let kind = self.kind()?;
        if kind == Kind::WeApplication {
            return Err(Error::new(
                "unsupported",
                format!("WE {} projects are not supported", self.r#type),
            ));
        }
        let mut relative = PathBuf::from(self.file.replace('\\', "/"));
        if kind == Kind::WeScene {
            let authored = relative.clone();
            if relative.as_os_str().is_empty() {
                relative = "scene.pkg".into();
            } else {
                relative.set_extension("pkg");
            }
            if !root.join(&relative).exists()
                && authored
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            {
                return project_file(root, &authored);
            }
        }
        project_file(root, &relative)
    }
    fn thumbnail(&self, root: &Path) -> Option<PathBuf> {
        std::iter::once(self.preview.as_str())
            .chain([
                "preview.gif",
                "preview.jpg",
                "preview.jpeg",
                "preview.png",
                "thumbnail.jpg",
                "thumbnail.png",
            ])
            .filter(|name| !name.is_empty())
            .find_map(|name| project_file(root, Path::new(name)).ok())
    }
}

pub(crate) fn read_project<T: serde::de::DeserializeOwned>(root: &Path) -> Result<T, Error> {
    let path = root.join("project.json");
    let read = || -> anyhow::Result<T> {
        let mut bytes = Vec::new();
        File::open(&path)?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 1024 * 1024, "project.json exceeds 1 MiB");
        Ok(serde_json::from_slice(
            bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes),
        )?)
    };
    read().map_err(|e| Error::new("asset_unavailable", format!("{}: {e}", path.display())))
}
fn project_file(root: &Path, relative: &Path) -> Result<PathBuf, Error> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
        || relative.to_string_lossy().as_bytes().get(1) == Some(&b':')
    {
        return Err(Error::new(
            "asset_unavailable",
            "WE file must be a relative path inside the project",
        ));
    }
    let path = canonical(&root.join(relative))?;
    if !path.starts_with(root) || !path.is_file() {
        return Err(Error::new(
            "asset_unavailable",
            format!(
                "WE file is outside the project or not a file: {}",
                path.display()
            ),
        ));
    }
    Ok(path)
}
fn project_root(path: &Path) -> Option<&Path> {
    if path.is_dir() {
        Some(path)
    } else if path.file_name().is_some_and(|n| n == "project.json") {
        path.parent()
    } else {
        None
    }
}
pub fn kind(path: &Path) -> Option<Kind> {
    if let Some(root) = project_root(path) {
        return Project::read(root).and_then(|p| p.kind()).ok();
    }
    let extension = path.extension()?.to_str()?;
    if VIDEO_EXTENSIONS
        .iter()
        .any(|v| extension.eq_ignore_ascii_case(v))
    {
        Some(Kind::Video)
    } else if IMAGE_EXTENSIONS
        .iter()
        .any(|v| extension.eq_ignore_ascii_case(v))
    {
        Some(Kind::Image)
    } else if ["frag", "glsl"]
        .iter()
        .any(|v| extension.eq_ignore_ascii_case(v))
    {
        Some(Kind::Shader)
    } else {
        None
    }
}
fn canonical(path: &Path) -> Result<PathBuf, Error> {
    path.canonicalize()
        .map_err(|e| Error::new("asset_unavailable", format!("{}: {e}", path.display())))
}
pub fn resolve(input: &str, fit: Fit) -> Result<Selection, Error> {
    let path = canonical(AssetRef::parse(input).map_or_else(|| Path::new(input), AssetRef::path))?;
    let asset_id = if let Some(root) = project_root(&path) {
        Project::read(root)?.source(root)?;
        format!("we:{}", root.display())
    } else if path.is_file() && kind(&path).is_some() {
        format!("local:{}", path.display())
    } else {
        return Err(Error::new(
            "unsupported",
            "supported assets are images, videos, GLSL fragment shaders and WE video/scene/web projects",
        ));
    };
    Ok(Selection { asset_id, fit })
}
pub fn path(selection: &Selection) -> &Path {
    selection.asset().path()
}
/// Resolve the entry at load time, retaining project identity in saved selections.
pub fn source(selection: &Selection) -> Result<PathBuf, Error> {
    let path = path(selection);
    if let Some(root) = project_root(path) {
        Project::read(root)?.source(root)
    } else {
        canonical(path)
    }
}
pub fn list(config: &Config, library: &Library) -> Vec<Asset> {
    let mut assets = Vec::new();
    for dir in config
        .asset_dirs
        .iter()
        .cloned()
        .chain(steam_workshops())
        .chain(library.sources.iter().cloned())
        .filter(|path| library.source_enabled(path))
    {
        visit(&dir, 0, &mut assets);
    }
    assets.sort_by(|a, b| a.id.cmp(&b.id));
    assets.dedup_by(|a, b| a.id == b.id);
    assets
}
fn visit(dir: &Path, depth: usize, assets: &mut Vec<Asset>) {
    if depth > 8 {
        return;
    }
    if dir.is_file() {
        if let Ok(selection) = resolve(&dir.to_string_lossy(), Fit::Cover) {
            let path = path(&selection);
            if matches!(selection.asset(), AssetRef::Project(_)) {
                visit(path, depth, assets);
            } else if let Some(kind) = kind(path) {
                assets.push(Asset {
                    id: selection.asset_id.clone(),
                    path: path.to_owned(),
                    name: path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    kind,
                    thumbnail: (kind == Kind::Image).then(|| path.to_owned()),
                    project: None,
                    error: None,
                });
            }
        }
        return;
    }
    if dir.join("project.json").is_file() {
        if let Ok(root) = canonical(dir)
            && let Ok(project) = Project::read(&root)
            && let Ok(kind) = project.kind()
        {
            assets.push(Asset {
                id: format!("we:{}", root.display()),
                name: if project.title.trim().is_empty() {
                    root.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                } else {
                    project.title.clone()
                },
                thumbnail: project.thumbnail(&root),
                error: project.source(&root).err(),
                path: root,
                kind,
                project: Some(project),
            });
        }
        // A project's previews, textures and embedded videos are not standalone wallpapers.
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            visit(&path, depth + 1, assets);
        } else if file_type.is_file()
            && let Some(kind) = kind(&path)
            && let Ok(path) = canonical(&path)
        {
            assets.push(Asset {
                id: format!("local:{}", path.display()),
                name: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                thumbnail: (kind == Kind::Image).then(|| path.clone()),
                path,
                kind,
                project: None,
                error: None,
            });
        }
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}
fn steam_roots() -> impl Iterator<Item = PathBuf> {
    [".local/share/Steam", ".steam/steam", "Share/Steam"]
        .into_iter()
        .map(|path| home().join(path))
}
pub fn steam_workshops() -> impl Iterator<Item = PathBuf> {
    steam_roots()
        .map(|root| root.join("steamapps/workshop/content/431960"))
        .filter(|root| root.is_dir())
        .filter_map(|root| root.canonicalize().ok())
        .collect::<BTreeSet<_>>()
        .into_iter()
}
pub(crate) fn we_assets(config: &WallpaperEngine) -> anyhow::Result<PathBuf> {
    let path = config
        .assets
        .clone()
        .or_else(|| {
            steam_roots()
                .map(|root| root.join("steamapps/common/wallpaper_engine/assets"))
                .find(|path| path.is_dir())
        })
        .context("WE assets not found; set wallpaper_engine.assets in wallpaperd.toml")?;
    anyhow::ensure!(
        path.is_dir(),
        "WE assets directory is unavailable: {}",
        path.display()
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_removal_overrides_config_and_persists_without_deleting_files() {
        use crate::library::{Edit, Library};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("wallpapers");
        std::fs::create_dir(&root).unwrap();
        let image = root.join("test.png");
        std::fs::write(&image, []).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let config = Config {
            asset_dirs: vec![alias.clone()],
            ..Config::default()
        };
        let mut library: Library = serde_json::from_str("{}").unwrap();
        library
            .edit(Edit::Import {
                path: alias.clone(),
            })
            .unwrap();
        let contains_image = |library: &Library| {
            list(&config, library)
                .iter()
                .filter(|asset| asset.path == image)
                .count()
        };
        assert_eq!(contains_image(&library), 1);
        library.edit(Edit::RemoveSource { path: alias }).unwrap();
        assert!(library.sources.is_empty());
        let mut restored: Library =
            serde_json::from_value(serde_json::to_value(library).unwrap()).unwrap();
        assert_eq!(
            contains_image(&restored),
            0,
            "configuration must not rediscover a removed source"
        );
        assert!(image.exists(), "removal never deletes wallpaper files");
        restored.edit(Edit::Import { path: root }).unwrap();
        assert_eq!(
            contains_image(&restored),
            1,
            "adding the same source restores discovery"
        );
    }
    fn project(root: &Path, ty: &str, file: &str) {
        std::fs::write(root.join("project.json"), serde_json::to_vec(&serde_json::json!({"title":"测试项目", "type":ty, "file":file, "preview":"preview.jpg"})).unwrap()).unwrap();
    }
    #[test]
    fn project_identity_metadata_and_packed_scene() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        project(root, "Video", "湖面 带音乐.mp4");
        std::fs::write(root.join("湖面 带音乐.mp4"), []).unwrap();
        std::fs::write(root.join("preview.jpg"), []).unwrap();
        let selection = resolve(root.to_str().unwrap(), Fit::Cover).unwrap();
        assert_eq!(
            resolve(root.join("project.json").to_str().unwrap(), Fit::Cover)
                .unwrap()
                .asset_id,
            selection.asset_id
        );
        assert_eq!(
            resolve(&selection.asset_id, Fit::Cover).unwrap().asset_id,
            selection.asset_id
        );
        assert_eq!(source(&selection).unwrap(), root.join("湖面 带音乐.mp4"));
        // Entry filenames can change without changing saved project identity.
        project(root, "Web", "index.html");
        std::fs::write(root.join("index.html"), b"<html></html>").unwrap();
        assert_eq!(kind(path(&selection)), Some(Kind::WeWeb));
        assert_eq!(source(&selection).unwrap(), root.join("index.html"));
        assert_eq!(
            resolve(root.to_str().unwrap(), Fit::Cover)
                .unwrap()
                .asset_id,
            selection.asset_id
        );
        project(root, "Scene", "scene.json");
        std::fs::write(root.join("scene.json"), b"{\"general\":{},\"objects\":[]}").unwrap();
        assert_eq!(source(&selection).unwrap(), root.join("scene.json"));
        std::fs::write(root.join("scene.pkg"), []).unwrap();
        assert_eq!(kind(path(&selection)), Some(Kind::WeScene));
        assert_eq!(source(&selection).unwrap(), root.join("scene.pkg"));
        let mut assets = Vec::new();
        visit(root, 0, &mut assets);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].name, "测试项目");
        assets[0].name = "WE title.with.dots".into();
        assert_eq!(assets[0].display_name(), "WE title.with.dots");
        assert_eq!(assets[0].thumbnail, Some(root.join("preview.jpg")));
        assert!(assets[0].error.is_none());
    }
    #[test]
    fn unsupported_missing_and_escaping_projects() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        project(root, "application", "index.html");
        assert_eq!(
            resolve(root.to_str().unwrap(), Fit::Cover)
                .unwrap_err()
                .code,
            "unsupported"
        );
        for kind in ["video", "web"] {
            for file in [
                "missing.mp4",
                "../escape.mp4",
                "/tmp/escape.mp4",
                "C:\\escape.mp4",
            ] {
                project(root, kind, file);
                assert_eq!(
                    resolve(root.to_str().unwrap(), Fit::Cover)
                        .unwrap_err()
                        .code,
                    "asset_unavailable"
                );
            }
        }
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("escape.mp4")).unwrap();
        for kind in ["video", "web"] {
            project(root, kind, "escape.mp4");
            assert!(resolve(root.to_str().unwrap(), Fit::Cover).is_err());
        }
    }
    #[test]
    fn ordinary_content_and_malformed_project() {
        let temp = tempfile::tempdir().unwrap();
        for (file, expected) in [
            ("image.JPG", Kind::Image),
            ("video.MP4", Kind::Video),
            ("animated.gif", Kind::Video),
            ("shader.frag", Kind::Shader),
        ] {
            let file = temp.path().join(file);
            std::fs::write(&file, []).unwrap();
            let selection = resolve(file.to_str().unwrap(), Fit::Cover).unwrap();
            assert_eq!(kind(path(&selection)), Some(expected));
            let mut assets = Vec::new();
            visit(&file, 0, &mut assets);
            assert_eq!(
                assets[0].display_name(),
                file.file_stem().unwrap().to_str().unwrap()
            );
        }
        std::fs::write(temp.path().join("project.json"), "broken").unwrap();
        assert_eq!(
            resolve(temp.path().to_str().unwrap(), Fit::Cover)
                .unwrap_err()
                .code,
            "asset_unavailable"
        );
    }
}
