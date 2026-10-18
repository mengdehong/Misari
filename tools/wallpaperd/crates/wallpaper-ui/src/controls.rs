use crate::{motion, palette};
use iced::{
    Color, Element, Event, Length, Padding, Pixels, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
        widget::{Operation, Tree, operation::Focusable, tree},
    },
    keyboard::{self, Key, key::Named},
};
use std::{cell::Cell, marker::PhantomData, ops::RangeInclusive, rc::Rc, time::Instant};

/// Keep keyboard focus visible in its own scrollable ancestors, including
/// native text inputs. Chaining waits for Iced's complete focus operation.
pub fn focus_next<Message: Send + 'static>() -> iced::Task<Message> {
    iced::widget::operation::focus_next().chain(scroll_to_focused())
}

pub fn focus_previous<Message: Send + 'static>() -> iced::Task<Message> {
    iced::widget::operation::focus_previous().chain(scroll_to_focused())
}

pub fn scroll_to_focused<Message: Send + 'static>() -> iced::Task<Message> {
    iced::advanced::widget::operate(FindFocus::default())
}

/// Shared field metrics keep search, property editors and settings consistent.
pub fn text_input<'a, Message: Clone + 'a>(
    placeholder: &str,
    value: &str,
) -> iced::widget::TextInput<'a, Message> {
    iced::widget::text_input(placeholder, value)
        .size(13)
        .padding([11, 14])
        .style(|theme: &Theme, status| {
            use iced::widget::text_input::Status;
            let focused = matches!(status, Status::Focused { .. });
            let disabled = matches!(status, Status::Disabled);
            let mut style = iced::widget::text_input::default(theme, status);
            style.background = palette::WASH
                .scale_alpha(if focused { 0.12 } else { 0.07 })
                .into();
            style.border = iced::Border {
                color: if focused {
                    theme.palette().primary.scale_alpha(0.7)
                } else if matches!(status, Status::Hovered) {
                    palette::MUTED.scale_alpha(0.3)
                } else {
                    palette::LINE
                },
                width: 1.0,
                radius: palette::CONTROL_RADIUS.into(),
            };
            style.value = if disabled {
                palette::MUTED
            } else {
                palette::TEXT
            };
            style.placeholder = palette::MUTED.scale_alpha(0.75);
            style.selection = theme.palette().primary.scale_alpha(0.25);
            style
        })
}

#[derive(Clone)]
struct ScrollAncestor {
    index: usize,
    bounds: Rectangle,
    content: Rectangle,
    translation: Vector,
}

#[derive(Default)]
struct FindFocus {
    next_scroll: usize,
    pending: Option<ScrollAncestor>,
    ancestors: Vec<ScrollAncestor>,
    target: Option<(Rectangle, Vec<ScrollAncestor>)>,
}

impl<Message: Send + 'static> Operation<Message> for FindFocus {
    fn scrollable(
        &mut self,
        _id: Option<&iced::advanced::widget::Id>,
        bounds: Rectangle,
        content: Rectangle,
        translation: Vector,
        _state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        self.pending = Some(ScrollAncestor {
            index: self.next_scroll,
            bounds,
            content,
            translation,
        });
        self.next_scroll += 1;
    }
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Message>)) {
        let nested = self.pending.take();
        if let Some(ancestor) = &nested {
            self.ancestors.push(ancestor.clone());
        }
        operate(self);
        if nested.is_some() {
            self.ancestors.pop();
        }
    }
    fn focusable(
        &mut self,
        _id: Option<&iced::advanced::widget::Id>,
        bounds: Rectangle,
        state: &mut dyn Focusable,
    ) {
        if state.is_focused() {
            self.target = Some((bounds, self.ancestors.clone()));
        }
    }
    fn finish(&self) -> iced::advanced::widget::operation::Outcome<Message> {
        use iced::advanced::widget::operation::Outcome;
        let Some((target, ancestors)) = &self.target else {
            return Outcome::None;
        };
        let mut target = *target;
        let mut changes = Vec::new();
        // Inner scrolling changes the target's position seen by outer containers.
        for ancestor in ancestors.iter().rev() {
            target.x -= ancestor.translation.x;
            target.y -= ancestor.translation.y;
            let desired = Vector::new(
                reveal_delta(
                    ancestor.bounds.x,
                    ancestor.bounds.width,
                    target.x,
                    target.width,
                ),
                reveal_delta(
                    ancestor.bounds.y,
                    ancestor.bounds.height,
                    target.y,
                    target.height,
                ),
            );
            let delta = Vector::new(
                (ancestor.translation.x + desired.x).clamp(
                    0.0,
                    (ancestor.content.width - ancestor.bounds.width).max(0.0),
                ) - ancestor.translation.x,
                (ancestor.translation.y + desired.y).clamp(
                    0.0,
                    (ancestor.content.height - ancestor.bounds.height).max(0.0),
                ) - ancestor.translation.y,
            );
            target.x -= delta.x;
            target.y -= delta.y;
            if delta != Vector::ZERO {
                changes.push((ancestor.index, delta));
            }
        }
        if changes.is_empty() {
            Outcome::None
        } else {
            Outcome::Chain(Box::new(RevealFocus {
                next_scroll: 0,
                changes,
            }))
        }
    }
}

fn reveal_delta(start: f32, size: f32, target: f32, target_size: f32) -> f32 {
    let margin = 8.0_f32.min(size / 4.0);
    if target < start + margin || target_size > size - 2.0 * margin {
        target - start - margin
    } else if target + target_size > start + size - margin {
        target + target_size - start - size + margin
    } else {
        0.0
    }
}

struct RevealFocus {
    next_scroll: usize,
    changes: Vec<(usize, Vector)>,
}
impl<Message> Operation<Message> for RevealFocus {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Message>)) {
        operate(self);
    }
    fn scrollable(
        &mut self,
        _id: Option<&iced::advanced::widget::Id>,
        bounds: Rectangle,
        content: Rectangle,
        _translation: Vector,
        state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        if let Some((_, delta)) = self
            .changes
            .iter()
            .find(|(index, _)| *index == self.next_scroll)
        {
            state.scroll_by(
                iced::advanced::widget::operation::scrollable::AbsoluteOffset {
                    x: delta.x,
                    y: delta.y,
                },
                bounds,
                content,
            );
        }
        self.next_scroll += 1;
    }
}

// Keep the existing button style vocabulary available beside the constructor.
pub mod button {
    pub use iced::widget::button::{Status, Style, text};
}

pub struct Button<'a, Message> {
    inner: iced::widget::Button<'a, Message>,
    message: Option<Message>,
    id: Option<iced::advanced::widget::Id>,
    visual: Rc<Cell<motion::Visual>>,
    card: Option<(String, bool)>,
}

pub fn button<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message>>,
) -> Button<'a, Message> {
    let visual = Rc::new(Cell::new(motion::Visual::default()));
    Button {
        inner: iced::widget::button(content)
            .padding([10, 14])
            .style(animated_style(visual.clone(), |theme, status| {
                let mut style = iced::widget::button::primary(theme, status);
                style.border.radius = palette::CONTROL_RADIUS.into();
                style
            })),
        message: None,
        id: None,
        visual,
        card: None,
    }
}

impl<'a, Message: Clone + 'a> Button<'a, Message> {
    pub fn card(mut self, identity: impl Into<String>, selected: bool) -> Self {
        self.card = Some((identity.into(), selected));
        self
    }
    pub fn id(mut self, id: impl Into<iced::advanced::widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }
    pub fn on_press(self, message: Message) -> Self {
        self.on_press_maybe(Some(message))
    }
    pub fn on_press_maybe(mut self, message: Option<Message>) -> Self {
        self.inner = self.inner.on_press_maybe(message.clone());
        self.message = message;
        self
    }
    pub fn style(mut self, style: impl Fn(&Theme, button::Status) -> button::Style + 'a) -> Self {
        self.inner = self.inner.style(animated_style(self.visual.clone(), style));
        self
    }
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.inner = self.inner.padding(padding);
        self
    }
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.inner = self.inner.width(width);
        self
    }
}

impl<'a, Message: Clone + 'a> From<Button<'a, Message>> for Element<'a, Message> {
    fn from(button: Button<'a, Message>) -> Self {
        let enabled = button.message.is_some();
        let mut control = Control::new(button.inner.into(), enabled, move |key| {
            activate_key(key).then(|| button.message.clone()).flatten()
        });
        control.id = button.id;
        control.visual = Some(button.visual);
        control.card = button.card;
        control.into()
    }
}

fn animated_style<'a>(
    visual: Rc<Cell<motion::Visual>>,
    style: impl Fn(&Theme, button::Status) -> button::Style + 'a,
) -> impl Fn(&Theme, button::Status) -> button::Style + 'a {
    move |theme, status| {
        if status == button::Status::Disabled {
            return style(theme, status);
        }
        let phase = visual.get();
        let hovered = blend_style(
            style(theme, button::Status::Active),
            style(theme, button::Status::Hovered),
            phase.hover,
        );
        let mut result = blend_style(hovered, style(theme, button::Status::Pressed), phase.press);
        if phase.card {
            result.border.color = theme
                .palette()
                .primary
                .scale_alpha(0.85 * phase.selection.clamp(0.0, 1.0));
            let hover = phase.hover.clamp(0.0, 1.0);
            result.shadow = iced::Shadow {
                color: iced::Color::BLACK.scale_alpha(0.22 * hover),
                offset: Vector::new(0.0, 3.0 * hover),
                blur_radius: 12.0 * hover,
            };
        }
        result
    }
}

fn blend_style(a: button::Style, b: button::Style, t: f32) -> button::Style {
    use iced::{Background, Color};
    let background = match (a.background, b.background) {
        (None, None) => None,
        (Some(Background::Color(a)), Some(Background::Color(b))) => {
            Some(motion::color(a, b, t).into())
        }
        (Some(Background::Color(a)), None) => Some(motion::color(a, Color::TRANSPARENT, t).into()),
        (None, Some(Background::Color(b))) => Some(motion::color(Color::TRANSPARENT, b, t).into()),
        _ => {
            if t < 0.5 {
                a.background
            } else {
                b.background
            }
        }
    };
    button::Style {
        background,
        text_color: motion::color(a.text_color, b.text_color, t),
        border: iced::Border {
            color: motion::color(a.border.color, b.border.color, t),
            width: motion::mix(a.border.width, b.border.width, t),
            radius: iced::border::Radius {
                top_left: motion::mix(a.border.radius.top_left, b.border.radius.top_left, t),
                top_right: motion::mix(a.border.radius.top_right, b.border.radius.top_right, t),
                bottom_left: motion::mix(
                    a.border.radius.bottom_left,
                    b.border.radius.bottom_left,
                    t,
                ),
                bottom_right: motion::mix(
                    a.border.radius.bottom_right,
                    b.border.radius.bottom_right,
                    t,
                ),
            },
        },
        shadow: iced::Shadow {
            color: motion::color(a.shadow.color, b.shadow.color, t),
            offset: a.shadow.offset + (b.shadow.offset - a.shadow.offset) * t.clamp(0.0, 1.0),
            blur_radius: motion::mix(a.shadow.blur_radius, b.shadow.blur_radius, t),
        },
        snap: false,
    }
}

pub struct Checkbox<'a, Message> {
    inner: iced::widget::Checkbox<'a, Message>,
    value: bool,
    handler: Option<Rc<dyn Fn(bool) -> Message + 'a>>,
}

pub fn checkbox<'a, Message: Clone + 'a>(value: bool) -> Checkbox<'a, Message> {
    Checkbox {
        inner: iced::widget::checkbox(value)
            .size(18)
            .spacing(12)
            .text_size(13)
            .width(Length::Fill)
            .style(|theme: &Theme, status| {
                use iced::widget::checkbox::Status;
                let (checked, hovered, disabled) = match status {
                    Status::Active { is_checked } => (is_checked, false, false),
                    Status::Hovered { is_checked } => (is_checked, true, false),
                    Status::Disabled { is_checked } => (is_checked, false, true),
                };
                iced::widget::checkbox::Style {
                    background: if checked {
                        theme
                            .palette()
                            .primary
                            .scale_alpha(if disabled { 0.3 } else { 1.0 })
                    } else {
                        palette::WASH.scale_alpha(if hovered { 0.20 } else { 0.08 })
                    }
                    .into(),
                    icon_color: palette::BG,
                    border: iced::Border {
                        color: if checked {
                            Color::TRANSPARENT
                        } else {
                            palette::MUTED.scale_alpha(if disabled { 0.18 } else { 0.45 })
                        },
                        width: 1.0,
                        radius: 5.0.into(),
                    },
                    text_color: Some(if disabled {
                        palette::MUTED
                    } else {
                        palette::TEXT
                    }),
                }
            }),
        value,
        handler: None,
    }
}

impl<'a, Message: Clone + 'a> Checkbox<'a, Message> {
    pub fn label(mut self, label: impl iced::advanced::text::IntoFragment<'a>) -> Self {
        self.inner = self.inner.label(label);
        self
    }
    pub fn on_toggle(mut self, handler: impl Fn(bool) -> Message + 'a) -> Self {
        let handler: Rc<dyn Fn(bool) -> Message + 'a> = Rc::new(handler);
        self.handler = Some(handler.clone());
        self.inner = self.inner.on_toggle(move |value| handler(value));
        self
    }
    pub fn text_size(mut self, size: impl Into<Pixels>) -> Self {
        self.inner = self.inner.text_size(size);
        self
    }
}

impl<'a, Message: Clone + 'a> From<Checkbox<'a, Message>> for Element<'a, Message> {
    fn from(checkbox: Checkbox<'a, Message>) -> Self {
        let enabled = checkbox.handler.is_some();
        let toggled = checkbox
            .handler
            .as_ref()
            .map(|handler| handler(!checkbox.value));
        let row = iced::widget::button(checkbox.inner)
            .padding([10, 12])
            .width(Length::Fill)
            .on_press_maybe(toggled)
            .style(move |theme: &Theme, status| {
                let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
                button::Style {
                    background: Some(
                        if checkbox.value {
                            theme
                                .palette()
                                .primary
                                .scale_alpha(if hovered { 0.13 } else { 0.07 })
                        } else {
                            palette::WASH.scale_alpha(if hovered { 0.14 } else { 0.05 })
                        }
                        .into(),
                    ),
                    text_color: palette::TEXT,
                    border: iced::Border {
                        radius: palette::CONTROL_RADIUS.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            });
        Control::new(row.into(), enabled, move |key| {
            activate_key(key)
                .then(|| {
                    checkbox
                        .handler
                        .as_ref()
                        .map(|handler| handler(!checkbox.value))
                })
                .flatten()
        })
        .into()
    }
}

pub struct PickList<'a, T: ToString + PartialEq + Clone, Message> {
    inner: iced::widget::PickList<'a, T, Vec<T>, T, Message>,
    options: Vec<T>,
    selected: Option<T>,
    handler: Rc<dyn Fn(T) -> Message + 'a>,
}

pub fn pick_list<'a, T: ToString + PartialEq + Clone + 'a, Message: Clone + 'a>(
    options: Vec<T>,
    selected: Option<T>,
    handler: impl Fn(T) -> Message + 'a,
) -> PickList<'a, T, Message> {
    let handler: Rc<dyn Fn(T) -> Message + 'a> = Rc::new(handler);
    let callback = handler.clone();
    PickList {
        inner: iced::widget::pick_list(options.clone(), selected.clone(), move |option| {
            callback(option)
        })
        .text_size(13)
        .padding([10, 12])
        .style(|theme: &Theme, status| iced::widget::pick_list::Style {
            text_color: crate::palette::TEXT,
            placeholder_color: crate::palette::MUTED,
            handle_color: crate::palette::MUTED,
            background: palette::WASH
                .scale_alpha(
                    if matches!(
                        status,
                        iced::widget::pick_list::Status::Hovered
                            | iced::widget::pick_list::Status::Opened { .. }
                    ) {
                        0.17
                    } else {
                        0.09
                    },
                )
                .into(),
            border: iced::Border {
                color: if matches!(status, iced::widget::pick_list::Status::Opened { .. }) {
                    theme.palette().primary.scale_alpha(0.5)
                } else {
                    palette::LINE
                },
                width: 1.0,
                radius: palette::CONTROL_RADIUS.into(),
            },
        })
        .menu_style(|theme| iced::widget::overlay::menu::Style {
            background: palette::popover(theme).background.unwrap(),
            text_color: crate::palette::TEXT,
            border: iced::Border {
                color: palette::WASH.scale_alpha(0.38),
                width: 1.0,
                radius: 12.0.into(),
            },
            selected_background: theme.palette().primary.scale_alpha(0.12).into(),
            selected_text_color: theme.palette().primary,
            shadow: palette::popover(theme).shadow,
        }),
        options,
        selected,
        handler,
    }
}

impl<'a, T: ToString + PartialEq + Clone + 'a, Message: Clone + 'a> PickList<'a, T, Message> {
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.inner = self.inner.padding(padding);
        self
    }
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.inner = self.inner.width(width);
        self
    }
    pub fn text_size(mut self, size: impl Into<Pixels>) -> Self {
        self.inner = self.inner.text_size(size);
        self
    }
}

fn selection_index<T: PartialEq>(options: &[T], selected: Option<&T>, key: &Key) -> Option<usize> {
    if options.is_empty() {
        return None;
    }
    let current = selected.and_then(|value| options.iter().position(|option| option == value));
    match key {
        Key::Named(Named::ArrowDown | Named::ArrowRight) => {
            Some(current.map_or(0, |index| (index + 1).min(options.len() - 1)))
        }
        Key::Named(Named::ArrowUp | Named::ArrowLeft) => {
            Some(current.map_or(options.len() - 1, |index| index.saturating_sub(1)))
        }
        Key::Named(Named::Home) => Some(0),
        Key::Named(Named::End) => Some(options.len() - 1),
        _ => None,
    }
}

impl<'a, T: ToString + PartialEq + Clone + 'a, Message: Clone + 'a> From<PickList<'a, T, Message>>
    for Element<'a, Message>
{
    fn from(pick: PickList<'a, T, Message>) -> Self {
        let enabled = !pick.options.is_empty();
        let mut control = Control::new(pick.inner.into(), enabled, move |key| {
            selection_index(&pick.options, pick.selected.as_ref(), key)
                .map(|index| (pick.handler)(pick.options[index].clone()))
        });
        control.popup = true;
        control.into()
    }
}

fn activate_key(key: &Key) -> bool {
    matches!(key, Key::Named(Named::Enter | Named::Space))
}

pub trait Number: Copy {
    fn number(self) -> f64;
    fn from_number(number: f64) -> Self;
}
impl Number for f64 {
    fn number(self) -> f64 {
        self
    }
    fn from_number(number: f64) -> Self {
        number
    }
}
impl Number for f32 {
    fn number(self) -> f64 {
        f64::from(self)
    }
    fn from_number(number: f64) -> Self {
        number as f32
    }
}
impl Number for u32 {
    fn number(self) -> f64 {
        f64::from(self)
    }
    fn from_number(number: f64) -> Self {
        number.round().clamp(0.0, f64::from(u32::MAX)) as u32
    }
}

pub struct Slider<'a, T, Message> {
    inner: iced::widget::Slider<'a, f64, Message>,
    range: RangeInclusive<f64>,
    value: f64,
    step: f64,
    handler: Rc<dyn Fn(f64) -> Message + 'a>,
    number: PhantomData<T>,
}

pub fn slider<'a, T: Number + 'a, Message: Clone + 'a>(
    range: RangeInclusive<T>,
    value: T,
    handler: impl Fn(T) -> Message + 'a,
) -> Slider<'a, T, Message> {
    let range = range.start().number()..=range.end().number();
    let value = value.number();
    let handler: Rc<dyn Fn(f64) -> Message + 'a> =
        Rc::new(move |value| handler(T::from_number(value)));
    let callback = handler.clone();
    Slider {
        inner: iced::widget::slider(range.clone(), value, move |value| callback(value))
            .height(28)
            .style(|theme: &Theme, status| {
                let active = !matches!(status, iced::widget::slider::Status::Active);
                iced::widget::slider::Style {
                    rail: iced::widget::slider::Rail {
                        backgrounds: (
                            theme.palette().primary.into(),
                            palette::WASH.scale_alpha(0.24).into(),
                        ),
                        width: 4.0,
                        border: iced::border::rounded(2),
                    },
                    handle: iced::widget::slider::Handle {
                        shape: iced::widget::slider::HandleShape::Circle {
                            radius: if active { 7.0 } else { 6.0 },
                        },
                        background: palette::TEXT.into(),
                        border_width: 2.0,
                        border_color: theme.palette().primary,
                    },
                }
            }),
        range,
        value,
        step: 1.0,
        handler,
        number: PhantomData,
    }
}

impl<'a, T: Number + 'a, Message: Clone + 'a> Slider<'a, T, Message> {
    pub fn style(
        mut self,
        style: impl Fn(&Theme, iced::widget::slider::Status) -> iced::widget::slider::Style + 'a,
    ) -> Self {
        self.inner = self.inner.style(style);
        self
    }
    pub fn step(mut self, step: T) -> Self {
        self.step = step.number().abs().max(f64::EPSILON);
        self.inner = self.inner.step(self.step);
        self
    }
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.inner = self.inner.width(width);
        self
    }
    pub fn height(mut self, height: impl Into<Pixels>) -> Self {
        self.inner = self.inner.height(height);
        self
    }
}

fn slider_value(range: &RangeInclusive<f64>, value: f64, step: f64, key: &Key) -> Option<f64> {
    let value = match key {
        Key::Named(Named::ArrowUp | Named::ArrowRight) => value + step,
        Key::Named(Named::ArrowDown | Named::ArrowLeft) => value - step,
        Key::Named(Named::PageUp) => value + step * 10.0,
        Key::Named(Named::PageDown) => value - step * 10.0,
        Key::Named(Named::Home) => *range.start(),
        Key::Named(Named::End) => *range.end(),
        _ => return None,
    };
    Some(value.clamp(*range.start(), *range.end()))
}

impl<'a, T: Number + 'a, Message: Clone + 'a> From<Slider<'a, T, Message>>
    for Element<'a, Message>
{
    fn from(slider: Slider<'a, T, Message>) -> Self {
        Control::new(slider.inner.into(), true, move |key| {
            slider_value(&slider.range, slider.value, slider.step, key)
                .map(|value| (slider.handler)(value))
        })
        .into()
    }
}

#[derive(Default)]
struct Focus {
    focused: bool,
    focus_visible: bool,
    motion: motion::Interaction,
    identity: Option<String>,
}
impl Focusable for Focus {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
        self.focus_visible = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
        self.focus_visible = false;
    }
}

type KeyHandler<'a, Message> = dyn Fn(&Key) -> Option<Message> + 'a;

pub(crate) struct Control<'a, Message> {
    inner: Element<'a, Message>,
    id: Option<iced::advanced::widget::Id>,
    enabled: bool,
    key: Box<KeyHandler<'a, Message>>,
    popup: bool,
    visual: Option<Rc<Cell<motion::Visual>>>,
    card: Option<(String, bool)>,
}

impl<'a, Message> Control<'a, Message> {
    pub(crate) fn new(
        inner: Element<'a, Message>,
        enabled: bool,
        key: impl Fn(&Key) -> Option<Message> + 'a,
    ) -> Self {
        Self {
            inner,
            id: None,
            enabled,
            key: Box::new(key),
            popup: false,
            visual: None,
            card: None,
        }
    }
    pub(crate) fn id(mut self, id: &'static str) -> Self {
        self.id = Some(id.into());
        self
    }
}

impl<Message> Widget<Message, Theme, Renderer> for Control<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Focus>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(Focus {
            motion: motion::Interaction::selected(
                self.card.as_ref().is_some_and(|(_, selected)| *selected),
            ),
            identity: self.card.as_ref().map(|(id, _)| id.clone()),
            ..Default::default()
        })
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.inner)]
    }
    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<Focus>();
        if state.identity.as_deref() != self.card.as_ref().map(|(id, _)| id.as_str()) {
            *state = Focus {
                motion: motion::Interaction::selected(
                    self.card.as_ref().is_some_and(|(_, selected)| *selected),
                ),
                identity: self.card.as_ref().map(|(id, _)| id.clone()),
                ..Default::default()
            };
            tree.children[0] = Tree::new(&self.inner);
        }
        tree.diff_children(std::slice::from_ref(&self.inner));
        if !self.enabled {
            tree.state.downcast_mut::<Focus>().unfocus();
        }
    }
    fn size(&self) -> Size<Length> {
        self.inner.as_widget().size()
    }
    fn size_hint(&self) -> Size<Length> {
        self.inner.as_widget().size_hint()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.inner
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
        if self.enabled {
            operation.focusable(
                self.id.as_ref(),
                layout.bounds(),
                tree.state.downcast_mut::<Focus>(),
            );
        }
        self.inner
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
        let state = tree.state.downcast_mut::<Focus>();
        let (now, frame) = if let Event::Window(iced::window::Event::RedrawRequested(now)) = event {
            (*now, true)
        } else {
            (Instant::now(), false)
        };
        let pressed = match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                Some(cursor.position())
            }
            Event::Touch(iced::touch::Event::FingerPressed { position, .. }) => {
                Some(Some(*position))
            }
            _ => None,
        };
        if let Some(position) = pressed {
            if state.focus_visible {
                state.focus_visible = false;
                shell.request_redraw();
            }
            let focused = self.enabled
                && !shell.is_event_captured()
                && position.is_some_and(|position| {
                    layout
                        .bounds()
                        .intersection(viewport)
                        .is_some_and(|visible| visible.contains(position))
                });
            if state.focused != focused {
                state.focused = focused;
                shell.request_redraw();
            }
            if focused && self.visual.is_some() {
                state.motion.press(now);
            }
        }
        match event {
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(
                iced::touch::Event::FingerLifted { .. } | iced::touch::Event::FingerLost { .. },
            ) => state.motion.down = false,
            Event::Window(iced::window::Event::Unfocused) => {
                state.motion.cancel_press();
                if self.visual.is_some() {
                    // Cancel the native press too, so releasing after a focus change cannot click.
                    self.inner.as_widget_mut().update(
                        &mut tree.children[0],
                        &Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                        layout,
                        mouse::Cursor::Unavailable,
                        renderer,
                        clipboard,
                        &mut Shell::new(&mut Vec::new()),
                        viewport,
                    );
                }
            }
            Event::Keyboard(keyboard::Event::KeyReleased { key, .. }) if activate_key(key) => {
                state.motion.down = false
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                repeat: false,
                ..
            }) if state.focused
                && self.enabled
                && self.visual.is_some()
                && activate_key(key)
                && !modifiers.control()
                && !modifiers.alt()
                && !modifiers.logo()
                && !shell.is_event_captured() =>
            {
                state.motion.press(now)
            }
            _ => {}
        }
        let hovered = !matches!(event, Event::Window(iced::window::Event::Unfocused))
            && layout
                .bounds()
                .intersection(viewport)
                .is_some_and(|bounds| cursor.is_over(bounds));
        let selected = self.card.as_ref().is_some_and(|(_, selected)| *selected);
        if state.focused
            && !shell.is_event_captured()
            && matches!(event, Event::Keyboard(keyboard::Event::KeyPressed { modifiers, .. })
                if !modifiers.control() && !modifiers.alt() && !modifiers.logo())
        {
            state.focus_visible = true;
        }
        if state.motion.update(
            now,
            frame,
            hovered,
            state.focused && state.focus_visible,
            selected,
            self.enabled,
        ) {
            shell.request_redraw();
        }
        if state.focused
            && self.enabled
            && !shell.is_event_captured()
            && let Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                repeat,
                ..
            }) = event
        {
            let unmodified = !modifiers.control() && !modifiers.alt() && !modifiers.logo();
            if self.popup {
                if !repeat || !activate_key(key) {
                    // Native pick-list overlays are mouse-only. Reuse their
                    // open/close transition, while arrows select through the
                    // controlled value and Tab/Escape close the overlay.
                    let popup_cursor = if activate_key(key) && unmodified {
                        mouse::Cursor::Available(layout.bounds().center())
                    } else {
                        mouse::Cursor::Unavailable
                    };
                    self.inner.as_widget_mut().update(
                        &mut tree.children[0],
                        &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                        layout,
                        popup_cursor,
                        renderer,
                        clipboard,
                        &mut Shell::new(&mut Vec::new()),
                        viewport,
                    );
                    shell.request_redraw();
                }
                if activate_key(key) && unmodified {
                    shell.capture_event();
                    return;
                }
            }
            if unmodified && let Some(message) = (self.key)(key) {
                if !repeat || !activate_key(key) {
                    shell.publish(message);
                }
                shell.capture_event();
                shell.request_redraw();
                return;
            }
        }
        // The standard slider accepts arrows while hovered. Keyboard actions
        // here belong exclusively to the control reached through focus.
        if matches!(
            event,
            Event::Keyboard(
                keyboard::Event::KeyPressed { .. } | keyboard::Event::KeyReleased { .. }
            )
        ) {
            return;
        }
        self.inner.as_widget_mut().update(
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
        use iced::advanced::Renderer as _;
        let state = tree.state.downcast_ref::<Focus>();
        let focus_opacity = if state.focus_visible {
            state.motion.focus.value
        } else {
            0.0
        };
        let mut visual = state.motion.visual();
        visual.card = self.card.is_some();
        if let Some(paint) = &self.visual {
            paint.set(visual);
        }
        let transform = if self.visual.is_some() && !motion::reduced() {
            let (scale, lift) = if visual.card {
                (
                    1.0 + 0.008 * visual.hover - 0.016 * visual.press,
                    -3.0 * visual.hover + 1.5 * visual.press,
                )
            } else {
                (1.0 - 0.025 * visual.press, 0.8 * visual.press)
            };
            motion::transform(layout.bounds(), scale, lift)
        } else {
            iced::Transformation::IDENTITY
        };
        if transform == iced::Transformation::IDENTITY && focus_opacity <= 0.001 {
            self.inner.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
            return;
        }
        renderer.with_layer(*viewport, |renderer| {
            renderer.with_transformation(transform, |renderer| {
                self.inner.as_widget().draw(
                    &tree.children[0],
                    renderer,
                    theme,
                    style,
                    layout,
                    cursor,
                    viewport,
                );
                if self.enabled && focus_opacity > 0.001 {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: layout.bounds(),
                            border: iced::Border {
                                color: theme
                                    .palette()
                                    .primary
                                    .scale_alpha(focus_opacity.clamp(0.0, 1.0)),
                                width: 2.0,
                                radius: 6.0.into(),
                            },
                            ..Default::default()
                        },
                        iced::Color::TRANSPARENT,
                    );
                }
            })
        });
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.inner.as_widget().mouse_interaction(
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
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.inner.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Control<'a, Message>> for Element<'a, Message> {
    fn from(control: Control<'a, Message>) -> Self {
        Element::new(control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_and_keyboard_selection_preserve_boundaries() {
        let mut state = Focus::default();
        assert!(!state.is_focused());
        state.focus();
        assert!(state.is_focused());
        state.unfocus();
        assert!(!state.is_focused());
        let options = [1, 2, 3];
        assert_eq!(
            selection_index(&options, Some(&2), &Key::Named(Named::ArrowDown)),
            Some(2)
        );
        assert_eq!(
            selection_index(&options, Some(&3), &Key::Named(Named::ArrowDown)),
            Some(2)
        );
        assert_eq!(
            selection_index(&options, None, &Key::Named(Named::ArrowUp)),
            Some(2)
        );
        assert_eq!(
            selection_index::<u8>(&[], None, &Key::Named(Named::Home)),
            None
        );
        assert!(!activate_key(&Key::Named(Named::Tab)));
        assert!(activate_key(&Key::Named(Named::Space)));
        assert_eq!(
            slider_value(&(0.0..=100.0), 99.0, 2.0, &Key::Named(Named::ArrowRight)),
            Some(100.0)
        );
        assert_eq!(
            slider_value(&(0.0..=100.0), 25.0, 2.0, &Key::Named(Named::PageDown)),
            Some(5.0)
        );
        assert_eq!(u32::from_number(30.6), 31);
    }
}
