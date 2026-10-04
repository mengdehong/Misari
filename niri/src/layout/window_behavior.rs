//! Window pinning and workspace-follow behavior.

use niri_ipc::{PinMode, PinWhenTiled, WindowFollowMode};

use super::workspace::WorkspaceId;
use super::{ActivateWindow, InteractiveMoveState, Layout, LayoutElement, MonitorSet, Tile};

/// Only activation state is remembered; windows and their placement live in the layout.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct FollowContext {
    focused: Option<WorkspaceId>,
    active: Vec<WorkspaceId>,
}

impl<W: LayoutElement> Layout<W> {
    pub fn set_window_follow(&mut self, id: Option<&W::Id>, mode: WindowFollowMode) {
        if let Some(tile) = self.find_tile_mut(id) {
            tile.follow_mode = mode;
        }
    }

    pub fn toggle_window_follow(&mut self, id: Option<&W::Id>) {
        if let Some(tile) = self.find_tile_mut(id) {
            tile.follow_mode = if tile.follow_mode == WindowFollowMode::Off {
                WindowFollowMode::Always
            } else {
                WindowFollowMode::Off
            };
        }
    }

    /// Follow committed workspace activation changes before updating keyboard focus.
    /// Returns whether any windows moved and the outputs need redrawing.
    pub fn refresh_follow_windows(&mut self) -> bool {
        // Do not interfere with a pointer move. The unchanged context makes us retry after it ends.
        if self.interactive_move.is_some() {
            return false;
        }

        let context = self.current_follow_context();
        if context == self.follow_context {
            return false;
        }
        let Some(target) = context.focused else {
            self.follow_context = context;
            return false;
        };

        let mut windows = Vec::new();
        for (_, _, ws) in self.workspaces() {
            if ws.id() == target {
                continue;
            }
            for tile in ws.tiles() {
                let should_move = match tile.follow_mode {
                    WindowFollowMode::Off => false,
                    WindowFollowMode::Always => true,
                    WindowFollowMode::IfInvisible => !context.active.contains(&ws.id()),
                };
                if should_move {
                    windows.push(tile.window().id().clone());
                }
            }
        }

        for window in &windows {
            self.move_to_workspace_by_id(window, target, ActivateWindow::No);
        }

        // Moving the last tile can clean up workspaces. Remember the resulting state so our own
        // changes do not trigger another round of following.
        self.follow_context = self.current_follow_context();
        !windows.is_empty()
    }

    pub fn toggle_window_pinned(&mut self, id: Option<&W::Id>, when_tiled: PinWhenTiled) -> bool {
        let Some(tile) = self.find_tile_mut(id) else {
            return false;
        };
        let id = tile.window().id().clone();
        let pinned = tile.is_pinned();
        let floating = self.is_window_floating(&id);
        // With float, a dormant preference still means the first trigger should float and pin.
        let enabled = !(pinned && (floating || when_tiled != PinWhenTiled::Float));
        let mode = if enabled { PinMode::On } else { PinMode::Off };
        let changed = self.set_window_pinned(Some(&id), mode, when_tiled);
        if !enabled && when_tiled == PinWhenTiled::Float {
            self.set_window_floating(Some(&id), false);
        }
        changed
    }

    pub fn set_window_pinned(
        &mut self,
        id: Option<&W::Id>,
        mode: PinMode,
        when_tiled: PinWhenTiled,
    ) -> bool {
        let Some(tile) = self.find_tile_mut(id) else {
            return false;
        };
        let id = tile.window().id().clone();
        let preference = match mode {
            PinMode::On => Some(true),
            PinMode::Off => Some(false),
            PinMode::Auto => None,
        };
        let enabled = preference.unwrap_or(tile.window().rules().pinned.unwrap_or(false));
        let mut changed = tile.pinned_override != preference;
        let floating = self.is_window_floating(&id);

        // Auto only clears the override; it must also work for a dormant tiled preference.
        if !floating && mode != PinMode::Auto {
            match when_tiled {
                PinWhenTiled::Ignore => return false,
                PinWhenTiled::Remember => (),
                PinWhenTiled::Float if enabled => {
                    self.set_window_floating(Some(&id), true);
                    changed = true;
                }
                PinWhenTiled::Float => (),
            }
        }
        let Some(tile) = self.find_tile_mut(Some(&id)) else {
            return false;
        };
        tile.pinned_override = preference;
        changed
    }

    fn current_follow_context(&self) -> FollowContext {
        let MonitorSet::Normal { monitors, .. } = &self.monitor_set else {
            return FollowContext::default();
        };

        let mut active: Vec<_> = monitors
            .iter()
            .map(|mon| mon.active_workspace_ref().id())
            .collect();
        active.sort_unstable_by_key(|id| id.get());
        FollowContext {
            focused: self.active_workspace().map(|ws| ws.id()),
            active,
        }
    }

    fn find_tile_mut(&mut self, id: Option<&W::Id>) -> Option<&mut Tile<W>> {
        let id = id
            .cloned()
            .or_else(|| self.focus().map(|win| win.id().clone()))?;
        if self
            .interactive_move
            .as_ref()
            .and_then(|state| state.moving())
            .is_some_and(|move_| *move_.tile.window().id() == id)
        {
            let Some(InteractiveMoveState::Moving(move_)) = &mut self.interactive_move else {
                unreachable!()
            };
            return Some(&mut move_.tile);
        }

        self.workspaces_mut()
            .flat_map(|ws| ws.tiles_mut())
            .find(|tile| *tile.window().id() == id)
    }

    fn is_window_floating(&self, id: &W::Id) -> bool {
        if let Some(move_) = self
            .interactive_move
            .as_ref()
            .and_then(|state| state.moving())
        {
            if move_.tile.window().id() == id {
                return move_.is_floating;
            }
        }
        self.workspaces().any(|(_, _, ws)| ws.is_floating(id))
    }
}
