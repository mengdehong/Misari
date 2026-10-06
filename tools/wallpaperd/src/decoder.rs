//! One running job, one replaceable pending job, and one bounded result.
use parking_lot::{Condvar, Mutex};
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
};

use crate::{
    domain::Selection,
    pixels::{self, Size},
};

use smithay_client_toolkit::{
    registry::SimpleGlobal,
    shm::slot::{Buffer, SlotPool},
};
use wayland_client::protocol::wl_shm;

pub enum Frame {
    Shm { size: Size, buffer: Buffer },
    Image(image::RgbaImage),
}

struct Job {
    generation: u64,
    selection: Selection,
    size: Size,
    texture: bool,
}
struct Mailbox {
    job: Mutex<Option<Job>>,
    ready: Condvar,
    latest: AtomicU64,
}

pub struct Decoded {
    pub generation: u64,
    pub frame: anyhow::Result<Frame>,
}

pub struct Decoder {
    mailbox: Arc<Mailbox>,
    pub wake: UnixStream,
    results: Receiver<Decoded>,
}

impl Decoder {
    pub fn new(shm: wl_shm::WlShm) -> io::Result<Self> {
        configure_scratch();
        let (wake, mut writer) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        let mailbox = Arc::new(Mailbox {
            job: Mutex::new(None),
            ready: Condvar::new(),
            latest: AtomicU64::new(0),
        });
        let work = mailbox.clone();
        let (tx, results) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("wallpaper-decode".into())
            .spawn(move || {
                let shm = SimpleGlobal::<_, 1>::from_bound(shm);
                loop {
                    let job = {
                        let mut slot = work.job.lock();
                        while slot.is_none() {
                            work.ready.wait(&mut slot);
                        }
                        slot.take().unwrap()
                    };
                    let is_current = || work.latest.load(Ordering::Acquire) == job.generation;
                    if !is_current() {
                        continue;
                    }
                    let image = pixels::decode(&job.selection);
                    if !is_current() {
                        drop(image);
                        release_scratch();
                        continue;
                    }
                    let frame = image.and_then(|image| {
                        if job.texture {
                            let mut target = image::RgbaImage::new(job.size.width, job.size.height);
                            pixels::render_rgba(
                                image,
                                job.size,
                                job.selection.fit,
                                target.as_mut(),
                            )?;
                            return Ok(Frame::Image(target));
                        }
                        let mut pool = SlotPool::new(job.size.bytes(), &shm)?;
                        let (buffer, canvas) = pool.create_buffer(
                            job.size.width as i32,
                            job.size.height as i32,
                            (job.size.width * 4) as i32,
                            wl_shm::Format::Xrgb8888,
                        )?;
                        pixels::render(image, job.size, job.selection.fit, canvas)?;
                        // The compositor keeps the storage behind the buffer. The worker
                        // does not need a CPU mapping after rendering, even before attach.
                        Ok(Frame::Shm {
                            size: job.size,
                            buffer,
                        })
                    });
                    release_scratch();
                    if !is_current() {
                        continue;
                    }
                    if tx
                        .send(Decoded {
                            generation: job.generation,
                            frame,
                        })
                        .is_err()
                    {
                        break;
                    }
                    match writer.write(&[1]) {
                        Ok(_) => {}
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            mailbox,
            wake,
            results,
        })
    }

    pub fn cancel(&self, generation: u64) {
        self.mailbox.latest.store(generation, Ordering::Release);
        self.mailbox.job.lock().take();
    }

    pub fn submit(&self, generation: u64, selection: Selection, size: Size, texture: bool) {
        self.mailbox.latest.store(generation, Ordering::Release);
        *self.mailbox.job.lock() = Some(Job {
            generation,
            selection,
            size,
            texture,
        });
        self.mailbox.ready.notify_one();
    }

    pub fn receive(&mut self) -> io::Result<Vec<Decoded>> {
        let mut bytes = [0; 64];
        loop {
            match self.wake.read(&mut bytes) {
                Ok(0) => return Err(io::ErrorKind::BrokenPipe.into()),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(self.results.try_iter().collect())
    }
}

pub(crate) fn release_scratch() {
    // glibc otherwise retains freed decoder/filter arenas for the lifetime of a static
    // worker. Trim only between jobs, when there is no further pixel work to amortize.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: glibc trims only unused pages, preserving all live allocations.
    unsafe {
        libc::malloc_trim(0);
    }
}

fn configure_scratch() {
    // glibc's adaptive mmap threshold can move multi-megabyte image allocations
    // into thread arenas after a few loads. Keep those allocations independently
    // releasable. This process-wide policy is confined to the isolated worker.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: mallopt changes allocator policy; it does not invalidate live allocations.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 128 * 1024);
    }
}
