mod audio_capture;
mod catalog;
mod content;
mod decoder;
mod desktop;
mod domain;
mod graphics;
mod ipc;
mod library;
mod media;
mod pixels;
mod policy;
mod properties;
mod renderer;
mod session;
mod store;
mod thumbnails;
mod web;
mod worker;

#[cfg(test)]
mod benchmark;

use std::{
    io::{BufRead, BufReader, IsTerminal, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::{
    domain::{AssetRef, Fit, Request},
    store::socket_path,
};

#[derive(Parser)]
#[command(about = "Session wallpaper daemon and control client")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve,
    /// List wallpapers by name and type; pipes retain the full JSON response.
    List {
        /// Print the full JSON response, including paths and project metadata.
        #[arg(long)]
        json: bool,
        /// Cache small previews in this client, outside the daemon and UI threads.
        #[arg(long)]
        thumbnails: bool,
    },
    /// Read imported sources, favorites and playlists.
    Library,
    /// Edit library metadata using a JSON action; source files are never deleted.
    LibraryEdit {
        edit: String,
    },
    /// Configure persistent per-output rotation using JSON.
    Rotation {
        settings: String,
        #[arg(long)]
        output: Option<String>,
    },
    /// Advance the configured rotation source once.
    Next {
        #[arg(long)]
        output: Option<String>,
    },
    Set {
        asset: String,
        #[arg(long)]
        output: Option<String>,
        /// Override the configured output/global fit.
        #[arg(long, value_enum)]
        fit: Option<CliFit>,
        /// Override the configured transition.
        #[arg(long)]
        transition: Option<String>,
        #[arg(long)]
        fps: Option<u32>,
        #[arg(long)]
        mute: Option<bool>,
        #[arg(long)]
        volume: Option<u32>,
        #[arg(long)]
        properties: Option<String>,
    },
    /// Read a Wallpaper Engine project's property definitions and saved values.
    Properties {
        asset: String,
        #[arg(long)]
        output: Option<String>,
        /// Resolve draft values and display conditions without saving or applying.
        #[arg(long)]
        properties: Option<String>,
    },
    /// Update current Wallpaper Engine properties; JSON null restores a default.
    SetProperties {
        asset: String,
        values: String,
        #[arg(long)]
        output: Option<String>,
    },
    Pause {
        #[arg(long)]
        output: Option<String>,
    },
    Resume {
        #[arg(long)]
        output: Option<String>,
    },
    Playback {
        #[arg(long)]
        output: Option<String>,
        #[arg(long)]
        fps: Option<u32>,
        #[arg(long)]
        mute: Option<bool>,
        #[arg(long)]
        volume: Option<u32>,
    },
    Release {
        #[arg(long)]
        output: Option<String>,
    },
    Status,
    /// Export the current wallpaper alone as a fitted PNG, without desktop windows.
    Snapshot {
        #[arg(long)]
        output: Option<String>,
        /// Reject the request if this output's wallpaper revision has changed.
        #[arg(long)]
        revision: Option<u64>,
    },
    Subscribe,
    #[command(hide = true)]
    RenderWorker {
        #[arg(long)]
        output: String,
    },
    #[command(hide = true)]
    RenderThumbnail {
        source: PathBuf,
        destination: PathBuf,
        #[arg(long)]
        position: Option<f64>,
    },
}

#[derive(Copy, Clone, ValueEnum)]
enum CliFit {
    Cover,
    Contain,
    Stretch,
}

impl From<CliFit> for Fit {
    fn from(value: CliFit) -> Self {
        match value {
            CliFit::Cover => Fit::Cover,
            CliFit::Contain => Fit::Contain,
            CliFit::Stretch => Fit::Stretch,
        }
    }
}

fn main() -> Result<()> {
    #[cfg(feature = "web")]
    if let Some(code) = we_web::execute_process() {
        std::process::exit(code);
    }
    let cli = Cli::parse();
    match cli.command {
        Command::Serve => session::serve(),
        Command::RenderWorker { output } => {
            // This is a fresh child entry, before any application threads/CEF initialization.
            unsafe {
                std::env::set_var("PULSE_PROP", "application.id=misari.wallpaperd");
            }
            worker::run(&output)
        }
        Command::RenderThumbnail {
            source,
            destination,
            position,
        } => if let Some(position) = position {
            thumbnails::capture_video(&source, Duration::try_from_secs_f64(position)?)?
        } else {
            content::shader_thumbnail(&source)?
        }
        .save_with_format(destination, image::ImageFormat::Png)
        .context("saving captured thumbnail"),
        Command::List { thumbnails, json } => {
            let mut reply = request("catalog", json!({}))?;
            if thumbnails && let Some(assets) = reply["assets"].as_array_mut() {
                thumbnails::fill(assets);
            }
            if json || !std::io::stdout().is_terminal() {
                print_reply(reply)
            } else {
                let home = std::env::var_os("HOME").map(PathBuf::from);
                print!(
                    "{}",
                    format_list(&reply, terminal_width(), home.as_deref())?
                );
                Ok(())
            }
        }
        Command::Library => print_reply(request("get_library", json!({}))?),
        Command::LibraryEdit { edit } => print_reply(request(
            "edit_library",
            serde_json::from_str(&edit).context("invalid library JSON")?,
        )?),
        Command::Rotation { settings, output } => print_reply(request(
            "set_rotation",
            json!({"rotation":serde_json::from_str::<Value>(&settings).context("invalid rotation JSON")?,"output":output}),
        )?),
        Command::Next { output } => {
            print_reply(request("rotation_next", json!({"output":output}))?)
        }
        Command::Status => print_reply(request("get", json!({}))?),
        Command::Snapshot { output, revision } => print_reply(request(
            "get_snapshot",
            json!({"output":output,"revision":revision}),
        )?),
        Command::Set {
            asset,
            output,
            fit,
            transition,
            fps,
            mute,
            volume,
            properties,
        } => {
            let asset = expand_home(asset);
            print_reply(request("apply", {
                let mut params =
                    json!({"asset_id":asset,"output":output,"fps":fps,"mute":mute,"volume":volume});
                if let Some(fit) = fit {
                    params["fit"] = json!(Fit::from(fit));
                }
                if let Some(transition) = transition {
                    params["transition"] = json!(transition);
                }
                if let Some(properties) = properties {
                    params["properties"] =
                        serde_json::from_str(&properties).context("invalid properties JSON")?;
                }
                params
            })?)
        }
        Command::Properties {
            asset,
            output,
            properties,
        } => {
            let mut params = json!({"asset_id":expand_home(asset),"output":output});
            if let Some(properties) = properties {
                params["properties"] =
                    serde_json::from_str(&properties).context("invalid properties JSON")?;
            }
            print_reply(request("get_properties", params)?)
        }
        Command::SetProperties {
            asset,
            values,
            output,
        } => print_reply(request(
            "set_properties",
            json!({"asset_id":expand_home(asset),"output":output,"properties":serde_json::from_str::<Value>(&values).context("invalid properties JSON")?}),
        )?),
        Command::Pause { output } => print_reply(request(
            "set_playback",
            json!({"output":output,"paused":true}),
        )?),
        Command::Resume { output } => print_reply(request(
            "set_playback",
            json!({"output":output,"paused":false}),
        )?),
        Command::Playback {
            output,
            fps,
            mute,
            volume,
        } => print_reply(request(
            "set_playback",
            json!({"output":output,"fps":fps,"mute":mute,"volume":volume}),
        )?),
        Command::Release { output } => print_reply(request("release", json!({"output":output}))?),
        Command::Subscribe => subscribe(),
    }
}

fn expand_home(asset: String) -> String {
    store::expand_home(PathBuf::from(asset))
        .to_string_lossy()
        .into_owned()
}

fn connect() -> Result<UnixStream> {
    let path = socket_path();
    UnixStream::connect(&path).with_context(|| {
        format!(
            "connecting to {} (is wallpaperd serve running?)",
            path.display()
        )
    })
}

fn send(stream: &mut UnixStream, method: &str, params: Value) -> Result<()> {
    serde_json::to_writer(
        &mut *stream,
        &Request {
            api: domain::API,
            method: method.into(),
            params,
        },
    )?;
    stream.write_all(b"\n")?;
    Ok(())
}

fn request(method: &str, params: Value) -> Result<Value> {
    let mut stream = connect()?;
    send(&mut stream, method, params)?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    anyhow::ensure!(
        !line.is_empty(),
        "daemon closed the connection without a reply"
    );
    Ok(serde_json::from_str(&line)?)
}

fn check_reply(reply: &Value) -> Result<()> {
    if reply.get("ok").and_then(Value::as_bool) == Some(false) {
        let error = reply.get("error");
        let code = error
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("error");
        let message = error
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        anyhow::bail!("{code}: {message}");
    }
    Ok(())
}

fn print_reply(reply: Value) -> Result<()> {
    check_reply(&reply)?;
    println!("{}", serde_json::to_string_pretty(&reply)?);
    Ok(())
}

fn terminal_width() -> usize {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    // ioctl writes only to this initialized winsize; stdout is already known to be a TTY.
    if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0
        && size.ws_col > 0
    {
        usize::from(size.ws_col)
    } else {
        80
    }
}

fn compact_cell(value: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;

    if value.width() <= width {
        return value.into();
    }
    if width == 0 {
        return String::new();
    }
    // Keep more of the end, where paths carry the filename or Workshop project ID.
    let left_width = (width - 1) / 3;
    let right_width = width - 1 - left_width;
    let left = value
        .char_indices()
        .map(|(i, _)| &value[..i])
        .take_while(|part| part.width() <= left_width)
        .last()
        .unwrap_or("");
    let right = value
        .char_indices()
        .rev()
        .map(|(i, _)| &value[i..])
        .take_while(|part| part.width() <= right_width)
        .last()
        .unwrap_or("");
    format!("{left}…{right}")
}

fn format_list(reply: &Value, width: usize, home: Option<&Path>) -> Result<String> {
    use std::fmt::Write;

    check_reply(reply)?;
    let assets = reply["assets"]
        .as_array()
        .context("missing catalog assets")?;
    if assets.is_empty() {
        return Ok(format!("{}\n", compact_cell("No wallpapers found.", width)));
    }
    // Escape controls so filenames and project titles cannot disrupt terminal rows.
    let clean = |value: &str| {
        value
            .chars()
            .map(|c| {
                if c.is_control() {
                    c.escape_default().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect::<String>()
    };
    let rows: Vec<_> = assets
        .iter()
        .map(|asset| {
            let kind = asset["kind"].as_str().unwrap_or("?");
            let id = asset["id"].as_str().unwrap_or("?");
            let path = AssetRef::parse(id).map_or(id, |asset| asset.path().to_str().unwrap_or(id));
            let path = home
                .filter(|home| !home.as_os_str().is_empty())
                .and_then(|home| Path::new(path).strip_prefix(home).ok())
                .map_or_else(
                    || path.to_owned(),
                    |relative| {
                        if relative.as_os_str().is_empty() {
                            "~".into()
                        } else {
                            format!("~/{}", relative.display())
                        }
                    },
                );
            (
                clean(asset["name"].as_str().unwrap_or("?")),
                clean(kind.strip_prefix("we_").unwrap_or(kind)),
                clean(&path),
            )
        })
        .collect();
    use unicode_width::UnicodeWidthStr;
    let mut text = String::new();
    if width < 24 {
        writeln!(text, "{}", compact_cell("NAME", width))?;
        for (name, _, _) in &rows {
            writeln!(text, "{}", compact_cell(name, width))?;
        }
    } else {
        let name_width = rows
            .iter()
            .map(|(name, _, _)| name.width())
            .max()
            .unwrap()
            .max(4);
        let kind_width = rows
            .iter()
            .map(|(_, kind, _)| kind.width())
            .max()
            .unwrap()
            .max(4);
        let available = width - kind_width - 4;
        let name_width = name_width.min(24).min((available / 3).max(4));
        let path_width = available - name_width;
        for (name, kind, path) in std::iter::once(("NAME", "TYPE", "PATH")).chain(
            rows.iter()
                .map(|(name, kind, path)| (name.as_str(), kind.as_str(), path.as_str())),
        ) {
            let name = compact_cell(name, name_width);
            let path = compact_cell(path, path_width);
            writeln!(
                text,
                "{name}{:name_pad$}  {kind}{:kind_pad$}  {path}",
                "",
                "",
                name_pad = name_width - name.width(),
                kind_pad = kind_width - kind.width()
            )?;
        }
    }
    for (asset, (name, _, _)) in assets.iter().zip(&rows) {
        if let Some(message) = asset["error"]["message"].as_str() {
            writeln!(
                text,
                "{}",
                compact_cell(&format!("! {name}: {}", clean(message)), width)
            )?;
        }
    }
    Ok(text)
}

fn subscribe() -> Result<()> {
    let mut stream = connect()?;
    send(&mut stream, "subscribe", json!({}))?;
    for line in BufReader::new(stream).lines() {
        println!("{}", line?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_compact_and_retains_paths_and_errors() {
        let reply = json!({"ok":true,"assets":[
            {"name":"湖泊's.png","kind":"image","id":"local:/Pictures/湖泊's.png",
             "path":"/Pictures/湖泊.png","thumbnail":"/cache/preview.png"},
            {"name":"Night\nSky","kind":"we_scene","id":"we:/workshop/123",
             "project":{"file":"scene.pkg"},"error":{"message":"missing scene.pkg"}}
        ]});
        assert_eq!(
            format_list(&reply, 80, None).unwrap(),
            "NAME        TYPE   PATH\n湖泊's.png  image  /Pictures/湖泊's.png\nNight\\nSky  scene  /workshop/123\n! Night\\nSky: missing scene.pkg\n"
        );
        assert_eq!(
            format_list(&json!({"ok":true,"assets":[]}), 80, None).unwrap(),
            "No wallpapers found.\n"
        );
        assert_eq!(
            format_list(
                &json!({"ok":false,"error":{"code":"bad_api","message":"unsupported API"}}),
                80,
                None
            )
            .unwrap_err()
            .to_string(),
            "bad_api: unsupported API"
        );
        let cli = Cli::try_parse_from(["wallpaperd", "list", "--json", "--thumbnails"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::List {
                json: true,
                thumbnails: true
            }
        ));
    }

    #[test]
    fn list_compresses_home_and_fits_narrow_terminals() {
        use unicode_width::UnicodeWidthStr;

        let reply = json!({"ok":true,"assets":[
            {"name":"星空中的湖泊和森林壁纸.png","kind":"image",
             "id":"local:/home/test/Pictures/Wallpapers/旅行/夏天/湖泊.png",
             "error":{"message":"a very long error message that must fit the terminal"}},
            {"name":"Sky","kind":"we_application","id":"we:/home/test/Workshop/123"}
        ]});
        let home = Some(Path::new("/home/test"));
        let wide = format_list(&reply, 120, home).unwrap();
        assert!(wide.contains("~/Pictures/Wallpapers/旅行/夏天/湖泊.png"));
        assert!(!wide.contains("/home/test"));
        assert!(format_list(&reply, 40, home).unwrap().contains("…"));
        for width in [0, 1, 12, 23, 24, 40, 80, 120] {
            let text = format_list(&reply, width, home).unwrap();
            assert!(
                text.lines().all(|line| line.width() <= width),
                "width {width}: {text}"
            );
        }
        assert_eq!(
            compact_cell("/long/directory/lake.jpg", 16),
            "/long…y/lake.jpg"
        );
        assert_eq!(compact_cell("湖泊.png", 30), "湖泊.png");
    }

    #[test]
    fn set_options_are_omitted_unless_explicitly_requested() {
        let cli = Cli::try_parse_from(["wallpaperd", "set", "/wallpaper.png"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Set {
                fit: None,
                transition: None,
                ..
            }
        ));
        let cli = Cli::try_parse_from([
            "wallpaperd",
            "set",
            "/wallpaper.png",
            "--fit",
            "contain",
            "--transition",
            "fade",
        ])
        .unwrap();
        let Command::Set {
            fit, transition, ..
        } = cli.command
        else {
            panic!("expected set");
        };
        assert!(matches!(fit, Some(CliFit::Contain)));
        assert_eq!(transition.as_deref(), Some("fade"));
    }
}
