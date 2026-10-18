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
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub presets: BTreeMap<String, BTreeMap<String, crate::properties::Values>>,
    next_id: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Playlist {
    pub name: String,
    pub members: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, crate::properties::Values>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    SavePreset {
        asset_id: String,
        name: String,
        properties: crate::properties::Values,
    },
    DeletePreset {
        asset_id: String,
        name: String,
    },
    PlaylistProperties {
        id: String,
        asset_id: String,
        properties: crate::properties::Values,
    },
    Import {
        path: PathBuf,
    },
    RemoveSource {
        path: PathBuf,
    },
    Favorite {
        asset_id: String,
        favorite: bool,
    },
    Hide {
        asset_id: String,
        hidden: bool,
    },
    RenameAsset {
        asset_id: String,
        title: String,
    },
    CreatePlaylist {
        name: String,
    },
    RenamePlaylist {
        id: String,
        name: String,
    },
    DeletePlaylist {
        id: String,
    },
    PlaylistMembers {
        id: String,
        members: Vec<String>,
    },
}

impl Library {
    pub fn edit(&mut self, edit: Edit) -> Result<Option<String>, Error> {
        match edit {
            Edit::SavePreset {
                asset_id,
                name,
                properties,
            } => {
                validate_id(&asset_id)?;
                let name = playlist_name(name)?;
                validate_properties(&asset_id, &properties)?;
                let presets = self.presets.entry(asset_id).or_default();
                if presets.len() >= 128 && !presets.contains_key(&name) {
                    return Err(Error::new(
                        "bad_request",
                        "at most 128 presets per wallpaper",
                    ));
                }
                presets.insert(name, properties);
            }
            Edit::DeletePreset { asset_id, name } => {
                if let Some(presets) = self.presets.get_mut(&asset_id) {
                    presets.remove(&name);
                    if presets.is_empty() {
                        self.presets.remove(&asset_id);
                    }
                }
            }
            Edit::PlaylistProperties {
                id,
                asset_id,
                properties,
            } => {
                if !self.playlist(&id)?.members.contains(&asset_id) {
                    return Err(Error::new(
                        "bad_request",
                        "wallpaper is not in this playlist",
                    ));
                }
                validate_properties(&asset_id, &properties)?;
                // An empty snapshot means authored defaults, distinct from no snapshot.
                self.playlist(&id)?.properties.insert(asset_id, properties);
            }
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
                let id = loop {
                    self.next_id = self
                        .next_id
                        .checked_add(1)
                        .ok_or_else(|| Error::new("bad_request", "playlist id limit reached"))?;
                    let id = format!("playlist:{}", self.next_id);
                    if !self.playlists.contains_key(&id) {
                        break id;
                    }
                };
                self.playlists.insert(
                    id.clone(),
                    Playlist {
                        name,
                        members: Vec::new(),
                        properties: BTreeMap::new(),
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
                let playlist = self.playlist(&id)?;
                playlist
                    .properties
                    .retain(|asset, _| members.contains(asset));
                playlist.members = members;
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
        if matches!(source, "library" | "single") {
            return Ok(Vec::new());
        }
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
        let mut members = if source == "library" {
            names.keys().cloned().collect()
        } else {
            self.members(source)?
        };
        members.retain(|id| names.contains_key(id) && !self.hidden.contains(id));
        if matches!(source, "favorites" | "library") {
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

fn validate_properties(asset: &str, properties: &crate::properties::Values) -> Result<(), Error> {
    let selection = catalog::resolve(asset, Fit::Cover)?;
    // Existing unknown fields must round-trip; only known authored values are validated.
    let schema = crate::properties::Schema::load(&selection)?;
    let resolved = schema.resolve(properties, &Default::default())?;
    if let Some((key, warning)) = resolved.warnings.iter().find(|(key, _)| {
        schema
            .definitions
            .iter()
            .any(|d| &d.key == *key && d.editable)
    }) {
        return Err(Error::new(
            "bad_request",
            format!("property {key}: {warning}"),
        ));
    }
    Ok(())
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimeRule {
    pub start: String,
    pub end: String,
    /// Monday = 1, Sunday = 7. Empty means every day; overnight belongs to its starting day.
    #[serde(default)]
    pub days: Vec<u8>,
}
impl TimeRule {
    fn minutes(text: &str) -> Option<u16> {
        if text.len() != 5
            || text.as_bytes()[2] != b':'
            || !text
                .bytes()
                .enumerate()
                .all(|(i, b)| i == 2 || b.is_ascii_digit())
        {
            return None;
        }
        let (hour, minute) = text.split_once(':')?;
        let hour: u16 = hour.parse().ok()?;
        let minute: u16 = minute.parse().ok()?;
        (hour < 24 && minute < 60).then_some(hour * 60 + minute)
    }
    fn validate(&self) -> Result<(), Error> {
        if Self::minutes(&self.start).is_none()
            || Self::minutes(&self.end).is_none()
            || self.days.iter().any(|d| !(1..=7).contains(d))
        {
            return Err(Error::new(
                "bad_request",
                "time rules need HH:MM times and weekdays 1–7",
            ));
        }
        Ok(())
    }
    fn active(&self, weekday: u8, minute: u16) -> bool {
        let (Some(start), Some(end)) = (Self::minutes(&self.start), Self::minutes(&self.end))
        else {
            return false;
        };
        let day_matches = |day| self.days.is_empty() || self.days.contains(&day);
        if start < end {
            day_matches(weekday) && (start..end).contains(&minute)
        } else if start == end {
            day_matches(weekday)
        } else {
            minute >= start && day_matches(weekday)
                || minute < end && day_matches(if weekday == 1 { 7 } else { weekday - 1 })
        }
    }
}
impl Rotation {
    pub fn scheduled_now(&self) -> bool {
        if self.schedule.is_empty() {
            return true;
        }
        let now = unsafe { libc::time(std::ptr::null_mut()) };
        let mut local: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&now, &mut local) }.is_null() {
            return false;
        }
        let day = if local.tm_wday == 0 {
            7
        } else {
            local.tm_wday as u8
        };
        self.schedule
            .iter()
            .any(|rule| rule.active(day, (local.tm_hour * 60 + local.tm_min) as u16))
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
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
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
    pub schedule: Vec<TimeRule>,
}

impl Default for Rotation {
    fn default() -> Self {
        Self {
            enabled: false,
            source: "library".into(),
            mode: Mode::Ordered,
            interval_seconds: 300,
            fit: Fit::Cover,
            transition: Transition::Fade,
            schedule: Vec::new(),
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
        if self.schedule.len() > 64 {
            return Err(Error::new("bad_request", "at most 64 time rules"));
        }
        for rule in &self.schedule {
            rule.validate()?;
        }
        if self.enabled {
            if self.source == "single" {
                return Err(Error::new("bad_request", "single wallpaper cannot rotate"));
            }
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
    fn library_playback_uses_all_visible_assets_in_name_order() {
        let mut library = Library::default();
        library.hidden.insert("local:/hidden.jpg".into());
        library
            .titles
            .insert("local:/z.jpg".into(), "A custom title".into());
        let names = BTreeMap::from([
            ("local:/b.jpg".into(), "B".into()),
            ("local:/z.jpg".into(), "Z".into()),
            ("local:/hidden.jpg".into(), "Hidden".into()),
        ]);
        let members = library.available_members("library", &names).unwrap();
        assert_eq!(members, ["local:/z.jpg", "local:/b.jpg"]);
        let rotation = Rotation::default();
        assert!(!rotation.enabled);
        assert_eq!(
            rotation.next(&members, Some("local:/b.jpg"), 0),
            Some("local:/z.jpg")
        );
        assert!(
            Rotation {
                enabled: true,
                ..rotation
            }
            .validate(&library)
            .is_ok()
        );
        assert!(
            Rotation {
                enabled: true,
                source: "single".into(),
                ..Default::default()
            }
            .validate(&library)
            .is_err()
        );
    }

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
                properties: BTreeMap::new(),
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
    #[test]
    fn presets_and_playlist_values_roundtrip_and_prune() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project.json");
        std::fs::write(&project, r#"{"type":"web","file":"index.html","general":{"properties":{"flag":{"type":"bool","value":true}}}}"#).unwrap();
        std::fs::write(temp.path().join("index.html"), "hello").unwrap();
        let asset = format!("we:{}", temp.path().display());
        let mut library = Library::default();
        let id = library
            .edit(Edit::CreatePlaylist {
                name: "工作".into(),
            })
            .unwrap()
            .unwrap();
        library
            .edit(Edit::PlaylistMembers {
                id: id.clone(),
                members: vec![asset.clone()],
            })
            .unwrap();
        let properties = BTreeMap::from([
            ("flag".into(), json!(false)),
            ("future".into(), json!({"preserve":true})),
        ]);
        library
            .edit(Edit::SavePreset {
                asset_id: asset.clone(),
                name: "Night".into(),
                properties: properties.clone(),
            })
            .unwrap();
        library
            .edit(Edit::PlaylistProperties {
                id: id.clone(),
                asset_id: asset.clone(),
                properties: properties.clone(),
            })
            .unwrap();
        let mut restored: Library =
            serde_json::from_value(serde_json::to_value(library).unwrap()).unwrap();
        assert_eq!(restored.presets[&asset]["Night"], properties);
        assert_eq!(restored.playlists[&id].properties[&asset], properties);
        assert!(
            restored
                .edit(Edit::SavePreset {
                    asset_id: asset.clone(),
                    name: "Broken".into(),
                    properties: BTreeMap::from([("flag".into(), json!("wrong"))])
                })
                .is_err()
        );
        restored
            .edit(Edit::PlaylistMembers {
                id: id.clone(),
                members: vec![],
            })
            .unwrap();
        assert!(restored.playlists[&id].properties.is_empty());
        restored
            .edit(Edit::DeletePreset {
                asset_id: asset.clone(),
                name: "Night".into(),
            })
            .unwrap();
        assert!(!restored.presets.contains_key(&asset));
    }

    #[test]
    fn daily_and_overnight_rules_follow_local_weekdays() {
        let rule = TimeRule {
            start: "22:00".into(),
            end: "06:00".into(),
            days: vec![7],
        };
        rule.validate().unwrap();
        assert!(rule.active(7, 23 * 60));
        assert!(rule.active(1, 5 * 60));
        assert!(!rule.active(1, 6 * 60));
        assert!(!rule.active(1, 22 * 60));
        assert!(!rule.active(7, 21 * 60));
        let all_day = TimeRule {
            start: "00:00".into(),
            end: "00:00".into(),
            days: vec![],
        };
        assert!(all_day.active(3, 10));
        for invalid in ["25:00", "12:60", "1:00", "ab:cd", "12:xx"] {
            assert!(
                TimeRule {
                    start: invalid.into(),
                    end: "12:00".into(),
                    days: vec![]
                }
                .validate()
                .is_err()
            );
        }
        let legacy: Rotation = serde_json::from_value(json!({"enabled":false})).unwrap();
        assert!(legacy.scheduled_now());
    }
}
