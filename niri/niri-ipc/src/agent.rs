//! Types for driving the agent seat.
//!
//! The agent seat is a second `wl_seat` that niri creates on demand for automation. It has its
//! own pointer, keyboard focus and clipboard, so a program can click and type into a window while
//! the user keeps working on another one.
//!
//! Every action targets a window by id. Points are in logical pixels relative to the top-left
//! corner of the window geometry (the same corner that [`AgentCapture`] reports its origin
//! against), so they don't depend on where the window currently is on screen, or whether it's
//! visible at all.

use serde::{Deserialize, Serialize};

/// Action for the agent seat.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "clap", derive(clap::Subcommand))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum AgentAction {
    /// Capture a window as a PNG image.
    ///
    /// Unlike the screenshot actions, this doesn't touch the clipboard, doesn't save anything to
    /// disk and doesn't show a notification.
    Capture {
        /// Id of the window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
        /// Render smaller so that the image's longer side is at most this many pixels.
        #[cfg_attr(feature = "clap", arg(long))]
        #[serde(default)]
        max_size: Option<u32>,
        /// Write the PNG to this file, and its coordinate mapping as JSON next to it
        /// (`<path>.json`), instead of writing the PNG to stdout.
        ///
        /// Handled by the CLI; not part of the request.
        #[cfg_attr(feature = "clap", arg(long))]
        #[serde(skip)]
        path: Option<std::path::PathBuf>,
    },
    /// Move the agent pointer to a point in a window.
    Move {
        /// Id of the window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
        /// X coordinate, in logical pixels from the left edge of the window.
        #[cfg_attr(feature = "clap", arg(allow_negative_numbers = true))]
        x: f64,
        /// Y coordinate, in logical pixels from the top edge of the window.
        #[cfg_attr(feature = "clap", arg(allow_negative_numbers = true))]
        y: f64,
    },
    /// Click at a point in a window.
    Click {
        /// Id of the window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
        /// X coordinate, in logical pixels from the left edge of the window.
        #[cfg_attr(feature = "clap", arg(allow_negative_numbers = true))]
        x: f64,
        /// Y coordinate, in logical pixels from the top edge of the window.
        #[cfg_attr(feature = "clap", arg(allow_negative_numbers = true))]
        y: f64,
        /// Mouse button to click.
        #[cfg_attr(feature = "clap", arg(long, value_enum, default_value_t = AgentButton::Left))]
        #[serde(default)]
        button: AgentButton,
        /// Number of clicks, for example 2 for a double click.
        #[cfg_attr(feature = "clap", arg(long, default_value_t = 1))]
        #[serde(default = "default_click_count")]
        count: u8,
    },
    /// Press or release a mouse button at a point in a window, for dragging.
    Button {
        /// Id of the window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
        /// X coordinate, in logical pixels from the left edge of the window.
        #[cfg_attr(feature = "clap", arg(allow_negative_numbers = true))]
        x: f64,
        /// Y coordinate, in logical pixels from the top edge of the window.
        #[cfg_attr(feature = "clap", arg(allow_negative_numbers = true))]
        y: f64,
        /// Mouse button.
        #[cfg_attr(feature = "clap", arg(long, value_enum, default_value_t = AgentButton::Left))]
        #[serde(default)]
        button: AgentButton,
        /// Whether to press (true) or release (false) the button.
        #[cfg_attr(feature = "clap", arg(long, action = clap::ArgAction::Set))]
        pressed: bool,
    },
    /// Scroll at a point in a window.
    Scroll {
        /// Id of the window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
        /// X coordinate, in logical pixels from the left edge of the window.
        #[cfg_attr(feature = "clap", arg(allow_negative_numbers = true))]
        x: f64,
        /// Y coordinate, in logical pixels from the top edge of the window.
        #[cfg_attr(feature = "clap", arg(allow_negative_numbers = true))]
        y: f64,
        /// Horizontal scroll amount in logical pixels, positive scrolls right.
        #[cfg_attr(
            feature = "clap",
            arg(long, default_value_t = 0., allow_negative_numbers = true)
        )]
        #[serde(default)]
        dx: f64,
        /// Vertical scroll amount in logical pixels, positive scrolls down.
        #[cfg_attr(
            feature = "clap",
            arg(long, default_value_t = 0., allow_negative_numbers = true)
        )]
        #[serde(default)]
        dy: f64,
    },
    /// Press a key combination in a window, for example `ctrl+shift+t` or `Return`.
    ///
    /// Key names are xkb keysym names; modifiers are `ctrl`, `shift`, `alt` and `super`.
    Key {
        /// Id of the window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
        /// Key combination.
        keys: String,
    },
    /// Launch an app for the agent seat and wait for its first window.
    ///
    /// The app connects through the agent's own Wayland socket: it sees only the agent seat, so
    /// apps that handle a single seat (Chromium, Electron, kitty) work too, and it runs with the
    /// same restrictions as sandboxed apps. Its windows match the `is-agent=true` window rule and
    /// never take your focus.
    ///
    /// Apps that hand off to an already running instance (most browsers with the same profile)
    /// open their window in that instance instead; give them a separate profile.
    Launch {
        /// Command to run.
        #[cfg_attr(feature = "clap", arg(last = true, required = true))]
        command: Vec<String>,
    },
    /// Print where a window's geometry sits in its surface, and its process id.
    ///
    /// For tools that read the window's accessibility tree: AT-SPI's window coordinates are
    /// relative to the surface, which can include client-side shadows; subtract `surface_offset`
    /// to get the window coordinates that actions take.
    Info {
        /// Id of the window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
    },
    /// Type text into a window.
    ///
    /// Any Unicode text works, independent of the keyboard layout and without an input method.
    Type {
        /// Id of the window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
        /// Text to type, or `-` to read it from stdin.
        text: String,
    },
}

fn default_click_count() -> u8 {
    1
}

/// Mouse button for the agent pointer.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum AgentButton {
    /// Left mouse button.
    #[default]
    Left,
    /// Right mouse button.
    Right,
    /// Middle mouse button.
    Middle,
}

/// Captured window image.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AgentCapture {
    /// Base64-encoded PNG image data.
    pub png_base64: String,
    /// Position of the image's top-left corner in window coordinates, in logical pixels.
    ///
    /// This is usually negative or zero: the image also covers client-side shadows and popups
    /// that stick out of the window geometry.
    pub origin: (f64, f64),
    /// Image pixels per logical pixel.
    ///
    /// Image pixel `(px, py)` is at window point `(origin.0 + px / scale, origin.1 + py / scale)`.
    pub scale: f64,
}

/// What an agent action caused, observed until the window settled.
///
/// Every input action replies with a receipt once the target window stops presenting new frames
/// (or after a time limit), so the caller knows whether anything happened without capturing and
/// comparing whole images.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AgentReceipt {
    /// Whether the window stopped presenting new frames before the time limit.
    ///
    /// `false` means it kept changing, for example while a page is loading or a video plays.
    pub settled: bool,
    /// Time from the action until the receipt was made, in milliseconds.
    pub elapsed_ms: u64,
    /// Areas of the window whose pixels changed, in window coordinates.
    ///
    /// Empty if nothing visible changed, for example after a click that hit nothing.
    pub changed: Vec<AgentRect>,
    /// New window size in logical pixels, if the window was resized.
    pub resized: Option<(f64, f64)>,
    /// Popups (menus, dropdowns, tooltips) of the window that opened, in window coordinates.
    pub popups_opened: Vec<AgentRect>,
    /// Popups of the window that closed, in window coordinates.
    pub popups_closed: Vec<AgentRect>,
    /// Windows that opened.
    pub windows_opened: Vec<AgentWindow>,
    /// Ids of windows that closed, including the target if it closed.
    pub windows_closed: Vec<u64>,
    /// New title of the window, if it changed.
    pub title: Option<String>,
    /// Where the agent keyboard focus is after the action.
    pub focus: AgentFocus,
}

/// Rectangle in window coordinates, in logical pixels.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AgentRect {
    /// X coordinate of the left edge.
    pub x: f64,
    /// Y coordinate of the top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

/// Facts about a window for tools that work with the agent seat.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AgentInfo {
    /// Id of the window.
    pub id: u64,
    /// Process id of the window's client, if known.
    pub pid: Option<i32>,
    /// Position of the window geometry's top-left corner in the window's main surface, in logical
    /// pixels.
    pub surface_offset: (f64, f64),
    /// Size of the window geometry in logical pixels.
    pub size: (f64, f64),
    /// Scale of the output the window is on.
    pub scale: f64,
    /// Top-left corners of the surfaces of the window's open popups (menus, dropdowns), in
    /// window coordinates, outermost popup first.
    pub popup_surfaces: Vec<(f64, f64)>,
}

/// Window that opened during an agent action.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AgentWindow {
    /// Id of the window.
    pub id: u64,
    /// Application id of the window.
    pub app_id: Option<String>,
    /// Title of the window.
    pub title: Option<String>,
    /// Id of the window's parent, for example the window a dialog belongs to.
    pub parent_id: Option<u64>,
}

/// Agent keyboard focus.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AgentFocus {
    /// No surface has the agent keyboard focus.
    None,
    /// The target window has the agent keyboard focus.
    Window,
    /// A popup of the target window has the agent keyboard focus, for example an open menu.
    Popup,
    /// Some other surface has the agent keyboard focus.
    Other,
}
