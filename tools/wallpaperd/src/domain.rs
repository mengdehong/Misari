//! Domain and protocol values shared by the session, presentation and content engines.
use serde::{Deserialize, Serialize};
pub use wallpaper_media::Fit;

pub const API: u32 = 1;

#[derive(Debug, Deserialize, Serialize)]
pub struct Request {
    pub api: u32,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Error {
    pub code: String,
    pub message: String,
}

impl Error {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Default, Deserialize)]
pub struct PlaybackPatch {
    pub paused: Option<bool>,
    pub mute: Option<bool>,
    pub fps: Option<u32>,
    pub volume: Option<u32>,
}

impl PlaybackPatch {
    pub fn update(&self, current: Playback) -> Result<Playback, Error> {
        Playback {
            paused: self.paused.unwrap_or(current.paused),
            mute: self.mute.unwrap_or(current.mute),
            fps: self.fps.unwrap_or(current.fps),
            volume: self.volume.unwrap_or(current.volume),
        }
        .validate()
    }

    pub fn empty(&self) -> bool {
        self.paused.is_none() && self.mute.is_none() && self.fps.is_none() && self.volume.is_none()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Selection {
    /// The stable serialized ID is retained at protocol/persistence boundaries.
    pub asset_id: String,
    pub fit: Fit,
}

#[derive(Clone, Copy)]
pub enum AssetRef<'a> {
    Local(&'a std::path::Path),
    Project(&'a std::path::Path),
}
impl<'a> AssetRef<'a> {
    pub fn parse(id: &'a str) -> Option<Self> {
        id.strip_prefix("we:")
            .map(|path| Self::Project(std::path::Path::new(path)))
            .or_else(|| {
                id.strip_prefix("local:")
                    .map(|path| Self::Local(std::path::Path::new(path)))
            })
    }
    pub fn path(self) -> &'a std::path::Path {
        match self {
            Self::Local(path) | Self::Project(path) => path,
        }
    }
}
impl Selection {
    pub fn asset(&self) -> AssetRef<'_> {
        AssetRef::parse(&self.asset_id)
            .unwrap_or_else(|| AssetRef::Local(std::path::Path::new(&self.asset_id)))
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Transition {
    #[default]
    Cut,
    Fade,
    Disc,
    Honeycomb,
    Spiral,
    Stripes,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Playback {
    pub paused: bool,
    pub mute: bool,
    pub fps: u32,
    pub volume: u32,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            paused: false,
            mute: true,
            fps: 30,
            volume: 100,
        }
    }
}

impl Playback {
    pub fn validate(self) -> Result<Self, Error> {
        if !(1..=240).contains(&self.fps) {
            return Err(Error::new("bad_request", "fps must be between 1 and 240"));
        }
        if self.volume > 100 {
            return Err(Error::new(
                "bad_request",
                "volume must be between 0 and 100",
            ));
        }
        Ok(self)
    }

    /// Worker-local suspension and candidate silence never change user intent.
    pub fn for_engine(self, throttled: bool, failed: bool, candidate: bool) -> Self {
        Self {
            paused: self.paused || throttled || failed,
            mute: self.mute || candidate,
            ..self
        }
    }
}

/// Environmental evidence is separate from accepted user settings.
#[derive(Clone, Copy, Default)]
pub struct PlaybackPolicy {
    pub session: crate::policy::logind::Policy,
    pub fullscreen: bool,
    pub other_audio: bool,
}

impl PlaybackPolicy {
    pub fn commanded(self, user: Playback) -> Playback {
        Playback {
            paused: user.paused
                || self.session.locked
                || self.session.sleeping
                || self.session.inactive
                || self.fullscreen,
            mute: user.mute || self.other_audio,
            ..user
        }
    }

    pub fn pause_reasons(self, user: Playback, runtime: &WorkerRuntime) -> Vec<&'static str> {
        [
            (user.paused, "user"),
            (self.session.locked, "session_locked"),
            (self.session.sleeping, "sleep"),
            (self.session.inactive, "session_inactive"),
            (self.fullscreen, "fullscreen"),
            (runtime.throttled, "frame_throttled"),
            (runtime.failed, "playback_error"),
        ]
        .into_iter()
        .filter_map(|(active, reason)| active.then_some(reason))
        .collect()
    }

    pub fn mute_reasons(self, user: Playback) -> Vec<&'static str> {
        [(user.mute, "user"), (self.other_audio, "other_audio")]
            .into_iter()
            .filter_map(|(active, reason)| active.then_some(reason))
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkerCommand {
    pub id: u64,
    #[serde(flatten)]
    pub action: WorkerAction,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorkerAction {
    Apply {
        selection: Selection,
        #[serde(default)]
        transition: Transition,
        #[serde(default)]
        playback: Playback,
        #[serde(default)]
        properties: crate::properties::Values,
    },
    SetPlayback {
        playback: Playback,
    },
    SetProperties {
        selection_id: u64,
        asset_id: String,
        properties: crate::properties::Values,
    },
    Snapshot {
        selection_id: u64,
    },
    SetClock {
        asset_id: String,
        clock: wallpaper_media::clock::Timeline,
    },
    SetMedia {
        media: Box<crate::media::Snapshot>,
    },
    SetAudio {
        audio: Box<we_scene::audio::AudioSnapshot>,
    },
    Release,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkerReply {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<WorkerRuntime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Image,
    Libmpv,
    RustScene,
    Shader,
    Cef,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WorkerRuntime {
    /// The committed instance that produced this observation.
    #[serde(default)]
    pub selection_id: u64,
    #[serde(default)]
    pub audio: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<Backend>,
    pub throttled: bool,
    pub failed: bool,
    #[serde(default)]
    pub pointer: PointerScope,
    #[serde(default)]
    pub properties: bool,
    #[serde(default)]
    pub media: bool,
    #[serde(default)]
    pub media_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerScope {
    #[default]
    None,
    Desktop,
    Global,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_reason_combinations_never_change_the_users_settings() {
        for bits in 0u16..512 {
            let user = Playback {
                paused: bits & 1 != 0,
                mute: bits & 2 != 0,
                ..Default::default()
            };
            let policy = PlaybackPolicy {
                session: crate::policy::logind::Policy {
                    locked: bits & 4 != 0,
                    sleeping: bits & 8 != 0,
                    inactive: bits & 16 != 0,
                    ..Default::default()
                },
                fullscreen: bits & 32 != 0,
                other_audio: bits & 64 != 0,
            };
            let runtime = WorkerRuntime {
                throttled: bits & 128 != 0,
                failed: bits & 256 != 0,
                ..Default::default()
            };
            let actual =
                policy
                    .commanded(user)
                    .for_engine(runtime.throttled, runtime.failed, false);
            assert_eq!(
                actual.paused,
                !policy.pause_reasons(user, &runtime).is_empty()
            );
            assert_eq!(actual.mute, !policy.mute_reasons(user).is_empty());
            assert_eq!(policy.commanded(user).fps, user.fps);
            assert_eq!(policy.commanded(user).volume, user.volume);
        }
    }
}
