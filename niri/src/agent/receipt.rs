//! Waiting for the target window to settle after an agent action, and describing what changed.
//!
//! While waiting, the window gets frame callbacks at display rate even when it's off-screen, so
//! the client presents its response promptly instead of at the ~1 Hz fallback rate. Outside of
//! these short waits, agent windows are throttled like any other off-screen window.

use std::collections::HashSet;
use std::thread;
use std::time::{Duration, Instant};

use async_channel::Sender;
use niri_ipc::agent::{AgentFocus, AgentReceipt, AgentRect, AgentWindow};
use niri_ipc::{Reply, Response};
use smithay::desktop::{PopupKind, PopupManager};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource as _;
use smithay::utils::{IsAlive as _, Physical, Rectangle, Size, SERIAL_COUNTER};
use smithay::wayland::compositor::{get_role, with_states};
use smithay::wayland::shell::xdg::{
    PopupSurface, ShellClient, XdgPopupSurfaceData, XDG_POPUP_ROLE,
};
use smithay::wayland::shell::PingError;

use crate::niri::State;
use crate::utils::{get_monotonic_time, with_toplevel_role};
use crate::window::Mapped;

/// How often to send frame callbacks and check for settling while waiting.
const TICK: Duration = Duration::from_millis(16);

/// Size of the squares compared between the before and after images, in physical pixels.
const TILE: i32 = 8;

/// Beyond this many changed areas, report one area covering all of them.
const MAX_CHANGED: usize = 16;

/// When to stop waiting for the window.
struct Limits {
    /// Settled after this long without a new frame, once there was one.
    quiet: Duration,
    /// Settled after this long without any new frame: nothing happened.
    idle: Duration,
    /// Give up after this long.
    max: Duration,
}

/// After an input action: wait for the response to finish drawing.
const RECEIPT_LIMITS: Limits = Limits {
    quiet: Duration::from_millis(150),
    idle: Duration::from_millis(300),
    max: Duration::from_secs(2),
};

/// Longest wait for a client that's busy: it hasn't answered a ping since the action, or it's
/// setting up a window it just created. Building a dialog can block an app for over a second.
const BUSY_MAX: Duration = Duration::from_secs(5);

/// Pings sent this long after the action show that the client got past handling it: an app
/// answers pings while reading its events, before acting on the input read in the same batch.
const RESPONSIVE_AFTER: Duration = Duration::from_millis(50);

/// Before a capture: wait for one fresh frame, if the window has anything new to show.
const CAPTURE_LIMITS: Limits = Limits {
    quiet: Duration::ZERO,
    idle: Duration::from_millis(100),
    max: Duration::from_millis(100),
};

pub(super) enum Wait {
    Capture {
        reply: Sender<Reply>,
        max_size: Option<u32>,
    },
    Receipt {
        before: Option<Snapshot>,
        reply: Sender<Reply>,
    },
}

pub(super) struct Watch {
    id: u64,
    window_id: u64,
    client: ClientId,
    start: Instant,
    last_tick: Instant,
    last_commit: Option<Instant>,
    limits: &'static Limits,
    wait: Wait,
    /// The window's xdg_wm_base, for pinging the client.
    shell: ShellClient,
    /// When the ping awaiting its pong was sent.
    ping_sent: Option<Instant>,
    /// When the last answered ping was sent.
    answered: Option<Instant>,
    /// Toplevels of the client that were waiting for their first commit before the action.
    unmapped_before: HashSet<ObjectId>,
}

/// State of the target window and its surroundings, to compare against after an action.
pub(super) struct Snapshot {
    /// The window geometry rendered at the output scale.
    image: Option<(Size<i32, Physical>, Vec<u8>)>,
    scale: f64,
    popups: Vec<(ObjectId, AgentRect)>,
    windows: HashSet<u64>,
    title: Option<String>,
}

impl State {
    /// Waits for the window to settle, then replies with a capture or a receipt.
    pub(super) fn agent_wait(&mut self, window_id: u64, wait: Wait) -> Result<(), String> {
        let mapped = self.agent_mapped(window_id)?;
        let client = mapped
            .toplevel()
            .wl_surface()
            .client()
            .ok_or_else(|| format!("window {window_id} has no client"))?
            .id();
        let shell = mapped.toplevel().client();
        let unmapped_before = self.agent_unmapped_of(&client);

        let limits = match wait {
            Wait::Capture { .. } => &CAPTURE_LIMITS,
            Wait::Receipt { .. } => &RECEIPT_LIMITS,
        };

        let agent = self.agent();
        let id = agent.next_watch_id;
        agent.next_watch_id += 1;
        agent.watches.push(Watch {
            id,
            window_id,
            client,
            start: Instant::now(),
            last_tick: Instant::now(),
            last_commit: None,
            limits,
            wait,
            shell,
            ping_sent: None,
            answered: None,
            unmapped_before,
        });

        self.agent_send_frame(window_id);
        self.niri
            .event_loop
            .insert_source(Timer::from_duration(TICK), move |_, _, state| {
                state.agent_wait_tick(id)
            })
            .unwrap();

        Ok(())
    }

    /// Notes new frames of clients that agent actions are waiting for.
    pub fn agent_on_commit(&mut self, surface: &WlSurface) {
        let Some(agent) = &mut self.niri.agent else {
            return;
        };
        if agent.watches.is_empty() {
            return;
        }

        let Some(client) = surface.client() else {
            return;
        };
        let client = client.id();
        let now = Instant::now();
        for watch in &mut agent.watches {
            if watch.client == client {
                watch.last_commit = Some(now);
            }
        }
    }

    /// Notes that a client answered the ping of a watch.
    pub fn agent_on_pong(&mut self, shell: &ShellClient) {
        let Some(agent) = &mut self.niri.agent else {
            return;
        };
        for watch in &mut agent.watches {
            if &watch.shell == shell {
                if let Some(sent) = watch.ping_sent.take() {
                    watch.answered = Some(sent);
                }
            }
        }
    }

    /// Toplevels of a client that haven't had their first commit with a buffer yet.
    fn agent_unmapped_of(&self, client: &ClientId) -> HashSet<ObjectId> {
        self.niri
            .unmapped_windows
            .keys()
            .filter(|surface| surface.client().is_some_and(|c| &c.id() == client))
            .map(|surface| surface.id())
            .collect()
    }

    /// Whether the watched client may still be working on its response to the action.
    fn agent_watch_busy(&mut self, idx: usize, now: Instant) -> bool {
        let agent = self.agent();
        let watch = &mut agent.watches[idx];
        if !matches!(watch.wait, Wait::Receipt { .. }) {
            return false;
        }

        if watch.ping_sent.is_none() {
            match watch.shell.send_ping(SERIAL_COUNTER.next_serial()) {
                Ok(()) | Err(PingError::PingAlreadyPending(_)) => watch.ping_sent = Some(now),
                // Nothing to wait for.
                Err(PingError::DeadSurface) => watch.answered = Some(now),
            }
        }
        let responsive = watch
            .answered
            .is_some_and(|sent| sent >= watch.start + RESPONSIVE_AFTER);

        let client = watch.client.clone();
        let before = watch.unmapped_before.clone();
        let opening = self
            .agent_unmapped_of(&client)
            .iter()
            .any(|surface| !before.contains(surface));

        !responsive || opening
    }

    fn agent_wait_tick(&mut self, id: u64) -> TimeoutAction {
        let now = Instant::now();
        let Some(idx) = self.agent().watches.iter().position(|w| w.id == id) else {
            return TimeoutAction::Drop;
        };
        let busy = self.agent_watch_busy(idx, now);

        let agent = self.agent();
        let Some(idx) = agent.watches.iter().position(|w| w.id == id) else {
            return TimeoutAction::Drop;
        };

        let watch = &mut agent.watches[idx];
        let elapsed = now - watch.start;
        let limits = watch.limits;

        // If niri itself was busy for a while, the client had no chance to respond yet: its
        // input and frame callbacks were waiting in the same event loop.
        let stalled = now - watch.last_tick > TICK * 4;
        watch.last_tick = now;

        let quiet = match watch.last_commit {
            Some(last) => now - last >= limits.quiet,
            None => elapsed >= limits.idle,
        };
        let settled = !stalled && !busy && quiet;
        // A client that keeps drawing ends the wait at the time limit; one that's busy gets
        // longer, as it's about to show something.
        let max = if busy { BUSY_MAX } else { limits.max };
        if !settled && elapsed < max {
            let window_id = watch.window_id;
            self.agent_send_frame(window_id);
            return TimeoutAction::ToDuration(TICK);
        }

        let watch = agent.watches.swap_remove(idx);
        match watch.wait {
            Wait::Capture { reply, max_size } => {
                if let Err(err) = self.agent_capture(watch.window_id, max_size, reply.clone()) {
                    let _ = reply.try_send(Err(err));
                }
            }
            Wait::Receipt { before, reply } => {
                let (mut receipt, images) =
                    self.agent_receipt(watch.window_id, before, settled, elapsed);
                match images {
                    // Comparing the images takes a few milliseconds on big windows; keep it off
                    // the compositor's thread.
                    Some(images) => {
                        thread::spawn(move || {
                            receipt.changed = images.changed();
                            let _ = reply.send_blocking(Ok(Response::AgentReceipt(receipt)));
                        });
                    }
                    None => {
                        let _ = reply.try_send(Ok(Response::AgentReceipt(receipt)));
                    }
                }
            }
        }

        TimeoutAction::Drop
    }

    /// Sends frame callbacks to the window even if it's off-screen.
    fn agent_send_frame(&mut self, window_id: u64) {
        let time = get_monotonic_time();
        let fallback = self.agent().frame_output.clone();
        self.niri.layout.with_windows_mut(|mapped, output| {
            if mapped.id().get() != window_id {
                return;
            }

            let output = output.unwrap_or(&fallback);
            // Throttle a bit under the tick so that callbacks the window already got from its
            // output this refresh cycle don't get doubled.
            let throttle = Some(TICK - Duration::from_millis(1));
            mapped.send_frame(output, time, throttle, |_, _| Some(output.clone()));
        });
    }

    pub(super) fn agent_snapshot(&mut self, window_id: u64) -> Option<Snapshot> {
        let image = self
            .agent_render(window_id, true, None)
            .ok()
            .map(|rendered| (rendered.area.size, rendered.pixels));

        let mapped = self.agent_mapped(window_id).ok()?;
        Some(Snapshot {
            image,
            scale: self.agent_window_scale(window_id),
            popups: popups(mapped),
            windows: self.agent_window_ids(),
            title: with_toplevel_role(mapped.toplevel(), |role| role.title.clone()),
        })
    }

    fn agent_receipt(
        &mut self,
        window_id: u64,
        before: Option<Snapshot>,
        settled: bool,
        elapsed: Duration,
    ) -> (AgentReceipt, Option<ImagePair>) {
        let mut receipt = AgentReceipt {
            settled,
            elapsed_ms: elapsed.as_millis() as u64,
            changed: Vec::new(),
            resized: None,
            popups_opened: Vec::new(),
            popups_closed: Vec::new(),
            windows_opened: Vec::new(),
            windows_closed: Vec::new(),
            title: None,
            focus: self.agent_focus_kind(window_id),
        };

        let Some(before) = before else {
            return (receipt, None);
        };

        let windows = self.agent_window_ids();
        let mut closed: Vec<u64> = before.windows.difference(&windows).copied().collect();
        closed.sort_unstable();
        receipt.windows_closed = closed;
        let mut opened: Vec<u64> = windows.difference(&before.windows).copied().collect();
        opened.sort_unstable();
        receipt.windows_opened = opened
            .into_iter()
            .filter_map(|id| self.agent_window_info(id))
            .collect();

        let Some(mut after) = self.agent_snapshot(window_id) else {
            return (receipt, None);
        };

        let mut images = None;
        if let (Some((size_before, before_pixels)), Some((size, after_pixels))) =
            (before.image, after.image.take())
        {
            if size_before == size && before.scale == after.scale {
                images = Some(ImagePair {
                    size,
                    scale: after.scale,
                    before: before_pixels,
                    after: after_pixels,
                });
            } else {
                let whole = to_logical(Rectangle::from_size(size), after.scale);
                receipt.resized = Some((whole.w, whole.h));
                receipt.changed = vec![whole];
            }
        }

        receipt.popups_opened = after
            .popups
            .iter()
            .filter(|(id, _)| !before.popups.iter().any(|(b, _)| b == id))
            .map(|(_, rect)| *rect)
            .collect();
        receipt.popups_closed = before
            .popups
            .iter()
            .filter(|(id, _)| !after.popups.iter().any(|(a, _)| a == id))
            .map(|(_, rect)| *rect)
            .collect();

        if after.title != before.title {
            receipt.title = after.title;
        }

        (receipt, images)
    }

    pub(super) fn agent_mapped(&self, window_id: u64) -> Result<&Mapped, String> {
        self.niri
            .layout
            .windows()
            .find(|(_, mapped)| mapped.id().get() == window_id)
            .map(|(_, mapped)| mapped)
            .ok_or_else(|| format!("window {window_id} was not found"))
    }

    fn agent_window_ids(&self) -> HashSet<u64> {
        let windows = self.niri.layout.windows();
        windows.map(|(_, mapped)| mapped.id().get()).collect()
    }

    pub(super) fn agent_window_info(&self, window_id: u64) -> Option<AgentWindow> {
        let mapped = self.agent_mapped(window_id).ok()?;
        let toplevel = mapped.toplevel();
        let (app_id, title) =
            with_toplevel_role(toplevel, |role| (role.app_id.clone(), role.title.clone()));
        let parent_id = toplevel.parent().and_then(|parent| {
            let (parent, _) = self.niri.layout.find_window_and_output(&parent)?;
            Some(parent.id().get())
        });

        Some(AgentWindow {
            id: window_id,
            app_id,
            title,
            parent_id,
        })
    }

    fn agent_focus_kind(&mut self, window_id: u64) -> AgentFocus {
        let Ok(mapped) = self.agent_mapped(window_id) else {
            return AgentFocus::Other;
        };
        let toplevel = mapped.toplevel().wl_surface().clone();

        // A popup grab moves the focus to the popup only on the next key.
        if self.agent_popup_grab_on(&toplevel) {
            return AgentFocus::Popup;
        }

        let keyboard = self.agent().seat.get_keyboard().unwrap();
        let Some(mut focus) = keyboard.current_focus() else {
            return AgentFocus::None;
        };
        // The focus stays on a popup that its client closed, for example with Escape.
        if !focus.alive() {
            keyboard.set_focus(self, Some(toplevel.clone()), SERIAL_COUNTER.next_serial());
            focus = toplevel.clone();
        }

        if focus == toplevel {
            AgentFocus::Window
        } else if PopupManager::popups_for_surface(&toplevel).any(|(p, _)| p.wl_surface() == &focus)
        {
            AgentFocus::Popup
        } else {
            AgentFocus::Other
        }
    }
}

/// Top-left corners of the surfaces of the window's xdg popups in window coordinates, outermost
/// first.
pub(super) fn popup_surfaces(mapped: &Mapped) -> Vec<(f64, f64)> {
    // Popup offsets are positions of their geometry relative to the window geometry.
    let mut popups: Vec<_> = PopupManager::popups_for_surface(mapped.toplevel().wl_surface())
        .filter_map(|(popup, offset)| match popup {
            PopupKind::Xdg(ref xdg) => {
                let origin = offset - popup.geometry().loc;
                Some((
                    popup_depth(&xdg),
                    (f64::from(origin.x), f64::from(origin.y)),
                ))
            }
            _ => None,
        })
        .collect();
    popups.sort_by_key(|(depth, _)| *depth);
    popups.into_iter().map(|(_, origin)| origin).collect()
}

/// How many popups an xdg popup is nested in.
fn popup_depth(popup: &PopupSurface) -> usize {
    let mut depth = 0;
    let mut parent = popup.get_parent_surface();
    while let Some(surface) = parent {
        if get_role(&surface) != Some(XDG_POPUP_ROLE) {
            break;
        }
        depth += 1;
        parent = with_states(&surface, |states| {
            states
                .data_map
                .get::<XdgPopupSurfaceData>()
                .and_then(|data| data.lock().unwrap().parent.clone())
        });
    }
    depth
}

/// The window's xdg popups, in window coordinates.
fn popups(mapped: &Mapped) -> Vec<(ObjectId, AgentRect)> {
    PopupManager::popups_for_surface(mapped.toplevel().wl_surface())
        .filter(|(popup, _)| matches!(popup, PopupKind::Xdg(_)))
        .map(|(popup, offset)| {
            let size = popup.geometry().size;
            let rect = AgentRect {
                x: f64::from(offset.x),
                y: f64::from(offset.y),
                w: f64::from(size.w),
                h: f64::from(size.h),
            };
            (popup.wl_surface().id(), rect)
        })
        .collect()
}

/// The window before and after an action, rendered at the same size.
struct ImagePair {
    size: Size<i32, Physical>,
    scale: f64,
    before: Vec<u8>,
    after: Vec<u8>,
}

impl ImagePair {
    /// Changed areas in window coordinates.
    fn changed(&self) -> Vec<AgentRect> {
        changed_rects(&self.before, &self.after, self.size)
            .into_iter()
            .map(|rect| to_logical(rect, self.scale))
            .collect()
    }
}

fn to_logical(rect: Rectangle<i32, Physical>, scale: f64) -> AgentRect {
    AgentRect {
        x: f64::from(rect.loc.x) / scale,
        y: f64::from(rect.loc.y) / scale,
        w: f64::from(rect.size.w) / scale,
        h: f64::from(rect.size.h) / scale,
    }
}

/// Areas that differ between two RGBA images of the same size.
///
/// Compares squares of `TILE` pixels and merges touching changed squares into rectangles.
fn changed_rects(
    before: &[u8],
    after: &[u8],
    size: Size<i32, Physical>,
) -> Vec<Rectangle<i32, Physical>> {
    let (w, h) = (size.w.max(0) as usize, size.h.max(0) as usize);
    let tile = TILE as usize;
    if before.len() != w * h * 4 || after.len() != before.len() {
        return vec![Rectangle::from_size(size)];
    }

    let cols = w.div_ceil(tile);
    let rows = h.div_ceil(tile);
    let mut changed = vec![false; cols * rows];
    for y in 0..h {
        let row = y * w * 4;
        for col in 0..cols {
            let idx = (y / tile) * cols + col;
            if changed[idx] {
                continue;
            }
            let start = row + col * tile * 4;
            let end = row + ((col + 1) * tile).min(w) * 4;
            if before[start..end] != after[start..end] {
                changed[idx] = true;
            }
        }
    }

    // Group touching squares (including diagonally) into bounding boxes.
    let mut seen = vec![false; changed.len()];
    let mut rects = Vec::new();
    for start in 0..changed.len() {
        if !changed[start] || seen[start] {
            continue;
        }

        let (mut min_c, mut min_r, mut max_c, mut max_r) = (cols, rows, 0, 0);
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(idx) = stack.pop() {
            let (c, r) = (idx % cols, idx / cols);
            (min_c, min_r) = (min_c.min(c), min_r.min(r));
            (max_c, max_r) = (max_c.max(c), max_r.max(r));

            for nr in r.saturating_sub(1)..=(r + 1).min(rows - 1) {
                for nc in c.saturating_sub(1)..=(c + 1).min(cols - 1) {
                    let n = nr * cols + nc;
                    if changed[n] && !seen[n] {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }

        let x = min_c * tile;
        let y = min_r * tile;
        let right = ((max_c + 1) * tile).min(w);
        let bottom = ((max_r + 1) * tile).min(h);
        rects.push(Rectangle::new(
            (x as i32, y as i32).into(),
            ((right - x) as i32, (bottom - y) as i32).into(),
        ));
    }

    if rects.len() > MAX_CHANGED {
        let merged = rects.iter().copied().reduce(|a, b| a.merge(b)).unwrap();
        rects = vec![merged];
    }

    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: i32, h: i32) -> Vec<u8> {
        vec![0; (w * h * 4) as usize]
    }

    fn set(img: &mut [u8], w: i32, x: i32, y: i32) {
        let i = ((y * w + x) * 4) as usize;
        img[i] = 255;
    }

    #[test]
    fn unchanged_image_has_no_changes() {
        let a = image(20, 10);
        assert!(changed_rects(&a, &a.clone(), Size::from((20, 10))).is_empty());
    }

    #[test]
    fn changes_group_by_tile() {
        let (w, h) = (40, 20);
        let a = image(w, h);
        let mut b = a.clone();
        // Two pixels in touching tiles, one far away.
        set(&mut b, w, 3, 3);
        set(&mut b, w, 9, 9);
        set(&mut b, w, 35, 2);

        let rects = changed_rects(&a, &b, Size::from((w, h)));
        assert_eq!(
            rects,
            [
                Rectangle::new((0, 0).into(), (16, 16).into()),
                Rectangle::new((32, 0).into(), (8, 8).into()),
            ]
        );
    }

    #[test]
    fn partial_tiles_are_clipped() {
        let (w, h) = (10, 10);
        let a = image(w, h);
        let mut b = a.clone();
        set(&mut b, w, 9, 9);

        let rects = changed_rects(&a, &b, Size::from((w, h)));
        assert_eq!(rects, [Rectangle::new((8, 8).into(), (2, 2).into())]);
    }
}
