//! Agent seat: a second seat for automation that runs alongside the user's.
//!
//! The seat is created at startup, after the user's seat: GTK 3 ignores seats that appear later,
//! and clients that only handle one seat pick the first one, which stays the user's. It has its own
//! pointer, keyboard focus, popup grabs and clipboard, so clicking and typing through it doesn't
//! move the user's cursor or focus. All points are in window coordinates (see [`niri_ipc::agent`]),
//! and the agent pointer lives in the target window's buffer coordinate space rather than in the
//! global space, so actions don't depend on where the window is on screen.

pub mod cursor;
mod receipt;
mod takeover;

use std::ffi::OsStr;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};
use std::{fs, thread};

use async_channel::Sender;
use base64::Engine as _;
use cursor::AgentCursor;
use niri_config::Config;
use niri_ipc::agent::{AgentAction, AgentButton, AgentCapture, AgentInfo};
use niri_ipc::{Reply, Response};
use receipt::{Wait, Watch};
use smithay::backend::allocator::Fourcc;
use smithay::backend::input::{Axis, AxisSource, ButtonState, InputTime, KeyState};
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::desktop::{
    find_popup_root_surface, PopupGrab, PopupKeyboardGrab, PopupKind, PopupPointerGrab, Window,
    WindowSurfaceType,
};
use smithay::input::keyboard::{xkb, FilterResult, Keycode, Keysym};
use smithay::input::pointer::{AxisFrame, ButtonEvent, CursorIcon, Focus, MotionEvent};
use smithay::input::{Seat, SeatState};
use smithay::output::{Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::rustix::net::sockopt::socket_peercred;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource as _};
use smithay::utils::{IsAlive, Physical, Point, Rectangle, Scale, Size, Transform, SERIAL_COUNTER};
use smithay::wayland::compositor::get_role;
use smithay::wayland::shell::xdg::{PopupSurface, ToplevelSurface, XDG_POPUP_ROLE};
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::text_input::set_text_input_seat_redirect;

use crate::cursor::XCursor;
use crate::layout::LayoutElement as _;
use crate::niri::{ClientState, NewClient, State};
use crate::render_helpers::xray::XrayPos;
use crate::render_helpers::{encompassing_geo, render_to_vec, RenderCtx, RenderTarget};
use crate::utils::spawning::spawn_with_env;

const SEAT_NAME: &str = "niri-agent";

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// Logical pixels per wheel notch, for the discrete scroll value.
const WHEEL_STEP: f64 = 15.;

/// Range of keycodes that xkb keymaps can hold.
const MIN_KEYCODE: u32 = 8;
const MAX_KEYCODE: u32 = 255;

/// Environment variable that tells which launch an agent app came from.
const LAUNCH_ENV: &str = "NIRI_AGENT_LAUNCH";

/// How long a click waits after the pointer entered a new surface.
const ENTER_DELAY: Duration = Duration::from_millis(50);

/// How long after a popup grab started a click in the popup waits.
///
/// GTK 3 menus ignore a button release within 500 ms of the press that opened them
/// (`MENU_SHELL_TIMEOUT`), so that press-drag-release works. An agent clicking an item right
/// after opening the menu would otherwise hit nothing.
const POPUP_SETTLE: Duration = Duration::from_millis(550);

/// How often the agent cursor is redrawn while it animates.
const CURSOR_TICK: Duration = Duration::from_millis(16);

/// How long a launch waits for the app's first window.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Windows that the user's apps open this soon after an agent action don't take focus.
const RECENT_ACTION: Duration = Duration::from_secs(5);

/// Executable names of clients that must not see the agent seat.
const HIDE_SEAT_FROM: &[&str] = &[
    // gpui keeps the seat whose capabilities arrive last, taking the user's keyboard, pointer and
    // input method away when the agent seat appears.
    "zed-editor",
    // Input methods bind every seat. On the agent seat, they grab its keyboard as soon as an agent
    // app focuses a text field, swallowing agent keys and sending unhandled ones back through a
    // virtual keyboard that niri treats as the user's. The agent types without an input method
    // anyway.
    "fcitx5",
    "ibus-wayland",
];

pub struct AgentState {
    seat: Seat<State>,
    popup_grab: Option<(WlSurface, PopupGrab<State>)>,
    /// When the current popup grab started.
    popup_grab_since: Instant,
    /// Actions waiting for their window to settle.
    watches: Vec<Watch>,
    next_watch_id: u64,
    /// Output for frame callbacks to windows that aren't on any output.
    frame_output: Output,
    /// Name of the Wayland socket for apps launched for the agent.
    socket_name: Option<String>,
    /// Launches waiting for their first window.
    launches: Vec<(u64, Sender<Reply>)>,
    next_launch_id: u64,
    /// Clients that agent actions recently went to, so that windows they open don't take the
    /// user's focus.
    recent_clients: Vec<(ClientId, Instant)>,
    /// Window that shows the agent cursor.
    cursor_window: Option<WlSurface>,
    /// Whether the agent cursor redraw timer runs.
    cursor_timer: bool,
    /// The user operating agent windows.
    takeover: takeover::Takeover,
}

impl AgentState {
    pub fn new(
        seat_state: &mut SeatState<State>,
        display: &DisplayHandle,
        config: &Config,
        user_seat: &Seat<State>,
    ) -> Self {
        let mut seat = seat_state.new_wl_seat_with_filter(display, SEAT_NAME, |client| {
            client
                .get_data::<ClientState>()
                .is_none_or(|data| data.agent_client || !data.hide_agent_seat)
        });

        // Agent apps type through the agent seat, which has no input method: the agent types
        // without one, and input methods must not see it (see HIDE_SEAT_FROM). Their text inputs
        // belong to the user's seat instead, so that the user's input method works in them while
        // the user's keyboard focus is on them, which is when the user takes them over.
        let user_seat = user_seat.clone();
        set_text_input_seat_redirect(&seat, move |client| {
            let agent_client = client
                .get_data::<ClientState>()
                .is_some_and(|data| data.agent_client);
            agent_client.then(|| user_seat.clone())
        });

        // Same layout as the user's keyboard, so key combinations resolve the same way.
        let keyboard = &config.input.keyboard;
        let (delay, rate) = (keyboard.repeat_delay.into(), keyboard.repeat_rate.into());
        if let Err(err) = seat.add_keyboard(keyboard.xkb.to_xkb_config(), delay, rate) {
            warn!("error adding agent keyboard with the configured layout: {err:?}");
            seat.add_keyboard(Default::default(), delay, rate).unwrap();
        }
        seat.add_pointer();

        let frame_output = Output::new(
            String::new(),
            PhysicalProperties {
                size: Size::from((0, 0)),
                subpixel: Subpixel::Unknown,
                make: String::new(),
                model: String::new(),
                serial_number: String::new(),
            },
        );

        Self {
            seat,
            popup_grab: None,
            popup_grab_since: Instant::now(),
            watches: Vec::new(),
            next_watch_id: 0,
            frame_output,
            socket_name: None,
            launches: Vec::new(),
            next_launch_id: 0,
            recent_clients: Vec::new(),
            cursor_window: None,
            cursor_timer: false,
            takeover: Default::default(),
        }
    }

    /// Creates the Wayland socket for apps launched for the agent, next to the main one.
    pub fn create_socket(&mut self, event_loop: &LoopHandle<'static, State>, main: &OsStr) {
        let name = format!("{}-agent", main.to_string_lossy());
        let source = match ListeningSocketSource::with_name(&name) {
            Ok(source) => source,
            Err(err) => {
                warn!("error creating the agent Wayland socket: {err:?}");
                return;
            }
        };

        event_loop
            .insert_source(source, |client, _, state| {
                // Agent apps get the same restrictions as sandboxed apps: no screencopy,
                // virtual input, data control, layer shell and so on.
                state.niri.insert_agent_client(NewClient {
                    client,
                    restricted: true,
                    credentials_unknown: false,
                });
            })
            .unwrap();

        info!("agent apps connect to Wayland socket: {name}");
        self.socket_name = Some(name);
    }

    pub fn is_agent_seat(&self, seat: &Seat<State>) -> bool {
        &self.seat == seat
    }
}

/// Whether a client sees the user's seat: all but agent apps do.
pub fn sees_user_seat(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_none_or(|data| !data.agent_client)
}

/// Whether a surface belongs to an app launched for the agent.
pub fn is_agent_surface(surface: &WlSurface) -> bool {
    surface.client().is_some_and(|client| {
        client
            .get_data::<ClientState>()
            .is_some_and(|data| data.agent_client)
    })
}

/// Whether the agent seat should be hidden from a newly connected client.
pub fn hides_agent_seat(stream: &UnixStream, config: &Config) -> bool {
    let Some(name) = client_executable_name(stream) else {
        return false;
    };

    let hide =
        HIDE_SEAT_FROM.contains(&name.as_str()) || config.agent.hide_seat_from.contains(&name);
    if hide {
        debug!("hiding the agent seat from {name}");
    }
    hide
}

/// The agent launch a process came from, by its environment.
fn launch_id_of(pid: i32) -> Option<u64> {
    let environ = fs::read(format!("/proc/{pid}/environ")).ok()?;
    let prefix = format!("{LAUNCH_ENV}=");
    environ
        .split(|b| *b == 0)
        .find_map(|var| var.strip_prefix(prefix.as_bytes()))
        .and_then(|value| std::str::from_utf8(value).ok()?.parse().ok())
}

fn client_executable_name(stream: &UnixStream) -> Option<String> {
    let cred = socket_peercred(stream).ok()?;
    let pid = cred.pid.as_raw_nonzero().get();
    let exe = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let name = exe.file_name()?.to_str()?;
    // The link of a replaced binary ends with " (deleted)".
    Some(name.trim_end_matches(" (deleted)").to_owned())
}

/// What a pointer action does once the pointer is at its point.
enum PointerInput {
    Click {
        button: AgentButton,
        count: u8,
    },
    Button {
        button: AgentButton,
        state: ButtonState,
    },
    Scroll {
        dx: f64,
        dy: f64,
    },
}

struct Target {
    window: Window,
    toplevel: WlSurface,
}

/// Window rendered to RGBA pixels.
struct Rendered {
    /// Rendered area in window coordinates.
    area: Rectangle<i32, Physical>,
    pixels: Vec<u8>,
    scale: Scale<f64>,
}

/// Agent input device an action needs the target's client to listen to.
#[derive(Clone, Copy)]
enum Device {
    Pointer,
    Keyboard,
}

impl State {
    pub fn agent_action(&mut self, action: AgentAction, reply: Sender<Reply>) {
        if self.niri.agent.is_none() {
            let err = String::from("the agent seat is off, see `agent` in the config");
            let _ = reply.try_send(Err(err));
            return;
        }

        let (id, device) = match &action {
            AgentAction::Launch { command } => {
                self.agent_launch(command.clone(), reply);
                return;
            }
            AgentAction::Info { id } => {
                let result = self.agent_info(*id).map(Response::AgentInfo);
                let _ = reply.try_send(result);
                return;
            }
            AgentAction::Capture { id, max_size, .. } => {
                let wait = Wait::Capture {
                    reply: reply.clone(),
                    max_size: *max_size,
                };
                if let Err(err) = self.agent_wait(*id, wait) {
                    let _ = reply.try_send(Err(err));
                }
                return;
            }
            AgentAction::Move { id, .. }
            | AgentAction::Click { id, .. }
            | AgentAction::Button { id, .. }
            | AgentAction::Scroll { id, .. } => (*id, Device::Pointer),
            AgentAction::Key { id, .. } | AgentAction::Type { id, .. } => (*id, Device::Keyboard),
        };

        if let Err(err) = self.agent_check_takeover() {
            let _ = reply.try_send(Err(err));
            return;
        }

        let target = match self.agent_target(id, device) {
            Ok(target) => target,
            Err(err) => {
                let _ = reply.try_send(Err(err));
                return;
            }
        };
        let before = self.agent_snapshot(id);

        if let Some(client) = target.toplevel.client() {
            let agent = self.agent();
            agent.recent_clients.retain(|(c, _)| *c != client.id());
            agent.recent_clients.push((client.id(), Instant::now()));
        }

        // Pointer actions first move the pointer; what they do at the destination may have to
        // wait for the client to take in the pointer entering a new surface.
        let (pos, pointer_input) = match action {
            AgentAction::Launch { .. } | AgentAction::Capture { .. } | AgentAction::Info { .. } => {
                unreachable!()
            }
            AgentAction::Move { x, y, .. } => ((x, y), None),
            AgentAction::Click {
                x,
                y,
                button,
                count,
                ..
            } => ((x, y), Some(PointerInput::Click { button, count })),
            AgentAction::Button {
                x,
                y,
                button,
                pressed,
                ..
            } => {
                let state = if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                };
                ((x, y), Some(PointerInput::Button { button, state }))
            }
            AgentAction::Scroll { x, y, dx, dy, .. } => {
                ((x, y), Some(PointerInput::Scroll { dx, dy }))
            }
            AgentAction::Key { keys, .. } => {
                let result = self.agent_key(&target, &keys);
                self.agent_finish(id, &target, None, before, result, reply);
                return;
            }
            AgentAction::Type { text, .. } => {
                self.agent_type(&target, &text);
                self.agent_finish(id, &target, None, before, Ok(()), reply);
                return;
            }
        };

        let entered = self.agent_pointer_move(&target, Point::from(pos));

        // A click also moves the keyboard focus. Do that now rather than together with the press.
        let presses = matches!(
            pointer_input,
            Some(PointerInput::Click { .. })
                | Some(PointerInput::Button {
                    state: ButtonState::Pressed,
                    ..
                })
        );
        let focused = presses && self.agent_focus(&target);

        // GTK 3 drops a button press that comes in the same batch as the second seat's pointer
        // entering one of its windows, or its keyboard focus arriving, as with the first click in
        // a dialog that just opened. Give the client a moment, then move again in place so that
        // the press comes with a motion like a normal click.
        let mut delay = (entered || focused).then_some(ENTER_DELAY);
        // A click in a menu that just opened needs to wait until the menu takes clicks.
        if presses && self.agent_popup_grab_on(&target.toplevel) {
            let since = self.agent().popup_grab_since.elapsed();
            if let Some(left) = POPUP_SETTLE.checked_sub(since) {
                delay = Some(delay.map_or(left, |d| d.max(left)));
            }
        }

        match pointer_input {
            Some(input) if delay.is_some() => {
                let mut pending = Some((target, input, before, reply));
                self.niri
                    .event_loop
                    .insert_source(Timer::from_duration(delay.unwrap()), move |_, _, state| {
                        if let Some((target, input, before, reply)) = pending.take() {
                            match state.agent_check_takeover() {
                                Ok(()) if target.toplevel.alive() => {
                                    state.agent_pointer_move(&target, Point::from(pos));
                                    state.agent_finish(
                                        id,
                                        &target,
                                        Some(input),
                                        before,
                                        Ok(()),
                                        reply,
                                    );
                                }
                                Ok(()) => {
                                    let _ =
                                        reply.try_send(Err(format!("window {id} was not found")));
                                }
                                Err(err) => {
                                    let _ = reply.try_send(Err(err));
                                }
                            }
                        }
                        TimeoutAction::Drop
                    })
                    .unwrap();
            }
            input => self.agent_finish(id, &target, input, before, Ok(()), reply),
        }
    }

    /// Delivers the rest of an action, then replies with a receipt once the window settles.
    fn agent_finish(
        &mut self,
        id: u64,
        target: &Target,
        input: Option<PointerInput>,
        before: Option<receipt::Snapshot>,
        result: Result<(), String>,
        reply: Sender<Reply>,
    ) {
        match input {
            None => (),
            Some(PointerInput::Click { button, count }) => {
                for _ in 0..count.max(1) {
                    self.agent_button(target, button, ButtonState::Pressed);
                    self.agent_button(target, button, ButtonState::Released);
                }
            }
            Some(PointerInput::Button { button, state }) => {
                self.agent_button(target, button, state);
            }
            Some(PointerInput::Scroll { dx, dy }) => self.agent_scroll(dx, dy),
        }

        let wait = Wait::Receipt {
            before,
            reply: reply.clone(),
        };
        if let Err(err) = result.and_then(|()| self.agent_wait(id, wait)) {
            let _ = reply.try_send(Err(err));
        }
    }

    /// Whether a newly mapped window comes from the agent and must not take the user's focus.
    ///
    /// That's every window of agent apps, and windows that the user's apps open shortly after an
    /// agent action, like a dialog the agent clicked open.
    pub fn agent_opened(&mut self, toplevel: &ToplevelSurface) -> bool {
        let surface = toplevel.wl_surface();
        if is_agent_surface(surface) {
            // Dialogs of an agent app that the user operates come up as usual.
            return !self.agent_user_operates(surface);
        }

        let Some(agent) = &mut self.niri.agent else {
            return false;
        };
        agent
            .recent_clients
            .retain(|(_, at)| at.elapsed() < RECENT_ACTION);
        let Some(client) = surface.client() else {
            return false;
        };
        agent.recent_clients.iter().any(|(c, _)| *c == client.id())
    }

    fn agent_launch(&mut self, command: Vec<String>, reply: Sender<Reply>) {
        let agent = self.agent();
        let Some(socket_name) = agent.socket_name.clone() else {
            let err = String::from("niri has no agent Wayland socket");
            let _ = reply.try_send(Err(err));
            return;
        };

        let id = agent.next_launch_id;
        agent.next_launch_id += 1;
        agent.launches.push((id, reply));

        let env = vec![
            (String::from("WAYLAND_DISPLAY"), Some(socket_name)),
            (String::from(LAUNCH_ENV), Some(id.to_string())),
            // X11 apps would go through Xwayland and the user's seat.
            (String::from("DISPLAY"), None),
            (
                String::from("ELECTRON_OZONE_PLATFORM_HINT"),
                Some(String::from("wayland")),
            ),
            (
                String::from("QT_QPA_PLATFORM"),
                Some(String::from("wayland")),
            ),
            (
                String::from("SDL_VIDEODRIVER"),
                Some(String::from("wayland")),
            ),
            // Agent apps shouldn't drive niri.
            (String::from("NIRI_SOCKET"), None),
        ];
        spawn_with_env(command, None, env);

        self.niri
            .event_loop
            .insert_source(Timer::from_duration(LAUNCH_TIMEOUT), move |_, _, state| {
                let agent = state.agent();
                if let Some(idx) = agent.launches.iter().position(|(i, _)| *i == id) {
                    let (_, reply) = agent.launches.remove(idx);
                    let err = format!(
                        "no window appeared within {} s; if the app hands off to an \
                         instance that's already running, give it a separate profile",
                        LAUNCH_TIMEOUT.as_secs()
                    );
                    let _ = reply.try_send(Err(err));
                }
                TimeoutAction::Drop
            })
            .unwrap();
    }

    /// Replies to the launch that a newly mapped agent window belongs to.
    pub fn agent_on_map(&mut self, window: &Window) {
        let Some(agent) = &self.niri.agent else {
            return;
        };
        if agent.launches.is_empty() {
            return;
        }

        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let surface = toplevel.wl_surface().clone();
        if !is_agent_surface(&surface) {
            return;
        }
        let Some(client) = surface.client() else {
            return;
        };
        let Some((mapped, _)) = self.niri.layout.find_window_and_output(&surface) else {
            return;
        };
        let window_id = mapped.id().get();
        let Some(info) = self.agent_window_info(window_id) else {
            return;
        };

        // The launch id in the environment is exact, but some apps (Chromium) overwrite their
        // environment block with the process title, and an app launched earlier may open the
        // window for a later launch that handed off to it (`firefox --new-window`). Then fall
        // back to the oldest launch, for windows that aren't dialogs of another window.
        let pid = client
            .get_credentials(&self.niri.display_handle)
            .ok()
            .map(|creds| creds.pid);
        let launch_id = pid.and_then(launch_id_of);

        let agent = self.agent();
        let exact = launch_id.and_then(|id| agent.launches.iter().position(|(i, _)| *i == id));
        let idx =
            exact.or_else(|| (info.parent_id.is_none() && !agent.launches.is_empty()).then_some(0));
        if let Some(idx) = idx {
            let (_, reply) = agent.launches.remove(idx);
            let _ = reply.try_send(Ok(Response::AgentLaunched(info)));
        }
    }

    fn agent(&mut self) -> &mut AgentState {
        // agent_action() refuses everything while the agent seat is off.
        self.niri.agent.as_mut().unwrap()
    }

    fn agent_seat(&mut self) -> Seat<State> {
        self.agent().seat.clone()
    }

    fn agent_target(&mut self, id: u64, device: Device) -> Result<Target, String> {
        let mapped = self.agent_mapped(id)?;
        let target = Target {
            window: mapped.window.clone(),
            toplevel: mapped.toplevel().wl_surface().clone(),
        };

        // Clients that only handle one seat never bind the agent's pointer or keyboard; input sent
        // to them would silently go nowhere.
        let seat = self.agent_seat();
        let listens = self
            .niri
            .display_handle
            .get_client(target.toplevel.id())
            .is_ok_and(|client| match device {
                Device::Pointer => {
                    let pointer = seat.get_pointer().unwrap();
                    let listens = pointer.client_pointers(&client).next().is_some();
                    listens
                }
                Device::Keyboard => {
                    let keyboard = seat.get_keyboard().unwrap();
                    let listens = keyboard.client_keyboards(&client).next().is_some();
                    listens
                }
            });
        if !listens {
            let device = match device {
                Device::Pointer => "pointer",
                Device::Keyboard => "keyboard",
            };
            return Err(format!(
                "window {id} doesn't listen to the agent {device}: its app only handles one seat"
            ));
        }

        Ok(target)
    }

    /// Moves the agent pointer to `pos` in window coordinates.
    ///
    /// Returns whether the pointer entered a surface it wasn't on before, other than a popup.
    fn agent_pointer_move(
        &mut self,
        target: &Target,
        pos: Point<f64, smithay::utils::Logical>,
    ) -> bool {
        let seat = self.agent_seat();
        let pointer = seat.get_pointer().unwrap();
        let previous = pointer.current_focus();

        // The agent pointer uses the window's buffer coordinates as its global space.
        let location = pos + target.window.geometry().loc.to_f64();
        let under = target
            .window
            .surface_under(location, WindowSurfaceType::ALL)
            .map(|(surface, surface_loc)| (surface, surface_loc.to_f64()));

        pointer.motion(
            self,
            under,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);

        self.agent_update_cursor(&target.toplevel, |cursor| cursor.move_to(pos));

        // Entering popups is part of using menus, where clicks have always worked right away.
        let focus = pointer.current_focus();
        focus != previous && focus.is_some_and(|surface| get_role(&surface) != Some(XDG_POPUP_ROLE))
    }

    /// Shows the agent cursor on the window, moving it off the window it was on before.
    fn agent_update_cursor(&mut self, toplevel: &WlSurface, update: impl FnOnce(&mut AgentCursor)) {
        let previous = self.agent().cursor_window.replace(toplevel.clone());
        if let Some(previous) = previous.filter(|previous| previous != toplevel) {
            if let Some((mapped, _)) = self.niri.layout.find_window_and_output_mut(&previous) {
                mapped.agent_cursor = None;
            }
        }

        let Some((_, output)) = self.niri.layout.find_window_and_output(toplevel) else {
            return;
        };
        let output = output.cloned();
        let scale = output
            .as_ref()
            .map_or(1, |output| output.current_scale().integer_scale());

        // The system arrow, whatever the user's own cursor looks like right now.
        let xcursor = self.niri.cursor_manager.get_default_cursor(scale);
        let (_, frame) = xcursor.frame(0);
        let hotspot = XCursor::hotspot(frame).to_logical(scale).to_f64();
        let arrow_size = Size::<i32, Physical>::from((frame.width as i32, frame.height as i32))
            .to_logical(scale)
            .to_f64();
        let arrow = self
            .niri
            .cursor_texture_cache
            .get(CursorIcon::Default, scale, &xcursor, 0);
        let focus_ring = self.niri.config.borrow().layout.focus_ring;

        let Some((mapped, _)) = self.niri.layout.find_window_and_output_mut(toplevel) else {
            return;
        };
        let cursor = mapped.agent_cursor.get_or_insert_with(|| {
            let halo = cursor::halo_buffer(&focus_ring, scale);
            AgentCursor::new(arrow, hotspot, arrow_size, halo)
        });
        update(cursor);

        match output {
            Some(output) => self.niri.queue_redraw(&output),
            None => self.niri.queue_redraw_all(),
        }

        if !self.agent().cursor_timer {
            self.agent().cursor_timer = true;
            self.niri
                .event_loop
                .insert_source(Timer::from_duration(CURSOR_TICK), |_, _, state| {
                    state.agent_cursor_tick()
                })
                .unwrap();
        }
    }

    /// Redraws the agent cursor while it animates, and removes it once it has faded out.
    fn agent_cursor_tick(&mut self) -> TimeoutAction {
        let Some(toplevel) = self.agent().cursor_window.clone() else {
            self.agent().cursor_timer = false;
            return TimeoutAction::Drop;
        };

        let mut redraw = None;
        let mut visible = false;
        if let Some((mapped, output)) = self.niri.layout.find_window_and_output_mut(&toplevel) {
            if let Some(cursor) = &mapped.agent_cursor {
                visible = cursor.is_visible();
                if cursor.is_animating() || !visible {
                    redraw = Some(output.cloned());
                }
                if !visible {
                    mapped.agent_cursor = None;
                }
            }
        }

        match redraw {
            Some(Some(output)) => self.niri.queue_redraw(&output),
            Some(None) => self.niri.queue_redraw_all(),
            None => (),
        }

        if visible {
            TimeoutAction::ToDuration(CURSOR_TICK)
        } else {
            let agent = self.agent();
            agent.cursor_window = None;
            agent.cursor_timer = false;
            TimeoutAction::Drop
        }
    }

    fn agent_button(&mut self, target: &Target, button: AgentButton, state: ButtonState) {
        let seat = self.agent_seat();
        let pointer = seat.get_pointer().unwrap();

        // Click to focus, like the user's seat. A click inside an open popup is handled by the
        // popup grab instead.
        if state == ButtonState::Pressed && !self.agent_popup_grab_on(&target.toplevel) {
            self.agent_focus(target);
        }
        if state == ButtonState::Pressed {
            self.agent_update_cursor(&target.toplevel, AgentCursor::click);
        }

        let button = match button {
            AgentButton::Left => BTN_LEFT,
            AgentButton::Right => BTN_RIGHT,
            AgentButton::Middle => BTN_MIDDLE,
        };
        pointer.button(
            self,
            &ButtonEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
                button,
                state,
            },
        );
        pointer.frame(self);
    }

    fn agent_scroll(&mut self, dx: f64, dy: f64) {
        let seat = self.agent_seat();
        let pointer = seat.get_pointer().unwrap();

        let mut frame = AxisFrame::new(InputTime::now()).source(AxisSource::Wheel);
        for (axis, value) in [(Axis::Horizontal, dx), (Axis::Vertical, dy)] {
            if value != 0. {
                let v120 = (value / WHEEL_STEP * 120.).round() as i32;
                frame = frame.value(axis, value).v120(axis, v120);
            }
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    fn agent_key(&mut self, target: &Target, keys: &str) -> Result<(), String> {
        let syms = parse_keys(keys)?;

        let seat = self.agent_seat();
        let keyboard = seat.get_keyboard().unwrap();
        let codes = syms
            .iter()
            .map(|sym| {
                keyboard.keycode_for_keysym(*sym).ok_or_else(|| {
                    format!(
                        "key {} is not on the keyboard layout",
                        xkb::keysym_get_name(*sym)
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        self.agent_focus(target);

        let input = |state: &mut State, code, key_state| {
            keyboard.input(
                state,
                code,
                key_state,
                SERIAL_COUNTER.next_serial(),
                InputTime::now(),
                |_, _, _| FilterResult::<()>::Forward,
            );
        };
        for code in &codes {
            input(self, *code, KeyState::Pressed);
        }
        for code in codes.iter().rev() {
            input(self, *code, KeyState::Released);
        }

        Ok(())
    }

    fn agent_type(&mut self, target: &Target, text: &str) {
        let syms: Vec<Keysym> = text
            .chars()
            .map(|c| match c {
                '\n' => Keysym::Return,
                '\t' => Keysym::Tab,
                c => xkb::utf32_to_keysym(c as u32),
            })
            .filter(|sym| *sym != Keysym::NoSymbol)
            .collect();

        self.agent_focus(target);

        // Type through a temporary keymap that puts the text's keysyms on keycodes that the
        // regular layout leaves empty. GTK matches shortcuts by keycode against the regular layout
        // too, so reusing ordinary keycodes would turn some letters into F1, F11 or Delete.
        let seat = self.agent_seat();
        let keyboard = seat.get_keyboard().unwrap();
        let (original, free) = keyboard.with_xkb_state(self, |context| {
            let xkb = context.xkb().lock().unwrap();
            let free: Vec<Keycode> = (MIN_KEYCODE..=MAX_KEYCODE)
                .map(Keycode::new)
                .filter(|code| {
                    xkb.layouts()
                        .all(|layout| xkb.raw_syms_for_key_in_layout(*code, layout).is_empty())
                })
                .collect();
            (xkb.keymap_as_string(), free)
        });
        if free.is_empty() {
            warn!("no free keycodes to type with");
            return;
        }

        let mut rest = &syms[..];
        while !rest.is_empty() {
            // Take as much text as has at most one keysym per free keycode.
            let mut distinct: Vec<Keysym> = Vec::new();
            let mut len = 0;
            for sym in rest {
                if !distinct.contains(sym) {
                    if distinct.len() == free.len() {
                        break;
                    }
                    distinct.push(*sym);
                }
                len += 1;
            }
            let (chunk, tail) = rest.split_at(len);
            rest = tail;

            let keymap = injection_keymap(&distinct, &free);
            if let Err(err) = keyboard.set_keymap_from_string(self, keymap) {
                warn!("error setting the agent typing keymap: {err:?}");
                break;
            }
            for sym in chunk {
                let idx = distinct.iter().position(|s| s == sym).unwrap();
                for state in [KeyState::Pressed, KeyState::Released] {
                    keyboard.input(
                        self,
                        free[idx],
                        state,
                        SERIAL_COUNTER.next_serial(),
                        InputTime::now(),
                        |_, _, _| FilterResult::<()>::Forward,
                    );
                }
            }
        }

        if let Err(err) = keyboard.set_keymap_from_string(self, original) {
            warn!("error restoring the agent keymap: {err:?}");
        }
    }

    /// Gives agent keyboard focus to the target window, unless one of its popups holds it.
    ///
    /// Returns whether the focus changed.
    fn agent_focus(&mut self, target: &Target) -> bool {
        if self.agent_popup_grab_on(&target.toplevel) {
            return false;
        }

        let seat = self.agent_seat();
        let keyboard = seat.get_keyboard().unwrap();
        if keyboard.current_focus().as_ref() == Some(&target.toplevel) {
            return false;
        }

        let serial = SERIAL_COUNTER.next_serial();
        keyboard.set_focus(self, Some(target.toplevel.clone()), serial);
        true
    }

    fn agent_popup_grab_on(&mut self, root: &WlSurface) -> bool {
        let agent = self.agent();
        if agent
            .popup_grab
            .as_ref()
            .is_some_and(|(_, grab)| grab.has_ended())
        {
            let (grab_root, _) = agent.popup_grab.take().unwrap();

            // The keyboard focus stays on the closed popup; give it back to its window.
            let keyboard = agent.seat.get_keyboard().unwrap();
            if keyboard.current_focus().is_none_or(|focus| !focus.alive()) && grab_root.alive() {
                keyboard.set_focus(self, Some(grab_root), SERIAL_COUNTER.next_serial());
            }
        }

        let agent = self.agent();
        agent
            .popup_grab
            .as_ref()
            .is_some_and(|(grab_root, _)| grab_root == root)
    }

    /// Handles an xdg_popup grab requested with the agent seat.
    ///
    /// Returns `false` if `seat` is not the agent seat. The agent seat doesn't take part in the
    /// user's focus, so this skips all checks against the user's focus and just grabs.
    pub fn agent_popup_grab(
        &mut self,
        surface: &PopupSurface,
        seat: &Seat<State>,
        serial: smithay::utils::Serial,
    ) -> bool {
        if !self
            .niri
            .agent
            .as_ref()
            .is_some_and(|agent| agent.is_agent_seat(seat))
        {
            return false;
        }

        let popup = PopupKind::Xdg(surface.clone());
        let Ok(root) = find_popup_root_surface(&popup) else {
            return true;
        };

        let mut grab = match self
            .niri
            .popups
            .grab_popup(root.clone(), popup, seat, serial)
        {
            Ok(grab) => grab,
            Err(err) => {
                trace!("ignoring agent popup grab: {err:?}");
                return true;
            }
        };

        let keyboard = seat.get_keyboard().unwrap();
        let pointer = seat.get_pointer().unwrap();
        let keyboard_grab_mismatches = keyboard.is_grabbed()
            && !(keyboard.has_grab(serial)
                || grab.previous_serial().is_none_or(|s| keyboard.has_grab(s)));
        let pointer_grab_mismatches = pointer.is_grabbed()
            && !(pointer.has_grab(serial)
                || grab.previous_serial().is_none_or(|s| pointer.has_grab(s)));
        if keyboard_grab_mismatches || pointer_grab_mismatches {
            trace!("ignoring agent popup grab because of current grab mismatch");
            grab.ungrab(smithay::desktop::PopupUngrabStrategy::All);
            return true;
        }

        keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        let agent = self.agent();
        agent.popup_grab = Some((root, grab));
        agent.popup_grab_since = Instant::now();
        true
    }

    /// Renders the window with its popups and replies with the PNG from a thread.
    fn agent_capture(
        &mut self,
        id: u64,
        max_size: Option<u32>,
        reply: Sender<Reply>,
    ) -> Result<(), String> {
        // Render smaller rather than downscaling the image afterwards.
        let mut scale = self.agent_window_scale(id);
        if let Some(max_size) = max_size {
            let bbox = self.agent_mapped(id)?.window.bbox_with_popups();
            let longest = f64::from(bbox.size.w.max(bbox.size.h).max(1));
            scale = scale.min(f64::from(max_size.max(1)) / longest);
        }

        let Rendered {
            area: geo,
            pixels,
            scale,
        } = self.agent_render(id, false, Some(scale))?;

        let origin = geo.loc.to_f64().to_logical(scale);
        let scale = scale.x;
        thread::spawn(move || {
            let mut png = Vec::new();
            let result = write_capture_png(&mut png, geo.size.w as u32, geo.size.h as u32, &pixels)
                .map(|()| {
                    Response::AgentCapture(AgentCapture {
                        png_base64: base64::engine::general_purpose::STANDARD.encode(&png),
                        origin: (origin.x, origin.y),
                        scale,
                    })
                })
                .map_err(|err| format!("error encoding image: {err:?}"));
            let _ = reply.send_blocking(result);
        });

        Ok(())
    }

    fn agent_info(&self, id: u64) -> Result<AgentInfo, String> {
        let mapped = self.agent_mapped(id)?;
        let geo = mapped.window.geometry();
        Ok(AgentInfo {
            id,
            pid: mapped.credentials().map(|creds| creds.pid),
            surface_offset: (f64::from(geo.loc.x), f64::from(geo.loc.y)),
            size: (f64::from(geo.size.w), f64::from(geo.size.h)),
            scale: self.agent_window_scale(id),
            popup_surfaces: receipt::popup_surfaces(mapped),
        })
    }

    /// Scale of the output the window is on.
    fn agent_window_scale(&self, id: u64) -> f64 {
        let mut windows = self.niri.layout.windows();
        let monitor = windows.find(|(_, mapped)| mapped.id().get() == id);
        monitor
            .and_then(|(monitor, _)| monitor)
            .map_or(1., |mon| mon.output().current_scale().fractional_scale())
    }

    /// Renders the window to RGBA pixels, returning the rendered area in window coordinates.
    ///
    /// With `geometry_only`, the area is just the window geometry; otherwise it also covers
    /// client-side shadows and popups.
    fn agent_render(
        &mut self,
        id: u64,
        geometry_only: bool,
        scale: Option<f64>,
    ) -> Result<Rendered, String> {
        let scale = Scale::from(scale.unwrap_or_else(|| self.agent_window_scale(id)));
        let mapped = self
            .niri
            .layout
            .windows()
            .find(|(_, mapped)| mapped.id().get() == id)
            .map(|(_, mapped)| mapped)
            .ok_or_else(|| format!("window {id} was not found"))?;

        let result = self.backend.with_primary_renderer(|renderer| {
            let mut elements = Vec::new();
            let ctx = RenderCtx {
                renderer,
                target: RenderTarget::ScreenCapture,
                xray: None,
            };
            // Render the window geometry at the origin, so element positions are in window
            // coordinates.
            // The agent doesn't see its own cursor.
            cursor::without_cursor(|| {
                mapped.render(
                    ctx,
                    Point::from((0., 0.)),
                    scale,
                    1.,
                    XrayPos::default(),
                    &mut |elem| elements.push(elem),
                )
            });

            let geo = if geometry_only {
                let size = mapped.window.geometry().size;
                Rectangle::from_size(size.to_physical_precise_round(scale))
            } else {
                encompassing_geo(scale, elements.iter())
            };
            let elements = elements.iter().rev().map(|elem| {
                RelocateRenderElement::from_element(elem, geo.loc.upscale(-1), Relocate::Relative)
            });
            let pixels = render_to_vec(
                renderer,
                geo.size,
                scale,
                Transform::Normal,
                Fourcc::Abgr8888,
                elements,
            )?;
            anyhow::Ok((geo, pixels))
        });
        let (geo, pixels) = result
            .ok_or_else(|| String::from("primary renderer is not available"))?
            .map_err(|err| format!("error rendering window: {err:?}"))?;

        Ok(Rendered {
            area: geo,
            pixels,
            scale,
        })
    }
}

/// Encodes a capture as PNG, favoring speed: the image is read once, right away.
fn write_capture_png(
    w: impl std::io::Write,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<(), png::EncodingError> {
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(pixels)
}

/// A keymap with each keysym on its own keycode at the base level, so no modifiers are needed.
fn injection_keymap(syms: &[Keysym], codes: &[Keycode]) -> String {
    let mut keycodes = String::new();
    let mut symbols = String::new();
    for (sym, code) in syms.iter().zip(codes) {
        let code = code.raw();
        keycodes.push_str(&format!("    <K{code}> = {code};\n"));
        symbols.push_str(&format!(
            "    key <K{code}> {{ [ {} ] }};\n",
            xkb::keysym_get_name(*sym)
        ));
    }
    format!(
        "xkb_keymap {{\n\
         xkb_keycodes \"agent\" {{\n    minimum = {MIN_KEYCODE};\n    maximum = {MAX_KEYCODE};\n{keycodes}}};\n\
         xkb_types \"agent\" {{ include \"complete\" }};\n\
         xkb_compatibility \"agent\" {{ include \"complete\" }};\n\
         xkb_symbols \"agent\" {{\n{symbols}}};\n\
         }};\n"
    )
}

/// Parses a combination like `ctrl+shift+t` into keysyms, modifiers first.
fn parse_keys(keys: &str) -> Result<Vec<Keysym>, String> {
    let mut parts: Vec<&str> = keys.split('+').map(str::trim).collect();
    // Allow "ctrl+plus" style as well as a literal trailing "+": "ctrl++".
    if keys.ends_with("++") {
        parts.pop();
        parts.pop();
        parts.push("+");
    }
    let (key, mods) = parts
        .split_last()
        .filter(|(key, _)| !key.is_empty())
        .ok_or_else(|| format!("no key in {keys:?}"))?;

    let mut syms = Vec::new();
    for name in mods {
        let sym = match name.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Keysym::Control_L,
            "shift" => Keysym::Shift_L,
            "alt" => Keysym::Alt_L,
            "super" | "mod4" | "logo" | "win" => Keysym::Super_L,
            _ => return Err(format!("unknown modifier {name:?} in {keys:?}")),
        };
        syms.push(sym);
    }

    let mut sym = xkb::keysym_from_name(key, xkb::KEYSYM_NO_FLAGS);
    if sym == Keysym::NoSymbol {
        sym = xkb::keysym_from_name(key, xkb::KEYSYM_CASE_INSENSITIVE);
    }
    if sym == Keysym::NoSymbol {
        let mut chars = key.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            sym = xkb::utf32_to_keysym(c as u32);
        }
    }
    if sym == Keysym::NoSymbol {
        return Err(format!("unknown key {key:?} in {keys:?}"));
    }
    syms.push(sym);

    Ok(syms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_combinations() {
        assert_eq!(parse_keys("Return").unwrap(), [Keysym::Return]);
        assert_eq!(parse_keys("return").unwrap(), [Keysym::Return]);
        assert_eq!(
            parse_keys("ctrl+shift+t").unwrap(),
            [Keysym::Control_L, Keysym::Shift_L, Keysym::t]
        );
        assert_eq!(
            parse_keys("ctrl++").unwrap(),
            [Keysym::Control_L, Keysym::plus]
        );
        assert_eq!(parse_keys("alt+F4").unwrap(), [Keysym::Alt_L, Keysym::F4]);
        assert!(parse_keys("hyper+a").is_err());
        assert!(parse_keys("ctrl+").is_err());
    }
}
