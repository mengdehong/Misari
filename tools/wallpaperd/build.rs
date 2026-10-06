fn main() {
    if std::env::var_os("CARGO_FEATURE_WEB").is_some() {
        println!(
            "cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN:$ORIGIN/..:$ORIGIN/../lib/wallpaperd/web"
        );
    }
}
