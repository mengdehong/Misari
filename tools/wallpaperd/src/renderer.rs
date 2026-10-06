//! Process isolation with nonblocking pipes; the session owns all reply handling.
use crate::{
    domain::{Playback, Selection, Transition, WorkerAction, WorkerCommand, WorkerReply},
    ipc::{self, Lines, Outbox},
};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    io,
    os::unix::process::CommandExt,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

pub struct Renderer {
    child: Child,
    #[cfg(feature = "web")]
    web_profile: Option<std::path::PathBuf>,
    pub input: ChildStdin,
    pub output: ChildStdout,
    lines: Lines,
    outbox: Outbox,
    apply: Option<Vec<u8>>,
    playback: Option<Vec<u8>>,
    properties: Option<Vec<u8>>,
    snapshot: Option<Vec<u8>>,
    clocks: BTreeMap<String, Vec<u8>>,
    media: Option<Vec<u8>>,
    audio: Option<Vec<u8>>,
}

impl Renderer {
    #[cfg(test)]
    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }

    #[cfg(test)]
    pub fn echo() -> Self {
        let mut child = Command::new("cat")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        ipc::nonblocking(&input).unwrap();
        ipc::nonblocking(&output).unwrap();
        Self {
            child,
            #[cfg(feature = "web")]
            web_profile: None,
            input,
            output,
            lines: Lines::default(),
            outbox: Outbox::default(),
            apply: None,
            playback: None,
            properties: None,
            snapshot: None,
            clocks: BTreeMap::new(),
            media: None,
            audio: None,
        }
    }

    pub fn spawn(output: &str) -> Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .process_group(0)
            .arg("render-worker")
            .arg("--output")
            .arg(output)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(feature = "web")]
        let web_profile = {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("wallpaperd-web-{}-{nonce}", std::process::id()));
            // Created lazily by CEF; the daemon owns cleanup even after worker SIGKILL.
            command.env("WALLPAPERD_WEB_PROFILE", &path);
            path
        };
        let mut child = command
            .spawn()
            .with_context(|| format!("starting renderer for {output}"))?;
        let result = (|| -> Result<_> {
            let input = child.stdin.take().context("worker stdin missing")?;
            let output = child.stdout.take().context("worker stdout missing")?;
            ipc::nonblocking(&input)?;
            ipc::nonblocking(&output)?;
            Ok((input, output))
        })();
        let (input, output) = match result {
            Ok(pipes) => pipes,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            child,
            #[cfg(feature = "web")]
            web_profile: Some(web_profile),
            input,
            output,
            lines: Lines::default(),
            outbox: Outbox::default(),
            apply: None,
            playback: None,
            properties: None,
            snapshot: None,
            clocks: BTreeMap::new(),
            media: None,
            audio: None,
        })
    }

    pub fn send(
        &mut self,
        id: u64,
        selection: Option<Selection>,
        transition: Transition,
        playback: Playback,
        properties: crate::properties::Values,
    ) {
        let command = WorkerCommand {
            id,
            action: match selection {
                Some(selection) => WorkerAction::Apply {
                    selection,
                    transition,
                    playback,
                    properties,
                },
                None => WorkerAction::Release,
            },
        };
        self.apply = Some(ipc::encode(&command));
    }

    pub fn set_playback(&mut self, id: u64, playback: Playback) {
        self.playback = Some(ipc::encode(&WorkerCommand {
            id,
            action: WorkerAction::SetPlayback { playback },
        }));
    }

    pub fn wants_write(&self) -> bool {
        self.outbox.pending()
            || self.apply.is_some()
            || self.playback.is_some()
            || self.properties.is_some()
            || self.snapshot.is_some()
            || !self.clocks.is_empty()
            || self.media.is_some()
            || self.audio.is_some()
    }

    pub fn snapshot(&mut self, id: u64, selection_id: u64) {
        self.snapshot = Some(ipc::encode(&WorkerCommand {
            id,
            action: WorkerAction::Snapshot { selection_id },
        }));
    }

    pub fn set_clock(&mut self, asset_id: String, clock: wallpaper_media::clock::Timeline) {
        self.clocks.insert(
            asset_id.clone(),
            ipc::encode(&WorkerCommand {
                id: 0,
                action: WorkerAction::SetClock { asset_id, clock },
            }),
        );
    }

    pub fn set_media(&mut self, media: &crate::media::Snapshot) {
        self.media = Some(ipc::encode(&WorkerCommand {
            id: 0,
            action: WorkerAction::SetMedia {
                media: Box::new(media.clone()),
            },
        }));
    }
    pub fn set_audio(&mut self, audio: &we_scene::audio::AudioSnapshot) {
        self.audio = Some(ipc::encode(&WorkerCommand {
            id: 0,
            action: WorkerAction::SetAudio {
                audio: Box::new(audio.clone()),
            },
        }));
    }

    pub fn set_properties(
        &mut self,
        id: u64,
        selection_id: u64,
        asset_id: String,
        properties: crate::properties::Values,
    ) {
        self.properties = Some(ipc::encode(&WorkerCommand {
            id,
            action: WorkerAction::SetProperties {
                selection_id,
                asset_id,
                properties,
            },
        }));
    }

    pub fn flush(&mut self) -> io::Result<()> {
        loop {
            if !self.outbox.pending() {
                let Some(bytes) = self
                    .apply
                    .take()
                    .or_else(|| self.playback.take())
                    .or_else(|| self.properties.take())
                    .or_else(|| self.clocks.pop_first().map(|(_, bytes)| bytes))
                    .or_else(|| self.media.take())
                    .or_else(|| self.snapshot.take())
                    .or_else(|| self.audio.take())
                else {
                    return Ok(());
                };
                self.outbox.replace(bytes);
            }
            self.outbox.flush(&mut self.input)?;
            if self.outbox.pending() {
                return Ok(());
            }
        }
    }

    pub fn read(&mut self) -> Result<(Vec<WorkerReply>, bool)> {
        let (lines, eof) = self.lines.read(&mut self.output)?;
        let replies = lines
            .iter()
            .map(|line| serde_json::from_slice(line))
            .collect::<Result<_, _>>()?;
        Ok((replies, eof))
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // The worker owns its process group, including Chromium subprocesses.
        // Cleanup must also work after a browser-host crash skips CefShutdown.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        #[cfg(feature = "web")]
        if let Some(path) = &self.web_profile {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_audio_consumer_keeps_only_current_line_and_latest_snapshot() {
        use std::os::fd::AsRawFd;
        let mut renderer = Renderer::echo();
        // SAFETY: limit the test-owned pipe so backpressure does not depend on host pipe sizing.
        assert!(unsafe { libc::fcntl(renderer.input.as_raw_fd(), libc::F_SETPIPE_SZ, 4096) } >= 0);
        // SAFETY: signal only the child owned by this test. Drop reaps it even on failure.
        assert_eq!(
            unsafe { libc::kill(renderer.child.id() as i32, libc::SIGSTOP) },
            0
        );
        for sequence in 1..=500 {
            renderer.set_audio(&we_scene::audio::AudioSnapshot {
                sequence,
                ..Default::default()
            });
            renderer.flush().unwrap();
        }
        assert!(
            renderer.audio.is_some(),
            "test did not fill the worker pipe"
        );
        let pending: WorkerCommand =
            serde_json::from_slice(renderer.audio.as_ref().unwrap()).unwrap();
        assert!(
            matches!(pending.action, WorkerAction::SetAudio { audio } if audio.sequence == 500)
        );
        renderer.set_playback(
            7,
            Playback {
                paused: true,
                ..Playback::default()
            },
        );
        // SAFETY: resume that same live child so it can drain the bounded pipe.
        assert_eq!(
            unsafe { libc::kill(renderer.child.id() as i32, libc::SIGCONT) },
            0
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut sequences = Vec::new();
        let mut paused = false;
        while sequences.last() != Some(&500) {
            assert!(
                std::time::Instant::now() < deadline,
                "worker queue did not drain"
            );
            renderer.flush().unwrap();
            let (lines, _) = renderer.lines.read(&mut renderer.output).unwrap();
            for line in lines {
                let command: WorkerCommand = serde_json::from_slice(&line).unwrap();
                match command.action {
                    WorkerAction::SetAudio { audio } => sequences.push(audio.sequence),
                    WorkerAction::SetPlayback { playback } => paused = playback.paused,
                    _ => panic!("unexpected worker action"),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(paused, "latest audio delayed the playback control");
        assert!(
            sequences.len() < 10,
            "historical spectrum queue: {sequences:?}"
        );
        assert!(!renderer.wants_write());
    }

    #[test]
    fn selection_and_playback_queues_do_not_replace_each_other() {
        let mut renderer = Renderer::echo();
        let selection = Selection {
            asset_id: "local:/video.mkv".into(),
            fit: crate::domain::Fit::Cover,
        };
        renderer.send(
            1,
            Some(selection.clone()),
            Transition::Cut,
            Playback::default(),
            Default::default(),
        );
        renderer.send(
            2,
            Some(selection),
            Transition::Cut,
            Playback::default(),
            Default::default(),
        );
        renderer.set_playback(
            3,
            Playback {
                paused: true,
                ..Playback::default()
            },
        );
        renderer.set_properties(4, 2, "we:/project".into(), Default::default());
        renderer.set_properties(
            5,
            2,
            "we:/project".into(),
            serde_json::from_value(serde_json::json!({"style":"0","strength":0.5})).unwrap(),
        );
        renderer.snapshot(6, 2);
        let clock = wallpaper_media::clock::Timeline::new(100, true);
        renderer.set_clock("we:/project".into(), clock);
        renderer.set_clock("we:/project".into(), clock);
        renderer.set_media(&crate::media::Snapshot {
            revision: 1,
            ..Default::default()
        });
        renderer.set_media(&crate::media::Snapshot {
            revision: 2,
            ..Default::default()
        });
        renderer.flush().unwrap();
        let mut commands: Vec<WorkerCommand> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while commands.len() < 6 {
            assert!(std::time::Instant::now() < deadline);
            let mut fds = [ipc::interest(&renderer.output, false)];
            ipc::poll(&mut fds, 100).unwrap();
            let (lines, _) = renderer.lines.read(&mut renderer.output).unwrap();
            commands.extend(
                lines
                    .iter()
                    .map(|line| serde_json::from_slice::<WorkerCommand>(line).unwrap()),
            );
        }
        assert_eq!(
            commands.iter().map(|c| c.id).collect::<Vec<_>>(),
            [2, 3, 5, 0, 0, 6]
        );
        assert!(matches!(commands[0].action, WorkerAction::Apply { .. }));
        assert!(matches!(
            commands[1].action,
            WorkerAction::SetPlayback { .. }
        ));
        assert!(
            matches!(&commands[2].action, WorkerAction::SetProperties { properties, selection_id: 2, .. } if properties["style"] == "0" && properties["strength"] == 0.5)
        );
        assert!(matches!(
            commands[5].action,
            WorkerAction::Snapshot { selection_id: 2 }
        ));
        assert!(
            matches!(&commands[3].action, WorkerAction::SetClock { asset_id, clock: actual }
            if asset_id == "we:/project" && *actual == clock)
        );
        assert!(
            matches!(&commands[4].action, WorkerAction::SetMedia { media } if media.revision == 2)
        );
    }
}
