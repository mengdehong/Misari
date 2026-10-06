use std::collections::HashMap;

use niri_config::{Bind, ModKey, Modifiers, ScreenEdge, Trigger};
use smithay::backend::input::{Axis, AxisSource, Device, Event, PointerAxisEvent};
use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraint};

use super::backend_ext::NiriInputBackend;
use super::modifiers_from_state;
use super::scroll_tracker::ScrollTracker;
use crate::niri::State;

#[derive(Default)]
pub struct EdgeScroll {
    gestures: HashMap<String, Gesture>,
    wheels: HashMap<String, Capture>,
}

struct Gesture {
    axes: [bool; 2],
    route: Route,
}

enum Route {
    Passthrough,
    Captured(Capture),
    Suppressed,
}

#[derive(Clone, PartialEq)]
struct Target {
    rule: usize,
    output: String,
    binds: Vec<Bind>,
}

struct Capture {
    target: Target,
    trackers: [ScrollTracker; 2],
}

impl Capture {
    fn new(target: Target, wheel: bool) -> Self {
        let tick = if wheel { 120 } else { 10 };
        Self {
            target,
            trackers: [ScrollTracker::new(tick), ScrollTracker::new(tick)],
        }
    }

    fn accumulate(&mut self, amounts: [Option<f64>; 2], wheel: bool) -> Vec<Bind> {
        let triggers = if wheel {
            [
                [Trigger::WheelScrollLeft, Trigger::WheelScrollRight],
                [Trigger::WheelScrollUp, Trigger::WheelScrollDown],
            ]
        } else {
            [
                [Trigger::TouchpadScrollLeft, Trigger::TouchpadScrollRight],
                [Trigger::TouchpadScrollUp, Trigger::TouchpadScrollDown],
            ]
        };
        let mut actions = Vec::new();
        for axis in 0..2 {
            let ticks = self.trackers[axis].accumulate(amounts[axis].unwrap_or(0.));
            let trigger = triggers[axis][usize::from(ticks > 0)];
            if let Some(bind) = self.target.binds.iter().find(|b| b.key.trigger == trigger) {
                actions.extend(std::iter::repeat_n(
                    bind.clone(),
                    usize::from(ticks.unsigned_abs()),
                ));
            }
        }
        actions
    }
}

impl EdgeScroll {
    // None means passthrough; Some(empty) still consumes a partial tick or cancelled tail.
    fn scroll(
        &mut self,
        device: String,
        wheel: bool,
        amounts: [Option<f64>; 2],
        target: Option<Target>,
    ) -> Option<Vec<Bind>> {
        if wheel {
            let Some(target) = target else {
                self.wheels.remove(&device);
                return None;
            };
            let capture = self
                .wheels
                .entry(device)
                .or_insert_with(|| Capture::new(target.clone(), true));
            if capture.target != target {
                *capture = Capture::new(target, true);
            }
            return Some(capture.accumulate(amounts, true));
        }

        let mut gesture = match self.gestures.remove(&device) {
            Some(gesture) => gesture,
            None => {
                if !amounts.iter().flatten().any(|v| *v != 0.) {
                    return None;
                }
                Gesture {
                    axes: [false; 2],
                    route: target
                        .map(|t| Route::Captured(Capture::new(t, false)))
                        .unwrap_or(Route::Passthrough),
                }
            }
        };
        for (axis, amount) in gesture.axes.iter_mut().zip(amounts) {
            if let Some(amount) = amount {
                *axis = amount != 0.;
            }
        }
        let actions = match &mut gesture.route {
            Route::Passthrough => None,
            Route::Captured(capture) => Some(capture.accumulate(amounts, false)),
            Route::Suppressed => Some(Vec::new()),
        };
        if gesture.axes.iter().any(|active| *active) {
            self.gestures.insert(device, gesture);
        }
        actions
    }

    pub fn refresh(&mut self, enabled: bool, output_exists: impl Fn(&str) -> bool) {
        self.wheels
            .retain(|_, c| enabled && output_exists(&c.target.output));
        for gesture in self.gestures.values_mut() {
            if let Route::Captured(capture) = &gesture.route {
                if !enabled || !output_exists(&capture.target.output) {
                    gesture.route = Route::Suppressed;
                }
            }
        }
    }

    pub fn cancel(&mut self) {
        self.refresh(false, |_| true);
    }

    pub fn remove_device(&mut self, device: &str) {
        self.gestures.remove(device);
        self.wheels.remove(device);
    }
}

fn resolve_modifiers(mut modifiers: Modifiers, mod_key: ModKey) -> Modifiers {
    if modifiers.contains(Modifiers::COMPOSITOR) {
        modifiers.remove(Modifiers::COMPOSITOR);
        modifiers.insert(mod_key.to_modifiers());
    }
    modifiers
}

impl State {
    fn edge_scroll_enabled(&self) -> bool {
        let niri = &self.niri;
        if niri.is_locked()
            || niri.layout.is_overview_open()
            || niri.screenshot_ui.is_open()
            || niri.window_mru_ui.is_open()
            || niri.exit_confirm_dialog.is_open()
        {
            return false;
        }
        let pointer = niri.seat.get_pointer().unwrap();
        if pointer.is_grabbed() {
            return false;
        }
        let locked = pointer.current_focus().is_some_and(|surface| {
            with_pointer_constraint(&surface, &pointer, |constraint| {
                constraint
                    .is_some_and(|c| c.is_active() && matches!(&*c, PointerConstraint::Locked(_)))
            })
        });
        !locked
    }

    pub fn refresh_edge_scroll(&mut self) {
        let enabled = self.edge_scroll_enabled();
        self.niri.edge_scroll.refresh(enabled, |name| {
            self.niri.global_space.outputs().any(|o| o.name() == name)
        });
    }

    pub fn on_edge_scroll<I: NiriInputBackend>(&mut self, event: &I::PointerAxisEvent) -> bool {
        let wheel = matches!(event.source(), AxisSource::Wheel | AxisSource::WheelTilt);
        self.refresh_edge_scroll();
        let config = self.niri.config.borrow();
        let factor = if wheel {
            config.input.mouse.scroll_factor
        } else {
            config.input.touchpad.scroll_factor
        }
        .map(|f| f.h_v_factors())
        .unwrap_or((1., 1.));
        let amounts = [Axis::Horizontal, Axis::Vertical].map(|axis| {
            if wheel {
                event
                    .amount_v120(axis)
                    .or_else(|| event.amount(axis).map(|v| v / 15. * 120.))
            } else {
                event.amount(axis)
            }
        });
        let amounts = [
            amounts[0].map(|v| v * factor.0),
            amounts[1].map(|v| v * factor.1),
        ];
        let pointer = self.niri.seat.get_pointer().unwrap();
        let modifiers =
            modifiers_from_state(self.niri.seat.get_keyboard().unwrap().modifier_state());
        let bound = match event.source() {
            AxisSource::Wheel => self.niri.mods_with_wheel_binds.contains(&modifiers),
            AxisSource::Finger | AxisSource::Continuous => {
                self.niri.mods_with_finger_scroll_binds.contains(&modifiers)
            }
            AxisSource::WheelTilt => false,
        };
        let target = if !bound
            && self.edge_scroll_enabled()
            && (wheel
                || !self
                    .niri
                    .edge_scroll
                    .gestures
                    .contains_key(&event.device().id()))
        {
            self.niri
                .output_under(pointer.current_location())
                .and_then(|(output, position)| {
                    let size = self.niri.global_space.output_geometry(output)?.size;
                    let mod_key = self.backend.mod_key(&config);
                    config
                        .edge_scroll
                        .iter()
                        .enumerate()
                        .find_map(|(index, rule)| {
                            if rule
                                .output
                                .as_deref()
                                .is_some_and(|name| name != output.name())
                            {
                                return None;
                            }
                            let hit = match rule.edge {
                                ScreenEdge::Top => position.y < rule.width.0,
                                ScreenEdge::Bottom => {
                                    f64::from(size.h) - position.y <= rule.width.0
                                }
                                ScreenEdge::Left => position.x < rule.width.0,
                                ScreenEdge::Right => f64::from(size.w) - position.x <= rule.width.0,
                            };
                            if !hit {
                                return None;
                            }
                            let binds: Vec<_> = rule
                                .binds
                                .0
                                .iter()
                                .filter(|bind| {
                                    let wheel_bind = matches!(
                                        bind.key.trigger,
                                        Trigger::WheelScrollUp
                                            | Trigger::WheelScrollDown
                                            | Trigger::WheelScrollLeft
                                            | Trigger::WheelScrollRight
                                    );
                                    wheel == wheel_bind
                                        && resolve_modifiers(bind.key.modifiers, mod_key)
                                            == modifiers
                                })
                                .cloned()
                                .collect();
                            (!binds.is_empty()).then(|| Target {
                                rule: index,
                                output: output.name(),
                                binds,
                            })
                        })
                })
        } else {
            None
        };
        drop(config);
        let Some(actions) =
            self.niri
                .edge_scroll
                .scroll(event.device().id(), wheel, amounts, target)
        else {
            return false;
        };
        for bind in actions {
            if !self.edge_scroll_enabled() {
                self.niri.edge_scroll.cancel();
                break;
            }
            self.handle_bind(bind);
        }
        true
    }
}
