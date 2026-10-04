use std::collections::HashSet;

use niri_config::Config;
use niri_ipc::{Action, PinMode, PinWhenTiled};
use smithay::backend::input::{ButtonState, InputEvent, InputTime, KeyState, Keycode};
use smithay::input::pointer::MotionEvent;
use smithay::reexports::wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
use smithay::utils::{Point, SERIAL_COUNTER};
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::ClientId;
use super::test_input_backend::{TestInputBackend, TestKeyboardKeyEvent, TestPointerButtonEvent};
use super::Fixture;

fn set_up(config: &str) -> (Fixture, ClientId) {
    let mut f = Fixture::with_config(Config::parse_mem(config).unwrap());
    f.add_output(1, (1920, 1080));
    let client = f.add_client();
    (f, client)
}

fn open(
    f: &mut Fixture,
    client: ClientId,
    title: &str,
    parent: Option<&XdgToplevel>,
) -> (u64, WlSurface) {
    let existing: HashSet<_> = f
        .niri()
        .layout
        .windows()
        .map(|(_, w)| w.id().get())
        .collect();
    let window = f.client(client).create_window();
    window.set_title(title);
    window.set_parent(parent);
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(client);
    let window = f.client(client).window(&surface);
    window.attach_new_buffer();
    window.set_size(100, 100);
    window.ack_last_and_commit();
    f.double_roundtrip(client);
    let id = f
        .niri()
        .layout
        .windows()
        .find(|(_, w)| !existing.contains(&w.id().get()))
        .unwrap()
        .1
        .id()
        .get();
    (id, surface)
}

fn status(f: &mut Fixture, id: u64) -> (bool, bool) {
    let mut result = None;
    f.niri().layout.with_tiles(|tile, _, _, _| {
        if tile.window().id().get() == id {
            result = Some((tile.is_pinned(), tile.window().is_floating()));
        }
    });
    result.unwrap()
}

fn set_pinned(f: &mut Fixture, id: u64, mode: PinMode, when_tiled: PinWhenTiled) {
    f.ipc_action(Action::SetWindowPinned {
        id: Some(id),
        mode,
        when_tiled,
    })
    .unwrap();
}

fn toggle_pinned(f: &mut Fixture, id: u64, when_tiled: PinWhenTiled) {
    f.ipc_action(Action::ToggleWindowPinned {
        id: Some(id),
        when_tiled,
    })
    .unwrap();
}

fn center(f: &mut Fixture, id: u64) -> Point<f64, smithay::utils::Logical> {
    let ws = f.niri().layout.active_workspace().unwrap();
    let (tile, pos, _) = ws
        .tiles_with_render_positions()
        .find(|(t, _, _)| t.window().id().get() == id)
        .unwrap();
    pos + tile.window_loc() + tile.window_size().to_point().downscale(2.)
}

#[test]
fn pinned_preferences_respect_rules_tiled_policy_and_follow_moves() {
    let (mut f, client) = set_up(
        r#"
        window-rule { open-follow-mode "always"; }
        window-rule { match title="^rule$"; pinned true; }
    "#,
    );
    let (id, surface) = open(&mut f, client, "tiled", None);
    toggle_pinned(&mut f, id, PinWhenTiled::Ignore);
    assert_eq!(status(&mut f, id), (false, false));
    toggle_pinned(&mut f, id, PinWhenTiled::Remember);
    assert_eq!(status(&mut f, id), (true, false));
    // Float enables a dormant preference, rather than toggling it off.
    toggle_pinned(&mut f, id, PinWhenTiled::Float);
    f.double_roundtrip(client);
    assert_eq!(status(&mut f, id), (true, true));
    set_pinned(&mut f, id, PinMode::Off, PinWhenTiled::Float);
    assert_eq!(status(&mut f, id), (false, true));
    toggle_pinned(&mut f, id, PinWhenTiled::Float);
    assert_eq!(status(&mut f, id), (true, true));
    toggle_pinned(&mut f, id, PinWhenTiled::Float);
    f.double_roundtrip(client);
    assert_eq!(status(&mut f, id), (false, false));

    // Changing the title updates the rule but preserves the manual override.
    f.client(client).window(&surface).set_title("rule");
    f.double_roundtrip(client);
    assert_eq!(status(&mut f, id), (false, false));
    f.ipc_action(Action::ToggleWindowFloating { id: Some(id) })
        .unwrap();
    f.double_roundtrip(client);
    assert_eq!(status(&mut f, id), (false, true));
    set_pinned(&mut f, id, PinMode::Auto, PinWhenTiled::Ignore);
    assert_eq!(status(&mut f, id), (true, true));
    f.niri().config.borrow_mut().window_rules[1].pinned = Some(false);
    f.niri().recompute_window_rules();
    assert_eq!(status(&mut f, id), (false, true));
    set_pinned(&mut f, id, PinMode::On, PinWhenTiled::Ignore);
    f.niri().recompute_window_rules();
    assert_eq!(status(&mut f, id), (true, true));

    f.add_output(2, (1280, 720));
    f.niri_focus_output(2);
    f.double_roundtrip(client);
    assert_eq!(status(&mut f, id), (true, true));
    assert!(f
        .niri()
        .layout
        .active_workspace()
        .unwrap()
        .windows()
        .any(|w| w.id().get() == id));
    // The native floating toggle preserves the pin preference in both directions.
    for expected in [false, true, false] {
        f.ipc_action(Action::ToggleWindowFloating { id: Some(id) })
            .unwrap();
        f.double_roundtrip(client);
        assert_eq!(status(&mut f, id), (true, expected));
    }
    // Auto clears a dormant override without changing the layout.
    set_pinned(&mut f, id, PinMode::Auto, PinWhenTiled::Ignore);
    assert_eq!(status(&mut f, id), (false, false));
    toggle_pinned(&mut f, id, PinWhenTiled::Remember);
    toggle_pinned(&mut f, id, PinWhenTiled::Remember);
    assert_eq!(status(&mut f, id), (false, false));
    set_pinned(&mut f, id, PinMode::On, PinWhenTiled::Float);
    f.double_roundtrip(client);
    assert_eq!(status(&mut f, id), (true, true));
}

#[test]
fn pinned_mouse_binding_toggles_focused_window_not_window_under_pointer() {
    let (mut f, client) = set_up(
        r#"
        input { mod-key-nested "Super"; }
        window-rule { open-floating true; }
        binds { Mod+MouseMiddle repeat=false { toggle-window-pinned when-tiled="float"; }; }
    "#,
    );
    let (first, _) = open(&mut f, client, "first", None);
    let (focused, _) = open(&mut f, client, "focused", None);
    f.niri_complete_animations();
    // Move one window away so the cursor can point at it without changing keyboard focus.
    f.ipc_action(Action::MoveFloatingWindow {
        id: Some(first),
        x: "100".parse().unwrap(),
        y: "100".parse().unwrap(),
    })
    .unwrap();
    f.niri_complete_animations();
    let location = center(&mut f, first);
    let pointer = f.niri().seat.get_pointer().unwrap();
    pointer.motion(
        f.niri_state(),
        None,
        &MotionEvent {
            location,
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_micros(0),
        },
    );
    f.niri_state().refresh_pointer_contents();
    assert_eq!(f.niri().window_under_cursor().unwrap().id().get(), first);
    f.niri_state()
        .process_input_event(InputEvent::<TestInputBackend>::Keyboard {
            event: TestKeyboardKeyEvent {
                time: InputTime::from_micros(0),
                code: Keycode::from(133u32),
                state: KeyState::Pressed,
                count: 1,
            },
        });
    for expected in [(true, true), (false, false), (true, true)] {
        for state in [ButtonState::Pressed, ButtonState::Released] {
            f.niri_state()
                .process_input_event(InputEvent::<TestInputBackend>::PointerButton {
                    event: TestPointerButtonEvent { code: 0x112, state },
                });
        }
        f.double_roundtrip(client);
        assert_eq!(status(&mut f, focused), expected);
        assert_eq!(status(&mut f, first), (false, true));
        assert_eq!(f.niri().layout.focus().unwrap().id().get(), focused);
    }
}

#[test]
fn egl_pinned_stacking_visibility_and_hit_testing() {
    use smithay::backend::renderer::element::{Element, Kind};
    use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
    use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;

    use super::client::LayerConfigureProps;
    use crate::render_helpers::surface::push_elements_from_surface_tree;
    use crate::render_helpers::{RenderCtx, RenderTarget};

    let (mut f, client) = set_up(
        r#"window-rule { match title="^(pin|child|grandchild|normal)$"; open-floating true; }"#,
    );
    f.niri_state().backend.headless().add_renderer().unwrap();
    let (pin, pin_surface) = open(&mut f, client, "pin", None);
    let toplevel = f.client(client).window(&pin_surface).xdg_toplevel.clone();
    let (child, child_surface) = open(&mut f, client, "child", Some(&toplevel));
    let toplevel = f.client(client).window(&child_surface).xdg_toplevel.clone();
    let (grandchild, _) = open(&mut f, client, "grandchild", Some(&toplevel));
    let (normal, _) = open(&mut f, client, "normal", None);
    set_pinned(&mut f, pin, PinMode::On, PinWhenTiled::Ignore);
    f.niri_complete_animations();

    let pos = center(&mut f, pin);
    assert_eq!(status(&mut f, child), (false, true));
    assert_eq!(status(&mut f, grandchild), (false, true));
    assert_eq!(f.niri().layout.focus().unwrap().id().get(), normal);
    assert_eq!(f.niri().window_under(pos).unwrap().id().get(), grandchild);
    let window_surface = |f: &mut Fixture, id| {
        f.niri()
            .layout
            .windows()
            .find(|(_, w)| w.id().get() == id)
            .unwrap()
            .1
            .toplevel()
            .wl_surface()
            .clone()
    };
    let surface_id =
        |f: &mut Fixture,
         surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface| {
            f.niri_state()
                .backend
                .headless()
                .with_primary_renderer(|renderer| {
                    let mut ids = Vec::new();
                    push_elements_from_surface_tree(
                        renderer,
                        surface,
                        (0, 0).into(),
                        1.0.into(),
                        1.0,
                        Kind::Unspecified,
                        &mut |elem| ids.push(elem.id().clone()),
                    );
                    ids.into_iter().next().unwrap()
                })
                .unwrap()
        };
    let render_ids = |f: &mut Fixture| {
        let output = f.niri_output(1);
        let state = f.niri_state();
        let niri = &state.niri;
        state
            .backend
            .headless()
            .with_primary_renderer(|renderer| {
                niri.render_to_vec(
                    RenderCtx {
                        renderer,
                        target: RenderTarget::Output,
                        xray: None,
                    },
                    &output,
                    false,
                )
                .into_iter()
                .map(|elem| elem.id().clone())
                .collect::<Vec<_>>()
            })
            .unwrap()
    };
    let [pin_id, child_id, grandchild_id, normal_id] = [pin, child, grandchild, normal].map(|id| {
        let surface = window_surface(&mut f, id);
        surface_id(&mut f, &surface)
    });
    let ids = render_ids(&mut f);
    for (above, below) in [
        (&grandchild_id, &child_id),
        (&child_id, &pin_id),
        (&pin_id, &normal_id),
    ] {
        assert!(
            ids.iter().position(|id| id == above).unwrap()
                < ids.iter().position(|id| id == below).unwrap()
        );
    }

    let (full, full_surface) = open(&mut f, client, "full", None);
    f.ipc_action(Action::FullscreenWindow { id: Some(full) })
        .unwrap();
    f.double_roundtrip(client);
    let window = f.client(client).window(&full_surface);
    window.set_size(1920, 1080);
    window.ack_last_and_commit();
    f.double_roundtrip(client);
    f.niri_complete_animations();
    let full_surface = window_surface(&mut f, full);
    let full_id = surface_id(&mut f, &full_surface);
    let ids = render_ids(&mut f);
    assert!(!ids.contains(&normal_id));
    assert!(
        ids.iter().position(|id| id == &pin_id).unwrap()
            < ids.iter().position(|id| id == &full_id).unwrap()
    );

    assert_eq!(f.niri().layout.focus().unwrap().id().get(), full);
    assert_eq!(f.niri().window_under(pos).unwrap().id().get(), grandchild);
    let ws = f.niri().layout.active_workspace().unwrap();
    assert!(ws
        .tiles_with_render_positions()
        .any(|(t, _, visible)| t.window().id().get() == normal && !visible));
    toggle_pinned(&mut f, pin, PinWhenTiled::Ignore);
    assert_eq!(f.niri().window_under(pos).unwrap().id().get(), full);
    assert!(!render_ids(&mut f).contains(&pin_id));
    toggle_pinned(&mut f, pin, PinWhenTiled::Ignore);
    assert_eq!(f.niri().window_under(pos).unwrap().id().get(), grandchild);

    let layer = f
        .client(client)
        .create_layer(None, Layer::Overlay, "pin-test");
    let layer_surface = layer.surface.clone();
    layer.set_configure_props(LayerConfigureProps {
        size: Some((100, 100)),
        anchor: Some(Anchor::Top | Anchor::Left),
        ..Default::default()
    });
    layer.commit();
    f.roundtrip(client);
    let layer = f.client(client).layer(&layer_surface);
    layer.attach_new_buffer();
    layer.set_size(100, 100);
    layer.ack_last_and_commit();
    f.double_roundtrip(client);
    let layer_surface = f
        .niri()
        .mapped_layer_surfaces
        .keys()
        .next()
        .unwrap()
        .wl_surface()
        .clone();
    let overlay_id = surface_id(&mut f, &layer_surface);
    let ids = render_ids(&mut f);
    assert!(
        ids.iter().position(|id| id == &overlay_id).unwrap()
            < ids.iter().position(|id| id == &pin_id).unwrap()
    );
}
