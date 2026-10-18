//! One session-wide media selection; D-Bus types never cross the worker interface.

pub use artwork::Artwork;

use anyhow::{Context, Result};
use futures_lite::{
    StreamExt,
    future::{self, block_on},
    stream,
};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll, Waker},
    thread,
    time::{Duration, Instant},
};
use zbus::{
    MatchRule,
    blocking::{Connection, MessageIterator, Proxy, connection::Builder},
    message::Type,
    zvariant::OwnedValue,
};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    Playing,
    Paused,
    #[default]
    Stopped,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkStatus {
    #[default]
    None,
    Loading,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub enabled: bool,
    pub available: bool,
    pub revision: u64,
    pub player: Option<String>,
    pub track_id: String,
    pub playback: PlaybackState,
    pub position: f64,
    pub duration: f64,
    pub rate: f64,
    pub sampled_ns: u64,
    pub title: String,
    pub artist: String,
    pub album_title: String,
    pub album_artist: String,
    pub genres: String,
    pub content_type: String,
    pub artwork: Option<Artwork>,
    pub previous_artwork: Option<Artwork>,
    pub artwork_status: ArtworkStatus,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Candidate {
    owner: String,
    track_id: String,
    playback: PlaybackState,
    title: String,
    artist: String,
    album_title: String,
    album_artist: String,
    genres: String,
    artwork_url: String,
    position: f64,
    duration: f64,
    rate: f64,
    sampled_ns: u64,
}

#[derive(Default)]
struct Players {
    owners: BTreeMap<String, String>,
    candidates: BTreeMap<String, Candidate>,
    selected: Option<String>,
}

impl Players {
    fn choose(&mut self) -> Option<(&str, &Candidate)> {
        let best = self.candidates.values().map(|c| c.playback).min()?;
        if self
            .selected
            .as_ref()
            .is_none_or(|name| self.candidates.get(name).is_none_or(|c| c.playback != best))
        {
            self.selected = self
                .candidates
                .iter()
                .find(|(_, c)| c.playback == best)
                .map(|(name, _)| name.clone());
        }
        let name = self.selected.as_ref()?;
        Some((name, &self.candidates[name]))
    }
}

// Owner and descriptive fields also identify tracks from players that omit trackid.
#[derive(PartialEq, Eq)]
struct TrackKey(String, String, String, String, String, String);

#[derive(Default)]
struct Bridge {
    snapshot: Snapshot,
    key: Option<TrackKey>,
    generation: u64,
    pending_artwork: Option<(u64, String)>,
}

impl Bridge {
    fn deactivate(&mut self) -> bool {
        let before = self.snapshot.clone();
        self.generation += 1;
        self.pending_artwork = None;
        self.key = None;
        self.snapshot = Snapshot {
            enabled: true,
            revision: before.revision,
            ..Snapshot::default()
        };
        let changed = before != self.snapshot;
        if changed {
            self.snapshot.revision += 1;
        }
        changed
    }

    fn update(
        &mut self,
        available: bool,
        player: Option<(&str, &Candidate)>,
        error: Option<String>,
    ) -> bool {
        let before = self.snapshot.clone();
        self.snapshot.enabled = true;
        self.snapshot.available = available;
        if error.is_some() || !available || self.snapshot.artwork_status != ArtworkStatus::Failed {
            self.snapshot.error = error;
        }
        let key = player.map(|(_, c)| {
            TrackKey(
                c.owner.clone(),
                c.track_id.clone(),
                c.title.clone(),
                c.artist.clone(),
                c.album_title.clone(),
                c.artwork_url.clone(),
            )
        });
        if key != self.key {
            self.generation += 1;
            self.pending_artwork = None;
            self.snapshot.previous_artwork = player.and_then(|_| {
                self.snapshot
                    .artwork
                    .take()
                    .or_else(|| self.snapshot.previous_artwork.take())
            });
            self.snapshot.artwork = None;
            self.snapshot.artwork_status = ArtworkStatus::None;
            if available {
                self.snapshot.error = None;
            }
            if let Some((_, c)) = player.filter(|(_, c)| !c.artwork_url.is_empty()) {
                self.pending_artwork = Some((self.generation, c.artwork_url.clone()));
                self.snapshot.artwork_status = ArtworkStatus::Loading;
            }
            self.key = key;
        }
        if let Some((name, c)) = player {
            self.snapshot.player = Some(name.into());
            self.snapshot.track_id = c.track_id.clone();
            self.snapshot.playback = c.playback;
            self.snapshot.position = c.position;
            self.snapshot.duration = c.duration;
            self.snapshot.rate = c.rate;
            self.snapshot.sampled_ns = c.sampled_ns;
            self.snapshot.title = c.title.clone();
            self.snapshot.artist = c.artist.clone();
            self.snapshot.album_title = c.album_title.clone();
            self.snapshot.album_artist = c.album_artist.clone();
            self.snapshot.genres = c.genres.clone();
            // MPRIS does not provide a universal music/video discriminator.
            self.snapshot.content_type.clear();
        } else {
            self.snapshot = Snapshot {
                enabled: true,
                available,
                revision: before.revision,
                error: self.snapshot.error.take(),
                ..Snapshot::default()
            };
        }
        let changed = before != self.snapshot;
        if changed {
            self.snapshot.revision += 1;
        }
        changed
    }

    fn complete_artwork(&mut self, generation: u64, result: Result<Artwork>) -> bool {
        if generation != self.generation || self.key.is_none() {
            return false;
        }
        match result {
            Ok(artwork) => {
                self.snapshot.artwork = Some(artwork);
                self.snapshot.artwork_status = ArtworkStatus::Ready;
                self.snapshot.error = None;
            }
            Err(error) => {
                self.snapshot.artwork_status = ArtworkStatus::Failed;
                self.snapshot.error =
                    Some(bounded_text(&format!("loading media artwork: {error:#}")));
            }
        }
        self.snapshot.revision += 1;
        true
    }
}

#[derive(Default)]
struct Control {
    active: bool,
    generation: u64,
    cancel: Option<Waker>,
}

struct Shared {
    bridge: Mutex<Bridge>,
    artwork_ready: Condvar,
    control: Mutex<Control>,
    control_ready: Condvar,
    writer: Mutex<UnixStream>,
}

impl Shared {
    fn notify(&self) {
        let _ = self.writer.lock().write_all(&[1]);
    }
    fn publish(
        &self,
        generation: u64,
        available: bool,
        player: Option<(&str, &Candidate)>,
        error: Option<String>,
    ) -> bool {
        let control = self.control.lock();
        if !control.active || control.generation != generation {
            return false;
        }
        let mut bridge = self.bridge.lock();
        let changed = bridge.update(available, player, error);
        let artwork_pending = bridge.pending_artwork.is_some();
        drop(bridge);
        if changed {
            self.notify();
        }
        if artwork_pending {
            self.artwork_ready.notify_one();
        }
        true
    }

    fn set_active(&self, active: bool) {
        let cancel = {
            let mut control = self.control.lock();
            if control.active == active {
                return;
            }
            control.active = active;
            control.generation = control.generation.wrapping_add(1);
            control.cancel.take()
        };
        if !active && self.bridge.lock().deactivate() {
            self.notify();
        }
        if let Some(cancel) = cancel {
            cancel.wake();
        }
        self.control_ready.notify_one();
    }

    fn wait_active(&self) -> u64 {
        let mut control = self.control.lock();
        while !control.active {
            self.control_ready.wait(&mut control);
        }
        control.generation
    }

    fn active(&self, generation: u64) -> bool {
        let control = self.control.lock();
        control.active && control.generation == generation
    }

    fn wait_retry(&self, generation: u64) {
        let mut control = self.control.lock();
        let deadline = Instant::now() + Duration::from_secs(5);
        while control.active && control.generation == generation {
            if self
                .control_ready
                .wait_until(&mut control, deadline)
                .timed_out()
            {
                break;
            }
        }
    }
}

struct Cancelled<'a> {
    shared: &'a Shared,
    generation: u64,
}

impl std::future::Future for Cancelled<'_> {
    type Output = WatchNext;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let mut control = self.shared.control.lock();
        if !control.active || control.generation != self.generation {
            Poll::Ready(WatchNext::Cancelled)
        } else {
            control.cancel = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

enum WatchNext {
    Message(Option<zbus::Result<zbus::Message>>),
    Cancelled,
}

pub struct Monitor {
    pub wake: UnixStream,
    shared: Arc<Shared>,
}

impl Monitor {
    pub fn start() -> io::Result<Self> {
        let (wake, writer) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            bridge: Mutex::new(Bridge {
                snapshot: Snapshot {
                    enabled: true,
                    ..Snapshot::default()
                },
                ..Bridge::default()
            }),
            artwork_ready: Condvar::new(),
            control: Mutex::new(Control::default()),
            control_ready: Condvar::new(),
            writer: Mutex::new(writer),
        });
        let covers = shared.clone();
        thread::Builder::new()
            .name("wallpaper-artwork".into())
            .spawn(move || {
                loop {
                    let (generation, uri) = {
                        let mut bridge = covers.bridge.lock();
                        while bridge.pending_artwork.is_none() {
                            covers.artwork_ready.wait(&mut bridge);
                        }
                        bridge.pending_artwork.take().unwrap()
                    };
                    let result = artwork::load(&uri);
                    if covers.bridge.lock().complete_artwork(generation, result) {
                        covers.notify();
                    }
                }
            })?;
        let events = shared.clone();
        thread::Builder::new()
            .name("wallpaper-mpris".into())
            .spawn(move || {
                loop {
                    let generation = events.wait_active();
                    if let Err(error) = watch(&events, generation) {
                        events.publish(generation, false, None, Some(format!("MPRIS: {error:#}")));
                    }
                    events.wait_retry(generation);
                }
            })?;
        Ok(Self { wake, shared })
    }

    pub fn set_active(&self, active: bool) {
        self.shared.set_active(active);
    }

    pub fn receive(&mut self) -> io::Result<Snapshot> {
        let mut bytes = [0; 64];
        loop {
            match self.wake.read(&mut bytes) {
                Ok(0) => return Err(io::ErrorKind::BrokenPipe.into()),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(self.shared.bridge.lock().snapshot.clone())
    }
}

fn watch(shared: &Shared, generation: u64) -> Result<()> {
    let conn = Builder::session()?
        .method_timeout(Duration::from_secs(1))
        .build()?;
    if !shared.active(generation) {
        return Ok(());
    }
    let dbus = Proxy::new(
        &conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    // Subscribe before discovery so a player cannot appear between the two steps.
    // Keep separate filtered streams: activating the connection-wide iterator
    // would also queue every synchronous discovery reply and can stall zbus's
    // socket reader when its bounded broadcast channel fills.
    let [owners, properties, seeked] = mpris_match_rules()?;
    let owners = MessageIterator::for_match_rule(owners, &conn, Some(64))?.into_inner();
    if !shared.active(generation) {
        return Ok(());
    }
    let properties = MessageIterator::for_match_rule(properties, &conn, Some(64))?.into_inner();
    if !shared.active(generation) {
        return Ok(());
    }
    let seeked = MessageIterator::for_match_rule(seeked, &conn, Some(64))?.into_inner();
    let mut messages = Box::pin(stream::race(stream::race(owners, properties), seeked));
    if !shared.active(generation) {
        return Ok(());
    }
    let mut players = Players::default();
    let names: Vec<String> = dbus.call("ListNames", &())?;
    for name in names.into_iter().filter(|name| name.starts_with(PREFIX)) {
        if !shared.active(generation) {
            return Ok(());
        }
        refresh_name(&conn, &dbus, &mut players, &name);
    }
    if !shared.publish(generation, true, players.choose(), None) {
        return Ok(());
    }
    loop {
        let next = async { WatchNext::Message(messages.next().await) };
        let message = match block_on(future::or(Cancelled { shared, generation }, next)) {
            WatchNext::Message(Some(message)) => message?,
            WatchNext::Message(None) => anyhow::bail!("session bus disconnected"),
            WatchNext::Cancelled => return Ok(()),
        };
        if !shared.active(generation) {
            return Ok(());
        }
        let header = message.header();
        let interface = header.interface().map(|v| v.as_str()).unwrap_or("");
        let member = header.member().map(|v| v.as_str()).unwrap_or("");
        if interface == "org.freedesktop.DBus"
            && member == "NameOwnerChanged"
            && header
                .sender()
                .is_some_and(|v| v.as_str() == "org.freedesktop.DBus")
        {
            let Ok((name, _, _)) = message.body().deserialize::<(String, String, String)>() else {
                continue;
            };
            if !name.starts_with(PREFIX) {
                continue;
            }
            // Re-read the current owner: queued startup signals may describe an older instance.
            refresh_name(&conn, &dbus, &mut players, &name);
        } else if interface == "org.freedesktop.DBus.Properties"
            && member == "PropertiesChanged"
            && header.path().is_some_and(|p| p.as_str() == PATH)
        {
            let Ok((interface, changed, invalidated)) =
                message
                    .body()
                    .deserialize::<(String, BTreeMap<String, OwnedValue>, Vec<String>)>()
            else {
                continue;
            };
            if interface != PLAYER
                || !["Metadata", "PlaybackStatus", "Rate"]
                    .iter()
                    .any(|key| changed.contains_key(*key) || invalidated.iter().any(|v| v == key))
            {
                continue;
            }
            let sender = header.sender().map(|v| v.as_str()).unwrap_or("");
            let names: Vec<_> = players
                .owners
                .iter()
                .filter(|(_, owner)| *owner == sender)
                .map(|(name, _)| name.clone())
                .collect();
            if names.is_empty() {
                continue;
            }
            for name in names {
                if let Ok(candidate) = read_player(&conn, sender) {
                    players.candidates.insert(name, candidate);
                } else {
                    players.candidates.remove(&name);
                }
            }
        } else if interface == PLAYER
            && member == "Seeked"
            && header.path().is_some_and(|p| p.as_str() == PATH)
        {
            let Ok((position,)) = message.body().deserialize::<(i64,)>() else {
                continue;
            };
            let sender = header.sender().map(|v| v.as_str()).unwrap_or("");
            for candidate in players
                .candidates
                .values_mut()
                .filter(|c| c.owner == sender)
            {
                candidate.position = position.max(0) as f64 / 1e6;
                candidate.sampled_ns = wallpaper_media::clock::now();
            }
        } else {
            continue;
        }
        if !shared.publish(generation, true, players.choose(), None) {
            return Ok(());
        }
    }
}

fn mpris_match_rules() -> zbus::Result<[MatchRule<'static>; 3]> {
    Ok([
        MatchRule::builder()
            .msg_type(Type::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg0ns("org.mpris.MediaPlayer2")?
            .build(),
        MatchRule::builder()
            .msg_type(Type::Signal)
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .path(PATH)?
            .arg(0, PLAYER)?
            .build(),
        MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(PLAYER)?
            .member("Seeked")?
            .path(PATH)?
            .build(),
    ])
}

fn refresh_name(conn: &Connection, dbus: &Proxy<'_>, players: &mut Players, name: &str) {
    players.candidates.remove(name);
    players.owners.remove(name);
    if let Ok(owner) = dbus.call::<_, _, String>("GetNameOwner", &(name,)) {
        if let Ok(candidate) = read_player(conn, &owner) {
            players.candidates.insert(name.into(), candidate);
        }
        players.owners.insert(name.into(), owner);
    }
}

fn read_player(conn: &Connection, owner: &str) -> Result<Candidate> {
    let properties = Proxy::new(conn, owner, PATH, "org.freedesktop.DBus.Properties")?;
    let mut values: BTreeMap<String, OwnedValue> = properties.call("GetAll", &(PLAYER,))?;
    let status = values
        .remove("PlaybackStatus")
        .context("missing PlaybackStatus")?;
    let playback = match String::try_from(status)?.as_str() {
        "Playing" => PlaybackState::Playing,
        "Paused" => PlaybackState::Paused,
        "Stopped" => PlaybackState::Stopped,
        _ => anyhow::bail!("invalid PlaybackStatus"),
    };
    let metadata = values
        .remove("Metadata")
        .and_then(|v| std::collections::HashMap::<String, OwnedValue>::try_from(v).ok())
        .unwrap_or_default();
    let text = |key: &str| {
        bounded_text(
            metadata
                .get(key)
                .and_then(|v| <&str>::try_from(v).ok())
                .unwrap_or(""),
        )
    };
    let list = |key: &str| {
        bounded_text(
            &metadata
                .get(key)
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| Vec::<String>::try_from(v).ok())
                .unwrap_or_default()
                .join(", "),
        )
    };
    let track_id = metadata
        .get("mpris:trackid")
        .and_then(|v| <&zbus::zvariant::ObjectPath>::try_from(v).ok())
        .map(|v| bounded_text(v.as_str()))
        .unwrap_or_default();
    let position = values
        .remove("Position")
        .and_then(|v| i64::try_from(v).ok())
        .unwrap_or(0)
        .max(0) as f64
        / 1e6;
    let duration = metadata
        .get("mpris:length")
        .and_then(|v| i64::try_from(v).ok())
        .unwrap_or(0)
        .max(0) as f64
        / 1e6;
    let rate = values
        .remove("Rate")
        .and_then(|v| f64::try_from(v).ok())
        .filter(|r| r.is_finite() && r.abs() <= 128.)
        .unwrap_or(1.);
    Ok(Candidate {
        owner: owner.into(),
        track_id,
        playback,
        title: text("xesam:title"),
        artist: list("xesam:artist"),
        album_title: text("xesam:album"),
        album_artist: list("xesam:albumArtist"),
        genres: list("xesam:genre"),
        artwork_url: text("mpris:artUrl"),
        position,
        duration,
        rate,
        sampled_ns: wallpaper_media::clock::now(),
    })
}

pub(crate) fn bounded_text(value: &str) -> String {
    // Keep Unicode metadata and the combined worker command inside the 64 KiB pipe limit.
    let mut text = String::new();
    for c in value.chars().filter(|c| !c.is_control()) {
        if text.len() + c.len_utf8() > 4096 {
            break;
        }
        text.push(c);
    }
    text
}

mod artwork {
    //! Decode and cache album art outside the session and render loops.
    use anyhow::{Context, Result};
    use image::{ImageReader, RgbaImage, imageops::FilterType};
    use serde::{Deserialize, Serialize};
    use std::{
        fs::OpenOptions,
        io::{Cursor, Read},
        os::unix::fs::OpenOptionsExt,
        path::PathBuf,
        time::Duration,
    };

    const MAX_BYTES: u64 = 8 * 1024 * 1024;

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    pub struct Artwork {
        pub path: PathBuf,
        pub primary: [f32; 3],
        pub secondary: [f32; 3],
        pub tertiary: [f32; 3],
        pub text: [f32; 3],
    }

    pub fn load(uri: &str) -> Result<Artwork> {
        load_into(uri, &crate::store::cache_path().with_file_name("media"))
    }

    fn load_into(uri: &str, cache: &std::path::Path) -> Result<Artwork> {
        let url = url::Url::parse(uri).context("invalid artwork URL")?;
        let mut bytes = Vec::new();
        match url.scheme() {
            "file" => {
                let path = url
                    .to_file_path()
                    .map_err(|_| anyhow::anyhow!("artwork must be a local file URL"))?;
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(path)?;
                let metadata = file.metadata()?;
                anyhow::ensure!(metadata.is_file(), "artwork must be a regular file");
                anyhow::ensure!(metadata.len() <= MAX_BYTES, "artwork exceeds 8 MiB");
                file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
            }
            "http" | "https" => {
                let agent = ureq::Agent::config_builder()
                    .timeout_global(Some(Duration::from_secs(5)))
                    .build()
                    .new_agent();
                agent
                    .get(uri)
                    .call()?
                    .body_mut()
                    .as_reader()
                    .take(MAX_BYTES + 1)
                    .read_to_end(&mut bytes)?;
            }
            _ => anyhow::bail!("unsupported artwork URL scheme"),
        }
        anyhow::ensure!(bytes.len() as u64 <= MAX_BYTES, "artwork exceeds 8 MiB");
        let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let image = reader
            .decode()?
            .resize(512, 512, FilterType::Triangle)
            .into_rgba8();
        let [primary, secondary, tertiary] = palette(&image);
        let path = crate::pixels::cache_frame(&image, cache)?;
        Ok(Artwork {
            path,
            primary,
            secondary,
            tertiary,
            text: contrasting(primary),
        })
    }

    fn palette(image: &RgbaImage) -> [[f32; 3]; 3] {
        let mut bins = [0u64; 4096];
        for pixel in image.pixels() {
            let [r, g, b, a] = pixel.0;
            bins[((r as usize >> 4) << 8) | ((g as usize >> 4) << 4) | (b as usize >> 4)] +=
                u64::from(a);
        }
        let mut bins: Vec<_> = bins
            .into_iter()
            .enumerate()
            .filter(|(_, count)| *count > 0)
            .collect();
        bins.sort_unstable_by_key(|(bin, count)| (std::cmp::Reverse(*count), *bin));
        let mut colors = Vec::new();
        for (bin, _) in bins {
            let color = [
                ((bin >> 8) & 15) as f32 / 15.0,
                ((bin >> 4) & 15) as f32 / 15.0,
                (bin & 15) as f32 / 15.0,
            ];
            if colors.iter().all(|other: &[f32; 3]| {
                other
                    .iter()
                    .zip(color)
                    .map(|(a, b)| (a - b).powi(2))
                    .sum::<f32>()
                    > 0.1
            }) {
                colors.push(color);
            }
            if colors.len() == 3 {
                break;
            }
        }
        let primary = colors.first().copied().unwrap_or([0.0; 3]);
        [
            primary,
            colors.get(1).copied().unwrap_or(primary),
            colors.get(2).copied().unwrap_or(primary),
        ]
    }

    fn contrasting(color: [f32; 3]) -> [f32; 3] {
        let linear = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let l = 0.2126 * linear(color[0]) + 0.7152 * linear(color[1]) + 0.0722 * linear(color[2]);
        if (l + 0.05) / 0.05 >= 1.05 / (l + 0.05) {
            [0.0; 3]
        } else {
            [1.0; 3]
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn file_urls_decode_escaped_paths_resize_and_reuse_pixels() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("封面 with spaces.png");
            RgbaImage::from_pixel(1024, 256, image::Rgba([255, 0, 0, 255]))
                .save(&path)
                .unwrap();
            let uri = url::Url::from_file_path(path).unwrap();
            let cache = root.path().join("cache");
            let cover = load_into(uri.as_str(), &cache).unwrap();
            assert_eq!(image::open(&cover.path).unwrap().width(), 512);
            assert_eq!(cover.primary, [1.0, 0.0, 0.0]);
            assert_eq!(load_into(uri.as_str(), &cache).unwrap().path, cover.path);
            assert!(load("data:image/png;base64,anything").is_err());
            let directory = url::Url::from_file_path(root.path()).unwrap();
            assert!(load_into(directory.as_str(), &cache).is_err());
            assert_eq!(contrasting([0.0; 3]), [1.0; 3]);
            assert_eq!(contrasting([1.0; 3]), [0.0; 3]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpris_bus_rules_filter_on_a_private_session_bus() {
        let status = std::process::Command::new("timeout")
            .args(["10s", "dbus-run-session", "--"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "media::tests::mpris_bus_rules_private_bus_child",
                "--ignored",
                "--nocapture",
            ])
            .status()
            .unwrap();
        assert!(status.success(), "private D-Bus test failed: {status}");
    }

    #[test]
    #[ignore = "runs only as the child of the private D-Bus test"]
    fn mpris_bus_rules_private_bus_child() {
        use std::os::fd::AsRawFd;

        let listener = Connection::session().unwrap();
        let [owners, properties, seeked] = mpris_match_rules().unwrap();
        let owners = MessageIterator::for_match_rule(owners, &listener, Some(8))
            .unwrap()
            .into_inner();
        let properties = MessageIterator::for_match_rule(properties, &listener, Some(8))
            .unwrap()
            .into_inner();
        let seeked = MessageIterator::for_match_rule(seeked, &listener, Some(8))
            .unwrap()
            .into_inner();
        let mut messages = Box::pin(stream::race(stream::race(owners, properties), seeked));

        let emitter = Connection::session().unwrap();
        emitter
            .emit_signal(None::<&str>, PATH, "org.example.Unrelated", "Changed", &())
            .unwrap();
        emitter
            .request_name("org.mpris.MediaPlayer2.WallpaperdTest")
            .unwrap();
        emitter
            .emit_signal(
                None::<&str>,
                PATH,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(
                    PLAYER,
                    BTreeMap::<String, OwnedValue>::new(),
                    Vec::<String>::new(),
                ),
            )
            .unwrap();
        emitter
            .emit_signal(None::<&str>, PATH, PLAYER, "Seeked", &(123_i64,))
            .unwrap();

        let mut received = std::collections::BTreeSet::new();
        for _ in 0..3 {
            let message = block_on(messages.next()).unwrap().unwrap();
            let header = message.header();
            let interface = header.interface().unwrap().as_str();
            let member = header.member().unwrap().as_str();
            assert_ne!(interface, "org.example.Unrelated");
            received.insert((interface.to_owned(), member.to_owned()));
        }
        assert_eq!(
            received,
            [
                (
                    "org.freedesktop.DBus".to_owned(),
                    "NameOwnerChanged".to_owned()
                ),
                (
                    "org.freedesktop.DBus.Properties".to_owned(),
                    "PropertiesChanged".to_owned()
                ),
                (PLAYER.to_owned(), "Seeked".to_owned()),
            ]
            .into()
        );

        let unique_names = || {
            let dbus = Proxy::new(
                &listener,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            )
            .unwrap();
            dbus.call::<_, _, Vec<String>>("ListNames", &())
                .unwrap()
                .into_iter()
                .filter(|name| name.starts_with(':'))
                .count()
        };
        let baseline = unique_names();
        let mut monitor = Monitor::start().unwrap();
        assert_eq!(
            unique_names(),
            baseline,
            "inactive monitor connected to D-Bus"
        );

        let receive = |monitor: &mut Monitor| {
            let mut fd = libc::pollfd {
                fd: monitor.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(unsafe { libc::poll(&mut fd, 1, 2_000) }, 1);
            monitor.receive().unwrap()
        };
        monitor.set_active(true);
        assert!(receive(&mut monitor).available);
        assert_eq!(unique_names(), baseline + 1);

        monitor.set_active(false);
        assert!(!receive(&mut monitor).available);
        let deadline = Instant::now() + Duration::from_secs(2);
        while unique_names() != baseline && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            unique_names(),
            baseline,
            "inactive monitor kept its D-Bus connection"
        );

        monitor.set_active(true);
        assert!(
            receive(&mut monitor).available,
            "reactivation did not rediscover the bus"
        );
    }

    #[test]
    fn playing_selection_is_sticky_and_falls_back_on_pause_or_exit() {
        let mut players = Players::default();
        let a = Candidate {
            owner: ":1.1".into(),
            playback: PlaybackState::Playing,
            ..Candidate::default()
        };
        let b = Candidate {
            owner: ":1.2".into(),
            ..a.clone()
        };
        players.candidates.insert("music".into(), a);
        assert_eq!(players.choose().unwrap().0, "music");
        players.candidates.insert("browser".into(), b);
        assert_eq!(players.choose().unwrap().0, "music");
        players.candidates.get_mut("music").unwrap().playback = PlaybackState::Paused;
        assert_eq!(players.choose().unwrap().0, "browser");
        players.candidates.remove("browser");
        assert_eq!(players.choose().unwrap().0, "music");
        players.candidates.clear();
        assert!(players.choose().is_none());
    }

    #[test]
    fn late_cover_cannot_replace_new_track_or_restarted_player() {
        let mut bridge = Bridge::default();
        let mut candidate = Candidate {
            owner: ":1.1".into(),
            title: "A".into(),
            artwork_url: "file:///a".into(),
            ..Candidate::default()
        };
        bridge.update(true, Some(("music", &candidate)), None);
        let old = bridge.generation;
        candidate.title = "B".into();
        bridge.update(true, Some(("music", &candidate)), None);
        assert!(!bridge.complete_artwork(old, Ok(Artwork::default())));
        assert!(bridge.snapshot.artwork.is_none());
        let old = bridge.generation;
        candidate.owner = ":1.2".into();
        bridge.update(true, Some(("music", &candidate)), None);
        assert!(!bridge.complete_artwork(old, Ok(Artwork::default())));
        bridge.update(false, None, None);
        assert!(!bridge.complete_artwork(bridge.generation - 1, Ok(Artwork::default())));
        assert!(bridge.snapshot.player.is_none());
    }

    #[test]
    fn pause_preserves_cover_and_new_track_keeps_it_only_as_previous() {
        let mut bridge = Bridge::default();
        let mut candidate = Candidate {
            title: "A".into(),
            artwork_url: "file:///a".into(),
            ..Candidate::default()
        };
        bridge.update(true, Some(("music", &candidate)), None);
        bridge.complete_artwork(bridge.generation, Ok(Artwork::default()));
        candidate.playback = PlaybackState::Paused;
        bridge.update(true, Some(("music", &candidate)), None);
        assert!(bridge.snapshot.artwork.is_some());
        candidate.title = "B".into();
        bridge.update(true, Some(("music", &candidate)), None);
        assert!(bridge.snapshot.artwork.is_none());
        assert!(bridge.snapshot.previous_artwork.is_some());
        bridge.complete_artwork(bridge.generation, Err(anyhow::anyhow!("bad image")));
        assert_eq!(bridge.snapshot.title, "B");
        assert_eq!(bridge.snapshot.artwork_status, ArtworkStatus::Failed);
        candidate.playback = PlaybackState::Playing;
        bridge.update(true, Some(("music", &candidate)), None);
        assert!(
            bridge
                .snapshot
                .error
                .as_ref()
                .unwrap()
                .contains("bad image")
        );
        assert_eq!(bounded_text(&"封面\0".repeat(2000)).len(), 4095);
    }
}
