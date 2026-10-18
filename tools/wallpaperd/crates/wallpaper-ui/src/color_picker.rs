//! A compact HSV editor anchored to its color swatch.
use crate::{Message, controls, palette::*};
use iced::{
    Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
        widget::{Operation, Tree, tree},
    },
    keyboard::{Key, key::Named},
    widget::{Space, column, container, row, stack, text},
};
use serde_json::Value;

pub struct Editor {
    pub key: String,
    pub hsv: [f32; 3],
}
impl Editor {
    pub fn new(key: String, value: &Value) -> Self {
        let c = color(value);
        let max = c.r.max(c.g).max(c.b);
        let min = c.r.min(c.g).min(c.b);
        let delta = max - min;
        let hue = if delta == 0.0 {
            0.0
        } else if max == c.r {
            ((c.g - c.b) / delta).rem_euclid(6.0)
        } else if max == c.g {
            (c.b - c.r) / delta + 2.0
        } else {
            (c.r - c.g) / delta + 4.0
        } / 6.0;
        Self {
            key,
            hsv: [hue, if max == 0.0 { 0.0 } else { delta / max }, max],
        }
    }
}

pub fn color(value: &Value) -> Color {
    let channels: Vec<f32> = if let Some(values) = value.as_array() {
        values
            .iter()
            .filter_map(|v| v.as_f64().map(|v| v as f32))
            .collect()
    } else {
        value
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect()
    };
    if channels.len() != 3 || channels.iter().any(|v| !v.is_finite()) {
        return Color::BLACK;
    }
    Color::from_rgb(
        channels[0].clamp(0.0, 1.0),
        channels[1].clamp(0.0, 1.0),
        channels[2].clamp(0.0, 1.0),
    )
}

pub fn rgb([h, s, v]: [f32; 3]) -> [f32; 3] {
    let hue = h.rem_euclid(1.0) * 6.0;
    let chroma = v * s;
    let x = chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
    let [r, g, b] = match hue as u32 {
        0 => [chroma, x, 0.0],
        1 => [x, chroma, 0.0],
        2 => [0.0, chroma, x],
        3 => [0.0, x, chroma],
        4 => [x, 0.0, chroma],
        _ => [chroma, 0.0, x],
    };
    [r + v - chroma, g + v - chroma, b + v - chroma]
}

fn swatch<'a>(color: Color, size: f32) -> Element<'a, Message> {
    container(Space::new().width(size).height(size))
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: iced::Border {
                color: Color::WHITE.scale_alpha(0.25),
                width: 1.0,
                radius: (size / 2.0).into(),
            },
            ..Default::default()
        })
        .into()
}

pub fn view<'a>(
    key: String,
    label: String,
    value: &Value,
    editor: Option<&Editor>,
    enabled: bool,
) -> Element<'a, Message> {
    let anchor = controls::button(
        row![
            text(label).size(13).width(Length::Fill),
            swatch(color(value), 26.0)
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center),
    )
    .padding([10, 12])
    .width(Length::Fill)
    .on_press_maybe(enabled.then_some(Message::EditColor(key.clone())))
    .style(move |_, status| controls::button::Style {
        background: Some(
            WASH.scale_alpha(if status == controls::button::Status::Hovered {
                0.14
            } else {
                0.05
            })
            .into(),
        ),
        text_color: if enabled { TEXT } else { MUTED },
        border: iced::Border {
            color: LINE,
            width: 1.0,
            radius: CONTROL_RADIUS.into(),
        },
        ..Default::default()
    })
    .id(format!("color-swatch-{key}"));
    let panel = editor.map(|editor| {
        crate::motion::enter(panel(editor), editor.key.clone(), Vector::new(0.0, -6.0))
    });
    Element::new(Picker {
        anchor: anchor.into(),
        open: panel.is_some(),
        panel: panel.unwrap_or_else(|| Space::new().into()),
    })
}

fn panel(editor: &Editor) -> Element<'static, Message> {
    let hsv = editor.hsv;
    let [r, g, b] = rgb(hsv);
    let field = controls::Control::new(Element::new(Field(hsv)), true, move |key| {
        let mut next = hsv;
        match key {
            Key::Named(Named::ArrowLeft) => next[1] -= 0.01,
            Key::Named(Named::ArrowRight) => next[1] += 0.01,
            Key::Named(Named::ArrowUp) => next[2] += 0.01,
            Key::Named(Named::ArrowDown) => next[2] -= 0.01,
            _ => return None,
        }
        Some(Message::ColorChanged(next.map(|v| v.clamp(0.0, 1.0))))
    })
    .id("color-field");
    let mut spectrum = iced::gradient::Linear::new(std::f32::consts::FRAC_PI_2);
    for i in 0..=6 {
        let [r, g, b] = rgb([i as f32 / 6.0, 1.0, 1.0]);
        spectrum = spectrum.add_stop(i as f32 / 6.0, Color::from_rgb(r, g, b));
    }
    let hue = controls::slider(0.0_f32..=1.0, hsv[0], move |h| {
        Message::ColorChanged([h, hsv[1], hsv[2]])
    })
    .step(1.0 / 360.0)
    .height(22)
    .style(|_, _| iced::widget::slider::Style {
        rail: iced::widget::slider::Rail {
            backgrounds: (Color::TRANSPARENT.into(), Color::TRANSPARENT.into()),
            width: 10.0,
            border: iced::border::rounded(5),
        },
        handle: iced::widget::slider::Handle {
            shape: iced::widget::slider::HandleShape::Circle { radius: 7.0 },
            background: Color::WHITE.into(),
            border_width: 2.0,
            border_color: BG,
        },
    });
    let hue = stack![
        container(
            container(Space::new().width(Length::Fill).height(10)).style(move |_| {
                container::Style {
                    background: Some(spectrum.into()),
                    border: iced::border::rounded(5),
                    ..Default::default()
                }
            })
        )
        .padding([0, 7])
        .center_y(22),
        hue
    ];
    container(
        column![
            row![
                text("选择颜色").size(14).width(Length::Fill),
                swatch(Color::from_rgb(r, g, b), 24.0)
            ]
            .align_y(iced::Alignment::Center),
            field,
            row![text("色相").size(12).color(MUTED), hue]
                .spacing(12)
                .align_y(iced::Alignment::Center),
            text("方向键微调明暗与饱和度").size(11).color(MUTED),
            row![
                crate::view::secondary("取消", Message::CloseColor).width(Length::Fill),
                crate::view::primary("完成", Message::SaveColor, true).width(Length::Fill)
            ]
            .spacing(10)
        ]
        .spacing(14),
    )
    .padding(18)
    .width(280)
    .style(popover)
    .into()
}

struct Field([f32; 3]);
impl Widget<Message, Theme, Renderer> for Field {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(156.0))
    }
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<bool>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(false)
    }
    fn layout(&mut self, _: &mut Tree, _: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.resolve(Length::Fill, Length::Fixed(156.0), Size::ZERO))
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _: &Rectangle,
    ) {
        let dragging = tree.state.downcast_mut::<bool>();
        let point = match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if cursor.is_over(layout.bounds()) =>
            {
                *dragging = true;
                cursor.position()
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if *dragging => Some(*position),
            Event::Touch(iced::touch::Event::FingerPressed { position, .. })
                if layout.bounds().contains(*position) =>
            {
                *dragging = true;
                Some(*position)
            }
            Event::Touch(iced::touch::Event::FingerMoved { position, .. }) if *dragging => {
                Some(*position)
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(
                iced::touch::Event::FingerLifted { .. } | iced::touch::Event::FingerLost { .. },
            ) if *dragging => {
                *dragging = false;
                shell.capture_event();
                None
            }
            _ => None,
        };
        if let Some(point) = point {
            let bounds = layout.bounds();
            shell.publish(Message::ColorChanged([
                self.0[0],
                ((point.x - bounds.x) / bounds.width.max(1.0)).clamp(0.0, 1.0),
                (1.0 - (point.y - bounds.y) / bounds.height.max(1.0)).clamp(0.0, 1.0),
            ]));
            shell.capture_event();
        }
    }
    fn draw(
        &self,
        _: &Tree,
        renderer: &mut Renderer,
        _: &Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        _: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let bounds = layout.bounds();
        let [r, g, b] = rgb([self.0[0], 1.0, 1.0]);
        let quad = renderer::Quad {
            bounds,
            border: iced::border::rounded(8),
            ..Default::default()
        };
        renderer.fill_quad(
            quad,
            iced::gradient::Linear::new(std::f32::consts::FRAC_PI_2)
                .add_stop(0.0, Color::WHITE)
                .add_stop(1.0, Color::from_rgb(r, g, b)),
        );
        renderer.fill_quad(
            quad,
            iced::gradient::Linear::new(std::f32::consts::PI)
                .add_stop(0.0, Color::TRANSPARENT)
                .add_stop(1.0, Color::BLACK),
        );
        let center = Point::new(
            bounds.x + self.0[1] * bounds.width,
            bounds.y + (1.0 - self.0[2]) * bounds.height,
        );
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle::new(center - Vector::new(6.0, 6.0), Size::new(12.0, 12.0)),
                border: iced::Border {
                    color: Color::WHITE,
                    width: 2.0,
                    radius: 6.0.into(),
                },
                shadow: iced::Shadow {
                    color: Color::BLACK.scale_alpha(0.6),
                    blur_radius: 3.0,
                    ..Default::default()
                },
                ..Default::default()
            },
            Color::TRANSPARENT,
        );
    }
    fn mouse_interaction(
        &self,
        _: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Rectangle,
        _: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::None
        }
    }
}

struct Picker<'a> {
    anchor: Element<'a, Message>,
    panel: Element<'a, Message>,
    open: bool,
}
impl Widget<Message, Theme, Renderer> for Picker<'_> {
    fn size(&self) -> Size<Length> {
        self.anchor.as_widget().size()
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.anchor), Tree::new(&self.panel)]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.anchor, &self.panel]);
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.anchor
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.anchor
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.anchor.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.anchor.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.anchor.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        _: &Renderer,
        _: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.open.then(|| {
            overlay::Element::new(Box::new(Popover {
                panel: &mut self.panel,
                tree: &mut tree.children[1],
                anchor: layout.bounds() + translation,
            }))
        })
    }
}

struct Popover<'a, 'b> {
    panel: &'a mut Element<'b, Message>,
    tree: &'a mut Tree,
    anchor: Rectangle,
}
impl iced::advanced::Overlay<Message, Theme, Renderer> for Popover<'_, '_> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let node = self.panel.as_widget_mut().layout(
            self.tree,
            renderer,
            &layout::Limits::new(Size::ZERO, bounds - Size::new(24.0, 24.0)),
        );
        let size = node.size();
        let y = if self.anchor.y + self.anchor.height + size.height + 8.0 <= bounds.height - 12.0 {
            self.anchor.y + self.anchor.height + 8.0
        } else {
            self.anchor.y - size.height - 8.0
        };
        node.move_to(Point::new(
            (self.anchor.x + self.anchor.width - size.width)
                .clamp(12.0, (bounds.width - size.width - 12.0).max(12.0)),
            y.clamp(12.0, (bounds.height - size.height - 12.0).max(12.0)),
        ))
    }
    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.panel
            .as_widget_mut()
            .operate(self.tree, layout, renderer, operation);
    }
    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let outside_press = matches!(event, Event::Mouse(mouse::Event::ButtonPressed(_)))
            && !cursor.is_over(layout.bounds());
        let outside_touch = matches!(event, Event::Touch(iced::touch::Event::FingerPressed { position, .. }) if !layout.bounds().contains(*position));
        if outside_press || outside_touch {
            shell.publish(Message::CloseColor);
            shell.capture_event();
            return;
        }
        self.panel.as_widget_mut().update(
            self.tree,
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        );
        if matches!(event, Event::Mouse(mouse::Event::WheelScrolled { .. })) {
            shell.capture_event();
        }
    }
    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.panel.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout,
            cursor,
            &layout.bounds().expand(32.0),
        );
    }
    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.panel.as_widget().mouse_interaction(
            self.tree,
            layout,
            cursor,
            &layout.bounds(),
            renderer,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hsv_preserves_color_and_bounds() {
        for value in [
            serde_json::json!([0.8, 0.2, 0.4]),
            serde_json::json!("0 0 0"),
            serde_json::json!([1, 1, 1]),
            serde_json::json!([0, 0.6, 0.9]),
        ] {
            let original = color(&value);
            let result = rgb(Editor::new("color".into(), &value).hsv);
            for (a, b) in result.into_iter().zip([original.r, original.g, original.b]) {
                assert!((a - b).abs() < 0.0001);
            }
        }
        assert_eq!(rgb([0.0, 1.0, 1.0]), rgb([1.0, 1.0, 1.0]));
        assert_eq!(
            color(&serde_json::json!([2, -1, 0.5])),
            Color::from_rgb(1.0, 0.0, 0.5)
        );
    }
}
