//! One daemon monitor stream and stereo FFT; consumers receive only its latest result.
mod spectrum;
use spectrum::Response;

use anyhow::{Context as _, Result, ensure};
use libpulse_binding::{
    callbacks::ListResult,
    context::{Context, FlagSet, State, subscribe::InterestMaskSet},
    def::BufferAttr,
    mainloop::standard::{IterateResult, Mainloop},
    sample::{Format, Spec},
    stream::{self, PeekResult, Stream},
};
use parking_lot::{Condvar, Mutex};
use std::{
    cell::{Cell, RefCell},
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    rc::Rc,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use we_scene::audio::AudioSnapshot;

#[derive(Default)]
struct Demand {
    active: bool,
    stop: bool,
}

pub struct Capture {
    pub wake: UnixStream,
    latest: Arc<Mutex<AudioSnapshot>>,
    control: Arc<(Mutex<Demand>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}
impl Capture {
    pub fn start() -> io::Result<Self> {
        Self::start_with_server(None)
    }
    pub(crate) fn start_with_server(server: Option<String>) -> io::Result<Self> {
        Self::start_response(server, Response::Scene)
    }
    #[cfg(test)]
    pub(crate) fn start_pcm_with_server(server: Option<String>) -> io::Result<Self> {
        Self::start_response(server, Response::Pcm)
    }
    fn start_response(server: Option<String>, response: Response) -> io::Result<Self> {
        let (wake, mut writer) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        let latest = Arc::new(Mutex::new(AudioSnapshot::default()));
        let control = Arc::new((Mutex::new(Demand::default()), Condvar::new()));
        let shared = latest.clone();
        let demand = control.clone();
        let thread = thread::Builder::new()
            .name("audio-spectrum".into())
            .spawn(move || {
                let mut sequence = 0u64;
                let mut publish = |mut snapshot: AudioSnapshot| {
                    sequence = sequence.wrapping_add(1);
                    snapshot.sequence = sequence;
                    *shared.lock() = snapshot;
                    let _ = writer.write(&[1]);
                };
                loop {
                    {
                        let mut state = demand.0.lock();
                        while !state.active && !state.stop {
                            demand.1.wait(&mut state);
                        }
                        if state.stop {
                            break;
                        }
                    }
                    if let Err(error) = record(server.as_deref(), &demand, response, &mut publish)
                        && active(&demand)
                    {
                        eprintln!("wallpaperd: audio spectrum: {error:#}");
                    }
                    publish(AudioSnapshot::default());
                    let mut state = demand.0.lock();
                    if state.active && !state.stop {
                        demand.1.wait_for(&mut state, Duration::from_secs(1));
                    }
                    if state.stop {
                        break;
                    }
                }
            })?;
        Ok(Self {
            wake,
            latest,
            control,
            thread: Some(thread),
        })
    }
    pub fn set_active(&self, active: bool) {
        let mut state = self.control.0.lock();
        if state.active != active {
            state.active = active;
            self.control.1.notify_one();
        }
    }
    pub fn receive(&mut self) -> io::Result<AudioSnapshot> {
        let mut bytes = [0; 256];
        loop {
            match self.wake.read(&mut bytes) {
                Ok(0) => return Err(io::ErrorKind::BrokenPipe.into()),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(self.latest.lock().clone())
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        {
            let mut state = self.control.0.lock();
            state.stop = true;
            self.control.1.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn active(control: &Arc<(Mutex<Demand>, Condvar)>) -> bool {
    let state = control.0.lock();
    state.active && !state.stop
}
fn pump(
    mainloop: &mut Mainloop,
    context: &Context,
    control: &Arc<(Mutex<Demand>, Condvar)>,
) -> Result<()> {
    ensure!(active(control), "audio capture cancelled");
    ensure!(
        matches!(mainloop.iterate(false), IterateResult::Success(_)),
        "Pulse mainloop stopped"
    );
    ensure!(
        !matches!(context.get_state(), State::Failed | State::Terminated),
        "Pulse audio server disconnected"
    );
    let mut state = control.0.lock();
    if state.active && !state.stop {
        control.1.wait_for(&mut state, Duration::from_millis(5));
    }
    Ok(())
}
fn monitor(
    mainloop: &mut Mainloop,
    context: &Context,
    control: &Arc<(Mutex<Demand>, Condvar)>,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let sink = Rc::new(RefCell::new(None));
    let result = sink.clone();
    let _server = context.introspect().get_server_info(move |info| {
        *result.borrow_mut() = Some(info.default_sink_name.as_ref().map(|v| v.to_string()));
    });
    while sink.borrow().is_none() {
        ensure!(Instant::now() < deadline, "Pulse server query timed out");
        pump(mainloop, context, control)?;
    }
    let sink = sink
        .borrow_mut()
        .take()
        .flatten()
        .context("audio server has no default output")?;
    let source = Rc::new(RefCell::new(None));
    let result = source.clone();
    let _query = context
        .introspect()
        .get_sink_info_by_name(&sink, move |item| match item {
            ListResult::Item(info) => {
                *result.borrow_mut() =
                    Some(info.monitor_source_name.as_ref().map(|v| v.to_string()))
            }
            ListResult::End | ListResult::Error => {
                if result.borrow().is_none() {
                    *result.borrow_mut() = Some(None);
                }
            }
        });
    while source.borrow().is_none() {
        ensure!(Instant::now() < deadline, "Pulse monitor query timed out");
        pump(mainloop, context, control)?;
    }
    source
        .borrow_mut()
        .take()
        .flatten()
        .context("default output has no monitor source")
}
fn record(
    server: Option<&str>,
    control: &Arc<(Mutex<Demand>, Condvar)>,
    response: Response,
    publish: &mut dyn FnMut(AudioSnapshot),
) -> Result<()> {
    let mut mainloop = Mainloop::new().context("creating audio mainloop")?;
    let mut context =
        Context::new(&mainloop, "wallpaperd-spectrum").context("creating audio context")?;
    context.connect(server, FlagSet::NOAUTOSPAWN, None)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while context.get_state() != State::Ready {
        ensure!(
            Instant::now() < deadline,
            "audio server connection timed out"
        );
        pump(&mut mainloop, &context, control)?;
    }
    let dirty = Rc::new(Cell::new(false));
    let changed = dirty.clone();
    context.set_subscribe_callback(Some(Box::new(move |_, _, _| changed.set(true))));
    let subscribed = Rc::new(Cell::new(None));
    let reply = subscribed.clone();
    let _subscription = context
        .subscribe(InterestMaskSet::SERVER | InterestMaskSet::SINK, move |ok| {
            reply.set(Some(ok))
        });
    while subscribed.get().is_none() {
        ensure!(Instant::now() < deadline, "audio subscription timed out");
        pump(&mut mainloop, &context, control)?;
    }
    ensure!(
        subscribed.get() == Some(true),
        "audio monitor subscription failed"
    );
    let mut device = monitor(&mut mainloop, &context, control)?;
    let mut analyzer = spectrum::Analyzer::new(response);
    let spec = Spec {
        format: Format::FLOAT32NE,
        channels: 2,
        rate: spectrum::RATE,
    };
    let mut stream = open_stream(&mut context, &spec, &device)?;
    let mut stream_deadline = Instant::now() + Duration::from_secs(3);
    let mut announced = false;
    let mut last_data = Instant::now();
    loop {
        pump(&mut mainloop, &context, control)?;
        if dirty.replace(false) {
            let next = monitor(&mut mainloop, &context, control)?;
            if next != device {
                let _ = stream.disconnect();
                device = next;
                analyzer = spectrum::Analyzer::new(response);
                stream = open_stream(&mut context, &spec, &device)?;
                stream_deadline = Instant::now() + Duration::from_secs(3);
                announced = false;
                publish(AudioSnapshot {
                    capturing: true,
                    device: Some(device.clone()),
                    ..Default::default()
                });
                last_data = Instant::now();
            }
        }
        ensure!(
            !matches!(
                stream.get_state(),
                stream::State::Failed | stream::State::Terminated
            ),
            "audio monitor stream disconnected"
        );
        if stream.get_state() != stream::State::Ready {
            ensure!(
                Instant::now() < stream_deadline,
                "audio monitor startup timed out"
            );
            continue;
        }
        if !announced {
            publish(AudioSnapshot {
                available: true,
                capturing: true,
                device: Some(device.clone()),
                ..Default::default()
            });
            announced = true;
            last_data = Instant::now();
        }
        // Bound each turn, including server holes, so cancellation/device changes stay responsive.
        for _ in 0..32 {
            let mut emit = |mut snapshot: AudioSnapshot| {
                snapshot.device = Some(device.clone());
                publish(snapshot);
            };
            match stream.peek()? {
                PeekResult::Empty => break,
                PeekResult::Data(bytes) => {
                    ensure!(bytes.len().is_multiple_of(8), "unaligned stereo PCM");
                    analyzer.push(
                        bytes.as_chunks::<8>().0.iter().map(|v| {
                            [
                                f32::from_ne_bytes(v[..4].try_into().unwrap()),
                                f32::from_ne_bytes(v[4..].try_into().unwrap()),
                            ]
                        }),
                        &mut emit,
                    );
                }
                PeekResult::Hole(bytes) => analyzer.push(
                    std::iter::repeat_n([0.0; 2], (bytes / 8).min(spectrum::RATE as usize)),
                    &mut emit,
                ),
            }
            stream.discard()?;
            last_data = Instant::now();
        }
        // Some servers suspend idle monitors instead of delivering silence.
        if last_data.elapsed() >= Duration::from_millis(100) {
            analyzer.push(
                std::iter::repeat_n([0.0; 2], spectrum::RATE as usize / 10),
                |mut snapshot| {
                    snapshot.device = Some(device.clone());
                    publish(snapshot);
                },
            );
            last_data = Instant::now();
        }
    }
}
fn open_stream(context: &mut Context, spec: &Spec, device: &str) -> Result<Stream> {
    let mut stream = Stream::new(context, "wallpaperd system output monitor", spec, None)
        .context("creating stereo monitor stream")?;
    stream.connect_record(
        Some(device),
        Some(&BufferAttr {
            maxlength: 256 * 1024,
            fragsize: 8192,
            ..Default::default()
        }),
        stream::FlagSet::ADJUST_LATENCY,
    )?;
    Ok(stream)
}

#[cfg(test)]
#[path = "../../tests/support/audio_server.rs"]
mod test_server;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn no_consumers_never_connect_and_shutdown_wakes_idle_thread() {
        let mut capture =
            Capture::start_with_server(Some("unix:/missing-wallpaperd-test".into())).unwrap();
        capture.set_active(false);
        assert_eq!(capture.receive().unwrap(), AudioSnapshot::default());
        let start = Instant::now();
        drop(capture);
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    pub(crate) use super::test_server::Server;

    fn wait_audio(
        capture: &mut Capture,
        predicate: impl Fn(&AudioSnapshot) -> bool,
    ) -> AudioSnapshot {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let snapshot = capture.receive().unwrap();
            if predicate(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "audio state did not converge: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn peak(values: &[f32]) -> (usize, f32) {
        values
            .iter()
            .copied()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap()
    }

    #[test]
    #[ignore = "starts an isolated PipeWire/Pulse server; requires pipewire, wireplumber and paplay"]
    fn private_monitor_stereo_latest_pause_switch_and_reconnect() {
        let mut server = Server::new();
        let pcm = server.root.path().join("stereo.f32");
        let mut file = File::create(&pcm).unwrap();
        for n in 0..spectrum::RATE * 15 {
            for (frequency, amplitude) in [(750.0, 0.8), (6000.0, 0.4)] {
                let value = amplitude
                    * (std::f32::consts::TAU * frequency * n as f32 / spectrum::RATE as f32).sin();
                file.write_all(&value.to_le_bytes()).unwrap();
            }
        }
        drop(file);
        let mut capture = Capture::start_pcm_with_server(Some(server.address())).unwrap();
        assert!(server.source_outputs().as_array().unwrap().is_empty());
        capture.set_active(true);
        let _playback = server.play(&pcm);
        let snapshot = wait_audio(&mut capture, |s| {
            s.available && peak(&s.bands[2].left).1 > 0.74 && peak(&s.bands[2].right).1 > 0.36
        });
        let left = peak(&snapshot.bands[2].left);
        let right = peak(&snapshot.bands[2].right);
        assert!(left.0 < right.0, "channels swapped: {left:?}, {right:?}");
        assert!(
            (left.1 - 0.8).abs() < 0.08 && (right.1 - 0.4).abs() < 0.08,
            "PCM amplitude mismatch: {left:?}, {right:?}"
        );
        for bands in &snapshot.bands {
            for i in 0..bands.left.len() {
                assert!((bands.average[i] - (bands.left[i] + bands.right[i]) * 0.5).abs() < 1e-6);
            }
        }
        assert_eq!(server.source_outputs().as_array().unwrap().len(), 1);
        // Deliberately leave the wake pipe unread. Results still overwrite one snapshot.
        thread::sleep(Duration::from_millis(800));
        let latest = capture.receive().unwrap();
        assert!(
            latest.sequence > snapshot.sequence + 20,
            "slow consumer retained history"
        );
        assert!(latest.valid());
        server.pactl(&[
            "load-module",
            "module-null-sink",
            "sink_name=wallpaperd_test_b",
            "channels=2",
        ]);
        server.pactl(&["set-default-sink", "wallpaperd_test_b"]);
        let silent = wait_audio(&mut capture, |s| {
            s.available
                && s.device.as_deref() == Some("wallpaperd_test_b.monitor")
                && peak(&s.bands[2].left).1 < 0.01
        });
        assert!(silent.available && silent.capturing);
        assert_eq!(server.source_outputs().as_array().unwrap().len(), 1);
        capture.set_active(false);
        wait_audio(&mut capture, |s| !s.capturing);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !server.source_outputs().as_array().unwrap().is_empty() {
            assert!(
                Instant::now() < deadline,
                "paused capture retained a record stream"
            );
            thread::sleep(Duration::from_millis(20));
        }
        capture.set_active(true);
        wait_audio(&mut capture, |s| s.available);
        server.disconnect();
        wait_audio(&mut capture, |s| !s.available);
        server.restart();
        wait_audio(&mut capture, |s| {
            s.available && s.device.as_deref() == Some("wallpaperd_test_a.monitor")
        });
        let start = Instant::now();
        drop(capture);
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "active consumer shutdown blocked"
        );
    }
}
