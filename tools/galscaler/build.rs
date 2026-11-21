#[cfg(not(feature = "daemon"))]
fn main() {}

#[cfg(feature = "daemon")]
fn main() {
    let helper = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("client.exe");
    let status = std::process::Command::new("winegcc")
        .args([
            "-b",
            "x86_64-windows",
            "-Os",
            "-Wall",
            "-Wextra",
            "-nostdlib",
            "-nostartfiles",
            "-mwindows",
            "-Wl,--entry,mainCRTStartup",
            "src/client.c",
            "-luser32",
            "-lkernel32",
            "-o",
        ])
        .arg(helper)
        .status()
        .expect("winegcc, clang and lld are required to build the Win32 client-area helper");
    assert!(
        status.success(),
        "cannot build the Win32 client-area helper"
    );
    println!("cargo:rerun-if-changed=src/client.c");
    let placebo = pkg_config::Config::new()
        .atleast_version("7.0")
        .probe("libplacebo")
        .expect("libplacebo development files are required");
    let egl = pkg_config::Config::new()
        .probe("egl")
        .expect("EGL development files are required");
    pkg_config::Config::new()
        .probe("glesv2")
        .expect("GLES development files are required");
    cc::Build::new()
        .file("src/gpu.c")
        .includes(placebo.include_paths)
        .includes(egl.include_paths)
        .flag_if_supported("-std=c11")
        .warnings(true)
        .compile("galscaler_gpu");
    let mut stream = cc::Build::new();
    for name in [
        "libavcodec",
        "libavutil",
        "libswscale",
        "libva",
        "libdrm",
        "opus",
    ] {
        let lib = pkg_config::probe_library(name).unwrap_or_else(|error| panic!("{error}"));
        stream.includes(lib.include_paths);
    }
    println!("cargo:rustc-link-lib=datachannel");
    stream
        .file("src/stream.c")
        .flag_if_supported("-std=c11")
        .compile("galscaler_stream");
    println!("cargo:rerun-if-changed=src/stream.c");
    println!("cargo:rerun-if-changed=src/gpu.c");
}
