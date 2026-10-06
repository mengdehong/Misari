//! Single-frame decoding to memory, independent of desktop rendering and image encoding.
use std::{
    ffi::{CStr, CString},
    path::Path,
    ptr,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};

use crate::api::{Api, EndFile, Node, NodeValue};

/// A display-oriented frame with tightly packed, top-to-bottom RGBA8 pixels.
/// Alpha is always 255; pixel conversion follows libmpv's video screenshot behavior.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

struct Capture {
    api: Arc<Api>,
    handle: *mut std::ffi::c_void,
}

/// Open a video/GIF and decode the frame at an absolute media position.
/// Returns the native display dimensions without thumbnail scaling or encoding.
/// This blocks during decoding; callers needing a hard deadline should isolate
/// the call in a process they can terminate, including libmpv initialization/cleanup.
pub fn extract_frame(path: &Path, position: Duration) -> Result<Frame> {
    let api = Api::load()?;
    // SAFETY: the independent core is owned by Capture until decoding and copying finish.
    let handle = unsafe { (api.create)() };
    ensure!(!handle.is_null(), "mpv_create failed");
    let capture = Capture { api, handle };
    for (name, value) in [
        ("config", "no"),
        ("terminal", "no"),
        ("msg-level", "all=no"),
        ("vo", "null"),
        ("audio", "no"),
        ("sub", "no"),
        ("audio-display", "no"),
        ("access-references", "no"),
        ("autoload-files", "no"),
        ("load-scripts", "no"),
        ("ytdl", "no"),
        ("osc", "no"),
        ("osd-level", "0"),
        ("input-default-bindings", "no"),
        ("hwdec", "no"),
        ("vd-lavc-threads", "2"),
        ("background", "color"),
        ("pause", "yes"),
        ("idle", "yes"),
        ("keep-open", "no"),
        ("loop-file", "no"),
        ("hr-seek", "yes"),
        ("start", &position.as_secs_f64().to_string()),
    ] {
        let name = CString::new(name)?;
        let value = CString::new(value)?;
        // SAFETY: options are copied before initialization, while both strings are live.
        capture
            .api
            .check(unsafe { (capture.api.option)(handle, name.as_ptr(), value.as_ptr()) })?;
    }
    // SAFETY: initialization runs before loading media; there is no render dependency.
    capture
        .api
        .check(unsafe { (capture.api.initialize)(handle) })?;
    let path = CString::new(path.as_os_str().as_encoded_bytes())?;
    let args = [
        c"loadfile".as_ptr(),
        path.as_ptr(),
        c"replace".as_ptr(),
        ptr::null(),
    ];
    // SAFETY: the asynchronous command copies arguments before returning.
    capture
        .api
        .check(unsafe { (capture.api.command)(handle, 0, args.as_ptr()) })?;
    loop {
        // SAFETY: no render context exists, so blocking event waits cannot deadlock a renderer.
        let event = unsafe { &*(capture.api.wait_event)(handle, 0.1) };
        capture.api.check(event.error)?;
        match event.kind {
            1 => bail!("libmpv shut down during frame capture"),
            7 if !event.data.is_null() => {
                // SAFETY: END_FILE data remains valid until the next wait_event call.
                let end = unsafe { &*event.data.cast::<EndFile>() };
                capture.api.check(end.error)?;
                ensure!(
                    end.reason == 5,
                    "media contains no frame at the requested position"
                );
            }
            // PLAYBACK_RESTART means the initial exact seek has produced its paused frame.
            21 => return capture.frame(position),
            _ => {}
        }
    }
}

impl Capture {
    fn frame(&self, position: Duration) -> Result<Frame> {
        let mut duration = 0f64;
        // SAFETY: null VO has no render-thread dependency; mpv writes an owned double.
        let code = unsafe {
            (self.api.get_property)(
                self.handle,
                c"duration".as_ptr(),
                5,
                ptr::from_mut(&mut duration).cast(),
            )
        };
        // Unknown duration is allowed; all other property errors remain visible.
        if !matches!(code, -10 | -11) {
            // PROPERTY_UNAVAILABLE / PROPERTY_ERROR
            self.api.check(code)?;
        }
        ensure!(
            !duration.is_finite() || duration <= 0. || position.as_secs_f64() < duration,
            "media contains no frame at the requested position"
        );
        let args = [c"screenshot-raw".as_ptr(), c"video".as_ptr(), ptr::null()];
        let mut node = Node {
            value: NodeValue { integer: 0 },
            format: 0,
        };
        // SAFETY: null VO has no render-thread dependency; mpv allocates the returned node.
        self.api
            .check(unsafe { (self.api.command_ret)(self.handle, args.as_ptr(), &mut node) })?;
        let result = copy_frame(&node);
        // SAFETY: release exactly the successful command result after copying its pixels.
        unsafe { (self.api.free_node)(&mut node) };
        result
    }
}

fn copy_frame(node: &Node) -> Result<Frame> {
    let width = u32::try_from(node.field("w")?.integer()?)?;
    let height = u32::try_from(node.field("h")?.integer()?)?;
    let stride = isize::try_from(node.field("stride")?.integer()?)?;
    let row_bytes = (width as usize)
        .checked_mul(4)
        .context("captured row exceeds address space")?;
    let length = row_bytes
        .checked_mul(height as usize)
        .filter(|&n| n > 0 && n <= 512 * 1024 * 1024)
        .context("captured frame exceeds 512 MiB or is empty")?;
    let format = node.field("format")?;
    ensure!(format.format == 1, "invalid captured pixel format");
    // SAFETY: the string union member is valid for MPV_FORMAT_STRING and lives with node.
    ensure!(
        unsafe { CStr::from_ptr(format.value.string) }.to_bytes() == b"bgr0",
        "unsupported captured pixel format"
    );
    let data = node.field("data")?;
    ensure!(data.format == 9, "invalid captured pixel data");
    // SAFETY: MPV_FORMAT_BYTE_ARRAY selects this union member; the owner is still live.
    let bytes = unsafe { &*data.value.bytes };
    let required = stride
        .unsigned_abs()
        .checked_mul(height as usize - 1)
        .and_then(|n| n.checked_add(row_bytes))
        .context("invalid captured stride")?;
    ensure!(
        stride.unsigned_abs() >= row_bytes
            && required <= bytes.size
            && required <= isize::MAX as usize
            && !bytes.data.is_null(),
        "invalid captured buffer"
    );
    let mut pixels = Vec::with_capacity(length);
    for y in 0..height as isize {
        // SAFETY: screenshot-raw guarantees row y at data + y*stride, including negative
        // strides. Each row is live until free_node; the bounds/offsets were checked above.
        let row = unsafe {
            std::slice::from_raw_parts(bytes.data.cast::<u8>().offset(y * stride), row_bytes)
        };
        for &[b, g, r, _] in row.as_chunks::<4>().0 {
            pixels.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Ok(Frame {
        width,
        height,
        pixels,
    })
}

impl Node {
    fn field(&self, name: &str) -> Result<&Node> {
        ensure!(self.format == 8, "invalid captured frame map");
        // SAFETY: only live mpv command results reach this helper; NODE_MAP owns count
        // key/value pairs with non-null C string keys, per the public client ABI.
        let list = unsafe { &*self.value.list };
        for i in 0..list.count as usize {
            if unsafe { CStr::from_ptr(*list.keys.add(i)) }.to_bytes() == name.as_bytes() {
                return Ok(unsafe { &*list.values.add(i) });
            }
        }
        bail!("captured frame has no {name}")
    }

    fn integer(&self) -> Result<i64> {
        ensure!(self.format == 4, "invalid captured integer");
        // SAFETY: MPV_FORMAT_INT64 selects the integer union member.
        Ok(unsafe { self.value.integer })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: terminate the independent core after copying all returned data.
        unsafe { (self.api.destroy)(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ByteArray, NodeList};

    #[test]
    fn captured_pixels_respect_padding_and_negative_stride() {
        let mut buffer = [
            0u8, 0, 255, 17, 0, 255, 0, 17, 99, 99, 99, 99, 255, 0, 0, 17, 255, 255, 255, 17, 99,
            99, 99, 99,
        ];
        let mut capture = |stride, height, size| {
            let data = buffer.as_mut_ptr();
            let mut bytes = ByteArray {
                data: if stride < 0 {
                    data.wrapping_add(12).cast()
                } else {
                    data.cast()
                },
                size,
            };
            let integer = |value| Node {
                value: NodeValue { integer: value },
                format: 4,
            };
            let mut values = [
                integer(2),
                integer(height),
                integer(stride),
                Node {
                    value: NodeValue {
                        string: c"bgr0".as_ptr().cast_mut(),
                    },
                    format: 1,
                },
                Node {
                    value: NodeValue { bytes: &mut bytes },
                    format: 9,
                },
            ];
            let mut keys =
                [c"w", c"h", c"stride", c"format", c"data"].map(|key| key.as_ptr().cast_mut());
            let mut list = NodeList {
                count: 5,
                values: values.as_mut_ptr(),
                keys: keys.as_mut_ptr(),
            };
            let node = Node {
                value: NodeValue { list: &mut list },
                format: 8,
            };
            copy_frame(&node)
        };
        let top = [255, 0, 0, 255, 0, 255, 0, 255];
        let bottom = [0, 0, 255, 255, 255, 255, 255, 255];
        assert_eq!(capture(12, 2, 24).unwrap().pixels, [top, bottom].concat());
        assert_eq!(capture(-12, 2, 24).unwrap().pixels, [bottom, top].concat());
        assert!(capture(12, 2, 8).is_err());
        assert!(capture(12, 0, 24).is_err());
    }
}
