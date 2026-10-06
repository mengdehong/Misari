use libpulse_binding::{
    def::BufferAttr,
    sample::{Format, Spec},
    stream::Direction,
};
use libpulse_simple_binding::Simple;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::{self, JoinHandle},
};

struct Packet {
    samples: Vec<f32>,
    spec: Spec,
}
struct Queue {
    packets: VecDeque<Packet>,
    spec: Spec,
    stopped: bool,
}
struct Output {
    queue: Mutex<Queue>,
    wake: Condvar,
    mute: AtomicBool,
    volume: AtomicU32,
}
pub struct Audio {
    output: Arc<Output>,
    thread: Option<JoinHandle<()>>,
}

impl Audio {
    pub fn new() -> Self {
        let output = Arc::new(Output {
            queue: Mutex::new(Queue {
                packets: VecDeque::new(),
                spec: Spec {
                    format: Format::FLOAT32NE,
                    channels: 2,
                    rate: 48000,
                },
                stopped: false,
            }),
            wake: Condvar::new(),
            mute: AtomicBool::new(true),
            volume: AtomicU32::new(1f32.to_bits()),
        });
        let thread = {
            let output = output.clone();
            thread::spawn(move || output.run())
        };
        Self {
            output,
            thread: Some(thread),
        }
    }
    pub fn playback(&self, mute: bool, volume: f32) {
        self.output.mute.store(mute, Ordering::Relaxed);
        self.output
            .volume
            .store(volume.to_bits(), Ordering::Relaxed);
    }
    pub fn start(&self, rate: i32, channels: i32) {
        let mut queue = self.output.queue.lock().unwrap();
        queue.spec.rate = rate.max(0) as u32;
        queue.spec.channels = u8::try_from(channels).unwrap_or(0);
        queue.packets.clear();
    }
    // CEF owns the planar buffers and keeps them valid for this callback only.
    pub unsafe fn packet(&self, data: *mut *const f32, frames: i32) {
        if data.is_null()
            || !(1..=16384).contains(&frames)
            || self.output.mute.load(Ordering::Relaxed)
        {
            return;
        }
        let mut queue = self.output.queue.lock().unwrap();
        let spec = queue.spec;
        if !(1..=8).contains(&spec.channels) || !spec.is_valid() {
            return;
        }
        let channels = unsafe { std::slice::from_raw_parts(data, spec.channels as usize) };
        if channels.iter().any(|channel| channel.is_null()) {
            return;
        }
        let mut samples = Vec::with_capacity(frames as usize * channels.len());
        for frame in 0..frames as usize {
            for &channel in channels {
                samples.push(unsafe { *channel.add(frame) });
            }
        }
        // ponytail: bounded PCM queue; use a Pulse mainloop if underruns matter.
        if queue.packets.len() == 8 {
            queue.packets.pop_front();
        }
        queue.packets.push_back(Packet { samples, spec });
        self.output.wake.notify_one();
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.output.queue.lock().unwrap().stopped = true;
        self.output.wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Output {
    fn run(&self) {
        let mut stream: Option<Simple> = None;
        let mut spec = None;
        loop {
            let mut packet = {
                let mut queue = self.queue.lock().unwrap();
                while !queue.stopped && queue.packets.is_empty() {
                    queue = self.wake.wait(queue).unwrap();
                }
                if queue.stopped {
                    break;
                }
                queue.packets.pop_front().unwrap()
            };
            if self.mute.load(Ordering::Relaxed) {
                if let Some(stream) = &stream {
                    let _ = stream.flush();
                }
                continue;
            }
            if stream.is_none() || spec != Some(packet.spec) {
                let attributes = BufferAttr {
                    maxlength: u32::MAX,
                    tlength: packet.spec.rate * u32::from(packet.spec.channels) * 4 / 20,
                    prebuf: u32::MAX,
                    minreq: u32::MAX,
                    fragsize: u32::MAX,
                };
                stream = match Simple::new(
                    None,
                    "wallpaperd",
                    Direction::Playback,
                    None,
                    "Web wallpaper",
                    &packet.spec,
                    None,
                    Some(&attributes),
                ) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        eprintln!("we-web audio: {error}");
                        None
                    }
                };
                spec = Some(packet.spec);
            }
            let gain = f32::from_bits(self.volume.load(Ordering::Relaxed));
            for sample in &mut packet.samples {
                *sample = if sample.is_finite() {
                    *sample * gain
                } else {
                    0.0
                };
            }
            // Native-endian f32 samples match Pulse's FLOAT32NE sample format.
            let bytes = unsafe {
                std::slice::from_raw_parts(packet.samples.as_ptr().cast(), packet.samples.len() * 4)
            };
            if stream
                .as_ref()
                .is_some_and(|stream| stream.write(bytes).is_err())
            {
                stream = None;
            }
        }
    }
}
