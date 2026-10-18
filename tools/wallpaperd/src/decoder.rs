//! One running job, one replaceable pending job, and one bounded result.
use parking_lot::{Condvar, Mutex};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
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

pub enum Frame {
    Shm(ShmFrame),
    Image(image::RgbaImage),
}

pub struct ShmFrame {
    pub size: Size,
    pub file: File,
}

impl ShmFrame {
    fn render(
        image: image::RgbaImage,
        size: Size,
        fit: crate::domain::Fit,
    ) -> anyhow::Result<Self> {
        // SAFETY: memfd_create returns a new descriptor owned by this frame.
        let fd = unsafe {
            libc::memfd_create(
                c"wallpaper-frame".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(size.bytes() as u64)?;
        // SAFETY: the private file has the required length and has no other users.
        let mut canvas = unsafe { memmap2::MmapMut::map_mut(&file)? };
        pixels::render(image, size, fit, &mut canvas)?;
        drop(canvas);
        // wl_shm compositors may map client buffers with PROT_WRITE even though
        // they only read the pixels. Seal the extent, and keep pixels immutable
        // by ownership; F_SEAL_WRITE would reject those compositor mappings.
        let seals = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
        // SAFETY: no writable mapping survives in the producer.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self { size, file })
    }

    pub fn image(&self) -> anyhow::Result<image::RgbaImage> {
        // SAFETY: the sealed extent cannot be truncated, and neither the producer
        // nor the compositor writes these published pixels. The mapping is transient.
        let pixels = unsafe { memmap2::Mmap::map(&self.file)? };
        let mut image = image::RgbaImage::new(self.size.width, self.size.height);
        for (source, target) in pixels.as_chunks::<4>().0.iter().zip(image.pixels_mut()) {
            let rgb = u32::from_ne_bytes(*source);
            target.0 = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255];
        }
        Ok(image)
    }
}

struct Job {
    generation: u64,
    source: Source,
    size: Size,
    texture: bool,
}
enum Source {
    Asset(Selection),
    Pixels(image::RgbaImage),
}
struct Mailbox {
    work: Mutex<Work>,
    ready: Condvar,
    latest: AtomicU64,
}

#[derive(Default)]
struct Work {
    job: Option<Job>,
    reclaim: bool,
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
    pub fn new() -> io::Result<Self> {
        configure_scratch();
        let (wake, mut writer) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        let mailbox = Arc::new(Mailbox {
            work: Mutex::new(Work::default()),
            ready: Condvar::new(),
            latest: AtomicU64::new(0),
        });
        let work = mailbox.clone();
        let (tx, results) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("wallpaper-decode".into())
            .spawn(move || {
                loop {
                    let (job, reclaim) = {
                        let mut pending = work.work.lock();
                        while pending.job.is_none() && !pending.reclaim {
                            work.ready.wait(&mut pending);
                        }
                        let job = pending.job.take();
                        let reclaim = std::mem::take(&mut pending.reclaim);
                        (job, reclaim)
                    };
                    let Some(job) = job else {
                        release_scratch();
                        continue;
                    };
                    let is_current = || work.latest.load(Ordering::Acquire) == job.generation;
                    if !is_current() {
                        if reclaim {
                            release_scratch();
                        }
                        continue;
                    }
                    let (image, fit) = match job.source {
                        Source::Asset(selection) => (pixels::decode(&selection), selection.fit),
                        Source::Pixels(image) => (Ok(image), crate::domain::Fit::Stretch),
                    };
                    if !is_current() {
                        drop(image);
                        release_scratch();
                        continue;
                    }
                    let frame = image.and_then(|image| {
                        if job.texture {
                            let mut target = image::RgbaImage::new(job.size.width, job.size.height);
                            pixels::render_rgba(image, job.size, fit, target.as_mut())?;
                            return Ok(Frame::Image(target));
                        }
                        ShmFrame::render(image, job.size, fit).map(Frame::Shm)
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
        self.mailbox.work.lock().job.take();
    }

    pub fn submit(&self, generation: u64, selection: Selection, size: Size, texture: bool) {
        self.mailbox.latest.store(generation, Ordering::Release);
        self.mailbox.work.lock().job = Some(Job {
            generation,
            source: Source::Asset(selection),
            size,
            texture,
        });
        self.mailbox.ready.notify_one();
    }

    pub fn submit_image(&self, generation: u64, image: image::RgbaImage) {
        self.mailbox.latest.store(generation, Ordering::Release);
        let size = Size {
            width: image.width(),
            height: image.height(),
        };
        self.mailbox.work.lock().job = Some(Job {
            generation,
            source: Source::Pixels(image),
            size,
            texture: false,
        });
        self.mailbox.ready.notify_one();
    }

    pub fn reclaim(&self) {
        let mut work = self.mailbox.work.lock();
        work.reclaim = true;
        drop(work);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::FileExt;

    #[test]
    fn static_frame_keeps_its_extent_and_exact_displayed_pixels() {
        let image =
            image::RgbaImage::from_fn(13, 7, |x, y| image::Rgba([x as u8, y as u8, 193, 255]));
        let frame = ShmFrame::render(
            image.clone(),
            Size::new(13, 7).unwrap(),
            crate::domain::Fit::Stretch,
        )
        .unwrap();
        assert_eq!(frame.image().unwrap(), image);
        assert!(
            frame
                .file
                .write_at(&[0; 4], frame.size.bytes() as u64)
                .is_err()
        );
        assert!(frame.file.set_len(0).is_err());
        let second = ShmFrame {
            size: frame.size,
            file: frame.file.try_clone().unwrap(),
        };
        drop(frame);
        assert_eq!(second.image().unwrap(), image);
    }
}
