// Portions adapted from Smithay examples.
// MIT License
//
// Copyright (c) 2017 Victor Berger and Victoria Brekenfeld
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use super::*;
use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::utils::on_commit_buffer_handler},
    desktop::PopupKind,
    input::{
        SeatHandler,
        dnd::DndGrabHandler,
        keyboard::{KeyboardTarget, KeysymHandle, ModifiersState},
        pointer::CursorImageStatus,
    },
    reexports::wayland_server::{
        Client, Resource,
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::{wl_buffer::WlBuffer, wl_output::WlOutput},
    },
    utils::{IsAlive, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, get_parent, is_sync_subsurface},
        dmabuf::{DmabufGlobal, DmabufHandler, ImportNotifier},
        output::OutputHandler,
        pointer_constraints::PointerConstraintsHandler,
        selection::{
            SelectionHandler,
            data_device::{DataDeviceHandler, WaylandDndGrabHandler, set_data_device_focus},
        },
        shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler},
        shm::ShmHandler,
        xwayland_shell::XWaylandShellHandler,
    },
    xwayland::{
        X11Surface, XWaylandClientData, XwmHandler,
        xwm::{Reorder, ResizeEdge, XwmId},
    },
};
use std::{collections::HashMap, fs, path::Path};

pub(super) type ClientRects = HashMap<u32, Rectangle<i32, Logical>>;

fn client_rects(line: &str) -> Result<Option<ClientRects>> {
    let Some(values) = line.strip_prefix("GALSCALER_CLIENT ") else {
        return Ok(None);
    };
    let values: Vec<i64> = values
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    anyhow::ensure!(values.len().is_multiple_of(5), "客户区数据不完整");
    let mut rects = ClientRects::new();
    for value in values.chunks_exact(5) {
        let id = u32::try_from(value[0])?;
        let x = i32::try_from(value[1])?;
        let y = i32::try_from(value[2])?;
        let w = i32::try_from(value[3])?;
        let h = i32::try_from(value[4])?;
        anyhow::ensure!(id != 0 && w > 0 && h > 0, "无效的游戏客户区");
        rects.insert(id, Rectangle::new((x, y).into(), (w, h).into()));
    }
    Ok(Some(rects))
}

pub(super) fn read_client_rects(path: &Path) -> Result<Option<ClientRects>> {
    match fs::read_to_string(path) {
        Ok(line) => client_rects(&line),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("无法读取游戏客户区"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_rectangles_crop_decorations_and_preserve_pointer_coordinates() {
        let rects = client_rects("GALSCALER_CLIENT 123 108 230 1280 720 456 -50 0 800 600 ")
            .unwrap()
            .unwrap();
        let view = View::new(rects[&123], (3840, 2160).into());
        assert_eq!(
            view.source,
            Rectangle::new((108, 230).into(), (1280, 720).into())
        );
        assert_eq!(view.target, Rectangle::from_size((3840, 2160).into()));
        assert_eq!(view.pointer((0.0, 0.0).into()), Some((108.0, 230.0).into()));
        assert_eq!(
            view.pointer((1920.0, 1080.0).into()),
            Some((748.0, 590.0).into())
        );
        assert_eq!(rects[&456].loc, (-50, 0).into());
        assert!(
            client_rects("GALSCALER_CLIENT ")
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert!(client_rects("game log").unwrap().is_none());
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Focus(pub Window);

impl Focus {
    fn target(&self) -> &dyn KeyboardTarget<State> {
        if let Some(surface) = self.0.x11_surface() {
            surface
        } else {
            self.0.toplevel().unwrap().wl_surface()
        }
    }
}

impl IsAlive for Focus {
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

impl WaylandFocus for Focus {
    fn wl_surface(&self) -> Option<std::borrow::Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }
}

impl KeyboardTarget<State> for Focus {
    fn enter(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        keys: Vec<KeysymHandle<'_>>,
        serial: Serial,
    ) {
        self.target().enter(seat, data, keys, serial);
    }
    fn leave(&self, seat: &Seat<State>, data: &mut State, serial: Serial) {
        self.target().leave(seat, data, serial);
    }
    fn key(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: InputTime,
    ) {
        self.target().key(seat, data, key, state, serial, time);
    }
    fn modifiers(
        &self,
        seat: &Seat<State>,
        data: &mut State,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        self.target().modifiers(seat, data, modifiers, serial);
    }
}

#[derive(Default)]
pub(super) struct ClientState {
    compositor: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(state) = client.get_data::<XWaylandClientData>() {
            &state.compositor_state
        } else {
            &client.get_data::<ClientState>().unwrap().compositor
        }
    }
    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if !matches!(&self.cursor, CursorImageStatus::Surface(cursor) if cursor == surface) {
            self.dirty = true;
        }
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            let window = self
                .space
                .elements()
                .find(|w| w.wl_surface().as_deref() == Some(&root))
                .cloned();
            if let Some(window) = window {
                window.on_commit();
                if self.seat.get_keyboard().unwrap().current_focus().is_none() {
                    self.focus(&window);
                }
            }
        }
        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
        {
            let _ = popup.send_configure();
        }
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf
    }
    fn dmabuf_imported(&mut self, _: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        let imported = self.renderer.borrow_mut().import_dmabuf(&dmabuf, None);
        if imported.is_ok() {
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.shell
    }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.size = Some(
                (
                    self.settings.source.width as i32,
                    self.settings.source.height as i32,
                )
                    .into(),
            );
        });
        surface.send_configure();
        let window = Window::new_wayland_window(surface);
        self.space.map_element(window.clone(), (0, 0), true);
        self.focus(&window);
        self.dirty = true;
    }
    fn new_popup(&mut self, surface: PopupSurface, _: PositionerState) {
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }
    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
        self.dirty = true;
    }
    fn grab(
        &mut self,
        _: PopupSurface,
        _: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        _: Serial,
    ) {
    }
    fn fullscreen_request(&mut self, surface: ToplevelSurface, _: Option<WlOutput>) {
        surface.with_pending_state(|state| {
            state.size = Some((self.settings.source.width as i32, self.settings.source.height as i32).into());
            state.states.set(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen);
        });
        surface.send_pending_configure();
    }
    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.unset(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen);
        });
        surface.send_pending_configure();
    }
}

impl SeatHandler for State {
    type KeyboardFocus = Focus;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    fn cursor_image(&mut self, _: &Seat<Self>, image: CursorImageStatus) {
        self.cursor = image;
    }
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&Focus>) {
        let client = focused
            .and_then(|f| f.wl_surface())
            .and_then(|s| self.display.get_client(s.id()).ok());
        set_data_device_focus(&self.display, seat, client);
    }
}

impl OutputHandler for State {}
impl PointerConstraintsHandler for State {}
impl SelectionHandler for State {
    type SelectionUserData = ();
}
impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device
    }
}
impl DndGrabHandler for State {}
impl WaylandDndGrabHandler for State {}

impl XWaylandShellHandler for State {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell
    }
    fn surface_associated(&mut self, _: XwmId, _: WlSurface, window: X11Surface) {
        if let Some(window) = self.x11_window(&window) {
            self.focus(&window);
        }
        self.dirty = true;
    }
}

impl State {
    fn x11_window(&self, surface: &X11Surface) -> Option<Window> {
        self.space
            .elements()
            .find(|w| w.x11_surface() == Some(surface))
            .cloned()
    }
    fn unmap_x11(&mut self, surface: &X11Surface) {
        if let Some(window) = self.x11_window(surface) {
            self.space.unmap_elem(&window);
        }
        self.dirty = true;
        if let Some(window) = self.primary() {
            self.focus(&window);
        }
    }
}

impl XwmHandler for State {
    fn xwm_state(&mut self, _: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().unwrap()
    }
    fn new_window(&mut self, _: XwmId, _: X11Surface) {}
    fn new_override_redirect_window(&mut self, _: XwmId, _: X11Surface) {}
    fn map_window_request(&mut self, _: XwmId, surface: X11Surface) {
        let geometry = surface.geometry();
        if let Err(error) = surface.set_mapped(true) {
            self.fail(error.into());
            return;
        }
        let window = Window::new_x11_window(surface.clone());
        self.space.map_element(window.clone(), geometry.loc, true);
        if geometry.size.w >= 64 && geometry.size.h >= 64 {
            self.focus(&window);
        }
        let _ = surface.configure(geometry);
        self.dirty = true;
    }
    fn mapped_override_redirect_window(&mut self, _: XwmId, surface: X11Surface) {
        let location = surface.geometry().loc;
        self.space
            .map_element(Window::new_x11_window(surface), location, true);
        self.dirty = true;
    }
    fn unmapped_window(&mut self, _: XwmId, surface: X11Surface) {
        self.unmap_x11(&surface);
        if !surface.is_override_redirect() {
            let _ = surface.set_mapped(false);
        }
    }
    fn destroyed_window(&mut self, _: XwmId, surface: X11Surface) {
        self.unmap_x11(&surface);
    }
    fn configure_request(
        &mut self,
        _: XwmId,
        surface: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        width: Option<u32>,
        height: Option<u32>,
        _: Option<Reorder>,
    ) {
        let mut geometry = surface.geometry();
        if let Some(x) = x {
            geometry.loc.x = x;
        }
        if let Some(y) = y {
            geometry.loc.y = y;
        }
        if let Some(w) = width {
            geometry.size.w = w as i32;
        }
        if let Some(h) = height {
            geometry.size.h = h as i32;
        }
        let _ = surface.configure(geometry);
    }
    fn configure_notify(
        &mut self,
        _: XwmId,
        surface: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _: Option<u32>,
    ) {
        if let Some(window) = self.x11_window(&surface) {
            self.space.map_element(window, geometry.loc, false);
        }
        self.dirty = true;
    }
    fn fullscreen_request(&mut self, _: XwmId, surface: X11Surface) {
        let _ = surface.set_fullscreen(true);
        let _ = surface.configure(Rectangle::from_size(
            (
                self.settings.source.width as i32,
                self.settings.source.height as i32,
            )
                .into(),
        ));
    }
    fn unfullscreen_request(&mut self, _: XwmId, surface: X11Surface) {
        let _ = surface.set_fullscreen(false);
    }
    fn maximize_request(&mut self, id: XwmId, surface: X11Surface) {
        XwmHandler::fullscreen_request(self, id, surface);
    }
    fn resize_request(&mut self, _: XwmId, _: X11Surface, _: u32, _: ResizeEdge) {}
    fn move_request(&mut self, _: XwmId, _: X11Surface, _: u32) {}
    fn active_window_request(
        &mut self,
        _: XwmId,
        surface: X11Surface,
        _: u32,
        _: Option<X11Surface>,
    ) {
        if let Some(window) = self.x11_window(&surface) {
            self.focus(&window);
        }
    }
}

smithay::delegate_dispatch2!(State);
