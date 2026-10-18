use crate::controls::{button, checkbox, pick_list, slider, text_input};
use crate::filters::{GROUPS, Group};
use crate::icons::{Icon, icon};
use crate::{App, Message, Page, array, strv};
use iced::{
    Alignment, Border, Color, Element,
    Length::{Fill, Fixed, Shrink},
    Theme,
    widget::{self, Column, Row, Space, column, container, image, progress_bar, row, text},
};
use serde_json::{Value, json};

use crate::palette;
pub use crate::palette::{BG, CARD, LINE, MUTED, PANEL, TEXT};

fn scrollable<'a>(content: impl Into<Element<'a, Message>>) -> widget::Scrollable<'a, Message> {
    widget::scrollable(content).direction(widget::scrollable::Direction::Vertical(
        widget::scrollable::Scrollbar::hidden(),
    ))
}

fn box_style(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(color.into()),
        border: Border {
            radius: 12.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}
fn card_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(if hover { CARD } else { Color::TRANSPARENT }.into()),
            text_color: TEXT,
            border: Border {
                color: if selected {
                    theme.palette().primary.scale_alpha(0.85)
                } else {
                    Color::TRANSPARENT
                },
                width: 1.0,
                radius: 13.0.into(),
            },
            ..Default::default()
        }
    }
}
pub(crate) fn secondary_style(_: &Theme, status: button::Status) -> button::Style {
    let disabled = status == button::Status::Disabled;
    button::Style {
        background: Some(
            palette::WASH
                .scale_alpha(match status {
                    button::Status::Hovered => 0.18,
                    button::Status::Pressed => 0.12,
                    button::Status::Disabled => 0.035,
                    _ => 0.09,
                })
                .into(),
        ),
        text_color: if disabled {
            Color::from_rgb8(86, 98, 110)
        } else {
            TEXT
        },
        border: Border {
            color: if disabled { Color::TRANSPARENT } else { LINE },
            width: 1.0,
            radius: palette::CONTROL_RADIUS.into(),
        },
        ..Default::default()
    }
}
pub(crate) fn secondary<'a>(
    label: impl Into<String>,
    message: Message,
) -> crate::controls::Button<'a, Message> {
    button(text(label.into()).size(13))
        .padding([9, 13])
        .on_press(message)
        .style(secondary_style)
}
fn icon_button<'a>(
    glyph: Icon,
    label: &'static str,
    message: Option<Message>,
) -> Element<'a, Message> {
    icon_action(glyph, label, message, false)
}
fn icon_action<'a>(
    glyph: Icon,
    label: &'static str,
    message: Option<Message>,
    selected: bool,
) -> Element<'a, Message> {
    widget::tooltip(
        button(container(icon(glyph)).center_x(22).center_y(22))
            .padding(9)
            .on_press_maybe(message)
            .style(move |theme, status| {
                let mut style = secondary_style(theme, status);
                style.border.radius = 50.0.into();
                if selected {
                    style.text_color = theme.palette().primary;
                    style.background = Some(theme.palette().primary.scale_alpha(0.10).into());
                }
                style
            }),
        text(label).size(12),
        widget::tooltip::Position::Top,
    )
    .gap(6)
    .padding(8)
    .delay(std::time::Duration::from_millis(400))
    .style(palette::glass)
    .into()
}
pub(crate) fn primary<'a>(
    label: impl Into<String>,
    message: Message,
    enabled: bool,
) -> crate::controls::Button<'a, Message> {
    button(text(label.into()).size(14))
        .padding([11, 17])
        .on_press_maybe(enabled.then_some(message))
        .style(|theme, status| button::Style {
            background: Some(
                if status == button::Status::Disabled {
                    CARD
                } else if status == button::Status::Hovered {
                    theme.palette().primary.scale_alpha(0.88)
                } else if status == button::Status::Pressed {
                    theme.palette().primary.scale_alpha(0.76)
                } else {
                    theme.palette().primary
                }
                .into(),
            ),
            text_color: if status == button::Status::Disabled {
                MUTED
            } else {
                BG
            },
            border: Border {
                radius: palette::CONTROL_RADIUS.into(),
                ..Default::default()
            },
            ..Default::default()
        })
}
fn muted<'a>(s: impl Into<String>) -> widget::Text<'a> {
    text(s.into()).size(12).color(MUTED)
}
fn heading<'a>(s: impl Into<String>) -> widget::Text<'a> {
    text(s.into()).size(18)
}
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.into()
    }
}
fn single_line_title<'a>(title: String, size: u32) -> Element<'a, Message> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    widget::responsive(move |bounds| {
        // Reserve an ellipsis and a full glyph width, including for CJK titles.
        let limit = (bounds.width / size as f32).floor() as usize;
        text(truncate(&title, limit.saturating_sub(1)))
            .size(size)
            .wrapping(widget::text::Wrapping::None)
            .width(Fill)
            .into()
    })
    .height(size as f32 * 1.3)
    .into()
}
pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "we_scene" | "scene" => "场景",
        "video" => "视频",
        "we_web" | "web" => "网页",
        "we_application" | "application" => "应用",
        "image" => "图片",
        "shader" => "着色器",
        _ => "壁纸",
    }
}
fn preview<'a>(app: &App, item: &Value, height: f32, fade: bool) -> Element<'a, Message> {
    let handle = app.previews.get(item).and_then(Clone::clone);
    let label = if matches!(app.previews.get(item), Some(None)) {
        "预览暂不可用"
    } else if item["preview_url"].is_string() || item["thumbnail"].is_string() {
        ""
    } else {
        "暂无预览"
    };
    widget::stack![
        container(muted(label))
            .center_x(Fill)
            .center_y(Fixed(height))
            .style(box_style(CARD)),
        Element::new(ClippedImage {
            handle,
            height,
            fade
        })
    ]
    .into()
}
// Iced's CPU image renderer does not enforce image drawing bounds itself.
// A renderer layer makes cover-cropping consistent for both raster and GPU renderers.
struct ClippedImage {
    handle: Option<widget::image::Handle>,
    height: f32,
    fade: bool,
}
struct PreviewMotion {
    current: Option<widget::image::Handle>,
    previous: Option<widget::image::Handle>,
    started: Option<std::time::Instant>,
    progress: f32,
}
impl PreviewMotion {
    fn replace(&mut self, handle: Option<widget::image::Handle>, fade: bool) {
        if !fade {
            self.current = handle;
            self.previous = None;
            self.started = None;
            self.progress = 1.0;
            return;
        }
        if self.current == handle {
            return;
        }
        // ponytail: retain at most two thumbnails; rapid switches use the dominant frame.
        self.previous = if self.progress >= 0.5 || self.previous.is_none() {
            self.current.clone()
        } else {
            self.previous.take()
        };
        self.current = handle;
        self.started = None;
        self.progress = if self.current.is_some() && !crate::motion::reduced() {
            0.0
        } else {
            1.0
        };
        if self.progress >= 1.0 {
            self.previous = None;
        }
    }
}
impl iced::advanced::Widget<Message, Theme, iced::Renderer> for ClippedImage {
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        iced::advanced::widget::tree::Tag::of::<PreviewMotion>()
    }
    fn state(&self) -> iced::advanced::widget::tree::State {
        iced::advanced::widget::tree::State::new(PreviewMotion {
            current: self.handle.clone(),
            previous: None,
            started: None,
            progress: if self.fade && self.handle.is_some() && !crate::motion::reduced() {
                0.0
            } else {
                1.0
            },
        })
    }
    fn diff(&self, tree: &mut iced::advanced::widget::Tree) {
        tree.state
            .downcast_mut::<PreviewMotion>()
            .replace(self.handle.clone(), self.fade);
    }
    fn size(&self) -> iced::Size<iced::Length> {
        iced::Size::new(Fill, Fixed(self.height))
    }
    fn layout(
        &mut self,
        _tree: &mut iced::advanced::widget::Tree,
        _renderer: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        // Fixed preview bounds need no image measurement. In particular, avoid
        // allocating renderer images for every off-screen row during layout.
        iced::advanced::layout::Node::new(limits.resolve(
            Fill,
            Fixed(self.height),
            iced::Size::ZERO,
        ))
    }
    fn update(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        _: iced::advanced::mouse::Cursor,
        _: &iced::Renderer,
        _: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        let iced::Event::Window(iced::window::Event::RedrawRequested(now)) = event else {
            return;
        };
        let state = tree.state.downcast_mut::<PreviewMotion>();
        if state.progress >= 1.0 {
            return;
        }
        if crate::motion::reduced() || !layout.bounds().intersects(viewport) {
            state.progress = 1.0;
        } else {
            let start = *state.started.get_or_insert(*now);
            state.progress = (now.saturating_duration_since(start).as_secs_f32() / 0.22).min(1.0);
        }
        if state.progress < 1.0 {
            shell.request_redraw();
        } else {
            state.previous = None;
            state.started = None;
        }
    }
    fn draw(
        &self,
        tree: &iced::advanced::widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let Some(bounds) = layout.bounds().intersection(viewport) else {
            return;
        };
        let state = tree.state.downcast_ref::<PreviewMotion>();
        renderer.with_layer(bounds, |renderer| {
            let mut paint = |handle: &widget::image::Handle, opacity: f32| {
                let image = image(handle.clone())
                    .width(Fill)
                    .height(Fixed(self.height))
                    .content_fit(iced::ContentFit::Cover)
                    .border_radius(9)
                    .opacity(opacity);
                iced::advanced::Widget::<Message, Theme, iced::Renderer>::draw(
                    &image, tree, renderer, theme, style, layout, cursor, viewport,
                );
            };
            if let Some(previous) = &state.previous {
                paint(previous, 1.0);
            }
            if let Some(current) = &state.current {
                paint(current, crate::motion::smoothstep(state.progress));
            }
        });
    }
}
struct LoadingIndicator;

#[derive(Default)]
struct LoadingPhase {
    started: Option<std::time::Instant>,
    turns: f32,
}

impl iced::advanced::Widget<Message, Theme, iced::Renderer> for LoadingIndicator {
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        iced::advanced::widget::tree::Tag::of::<LoadingPhase>()
    }

    fn state(&self) -> iced::advanced::widget::tree::State {
        iced::advanced::widget::tree::State::new(LoadingPhase::default())
    }

    fn size(&self) -> iced::Size<iced::Length> {
        iced::Size::new(Fixed(64.0), Fixed(64.0))
    }

    fn layout(
        &mut self,
        _: &mut iced::advanced::widget::Tree,
        _: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        iced::advanced::layout::Node::new(limits.resolve(
            Fixed(64.0),
            Fixed(64.0),
            iced::Size::ZERO,
        ))
    }

    fn update(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        _: iced::advanced::mouse::Cursor,
        _: &iced::Renderer,
        _: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        let iced::Event::Window(iced::window::Event::RedrawRequested(now)) = event else {
            return;
        };
        if crate::motion::reduced() || !layout.bounds().intersects(viewport) {
            return;
        }
        let phase = tree.state.downcast_mut::<LoadingPhase>();
        let started = *phase.started.get_or_insert(*now);
        phase.turns = (now.saturating_duration_since(started).as_secs_f32() / 1.2).fract();
        shell.request_redraw_at(*now + std::time::Duration::from_millis(16));
    }

    fn draw(
        &self,
        tree: &iced::advanced::widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        _: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        _: iced::advanced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) {
            return;
        }
        let phase = tree.state.downcast_ref::<LoadingPhase>().turns;
        let center = bounds.center();
        let radius = (bounds.width.min(bounds.height) / 2.0 - 6.0).max(0.0);
        renderer.with_layer(*viewport, |renderer| {
            for dot in 0..16 {
                let angle = (phase + dot as f32 / 16.0) * std::f32::consts::TAU
                    - std::f32::consts::FRAC_PI_2;
                let (sin, cos) = angle.sin_cos();
                renderer.fill_quad(
                    iced::advanced::renderer::Quad {
                        bounds: iced::Rectangle::new(
                            iced::Point::new(
                                center.x + radius * cos - 2.5,
                                center.y + radius * sin - 2.5,
                            ),
                            iced::Size::new(5.0, 5.0),
                        ),
                        border: iced::border::rounded(2.5),
                        ..Default::default()
                    },
                    theme
                        .palette()
                        .primary
                        .scale_alpha(0.15 + 0.85 * (dot as f32 / 15.0).powi(2)),
                );
            }
        });
    }
}

fn loading<'a>() -> Element<'a, Message> {
    container(
        column![Element::new(LoadingIndicator), heading("正在载入壁纸")]
            .spacing(18)
            .align_x(Alignment::Center),
    )
    .center_x(Fill)
    .center_y(Fill)
    .padding(35)
    .into()
}

fn badge<'a>(label: impl Into<String>) -> Element<'a, Message> {
    container(text(label.into()).size(10).color(MUTED))
        .padding([4, 9])
        .style(|_| container::Style {
            background: Some(palette::WASH.scale_alpha(0.09).into()),
            border: Border {
                color: LINE,
                width: 1.0,
                radius: 50.0.into(),
            },
            ..Default::default()
        })
        .into()
}
fn empty<'a>(
    title: &str,
    description: &str,
    action: Option<(&str, Message)>,
) -> Element<'a, Message> {
    let mut content = column![
        icon(Icon::Empty).size(48).color(MUTED),
        heading(title.to_owned()),
        muted(description.to_owned())
    ]
    .spacing(14)
    .align_x(Alignment::Center);
    if let Some((label, message)) = action {
        content = content.push(primary(label, message, true));
    }
    container(content)
        .center_x(Fill)
        .center_y(Fill)
        .padding(35)
        .into()
}

pub fn view(app: &App) -> Element<'_, Message> {
    let sidebar = sidebar(app);
    let content = match app.page {
        Page::Library | Page::Discover | Page::Workshop => browser(app),
        Page::Downloads => downloads(app),
        Page::Playlists => playlists(app),
        Page::Displays => displays(app),
        Page::Settings => settings(app),
    };
    let content = if matches!(app.page, Page::Library | Page::Discover | Page::Workshop) {
        content
    } else {
        container(content)
            .padding(iced::Padding {
                bottom: 140.0,
                ..Default::default()
            })
            .into()
    };
    let gallery = container(content)
        .id("gallery-canvas")
        .width(Fill)
        .height(Fill)
        .style(palette::canvas)
        .clip(true);
    let floating_playback = container(widget::opaque(
        container(playback(app)).width(Fill).max_width(960),
    ))
    .align_bottom(Fill)
    .center_x(Fill)
    .padding(iced::Padding {
        top: 0.0,
        left: 18.0,
        right: 18.0,
        bottom: 18.0,
    });
    let mut middle = row![sidebar, widget::stack![gallery, floating_playback]].spacing(10);
    if app.selected.is_some()
        && matches!(
            app.page,
            Page::Library | Page::Discover | Page::Workshop | Page::Playlists
        )
    {
        middle = middle.push(detail(app));
    }
    let body = middle.height(Fill).padding(12);
    let mut layers = widget::Stack::new().width(Fill).height(Fill);
    if let Some(ambient) = &app.ambient {
        layers = layers.push(
            image(ambient.clone())
                .width(Fill)
                .height(Fill)
                .content_fit(iced::ContentFit::Fill),
        );
    }
    layers = layers.push(body);
    if let Some(notice) = &app.notice {
        let toast = row![
            icon(if notice.error {
                Icon::Alert
            } else {
                Icon::Check
            }),
            text(&notice.message).size(13).width(Fill),
            icon_button(Icon::Close, "关闭提示", Some(Message::Dismiss))
        ]
        .spacing(14)
        .align_y(Alignment::Center);
        let toast = container(toast)
            .max_width(420)
            .padding(12)
            .style(move |theme| {
                let mut style = palette::glass(theme);
                if notice.error {
                    style.background = Some(Color::from_rgba(0.25, 0.10, 0.13, 0.94).into());
                }
                style
            });
        let toast = crate::motion::enter(
            toast,
            format!("{:?}", notice.expires_at),
            iced::Vector::new(0.0, -8.0),
        );
        layers = layers.push(
            container(widget::opaque(toast))
                .align_right(Fill)
                .height(Fill)
                .padding(20),
        );
    }
    container(layers)
        .width(Fill)
        .height(Fill)
        .style(|_| container::Style {
            background: Some(BG.into()),
            ..Default::default()
        })
        .into()
}
fn sidebar(app: &App) -> Element<'_, Message> {
    if app.filters_open && matches!(app.page, Page::Library | Page::Discover | Page::Workshop) {
        return crate::motion::enter(filter_panel(app), "filters", iced::Vector::new(-8.0, 0.0));
    }
    let collapsed = app.sidebar_collapsed;
    let mut nav = Column::new().spacing(8);
    if collapsed {
        nav = nav.push(Space::new().height(28));
    } else {
        nav = nav
            .push(
                row![
                    text("M I S A R I").size(18).width(Fill),
                    Space::new().width(32).height(28)
                ]
                .align_y(Alignment::Center),
            )
            .push(Space::new().height(18));
    }
    for (page, glyph) in [
        (Page::Library, Icon::Grid),
        (Page::Discover, Icon::Compass),
        (Page::Workshop, Icon::Workshop),
        (Page::Playlists, Icon::Playlist),
    ] {
        let selected = app.page == page;
        let mut label = row![icon(glyph).width(23)]
            .align_y(Alignment::Center)
            .spacing(7);
        if !collapsed {
            label = label.push(text(page.title()).size(14).width(Fill));
        }
        let control = button(label)
            .width(Fill)
            .padding([12, 12])
            .on_press(Message::Navigate(page))
            .style(move |theme, status| button::Style {
                background: if selected {
                    Some(theme.palette().primary.scale_alpha(0.13).into())
                } else if status == button::Status::Hovered {
                    Some(palette::WASH.scale_alpha(0.07).into())
                } else {
                    None
                },
                text_color: if selected {
                    theme.palette().primary
                } else {
                    MUTED
                },
                border: Border {
                    radius: palette::CONTROL_RADIUS.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
        nav = if collapsed {
            nav.push(
                widget::tooltip(
                    control,
                    text(page.title()).size(12),
                    widget::tooltip::Position::Right,
                )
                .padding(8)
                .style(box_style(CARD)),
            )
        } else {
            nav.push(control)
        };
    }
    nav = nav
        .push(Space::new().height(Fill))
        .push(widget::rule::horizontal(1));
    for (page, glyph) in [
        (Page::Downloads, Icon::Download),
        (Page::Displays, Icon::Monitor),
        (Page::Settings, Icon::Settings),
    ] {
        nav = if collapsed {
            nav.push(icon_button(
                glyph,
                page.title(),
                Some(Message::Navigate(page)),
            ))
        } else {
            nav.push(
                button(
                    row![icon(glyph), text(page.title()).size(13)]
                        .spacing(10)
                        .align_y(Alignment::Center),
                )
                .padding([10, 12])
                .on_press(Message::Navigate(page))
                .width(Fill)
                .style(move |theme, status| {
                    let mut style = filter_quiet_style(theme, status);
                    if app.page == page {
                        style.background = Some(theme.palette().primary.scale_alpha(0.13).into());
                        style.text_color = theme.palette().primary;
                    }
                    style
                }),
            )
        };
    }
    if !collapsed {
        let logged = app.service["account"]["logged_in"] == true;
        nav = nav.push(Space::new().height(15)).push(
            row![
                text("●").size(10).color(MUTED),
                text(if logged {
                    strv(&app.service["account"], "name")
                } else {
                    "连接 Steam"
                })
                .size(12)
            ]
            .spacing(8),
        );
    }
    let pane = container(nav)
        .id("navigation-pane")
        .padding(if collapsed { 8 } else { 14 })
        .width(if collapsed { 60 } else { 200 })
        .height(Fill)
        .style(palette::mica);
    let toggle = container(sidebar_toggle(collapsed)).width(Fill);
    let toggle = if collapsed {
        toggle.center_x(Fill)
    } else {
        toggle.align_right(Fill)
    };
    widget::hover(pane, toggle.padding(if collapsed { 8 } else { 14 }))
}

fn sidebar_toggle<'a>(collapsed: bool) -> Element<'a, Message> {
    widget::tooltip(
        button(
            container(icon(if collapsed {
                Icon::PanelLeftOpen
            } else {
                Icon::PanelLeftClose
            }))
            .center_x(20)
            .center_y(18),
        )
        .id("sidebar-toggle")
        .padding([5, 6])
        .on_press(Message::ToggleSidebar)
        .style(|theme, status| {
            let mut style = filter_quiet_style(theme, status);
            style.border.radius = 6.0.into();
            style
        }),
        text(if collapsed {
            "展开导航"
        } else {
            "折叠导航"
        })
        .size(12),
        widget::tooltip::Position::Right,
    )
    .gap(6)
    .padding(8)
    .delay(std::time::Duration::from_millis(400))
    .style(palette::glass)
    .into()
}

fn filter_quiet_style(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::text(theme, status);
    style.text_color = MUTED;
    style.background = matches!(status, button::Status::Hovered | button::Status::Pressed)
        .then_some(palette::WASH.scale_alpha(0.07).into());
    style.border.radius = palette::CONTROL_RADIUS.into();
    style
}

fn chip_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(
                if selected {
                    theme
                        .palette()
                        .primary
                        .scale_alpha(if hover { 0.16 } else { 0.09 })
                } else {
                    palette::WASH.scale_alpha(if hover { 0.15 } else { 0.055 })
                }
                .into(),
            ),
            text_color: if selected {
                theme.palette().primary
            } else {
                MUTED
            },
            border: Border {
                color: if selected {
                    theme.palette().primary.scale_alpha(0.18)
                } else {
                    palette::WASH.scale_alpha(0.07)
                },
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        }
    }
}

fn filter_panel(app: &App) -> Element<'_, Message> {
    let remote = matches!(app.page, Page::Discover | Page::Workshop);
    let mut sections = Column::new().spacing(24);
    for group in GROUPS {
        let expanded = app.filter_sections.contains(&group);
        let count = app
            .filter_draft
            .selected
            .get(&group)
            .map_or(0, |values| values.len());
        let header = row![
            text(group.title()).size(13).width(Fill),
            text(if count > 0 {
                count.to_string()
            } else {
                String::new()
            })
            .size(11)
            .color(app.theme.palette().primary),
            icon(if expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            })
            .size(14)
        ]
        .spacing(7)
        .align_y(Alignment::Center);
        let header = button(header)
            .width(Fill)
            .padding([6, 2])
            .style(filter_quiet_style)
            .on_press(Message::FilterSection(group));
        let mut section = column![header].spacing(8);
        if expanded {
            if group == Group::Tag {
                section = section.push(
                    text_input("查找标签…", &app.filter_tag_search)
                        .id("filter-tag-search")
                        .size(13)
                        .padding([10, 12])
                        .on_input(Message::FilterTagSearch),
                );
            }
            let query = app.filter_tag_search.trim().to_lowercase();
            let mut options = Row::new().spacing(7);
            let mut found = false;
            for &(key, label) in group.options() {
                if remote && group == Group::Kind && matches!(key, "Image" | "Shader") {
                    continue;
                }
                if group == Group::Tag
                    && !query.is_empty()
                    && !label.to_lowercase().contains(&query)
                    && !key.to_lowercase().contains(&query)
                {
                    continue;
                }
                found = true;
                let selected = app.filter_draft.contains(group, key);
                let mut label_row = Row::new().spacing(5).align_y(Alignment::Center);
                if selected {
                    label_row = label_row.push(icon(Icon::Check).size(12));
                }
                label_row = label_row.push(text(label).size(13));
                options = options.push(
                    button(label_row)
                        .padding([5, 12])
                        .style(chip_style(selected))
                        .on_press(Message::EditFilter(group, key.into(), !selected))
                        .id(format!("filter-{group:?}-{key}")),
                );
            }
            if !found {
                section = section.push(muted("没有匹配的标签"));
            } else {
                section = section.push(options.wrap());
            }
        }
        sections = sections.push(section);
    }
    let mut footer = Column::new().spacing(10);
    if app.page == Page::Discover
        && app
            .filter_draft
            .tag_groups()
            .iter()
            .any(|group| group.len() > 1)
    {
        footer = footer.push(muted("多选组合需要连接 Steam"));
    }
    footer = footer.push(
        row![
            secondary("重置", Message::ResetFilters)
                .padding([7, 14])
                .style(filter_quiet_style),
            Space::new().width(Fill),
            primary("应用", Message::ApplyFilters, true)
                .padding([7, 14])
                .id("apply-filters")
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    );
    let back = button(icon(Icon::ChevronLeft).size(20))
        .padding([6, 7])
        .style(filter_quiet_style)
        .on_press(Message::ShowNavigation);
    container(
        column![
            row![back, text("筛选").size(16)]
                .spacing(6)
                .align_y(Alignment::Center),
            scrollable(sections).id("filter-panel").height(Fill),
            container(footer).padding(iced::Padding {
                top: 6.0,
                ..Default::default()
            })
        ]
        .spacing(18),
    )
    .padding(iced::Padding {
        top: 16.0,
        right: 16.0,
        bottom: 28.0,
        left: 16.0,
    })
    .id("navigation-pane")
    .width(260)
    .height(Fill)
    .style(palette::mica)
    .into()
}

fn browser(app: &App) -> Element<'_, Message> {
    let remote = matches!(app.page, Page::Discover | Page::Workshop);
    let values = if remote {
        app.remote.iter().collect::<Vec<_>>()
    } else {
        app.filtered()
    };
    let count = values.len();
    let page_size = app.page_size;
    let pages = if remote {
        app.remote_pages
    } else {
        count.div_ceil(page_size)
    };
    let page = if remote {
        app.displayed_remote_page.saturating_sub(1)
    } else {
        app.page_index.min(pages.saturating_sub(1))
    };
    let title = row![
        text(app.page.title()).size(28).width(Fill),
        icon_button(Icon::Refresh, "刷新", Some(Message::Refresh)),
        secondary("导入文件", Message::Import(false)),
        secondary("导入项目", Message::Import(true))
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let active_count = app.filters.count();
    let controls = container(widget::responsive(move |size| {
        let search = text_input(
            if remote {
                "搜索创意工坊，按 Enter 搜索"
            } else {
                "搜索我的壁纸…  Ctrl F"
            },
            &app.search,
        )
        .id("search")
        .padding(12)
        .on_input(Message::Search)
        .on_submit(Message::SearchNow);
        let sorts = if remote {
            vec!["热门", "最新", "评分最高", "订阅最多"]
        } else {
            vec!["名称 A–Z", "名称 Z–A", "类型"]
        }
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
        let filter_button = button(
            text(if active_count == 0 {
                "筛选".to_owned()
            } else {
                format!("筛选 · {active_count}")
            })
            .size(13),
        )
        .padding([12, 14])
        .on_press(Message::ToggleFilters)
        .id("toggle-filters")
        .style(move |theme, status| {
            let mut style = secondary_style(theme, status);
            if app.filters_open {
                style.background = Some(theme.palette().primary.scale_alpha(0.16).into());
                style.text_color = theme.palette().primary;
            }
            style
        });
        let mut controls = Row::new().spacing(8).align_y(Alignment::Center);
        if app.page == Page::Workshop {
            controls = controls.push(
                pick_list(
                    vec!["全部作品".to_owned(), "已订阅".into(), "Steam 收藏".into()],
                    Some(app.scope.clone()),
                    Message::Scope,
                )
                .padding(11)
                .width(112),
            );
        }
        controls = controls.push(search).push(filter_button);
        if remote {
            controls = controls.push(primary("搜索", Message::SearchNow, !app.service_busy));
        }
        let pagination = row![
            icon_button(
                Icon::ArrowLeft,
                "上一页",
                (page > 0 && !app.loading && !app.service_busy && !app.remote_loading())
                    .then_some(Message::Page(-1))
            ),
            container(muted(format!("{} / {}", page + 1, pages.max(1)))).center_x(76),
            icon_button(
                Icon::ArrowRight,
                "下一页",
                (page + 1 < pages && !app.loading && !app.service_busy && !app.remote_loading())
                    .then_some(Message::Page(1))
            )
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        controls = controls
            .push(
                pick_list(sorts, Some(app.sort.clone()), Message::Sort)
                    .padding(11)
                    .width(112),
            )
            .push(pagination);
        let minimum_width = if app.page == Page::Workshop {
            760.0
        } else if remote {
            640.0
        } else {
            560.0
        };
        widget::scrollable(controls.width(size.width.max(minimum_width)))
            .direction(widget::scrollable::Direction::Horizontal(
                widget::scrollable::Scrollbar::hidden(),
            ))
            .id("browser-toolbar")
            .width(Fill)
            .height(Fill)
            .into()
    }))
    .height(46);
    let mut body = column![title, controls].spacing(16);
    if active_count > 0 {
        let mut chips = Row::new().spacing(6);
        for (&group, values) in &app.filters.selected {
            for value in values {
                chips = chips.push(
                    button(
                        row![
                            text(group.label(value)).size(13),
                            icon(Icon::Close).size(12)
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center),
                    )
                    .padding([5, 12])
                    .style(chip_style(true))
                    .on_press(Message::RemoveFilter(group, value.clone())),
                );
            }
        }
        chips = chips.push(
            button(text("清除全部").size(12))
                .padding([5, 8])
                .style(filter_quiet_style)
                .on_press(Message::ClearFilters),
        );
        body = body.push(chips.wrap());
    }
    if app.page == Page::Discover
        && app.filters.count() == 0
        && app.search.trim().is_empty()
        && !app.recommendations.is_empty()
    {
        let mut recommendations = Row::new().spacing(12);
        for item in app.recommendations.iter().take(3) {
            recommendations = recommendations.push(
                button(column![
                    preview(app, item, 76.0, true),
                    container(text(truncate(&app.local_title(item), 24)).size(12)).padding([7, 10])
                ])
                .padding(0)
                .width(Fill)
                .on_press(Message::Select(strv(item, "id").into()))
                .card(strv(item, "id"), false)
                .style(card_style(false)),
            );
        }
        body = body
            .push(column![muted("本期精选 · Wallpaper Engine 推荐"), recommendations].spacing(8));
    }
    if app.loading || (remote && app.remote_loading() && app.remote.is_empty()) {
        return container(body.push(loading()))
            .padding(28)
            .height(Fill)
            .into();
    }
    if count == 0 {
        let (title, desc) = if remote {
            (
                "暂时没有可显示的作品",
                "检查 Steam 连接，或换一个关键词重新搜索。",
            )
        } else if app.catalog.is_empty() {
            (
                "从第一张壁纸开始",
                "会自动找到 Steam 中已下载的壁纸，也可以导入本地项目。",
            )
        } else {
            ("没有找到匹配的壁纸", "试试移除部分筛选条件，或更换关键词。")
        };
        return container(body.push(empty(
            title,
            desc,
            Some((
                if active_count > 0 {
                    "清除筛选"
                } else if remote {
                    "重试搜索"
                } else {
                    "刷新资源库"
                },
                if active_count > 0 {
                    Message::ClearFilters
                } else if remote {
                    Message::SearchNow
                } else {
                    Message::Refresh
                },
            )),
        )))
        .padding(28)
        .height(Fill)
        .into();
    }
    let items: Vec<_> = if remote {
        values
    } else {
        values
            .into_iter()
            .skip(page * page_size)
            .take(page_size)
            .collect()
    };
    let grid = widget::responsive(move |size| {
        let cols = grid_columns(size.width);
        let mut grid = Column::new().spacing(22);
        for chunk in items.chunks(cols) {
            let mut line = Row::new().spacing(16);
            for item in chunk {
                line = line.push(wallpaper_card(app, item));
            }
            for _ in chunk.len()..cols {
                line = line.push(Space::new().width(Fill));
            }
            grid = grid.push(line);
        }
        scrollable(grid.padding(iced::Padding {
            bottom: 148.0,
            ..Default::default()
        }))
        .id("wallpaper-grid")
        .on_scroll(|viewport| {
            Message::GridScrolled((viewport.absolute_offset().y / 242.0) as usize)
        })
        .height(Fill)
        .into()
    });
    body = body.push(
        widget::sensor(grid)
            .on_show(Message::GridResized)
            .on_resize(Message::GridResized),
    );
    container(body).padding(28).height(Fill).into()
}
pub fn grid_columns(width: f32) -> usize {
    let columns = ((width + 16.0) / (220.0 + 16.0)).floor();
    columns.max(1.0) as usize
}
pub fn grid_page_size(size: iced::Size) -> usize {
    // Include a partial bottom row so the grid fills the available height.
    let rows = (size.height / 242.0).ceil().max(1.0) as usize;
    grid_columns(size.width) * rows
}
fn wallpaper_card<'a>(app: &'a App, item: &'a Value) -> Element<'a, Message> {
    let id = strv(item, "id");
    let selected = app.selected.as_deref() == Some(id);
    let title = app.local_title(item);
    let remote = !id.starts_with("we:") && !id.starts_with("local:");
    let status = if remote {
        if item["downloaded"] == true {
            ""
        } else if item["subscribed"] == true {
            "已订阅"
        } else {
            "创意工坊"
        }
    } else if app.state["outputs"]
        .as_object()
        .is_some_and(|m| m.values().any(|v| v["current"] == id))
    {
        "正在使用"
    } else if app.favorite(id) {
        "已收藏"
    } else {
        ""
    };
    let info = column![
        row![
            single_line_title(title, 13),
            muted(kind_label(strv(item, "kind")))
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .height(22),
        text(status)
            .size(10)
            .height(14)
            .color(if status == "正在使用" {
                app.theme.palette().primary
            } else {
                MUTED
            })
    ]
    .spacing(4)
    .padding([0, 3]);
    let mut thumbnail = widget::stack![preview(app, item, 156.0, true)];
    if selected {
        let accent = app.theme.palette().primary;
        thumbnail = thumbnail.push(
            container(
                container(icon(Icon::Check).size(12).color(BG))
                    .center_x(22)
                    .center_y(22)
                    .style(move |_| container::Style {
                        background: Some(accent.into()),
                        border: iced::border::rounded(11),
                        ..Default::default()
                    }),
            )
            .align_right(Fill)
            .height(Fill)
            .padding(9),
        );
    }
    button(column![thumbnail, info].spacing(10))
        .padding(5)
        .width(Fill)
        .on_press(Message::Select(id.into()))
        .card(id, selected)
        .style(card_style(selected))
        .into()
}
fn size_label(app: &App, item: &Value) -> (String, &'static str) {
    if let Some(local) = app.selected_local_asset() {
        let label = match app.asset_sizes.get(strv(local, "path")) {
            Some(crate::AssetSize::Loaded(Some(size))) => bytes(*size),
            Some(crate::AssetSize::Loaded(None)) => "—".into(),
            _ => "…".into(),
        };
        (label, "本地大小")
    } else {
        let size = item["size"]
            .as_u64()
            .or_else(|| item["size"].as_str()?.parse::<u64>().ok());
        (
            size.filter(|size| *size > 0)
                .map(bytes)
                .unwrap_or_else(|| "—".into()),
            "下载大小",
        )
    }
}
fn detail(app: &App) -> Element<'_, Message> {
    let Some(item) = app.selected_asset() else {
        return container(empty(
            "项目已不在列表中",
            "刷新资源库后重新选择。",
            Some(("关闭", Message::CloseDetail)),
        ))
        .width(340)
        .into();
    };
    let id = strv(item, "id");
    let remote = app.is_remote();
    let deleting = app
        .active_command
        .as_ref()
        .is_some_and(|(method, params)| *method == "trash_wallpaper" && params["asset_id"] == id);
    let (size, size_description) = size_label(app, item);
    let mut content = column![
        row![
            muted(if remote {
                "创意工坊详情"
            } else {
                "壁纸详情"
            }),
            Space::new().width(Fill),
            icon_button(Icon::Close, "关闭详情", Some(Message::CloseDetail))
        ],
        preview(app, item, 172.0, false),
        text(app.local_title(item))
            .size(19)
            .width(Fill)
            .wrapping(widget::text::Wrapping::WordOrGlyph),
        row![
            widget::tooltip(
                badge(format!("{} · {size}", kind_label(strv(item, "kind")))),
                text(size_description).size(12),
                widget::tooltip::Position::Top,
            )
            .gap(6)
            .padding(8)
            .delay(std::time::Duration::from_millis(400))
            .style(box_style(CARD)),
            Space::new().width(Fill),
            muted(if remote {
                "STEAM WORKSHOP"
            } else {
                "本地项目"
            })
        ]
    ]
    .spacing(18);
    if app.delete_confirmation.as_deref() == Some(id) || deleting {
        content = content
            .push(heading(if deleting { "正在删除…" } else { "删除这张壁纸？" }))
            .push(muted(if crate::engine::workshop_id(std::path::Path::new(strv(item, "path"))).is_some() {
                "项目目录将移到系统回收站，并取消 Steam 订阅。正在播放这张壁纸的显示器会停止播放。"
            } else {
                "壁纸文件或项目目录将移到系统回收站。正在播放这张壁纸的显示器会停止播放。"
            }))
            .push(text(strv(item, "path")).size(12));
        if !deleting {
            content = content.push(
                row![
                    secondary("取消", Message::CancelDeleteWallpaper),
                    primary("确认删除", Message::ConfirmDeleteWallpaper, !app.busy)
                        .style(widget::button::danger)
                ]
                .spacing(10),
            );
        }
        return container(scrollable(content).id("wallpaper-detail").height(Fill))
            .id("detail-pane")
            .padding(22)
            .width(340)
            .height(Fill)
            .style(palette::mica)
            .into();
    }
    if remote {
        let subscribed = item["subscribed"].as_bool().unwrap_or(false);
        let favorite = item["steam_favorite"].as_bool().unwrap_or(false);
        content = content.push(
            row![
                badge(if item["downloaded"] == true {
                    "已下载"
                } else {
                    "未下载"
                }),
                badge(if subscribed { "已订阅" } else { "未订阅" })
            ]
            .spacing(8),
        );
        if item["downloaded"] == true {
            if let Some(local) = app.catalog.iter().find(|v| {
                std::path::Path::new(strv(v, "path"))
                    .file_name()
                    .and_then(|v| v.to_str())
                    == Some(id)
            }) {
                content = content.push(
                    primary(
                        "打开本地壁纸并应用",
                        Message::Select(strv(local, "id").into()),
                        true,
                    )
                    .width(Fill),
                );
            } else {
                content = content.push(secondary("刷新本地资源库", Message::RefreshLibrary));
            }
        }
        if item["needs_update"] == true {
            content = content.push(badge("有可用更新"));
        }
        content = content.push(
            primary(
                if subscribed {
                    "取消 Steam 订阅"
                } else {
                    "订阅到 Steam"
                },
                Message::Service(json!({"action":"subscribe","id":id,"value":!subscribed})),
                !app.service_busy && app.service["account"]["logged_in"] == true,
            )
            .width(Fill),
        );
        content = content.push(
            row![
                primary(
                    "下载 / 更新",
                    Message::Service(
                        json!({"action":"enqueue","id":id,"title":app.local_title(item)})
                    ),
                    !app.service_busy
                )
                .width(Fill),
                secondary(
                    if favorite {
                        "已在 Steam 收藏"
                    } else {
                        "Steam 收藏"
                    },
                    Message::Service(json!({"action":"favorite","id":id,"value":!favorite}))
                )
            ]
            .spacing(8),
        );
        content = content.push(
            secondary(
                "在 Steam 中查看",
                Message::Open(format!("steam://url/CommunityFilePage/{id}")),
            )
            .width(Fill),
        );
        let description = strv(item, "description");
        content = content.push(muted(if description.is_empty() {
            "该作品暂无描述。作者提供的项目属性将在下载后显示。"
        } else {
            description
        }));
        return container(scrollable(content).id("wallpaper-detail").height(Fill))
            .id("detail-pane")
            .padding(22)
            .width(340)
            .height(Fill)
            .style(palette::mica)
            .into();
    }
    if let Some(error) = item["error"]["message"].as_str() {
        content = content.push(
            text(format!("兼容提示：{error}"))
                .size(12)
                .color(Color::from_rgb8(231, 181, 111)),
        );
    }
    let outputs = std::iter::once("全部显示器".to_owned())
        .chain(
            app.state["outputs"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys().cloned()),
        )
        .collect::<Vec<_>>();
    if matches!(strv(item, "kind"), "we_application" | "application")
        && app.state["backends"]["application"]["available"] != true
    {
        content = content
            .push(
                text(
                    app.state["backends"]["application"]["reason"]
                        .as_str()
                        .unwrap_or("需要启用应用兼容后端。"),
                )
                .size(12)
                .color(Color::from_rgb8(231, 181, 111)),
            )
            .push(secondary("打开兼容设置", Message::Navigate(Page::Settings)));
    }
    content = content.push(muted("应用到")).push(
        pick_list(outputs, Some(app.output.clone()), Message::Output)
            .padding(10)
            .width(Fill),
    );
    content = content.push(
        row![
            muted("缩放方式"),
            Space::new().width(Fill),
            pick_list(
                vec!["填充".to_owned(), "适应".into(), "拉伸".into()],
                Some(app.fit.clone()),
                Message::Fit
            )
            .padding(10)
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    );
    content = content.push(
        primary(
            if app.busy {
                "正在应用…"
            } else {
                "应用壁纸"
            },
            Message::Apply,
            !app.busy
                && item["error"].is_null()
                && (!matches!(strv(item, "kind"), "we_application" | "application")
                    || app.state["backends"]["application"]["available"] == true)
                && app.state["outputs"]
                    .as_object()
                    .is_some_and(|m| !m.is_empty()),
        )
        .width(Fill),
    );
    content = content.push(
        row![
            icon_action(
                Icon::Star,
                if app.favorite(id) {
                    "取消本地收藏"
                } else {
                    "本地收藏"
                },
                Some(Message::Favorite),
                app.favorite(id)
            ),
            icon_button(
                Icon::Folder,
                "定位文件",
                Some(Message::Open(strv(item, "path").into()))
            ),
            Space::new().width(Fill),
            icon_button(
                Icon::Trash,
                "删除壁纸",
                (!app.busy && !app.loading).then_some(Message::DeleteWallpaper)
            )
        ]
        .spacing(8),
    );
    if let Some(id) = std::path::Path::new(strv(item, "path"))
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|v| v.bytes().all(|b| b.is_ascii_digit()))
    {
        content = content.push(
            secondary(
                "查看 Steam 页面",
                Message::Open(format!("steam://url/CommunityFilePage/{id}")),
            )
            .width(Fill),
        );
    }
    if let Some(lists) = app.library["playlists"]
        .as_object()
        .filter(|v| !v.is_empty())
    {
        let options = lists
            .iter()
            .map(|(id, v)| Choice {
                id: id.clone(),
                label: strv(v, "name").into(),
                value: Value::Null,
            })
            .collect::<Vec<_>>();
        let current = options.iter().find(|v| v.id == app.playlist).cloned();
        content = content
            .push(widget::rule::horizontal(1))
            .push(muted("加入播放列表"))
            .push(
                row![
                    pick_list(options, current, |v| Message::PlaylistSelect(v.id))
                        .padding(9)
                        .width(Fill),
                    icon_button(Icon::Plus, "加入播放列表", Some(Message::PlaylistAdd))
                ]
                .spacing(6),
            );
        if array(&app.library["playlists"][&app.playlist]["members"])
            .iter()
            .any(|v| v.as_str() == Some(strv(item, "id")))
        {
            content = content.push(
                secondary("将当前属性保存到此列表", Message::PlaylistSaveProperties).width(Fill),
            );
        }
    }
    content = content.push(widget::rule::horizontal(1)).push(row![
        heading("项目属性"),
        Space::new().width(Fill),
        button("重置")
            .on_press_maybe(
                (!app.busy
                    && app.can_edit_properties()
                    && !array(&app.properties["definitions"]).is_empty())
                .then_some(Message::ResetProperties)
            )
            .style(button::text)
    ]);
    let defs = array(&app.properties["definitions"]);
    if defs.is_empty() {
        content = content.push(muted(
            if id.starts_with("we:") && app.properties.is_null() {
                "正在读取项目属性…"
            } else {
                "这个项目没有可编辑的属性。音量和播放控制在窗口底部。"
            },
        ));
    } else {
        content = content.push(muted(if app.can_edit_properties() {
            "修改会自动保存，支持的项目即时生效。"
        } else {
            "请先将此壁纸应用到所选显示器，再调整实时属性。"
        }));
        for def in defs {
            let key = strv(def, "key");
            if app.prop_state()["visible"][key] == false {
                continue;
            }
            content = content.push(property(app, def));
        }
        content = content
            .push(widget::rule::horizontal(1))
            .push(heading("属性预设"))
            .push(
                row![
                    text_input("预设名称", &app.preset_name)
                        .on_input(Message::PresetName)
                        .padding(9),
                    primary(
                        "保存",
                        Message::SavePreset,
                        !app.busy && !app.preset_name.trim().is_empty()
                    )
                ]
                .spacing(6),
            );
        if let Some(presets) = app.properties["presets"].as_object() {
            for name in presets.keys() {
                content = content.push(
                    row![
                        button(text(name).size(13))
                            .padding([9, 13])
                            .on_press_maybe(
                                app.can_edit_properties()
                                    .then_some(Message::LoadPreset(name.clone()))
                            )
                            .style(secondary_style)
                            .width(Fill),
                        button("删除")
                            .on_press(Message::DeletePreset(name.clone()))
                            .style(button::text)
                    ]
                    .spacing(6),
                );
            }
        }
    }
    container(scrollable(content).id("wallpaper-detail").height(Fill))
        .id("detail-pane")
        .padding(22)
        .width(340)
        .height(Fill)
        .style(palette::mica)
        .into()
}
#[derive(Clone, Debug, PartialEq)]
struct Choice {
    id: String,
    label: String,
    value: Value,
}
impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}
fn property<'a>(app: &'a App, def: &'a Value) -> Element<'a, Message> {
    let key = strv(def, "key").to_owned();
    let kind = strv(def, "type");
    let label = property_label(
        &app.properties["localization"],
        def["text"].as_str().unwrap_or(&key),
    );
    let value = app.property_value(&key);
    if kind == "group" {
        return container(text(label).size(14).color(TEXT))
            .padding(iced::Padding {
                top: 12.0,
                ..Default::default()
            })
            .into();
    }
    if kind == "text" {
        return muted(truncate(&label, 160)).into();
    }
    if def["editable"] != true {
        return column![
            muted(label),
            muted(
                def["error"]
                    .as_str()
                    .unwrap_or("暂不支持此属性，原始定义会保留。")
            )
        ]
        .spacing(4)
        .into();
    }
    if kind == "color" {
        return crate::color_picker::view(
            key.clone(),
            label,
            &value,
            app.color_editor.as_ref().filter(|editor| editor.key == key),
            app.can_edit_properties(),
        );
    }
    if !app.can_edit_properties() {
        let display = match kind {
            "combo" => array(&def["options"])
                .iter()
                .find(|option| option["value"] == value)
                .map(|option| {
                    property_label(&app.properties["localization"], strv(option, "label"))
                })
                .unwrap_or_else(|| "未选择".into()),
            "bool" => if value == true {
                "已开启"
            } else {
                "已关闭"
            }
            .into(),
            _ => value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string()),
        };
        return column![
            muted(label),
            text(truncate(&display, 60)).size(12).color(MUTED)
        ]
        .spacing(6)
        .into();
    }
    if kind == "bool" {
        let k = key.clone();
        return checkbox(value.as_bool().unwrap_or(false))
            .label(label)
            .text_size(13)
            .on_toggle(move |v| Message::Property(k.clone(), json!(v)))
            .into();
    }
    let mut field = column![text(label).size(12)].spacing(8);
    match kind {
        "slider" => {
            let min = def["min"].as_f64().unwrap_or(0.0) as f32;
            let max = def["max"].as_f64().unwrap_or(100.0) as f32;
            let v = value.as_f64().unwrap_or(min as f64) as f32;
            let k = key.clone();
            let step =
                def["step"]
                    .as_f64()
                    .unwrap_or(if def["fraction"] == true { 0.01 } else { 1.0 })
                    as f32;
            field = field.push(
                row![
                    slider(min..=max, v, move |v| Message::Property(
                        k.clone(),
                        json!(v)
                    ))
                    .step(step.max(0.0001))
                    .width(Fill),
                    text(format!("{v:.2}")).size(11).width(45)
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        }
        "combo" => {
            let choices = array(&def["options"])
                .iter()
                .enumerate()
                .map(|(i, o)| Choice {
                    id: i.to_string(),
                    label: property_label(
                        &app.properties["localization"],
                        o["label"]
                            .as_str()
                            .unwrap_or_else(|| o["value"].as_str().unwrap_or("选项")),
                    ),
                    value: o["value"].clone(),
                })
                .collect::<Vec<_>>();
            let selected = choices.iter().find(|c| c.value == value).cloned();
            let k = key.clone();
            field = field.push(
                pick_list(choices, selected, move |v| {
                    Message::Property(k.clone(), v.value)
                })
                .padding(8)
                .width(Fill),
            );
        }
        _ => {
            let k = key.clone();
            let input = text_input("", app.drafts.get(&key).map(String::as_str).unwrap_or(""))
                .on_input(move |v| Message::PropertyText(k.clone(), v))
                .on_submit(Message::CommitProperty(key.clone()))
                .padding(8)
                .size(12);
            let mut line = row![input];
            if matches!(kind, "file" | "directory") {
                line = line.push(secondary(
                    "选择",
                    Message::PickProperty(key.clone(), kind == "directory"),
                ));
            } else {
                line = line.push(secondary("保存", Message::CommitProperty(key.clone())));
            }
            field = field.push(line.spacing(6));
        }
    }
    field.into()
}
fn property_label(localization: &Value, key: &str) -> String {
    for language in [
        "zh-chs", "zh-cn", "zh-CN", "schinese", "en-us", "en-US", "english",
    ] {
        if let Some(label) = localization[language][key]
            .as_str()
            .filter(|label| !label.trim().is_empty() && *label != key)
        {
            return plain_label(label);
        }
    }
    plain_label(key)
}
fn plain_label(s: &str) -> String {
    if let Some(label) = match s {
        "ui_browse_properties_scheme_color" => Some("配色"),
        "ui_browse_properties_audio_volume" => Some("音量"),
        "ui_browse_properties_playbackrate" => Some("播放速度"),
        "ui_boot" => Some("启动时播放开场动画"),
        "ui_language" => Some("界面语言"),
        "ui_language_0" => Some("简体中文"),
        "ui_language_1" => Some("English"),
        "ui_desktopmode" => Some("桌面模式"),
        _ => None,
    } {
        return label.into();
    }

    let mut result = String::new();
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            _ if !tag => result.push(c),
            _ => {}
        }
    }
    let result = result
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .trim()
        .to_owned();
    if let Some(key) = result.strip_prefix("ui_") {
        if let Some(index) = key
            .rsplit('_')
            .next()
            .and_then(|index| index.parse::<usize>().ok())
        {
            return format!("选项 {}", index.saturating_add(1));
        }
        return "自定义设置".into();
    }
    // Many projects author bilingual headings. Keep the Chinese side compact.
    if let Some((chinese, _)) = result.split_once(" | ")
        && chinese
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    {
        return chinese.trim().to_owned();
    }
    result
}
fn playback(app: &App) -> Element<'_, Message> {
    widget::responsive(move |bounds| {
        let mut content = Column::new().spacing(8);
        if app.queue_open {
            content = content.push(
                container(container(playback_queue(app)).width(Fill).max_width(520))
                    .align_right(Fill),
            );
        }
        content.push(playback_bar(app, bounds.width < 820.0)).into()
    })
    .height(Shrink)
    .into()
}

fn playback_queue(app: &App) -> Element<'_, Message> {
    let targets = app.playback_targets();
    let mut content = column![
        row![
            heading("当前播放范围"),
            Space::new().width(Fill),
            icon_button(Icon::Close, "关闭播放队列", Some(Message::ToggleQueue))
        ]
        .align_y(Alignment::Center)
    ]
    .spacing(12);
    if app.playback_mixed() {
        let mut outputs = Column::new().spacing(8);
        for name in targets {
            let current = strv(&app.state["outputs"][&name], "current");
            let title = app
                .catalog
                .iter()
                .find(|item| strv(item, "id") == current)
                .map(|item| app.local_title(item))
                .unwrap_or_else(|| "未播放".into());
            outputs = outputs
                .push(secondary(format!("{name} · {title}"), Message::Output(name)).width(Fill));
        }
        return container(
            content
                .push(muted("选择显示器以查看它的播放范围与设置"))
                .push(scrollable(outputs).height(180)),
        )
        .padding(18)
        .style(palette::popover)
        .into();
    }
    let name = targets.first().map(String::as_str).unwrap_or("");
    let settings = app.playback_settings(name);
    let source = app.playback_source();
    let members = app.playback_members(name);
    let current = strv(app.current_output(), "current");
    let mut queue = Column::new().spacing(4);
    for (index, id) in members.iter().enumerate() {
        let item = app.catalog.iter().find(|item| strv(item, "id") == id);
        let title = item
            .map(|item| app.local_title(item))
            .unwrap_or_else(|| id.clone());
        let active = id == current;
        queue = queue.push(
            button(
                row![
                    if active {
                        icon(if app.current_output()["paused"] == true {
                            Icon::Pause
                        } else {
                            Icon::Play
                        })
                    } else {
                        icon(Icon::Empty).color(Color::TRANSPARENT)
                    },
                    muted(format!("{:02}", index + 1)),
                    single_line_title(title, 13)
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .padding(9)
            .width(Fill)
            .style(card_style(active))
            .on_press_maybe((!app.busy).then_some(Message::PlayQueued(id.clone()))),
        );
    }
    if members.is_empty() {
        queue = queue.push(muted("当前为单张壁纸，可加入播放列表后继续切换。"));
    }
    content = content.push(scrollable(queue).height(180));
    if !members.is_empty() {
        let minutes = settings["interval_seconds"].as_u64().unwrap_or(1800) / 60;
        let mut intervals = vec![1, 5, 15, 30, 60, 120];
        if !intervals.contains(&minutes) {
            intervals.push(minutes);
            intervals.sort_unstable();
        }
        content = content.push(
            row![
                checkbox(settings["enabled"] == true)
                    .label("自动轮换")
                    .on_toggle(Message::AutoRotation)
                    .text_size(12),
                Space::new().width(Fill),
                muted("每隔"),
                pick_list(intervals, Some(minutes), Message::PlaybackInterval)
                    .padding(6)
                    .text_size(12),
                muted("分钟")
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    if !current.is_empty() && strv(app.current_output(), "backend") != "application" {
        content = content.push(
            row![
                muted("音量"),
                slider(
                    0..=100,
                    app.current_output()["volume"].as_u64().unwrap_or(100) as u32,
                    |volume| Message::Playback(json!({"volume":volume}))
                )
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
    }
    content = content.push(
        row![
            secondary(
                if app.library["playlists"][source].is_object() {
                    "管理列表"
                } else {
                    "查看资源库"
                },
                Message::ShowPlaybackSource
            ),
            Space::new().width(Fill),
            icon_button(
                Icon::Stop,
                "停止播放",
                (!current.is_empty() && !app.busy).then_some(Message::Stop)
            )
        ]
        .align_y(Alignment::Center),
    );
    container(content)
        .id("playback-queue")
        .padding(18)
        .style(palette::popover)
        .into()
}

fn playback_bar(app: &App, compact: bool) -> Element<'_, Message> {
    let output = app.current_output();
    let mixed = app.playback_mixed();
    let current = strv(output, "current");
    let item = app
        .catalog
        .iter()
        .find(|v| !mixed && strv(v, "id") == current);
    let title = if mixed {
        "多显示器 · 状态不同".into()
    } else {
        item.map(|v| app.local_title(v)).unwrap_or_else(|| {
            if current.is_empty() {
                "桌面已就绪".into()
            } else {
                "当前壁纸".into()
            }
        })
    };
    let targets = app.playback_targets();
    let name = targets.first().map(String::as_str).unwrap_or("");
    let settings = app.playback_settings(name);
    let members = app.playback_members(name);
    let has_current = targets.iter().any(|name| {
        app.state["outputs"][name]["current"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    });
    let paused = targets
        .iter()
        .all(|name| app.state["outputs"][name]["paused"] == true);
    let reasons = array(&output["pause_reasons"])
        .iter()
        .filter_map(Value::as_str)
        .map(|r| match r {
            "session_locked" => "锁屏时暂停",
            "sleep" => "休眠时暂停",
            "session_inactive" => "会话不活跃",
            "fullscreen" => "全屏应用时暂停",
            "battery" => "电池供电时暂停",
            "user" => "手动暂停",
            "playback_error" => "播放遇到错误",
            _ => "性能策略暂停",
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let status = if !reasons.is_empty() {
        reasons
    } else if current.is_empty() {
        "选择一张壁纸开始".into()
    } else {
        "正在播放".into()
    };
    let mut info = Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .width(Fill);
    if let Some(item) = item {
        info = info.push(container(preview(app, item, 40.0, true)).width(60));
    }
    let source = app.playback_source();
    let source_title = match source {
        "library" => "资源库",
        "favorites" => "收藏",
        "single" => "单张壁纸",
        source => app.library["playlists"][source]["name"]
            .as_str()
            .unwrap_or("单张壁纸"),
    };
    let position = members
        .iter()
        .position(|id| id == current)
        .map(|i| format!(" · {}/{}", i + 1, members.len()))
        .unwrap_or_default();
    let summary = if mixed {
        "点击查看各显示器".into()
    } else if current.is_empty() {
        status.clone()
    } else {
        format!(
            "{source_title}{position}{}{}",
            if settings["enabled"] == true {
                " · 自动轮换"
            } else {
                ""
            },
            if output["effective_paused"] == true || paused {
                format!(" · {status}")
            } else {
                String::new()
            }
        )
    };
    info = info.push(
        column![
            single_line_title(title, 13),
            button(single_line_title(summary, 11))
                .padding(0)
                .style(button::text)
                .on_press(if mixed {
                    Message::ToggleQueue
                } else {
                    Message::ShowPlaybackSource
                })
        ]
        .spacing(5)
        .width(Fill),
    );
    let toggle = widget::tooltip(
        button(
            container(icon(if paused { Icon::Play } else { Icon::Pause }))
                .center_x(18)
                .center_y(18),
        )
        .padding(11)
        .id("playback-toggle")
        .on_press_maybe(
            (has_current && !app.busy).then_some(Message::Playback(json!({"paused":!paused}))),
        )
        .style(|theme, status| {
            let mut style = secondary_style(theme, status);
            let disabled = status == button::Status::Disabled;
            style.background = Some(
                theme
                    .palette()
                    .primary
                    .scale_alpha(if disabled {
                        0.08
                    } else if status == button::Status::Hovered {
                        0.95
                    } else {
                        0.82
                    })
                    .into(),
            );
            style.text_color = if disabled { MUTED } else { BG };
            style.border = iced::border::rounded(50);
            style
        }),
        text(if paused { "继续播放" } else { "暂停" }).size(12),
        widget::tooltip::Position::Top,
    )
    .padding(8)
    .style(palette::glass);
    let can_step = !app.busy
        && targets.iter().any(|name| {
            app.state["outputs"][name]["current"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
                && !app.playback_members(name).is_empty()
        });
    let can_previous = can_step
        && targets
            .iter()
            .any(|name| app.state["rotations"][name]["can_previous"] != false);
    let mut controls = row![
        icon_button(
            Icon::ChevronLeft,
            "上一张",
            can_previous.then_some(Message::Previous)
        ),
        toggle,
        icon_button(Icon::Next, "下一张", can_step.then_some(Message::Next)),
        icon_button(
            Icon::Playlist,
            "当前播放队列与轮换设置",
            Some(Message::ToggleQueue)
        )
    ]
    .spacing(if compact { 4 } else { 8 })
    .align_y(Alignment::Center);
    if can_step && !mixed {
        controls = controls.push(
            pick_list(
                vec!["顺序切换".to_owned(), "随机挑选".to_owned()],
                (!mixed).then(|| {
                    if settings["mode"] == "random" {
                        "随机挑选".into()
                    } else {
                        "顺序切换".into()
                    }
                }),
                Message::PlaybackMode,
            )
            .padding(7)
            .text_size(12)
            .width(100),
        );
    }
    let audio_available = strv(output, "backend") != "application";
    let muted = targets
        .iter()
        .all(|name| app.state["outputs"][name]["mute"] == true);
    controls = controls.push(icon_button(
        if muted { Icon::Muted } else { Icon::Volume },
        if muted { "取消静音" } else { "静音" },
        (has_current && audio_available).then_some(Message::Playback(json!({"mute":!muted}))),
    ));
    if audio_available && !current.is_empty() && !mixed && !compact {
        controls = controls.push(
            slider(
                0..=100,
                output["volume"].as_u64().unwrap_or(100) as u32,
                |v| Message::Playback(json!({"volume":v})),
            )
            .width(if compact { Fill } else { Fixed(84.0) }),
        );
    }
    let outputs = std::iter::once("全部显示器".to_owned())
        .chain(
            app.state["outputs"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys().cloned()),
        )
        .collect::<Vec<_>>();
    let output = pick_list(outputs, Some(app.output.clone()), Message::Output)
        .padding([9, 10])
        .text_size(12)
        .width(if compact { 126 } else { 136 });
    let content: Element<'_, Message> = if compact {
        column![
            row![info, output].spacing(10).align_y(Alignment::Center),
            controls
        ]
        .spacing(10)
        .into()
    } else {
        row![info, controls, output]
            .spacing(14)
            .align_y(Alignment::Center)
            .into()
    };
    let surface = container(content)
        .id("playback-bar")
        .padding([10, 14])
        .width(Fill)
        .style(palette::glass);
    // A cached frosted backdrop keeps sharp gallery images out of the control surface.
    let backdrop: Element<'_, Message> = if let Some(ambient) = &app.ambient {
        image(ambient.clone())
            .width(Fill)
            .height(Fill)
            .content_fit(iced::ContentFit::Fill)
            .border_radius(24)
            .into()
    } else {
        container(Space::new().width(Fill).height(Fill))
            .style(|_| container::Style {
                background: Some(PANEL.into()),
                border: iced::border::rounded(24),
                ..Default::default()
            })
            .into()
    };
    widget::stack![surface].push_under(backdrop).into()
}

fn downloads(app: &App) -> Element<'_, Message> {
    let mut content = column![text("下载").size(28)].spacing(15);
    if let Some(error) = app.service["account"]["error"].as_str() {
        content = content
            .push(text(error).size(13).color(Color::from_rgb8(231, 181, 111)))
            .push(secondary(
                "打开 Steam",
                Message::Open("steam://open/main".into()),
            ));
    }
    let jobs = array(&app.service["jobs"]);
    if jobs.is_empty() {
        content = content.push(empty(
            "下载队列很安静",
            "在创意工坊选择喜欢的作品，点击「下载 / 更新」。",
            Some(("浏览创意工坊", Message::Navigate(Page::Workshop))),
        ));
    } else {
        content = content.push(
            row![
                icon_button(
                    Icon::Pause,
                    "暂停全部下载",
                    (!app.service_busy
                        && jobs.iter().any(|j| matches!(
                            strv(j, "status"),
                            "queued" | "downloading" | "updating"
                        )))
                    .then_some(Message::Service(json!({"action":"pause"})))
                ),
                icon_button(
                    Icon::Play,
                    "恢复全部下载",
                    (!app.service_busy && jobs.iter().any(|j| strv(j, "status") == "paused"))
                        .then_some(Message::Service(json!({"action":"resume"})))
                ),
                secondary(
                    "在 Steam 管理",
                    Message::Open("steam://nav/downloads".into())
                )
            ]
            .spacing(8),
        );
        let mut list = Column::new().spacing(12);
        for job in jobs {
            let id = strv(job, "id");
            let downloaded = job["downloaded"].as_u64().unwrap_or(0);
            let total = job["total"].as_u64().unwrap_or(0);
            let status = strv(job, "status");
            let label = match status {
                "queued" => "等待中",
                "downloading" => "下载中",
                "updating" => "正在更新",
                "paused" => "已暂停",
                "completed" | "installed" => "已完成",
                "cancelled" => "已移出队列",
                "failed" | "error" => "需要重试",
                "update_required" | "update_available" => "有更新",
                _ => status,
            };
            let mut entry = column![
                row![
                    text(if strv(job, "title").is_empty() {
                        id
                    } else {
                        strv(job, "title")
                    })
                    .size(17)
                    .width(Fill),
                    badge(label)
                ]
                .align_y(Alignment::Center),
                progress_bar(
                    0.0..=1.0,
                    if total > 0 {
                        downloaded as f32 / total as f32
                    } else {
                        0.0
                    }
                )
                .girth(5),
                row![
                    muted(format!(
                        "{} / {}",
                        bytes(downloaded),
                        if total > 0 {
                            bytes(total)
                        } else {
                            "等待 Steam 提供大小".into()
                        }
                    )),
                    Space::new().width(Fill),
                    icon_button(
                        Icon::Refresh,
                        if status == "update_available" {
                            "更新"
                        } else {
                            "重试"
                        },
                        (!app.service_busy
                            && matches!(
                                status,
                                "error" | "failed" | "cancelled" | "update_available"
                            ))
                        .then_some(Message::Service(json!({"action":"retry","id":id})))
                    ),
                    icon_button(
                        Icon::Close,
                        "取消排队",
                        (!app.service_busy && status == "queued")
                            .then_some(Message::Service(json!({"action":"cancel","id":id})))
                    )
                ]
                .spacing(8)
                .align_y(Alignment::Center)
            ]
            .spacing(15);
            if let Some(error) = job["error"].as_str().filter(|s| !s.is_empty()) {
                entry = entry.push(text(error).size(12).color(Color::from_rgb8(240, 169, 143)));
            }
            list = list.push(container(entry).padding(22).style(box_style(CARD)));
        }
        content = content.push(scrollable(list));
    }
    container(content).padding(32).height(Fill).into()
}
fn bytes(v: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    let mut value = v as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < units.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{v} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

fn playlists(app: &App) -> Element<'_, Message> {
    let mut content = column![
        text("播放列表").size(28),
        row![
            text_input("输入播放列表名称", &app.playlist_name)
                .on_input(Message::PlaylistName)
                .padding(11),
            primary(
                "新建列表",
                Message::CreatePlaylist,
                !app.playlist_name.trim().is_empty() && !app.busy
            )
        ]
        .spacing(10)
    ]
    .spacing(18);
    let lists = app.library["playlists"].as_object();
    if lists.is_none_or(|m| m.is_empty()) {
        return container(content.push(empty(
            "创建你的第一份播放列表",
            "创建后，从资源库选中壁纸并加入列表。",
            None,
        )))
        .padding(32)
        .height(Fill)
        .into();
    }
    let options = lists
        .unwrap()
        .iter()
        .map(|(id, v)| Choice {
            id: id.clone(),
            label: strv(v, "name").into(),
            value: Value::Null,
        })
        .collect::<Vec<_>>();
    let selected = options.iter().find(|v| v.id == app.playlist).cloned();
    content = content.push(
        row![
            pick_list(options, selected, |v| Message::PlaylistSelect(v.id))
                .padding(11)
                .width(Fill),
            secondary("重命名", Message::RenamePlaylist),
            button("删除列表")
                .padding([9, 14])
                .style(secondary_style)
                .on_press_maybe(
                    (!app.busy && !app.playlist.is_empty()).then_some(Message::DeletePlaylist)
                )
        ]
        .spacing(8),
    );
    let members = array(&app.library["playlists"][&app.playlist]["members"]);
    content = content.push(primary(
        "播放此列表",
        Message::PlayPlaylist(None),
        !members.is_empty() && !app.busy,
    ));
    let mut list = Column::new().spacing(8);
    for (i, member) in members.iter().enumerate() {
        let id = member.as_str().unwrap_or("");
        let item = app.catalog.iter().find(|a| strv(a, "id") == id);
        let title = item
            .map(|v| app.local_title(v))
            .unwrap_or_else(|| format!("项目暂不可用：{id}"));
        let playing = !app.playback_mixed()
            && app.playback_source() == app.playlist
            && strv(app.current_output(), "current") == id;
        let mut line = row![muted(format!("{:02}", i + 1))]
            .spacing(12)
            .align_y(Alignment::Center);
        line = line.push(icon_button(
            if playing && app.current_output()["paused"] != true {
                Icon::Pause
            } else {
                Icon::Play
            },
            if playing {
                "正在播放此项"
            } else {
                "从此项播放列表"
            },
            (!app.busy && item.is_some()).then_some(if playing {
                Message::Playback(json!({"paused": app.current_output()["paused"] != true}))
            } else {
                Message::PlayPlaylist(Some(id.into()))
            }),
        ));
        if let Some(item) = item {
            line = line.push(container(preview(app, item, 48.0, true)).width(76));
        }
        line = line
            .push(
                button(text(truncate(&title, 38)).size(13))
                    .id(format!("playlist-item-{i}"))
                    .style(button::text)
                    .on_press(Message::Select(id.into()))
                    .width(Fill),
            )
            .push(
                button(icon(Icon::ArrowUp))
                    .on_press_maybe((i > 0).then_some(Message::PlaylistMove(i, -1)))
                    .style(secondary_style),
            )
            .push(
                button(icon(Icon::ArrowDown))
                    .on_press_maybe((i + 1 < members.len()).then_some(Message::PlaylistMove(i, 1)))
                    .style(secondary_style),
            )
            .push(
                button("移除")
                    .on_press(Message::PlaylistRemove(i))
                    .style(button::text),
            );
        list = list.push(
            container(line)
                .padding(12)
                .style(move |theme| container::Style {
                    background: Some(CARD.into()),
                    border: Border {
                        radius: 12.0.into(),
                        width: if playing { 1.0 } else { 0.0 },
                        color: theme.palette().primary,
                    },
                    ..Default::default()
                }),
        );
    }
    if members.is_empty() {
        list = list.push(muted(
            "列表还是空的。去资源库选择壁纸，在详情面板加入这个列表。",
        ));
    }
    content = content.push(list);
    let outputs = std::iter::once("全部显示器".to_owned())
        .chain(
            app.state["outputs"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys().cloned()),
        )
        .collect::<Vec<_>>();
    let mut rotation = column![
        heading("轮换规则"),
        row![
            muted("显示器"),
            pick_list(outputs, Some(app.output.clone()), Message::Output).padding(9),
            Space::new().width(Fill),
            checkbox(app.rotation_random)
                .label("随机播放")
                .on_toggle(Message::RotationRandom)
                .text_size(13)
        ]
        .spacing(12)
        .align_y(Alignment::Center),
        row![
            muted("每隔"),
            text_input("30", &app.rotation_interval)
                .on_input(Message::RotationInterval)
                .padding(9)
                .width(70),
            muted("分钟切换"),
            Space::new().width(Fill),
            checkbox(app.schedule_enabled)
                .label("限制时间段")
                .on_toggle(Message::ScheduleEnabled)
                .text_size(13)
        ]
        .spacing(10)
        .align_y(Alignment::Center)
    ]
    .spacing(14);
    if app.schedule_enabled {
        rotation = rotation.push(
            row![
                muted("每天"),
                text_input("08:00", &app.schedule_start)
                    .on_input(Message::ScheduleStart)
                    .padding(9)
                    .width(90),
                muted("至"),
                text_input("23:00", &app.schedule_end)
                    .on_input(Message::ScheduleEnd)
                    .padding(9)
                    .width(90),
                muted("支持跨午夜；时段外保留当前壁纸")
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
    }
    rotation = rotation.push(
        row![
            primary(
                "保存并启用自动轮换",
                Message::StartRotation(true),
                !members.is_empty() && !app.busy
            ),
            icon_button(
                Icon::Stop,
                "关闭此列表的自动轮换",
                app.playback_targets()
                    .iter()
                    .any(|name| {
                        let settings = app.playback_settings(name);
                        settings["source"] == app.playlist && settings["enabled"] == true
                    })
                    .then_some(Message::StartRotation(false))
            ),
            Space::new().width(Fill),
            secondary("添加壁纸", Message::Navigate(Page::Library))
        ]
        .spacing(8),
    );
    content = content.push(container(rotation).padding(22).style(box_style(PANEL)));
    container(scrollable(content))
        .padding(32)
        .height(Fill)
        .into()
}
fn displays(app: &App) -> Element<'_, Message> {
    let mut content = column![
        text("显示器").size(28),
        muted("独立选择壁纸，或应用到全部显示器。相同视频和场景共享播放时钟；网页与应用暂不支持帧同步。")
    ]
    .spacing(20);
    if let Some(outputs) = app.state["outputs"].as_object() {
        for (name, output) in outputs {
            let display = &app.displays[name];
            let mode = display["current_mode"]
                .as_u64()
                .and_then(|i| display["modes"].get(i as usize));
            let geometry = mode
                .map(|m| format!("{} × {}", m["width"], m["height"]))
                .unwrap_or_else(|| "Wayland 显示器".into());
            let scale = display["logical"]["scale"].as_f64().unwrap_or(1.0);
            let title = app
                .catalog
                .iter()
                .find(|v| v["id"] == output["current"])
                .map(|v| app.local_title(v))
                .unwrap_or_else(|| "尚未设置壁纸".into());
            let screen = container(
                column![
                    icon(Icon::Monitor).size(44).color(MUTED),
                    text(name).size(18),
                    muted(geometry)
                ]
                .align_x(Alignment::Center)
                .spacing(7),
            )
            .center_x(200)
            .center_y(130)
            .style(box_style(CARD));
            let mut info = column![
                heading(name),
                muted(format!(
                    "缩放 {:.0}% · 方向 {}",
                    scale * 100.0,
                    display["logical"]["transform"].as_str().unwrap_or("正常")
                )),
                text(title).size(14),
                row![
                    secondary("选择此显示器", Message::Output(name.clone())),
                    icon_button(
                        if output["paused"] == true {
                            Icon::Play
                        } else {
                            Icon::Pause
                        },
                        if output["paused"] == true {
                            "继续播放"
                        } else {
                            "暂停"
                        },
                        Some(Message::Command(
                            "set_playback",
                            json!({"output":name,"paused":output["paused"]!=true})
                        ))
                    ),
                    icon_button(
                        Icon::Stop,
                        "停止",
                        Some(Message::Command("release", json!({"output":name})))
                    )
                ]
                .spacing(8)
            ]
            .spacing(12)
            .width(Fill);
            if let Some(error) = output["error"]["message"].as_str() {
                info = info.push(text(error).size(12).color(Color::from_rgb8(240, 129, 136)));
            }
            content = content.push(
                container(row![screen, info].spacing(25).align_y(Alignment::Center))
                    .padding(24)
                    .style(box_style(PANEL)),
            );
        }
    }
    content = content
        .push(secondary(
            "返回资源库选择壁纸",
            Message::Navigate(Page::Library),
        ))
        .push(muted(
            "显示器断开后会保留设置，重新连接时恢复。缩放和方向由 Wayland 合成器处理。",
        ));
    container(scrollable(content))
        .padding(32)
        .height(Fill)
        .into()
}
fn settings(app: &App) -> Element<'_, Message> {
    let mut content = column![
        text("设置").size(28),
        Space::new().height(8),
        heading("播放与性能")
    ]
    .spacing(18);
    for (key, label, description, default) in [
        (
            "pause_on_session",
            "锁屏、休眠或切换会话时暂停",
            "解锁后恢复；手动暂停的内容会继续保持暂停。",
            true,
        ),
        (
            "pause_on_fullscreen",
            "全屏应用时暂停",
            "为全屏游戏和视频释放资源。",
            true,
        ),
        (
            "mute_on_other_audio",
            "其他应用播放声音时静音",
            "其他声音结束后，恢复原来的音量设置。",
            true,
        ),
        (
            "pause_on_battery",
            "使用电池时暂停",
            "电源状态不可用时，不触发这个策略。",
            false,
        ),
    ] {
        let enabled = app.state["policy_settings"][key]
            .as_bool()
            .unwrap_or(default);
        content = content.push(
            container(
                column![
                    checkbox(enabled)
                        .label(label)
                        .on_toggle(move |v| Message::Command("set_policy", json!({key:v}))),
                    muted(description)
                ]
                .spacing(8),
            )
            .padding(18)
            .width(Fill)
            .style(box_style(PANEL)),
        );
    }
    let fps = app.current_output()["fps"].as_u64().unwrap_or(30) as u32;
    content = content.push(
        row![
            text("壁纸帧率").size(14),
            Space::new().width(Fill),
            pick_list(vec![15_u32, 24, 30, 60, 90, 120], Some(fps), |v| {
                Message::Playback(json!({"fps":v}))
            })
            .padding(10),
            muted("FPS")
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    );
    content = content
        .push(widget::rule::horizontal(1))
        .push(heading("Steam 账号"));
    let account = &app.service["account"];
    content = content
        .push(
            text(if account["logged_in"] == true {
                format!("已连接 · {}", strv(account, "name"))
            } else {
                "Steam 尚未连接".into()
            })
            .size(15),
        )
        .push(muted(
            "使用官方 Steam 客户端完成登录、Steam Guard 与许可证授权。Misari 不收集密码。",
        ));
    if let Some(error) = account["error"].as_str() {
        content = content.push(text(error).size(12).color(Color::from_rgb8(231, 181, 111)));
    }
    content = content
        .push(secondary(
            "打开 Steam",
            Message::Open("steam://open/main".into()),
        ))
        .push(widget::rule::horizontal(1))
        .push(heading("播放能力"));
    for (key, label) in [
        ("rust_scene", "场景 · Rust 原生渲染"),
        ("libmpv", "视频 · libmpv"),
        ("cef", "网页 · Chromium / CEF"),
    ] {
        let backend = &app.state["backends"][key];
        content = content.push(row![
            text(label).size(13).width(Fill),
            badge(if backend["available"] == true {
                "已就绪"
            } else {
                "不可用"
            })
        ]);
        if backend["available"] != true {
            content = content.push(muted(
                backend["error"].as_str().unwrap_or("请检查引擎安装。"),
            ));
        }
    }
    content = content
        .push(widget::rule::horizontal(1))
        .push(heading("应用型项目"));
    let application = &app.state["backends"]["application"];
    content=content.push(checkbox(app.state["policy_settings"]["allow_applications"]==true).label("启用 Wine 应用兼容后端").on_toggle(|v|Message::Command("set_policy",json!({"allow_applications":v})))).push(muted("应用项目会运行其中的 Windows 程序。仅应用你信任的项目；兼容后端暂不支持声音、属性、输入或跨屏同步。"));
    if let Some(reason) = application["reason"].as_str() {
        content = content.push(muted(reason));
    }
    content = content
        .push(widget::rule::horizontal(1))
        .push(heading("键盘操作"))
        .push(muted(
            "Tab / Shift Tab 切换控件 · Enter / 空格操作 · 方向键调整选项
Ctrl 1–7 切换页面 · Ctrl F 搜索 · Ctrl R 刷新 · Ctrl I 导入 · Ctrl P 暂停 · Esc 关闭详情",
        ));
    container(scrollable(content.padding(iced::Padding {
        right: 16.0,
        ..Default::default()
    })))
    .padding(32)
    .height(Fill)
    .into()
}
