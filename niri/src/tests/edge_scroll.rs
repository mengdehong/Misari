use niri_config::{Action, Config};
use smithay::backend::input::{AxisSource, InputEvent, InputTime, KeyState};
use smithay::input::pointer::MotionEvent;
use smithay::output::Scale;
use smithay::utils::{Transform, SERIAL_COUNTER};

use super::fixture::Fixture;
use super::test_input_backend::{
    TestInputBackend, TestInputDevice, TestKeyboardKeyEvent, TestPointerAxisEvent,
};

const RULE: &str = r#"
    edge-scroll "top" {
        binds {
            WheelScrollUp { focus-workspace-up; }
            TouchpadScrollUp { focus-workspace-up; }
        }
    }
"#;

fn config(text: &str) -> Config {
    let mut config = Config::parse_mem(text).unwrap();
    for rule in &mut config.edge_scroll {
        for bind in &mut rule.binds.0 {
            bind.action = Action::TestAction;
        }
    }
    config
}

fn fixture(text: &str) -> Fixture {
    let mut f = Fixture::with_config(config(text));
    f.add_output(1, (1920, 1080));
    f.add_output(2, (1920, 1080));
    move_pointer(&mut f, (100., 1.));
    f
}

fn move_pointer(f: &mut Fixture, position: (f64, f64)) {
    let state = f.niri_state();
    let pointer = state.niri.seat.get_pointer().unwrap();
    pointer.motion(
        state,
        None,
        &MotionEvent {
            location: position.into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::now(),
        },
    );
}

fn axis(f: &mut Fixture, source: AxisSource, amounts: [Option<f64>; 2], v120: [Option<f64>; 2]) {
    f.niri_state()
        .process_input_event(InputEvent::<TestInputBackend>::PointerAxis {
            event: TestPointerAxisEvent {
                source,
                amounts,
                v120,
            },
        });
}

fn scroll(f: &mut Fixture, amounts: [Option<f64>; 2]) {
    axis(f, AxisSource::Finger, amounts, [None; 2]);
}

fn wheel(f: &mut Fixture, v120: f64) {
    axis(f, AxisSource::Wheel, [None; 2], [None, Some(v120)]);
}

fn shift(f: &mut Fixture, state: KeyState) {
    f.niri_state()
        .process_input_event(InputEvent::<TestInputBackend>::Keyboard {
            event: TestKeyboardKeyEvent {
                time: InputTime::now(),
                code: 50u32.into(),
                state,
                count: 1,
            },
        });
}

#[test]
fn touchpad_latches_rule_and_waits_for_both_axes() {
    let mut f = fixture(RULE);
    scroll(&mut f, [Some(2.), Some(-3.)]);
    assert_eq!(f.niri().test_action_count, 0);
    shift(&mut f, KeyState::Pressed);
    move_pointer(&mut f, (2000., 200.));
    scroll(&mut f, [Some(0.), None]);
    scroll(&mut f, [None, Some(-7.)]);
    assert_eq!(
        f.niri().test_action_count,
        1,
        "original rule survives position/modifier changes"
    );
    scroll(&mut f, [None, Some(0.)]);
    scroll(&mut f, [None, Some(-10.)]);
    assert_eq!(
        f.niri().test_action_count,
        1,
        "new sequence must match its own rule"
    );
    scroll(&mut f, [None, Some(0.)]);

    shift(&mut f, KeyState::Released);
    scroll(&mut f, [None, Some(-5.)]);
    move_pointer(&mut f, (2000., 1.));
    scroll(&mut f, [None, Some(-10.)]);
    assert_eq!(
        f.niri().test_action_count,
        1,
        "application scroll must not be stolen mid-sequence"
    );
    scroll(&mut f, [None, Some(0.)]);
    scroll(&mut f, [None, Some(-10.)]);
    assert_eq!(f.niri().test_action_count, 2);
}

#[test]
fn transformed_edges_use_logical_pixels_and_device_factors() {
    let text = r#"
        input { touchpad { natural-scroll; scroll-factor horizontal=2 vertical=3; }; }
        edge-scroll "bottom" output="headless-1" {
            binds { TouchpadScrollUp { focus-workspace-up; }; }
        }
    "#;
    let mut f = fixture(text);
    let output = f.niri_output(1);
    output.change_current_state(
        None,
        Some(Transform::_90),
        Some(Scale::Fractional(2.)),
        None,
    );
    let size = f.niri().global_space.output_geometry(&output).unwrap().size;
    assert_eq!((size.w, size.h), (540, 960));
    move_pointer(&mut f, (100., 957.));
    shift(&mut f, KeyState::Pressed);
    scroll(&mut f, [None, Some(-4.)]);
    assert_eq!(
        f.niri().test_action_count,
        0,
        "extra modifiers must not match"
    );
    scroll(&mut f, [None, Some(0.)]);
    shift(&mut f, KeyState::Released);
    scroll(&mut f, [Some(2.), Some(-4.)]);
    assert_eq!(
        f.niri().test_action_count,
        1,
        "device factor applies, natural direction is already applied by backend"
    );
}

#[test]
fn output_and_device_removal_cancel_without_retargeting() {
    let mut f = fixture(RULE);
    scroll(&mut f, [None, Some(-5.)]);
    let output = f.niri_output(1);
    f.niri().remove_output(&output);
    f.niri_state().refresh_edge_scroll();
    move_pointer(&mut f, (100., 1.));
    scroll(&mut f, [None, Some(-10.)]);
    assert_eq!(f.niri().test_action_count, 0);
    scroll(&mut f, [None, Some(0.)]);
    scroll(&mut f, [None, Some(-10.)]);
    assert_eq!(f.niri().test_action_count, 1);
    scroll(&mut f, [None, Some(-5.)]);
    f.niri_state()
        .process_input_event(InputEvent::<TestInputBackend>::DeviceRemoved {
            device: TestInputDevice,
        });
    scroll(&mut f, [None, Some(-5.)]);
    assert_eq!(
        f.niri().test_action_count,
        1,
        "removed device's remainder must be discarded"
    );
    scroll(&mut f, [None, Some(-5.)]);
    assert_eq!(f.niri().test_action_count, 2);
}

#[test]
fn cancellation_and_config_reload_suppress_the_tail() {
    let mut f = fixture(RULE);
    scroll(&mut f, [None, Some(-5.)]);
    f.niri().layout.toggle_overview();
    f.niri_state().refresh_edge_scroll();
    f.niri().layout.toggle_overview();
    scroll(&mut f, [None, Some(-10.)]);
    assert_eq!(f.niri().test_action_count, 0);
    scroll(&mut f, [None, Some(0.)]);
    scroll(&mut f, [None, Some(-10.)]);
    assert_eq!(f.niri().test_action_count, 1);
    f.niri_state().reload_config(Ok(config(RULE)));
    scroll(&mut f, [None, Some(-10.)]);
    assert_eq!(f.niri().test_action_count, 1);
    scroll(&mut f, [None, Some(0.)]);
    scroll(&mut f, [None, Some(-10.)]);
    assert_eq!(f.niri().test_action_count, 2);
    scroll(&mut f, [None, Some(0.)]);
    f.niri_state().reload_config(Ok(config("")));
    scroll(&mut f, [None, Some(-10.)]);
    assert_eq!(f.niri().test_action_count, 2);
}

#[test]
fn existing_bindings_win_and_wheel_precision_survives() {
    let text = format!("{RULE}\nbinds {{ WheelScrollUp {{ focus-workspace-up; }}; TouchpadScrollUp {{ focus-workspace-up; }}; }}");
    let mut cfg = config(&text);
    // Native bindings do nothing; an edge binding would increment the counter.
    for bind in &mut cfg.binds.0 {
        bind.action = Action::FocusWorkspaceUp;
    }
    let mut f = Fixture::with_config(cfg);
    f.add_output(1, (1920, 1080));
    move_pointer(&mut f, (100., 1.));
    wheel(&mut f, -120.);
    axis(
        &mut f,
        AxisSource::Continuous,
        [None, Some(-10.)],
        [None; 2],
    );
    assert_eq!(f.niri().test_action_count, 0);

    let mut f = fixture(RULE);
    for _ in 0..3 {
        wheel(&mut f, -30.);
    }
    assert_eq!(f.niri().test_action_count, 0);
    wheel(&mut f, -30.);
    assert_eq!(f.niri().test_action_count, 1);
    wheel(&mut f, -60.);
    move_pointer(&mut f, (100., 200.));
    wheel(&mut f, -60.);
    move_pointer(&mut f, (100., 1.));
    wheel(&mut f, -60.);
    assert_eq!(
        f.niri().test_action_count,
        1,
        "leaving the edge resets fractional wheel ticks"
    );
    wheel(&mut f, -60.);
    assert_eq!(f.niri().test_action_count, 2);
}

#[test]
fn output_filter_mod_alias_and_corner_order() {
    let text = r#"
        input { mod-key "Shift"; }
        edge-scroll "top" output="headless-1" {
            binds { Mod+WheelScrollUp { focus-workspace-up; }; }
        }
        edge-scroll "left" {
            binds { WheelScrollUp { focus-workspace-up; }; }
        }
    "#;
    let mut f = fixture(text);
    shift(&mut f, KeyState::Pressed);
    wheel(&mut f, -120.);
    assert_eq!(f.niri().test_action_count, 1);
    move_pointer(&mut f, (2000., 1.));
    wheel(&mut f, -120.);
    assert_eq!(f.niri().test_action_count, 1);

    // First matching region wins at a corner instead of executing two actions.
    let text = format!(
        "{RULE}\nedge-scroll \"left\" {{ binds {{ WheelScrollUp {{ focus-workspace-up; }}; }}; }}"
    );
    let mut f = fixture(&text);
    move_pointer(&mut f, (1., 1.));
    wheel(&mut f, -120.);
    assert_eq!(f.niri().test_action_count, 1);
}

#[test]
fn unconfigured_or_unmatched_input_is_not_consumed() {
    let event = TestPointerAxisEvent {
        source: AxisSource::Wheel,
        amounts: [None; 2],
        v120: [None, Some(-30.)],
    };
    let mut f = fixture("");
    assert!(!f.niri_state().on_edge_scroll::<TestInputBackend>(&event));
    let mut f = fixture(RULE);
    move_pointer(&mut f, (100., 100.));
    assert!(!f.niri_state().on_edge_scroll::<TestInputBackend>(&event));
    move_pointer(&mut f, (100., 1.));
    assert!(f.niri_state().on_edge_scroll::<TestInputBackend>(&event));
    assert_eq!(
        f.niri().test_action_count,
        0,
        "a partial tick is consumed before an action is triggered"
    );
    shift(&mut f, KeyState::Pressed);
    assert!(!f.niri_state().on_edge_scroll::<TestInputBackend>(&event));
}

#[test]
fn horizontal_scroll_other_sources_and_cooldown_use_native_actions() {
    let mut f = fixture(
        r#"
        edge-scroll "right" {
            binds {
                WheelScrollLeft { focus-workspace-up; }
                TouchpadScrollLeft { focus-workspace-up; }
            }
        }
    "#,
    );
    move_pointer(&mut f, (1919., 200.));
    axis(
        &mut f,
        AxisSource::WheelTilt,
        [None; 2],
        [Some(-120.), None],
    );
    axis(
        &mut f,
        AxisSource::Continuous,
        [Some(-10.), None],
        [None; 2],
    );
    assert_eq!(f.niri().test_action_count, 2);

    let mut f = fixture(
        r#"
        edge-scroll "top" {
            binds { WheelScrollUp cooldown-ms=50 { focus-workspace-up; }; }
        }
    "#,
    );
    wheel(&mut f, -240.);
    wheel(&mut f, -120.);
    assert_eq!(
        f.niri().test_action_count,
        1,
        "native cooldown suppresses extra triggers"
    );
}
