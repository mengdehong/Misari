// Copyright (C) 2025  Tassilo Horn <tsdh@gnu.org>
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
// FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General Public License for
// more details.
//
// You should have received a copy of the GNU General Public License along with
// this program.  If not, see <https://www.gnu.org/licenses/>.

//! Functions and data structures of the niriusd daemon.

use std::io::ErrorKind;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;

use niri_ipc::Request;
use niri_ipc::Response;
use niri_ipc::WorkspaceReferenceArg;

use crate::cmds;
use crate::ipc;
use crate::state::STATE;
use crate::state::State;
use crate::util;

pub fn run_daemon() {
    std::thread::spawn(init_then_process_events);
    serve_client_requests();
}

fn init_then_process_events() -> std::io::Result<()> {
    match ipc::query_niri(Request::Windows) {
        Ok(response) => match response {
            Response::Windows(wins) => {
                let mut state =
                    STATE.write().expect("Could not write() STATE.");
                log::info!("Initializing state with {} windows.", wins.len());
                for win in wins {
                    let msg =
                        state.window_opened_or_changed(win.clone()).unwrap();
                    log::info!("{}", msg);
                }
            }
            x => panic!("Received unexpected reply {x:?}"),
        },
        Err(err) => panic!("Could not query niri for windows: {err}"),
    }

    process_events()
}

fn process_events() -> std::io::Result<()> {
    let mut socket = niri_ipc::socket::Socket::connect()?;

    match socket.send(Request::EventStream) {
        Ok(response) => match response {
            Ok(Response::Handled) => {
                let mut read_event = socket.read_events();
                loop {
                    match read_event() {
                        Ok(event) => match handle_event(&event) {
                            Ok(msg) => {
                                log::info!(
                                    "Handled event successfully: {event:?} => {msg}"
                                )
                            }
                            Err(e) => {
                                log::error!(
                                    "Error during event-handling: {e:?}"
                                )
                            }
                        },
                        Err(err) => {
                            if err.kind() == ErrorKind::UnexpectedEof {
                                log::error!(
                                    "Received EOF, niri has quit and so do I. Goodbye!"
                                );
                                std::process::exit(0)
                            }
                            log::error!("Could not read event: {err:?}")
                        }
                    }
                }
            }
            Ok(other) => {
                let msg = format!(
                    "Unexpected response for Request::EventStream: {other:?}"
                );
                log::error!("{msg}");
                panic!("{msg}")
            }
            Err(e) => {
                let msg = format!("Error when requesting EventStream: {e:?}");
                log::error!("{msg}");
                panic!("{msg}")
            }
        },
        Err(e) => {
            let msg = format!("Could not send Request::EventStream: {e:?}");
            log::error!("{msg}");
            panic!("{msg}")
        }
    }
}

fn handle_event(event: &niri_ipc::Event) -> Result<String, String> {
    match event {
        // A workspace activated on an output which isn't focused doesn't
        // change the focus but can still hide follow-mode windows, so both
        // cases are handled here.  Conversely, moving the focus to another
        // output re-announces that output's already active workspace, which is
        // no navigation onto the scratchpad workspace.
        niri_ipc::Event::WorkspaceActivated { id, focused } => {
            let newly_activated = {
                let mut state =
                    STATE.write().expect("Could not write() STATE.");
                let was_active = state.is_workspace_visible(*id);
                state.workspace_activated(*id);
                if *focused {
                    state.workspace_focused(*id);
                }
                !was_active
            };

            let mut str = String::from("Updated workspace activation.");

            let state = STATE.read().expect("Could not read() STATE.");
            if *focused && newly_activated {
                str += &move_scratchpad_windows(&state)?;
            }
            str += &move_follow_mode_windows(&state)?;

            Ok(str)
        }
        niri_ipc::Event::WindowOpenedOrChanged { window } => {
            let mut state = STATE.write().expect("Could not write() STATE.");
            state.window_opened_or_changed(window.clone())
        }
        niri_ipc::Event::WindowClosed { id } => {
            let mut state = STATE.write().expect("Could not write() STATE.");
            state.window_closed(id)
        }
        niri_ipc::Event::WindowFocusChanged { id } => {
            let mut state = STATE.write().expect("Could not write() STATE.");
            state.window_focus_changed(*id)
        }
        niri_ipc::Event::WindowUrgencyChanged { id, urgent } => {
            let mut state = STATE.write().expect("Could not write() STATE.");
            state.window_urgency_changed(*id, *urgent)
        }
        niri_ipc::Event::WorkspacesChanged { workspaces } => {
            let mut str = {
                let mut state =
                    STATE.write().expect("Could not write() STATE.");
                state.workspaces_changed(workspaces.clone())?
            };

            // Insurance: niri drops the granular events when the set of
            // workspaces changed in the same refresh, so a workspace switch
            // could in theory arrive here instead of as WorkspaceActivated.
            // Never observed, but move_follow_mode_windows is a no-op if all
            // windows are already where they belong.
            let state = STATE.read().expect("Could not read() STATE.");
            str += &move_follow_mode_windows(&state)?;

            Ok(str)
        }
        _other => Ok("Nothing to do.".to_owned()),
    }
}

/// Moves scratchpad windows down to the bottom workspace when the scratchpad
/// workspace has just been focused, i.e., when the user navigated onto the
/// windows resting there.
fn move_scratchpad_windows(state: &State) -> Result<String, String> {
    if state.is_scratchpad_workspace_focused() {
        cmds::scratchpad_move()
    } else {
        Ok(String::new())
    }
}

/// Moves all follow-mode windows which are due to move to the focused
/// workspace.
fn move_follow_mode_windows(state: &State) -> Result<String, String> {
    // There's nothing to follow while no workspace has focus.
    let Some(ws_id) = state.get_focused_workspace().map(|ws| ws.id) else {
        return Ok(String::new());
    };

    let i = cmds::try_for_each_and_count(
        state.follow_mode_wins_to_move(ws_id),
        |w| {
            cmds::move_window_to_workspace(
                w,
                WorkspaceReferenceArg::Id(ws_id),
                false,
            )
        },
    )?;
    if i > 0 {
        Ok(format!("Moved {i} follow-mode windows"))
    } else {
        Ok(String::new())
    }
}

fn serve_client_requests() {
    let socket_path = util::get_nirius_socket_path();

    match std::fs::exists(&socket_path) {
        Ok(true) => match std::fs::remove_file(&socket_path) {
            Ok(()) => log::debug!(
                "Deleted stale socket {socket_path} from previous run."
            ),
            Err(e) => {
                panic!("Could not delete stale socket {socket_path}.\n{e:?}");
            }
        },
        Err(err) => {
            panic!("Error when trying to access {socket_path}.\n{err:?}")
        }
        _ => (),
    };

    log::debug!("niriusd starts listening on {socket_path}.");

    match UnixListener::bind(socket_path) {
        Ok(listener) => {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        handle_client_request(stream);
                    }
                    Err(err) => {
                        log::error!("Error handling client request: {err}");
                    }
                }
            }
        }
        Err(err) => {
            log::error!("Could not bind socket: {err}")
        }
    }
}

fn handle_client_request(stream: UnixStream) {
    match serde_json::from_reader::<_, cmds::NiriusCmd>(&stream) {
        Ok(cmd) => {
            log::debug!("Received command: {cmd:?}");
            if let Err(err) = stream.shutdown(std::net::Shutdown::Read) {
                log::error!("Could not shutdown stream for read: {err}")
            }
            let result = cmds::exec_nirius_cmd(cmd);
            log::debug!("Executed command, returning result {result:?}");
            if let Err(err) = serde_json::to_writer(&stream, &result) {
                log::error!("Couldn't send result back to client: {err}");
            }
            if let Err(err) = stream.shutdown(std::net::Shutdown::Write) {
                log::error!("Could not shutdown stream for read: {err}");
            }
        }
        Err(err) => {
            log::error!("Could not read command from client: {err}");
        }
    }
}
