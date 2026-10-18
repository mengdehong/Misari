use iced::{
    Element, Event, Length, Padding, Pixels, Rectangle, Renderer, Size, Theme, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
        widget::{Operation, Tree, operation::Focusable, tree},
    },
    keyboard::{self, Key, key::Named},
};
use std::{marker::PhantomData, ops::RangeInclusive, rc::Rc};

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
}

pub fn button<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message>>,
) -> Button<'a, Message> {
    Button {
        inner: iced::widget::button(content),
        message: None,
    }
}

impl<'a, Message: Clone + 'a> Button<'a, Message> {
    pub fn on_press(self, message: Message) -> Self {
        self.on_press_maybe(Some(message))
    }
    pub fn on_press_maybe(mut self, message: Option<Message>) -> Self {
        self.inner = self.inner.on_press_maybe(message.clone());
        self.message = message;
        self
    }
    pub fn style(mut self, style: impl Fn(&Theme, button::Status) -> button::Style + 'a) -> Self {
        self.inner = self.inner.style(style);
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
        Control::new(button.inner.into(), enabled, move |key| {
            activate_key(key).then(|| button.message.clone()).flatten()
        })
        .into()
    }
}

pub struct Checkbox<'a, Message> {
    inner: iced::widget::Checkbox<'a, Message>,
    value: bool,
    handler: Option<Rc<dyn Fn(bool) -> Message + 'a>>,
}

pub fn checkbox<'a, Message: Clone + 'a>(value: bool) -> Checkbox<'a, Message> {
    Checkbox {
        inner: iced::widget::checkbox(value),
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
        Control::new(checkbox.inner.into(), enabled, move |key| {
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
        inner: iced::widget::slider(range.clone(), value, move |value| callback(value)),
        range,
        value,
        step: 1.0,
        handler,
        number: PhantomData,
    }
}

impl<'a, T: Number + 'a, Message: Clone + 'a> Slider<'a, T, Message> {
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
}
impl Focusable for Focus {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
    }
}

type KeyHandler<'a, Message> = dyn Fn(&Key) -> Option<Message> + 'a;

struct Control<'a, Message> {
    inner: Element<'a, Message>,
    enabled: bool,
    key: Box<KeyHandler<'a, Message>>,
    popup: bool,
}

impl<'a, Message> Control<'a, Message> {
    fn new(
        inner: Element<'a, Message>,
        enabled: bool,
        key: impl Fn(&Key) -> Option<Message> + 'a,
    ) -> Self {
        Self {
            inner,
            enabled,
            key: Box::new(key),
            popup: false,
        }
    }
}

impl<Message> Widget<Message, Theme, Renderer> for Control<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Focus>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(Focus::default())
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.inner)]
    }
    fn diff(&self, tree: &mut Tree) {
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
            operation.focusable(None, layout.bounds(), tree.state.downcast_mut::<Focus>());
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
        self.inner.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        if self.enabled && tree.state.downcast_ref::<Focus>().focused {
            use iced::advanced::Renderer as _;
            renderer.with_layer(*viewport, |renderer| {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: layout.bounds(),
                        border: iced::Border {
                            color: iced::Color::from_rgb8(167, 224, 197),
                            width: 2.0,
                            radius: 6.0.into(),
                        },
                        ..Default::default()
                    },
                    iced::Color::TRANSPARENT,
                )
            });
        }
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

    #[test]
    fn widget_tree_registers_enabled_controls_and_publishes_once() {
        use iced::advanced::widget::operation::{self, Outcome, focusable};
        let mut element: Element<'_, u8> = iced::widget::row![
            button("Enabled").on_press(1),
            button("Disabled"),
            checkbox(false).on_toggle(|_| 2),
            pick_list(vec![1, 2], Some(1), |_| 3),
            slider(0_u32..=100, 30, |_| 4),
        ]
        .into();
        let mut tree = Tree::new(&element);
        let renderer = Renderer::new(iced::Font::DEFAULT, Pixels(16.0));
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(800.0, 120.0)),
        );
        let layout = Layout::new(&node);
        let mut count = focusable::count();
        element.as_widget_mut().operate(
            &mut tree,
            layout,
            &renderer,
            &mut operation::black_box(&mut count),
        );
        let Outcome::Some(count) = count.finish() else {
            panic!("focus count");
        };
        assert_eq!(count.total, 4);
        let mut focus: Box<dyn Operation> = Box::new(focusable::focus_next::<()>());
        loop {
            element
                .as_widget_mut()
                .operate(&mut tree, layout, &renderer, focus.as_mut());
            if let Outcome::Chain(next) = focus.finish() {
                focus = next;
            } else {
                break;
            }
        }
        assert!(tree.children[0].state.downcast_ref::<Focus>().is_focused());
        let event = |key, repeat| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(key),
                modified_key: Key::Named(key),
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Enter),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat,
            })
        };
        let mut messages = Vec::new();
        let viewport = Rectangle::with_size(Size::new(800.0, 120.0));
        element.as_widget_mut().update(
            &mut tree,
            &event(Named::Enter, false),
            layout,
            mouse::Cursor::Unavailable,
            &renderer,
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut messages),
            &viewport,
        );
        element.as_widget_mut().update(
            &mut tree,
            &event(Named::Enter, true),
            layout,
            mouse::Cursor::Unavailable,
            &renderer,
            &mut iced::advanced::clipboard::Null,
            &mut Shell::new(&mut messages),
            &viewport,
        );
        assert_eq!(messages, [1]);
        tree.children[0].state.downcast_mut::<Focus>().unfocus();
        tree.children[3].state.downcast_mut::<Focus>().focus();
        for (key, popup_open) in [
            (Named::Enter, true),
            (Named::Tab, false),
            (Named::ArrowDown, false),
        ] {
            element.as_widget_mut().update(
                &mut tree,
                &event(key, false),
                layout,
                mouse::Cursor::Unavailable,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(&mut messages),
                &viewport,
            );
            assert_eq!(
                element
                    .as_widget_mut()
                    .overlay(&mut tree, layout, &renderer, &viewport, Vector::ZERO)
                    .is_some(),
                popup_open
            );
        }
        assert_eq!(messages, [1, 3]);
    }

    #[test]
    fn focus_reveals_native_input_in_nested_scrollables_and_leaves_sibling_alone() {
        use iced::advanced::widget::operation::{self, Outcome, focusable};
        use iced::widget::{Space, column, row, scrollable, text_input};
        let mut element: Element<'_, u8> = row![
            scrollable(column![
                Space::new().height(400),
                scrollable(column![
                    Space::new().height(500),
                    text_input("Target", "")
                        .id("offscreen-input")
                        .on_input(|_| 0)
                ])
                .height(120)
            ])
            .width(300)
            .height(100),
            scrollable(column![
                Space::new().height(800),
                button("Sibling").on_press(1)
            ])
            .width(300)
            .height(100)
        ]
        .into();
        let mut tree = Tree::new(&element);
        let renderer = Renderer::new(iced::Font::DEFAULT, Pixels(16.0));
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(800.0, 160.0)),
        );
        let layout = Layout::new(&node);
        let run =
            |element: &mut Element<'_, u8>, tree: &mut Tree, mut operation: Box<dyn Operation>| {
                loop {
                    element
                        .as_widget_mut()
                        .operate(tree, layout, &renderer, operation.as_mut());
                    if let Outcome::Chain(next) = operation.finish() {
                        operation = next;
                    } else {
                        break;
                    }
                }
            };
        #[derive(Default)]
        struct Positions(Vec<Vector>);
        impl Operation for Positions {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn scrollable(
                &mut self,
                _: Option<&iced::advanced::widget::Id>,
                _: Rectangle,
                _: Rectangle,
                translation: Vector,
                _: &mut dyn operation::Scrollable,
            ) {
                self.0.push(translation);
            }
        }
        run(
            &mut element,
            &mut tree,
            Box::new(focusable::focus::<()>("offscreen-input".into())),
        );
        run(&mut element, &mut tree, Box::<FindFocus>::default());
        let mut positions = Positions::default();
        element
            .as_widget_mut()
            .operate(&mut tree, layout, &renderer, &mut positions);
        assert_eq!(positions.0.len(), 3);
        assert!(positions.0[0].y > 0.0);
        assert!(positions.0[1].y > 0.0);
        assert_eq!(positions.0[2], Vector::ZERO);
        let target_y = 900.0 - positions.0[0].y - positions.0[1].y;
        assert!(
            (0.0..100.0).contains(&target_y),
            "focused input remains offscreen: {target_y}"
        );
    }
}
