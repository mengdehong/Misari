//! Dynamically loaded libmpv client/render API; one owner keeps callbacks and contexts alive.
use anyhow::{Context, Result, bail};
use libloading::Library;
use std::{
    ffi::{CStr, c_char, c_void},
    ptr,
    sync::{Arc, OnceLock},
};
#[repr(C)]
pub(crate) struct Param {
    pub(crate) kind: i32,
    pub(crate) data: *mut c_void,
}
impl Param {
    pub(crate) fn new<T>(kind: i32, data: &mut T) -> Self {
        Self {
            kind,
            data: ptr::from_mut(data).cast(),
        }
    }
    pub(crate) fn end() -> Self {
        Self {
            kind: 0,
            data: ptr::null_mut(),
        }
    }
}
#[repr(C)]
pub(crate) struct GlInit {
    pub(crate) get_proc_address: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    pub(crate) context: *mut c_void,
}
#[repr(C)]
pub(crate) struct Fbo {
    pub(crate) fbo: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) format: i32,
}
#[repr(C)]
pub(crate) struct Event {
    pub(crate) kind: i32,
    pub(crate) error: i32,
    pub(crate) userdata: u64,
    pub(crate) data: *mut c_void,
}
#[repr(C)]
pub(crate) struct EndFile {
    pub(crate) reason: i32,
    pub(crate) error: i32,
}
#[repr(C)]
pub(crate) struct Property {
    pub(crate) name: *const c_char,
    pub(crate) format: i32,
    pub(crate) data: *mut c_void,
}
#[repr(C)]
pub(crate) struct FrameInfo {
    pub(crate) flags: u64,
    pub(crate) target_time: i64,
}

#[repr(C)]
pub(crate) union NodeValue {
    pub(crate) integer: i64,
    pub(crate) string: *mut c_char,
    pub(crate) list: *mut NodeList,
    pub(crate) bytes: *mut ByteArray,
    // These members also preserve the complete public union's size/alignment.
    pub(crate) double: f64,
    pub(crate) flag: i32,
}
#[repr(C)]
pub(crate) struct Node {
    pub(crate) value: NodeValue,
    pub(crate) format: i32,
}
#[repr(C)]
pub(crate) struct NodeList {
    pub(crate) count: i32,
    pub(crate) values: *mut Node,
    pub(crate) keys: *mut *mut c_char,
}
#[repr(C)]
pub(crate) struct ByteArray {
    pub(crate) data: *mut c_void,
    pub(crate) size: usize,
}

pub(crate) type Callback = extern "C" fn(*mut c_void);

pub(crate) struct Api {
    pub(crate) create: unsafe extern "C" fn() -> *mut c_void,
    pub(crate) initialize: unsafe extern "C" fn(*mut c_void) -> i32,
    pub(crate) destroy: unsafe extern "C" fn(*mut c_void),
    pub(crate) option: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> i32,
    pub(crate) command: unsafe extern "C" fn(*mut c_void, u64, *const *const c_char) -> i32,
    pub(crate) command_ret:
        unsafe extern "C" fn(*mut c_void, *const *const c_char, *mut Node) -> i32,
    pub(crate) free_node: unsafe extern "C" fn(*mut Node),
    pub(crate) get_property:
        unsafe extern "C" fn(*mut c_void, *const c_char, i32, *mut c_void) -> i32,
    pub(crate) property:
        unsafe extern "C" fn(*mut c_void, u64, *const c_char, i32, *mut c_void) -> i32,
    pub(crate) observe: unsafe extern "C" fn(*mut c_void, u64, *const c_char, i32) -> i32,
    pub(crate) wait_event: unsafe extern "C" fn(*mut c_void, f64) -> *const Event,
    pub(crate) set_wakeup: unsafe extern "C" fn(*mut c_void, Option<Callback>, *mut c_void),
    pub(crate) render_create:
        unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *mut Param) -> i32,
    pub(crate) render_free: unsafe extern "C" fn(*mut c_void),
    pub(crate) render_callback: unsafe extern "C" fn(*mut c_void, Option<Callback>, *mut c_void),
    pub(crate) render_update: unsafe extern "C" fn(*mut c_void) -> u64,
    pub(crate) render_info: unsafe extern "C" fn(*mut c_void, Param) -> i32,
    pub(crate) render: unsafe extern "C" fn(*mut c_void, *mut Param) -> i32,
    pub(crate) report_swap: unsafe extern "C" fn(*mut c_void),
    pub(crate) error_string: unsafe extern "C" fn(i32) -> *const c_char,
    pub(crate) version: unsafe extern "C" fn() -> u64,
    pub(crate) _library: Library,
}

impl Api {
    pub(crate) fn load() -> Result<Arc<Self>> {
        static API: OnceLock<Result<Arc<Api>, String>> = OnceLock::new();
        API.get_or_init(|| Self::open().map(Arc::new).map_err(|e| e.to_string()))
            .clone()
            .map_err(anyhow::Error::msg)
    }

    fn open() -> Result<Self> {
        // SAFETY: all symbols use the public libmpv C ABI; the library outlives every handle.
        unsafe {
            let library =
                Library::new("libmpv.so.2").context("libmpv is not installed (libmpv.so.2)")?;
            macro_rules! symbol {
                ($name:literal) => {
                    *library.get(concat!($name, "\0").as_bytes())?
                };
            }
            let api = Self {
                create: symbol!("mpv_create"),
                initialize: symbol!("mpv_initialize"),
                destroy: symbol!("mpv_terminate_destroy"),
                option: symbol!("mpv_set_option_string"),
                command: symbol!("mpv_command_async"),
                command_ret: symbol!("mpv_command_ret"),
                free_node: symbol!("mpv_free_node_contents"),
                get_property: symbol!("mpv_get_property"),
                property: symbol!("mpv_set_property_async"),
                observe: symbol!("mpv_observe_property"),
                wait_event: symbol!("mpv_wait_event"),
                set_wakeup: symbol!("mpv_set_wakeup_callback"),
                render_create: symbol!("mpv_render_context_create"),
                render_free: symbol!("mpv_render_context_free"),
                render_callback: symbol!("mpv_render_context_set_update_callback"),
                render_update: symbol!("mpv_render_context_update"),
                render_info: symbol!("mpv_render_context_get_info"),
                render: symbol!("mpv_render_context_render"),
                report_swap: symbol!("mpv_render_context_report_swap"),
                error_string: symbol!("mpv_error_string"),
                version: symbol!("mpv_client_api_version"),
                _library: library,
            };
            anyhow::ensure!(
                (api.version)() >> 16 == 2,
                "wallpaperd requires libmpv client API major version 2"
            );
            Ok(api)
        }
    }

    pub(crate) fn check(&self, code: i32) -> Result<()> {
        if code < 0 {
            // SAFETY: mpv returns a static NUL-terminated string for any error code.
            bail!(
                "libmpv: {}",
                unsafe { CStr::from_ptr((self.error_string)(code)) }.to_string_lossy()
            );
        }
        Ok(())
    }
}

pub fn capability() -> serde_json::Value {
    match Api::load() {
        Ok(api) => {
            // SAFETY: this function has no handle and only returns the library's ABI version.
            let version = unsafe { (api.version)() };
            serde_json::json!({"available":true,"client_api":format!("{}.{}",version >> 16,version & 0xffff),"content":["video","wallpaper_media"],"pause":true,"mute":true,"volume":true,"fps_limit":true,"frame_output":"opengl_fbo"})
        }
        Err(error) => {
            serde_json::json!({"available":false,"error":error.to_string(),"content":["video","wallpaper_media"]})
        }
    }
}
