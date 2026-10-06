//! Embedded videos share scene textures/materials; sounds share the same libmpv owner.
//! Package media is extracted to a private temporary directory, removed with the scene.

use crate::{
    assets::Assets,
    gpu::{Buffer, Texture},
    scene::Scene,
    scene::bindings::components,
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sound_clock::SoundClock;
use std::collections::HashSet;
use std::{os::unix::net::UnixStream, path::PathBuf, rc::Rc};
use video_clock::VideoClock;
use wallpaper_media::{Playback, Player, RenderContext, clock::Timeline};

// mpv's master volume is cubic (player/audio.c::audio_get_gain). Scene sound
// volume is an amplitude multiplier, so convert that factor to the slider's
// scale while retaining the existing public master volume behavior.
fn sound_volume(master: f32, object: &Value) -> Result<f32> {
    let gain = components(&object["volume"], &[1.], 1)?[0].clamp(0., 1.);
    Ok(master * gain.cbrt())
}

struct Video {
    buffer: Buffer,
    player: Player,
    node: Option<usize>,
    clock: VideoClock,
    moving: bool,
    playing: bool,
    seek_render: bool,
    pending_seek: Option<f64>,
    file: PathBuf,
    encoded_bytes: usize,
    _directory: Rc<tempfile::TempDir>,
}
struct Sound {
    node: usize,
    player: Player,
    files: Vec<PathBuf>,
    mode: String,
    index: usize,
    revision: u64,
    next: Option<f64>,
    random: rand_chacha::ChaCha8Rng,
    clock: SoundClock,
    encoded_bytes: usize,
    _directory: Rc<tempfile::TempDir>,
}
pub(crate) struct Media {
    pending: Vec<Rc<Texture>>,
    sounds: Vec<usize>,
    assets: Option<Assets>,
    videos: Vec<Video>,
    video_nodes: Vec<Option<usize>>,
    video_events: Vec<Value>,
    players: Vec<Sound>,
    pub environment: Option<(RenderContext, UnixStream, Playback)>,
    clock: Option<Timeline>,
    last_time: Option<f64>,
}
impl Media {
    pub fn new<'a>(
        assets: &Assets,
        scene: &Scene,
        layers: impl Iterator<Item = (usize, &'a crate::render::Layer)>,
        textures: impl Iterator<Item = Rc<Texture>>,
    ) -> Self {
        let layers = layers.collect::<Vec<_>>();
        let pending = textures
            .filter(|t| t.video.borrow().is_some())
            .collect::<Vec<_>>();
        let video_nodes = pending
            .iter()
            .map(|texture| {
                layers.iter().find_map(|(node, layer)| {
                    layer
                        .base
                        .textures
                        .first()
                        .and_then(Option::as_ref)
                        .filter(|source| Rc::ptr_eq(texture, source))
                        .map(|_| *node)
                })
            })
            .collect();
        Self {
            pending,
            sounds: scene.sounds.clone(),
            assets: (!scene.sounds.is_empty()).then(|| assets.clone()),
            videos: Vec::new(),
            video_nodes,
            video_events: Vec::new(),
            players: Vec::new(),
            environment: None,
            clock: None,
            last_time: None,
        }
    }
    pub fn present(&self) -> bool {
        !self.pending.is_empty()
            || !self.sounds.is_empty()
            || !self.videos.is_empty()
            || !self.players.is_empty()
    }
    pub fn ready(&self) -> bool {
        !self.present()
            || (self.environment.is_some()
                && self
                    .videos
                    .iter()
                    .all(|v| v.player.ready() && v.pending_seek.is_none() && !v.seek_render)
                && self.players.iter().all(|s| s.player.loaded))
    }
    pub fn texture_ready(&self, texture: &Texture) -> bool {
        !self.pending.iter().any(|t| t.handle == texture.handle)
            && self.videos.iter().all(|video| {
                video.buffer.texture.handle != texture.handle
                    || (video.player.ready() && video.pending_seek.is_none() && !video.seek_render)
            })
    }
    pub fn animated(&self) -> bool {
        !self.video_events.is_empty()
            || !self.pending.is_empty() && self.videos.is_empty()
            || self.videos.iter().any(|video| {
                video.moving
                    || video.seek_render
                    || video.pending_seek.is_some()
                    || !video.player.first_frame
            })
            || self.players.iter().any(|s| s.next.is_some())
    }
    pub fn take_video_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.video_events)
    }
    pub fn attach(
        &mut self,
        gl: Rc<glow::Context>,
        mut context: RenderContext,
        wake: &UnixStream,
        playback: Playback,
        objects: &[Value],
    ) -> Result<()> {
        ensure!(
            self.environment.is_none(),
            "scene media is already attached"
        );
        context.flip_y = false;
        if !self.present() {
            self.environment = Some((context, wake.try_clone()?, playback));
            return Ok(());
        }
        ensure!(
            self.pending.len() <= 16 && self.sounds.len() <= 32,
            "scene media player limit exceeded"
        );
        let directory = Rc::new(
            tempfile::Builder::new()
                .prefix("wallpaperd-scene-")
                .tempdir()?,
        );
        let mut bytes = 0usize;
        let mut write = |name: String, data: &[u8]| -> Result<PathBuf> {
            bytes = bytes
                .checked_add(data.len())
                .context("scene media byte overflow")?;
            ensure!(
                bytes <= 512 * 1024 * 1024,
                "scene encoded media exceeds 512 MiB"
            );
            let path = directory.path().join(name);
            std::fs::write(&path, data)?;
            Ok(path)
        };
        let mut videos = Vec::new();
        for (index, texture) in self.pending.iter().enumerate() {
            let encoded_bytes = texture
                .video
                .borrow()
                .as_ref()
                .context("video payload is missing")?
                .len();
            let path = write(
                format!("video-{index}.mp4"),
                texture.video.borrow().as_ref().unwrap(),
            )?;
            let buffer = Buffer::from_texture(gl.clone(), texture.clone())?;
            let node = self.video_nodes[index].map(|node| {
                objects[node]["__videoMaster"]
                    .as_u64()
                    .unwrap_or(node as u64) as usize
            });
            let mut initial = playback;
            initial.paused |= node.is_some_and(|node| {
                objects[node]["__texture"]["playing"] == false
                    || objects[node]["__texture"]["rate"].as_f64().unwrap_or(1.) < 0.01
            });
            let player = Player::video(context.clone(), &path, initial, wake)?;
            videos.push(Video {
                buffer,
                player,
                node,
                clock: VideoClock::default(),
                moving: !initial.paused,
                playing: !initial.paused,
                seek_render: false,
                pending_seek: None,
                file: path,
                encoded_bytes,
                _directory: directory.clone(),
            });
        }
        let mut players = Vec::new();
        for node in &self.sounds {
            let object = &objects[*node];
            let mut files = Vec::new();
            let mut encoded_bytes = 0;
            for (index, name) in object["sound"]
                .as_array()
                .context("sound files")?
                .iter()
                .enumerate()
            {
                let name = name.as_str().context("sound path")?;
                let extension = std::path::Path::new(name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("audio");
                ensure!(
                    extension.chars().all(|c| c.is_ascii_alphanumeric()) && extension.len() <= 16,
                    "invalid sound extension"
                );
                let data = self.assets.as_ref().unwrap().read(name)?;
                encoded_bytes += data.len();
                files.push(write(format!("sound-{node}-{index}.{extension}"), &data)?);
            }
            let mode = object["playbackmode"].as_str().unwrap_or("loop").to_owned();
            ensure!(
                ["loop", "random", "once", "single"].contains(&mode.as_str()),
                "unsupported sound playback mode {mode}"
            );
            let mut initial = playback;
            initial.paused |= !object["__sound"]["playing"].as_bool().unwrap_or(true);
            initial.volume = sound_volume(initial.volume, object)?;
            use rand_core::SeedableRng;
            let player = Player::audio_on_server(
                &files[0],
                initial,
                wake,
                mode == "loop",
                context.pulse_server.as_deref(),
            )?;
            players.push(Sound {
                node: *node,
                player,
                files,
                mode,
                index: 0,
                revision: 0,
                next: None,
                random: rand_chacha::ChaCha8Rng::seed_from_u64(0x534f554e44 ^ *node as u64),
                clock: SoundClock::default(),
                encoded_bytes,
                _directory: directory.clone(),
            });
        }
        self.videos = videos;
        self.players = players;
        self.assets = None;
        for texture in &self.pending {
            texture.video.borrow_mut().take();
        }
        self.pending.clear();
        self.video_nodes.clear();
        self.environment = Some((context, wake.try_clone()?, playback));
        if let Some(clock) = self.clock {
            self.synchronize(clock)?;
        }
        Ok(())
    }
    pub fn playback(&mut self, playback: Playback, objects: &[Value]) -> Result<()> {
        if let Some((_, _, current)) = &mut self.environment {
            *current = playback;
        }
        self.apply_playback(playback, objects)
    }
    pub fn pause(&mut self, paused: bool, objects: &[Value]) -> Result<()> {
        if let Some((_, _, playback)) = &self.environment {
            let mut value = *playback;
            value.paused |= paused;
            self.apply_playback(value, objects)?;
        }
        Ok(())
    }
    fn apply_playback(&mut self, playback: Playback, objects: &[Value]) -> Result<()> {
        for video in &mut self.videos {
            let mut value = playback;
            value.paused |= !video.playing;
            video.player.playback(value)?;
        }
        for sound in &mut self.players {
            let object = &objects[sound.node];
            let control = &object["__sound"];
            let mut value = playback;
            value.paused |= !control["playing"].as_bool().unwrap_or(true)
                || control["paused"].as_bool().unwrap_or(false)
                || sound.next.is_some();
            value.volume = sound_volume(value.volume, object)?;
            sound.player.playback(value)?;
        }
        Ok(())
    }
    pub fn synchronize(&mut self, clock: Timeline) -> Result<()> {
        self.clock = Some(clock);
        Ok(())
    }
    pub fn poll(&mut self) -> Result<bool> {
        let was_ready = self.ready();
        let mut dirty = false;
        for video in &mut self.videos {
            video.player.poll()?;
            dirty |= video.player.needs_render(video.seek_render);
        }
        for sound in &mut self.players {
            sound.player.poll()?;
            dirty |= sound.player.ended;
        }
        Ok(dirty || (!was_ready && self.ready()))
    }
    pub fn advance(&mut self, time: f64, paused: bool, objects: &[Value]) -> Result<Vec<Value>> {
        if !self.present() {
            return Ok(Vec::new());
        }
        ensure!(
            !self.present() || self.environment.is_some(),
            "scene media requires a libmpv context"
        );
        self.poll()?;
        if self.clock.is_none() {
            self.synchronize(Timeline {
                position_ns: (time.max(0.) * 1e9) as u64,
                sampled_ns: wallpaper_media::clock::now(),
                running: !paused,
            })?;
        }
        self.last_time = Some(time);
        let mut changes = Vec::new();
        for video in &mut self.videos {
            let default = serde_json::json!({"rate":1,"playing":true,"joined":true,"loop":true});
            let control = video
                .node
                .map(|node| &objects[node]["__texture"])
                .unwrap_or(&default);
            let duration = video
                .player
                .duration()
                .unwrap_or(control["duration"].as_f64().unwrap_or(0.));
            let step = video.clock.update(
                time,
                control,
                duration,
                paused,
                self.clock,
                wallpaper_media::clock::now(),
            )?;
            video.player.set_loop(step.looping)?;
            if let Some(clock) = step.clock {
                video.player.synchronize_rate(clock, step.rate)?;
            }
            if let Some(position) = step.seek {
                // An exact seek after the last encoded timestamp can reach
                // EOF without delivering a frame. Sample the final frame,
                // retaining the logical duration in the script host.
                let position = if duration > 0. {
                    position.min((duration - video.player.frame_duration() * 1.001).max(0.))
                } else {
                    position
                };
                if video
                    .player
                    .seek_target()
                    .is_some_and(|target| (target - position).abs() < 0.000001)
                {
                    video.seek_render = true;
                } else {
                    video.pending_seek = Some(position);
                }
            }
            if let Some(position) = video.pending_seek
                && video.player.loaded
                && !video.player.pending_seek()
            {
                video.player.seek(position)?;
                video.seek_render = true;
                video.pending_seek = None;
            }
            video.moving = step.moving;
            video.playing = step.playing;
            if let Some(node) = video.node {
                for _ in 0..step.ended {
                    self.video_events.push(serde_json::json!({"node":node}));
                }
                let control = &objects[node]["__texture"];
                let update = control["duration"].as_f64() != Some(duration)
                    || step.finish && control["playing"] != false;
                if update {
                    let mut next = control.clone();
                    next["duration"] = duration.into();
                    if step.finish {
                        next["playing"] = false.into();
                        next["joined"] = false.into();
                        next["position"] = if control["rate"].as_f64().unwrap_or(1.) < 0. {
                            0.
                        } else {
                            duration
                        }
                        .into();
                        next["anchor"] = time.into();
                        next["revision"] = (control["revision"].as_u64().unwrap_or(0) + 1).into();
                    }
                    changes.push(serde_json::json!([node, ["__texture"], next]));
                }
            }
            if let Some((_, _, playback)) = &self.environment {
                let mut value = *playback;
                value.paused |= paused || !video.playing;
                video.player.playback(value)?;
            }
            let rendered = if video.seek_render {
                video
                    .player
                    .render_seek(video.buffer.handle.0.get(), video.buffer.texture.size)?
            } else {
                video.player.render(
                    video.buffer.handle.0.get(),
                    video.buffer.texture.size,
                    false,
                )?
            };
            if rendered {
                video.seek_render = false;
            }
        }
        for sound in &mut self.players {
            let object = &objects[sound.node];
            let mut control = std::borrow::Cow::Borrowed(&object["__sound"]);
            let revision = control["revision"].as_u64().unwrap_or(0);
            if revision != sound.revision {
                sound.revision = revision;
                sound.next = None;
                if control["playing"].as_bool().unwrap_or(true)
                    && control["restart"].as_bool().unwrap_or(false)
                {
                    // Audio-only EOF can unload mpv's file despite keep-open.
                    // Restart through the same core, as random sound playback
                    // already does, instead of seeking an ended file.
                    sound.player.load_file(&sound.files[sound.index])?;
                } else if !control["playing"].as_bool().unwrap_or(true) && sound.player.loaded {
                    sound.player.seek(0.)?;
                }
            }
            if !paused
                && control["playing"].as_bool().unwrap_or(true)
                && !control["paused"].as_bool().unwrap_or(false)
            {
                if sound.player.ended && sound.next.is_none() {
                    sound.player.ended = false;
                    if sound.mode == "random" {
                        let min = components(&object["mintime"], &[1.], 1)?[0].max(0.);
                        let max = components(&object["maxtime"], &[5.], 1)?[0].max(min);
                        use rand_core::Rng;
                        let unit = (sound.random.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
                        sound.next = Some(time + min as f64 + (max - min) as f64 * unit);
                    } else {
                        control.to_mut()["playing"] = Value::Bool(false);
                        control.to_mut()["revision"] = (sound.revision + 1).into();
                        control.to_mut()["__time"] = time.into();
                        changes.push(serde_json::json!([sound.node, ["__sound"], *control]));
                    }
                }
                if sound.next.is_some_and(|due| time >= due) {
                    use rand_core::Rng;
                    sound.index = (sound.random.next_u64() as usize) % sound.files.len();
                    sound.player.load_file(&sound.files[sound.index])?;
                    sound.next = None;
                    control.to_mut()["revision"] = (sound.revision + 1).into();
                    control.to_mut()["restart"] = true.into();
                    control.to_mut()["__time"] = time.into();
                    changes.push(serde_json::json!([sound.node, ["__sound"], *control]));
                }
            }
            if let Some(clock) = sound.clock.update(
                time,
                &control,
                sound.next.is_some(),
                paused,
                self.clock,
                wallpaper_media::clock::now(),
            ) {
                sound.player.synchronize(clock)?;
            }
        }
        Ok(changes)
    }
    pub fn report_swap(&self) {
        for video in &self.videos {
            video.player.report_swap();
        }
    }

    fn encoded_bytes(&self, objects: &[Value]) -> Result<usize> {
        let mut bytes = self
            .videos
            .iter()
            .map(|v| v.encoded_bytes)
            .chain(self.players.iter().map(|s| s.encoded_bytes))
            .sum::<usize>();
        bytes += self
            .pending
            .iter()
            .filter_map(|t| t.video.borrow().as_ref().map(|v| v.len()))
            .sum::<usize>();
        if let Some(assets) = &self.assets {
            for node in &self.sounds {
                for file in objects[*node]["sound"]
                    .as_array()
                    .context("sound asset list")?
                {
                    bytes = bytes
                        .checked_add(
                            assets
                                .read(file.as_str().context("sound asset path")?)?
                                .len(),
                        )
                        .context("media byte overflow")?;
                }
            }
        }
        Ok(bytes)
    }
    pub fn extend(
        &mut self,
        gl: Rc<glow::Context>,
        mut addition: Media,
        objects: &[Value],
    ) -> Result<()> {
        ensure!(
            self.videos.len() + self.pending.len() + addition.pending.len() <= 16
                && self.players.len().max(self.sounds.len()) + addition.sounds.len() <= 32,
            "scene media player limit exceeded"
        );
        let bytes = self
            .encoded_bytes(objects)?
            .checked_add(addition.encoded_bytes(objects)?)
            .context("media byte overflow")?;
        ensure!(
            bytes <= 512 * 1024 * 1024,
            "scene encoded media exceeds 512 MiB"
        );
        if let Some((context, wake, playback)) = &self.environment {
            let mut quiet = *playback;
            quiet.paused = true;
            addition.attach(gl, context.clone(), wake, quiet, objects)?;
        }
        self.pending.append(&mut addition.pending);
        self.video_nodes.append(&mut addition.video_nodes);
        self.sounds.append(&mut addition.sounds);
        if self.assets.is_none() {
            self.assets = addition.assets;
        }
        self.videos.append(&mut addition.videos);
        self.players.append(&mut addition.players);
        Ok(())
    }
    pub fn release_destroyed(&mut self, objects: &[Value], textures: &HashSet<glow::Texture>) {
        self.players
            .retain(|s| objects[s.node]["__destroyed"] != true);
        self.sounds
            .retain(|node| objects[*node]["__destroyed"] != true);
        self.videos
            .retain(|v| textures.contains(&v.buffer.texture.handle));
        let pending = std::mem::take(&mut self.pending);
        let nodes = std::mem::take(&mut self.video_nodes);
        for (texture, node) in pending.into_iter().zip(nodes) {
            if textures.contains(&texture.handle) {
                self.pending.push(texture);
                self.video_nodes.push(node);
            }
        }
        self.video_events.retain(|event| {
            event["node"]
                .as_u64()
                .is_none_or(|node| objects[node as usize]["__destroyed"] != true)
        });
    }
    pub fn resources(&self, usage: &mut crate::gpu::Usage) {
        for video in &self.videos {
            usage.buffer(&video.buffer);
        }
        for texture in &self.pending {
            usage.texture(texture);
        }
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
    }
}
impl Drop for Sound {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = std::fs::remove_file(file);
        }
    }
}

mod sound_clock {
    //! Sound commands detach playback from the shared scene clock while retaining pause position.
    use serde_json::Value;
    use wallpaper_media::clock::Timeline;

    #[derive(Default)]
    pub(super) struct SoundClock {
        position: f64,
        scene_anchor: f64,
        active: bool,
        following: bool,
        revision: u64,
        initialized: bool,
        master: Option<Timeline>,
        paused: bool,
    }
    impl SoundClock {
        pub fn update(
            &mut self,
            time: f64,
            control: &Value,
            waiting: bool,
            paused: bool,
            master: Option<Timeline>,
            now: u64,
        ) -> Option<Timeline> {
            let revision = control["revision"].as_u64().unwrap_or(0);
            let active = control["playing"].as_bool().unwrap_or(true)
                && control["paused"] != true
                && !waiting;
            let initial = !self.initialized;
            if initial {
                self.active = control["playing"].as_bool().unwrap_or(true);
                self.following = self.active && revision == 0;
                self.initialized = true;
            }
            let changed = initial
                || revision != self.revision
                || active != self.active
                || paused != self.paused
                || master != self.master;
            if revision != self.revision || active != self.active {
                let command = if revision != self.revision {
                    control["__time"].as_f64().unwrap_or(time).min(time)
                } else {
                    time
                };
                self.position = if control["playing"] == false
                    || revision != self.revision && control["restart"] == true
                {
                    0.
                } else if self.following {
                    command
                } else {
                    self.position
                        + if self.active {
                            (command - self.scene_anchor).max(0.)
                        } else {
                            0.
                        }
                };
                self.scene_anchor = command;
                self.following = false;
            }
            self.active = active;
            self.revision = revision;
            self.paused = paused;
            self.master = master;
            changed.then(|| Timeline {
                position_ns: (self.position(time).max(0.) * 1e9) as u64,
                sampled_ns: now,
                running: active && !paused && master.is_none_or(|m| m.running),
            })
        }
        pub fn position(&self, time: f64) -> f64 {
            if self.following {
                time
            } else {
                self.position
                    + if self.active {
                        (time - self.scene_anchor).max(0.)
                    } else {
                        0.
                    }
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;
        #[test]
        fn late_join_script_pause_resume_stop_restart_and_master_pause() {
            let mut clock = SoundClock::default();
            let mut c = json!({"playing":true,"paused":false,"revision":0});
            assert_eq!(
                clock
                    .update(10., &c, false, false, None, 0)
                    .unwrap()
                    .position_ns,
                10_000_000_000
            );
            c = json!({"playing":true,"paused":true,"revision":1,"__time":11.});
            assert!(
                !clock
                    .update(12., &c, false, false, None, 0)
                    .unwrap()
                    .running
            );
            assert_eq!(clock.position(20.), 11.);
            c = json!({"playing":true,"paused":false,"revision":2,"__time":20.});
            clock.update(21., &c, false, false, None, 0);
            assert_eq!(clock.position(22.), 13.);
            c = json!({"playing":false,"paused":false,"restart":true,"revision":3,"__time":22.});
            clock.update(23., &c, false, false, None, 0);
            assert_eq!(clock.position(24.), 0.);
            c = json!({"playing":true,"paused":false,"restart":true,"revision":4,"__time":24.});
            assert_eq!(
                clock
                    .update(25., &c, false, false, None, 0)
                    .unwrap()
                    .position_ns,
                1_000_000_000
            );
            let master = Timeline {
                position_ns: 25_000_000_000,
                sampled_ns: 0,
                running: false,
            };
            assert!(
                !clock
                    .update(25., &c, false, true, Some(master), 0)
                    .unwrap()
                    .running
            );
            assert!(
                clock
                    .update(
                        25.,
                        &c,
                        false,
                        false,
                        Some(Timeline {
                            running: true,
                            ..master
                        }),
                        0
                    )
                    .unwrap()
                    .running
            );
        }
    }
}

pub(crate) mod video_clock {
    //! Script video commands anchor to scene time, including reverse/zero-rate sampling.
    use anyhow::{Result, ensure};
    use serde_json::Value;
    use wallpaper_media::clock::Timeline;

    pub(super) struct Step {
        pub clock: Option<Timeline>,
        pub rate: f64,
        pub looping: bool,
        pub seek: Option<f64>,
        pub moving: bool,
        pub playing: bool,
        pub ended: usize,
        pub finish: bool,
    }
    #[derive(Default)]
    pub(super) struct VideoClock {
        revision: Option<u64>,
        last: Option<f64>,
        sampled: u64,
        raw: Option<f64>,
        paused: bool,
        master: Option<Timeline>,
        rate: f64,
        looping: bool,
        active: bool,
    }
    pub(crate) fn validate(control: &Value) -> Result<()> {
        for (name, default) in [
            ("rate", 1.),
            ("position", 0.),
            ("anchor", 0.),
            ("duration", 0.),
        ] {
            let value = control[name].as_f64().unwrap_or(default);
            ensure!(
                control[name].is_null() || control[name].is_number(),
                "invalid video {name}"
            );
            ensure!(
                value.is_finite() && value.abs() <= if name == "rate" { 100. } else { 1e12 },
                "invalid video {name}"
            );
        }
        for name in ["playing", "joined", "loop"] {
            ensure!(
                control[name].is_null() || control[name].is_boolean(),
                "invalid video {name}"
            );
        }
        Ok(())
    }
    impl VideoClock {
        pub fn update(
            &mut self,
            time: f64,
            control: &Value,
            duration: f64,
            paused: bool,
            master: Option<Timeline>,
            now: u64,
        ) -> Result<Step> {
            validate(control)?;
            let rate = control["rate"].as_f64().unwrap_or(1.);
            let active = control["playing"] != false;
            let looping = control["loop"] != false;
            let raw = if control["joined"] != false {
                time * rate
            } else {
                control["position"].as_f64().unwrap_or(0.)
                    + if active {
                        (time - control["anchor"].as_f64().unwrap_or(0.)) * rate
                    } else {
                        0.
                    }
            };
            ensure!(
                raw.is_finite() && raw.abs() <= 1e12,
                "video clock overflows"
            );
            let position = if duration > 0. {
                if looping {
                    raw.rem_euclid(duration)
                } else {
                    raw.clamp(0., duration)
                }
            } else {
                raw.max(0.)
            };
            let revision = control["revision"].as_u64().unwrap_or(0);
            let command = self.revision != Some(revision);
            let jump = self.last.is_some_and(|last| {
                active
                    && rate != 0.
                    && (time < last
                        || time - last > 0.5
                            && ((time - last) - now.saturating_sub(self.sampled) as f64 / 1e9)
                                .abs()
                                > 0.5)
            });
            let changed = command
                || jump
                || self.paused != paused
                || self.master != master
                || self.rate != rate
                || self.looping != looping
                || self.active != active;
            let forward = rate >= 0.01;
            let finish = active
                && !looping
                && duration > 0.
                && if rate < 0. {
                    raw <= 0.
                } else {
                    raw >= duration
                };
            let moving = active
                && rate != 0.
                && !finish
                && !paused
                && master.is_none_or(|master| master.running);
            let ended = if !paused && active && rate != 0. && duration > 0. {
                self.raw.map_or(0, |previous| {
                    if looping {
                        if command {
                            return 0;
                        }
                        ((raw / duration).floor() - (previous / duration).floor())
                            .abs()
                            .min(128.) as usize
                    } else {
                        usize::from(
                            finish
                                && if rate < 0. {
                                    previous > 0.
                                } else {
                                    previous < duration
                                },
                        )
                    }
                })
            } else {
                0
            };
            // Repeated draws while waiting for an asynchronous decode must not
            // restart the same reverse seek before its frame can reach the FBO.
            let resample = !forward
                && moving
                && self
                    .raw
                    .is_none_or(|previous| (raw - previous).abs() > 0.000001);
            let seek = if command || jump || resample || finish && (changed || ended > 0) {
                Some(if duration > 0. {
                    position.min((duration - 0.001).max(0.))
                } else {
                    position
                })
            } else {
                None
            };
            self.revision = Some(revision);
            self.last = Some(time);
            self.sampled = now;
            self.raw = Some(raw);
            self.paused = paused;
            self.master = master;
            self.rate = rate;
            self.looping = looping;
            self.active = active;
            Ok(Step {
                clock: changed.then_some(Timeline {
                    position_ns: (position * 1e9) as u64,
                    sampled_ns: now,
                    running: moving && forward,
                }),
                rate: rate.clamp(0.01, 100.),
                looping,
                seek,
                moving,
                playing: moving && forward,
                ended,
                finish,
            })
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;
        #[test]
        fn reverse_decode_waits_do_not_repeat_the_same_seek_but_commands_do() {
            let mut clock = VideoClock::default();
            let mut control = json!({"rate":-1,"position":1.5,"anchor":0,"playing":true,"joined":false,"loop":true,"revision":0});
            assert_eq!(
                clock.update(0., &control, 2., false, None, 0).unwrap().seek,
                Some(1.5)
            );
            for now in [5_000_000, 100_000_000, 500_000_000] {
                assert_eq!(
                    clock
                        .update(0., &control, 2., false, None, now)
                        .unwrap()
                        .seek,
                    None
                );
            }
            assert_eq!(
                clock
                    .update(0.5, &control, 2., false, None, 600_000_000)
                    .unwrap()
                    .seek,
                Some(1.)
            );
            assert_eq!(
                clock
                    .update(0.5, &control, 2., false, None, 700_000_000)
                    .unwrap()
                    .seek,
                None
            );
            control["revision"] = json!(1);
            assert_eq!(
                clock
                    .update(0.5, &control, 2., false, None, 800_000_000)
                    .unwrap()
                    .seek,
                Some(1.)
            );
        }
        #[test]
        fn reaching_a_non_looping_endpoint_seeks_its_pixel_before_stopping() {
            for (rate, position, expected) in [(-1., 0.25, 0.), (1., 1.75, 1.999)] {
                let mut clock = VideoClock::default();
                let control = json!({"rate":rate,"position":position,"anchor":0,"playing":true,"joined":false,"loop":false,"revision":0});
                clock.update(0., &control, 2., false, None, 0).unwrap();
                let step = clock
                    .update(0.5, &control, 2., false, None, 500_000_000)
                    .unwrap();
                assert!(step.finish && !step.moving);
                assert_eq!(step.ended, 1);
                assert_eq!(step.seek, Some(expected));
            }
        }
        #[test]
        fn rate_anchor_pause_seek_loop_ended_and_reverse_use_one_scene_clock() {
            let mut clock = VideoClock::default();
            let mut control = json!({"rate":2,"position":0,"anchor":0,"playing":true,"joined":false,"loop":true,"revision":0});
            let step = clock.update(0., &control, 2., false, None, 0).unwrap();
            assert_eq!(step.rate, 2.);
            assert_eq!(step.clock.unwrap().position_ns, 0);
            let step = clock.update(1., &control, 2., false, None, 0).unwrap();
            assert_eq!(step.ended, 1);
            assert!(step.moving);
            assert_eq!(
                clock.update(1., &control, 2., true, None, 0).unwrap().ended,
                0
            );
            control["rate"] = json!(0);
            control["revision"] = json!(1);
            assert!(
                !clock
                    .update(1., &control, 2., false, None, 0)
                    .unwrap()
                    .moving
            );
            control = json!({"rate":-1,"position":1.5,"anchor":1,"playing":true,"joined":false,"loop":false,"revision":2});
            let step = clock.update(1., &control, 2., false, None, 0).unwrap();
            assert_eq!(step.seek, Some(1.5));
            assert!(step.moving && !step.playing);
            assert_eq!(
                clock
                    .update(1.5, &control, 2., false, None, 0)
                    .unwrap()
                    .seek,
                Some(1.)
            );
            let step = clock.update(2.5, &control, 2., false, None, 0).unwrap();
            assert_eq!(step.ended, 1);
            assert!(step.finish && !step.moving);
            control["rate"] = json!(1);
            control["position"] = json!(0.25);
            control["anchor"] = json!(2.5);
            control["playing"] = json!(false);
            control["revision"] = json!(3);
            let step = clock.update(20., &control, 2., false, None, 0).unwrap();
            assert_eq!(step.seek, Some(0.25));
            assert!(!step.moving);
            control["rate"] = json!(101);
            assert!(clock.update(20., &control, 2., false, None, 0).is_err());
        }
        #[test]
        fn low_presentation_rate_and_a_stopped_video_do_not_repeat_seeks() {
            let mut clock = VideoClock::default();
            let mut control = json!({"rate":1,"playing":true,"joined":true,"loop":true});
            assert_eq!(
                clock
                    .update(0., &control, 10., false, None, 0)
                    .unwrap()
                    .seek,
                Some(0.)
            );
            assert_eq!(
                clock
                    .update(1., &control, 10., false, None, 1_000_000_000)
                    .unwrap()
                    .seek,
                None
            );
            assert_eq!(
                clock
                    .update(3., &control, 10., false, None, 1_100_000_000)
                    .unwrap()
                    .seek,
                Some(3.)
            );
            control["playing"] = json!(false);
            control["joined"] = json!(false);
            control["position"] = json!(3);
            clock
                .update(3., &control, 10., false, None, 1_100_000_000)
                .unwrap();
            assert_eq!(
                clock
                    .update(10., &control, 10., false, None, 1_200_000_000)
                    .unwrap()
                    .seek,
                None
            );
        }
        #[test]
        fn first_frame_after_a_rate_command_can_complete_playback() {
            let mut clock = VideoClock::default();
            let mut control = json!({"rate":1,"position":1,"anchor":0,"joined":false,"playing":false,"loop":false,"revision":0});
            assert_eq!(
                clock
                    .update(0., &control, 2., false, None, 0)
                    .unwrap()
                    .ended,
                0
            );
            control["playing"] = json!(true);
            control["rate"] = json!(2);
            control["revision"] = json!(1);
            let step = clock.update(1., &control, 2., false, None, 0).unwrap();
            assert!(step.finish);
            assert_eq!(step.ended, 1);
            control["playing"] = json!(false);
            control["position"] = json!(2);
            control["revision"] = json!(2);
            assert_eq!(
                clock
                    .update(1., &control, 2., false, None, 0)
                    .unwrap()
                    .ended,
                0
            );
        }
    }
}

pub(crate) mod events {
    //! MPRIS transport fields mapped to the official SceneScript event vocabulary.
    use serde_json::{Value, json};
    pub(crate) fn changes(old: &Value, current: &Value) -> Vec<(&'static str, Value)> {
        let mut events = Vec::new();
        if old["enabled"] != current["enabled"] {
            events.push((
                "mediaStatusChanged",
                json!({"enabled":current["enabled"].as_bool().unwrap_or(false)}),
            ));
        }
        if old["playback"] != current["playback"] {
            events.push(("mediaPlaybackChanged",json!({"state":match current["playback"].as_str(){Some("playing")=>1,Some("paused")=>2,_=>0}})));
        }
        if [
            "track_id",
            "title",
            "artist",
            "album_title",
            "album_artist",
            "genres",
            "content_type",
        ]
        .iter()
        .any(|key| old[key] != current[key])
        {
            let text = |key| current[key].as_str().unwrap_or("");
            events.push(("mediaPropertiesChanged",json!({"title":text("title"),"artist":text("artist"),"subTitle":"","albumTitle":text("album_title"),"albumArtist":text("album_artist"),"genres":text("genres"),"contentType":text("content_type")})));
        }
        if old["artwork"] != current["artwork"] {
            let art = &current["artwork"];
            let color = |key, default| art.get(key).cloned().unwrap_or(json!(default));
            let primary = color("primary", [0.0; 3]);
            let text = color("text", [1.0; 3]);
            events.push(("mediaThumbnailChanged",json!({"hasThumbnail":art.is_object(),"primaryColor":primary,"secondaryColor":color("secondary",[0.0;3]),"tertiaryColor":color("tertiary",[0.0;3]),"textColor":text,"highContrastColor":text})));
        }
        if ["position", "duration", "rate"]
            .iter()
            .any(|key| old[key] != current[key])
        {
            events.push((
                "mediaTimelineChanged",
                timeline(current, current["sampled_ns"].as_u64().unwrap_or(0)),
            ));
        }
        events
    }
    pub(crate) fn timeline(media: &Value, now: u64) -> Value {
        let finite = |key: &str, default: f64| {
            media[key]
                .as_f64()
                .filter(|v| v.is_finite())
                .unwrap_or(default)
        };
        let duration = finite("duration", 0.).max(0.);
        let delta = if media["playback"] == "playing" {
            now.saturating_sub(media["sampled_ns"].as_u64().unwrap_or(now)) as f64 / 1e9
                * finite("rate", 1.)
        } else {
            0.
        };
        let position = (finite("position", 0.) + delta).max(0.);
        json!({"position":if duration>0. {position.min(duration)} else {position},"duration":duration})
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn timeline_rate_pause_seek_duration_and_absent_clock() {
            let mut media = json!({"playback":"playing","position":3.,"duration":10.,"rate":2.,"sampled_ns":1_000_000_000u64});
            assert_eq!(
                timeline(&media, 3_000_000_000),
                json!({"position":7.,"duration":10.})
            );
            media["playback"] = json!("paused");
            assert_eq!(
                timeline(&media, 20_000_000_000),
                json!({"position":3.,"duration":10.})
            );
            media["playback"] = json!("playing");
            assert_eq!(timeline(&media, 20_000_000_000)["position"], 10.);
            assert_eq!(
                timeline(&json!({}), 20_000_000_000),
                json!({"position":0.,"duration":0.})
            );
        }
        #[test]
        fn metadata_updates_with_same_track_and_typed_palette_payloads() {
            let old =
                json!({"enabled":true,"playback":"playing","track_id":"same","title":"before"});
            let new = json!({"enabled":true,"playback":"paused","track_id":"same","title":"after","album_title":"album","artwork":{"primary":[0.2,0.3,0.4],"text":[1,1,1]}});
            let changes = changes(&old, &new);
            assert_eq!(changes[0], ("mediaPlaybackChanged", json!({"state":2})));
            assert_eq!(changes[1].1["title"], "after");
            assert_eq!(changes[1].1["albumTitle"], "album");
            assert_eq!(changes[2].1["primaryColor"], json!([0.2, 0.3, 0.4]));
            assert_eq!(changes[2].1["hasThumbnail"], true);
        }
    }
}
