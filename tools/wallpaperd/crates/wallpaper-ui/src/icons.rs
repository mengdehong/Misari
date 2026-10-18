//! Lucide Static 0.468.0, bundled under ISC; see assets/lucide/LICENSE.
use iced::{
    Font,
    widget::{Text, text},
};

pub const FONT_BYTES: &[u8] = include_bytes!("../assets/lucide/lucide.ttf");

#[derive(Clone, Copy)]
pub enum Icon {
    Grid,
    Compass,
    Workshop,
    Playlist,
    Download,
    Monitor,
    Settings,
    PanelLeftClose,
    PanelLeftOpen,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    Refresh,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Close,
    Star,
    Folder,
    Trash,
    Pause,
    Play,
    Stop,
    Volume,
    Muted,
    Next,
    Plus,
    Check,
    Empty,
    Alert,
}

pub fn icon(icon: Icon) -> Text<'static> {
    let glyph = match icon {
        Icon::Grid => '\u{e104}',
        Icon::Compass => '\u{e09f}',
        Icon::Workshop => '\u{e0eb}',
        Icon::Playlist => '\u{e2df}',
        Icon::Download => '\u{e0b6}',
        Icon::Monitor => '\u{e120}',
        Icon::Settings => '\u{e244}',
        Icon::PanelLeftClose => '\u{e21b}',
        Icon::PanelLeftOpen => '\u{e21c}',
        Icon::ChevronLeft => '\u{e072}',
        Icon::ChevronRight => '\u{e073}',
        Icon::ChevronDown => '\u{e071}',
        Icon::Refresh => '\u{e148}',
        Icon::ArrowLeft => '\u{e04c}',
        Icon::ArrowRight => '\u{e04d}',
        Icon::ArrowUp => '\u{e04e}',
        Icon::ArrowDown => '\u{e046}',
        Icon::Close => '\u{e1b1}',
        Icon::Star => '\u{e179}',
        Icon::Folder => '\u{e246}',
        Icon::Trash => '\u{e18d}',
        Icon::Pause => '\u{e131}',
        Icon::Play => '\u{e13f}',
        Icon::Stop => '\u{e16a}',
        Icon::Volume => '\u{e1aa}',
        Icon::Muted => '\u{e1ab}',
        Icon::Next => '\u{e163}',
        Icon::Plus => '\u{e140}',
        Icon::Check => '\u{e070}',
        Icon::Empty => '\u{e0f9}',
        Icon::Alert => '\u{e07b}',
    };
    text(glyph.to_string())
        .font(Font::with_name("lucide"))
        .size(18)
        .line_height(1.0)
}
