use niri_ipc::WindowFollowMode;

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
        if let Some(tile) = self.follow_tile_mut(id) {
            tile.follow_mode = mode;
        }
    }

    pub fn toggle_window_follow(&mut self, id: Option<&W::Id>) {
        if let Some(tile) = self.follow_tile_mut(id) {
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

    fn follow_tile_mut(&mut self, id: Option<&W::Id>) -> Option<&mut Tile<W>> {
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
}
