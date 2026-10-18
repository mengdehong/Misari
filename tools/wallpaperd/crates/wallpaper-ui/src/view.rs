use crate::controls::{button, checkbox, pick_list, slider};
use crate::{App, Message, Page, array, strv};
use iced::{
    Alignment, Border, Color, Element,
    Length::{Fill, Fixed},
    Theme,
    widget::{
        self, Column, Row, Space, column, container, image, progress_bar, row, scrollable, text,
        text_input,
    },
};
use serde_json::{Value, json};

pub const BG: Color = Color::from_rgb8(17, 21, 27);
pub const PANEL: Color = Color::from_rgb8(24, 29, 36);
pub const CARD: Color = Color::from_rgb8(31, 37, 46);
pub const TEXT: Color = Color::from_rgb8(232, 237, 242);
pub const MUTED: Color = Color::from_rgb8(141, 155, 169);
pub const ACCENT: Color = Color::from_rgb8(167, 224, 197);
const LINE: Color = Color::from_rgb8(45, 54, 65);

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
    move |_, status| {
        let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(
                if hover {
                    Color::from_rgb8(40, 51, 59)
                } else {
                    CARD
                }
                .into(),
            ),
            text_color: TEXT,
            border: Border {
                color: if selected { ACCENT } else { LINE },
                width: if selected { 1.5 } else { 0.5 },
                radius: 10.0.into(),
            },
            ..Default::default()
        }
    }
}
fn secondary_style(_: &Theme, status: button::Status) -> button::Style {
    let disabled = status == button::Status::Disabled;
    button::Style {
        background: Some(
            if status == button::Status::Hovered {
                Color::from_rgb8(48, 61, 67)
            } else {
                CARD
            }
            .into(),
        ),
        text_color: if disabled {
            Color::from_rgb8(86, 98, 110)
        } else {
            TEXT
        },
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 7.0.into(),
        },
        ..Default::default()
    }
}
fn secondary<'a>(
    label: impl Into<String>,
    message: Message,
) -> crate::controls::Button<'a, Message> {
    button(text(label.into()).size(13))
        .padding([9, 13])
        .on_press(message)
        .style(secondary_style)
}
fn primary<'a>(
    label: impl Into<String>,
    message: Message,
    enabled: bool,
) -> crate::controls::Button<'a, Message> {
    button(text(label.into()).size(14))
        .padding([11, 17])
        .on_press_maybe(enabled.then_some(message))
}
fn muted<'a>(s: impl Into<String>) -> widget::Text<'a> {
    text(s.into()).size(12).color(MUTED)
}
fn heading<'a>(s: impl Into<String>) -> widget::Text<'a> {
    text(s.into()).size(20)
}
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.into()
    }
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
fn preview<'a>(item: &Value, height: f32) -> Element<'a, Message> {
    if let Some(path) = item["thumbnail"]
        .as_str()
        .filter(|s| std::path::Path::new(s).is_file())
    {
        Element::new(ClippedImage(
            image(path.to_owned())
                .width(Fill)
                .height(Fixed(height))
                .content_fit(iced::ContentFit::Cover),
        ))
    } else {
        container(
            column![
                text("◈").size(36).color(ACCENT),
                muted(if item["preview_failed"] == true {
                    "预览暂不可用"
                } else if item["preview_url"].is_string() {
                    "预览正在准备"
                } else {
                    "暂无预览"
                })
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        )
        .center_x(Fill)
        .center_y(Fixed(height))
        .style(box_style(Color::from_rgb8(32, 45, 48)))
        .into()
    }
}
// Iced's CPU image renderer does not enforce image drawing bounds itself.
// A renderer layer makes cover-cropping consistent for both raster and GPU renderers.
struct ClippedImage(widget::Image);
impl iced::advanced::Widget<Message, Theme, iced::Renderer> for ClippedImage {
    fn size(&self) -> iced::Size<iced::Length> {
        iced::advanced::Widget::<Message, Theme, iced::Renderer>::size(&self.0)
    }
    fn layout(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        renderer: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        iced::advanced::Widget::<Message, Theme, iced::Renderer>::layout(
            &mut self.0,
            tree,
            renderer,
            limits,
        )
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
        renderer.with_layer(bounds, |renderer| {
            iced::advanced::Widget::<Message, Theme, iced::Renderer>::draw(
                &self.0, tree, renderer, theme, style, layout, cursor, viewport,
            )
        });
    }
}
fn badge<'a>(label: impl Into<String>) -> Element<'a, Message> {
    container(text(label.into()).size(10).color(ACCENT))
        .padding([3, 7])
        .style(box_style(Color::from_rgb8(42, 62, 57)))
        .into()
}
fn empty<'a>(
    title: &str,
    description: &str,
    action: Option<(&str, Message)>,
) -> Element<'a, Message> {
    let mut content = column![
        text("◌").size(52).color(ACCENT),
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
    let mut middle = row![sidebar, container(content).width(Fill).height(Fill)];
    if app.selected.is_some()
        && matches!(
            app.page,
            Page::Library | Page::Discover | Page::Workshop | Page::Playlists
        )
    {
        middle = middle.push(detail(app));
    }
    let mut body = column![middle.height(Fill), playback(app)];
    if let Some((error, message)) = &app.notice {
        let banner = row![
            text(if *error { "!" } else { "✓" }).size(17),
            text(message).size(13).width(Fill),
            button("关闭")
                .on_press(Message::Dismiss)
                .style(button::text)
        ]
        .spacing(14)
        .align_y(Alignment::Center);
        body = column![
            container(banner)
                .padding([12, 20])
                .style(box_style(if *error {
                    Color::from_rgb8(67, 38, 42)
                } else {
                    Color::from_rgb8(32, 58, 49)
                })),
            body
        ];
    }
    container(body)
        .width(Fill)
        .height(Fill)
        .style(box_style(BG))
        .into()
}
fn sidebar(app: &App) -> Element<'_, Message> {
    let mut nav = column![
        text("M I S A R I").size(19).color(ACCENT),
        muted("让桌面，自有风景"),
        Space::new().height(28),
        muted("工作空间")
    ]
    .spacing(8);
    for (page, icon) in [
        (Page::Library, "▦"),
        (Page::Discover, "◈"),
        (Page::Workshop, "◎"),
        (Page::Downloads, "↓"),
        (Page::Playlists, "☷"),
    ] {
        let selected = app.page == page;
        let count = match page {
            Page::Library => app.catalog.len(),
            Page::Downloads => array(&app.service["jobs"]).len(),
            Page::Playlists => app.library["playlists"].as_object().map_or(0, |m| m.len()),
            _ => 0,
        };
        let label = row![
            text(icon).size(18).width(23),
            text(page.title()).size(14).width(Fill),
            muted(if count > 0 {
                count.to_string()
            } else {
                String::new()
            })
        ]
        .align_y(Alignment::Center)
        .spacing(7);
        nav = nav.push(
            button(label)
                .width(Fill)
                .padding([12, 10])
                .on_press(Message::Navigate(page))
                .style(move |_, status| button::Style {
                    background: if selected {
                        Some(Color::from_rgb8(39, 58, 54).into())
                    } else if status == button::Status::Hovered {
                        Some(CARD.into())
                    } else {
                        None
                    },
                    text_color: if selected { ACCENT } else { MUTED },
                    border: Border {
                        radius: 8.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        );
    }
    nav = nav
        .push(Space::new().height(Fill))
        .push(widget::rule::horizontal(1))
        .push(secondary("▣   显示器", Message::Navigate(Page::Displays)).width(Fill))
        .push(secondary("⚙   设置", Message::Navigate(Page::Settings)).width(Fill))
        .push(Space::new().height(15));
    let logged = app.service["account"]["logged_in"] == true;
    nav = nav
        .push(
            row![
                text("●")
                    .size(10)
                    .color(if logged { ACCENT } else { MUTED }),
                text(if logged {
                    strv(&app.service["account"], "name")
                } else {
                    "连接 Steam"
                })
                .size(12)
            ]
            .spacing(8),
        )
        .push(muted(if logged {
            "已复用 Steam 登录"
        } else {
            "登录与授权由 Steam 管理"
        }));
    container(nav)
        .padding([28, 18])
        .width(188)
        .height(Fill)
        .style(box_style(PANEL))
        .into()
}
fn browser(app: &App) -> Element<'_, Message> {
    let remote = matches!(app.page, Page::Discover | Page::Workshop);
    let sub = match app.page {
        Page::Discover => "来自 Wallpaper Engine 的发现与推荐",
        Page::Workshop => "探索创作者的世界 · Steam 创意工坊",
        _ => "收藏每一处风景，让喜欢的内容回到桌面",
    };
    let title = row![
        column![text(app.page.title()).size(28), muted(sub)]
            .spacing(5)
            .width(Fill),
        secondary("刷新", Message::Refresh),
        secondary("导入文件", Message::Import(false)),
        secondary("导入项目", Message::Import(true))
    ]
    .spacing(8)
    .align_y(Alignment::Center);
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
    let filters = if remote {
        vec!["全部类型", "场景", "视频", "网页", "应用"]
    } else {
        vec!["全部类型", "场景", "视频", "网页", "应用", "图片", "着色器"]
    }
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    let sorts = if remote {
        vec!["热门", "最新", "评分最高", "订阅最多"]
    } else {
        vec!["名称 A–Z", "名称 Z–A", "类型"]
    }
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    let mut controls = row![
        search,
        pick_list(filters, Some(app.filter.clone()), Message::Filter).padding(11),
        pick_list(sorts, Some(app.sort.clone()), Message::Sort).padding(11)
    ]
    .spacing(10);
    if remote {
        controls = controls.push(primary("搜索", Message::SearchNow, !app.service_busy));
    } else {
        controls = controls.push(
            checkbox(app.favorites_only)
                .label("本地收藏")
                .on_toggle(Message::Favorites)
                .text_size(12),
        );
    }
    let mut body = column![title, Space::new().height(8), controls].spacing(16);
    if app.page == Page::Workshop {
        body = body.push(
            row![
                pick_list(
                    vec!["全部作品".to_owned(), "已订阅".into(), "Steam 收藏".into()],
                    Some(app.scope.clone()),
                    Message::Scope
                )
                .padding(9),
                muted("Steam 收藏与资源库中的本地收藏分别管理")
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
    }
    if app.page == Page::Discover {
        body = body.push(
            container(
                row![
                    column![
                        text("为桌面找到下一份心动").size(23),
                        muted("动态场景、沉浸视频与可交互的网页，来自同一个创作社区。")
                    ]
                    .spacing(9)
                    .width(Fill),
                    badge("WALLPAPER ENGINE")
                ]
                .align_y(Alignment::Center),
            )
            .padding(24)
            .style(box_style(Color::from_rgb8(31, 48, 48))),
        );
    }
    if app.page == Page::Discover && !app.recommendations.is_empty() {
        let mut recommendations = Row::new().spacing(12);
        for item in app.recommendations.iter().take(3) {
            recommendations = recommendations.push(
                button(column![
                    preview(item, 76.0),
                    container(text(truncate(&app.local_title(item), 24)).size(12)).padding([7, 10])
                ])
                .padding(0)
                .width(Fill)
                .on_press(Message::Select(strv(item, "id").into()))
                .style(card_style(false)),
            );
        }
        body = body
            .push(column![muted("本期精选 · Wallpaper Engine 推荐"), recommendations].spacing(8));
    }
    if app.loading || (remote && app.service_busy && app.remote.is_empty()) {
        return container(body.push(empty("正在载入壁纸", "读取真实项目、预览和服务状态…", None)))
            .padding(28)
            .height(Fill)
            .into();
    }
    let values = if remote {
        app.remote.iter().collect::<Vec<_>>()
    } else {
        app.filtered()
    };
    let count = values.len();
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
            (
                "没有找到匹配的壁纸",
                "试试其他关键词或类型，或取消「本地收藏」筛选。",
            )
        };
        return container(body.push(empty(
            title,
            desc,
            Some((
                if remote {
                    "重试搜索"
                } else {
                    "刷新资源库"
                },
                if remote {
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
    let page_size = 12;
    let pages = if remote {
        app.remote_pages
    } else {
        count.div_ceil(page_size)
    };
    let page = if remote {
        app.remote_page.saturating_sub(1)
    } else {
        app.page_index.min(pages.saturating_sub(1))
    };
    let items: Vec<_> = if remote {
        values
    } else {
        values
            .into_iter()
            .skip(page * page_size)
            .take(page_size)
            .collect()
    };
    body = body.push(row![
        muted(if remote {
            format!("第 {} 页 · Steam 状态独立显示", page + 1)
        } else {
            format!("{} 个项目 · 本地资源", count)
        }),
        Space::new().width(Fill),
        muted(if app.service_busy {
            "正在搜索，当前显示上次结果…"
        } else {
            "选择壁纸以预览和调整"
        })
    ]);
    let grid = widget::responsive(move |size| {
        let cols = if size.width >= 940.0 {
            4
        } else if size.width >= 680.0 {
            3
        } else {
            2
        };
        let mut grid = Column::new().spacing(16);
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
        scrollable(grid).height(Fill).into()
    });
    body = body.push(grid).push(
        row![
            button("← 上一页")
                .padding([8, 13])
                .on_press_maybe((page > 0 && !app.service_busy).then_some(Message::Page(-1)))
                .style(secondary_style),
            Space::new().width(Fill),
            muted(format!("{} / {}", page + 1, pages.max(1))),
            Space::new().width(Fill),
            button("下一页 →")
                .padding([8, 13])
                .on_press_maybe((page + 1 < pages && !app.service_busy).then_some(Message::Page(1)))
                .style(secondary_style)
        ]
        .align_y(Alignment::Center),
    );
    container(body).padding(28).height(Fill).into()
}
fn wallpaper_card<'a>(app: &'a App, item: &'a Value) -> Element<'a, Message> {
    let id = strv(item, "id");
    let selected = app.selected.as_deref() == Some(id);
    let title = app.local_title(item);
    let remote = !id.starts_with("we:") && !id.starts_with("local:");
    let status = if remote {
        if item["downloaded"] == true {
            "已下载"
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
        "本地收藏"
    } else {
        "已下载"
    };
    let info = column![
        text(truncate(&title, 25)).size(13).height(38),
        row![
            muted(kind_label(strv(item, "kind"))),
            Space::new().width(Fill),
            text(status).size(10).color(if status == "正在使用" {
                ACCENT
            } else {
                MUTED
            })
        ]
    ]
    .spacing(10)
    .padding([12, 13]);
    button(column![preview(item, 142.0), info])
        .padding(0)
        .width(Fill)
        .on_press(Message::Select(id.into()))
        .style(card_style(selected))
        .into()
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
    let mut content = column![
        row![
            muted(if remote {
                "创意工坊详情"
            } else {
                "壁纸详情"
            }),
            Space::new().width(Fill),
            button("×")
                .on_press(Message::CloseDetail)
                .style(button::text)
        ],
        preview(item, 172.0),
        text(app.local_title(item)).size(19),
        row![
            badge(kind_label(strv(item, "kind"))),
            Space::new().width(Fill),
            muted(if remote {
                "STEAM WORKSHOP"
            } else {
                "本地项目"
            })
        ]
    ]
    .spacing(14);
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
        return container(scrollable(content).height(Fill))
            .padding(22)
            .width(340)
            .height(Fill)
            .style(box_style(PANEL))
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
            pick_list(
                vec!["填充".to_owned(), "适应".into(), "拉伸".into()],
                Some(app.fit.clone()),
                Message::Fit
            )
            .padding(10),
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
                        .is_some_and(|m| !m.is_empty())
            )
            .width(Fill)
        ]
        .spacing(10),
    );
    content = content.push(
        row![
            secondary(
                if app.favorite(id) {
                    "★ 已收藏到本地"
                } else {
                    "☆ 本地收藏"
                },
                Message::Favorite
            )
            .width(Fill),
            secondary(
                "定位文件",
                Message::Open(
                    {
                        let path = std::path::Path::new(strv(item, "path"));
                        if path.is_file() {
                            path.parent().unwrap_or(path)
                        } else {
                            path
                        }
                    }
                    .to_string_lossy()
                    .into_owned()
                )
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
                    secondary("＋", Message::PlaylistAdd)
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
    container(scrollable(content).height(Fill))
        .padding(22)
        .width(340)
        .height(Fill)
        .style(box_style(PANEL))
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
    let label = plain_label(def["text"].as_str().unwrap_or(&key));
    let value = app.property_value(&key);
    if kind == "group" {
        return text(label).size(15).color(ACCENT).into();
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
    if !app.can_edit_properties() {
        return column![
            muted(label),
            text(truncate(&value.to_string(), 60)).size(12).color(MUTED)
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
                    label: plain_label(
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
        "color" => {
            let channels = if let Some(a) = value.as_array() {
                a.iter()
                    .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                    .collect::<Vec<_>>()
            } else {
                value
                    .as_str()
                    .unwrap_or("")
                    .split_whitespace()
                    .filter_map(|v| v.parse().ok())
                    .collect()
            };
            let c = if channels.len() == 3 {
                channels
            } else {
                vec![0.0; 3]
            };
            field = field.push(
                container(
                    text(format!(
                        "#{:02X}{:02X}{:02X}",
                        (c[0] * 255.0) as u8,
                        (c[1] * 255.0) as u8,
                        (c[2] * 255.0) as u8
                    ))
                    .size(12)
                    .color(if c.iter().sum::<f32>() > 1.7 {
                        BG
                    } else {
                        TEXT
                    }),
                )
                .padding(9)
                .width(Fill)
                .style(box_style(Color::from_rgb(
                    c[0].clamp(0.0, 1.0),
                    c[1].clamp(0.0, 1.0),
                    c[2].clamp(0.0, 1.0),
                ))),
            );
            for i in 0..3 {
                let k = key.clone();
                let c = c.clone();
                let value = c[i];
                field = field.push(
                    row![
                        muted(["R", "G", "B"][i]),
                        slider(0.0..=1.0, value, move |v| {
                            let mut c = c.clone();
                            c[i] = v;
                            Message::Property(k.clone(), json!(c))
                        })
                        .step(1.0_f32 / 255.0_f32)
                    ]
                    .spacing(9),
                );
            }
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
fn plain_label(s: &str) -> String {
    if let Some(label) = match s {
        "ui_browse_properties_scheme_color" => Some("配色"),
        "ui_browse_properties_audio_volume" => Some("音量"),
        "ui_browse_properties_playbackrate" => Some("播放速度"),
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
    result
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .trim()
        .to_owned()
}
fn playback(app: &App) -> Element<'_, Message> {
    let output = app.current_output();
    let current = strv(output, "current");
    let item = app.catalog.iter().find(|v| strv(v, "id") == current);
    let title = item
        .map(|v| app.local_title(v))
        .unwrap_or_else(|| "桌面已就绪".into());
    let paused = output["paused"] == true;
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
    let mut bar = row![];
    if let Some(item) = item {
        bar = bar.push(container(preview(item, 48.0)).width(78));
    }
    bar = bar.push(
        column![text(truncate(&title, 48)).size(13), muted(status)]
            .spacing(5)
            .width(Fill),
    );
    bar = bar.push(
        button(if paused { "▶ 继续" } else { "Ⅱ 暂停" })
            .padding([9, 15])
            .on_press_maybe(
                (!current.is_empty() && !app.busy)
                    .then_some(Message::Playback(json!({"paused":!paused}))),
            )
            .style(secondary_style),
    );
    bar = bar.push(
        button("■ 停止")
            .padding([9, 15])
            .on_press_maybe((!current.is_empty() && !app.busy).then_some(Message::Stop))
            .style(secondary_style),
    );
    let audio_available = strv(output, "backend") != "application";
    let muted = output["mute"] == true;
    bar = bar.push(
        button(if muted { "静音" } else { "声音" })
            .on_press_maybe(
                (!current.is_empty() && audio_available)
                    .then_some(Message::Playback(json!({"mute":!muted}))),
            )
            .style(button::text),
    );
    if audio_available && !current.is_empty() {
        bar = bar.push(
            slider(
                0..=100,
                output["volume"].as_u64().unwrap_or(100) as u32,
                |v| Message::Playback(json!({"volume":v})),
            )
            .width(100),
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
    bar = bar.push(
        pick_list(outputs, Some(app.output.clone()), Message::Output)
            .padding(8)
            .text_size(12),
    );
    container(bar.spacing(14).align_y(Alignment::Center))
        .padding([14, 24])
        .style(box_style(PANEL))
        .into()
}
fn downloads(app: &App) -> Element<'_, Message> {
    let mut content = column![
        text("下载与更新").size(28),
        muted("Steam 负责内容传输与校验，下载进度来自真实客户端。"),
        Space::new().height(8)
    ]
    .spacing(15);
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
        content=content.push(muted("暂停 / 恢复控制全部 Wallpaper Engine 工坊传输。仅等待中的任务可以取消；已开始的传输请到 Steam 管理。"));
        content = content.push(
            row![
                primary(
                    "暂停全部",
                    Message::Service(json!({"action":"pause"})),
                    !app.service_busy
                        && jobs.iter().any(|j| matches!(
                            strv(j, "status"),
                            "queued" | "downloading" | "updating"
                        ))
                ),
                primary(
                    "恢复全部",
                    Message::Service(json!({"action":"resume"})),
                    !app.service_busy && jobs.iter().any(|j| strv(j, "status") == "paused")
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
                    button(if status == "update_available" {
                        "更新"
                    } else {
                        "重试"
                    })
                    .padding([9, 14])
                    .style(secondary_style)
                    .on_press_maybe(
                        (!app.service_busy
                            && matches!(
                                status,
                                "error" | "failed" | "cancelled" | "update_available"
                            ))
                        .then_some(Message::Service(json!({"action":"retry","id":id})))
                    ),
                    button("取消排队")
                        .padding([9, 14])
                        .style(secondary_style)
                        .on_press_maybe(
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
    if v >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", v as f64 / (1024.0 * 1024.0 * 1024.0))
    } else {
        format!("{:.1} MB", v as f64 / (1024.0 * 1024.0))
    }
}
fn playlists(app: &App) -> Element<'_, Message> {
    let mut content = column![
        text("播放列表").size(28),
        muted("把喜欢的风景排成一段日常，轮换会在后台继续。"),
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
    let mut list = Column::new().spacing(8);
    for (i, member) in members.iter().enumerate() {
        let id = member.as_str().unwrap_or("");
        let item = app.catalog.iter().find(|a| strv(a, "id") == id);
        let title = item
            .map(|v| app.local_title(v))
            .unwrap_or_else(|| format!("项目暂不可用：{id}"));
        let mut line = row![muted(format!("{:02}", i + 1))]
            .spacing(12)
            .align_y(Alignment::Center);
        if let Some(item) = item {
            line = line.push(container(preview(item, 48.0)).width(76));
        }
        line = line
            .push(
                button(text(truncate(&title, 38)).size(13))
                    .style(button::text)
                    .on_press(Message::Select(id.into()))
                    .width(Fill),
            )
            .push(
                button("↑")
                    .on_press_maybe((i > 0).then_some(Message::PlaylistMove(i, -1)))
                    .style(secondary_style),
            )
            .push(
                button("↓")
                    .on_press_maybe((i + 1 < members.len()).then_some(Message::PlaylistMove(i, 1)))
                    .style(secondary_style),
            )
            .push(
                button("移除")
                    .on_press(Message::PlaylistRemove(i))
                    .style(button::text),
            );
        list = list.push(container(line).padding(12).style(box_style(CARD)));
    }
    if members.is_empty() {
        list = list.push(muted(
            "列表还是空的。去资源库选择壁纸，在详情面板加入这个列表。",
        ));
    }
    content = content.push(scrollable(list).height(Fill));
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
                "保存并开始轮播",
                Message::StartRotation(true),
                !members.is_empty() && !app.busy
            ),
            secondary("停止轮播", Message::StartRotation(false)),
            secondary("下一张", Message::Next),
            Space::new().width(Fill),
            secondary("添加壁纸", Message::Navigate(Page::Library))
        ]
        .spacing(8),
    );
    content = content.push(container(rotation).padding(22).style(box_style(PANEL)));
    container(content).padding(32).height(Fill).into()
}
fn displays(app: &App) -> Element<'_, Message> {
    let mut content = column![
        text("每一块屏幕，都有自己的风景").size(28),
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
                    text("▣").size(44).color(ACCENT),
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
                    secondary(
                        if output["paused"] == true {
                            "继续"
                        } else {
                            "暂停"
                        },
                        Message::Command(
                            "set_playback",
                            json!({"output":name,"paused":output["paused"]!=true})
                        )
                    ),
                    secondary("停止", Message::Command("release", json!({"output":name})))
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
        muted("安静地运行，在需要时让出资源。"),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_labels_are_plain_text() {
        assert_eq!(plain_label("<b>颜色</b> &amp; 光线"), "颜色 & 光线");
        assert_eq!(truncate("你好世界", 2), "你好…");
    }
}
