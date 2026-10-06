//! Resolve the C API only when a Web worker first needs it. Native workers and
//! the daemon must not pay libcef's relocations or keep its private pages alive.
use cef::sys::*;
use std::{
    ffi::{CStr, CString},
    os::{
        raw::{c_char, c_int, c_void},
        unix::ffi::OsStrExt,
    },
    path::PathBuf,
    sync::OnceLock,
};

struct Library {
    handle: usize,
    directory: PathBuf,
}
static LIBRARY: OnceLock<Result<Library, String>> = OnceLock::new();

pub(crate) fn directory() -> Option<PathBuf> {
    if let Some(Ok(library)) = LIBRARY.get() {
        return Some(library.directory.clone());
    }
    let executable = std::env::current_exe().ok()?;
    let mut origin = executable.parent()?;
    if origin.ends_with("deps") {
        origin = origin.parent()?;
    }
    let mut paths = std::env::var_os("LD_LIBRARY_PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    paths.extend([
        origin.to_owned(),
        origin.join(".."),
        origin.join("../lib/wallpaperd/web"),
    ]);
    paths
        .into_iter()
        .find(|path| path.join("libcef.so").is_file())?
        .canonicalize()
        .ok()
}

pub(crate) fn ensure_loaded() -> Result<(), String> {
    library().map(|_| ())
}

fn library() -> Result<&'static Library, String> {
    LIBRARY
        .get_or_init(|| {
            let directory = directory().ok_or("CEF runtime is unavailable")?;
            let path = CString::new(directory.join("libcef.so").as_os_str().as_bytes())
                .map_err(|_| "invalid CEF runtime path")?;
            // SAFETY: the path is terminated and CEF stays loaded until process exit.
            // Chromium retains callbacks and vtables even after its runtime shuts down.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                // SAFETY: a failed dlopen sets a thread-local, terminated error string.
                let error = unsafe { CStr::from_ptr(libc::dlerror()) }.to_string_lossy();
                return Err(format!("loading CEF: {error}"));
            }
            Ok(Library {
                handle: handle as usize,
                directory,
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn symbol(name: &CStr) -> *mut c_void {
    let library = library().unwrap_or_else(|error| panic!("{error}"));
    // SAFETY: this live handle owns the CEF C API, not our forwarding symbols.
    let address = unsafe { libc::dlsym(library.handle as *mut c_void, name.as_ptr()) };
    assert!(
        !address.is_null(),
        "CEF entry point is unavailable: {name:?}"
    );
    address
}

// Providing the used C entry points lets --as-needed omit libcef from the ELF
// dependencies. Keep each signature checked against the pinned cef-rs bindings.
macro_rules! entry_points {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $result:ty;)+) => {$(
        #[unsafe(no_mangle)]
        unsafe extern "C" fn $name($($arg: $ty),*) -> $result {
            const _: unsafe extern "C" fn($($ty),*) -> $result = cef::sys::$name;
            static FUNCTION: OnceLock<unsafe extern "C" fn($($ty),*) -> $result> = OnceLock::new();
            let function = FUNCTION.get_or_init(|| {
                let name = CStr::from_bytes_with_nul(concat!(stringify!($name), "\0").as_bytes()).unwrap();
                // SAFETY: dlsym returns the exact function checked above, and its
                // library remains loaded for the lifetime of this function pointer.
                unsafe { std::mem::transmute(symbol(name)) }
            });
            // SAFETY: we forward the caller's unchanged C API arguments.
            unsafe { function($($arg),*) }
        }
    )+};
}

entry_points! {
    cef_api_hash(version: c_int, entry: c_int) -> *const c_char;
    cef_browser_host_create_browser(window: *const cef_window_info_t, client: *mut cef_client_t,
        url: *const cef_string_t, settings: *const cef_browser_settings_t,
        extra: *mut cef_dictionary_value_t, context: *mut cef_request_context_t) -> c_int;
    cef_dictionary_value_create() -> *mut cef_dictionary_value_t;
    cef_execute_process(args: *const cef_main_args_t, app: *mut cef_app_t, sandbox: *mut c_void) -> c_int;
    cef_initialize(args: *const cef_main_args_t, settings: *const cef_settings_t,
        app: *mut cef_app_t, sandbox: *mut c_void) -> c_int;
    cef_post_delayed_task(thread: cef_thread_id_t, task: *mut cef_task_t, delay: i64) -> c_int;
    cef_post_task(thread: cef_thread_id_t, task: *mut cef_task_t) -> c_int;
    cef_process_message_create(name: *const cef_string_t) -> *mut cef_process_message_t;
    cef_request_context_create_context(settings: *const cef_request_context_settings_t,
        handler: *mut cef_request_context_handler_t) -> *mut cef_request_context_t;
    cef_shutdown() -> ();
    cef_string_list_free(value: cef_string_list_t) -> ();
    cef_string_userfree_utf16_free(value: cef_string_userfree_utf16_t) -> ();
    cef_string_utf16_clear(value: *mut cef_string_utf16_t) -> ();
    cef_string_utf16_set(source: *const char16_t, len: usize, output: *mut cef_string_utf16_t, copy: c_int) -> c_int;
    cef_string_utf16_to_utf8(source: *const char16_t, len: usize, output: *mut cef_string_utf8_t) -> c_int;
    cef_string_utf8_clear(value: *mut cef_string_utf8_t) -> ();
    cef_string_utf8_to_utf16(source: *const c_char, len: usize, output: *mut cef_string_utf16_t) -> c_int;
    cef_v8_value_create_function(name: *const cef_string_t, handler: *mut cef_v8_handler_t) -> *mut cef_v8_value_t;
}
