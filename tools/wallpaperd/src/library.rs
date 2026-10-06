//! Persistent library edits and ordered rotation sources; rendering stays in Session.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use crate::{
    catalog,
    domain::{Error, Fit, Transition},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Library {
    pub sources: Vec<PathBuf>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub excluded_sources: BTreeSet<PathBuf>,
    pub favorites: BTreeSet<String>,
    pub playlists: BTreeMap<String, Playlist>,
    pub hidden: BTreeSet<String>,
    pub titles: BTreeMap<String, String>,
    next_id: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Playlist {
    pub name: String,
    pub members: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    Import { path: PathBuf },
    RemoveSource { path: PathBuf },
    Favorite { asset_id: String, favorite: bool },
    Hide { asset_id: String, hidden: bool },
    RenameAsset { asset_id: String, title: String },
    CreatePlaylist { name: String },
    RenamePlaylist { id: String, name: String },
    DeletePlaylist { id: String },
    PlaylistMembers { id: String, members: Vec<String> },
}

impl Library {
    pub fn edit(&mut self, edit: Edit) -> Result<Option<String>, Error> {
        match edit {
            Edit::Import { path } => {
                let path = path
                    .canonicalize()
                    .map_err(|e| Error::new("asset_unavailable", e.to_string()))?;
                if path.is_dir() {
                    std::fs::read_dir(&path)
                        .map_err(|e| Error::new("asset_unavailable", e.to_string()))?;
                } else {
                    catalog::resolve(&path.to_string_lossy(), Fit::Cover)?;
                }
                self.excluded_sources.remove(&path);
                if !self.sources.contains(&path) {
                    self.sources.push(path);
                }
            }
            Edit::RemoveSource { path } => {
                let path = path.canonicalize().unwrap_or(path);
                if !path.is_absolute() {
                    return Err(Error::new("bad_request", "source path must be absolute"));
                }
                self.sources.retain(|p| p != &path);
                self.excluded_sources.insert(path);
            }
            Edit::Favorite { asset_id, favorite } => {
                validate_id(&asset_id)?;
                if favorite {
                    self.favorites.insert(asset_id);
                } else {
                    self.favorites.remove(&asset_id);
                }
            }
            Edit::Hide { asset_id, hidden } => {
                validate_id(&asset_id)?;
                if hidden {
                    self.hidden.insert(asset_id);
                } else {
                    self.hidden.remove(&asset_id);
                }
            }
            Edit::RenameAsset { asset_id, title } => {
                validate_id(&asset_id)?;
                let title = title.trim();
                if title.chars().count() > 256 || title.chars().any(char::is_control) {
                    return Err(Error::new(
                        "bad_request",
                        "title must be a single line of at most 256 characters",
                    ));
                }
                if title.is_empty() {
                    self.titles.remove(&asset_id);
                } else {
                    self.titles.insert(asset_id, title.into());
                }
            }
            Edit::CreatePlaylist { name } => {
                let name = playlist_name(name)?;
                self.next_id += 1;
                let id = format!("playlist:{}", self.next_id);
                self.playlists.insert(
                    id.clone(),
                    Playlist {
                        name,
                        members: Vec::new(),
                    },
                );
                return Ok(Some(id));
            }
            Edit::RenamePlaylist { id, name } => {
                let name = playlist_name(name)?;
                self.playlist(&id)?.name = name;
            }
            Edit::DeletePlaylist { id } => {
                self.playlists.remove(&id).ok_or_else(missing_playlist)?;
            }
            Edit::PlaylistMembers { id, members } => {
                if members.len() > 4096 {
                    return Err(Error::new("bad_request", "playlist exceeds 4096 members"));
                }
                let mut seen = BTreeSet::new();
                for member in &members {
                    validate_id(member)?;
                    if !seen.insert(member) {
                        return Err(Error::new("bad_request", "duplicate playlist member"));
                    }
                }
                self.playlist(&id)?.members = members;
            }
        }
        Ok(None)
    }

    fn playlist(&mut self, id: &str) -> Result<&mut Playlist, Error> {
        self.playlists.get_mut(id).ok_or_else(missing_playlist)
    }

    pub fn source_enabled(&self, path: &Path) -> bool {
        self.excluded_sources.is_empty()
            || !self
                .excluded_sources
                .contains(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
    }

    pub fn members(&self, source: &str) -> Result<Vec<String>, Error> {
        if source == "favorites" {
            return Ok(self.favorites.iter().cloned().collect());
        }
        self.playlists
            .get(source)
            .map(|p| p.members.clone())
            .ok_or_else(missing_playlist)
    }

    pub fn available_members(
        &self,
        source: &str,
        names: &BTreeMap<String, String>,
    ) -> Result<Vec<String>, Error> {
        let mut members = self.members(source)?;
        members.retain(|id| names.contains_key(id) && !self.hidden.contains(id));
        if source == "favorites" {
            members.sort_by_cached_key(|id| {
                (
                    self.titles
                        .get(id)
                        .unwrap_or(&names[id])
                        .to_ascii_lowercase(),
                    id.clone(),
                )
            });
        }
        Ok(members)
    }
}

fn validate_id(id: &str) -> Result<(), Error> {
    if crate::domain::AssetRef::parse(id).is_some_and(|asset| asset.path().is_absolute())
        && id.len() <= 4096
    {
        Ok(())
    } else {
        Err(Error::new("bad_request", "invalid asset_id"))
    }
}

fn playlist_name(name: String) -> Result<String, Error> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(Error::new(
            "bad_request",
            "playlist name must contain 1 to 80 characters",
        ));
    }
    Ok(name.into())
}

fn missing_playlist() -> Error {
    Error::new("playlist_missing", "playlist no longer exists")
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rotation {
    pub enabled: bool,
    pub source: String,
    pub mode: Mode,
    pub interval_seconds: u64,
    pub fit: Fit,
    pub transition: Transition,
}

impl Default for Rotation {
    fn default() -> Self {
        Self {
            enabled: false,
            source: "favorites".into(),
            mode: Mode::Ordered,
            interval_seconds: 300,
            fit: Fit::Cover,
            transition: Transition::Fade,
        }
    }
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Ordered,
    Random,
}

impl Rotation {
    pub fn validate(&self, library: &Library) -> Result<(), Error> {
        if !(10..=86400).contains(&self.interval_seconds) {
            return Err(Error::new(
                "bad_request",
                "rotation interval must be between 10 and 86400 seconds",
            ));
        }
        if self.enabled {
            library.members(&self.source)?;
        }
        Ok(())
    }

    pub fn next<'a>(
        &self,
        members: &'a [String],
        previous: Option<&str>,
        seed: u64,
    ) -> Option<&'a str> {
        if members.is_empty() {
            return None;
        }
        let previous = members.iter().position(|id| Some(id.as_str()) == previous);
        let index = if self.mode == Mode::Ordered {
            previous.map_or(0, |i| (i + 1) % members.len())
        } else if members.len() > 1 && previous.is_some() {
            let index = seed as usize % (members.len() - 1);
            index + usize::from(index >= previous.unwrap())
        } else {
            seed as usize % members.len()
        };
        Some(&members[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn custom_titles_persist_sort_and_restore() {
        let mut library: Library = serde_json::from_value(json!({
            "favorites": ["we:/1", "we:/2"],
            "playlists": {"playlist:1": {"name":"工作", "members":["we:/2", "we:/1"]}}
        }))
        .unwrap();
        assert!(library.titles.is_empty(), "old state needs no migration");
        let names = BTreeMap::from([
            ("we:/1".into(), "zebra".into()),
            ("we:/2".into(), "aqua".into()),
        ]);
        library
            .edit(Edit::RenameAsset {
                asset_id: "we:/1".into(),
                title: "  A custom.jpg  ".into(),
            })
            .unwrap();
        let mut restored: Library =
            serde_json::from_value(serde_json::to_value(&library).unwrap()).unwrap();
        assert_eq!(restored.titles["we:/1"], "A custom.jpg");
        assert_eq!(
            restored.available_members("favorites", &names).unwrap(),
            ["we:/1", "we:/2"]
        );
        assert_eq!(
            restored.available_members("playlist:1", &names).unwrap(),
            ["we:/2", "we:/1"]
        );
        for title in ["bad\nname".into(), "界".repeat(257)] {
            assert!(
                restored
                    .edit(Edit::RenameAsset {
                        asset_id: "we:/1".into(),
                        title
                    })
                    .is_err()
            );
        }
        assert_eq!(
            restored.titles["we:/1"], "A custom.jpg",
            "invalid edits retain the alias"
        );
        restored
            .edit(Edit::RenameAsset {
                asset_id: "we:/1".into(),
                title: " ".into(),
            })
            .unwrap();
        assert!(restored.titles.is_empty());
        assert_eq!(
            restored.available_members("favorites", &names).unwrap(),
            ["we:/2", "we:/1"]
        );
        assert!(
            restored
                .edit(Edit::RenameAsset {
                    asset_id: "not-an-id".into(),
                    title: "Title".into()
                })
                .is_err()
        );
    }

    #[test]
    fn favorites_follow_display_names_and_playlists_keep_member_order() {
        let mut library = Library::default();
        let names = BTreeMap::from([
            ("we:/1".into(), "zebra".into()),
            ("we:/2".into(), "aqua".into()),
            ("we:/3".into(), "aqua".into()),
            ("we:/4".into(), "hidden".into()),
        ]);
        library.favorites = ["we:/1", "we:/2", "we:/3", "we:/4", "we:/missing"]
            .map(String::from)
            .into();
        library.hidden.insert("we:/4".into());
        library.playlists.insert(
            "playlist:1".into(),
            Playlist {
                name: "工作".into(),
                members: vec![
                    "we:/3".into(),
                    "we:/1".into(),
                    "we:/4".into(),
                    "we:/missing".into(),
                ],
            },
        );
        assert_eq!(
            library.available_members("favorites", &names).unwrap(),
            ["we:/2", "we:/3", "we:/1"]
        );
        assert_eq!(
            library.available_members("playlist:1", &names).unwrap(),
            ["we:/3", "we:/1"]
        );
    }

    #[test]
    fn library_roundtrip_and_rotation_order() {
        let mut library = Library::default();
        let id = library
            .edit(Edit::CreatePlaylist {
                name: " 工作 ".into(),
            })
            .unwrap()
            .unwrap();
        let members = vec!["local:/a.jpg".into(), "we:/b".into(), "local:/c.jpg".into()];
        library
            .edit(Edit::PlaylistMembers {
                id: id.clone(),
                members: members.clone(),
            })
            .unwrap();
        library
            .edit(Edit::Favorite {
                asset_id: members[0].clone(),
                favorite: true,
            })
            .unwrap();
        let library: Library =
            serde_json::from_value(serde_json::to_value(library).unwrap()).unwrap();
        assert_eq!(library.members(&id).unwrap(), members);
        assert_eq!(
            library.members("favorites").unwrap(),
            vec![members[0].clone()]
        );
        let mut rotation = Rotation {
            source: id,
            enabled: true,
            ..Rotation::default()
        };
        rotation.validate(&library).unwrap();
        assert_eq!(
            rotation.next(&members, Some("we:/b"), 0),
            Some("local:/c.jpg")
        );
        assert_eq!(
            rotation.next(&members, Some("local:/c.jpg"), 0),
            Some("local:/a.jpg")
        );
        rotation.mode = Mode::Random;
        for seed in 0..100 {
            assert_ne!(rotation.next(&members, Some("we:/b"), seed), Some("we:/b"));
        }
        assert!(rotation.next(&[], None, 0).is_none());
        assert!(
            serde_json::from_value::<Edit>(
                json!({"action":"favorite","asset_id":"local:/a","favorite":"true"})
            )
            .is_err()
        );
        rotation.interval_seconds = 0;
        assert!(rotation.validate(&library).is_err());
    }
}
