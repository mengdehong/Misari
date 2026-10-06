//! Optional direct CEF DMA-BUF presentation, with a bounded Wayland buffer cache.
use super::State;
use crate::web::frames::{ABGR8888, ARGB8888, Frame};
use anyhow::{Context, Result};
use std::{
    collections::HashSet,
    os::fd::{AsFd, AsRawFd, OwnedFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use wayland_client::{Connection, Dispatch, QueueHandle, globals::GlobalList, protocol::wl_buffer};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_v1::{self, ZwpLinuxDmabufV1},
};

const INVALID: u64 = 0x00ff_ffff_ffff_ffff;
#[derive(Clone, Default)]
struct Formats(Arc<Mutex<HashSet<(u32, u64)>>>);

#[derive(PartialEq, Eq)]
struct Key {
    size: [u32; 2],
    format: u32,
    modifier: u64,
    planes: Vec<(u64, u64, u64, u32)>,
}
struct Entry {
    key: Key,
    buffer: wl_buffer::WlBuffer,
    released: Arc<AtomicBool>,
    // Keep DMA-BUF identities alive for the lifetime of the cache entry.
    _fds: Vec<OwnedFd>,
}
impl Drop for Entry {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

pub(super) struct Dmabuf {
    proxy: ZwpLinuxDmabufV1,
    formats: Formats,
    buffers: Vec<Entry>,
}
impl Dmabuf {
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Option<Self> {
        if std::env::var("WALLPAPERD_WEB_DIRECT").as_deref() != Ok("1") {
            return None;
        }
        let formats = Formats::default();
        // Version 3 advertises format/modifier pairs without a feedback mapping.
        let proxy = globals.bind(qh, 3..=3, formats.clone()).ok()?;
        Some(Self {
            proxy,
            formats,
            buffers: Vec::new(),
        })
    }
    fn format(&self, frame: &Frame) -> Option<(u32, u64)> {
        let format = match frame.info.format {
            ARGB8888 => 0x34325258, // XRGB8888: all wallpaper surfaces are opaque.
            ABGR8888 => 0x34324258, // XBGR8888.
            _ => return None,
        };
        let formats = self.formats.0.lock().unwrap();
        let modifier = if frame.info.modifier == INVALID && formats.contains(&(format, 0)) {
            0
        } else {
            frame.info.modifier
        };
        formats
            .contains(&(format, modifier))
            .then_some((format, modifier))
    }
    pub fn supports(&self, frame: &Frame, viewport: bool) -> bool {
        let [x, y, w, h] = frame.info.visible;
        let [width, height] = frame.info.size;
        self.format(frame).is_some()
            && w > 0
            && h > 0
            && width <= i32::MAX as u32
            && height <= i32::MAX as u32
            && x.checked_add(w).is_some_and(|end| end <= width)
            && y.checked_add(h).is_some_and(|end| end <= height)
            && (viewport || frame.info.visible == [0, 0, width, height])
            && !frame.fds.is_empty()
            && frame.fds.len() <= 4
            && frame.fds.len() == frame.info.planes.len()
            && frame
                .info
                .planes
                .iter()
                .all(|plane| plane.stride > 0 && u32::try_from(plane.offset).is_ok())
    }
    pub fn buffer(
        &mut self,
        frame: &Frame,
        qh: &QueueHandle<State>,
    ) -> Result<wl_buffer::WlBuffer> {
        let (format, modifier) = self
            .format(frame)
            .context("unsupported Wayland DMA-BUF format")?;
        let planes = frame
            .fds
            .iter()
            .zip(&frame.info.planes)
            .map(|(fd, plane)| {
                let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                // SAFETY: fstat initializes stat on success; the descriptor is owned by frame.
                anyhow::ensure!(
                    unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } == 0,
                    "identifying DMA-BUF: {}",
                    std::io::Error::last_os_error()
                );
                let stat = unsafe { stat.assume_init() };
                Ok((stat.st_dev, stat.st_ino, plane.offset, plane.stride))
            })
            .collect::<Result<Vec<_>>>()?;
        let key = Key {
            size: frame.info.size,
            format,
            modifier,
            planes,
        };
        if let Some(entry) = self
            .buffers
            .iter()
            .find(|entry| entry.key == key && entry.released.load(Ordering::Acquire))
        {
            entry.released.store(false, Ordering::Release);
            return Ok(entry.buffer.clone());
        }
        // ponytail: eight entries cover CEF's small pool and in-flight commits.
        // wl_buffer destruction is allowed while the compositor retains its contents.
        if self.buffers.len() >= 8 {
            let index = self
                .buffers
                .iter()
                .position(|entry| entry.released.load(Ordering::Acquire))
                .unwrap_or(0);
            self.buffers.swap_remove(index);
        }
        let fds = frame
            .fds
            .iter()
            .map(|fd| fd.try_clone())
            .collect::<std::io::Result<Vec<_>>>()?;
        let params = self.proxy.create_params(qh, ());
        for (index, (fd, plane)) in fds.iter().zip(&frame.info.planes).enumerate() {
            params.add(
                fd.as_fd(),
                index as u32,
                plane.offset as u32,
                plane.stride,
                (modifier >> 32) as u32,
                modifier as u32,
            );
        }
        let released = Arc::new(AtomicBool::new(false));
        let buffer = params.create_immed(
            frame.info.size[0] as i32,
            frame.info.size[1] as i32,
            format,
            zwp_linux_buffer_params_v1::Flags::empty(),
            qh,
            released.clone(),
        );
        params.destroy();
        if self.buffers.is_empty() {
            eprintln!(
                "wallpaperd web: direct Wayland DMA-BUF presentation ({format:#x}, modifier {modifier:#x})"
            );
        }
        self.buffers.push(Entry {
            key,
            buffer: buffer.clone(),
            released,
            _fds: fds,
        });
        Ok(buffer)
    }
    pub fn clear(&mut self) {
        self.buffers.clear();
    }
}
impl Drop for Dmabuf {
    fn drop(&mut self) {
        self.clear();
        self.proxy.destroy();
    }
}
impl Dispatch<ZwpLinuxDmabufV1, Formats> for State {
    fn event(
        _: &mut Self,
        _: &ZwpLinuxDmabufV1,
        event: zwp_linux_dmabuf_v1::Event,
        formats: &Formats,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let pair = match event {
            zwp_linux_dmabuf_v1::Event::Format { format } => (format, INVALID),
            zwp_linux_dmabuf_v1::Event::Modifier {
                format,
                modifier_hi,
                modifier_lo,
            } => (format, ((modifier_hi as u64) << 32) | modifier_lo as u64),
            _ => return,
        };
        formats.0.lock().unwrap().insert(pair);
    }
}
impl Dispatch<wl_buffer::WlBuffer, Arc<AtomicBool>> for State {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        released: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            released.store(true, Ordering::Release);
        }
    }
}
wayland_client::delegate_noop!(State: ignore ZwpLinuxBufferParamsV1);
