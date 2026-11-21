//! Takeover: the user operating windows of agent apps.
//!
//! Agent apps see only the agent seat, so the user's own seat can't reach their windows. niri
//! hands the user's input on them to the app through the agent seat instead. Pressing a button or
//! key on such a window takes it over: agent input is refused until the user's keyboard focus
//! leaves the window, and the clipboards of the two seats are synced in the meantime.

use std::cell::{Cell, Ref};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use smithay::backend::input::{ButtonState, InputTime, KeyState};
use smithay::input::keyboard::{FilterResult, Keycode};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent};
use smithay::input::Seat;
use smithay::reexports::rustix::fs::{fcntl_setfl, memfd_create, MemfdFlags, OFlags};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{DisplayHandle, Resource as _};
use smithay::utils::{IsAlive, Logical, Point, SERIAL_COUNTER};
use smithay::wayland::input_method::InputMethodSeat;
use smithay::wayland::selection::data_device::{
    clear_data_device_selection, current_data_device_selection_userdata,
    request_data_device_client_selection, set_data_device_selection,
};
use smithay::wayland::selection::primary_selection::{
    clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection,
    set_primary_selection,
};
use smithay::wayland::selection::{SelectionSource, SelectionTarget};

use super::is_agent_surface;
use crate::niri::State;

/// How long after the last agent action the user's hovering reaches agent apps again.
///
/// Until then, the agent pointer stays where the agent left it, so that the user passing over an
/// agent window doesn't move it in the middle of an action.
const HOVER_AFTER: Duration = Duration::from_secs(2);

const TARGETS: [SelectionTarget; 2] = [SelectionTarget::Clipboard, SelectionTarget::Primary];

#[derive(Default)]
pub struct Takeover {
    /// Toplevel that the user took over.
    window: Option<WlSurface>,
    /// Whether the user's pointer drives the agent pointer.
    pointer: bool,
    /// Keys that the user pressed through the agent keyboard.
    keys: Vec<Keycode>,
    /// When the agent last sent input.
    last_action: Option<Instant>,
    /// Selections that niri set on one seat to pass on the other seat's.
    mirrors: Vec<Mirror>,
    /// Mime types of the latest selection that a client set, per seat and target.
    selections: Vec<(Side, SelectionTarget, Vec<String>)>,
    /// The agent's selections while the window is using the user's clipboard.
    saved_agent_selections: Vec<(SelectionTarget, Rc<ClipboardCopy>)>,
}

impl Takeover {
    fn formats(&self, side: Side, ty: SelectionTarget) -> Option<Vec<String>> {
        self.selections
            .iter()
            .find(|(owner, target, _)| *owner == side && *target == ty)
            .map(|(_, _, mimes)| mimes.clone())
    }

    fn copy_for(&self, token: &Arc<[u8]>) -> Option<Rc<ClipboardCopy>> {
        self.mirrors
            .iter()
            .find(|mirror| Arc::ptr_eq(&mirror.token, token))?
            .copy
            .clone()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    User,
    Agent,
}

impl Side {
    fn other(self) -> Self {
        match self {
            Side::User => Side::Agent,
            Side::Agent => Side::User,
        }
    }
}

struct Mirror {
    /// User data of the selection, to recognize it by.
    token: Arc<[u8]>,
    /// Seat that holds the mirror; it passes on the other seat's selection.
    on: Side,
    ty: SelectionTarget,
    /// A user's copy made during takeover, preserved independently of the agent's selection.
    copy: Option<Rc<ClipboardCopy>>,
}

type ClipboardFile = Arc<OnceLock<Option<File>>>;

struct ClipboardCopy {
    files: Vec<(String, ClipboardFile)>,
    started: Cell<bool>,
}

impl ClipboardCopy {
    fn new(mimes: Vec<String>) -> Self {
        Self {
            files: mimes
                .into_iter()
                .map(|mime| (mime, Arc::default()))
                .collect(),
            started: Cell::new(false),
        }
    }

    fn capture(&self, seat: &Seat<State>, ty: SelectionTarget) {
        if self.started.replace(true) {
            return;
        }
        for (mime, data) in &self.files {
            match capture_clipboard_file(data.clone()) {
                Ok(fd) => request_selection(seat, ty, mime.clone(), fd),
                Err(err) => {
                    warn!("error starting clipboard transfer for {mime}: {err}");
                    let _ = data.set(None);
                }
            }
        }
    }
}

impl Drop for ClipboardCopy {
    fn drop(&mut self) {
        // A newer clipboard selection can replace this one before its idle callback starts it.
        if !self.started.get() {
            for (_, data) in &self.files {
                let _ = data.set(None);
            }
        }
    }
}

fn selection_data(seat: &Seat<State>, ty: SelectionTarget) -> Option<Ref<'_, Arc<[u8]>>> {
    match ty {
        SelectionTarget::Clipboard => current_data_device_selection_userdata(seat),
        SelectionTarget::Primary => current_primary_selection_userdata(seat),
    }
}

fn clear_selection(seat: &Seat<State>, dh: &DisplayHandle, ty: SelectionTarget) {
    match ty {
        SelectionTarget::Clipboard => clear_data_device_selection(dh, seat),
        SelectionTarget::Primary => clear_primary_selection(dh, seat),
    }
}

fn request_selection(seat: &Seat<State>, ty: SelectionTarget, mime: String, fd: OwnedFd) {
    let result = match ty {
        SelectionTarget::Clipboard => {
            request_data_device_client_selection(seat, mime, fd).map_err(|err| err.to_string())
        }
        SelectionTarget::Primary => {
            request_primary_client_selection(seat, mime, fd).map_err(|err| err.to_string())
        }
    };
    if let Err(err) = result {
        debug!("error requesting clipboard data: {err}");
    }
}

impl State {
    /// Refuses agent input while the user has taken over a window, and records the action.
    pub(super) fn agent_check_takeover(&mut self) -> Result<(), String> {
        if let Some(window) = self.agent_takeover_window() {
            let window = match self.niri.layout.find_window_and_output(&window) {
                Some((mapped, _)) => format!("window {}", mapped.id().get()),
                None => String::from("an agent window"),
            };
            return Err(format!(
                "the user took over {window} and is using it; agent input waits until they \
                 switch away from it"
            ));
        }

        let takeover = &mut self.agent().takeover;
        takeover.last_action = Some(Instant::now());
        takeover.pointer = false;
        Ok(())
    }

    /// The window that the user took over, if it's still around.
    fn agent_takeover_window(&mut self) -> Option<WlSurface> {
        let window = self.niri.agent.as_ref()?.takeover.window.clone()?;
        if window.alive() {
            Some(window)
        } else {
            self.agent_end_takeover();
            None
        }
    }

    /// Whether the user's pointer drives the agent pointer, so the cursor that the agent app sets
    /// is the user's.
    pub fn agent_user_drives_pointer(&self) -> bool {
        self.niri
            .agent
            .as_ref()
            .is_some_and(|agent| agent.takeover.pointer)
    }

    /// The agent window that takes the user's input at `surface`.
    fn agent_user_target(&self, surface: &WlSurface) -> Option<WlSurface> {
        if !is_agent_surface(surface) {
            return None;
        }
        let root = self.niri.find_root_shell_surface(surface);
        self.niri
            .layout
            .find_window_and_output(&root)
            .is_some()
            .then_some(root)
    }

    fn agent_start_takeover(&mut self, window: WlSurface) {
        if self.niri.agent.as_ref().unwrap().takeover.window.as_ref() == Some(&window) {
            return;
        }
        self.agent_end_takeover();
        debug!("the user took over an agent window");

        // The user's own cursor is right there.
        let agent = self.agent();
        if agent.cursor_window.as_ref() == Some(&window) {
            agent.cursor_window = None;
        }
        if let Some((mapped, output)) = self.niri.layout.find_window_and_output_mut(&window) {
            if mapped.agent_cursor.take().is_some() {
                match output.cloned() {
                    Some(output) => self.niri.queue_redraw(&output),
                    None => self.niri.queue_redraw_all(),
                }
            }
        }

        self.agent_give_keyboard(&window);
        self.agent().takeover.window = Some(window);

        // Preserve the agent's clipboard before handing the user's clipboard to this window.
        let user_seat = self.niri.seat.clone();
        let agent_seat = self.agent_seat();
        for ty in TARGETS {
            let takeover = &mut self.agent().takeover;
            let saved = if let Some(token) = selection_data(&agent_seat, ty) {
                takeover.copy_for(&token)
            } else {
                takeover.formats(Side::Agent, ty).map(|mimes| {
                    let copy = Rc::new(ClipboardCopy::new(mimes));
                    copy.capture(&agent_seat, ty);
                    copy
                })
            };
            if let Some(saved) = saved {
                takeover.saved_agent_selections.push((ty, saved));
            }

            if let Some(token) = selection_data(&user_seat, ty) {
                if let Some(copy) = takeover.copy_for(&token) {
                    self.agent_offer_copy(Side::Agent, ty, copy);
                    continue;
                }
            } else if let Some(mimes) = takeover.formats(Side::User, ty) {
                self.agent_mirror_selection(Side::Agent, ty, mimes, None);
                continue;
            }
            clear_selection(&agent_seat, &self.niri.display_handle, ty);
        }
    }

    /// Hands the window that the user took over back to the agent.
    fn agent_end_takeover(&mut self) {
        let Some(agent) = &mut self.niri.agent else {
            return;
        };
        if agent.takeover.window.take().is_none() {
            return;
        }
        debug!("the user handed an agent window back");

        let seat = agent.seat.clone();
        let keys = std::mem::take(&mut agent.takeover.keys);
        let saved = std::mem::take(&mut agent.takeover.saved_agent_selections);
        let keyboard = seat.get_keyboard().unwrap();
        for code in keys {
            keyboard.input(
                self,
                code,
                KeyState::Released,
                SERIAL_COUNTER.next_serial(),
                InputTime::now(),
                |_, _, _| FilterResult::<()>::Forward,
            );
        }

        // Finish requesting the user's data before replacing the app's selection.
        for ty in TARGETS {
            self.agent_capture_selection(ty);
            let takeover = &mut self.agent().takeover;
            takeover
                .selections
                .retain(|(side, t, _)| !(*side == Side::Agent && *t == ty));
            if let Some((_, copy)) = saved.iter().find(|(target, _)| *target == ty) {
                self.agent_offer_copy(Side::Agent, ty, copy.clone());
            } else {
                takeover
                    .mirrors
                    .retain(|mirror| !(mirror.on == Side::Agent && mirror.ty == ty));
                clear_selection(&seat, &self.niri.display_handle, ty);
            }
        }
    }

    /// Gives the agent keyboard focus to the window, unless one of its popups holds it.
    fn agent_give_keyboard(&mut self, window: &WlSurface) {
        let seat = self.agent_seat();
        let keyboard = seat.get_keyboard().unwrap();
        if !self.agent_popup_grab_on(window) && keyboard.current_focus().as_ref() != Some(window) {
            keyboard.set_focus(self, Some(window.clone()), SERIAL_COUNTER.next_serial());
        }
    }

    /// Follows the user's keyboard focus: the takeover moves along to other windows of the same
    /// app, like its dialogs, and ends when the focus goes anywhere else.
    pub fn agent_on_user_focus(&mut self, focus: Option<&WlSurface>) {
        let Some(agent) = &self.niri.agent else {
            return;
        };
        let takeover = &agent.takeover;
        let window = takeover.window.clone();
        let idle = takeover
            .last_action
            .is_none_or(|at| at.elapsed() >= HOVER_AFTER);
        let target = focus.and_then(|surface| self.agent_user_target(surface));

        match (window, target) {
            (Some(window), Some(target)) if window == target => (),
            (Some(window), Some(target)) if window.client() == target.client() => {
                self.agent_give_keyboard(&target);
                self.agent().takeover.window = Some(target);
            }
            (Some(_), target) => {
                self.agent_end_takeover();
                if let Some(target) = target.filter(|_| idle) {
                    self.agent_give_keyboard(&target);
                }
            }
            // Focus the agent app right away, so that the first key the user types doesn't come
            // together with the focus, which GTK 3 drops.
            (None, Some(target)) if idle => self.agent_give_keyboard(&target),
            (None, _) => (),
        }
    }

    /// Whether the user operates the app of this agent surface, so that its new windows may take
    /// the focus.
    pub fn agent_user_operates(&self, surface: &WlSurface) -> bool {
        self.niri.agent.as_ref().is_some_and(|agent| {
            agent
                .takeover
                .window
                .as_ref()
                .is_some_and(|window| window.alive() && window.client() == surface.client())
        })
    }

    /// Passes the user's pointer motion on to the agent app under the pointer.
    pub fn agent_user_motion(
        &mut self,
        under: Option<(WlSurface, Point<f64, Logical>)>,
        location: Point<f64, Logical>,
        time: InputTime,
    ) {
        let Some(agent) = &self.niri.agent else {
            return;
        };
        let takeover = &agent.takeover;
        let target = under
            .as_ref()
            .and_then(|(surface, _)| self.agent_user_target(surface));

        let forward = match (&takeover.window, &target) {
            (Some(window), Some(target)) => window == target || takeover.pointer,
            // Hovering reaches agent apps when the agent isn't busy with its pointer.
            (None, Some(_)) => {
                takeover.pointer
                    || takeover
                        .last_action
                        .is_none_or(|at| at.elapsed() >= HOVER_AFTER)
            }
            // Leaving an agent window, or dragging out of it.
            (_, None) => takeover.pointer,
        };
        if !forward {
            return;
        }

        let focus = target.is_some().then_some(under).flatten();
        let seat = self.agent_seat();
        let pointer = seat.get_pointer().unwrap();
        pointer.motion(
            self,
            focus,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);

        // An implicit grab keeps the focus while the user drags out of the window.
        let drives = pointer.current_focus().is_some();
        self.agent().takeover.pointer = drives;
    }

    /// Passes a button of the user's pointer on to the agent app under it.
    pub fn agent_user_button(&mut self, button: u32, state: ButtonState, time: InputTime) {
        if self.niri.agent.is_none() {
            return;
        }

        if state == ButtonState::Pressed {
            let under = self.niri.pointer_contents.surface.clone();
            let target = under
                .as_ref()
                .and_then(|(surface, _)| self.agent_user_target(surface));
            match target {
                Some(target) => {
                    self.agent_start_takeover(target);
                    if !self.agent().takeover.pointer {
                        let location = self.niri.seat.get_pointer().unwrap().current_location();
                        self.agent_user_motion(under, location, time);
                    }
                }
                None => {
                    // Clicking elsewhere closes menus open in the window, like for the user's
                    // own apps.
                    if let Some(window) = self.agent().takeover.window.clone() {
                        if self.agent_popup_grab_on(&window) {
                            if let Some((_, grab)) = &mut self.agent().popup_grab {
                                grab.ungrab(smithay::desktop::PopupUngrabStrategy::All);
                            }
                        }
                    }
                }
            }
        }

        if !self.agent().takeover.pointer {
            return;
        }
        let seat = self.agent_seat();
        let pointer = seat.get_pointer().unwrap();
        pointer.button(
            self,
            &ButtonEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                button,
                state,
            },
        );
        pointer.frame(self);
    }

    /// Passes the user's scrolling on to the agent app under the pointer.
    pub fn agent_user_axis(&mut self, frame: AxisFrame) {
        if !self.agent_user_drives_pointer() {
            return;
        }
        let seat = self.agent_seat();
        let pointer = seat.get_pointer().unwrap();
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// Passes a key that the user's keyboard sent to its focus on to the agent app, when the
    /// focus is one of its windows.
    pub fn agent_user_key(&mut self, code: Keycode, state: KeyState, time: InputTime) {
        self.agent_pass_user_key(code, state, time, false);
    }

    /// Passes a key that the user's input method didn't use on to the agent app, when the user's
    /// keyboard focus is one of its windows.
    pub fn agent_user_ime_key(&mut self, code: Keycode, state: KeyState, time: InputTime) {
        self.agent_pass_user_key(code, state, time, true);
    }

    fn agent_pass_user_key(
        &mut self,
        code: Keycode,
        state: KeyState,
        time: InputTime,
        from_input_method: bool,
    ) {
        if self.niri.agent.is_none() {
            return;
        }

        // Agent apps share the user's input method (see AgentState::new). While it holds the
        // keyboard, keys go to it first, and it sends the ones it doesn't use back.
        let to_input_method =
            !from_input_method && self.niri.seat.input_method().keyboard_grabbed();

        if state == KeyState::Pressed {
            let focus = self.niri.seat.get_keyboard().unwrap().current_focus();
            let Some(target) = focus.and_then(|focus| self.agent_user_target(&focus)) else {
                return;
            };
            self.agent_start_takeover(target);
            if to_input_method {
                return;
            }
            self.agent().takeover.keys.push(code);
        } else {
            let keys = &mut self.agent().takeover.keys;
            let Some(idx) = keys.iter().position(|key| *key == code) else {
                return;
            };
            keys.remove(idx);
        }

        let seat = self.agent_seat();
        let keyboard = seat.get_keyboard().unwrap();
        keyboard.input(
            self,
            code,
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |_, _, _| FilterResult::<()>::Forward,
        );
    }

    /// Syncs a selection that a client set to the other seat during a takeover.
    pub fn agent_new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        seat: &Seat<State>,
    ) {
        let Some(agent) = &self.niri.agent else {
            return;
        };
        let side = if agent.is_agent_seat(seat) {
            Side::Agent
        } else if *seat == self.niri.seat {
            Side::User
        } else {
            return;
        };

        // Smithay calls this before it changes the selection. If another copy came in before
        // idle, this is our last chance to request the previous selection from its owner.
        if side == Side::Agent {
            self.agent_capture_selection(ty);
        }

        let takeover = &mut self.agent().takeover;
        takeover
            .selections
            .retain(|(s, t, _)| !(*s == side && *t == ty));
        let mimes = source.map(|source| source.mime_types());
        if let Some(mimes) = &mimes {
            takeover.selections.push((side, ty, mimes.clone()));
        }
        // The client's selection replaced the mirror on this seat.
        takeover
            .mirrors
            .retain(|mirror| !(mirror.on == side && mirror.ty == ty));

        if takeover.window.is_none() {
            return;
        }
        match (side, mimes) {
            (_, None) => self.agent_clear_mirror(side.other(), ty),
            (Side::User, Some(mimes)) => self.agent_mirror_selection(Side::Agent, ty, mimes, None),
            (Side::Agent, Some(mimes)) => {
                self.agent_offer_copy(Side::User, ty, Rc::new(ClipboardCopy::new(mimes)));
                // The new client selection becomes current after this callback returns.
                self.niri
                    .event_loop
                    .insert_idle(move |state| state.agent_capture_selection(ty));
            }
        }
    }

    fn agent_capture_selection(&mut self, ty: SelectionTarget) {
        let copy = self
            .agent()
            .takeover
            .mirrors
            .iter()
            .find(|mirror| mirror.on == Side::User && mirror.ty == ty)
            .and_then(|mirror| mirror.copy.clone());
        if let Some(copy) = copy {
            copy.capture(&self.agent_seat(), ty);
        }
    }

    fn agent_seat_of(&mut self, side: Side) -> Seat<State> {
        match side {
            Side::User => self.niri.seat.clone(),
            Side::Agent => self.agent_seat(),
        }
    }

    /// Offer the user's clipboard to the destination seat, retaining copies from agent apps.
    fn agent_offer_copy(&mut self, on: Side, ty: SelectionTarget, copy: Rc<ClipboardCopy>) {
        let mimes = copy.files.iter().map(|(mime, _)| mime.clone()).collect();
        self.agent_mirror_selection(on, ty, mimes, Some(copy));
    }

    fn agent_mirror_selection(
        &mut self,
        on: Side,
        ty: SelectionTarget,
        mimes: Vec<String>,
        copy: Option<Rc<ClipboardCopy>>,
    ) {
        // Only recognized by its address.
        let token: Arc<[u8]> = Arc::from(Vec::new());
        let seat = self.agent_seat_of(on);
        let dh = &self.niri.display_handle;
        match ty {
            SelectionTarget::Clipboard => {
                set_data_device_selection(dh, &seat, mimes, token.clone())
            }
            SelectionTarget::Primary => set_primary_selection(dh, &seat, mimes, token.clone()),
        }

        let mirrors = &mut self.agent().takeover.mirrors;
        mirrors.retain(|mirror| !(mirror.on == on && mirror.ty == ty));
        mirrors.push(Mirror {
            token,
            on,
            ty,
            copy,
        });
    }

    /// Clears the selection on the seat on `on` if it's a mirror.
    fn agent_clear_mirror(&mut self, on: Side, ty: SelectionTarget) {
        let mirrors = &mut self.agent().takeover.mirrors;
        let Some(idx) = mirrors
            .iter()
            .position(|mirror| mirror.on == on && mirror.ty == ty)
        else {
            return;
        };
        let mirror = mirrors.remove(idx);

        let seat = self.agent_seat_of(on);
        if !selection_data(&seat, ty).is_some_and(|data| Arc::ptr_eq(&data, &mirror.token)) {
            return;
        }
        clear_selection(&seat, &self.niri.display_handle, ty);
    }

    /// Serves clipboard copies and forwards requests for live selections.
    ///
    /// Gives `fd` back if `user_data` isn't a mirror.
    pub fn agent_send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        user_data: &Arc<[u8]>,
    ) -> Option<OwnedFd> {
        let Some(agent) = &self.niri.agent else {
            return Some(fd);
        };
        let Some(mirror) = agent
            .takeover
            .mirrors
            .iter()
            .find(|mirror| Arc::ptr_eq(&mirror.token, user_data))
        else {
            return Some(fd);
        };

        if let Some(copy) = &mirror.copy {
            if let Some((_, data)) = copy.files.iter().find(|(mime, _)| *mime == mime_type) {
                let data = data.clone();
                if let Err(err) = thread::Builder::new().spawn(move || {
                    if let Some(file) = data.wait() {
                        if let Err(err) = write_clipboard_file(file, fd) {
                            debug!("error sending copied clipboard data: {err}");
                        }
                    }
                }) {
                    warn!("error starting clipboard paste: {err}");
                }
            }
        } else {
            let from = self.agent_seat_of(mirror.on.other());
            request_selection(&from, ty, mime_type, fd);
        }
        None
    }
}

fn capture_clipboard_file(data: ClipboardFile) -> io::Result<OwnedFd> {
    let (mut read, write) = UnixStream::pair()?;
    read.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut file = File::from(memfd_create("niri-clipboard", MemfdFlags::CLOEXEC)?);

    thread::Builder::new().spawn(move || {
        let file = match io::copy(&mut read, &mut file) {
            Ok(_) => Some(file),
            Err(err) => {
                debug!("error saving clipboard data: {err}");
                None
            }
        };
        let _ = data.set(file);
    })?;
    Ok(write.into())
}

fn write_clipboard_file(file: &File, fd: OwnedFd) -> io::Result<()> {
    // Reopen with an independent offset so simultaneous pastes can read the same data.
    let mut read = File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    fcntl_setfl(&fd, OFlags::empty())?;
    io::copy(&mut read, &mut File::from(fd))?;
    Ok(())
}
