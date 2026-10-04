use std::collections::HashSet;
use std::time::{Duration, Instant};

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
    let existing: HashSet<_> = f
        .niri()
        .layout
        .windows()
        .map(|(_, w)| w.id().get())
        .collect();
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
    f.niri()
        .layout
        .windows()
        .find(|(_, w)| !existing.contains(&w.id().get()))
        .unwrap()
        .1
        .id()
        .get()
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
    for action in [
        Action::FocusWindowMatching(filter(r#"app-id="^firefox$""#)),
        focus_or_spawn(r#"app-id="^firefox$""#),
    ] {
        let mut f = Fixture::new();
        f.add_output(1, (1920, 1080));
        f.add_output(2, (1920, 1080));
        let client = f.add_client();
        let first = open(&mut f, client, Some("firefox"), "First");
        let second = open(&mut f, client, Some("firefox"), "Second");
        let source = workspace_of(&mut f, first);
        f.niri_focus_output(2);
        let other = open(&mut f, client, Some("terminal"), "Other");
        set_history(&mut f, Some(first));
        f.niri_state().do_action(action.clone(), false);
        f.double_roundtrip(client);
        assert_eq!(f.niri().layout.focus().unwrap().id().get(), first);
        assert_eq!(f.niri().layout.active_workspace().unwrap().id(), source);
        assert_eq!(workspace_of(&mut f, second), source);

        // An explicit choice remains focused, even if another match is more recent.
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
        assert!(f.niri().pending_window_launches.is_empty());

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
}

fn recall(properties: &str, launch: bool) -> Action {
    Action::RecallWindow(
        filter(properties),
        if launch {
            vec!["/bin/true".into()]
        } else {
            vec![]
        },
    )
}

fn workspace_of(f: &mut Fixture, id: u64) -> crate::layout::workspace::WorkspaceId {
    f.niri()
        .layout
        .workspaces()
        .find(|(_, _, ws)| ws.windows().any(|w| w.id().get() == id))
        .unwrap()
        .2
        .id()
}

fn focus_or_spawn(properties: &str) -> Action {
    Action::FocusOrSpawn(filter(properties), vec!["/bin/true".into()])
}

#[test]
fn focus_or_spawn_launch_lifecycle() {
    let config = Config::parse_mem(
        r#"
        binds { Mod+B { focus-or-spawn "/bin/true" app-id="^browser$" title="^Ready$"; }; }
        workspace "apps"
        window-rule {
            match app-id="^browser$"
            open-on-workspace "apps"
            open-focused false
        }
        "#,
    )
    .unwrap();
    let bind = config.binds.0[0].clone();
    assert!(bind.repeat);
    let mut f = Fixture::with_config(config);
    f.add_output(1, (1920, 1080));
    f.add_output(2, (1920, 1080));
    let client = f.add_client();
    let home = open(&mut f, client, Some("terminal"), "Home");
    let apps = workspace_of(&mut f, home);
    f.niri_focus_output(2);
    let away = open(&mut f, client, Some("terminal"), "Away");
    let action: niri_ipc::Action = serde_json::from_str(
        r#"{"FocusOrSpawn":{"filter":{"app_id":"^browser$","title":"^Ready$"},"command":["/bin/true"]}}"#,
    )
    .unwrap();
    for command in [vec![], vec!["/bin/true".into(), "bad\0arg".into()]] {
        let mut invalid = action.clone();
        if let niri_ipc::Action::FocusOrSpawn { command: argv, .. } = &mut invalid {
            *argv = command;
        }
        assert!(f.ipc_action(invalid).is_err());
    }
    f.niri_state().handle_bind(bind.clone());
    let deadline = f.niri().pending_window_launches[0].deadline;
    f.ipc_action(action.clone()).unwrap();
    f.niri_state().handle_bind(bind);
    assert_eq!(f.niri().pending_window_launches.len(), 1);
    assert_eq!(f.niri().pending_window_launches[0].deadline, deadline);

    let browser = open(&mut f, client, Some("browser"), "Loading");
    assert_eq!(f.niri().pending_window_launches.len(), 1);
    f.client(client)
        .state
        .windows
        .last()
        .unwrap()
        .set_title("Ready");
    f.double_roundtrip(client);
    assert!(f.niri().pending_window_launches.is_empty());
    assert_eq!(workspace_of(&mut f, browser), apps);
    assert_eq!(f.niri().layout.focus().unwrap().id().get(), away);

    f.ipc_action(action).unwrap();
    f.double_roundtrip(client);
    assert_eq!(f.niri().layout.focus().unwrap().id().get(), browser);
}

#[test]
fn focus_or_spawn_failure_and_timeout_allow_retry() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let failed = Action::FocusOrSpawn(
        filter(r#"app-id="^browser$""#),
        vec!["/nonexistent/niri-focus-or-spawn-test".into()],
    );
    f.niri_state().do_action(failed, false);
    assert_eq!(f.niri().pending_window_launches.len(), 1);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !f.niri().pending_window_launches.is_empty() {
        assert!(Instant::now() < deadline, "spawn failure was not cleared");
        f.dispatch();
        std::thread::sleep(Duration::from_millis(1));
    }

    let action = focus_or_spawn(r#"app-id="^browser$""#);
    f.niri_state().do_action(action.clone(), false);
    let token = f.niri().pending_window_launches[0].token.clone();
    f.niri().pending_window_launches[0].deadline = Instant::now();
    f.niri_state().do_action(action, false);
    assert_eq!(f.niri().pending_window_launches.len(), 1);
    assert_ne!(f.niri().pending_window_launches[0].token, token);
}

#[test]
fn recall_moves_floating_window_to_current_workspace() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    f.add_output(2, (1920, 1080));
    let client = f.add_client();
    let browser = open(&mut f, client, Some("browser"), "Browser");
    f.niri().layout.toggle_window_floating(None);
    f.niri_focus_output(2);
    let target = f.niri().layout.active_workspace().unwrap().id();
    let action = recall(r#"app-id="browser""#, false);
    // Repeating the action while already here must also preserve floating state.
    for _ in 0..2 {
        f.niri_state().do_action(action.clone(), false);
        f.double_roundtrip(client);
        assert_eq!(workspace_of(&mut f, browser), target);
        assert_eq!(f.niri().layout.focus().unwrap().id().get(), browser);
        assert!(f.niri().layout.focus().unwrap().is_floating());
    }
}

#[test]
fn recall_launch_places_window_without_switching_back() {
    use crate::niri::RecallActivationMarker;

    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    f.add_output(2, (1920, 1080));
    let client = f.add_client();
    let home = open(&mut f, client, Some("terminal"), "Home");
    let target = workspace_of(&mut f, home);
    let action = recall(r#"app-id="browser""#, true);
    f.niri_state().do_action(action.clone(), false);
    f.niri_state().do_action(action, false);
    assert_eq!(f.niri().pending_window_launches.len(), 1);
    f.niri_focus_output(2);
    let away = open(&mut f, client, Some("terminal"), "Away");
    f.niri_state()
        .do_action(focus_or_spawn(r#"app-id="browser""#), false);
    let niri = f.niri();
    assert_eq!(niri.pending_window_launches.len(), 2);
    for request in &niri.pending_window_launches {
        let data = niri
            .activation_state
            .data_for_token(&request.token)
            .unwrap();
        assert_eq!(
            data.user_data.get::<RecallActivationMarker>().is_some(),
            request.recall_target.is_some()
        );
    }
    let browser = open(&mut f, client, Some("browser"), "Browser");
    assert_eq!(workspace_of(&mut f, browser), target);
    assert_eq!(f.niri().layout.focus().unwrap().id().get(), away);
    assert!(f.niri().pending_window_launches.is_empty());
}
