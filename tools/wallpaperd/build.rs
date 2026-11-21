fn main() {
    let mut paths = vec!["$ORIGIN"];
    if std::env::var_os("CARGO_FEATURE_UI").is_some() {
        paths.push("$ORIGIN/../lib/wallpaperd");
    }
    if std::env::var_os("CARGO_FEATURE_WEB").is_some() {
        paths.extend(["$ORIGIN/..", "$ORIGIN/../lib/wallpaperd/web"]);
    }
    if paths.len() > 1 {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", paths.join(":"));
    }
}
