use std::time::Duration;

use niri_config::window_filter::WindowFilter;
use niri_config::{Action, Config};

use super::client::ClientId;
use super::Fixture;
use crate::window::selector::WindowSelector;

fn filter(properties: &str) -> WindowFilter {
    let config = Config::parse_mem(&format!(
        "binds {{ Mod+B {{ focus-window-matching {properties}; }}; }}"
    ))
    .unwrap();
    let Action::FocusWindowMatching(filter) = config.binds.0.into_iter().next().unwrap().action
    else {
        unreachable!()
    };
    filter
}

fn open(f: &mut Fixture, client: ClientId, app: Option<&str>, title: &str) -> u64 {
    let window = f.client(client).create_window();
    if let Some(app) = app {
        window.xdg_toplevel.set_app_id(app.to_owned());
    }
    window.set_title(title);
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(client);
    let window = f.client(client).window(&surface);
    window.attach_new_buffer();
    window.set_size(100, 100);
    window.ack_last_and_commit();
    f.double_roundtrip(client);
    f.niri().layout.focus().unwrap().id().get()
}

#[test]
fn window_selection_matches_live_attributes_and_workspace() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    f.add_output(2, (1920, 1080));
    let client = f.add_client();
    let browser = open(&mut f, client, Some("firefox"), "GitHub");
    let workspace = f.niri().layout.active_workspace().unwrap().id().get();
    f.niri().layout.toggle_window_floating(None);
    f.double_roundtrip(client);
    f.niri_focus_output(2);
    let terminal = open(&mut f, client, None, "Terminal");

    let browser_filter = format!(
        r#"app-id="^firefox$" title="Hub$" floating=true urgent=false workspace-id={workspace}"#
    );
    let layout = &f.niri().layout;
    let current = layout.active_workspace().map(|ws| ws.id());
    for (properties, expected) in [
        (browser_filter.clone(), vec![browser]),
        (format!("{browser_filter} current-workspace=true"), vec![]),
        (
            "current-workspace=true floating=false".into(),
            vec![terminal],
        ),
        (r#"app-id=".*""#.into(), vec![browser]), // Missing app-id does not match .*
        (format!("id={terminal}"), vec![terminal]),
        (format!("id={terminal} urgent=true"), vec![]),
    ] {
        let selector = WindowSelector::new(filter(&properties)).unwrap();
        let mut ids = Vec::new();
        layout.with_windows(|window, _, workspace, _| {
            if selector.matches(window, workspace, current) {
                ids.push(window.id().get());
            }
        });
        assert_eq!(ids, expected, "{properties}");
    }
    let selector = WindowSelector::new(filter("current-workspace=true")).unwrap();
    assert!(!selector.matches(layout.focus().unwrap(), None, None));
}

// Set deterministic history without depending on the MRU debounce timer.
fn set_history(f: &mut Fixture, recent: Option<u64>) {
    f.niri().layout.with_windows_mut(|window, _| {
        let seconds = if Some(window.id().get()) == recent {
            20
        } else {
            10
        };
        window.set_focus_timestamp(Duration::from_secs(seconds));
    });
}

#[test]
fn window_selection_focus_preserves_current_match_and_uses_mru() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    f.add_output(2, (1920, 1080));
    let client = f.add_client();
    let first = open(&mut f, client, Some("firefox"), "First");
    let second = open(&mut f, client, Some("firefox"), "Second");
    let source_workspace = f.niri().layout.active_workspace().unwrap().id();
    f.niri_focus_output(2);
    let other = open(&mut f, client, Some("terminal"), "Other");
    set_history(&mut f, Some(first));
    let action = Action::FocusWindowMatching(filter(r#"app-id="^firefox$""#));
    f.niri_state().do_action(action.clone(), false);
    f.double_roundtrip(client);
    assert_eq!(f.niri().layout.focus().unwrap().id().get(), first);
    assert_eq!(
        f.niri().layout.active_workspace().unwrap().id(),
        source_workspace
    );

    // An explicit choice remains focused, even if another match was used more recently.
    f.niri_state().do_action(Action::FocusWindow(second), false);
    f.double_roundtrip(client);
    set_history(&mut f, Some(first));
    for action in [
        action.clone(),
        Action::FocusWindowMatching(filter(r#"title="missing""#)),
    ] {
        f.niri_state().do_action(action, false);
        f.double_roundtrip(client);
        assert_eq!(f.niri().layout.focus().unwrap().id().get(), second);
    }

    f.niri_state().do_action(Action::FocusWindow(other), false);
    f.double_roundtrip(client);
    set_history(&mut f, None);
    f.niri_state().do_action(action, false);
    f.double_roundtrip(client);
    assert_eq!(
        f.niri().layout.focus().unwrap().id().get(),
        first.min(second)
    );
}
