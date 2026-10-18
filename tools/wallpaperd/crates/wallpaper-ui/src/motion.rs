//! Small, interruptible motions. Widgets request frames only until they settle.
use iced::{Color, Transformation};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

static REDUCED: AtomicBool = AtomicBool::new(false);

pub fn set_reduced(reduced: bool) {
    REDUCED.store(reduced, Ordering::Relaxed);
}

pub fn reduced() -> bool {
    REDUCED.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Spring {
    pub value: f32,
    velocity: f32,
    pub target: f32,
}

impl Spring {
    pub fn at(value: f32) -> Self {
        Self {
            value,
            target: value,
            velocity: 0.0,
        }
    }

    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < 0.001 && self.velocity.abs() < 0.005
    }

    /// Exact damped-oscillator solution; frame rate and interrupted targets do not reset velocity.
    pub fn advance(&mut self, dt: f32, frequency: f32, damping: f32) {
        if dt <= 0.0 {
            return;
        }
        let x = self.value - self.target;
        let decay = (-damping * frequency * dt).exp();
        let (position, velocity) = if damping == 1.0 {
            let c = self.velocity + frequency * x;
            let position = (x + c * dt) * decay;
            (position, (self.velocity - frequency * c * dt) * decay)
        } else {
            let w = frequency * (1.0 - damping * damping).sqrt();
            let (sin, cos) = (w * dt).sin_cos();
            (
                (x * cos + (self.velocity + damping * frequency * x) * sin / w) * decay,
                (self.velocity * cos
                    - (damping * frequency * self.velocity + frequency * frequency * x) * sin / w)
                    * decay,
            )
        };
        self.value = self.target + position;
        self.velocity = velocity;
        if self.settled() {
            *self = Self::at(self.target);
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Visual {
    pub hover: f32,
    pub press: f32,
    pub selection: f32,
    pub card: bool,
}

#[derive(Default)]
pub struct Interaction {
    hover: Spring,
    press: Spring,
    pub focus: Spring,
    selection: Spring,
    last_frame: Option<Instant>,
    press_until: Option<Instant>,
    pub down: bool,
}

impl Interaction {
    pub fn selected(selected: bool) -> Self {
        Self {
            selection: Spring::at(f32::from(selected)),
            ..Self::default()
        }
    }

    pub fn press(&mut self, now: Instant) {
        self.down = true;
        // A fast click must still have a visible compression before the release.
        self.press_until = Some(now + std::time::Duration::from_millis(55));
    }

    pub fn cancel_press(&mut self) {
        self.down = false;
        self.press_until = None;
    }

    pub fn visual(&self) -> Visual {
        Visual {
            hover: self.hover.value,
            press: self.press.value,
            selection: self.selection.value,
            card: false,
        }
    }

    pub fn update(
        &mut self,
        now: Instant,
        frame: bool,
        hovered: bool,
        focused: bool,
        selected: bool,
        enabled: bool,
    ) -> bool {
        if !enabled || reduced() {
            self.hover = Spring::at(f32::from(enabled && hovered));
            self.focus = Spring::at(f32::from(enabled && focused));
            self.selection = Spring::at(f32::from(selected));
            self.press = Spring::at(0.0);
            self.down = false;
            self.press_until = None;
            self.last_frame = None;
            return false;
        }
        let pressed = self.down || self.press_until.is_some_and(|deadline| now < deadline);
        self.hover.target = f32::from(hovered);
        self.press.target = f32::from(pressed);
        self.focus.target = f32::from(focused);
        self.selection.target = f32::from(selected);
        let dt = self.last_frame.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f32()
        });
        if frame {
            self.hover.advance(dt, 28.0, 1.0);
            self.press.advance(dt, 38.0, 0.78);
            self.focus.advance(dt, 32.0, 1.0);
            self.selection.advance(dt, 30.0, 1.0);
        }
        let moving = !self.hover.settled()
            || !self.press.settled()
            || !self.focus.settled()
            || !self.selection.settled()
            || self.press_until.is_some_and(|deadline| now < deadline);
        if moving {
            if frame || self.last_frame.is_none() {
                self.last_frame = Some(now);
            }
        } else {
            self.last_frame = None;
            self.press_until = None;
        }
        moving
    }
}

pub fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

pub fn color(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: mix(a.r, b.r, t),
        g: mix(a.g, b.g, t),
        b: mix(a.b, b.b, t),
        a: mix(a.a, b.a, t),
    }
}

pub fn linear_color(a: Color, b: Color, t: f32) -> Color {
    let a = a.into_linear();
    let b = b.into_linear();
    Color::from_linear_rgba(
        mix(a[0], b[0], t),
        mix(a[1], b[1], t),
        mix(a[2], b[2], t),
        mix(a[3], b[3], t),
    )
}

pub fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn transform(bounds: iced::Rectangle, scale: f32, lift: f32) -> Transformation {
    let center = bounds.center();
    Transformation::translate(center.x, center.y + lift)
        * Transformation::scale(scale)
        * Transformation::translate(-center.x, -center.y)
}

/// Translate a newly shown panel without changing layout or its hit targets.
pub fn enter<'a, Message: 'a>(
    content: impl Into<iced::Element<'a, Message>>,
    identity: impl Into<String>,
    offset: iced::Vector,
) -> iced::Element<'a, Message> {
    iced::Element::new(Entrance {
        content: content.into(),
        identity: identity.into(),
        offset,
    })
}

struct Entrance<'a, Message> {
    content: iced::Element<'a, Message>,
    identity: String,
    offset: iced::Vector,
}

struct Entering {
    identity: String,
    spring: Spring,
    last_frame: Option<Instant>,
}

impl Entering {
    fn new(identity: String) -> Self {
        Self {
            identity,
            spring: Spring {
                target: 1.0,
                ..Spring::default()
            },
            last_frame: None,
        }
    }
}

impl<Message> iced::advanced::Widget<Message, iced::Theme, iced::Renderer>
    for Entrance<'_, Message>
{
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        iced::advanced::widget::tree::Tag::of::<Entering>()
    }
    fn state(&self) -> iced::advanced::widget::tree::State {
        iced::advanced::widget::tree::State::new(Entering::new(self.identity.clone()))
    }
    fn children(&self) -> Vec<iced::advanced::widget::Tree> {
        vec![iced::advanced::widget::Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut iced::advanced::widget::Tree) {
        let state = tree.state.downcast_mut::<Entering>();
        if state.identity != self.identity {
            *state = Entering::new(self.identity.clone());
        }
        tree.diff_children(std::slice::from_ref(&self.content));
    }
    fn size(&self) -> iced::Size<iced::Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        renderer: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        let state = tree.state.downcast_mut::<Entering>();
        if let iced::Event::Window(iced::window::Event::RedrawRequested(now)) = event {
            if reduced() || !layout.bounds().intersects(viewport) {
                state.spring = Spring::at(1.0);
            } else {
                let last = state.last_frame.replace(*now).unwrap_or(*now);
                state
                    .spring
                    .advance(now.saturating_duration_since(last).as_secs_f32(), 32.0, 1.0);
            }
            if !state.spring.settled() {
                shell.request_redraw();
            }
        }
        self.content.as_widget_mut().update(
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
        tree: &iced::advanced::widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let phase = if reduced() {
            1.0
        } else {
            tree.state.downcast_ref::<Entering>().spring.value
        };
        let offset = self.offset * (1.0 - phase);
        if offset == iced::Vector::ZERO {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        } else {
            renderer.with_layer(*viewport, |renderer| {
                renderer.with_translation(offset, |renderer| {
                    self.content.as_widget().draw(
                        &tree.children[0],
                        renderer,
                        theme,
                        style,
                        layout,
                        cursor,
                        viewport,
                    );
                })
            });
        }
    }
    fn mouse_interaction(
        &self,
        tree: &iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::advanced::mouse::Cursor,
        viewport: &iced::Rectangle,
        renderer: &iced::Renderer,
    ) -> iced::advanced::mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &iced::Rectangle,
        translation: iced::Vector,
    ) -> Option<iced::advanced::overlay::Element<'b, Message, iced::Theme, iced::Renderer>> {
        let state = tree.state.downcast_ref::<Entering>();
        let offset = if reduced() {
            iced::Vector::ZERO
        } else {
            self.offset * (1.0 - state.spring.value)
        };
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation + offset,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_is_frame_rate_independent_and_keeps_momentum_when_retargeted() {
        for damping in [0.78, 1.0] {
            let simulate = |fps: u32| {
                let mut spring = Spring::at(0.0);
                spring.target = 1.0;
                for _ in 0..fps / 10 {
                    spring.advance(1.0 / fps as f32, 28.0, damping);
                }
                spring
            };
            let mut a = simulate(60);
            let b = simulate(120);
            assert!((a.value - b.value).abs() < 0.0001);
            assert!((a.velocity - b.velocity).abs() < 0.0001);
            let velocity = a.velocity;
            a.target = 0.0;
            assert_eq!(a.velocity, velocity);
            a.advance(10.0, 28.0, damping);
            assert!(a.settled());
            assert_eq!(a.value, 0.0);
        }
        assert_eq!(smoothstep(0.0), 0.0);
        assert_eq!(smoothstep(1.0), 1.0);
        assert!((smoothstep(0.001) / 0.001).abs() < 0.01);
        let midpoint = linear_color(Color::BLACK, Color::WHITE, 0.5);
        assert!((midpoint.into_linear()[0] - 0.5).abs() < 0.0001);
    }

    #[test]
    fn quick_click_has_feedback_then_stops_requesting_frames() {
        let mut motion = Interaction::default();
        let start = Instant::now();
        motion.press(start);
        motion.down = false;
        assert!(motion.update(start, false, false, false, false, true));
        for frame in 1..=2 {
            motion.update(
                start + std::time::Duration::from_millis(frame * 16),
                true,
                false,
                false,
                false,
                true,
            );
        }
        assert!(motion.visual().press > 0.3);
        assert!(!motion.update(
            start + std::time::Duration::from_secs(2),
            true,
            false,
            false,
            false,
            true
        ));
        assert_eq!(motion.visual().press, 0.0);
    }
}
