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

use std::{
    collections::{HashMap, VecDeque},
    sync::{LazyLock, RwLock},
};

use niri_ipc::{Window, Workspace};

use crate::cmds::FollowPolicy;

pub struct State {
    pub all_windows: VecDeque<Window>,
    pub all_workspaces: Vec<Workspace>,
    /// The windows in follow-mode together with their policy.
    pub follow_mode_wins: HashMap<u64, FollowPolicy>,
    pub scratchpad_win_ids: Vec<u64>,
    pub mark_to_win_ids: HashMap<String, Vec<u64>>,
}

impl State {
    pub fn get_focused_win_id(&self) -> Option<u64> {
        self.all_windows.iter().find(|w| w.is_focused).map(|w| w.id)
    }

    /// Returns whether the currently focused window is a scratchpad window.
    pub fn focused_win_is_scratchpad_window(&self) -> bool {
        self.get_focused_win_id()
            .is_some_and(|id| self.scratchpad_win_ids.contains(&id))
    }

    pub fn get_last_focused_matching<F>(&self, predicate: F) -> Option<u64>
    where
        F: Fn(&Window) -> bool,
    {
        self.all_windows
            .iter()
            .rev()
            .find(|w| predicate(w))
            .map(|w| w.id)
    }

    pub fn window_opened_or_changed(
        &mut self,
        win: Window,
    ) -> Result<String, String> {
        let id = win.id;

        // If it's not floating, then it's no scratchpad window anymore.
        if !win.is_floating {
            self.scratchpad_win_ids.retain(|w| *w != id);
        }

        // A window can become focused via a WindowOpenedOrChanged event (which
        // routes here) instead of WindowFocusChanged, e.g. when it is shown on
        // an otherwise empty workspace or when it opens on another output.  So
        // take the focus away from the other windows here, no matter whether
        // this window is a new one or just an updated one.
        let is_focused = win.is_focused;
        if is_focused {
            self.all_windows
                .iter_mut()
                .for_each(|w| w.is_focused = false);
        }

        if let Some(idx) = self.all_windows.iter().position(|w| w.id == id) {
            self.all_windows[idx] = win;

            // Keep the focus-history ordering correct by moving a newly
            // focused window to the back, just like window_focus_changed does.
            if is_focused {
                self.move_window_to_back(id);
            }

            Ok(format!("Updated window {id}."))
        } else {
            // Register a new window.  push_back() already puts it where the
            // most recently focused window belongs.
            self.all_windows.push_back(win);
            Ok(format!(
                "Registered window {}. Currently managing {} windows.",
                id,
                self.all_windows.len()
            ))
        }
    }

    pub fn window_closed(&mut self, id: &u64) -> Result<String, String> {
        self.all_windows.retain(|w| w.id != *id);
        self.follow_mode_wins.remove(id);
        self.scratchpad_win_ids.retain(|i| i != id);
        for v in self.mark_to_win_ids.values_mut() {
            v.retain(|i| i != id);
        }
        Ok(format!(
            "Removed window with id {id}. Currently managing {} windows.",
            self.all_windows.len()
        ))
    }

    pub fn window_urgency_changed(
        &mut self,
        id: u64,
        urgent: bool,
    ) -> Result<String, String> {
        if let Some(win) = self.all_windows.iter_mut().find(|w| w.id == id) {
            win.is_urgent = urgent;
            Ok(format!("Set urgency of window {id} to {urgent}."))
        } else {
            Err(format!("No window with id {id} to update urgency."))
        }
    }

    /// Moves the window with the given id to the back of `all_windows`, i.e.,
    /// marks it as the most recently focused one.  Returns whether a window
    /// with that id existed.
    pub fn move_window_to_back(&mut self, id: u64) -> bool {
        if let Some(idx) = self.all_windows.iter().position(|w| w.id == id)
            && let Some(win) = self.all_windows.remove(idx)
        {
            self.all_windows.push_back(win);
            true
        } else {
            false
        }
    }

    pub fn window_focus_changed(
        &mut self,
        opt_id: Option<u64>,
    ) -> Result<String, String> {
        if let Some(id) = opt_id {
            for win in self.all_windows.iter_mut() {
                win.is_focused = win.id == id;
            }
            if self.move_window_to_back(id) {
                Ok(format!("Updated focus to window {id}."))
            } else {
                Ok("Updated focus (no window is focused).".to_string())
            }
        } else {
            self.all_windows
                .iter_mut()
                .for_each(|w| w.is_focused = false);
            Ok("No window has focus anymore.".to_owned())
        }
    }

    pub fn workspaces_changed(
        &mut self,
        workspaces: Vec<Workspace>,
    ) -> Result<String, String> {
        self.all_workspaces = workspaces;
        Ok("Updated all workspaces.".to_owned())
    }

    pub fn workspace_focused(&mut self, id: u64) {
        for ws in &mut self.all_workspaces {
            ws.is_focused = ws.id == id;
        }
    }

    /// Makes the workspace with the given id the active one of its output,
    /// deactivating the other workspaces of that very output.  The active
    /// workspaces of the other outputs are left alone since every output has
    /// its own active workspace.
    pub fn workspace_activated(&mut self, id: u64) {
        let Some(output) = self
            .all_workspaces
            .iter()
            .find(|ws| ws.id == id)
            .and_then(|ws| ws.output.clone())
        else {
            return;
        };

        for ws in &mut self.all_workspaces {
            if ws.output.as_ref().is_some_and(|o| *o == output) {
                ws.is_active = ws.id == id;
            }
        }
    }

    /// Returns whether the workspace with the given id is currently visible,
    /// i.e., it is the active workspace of its output.
    pub fn is_workspace_visible(&self, id: u64) -> bool {
        self.all_workspaces
            .iter()
            .any(|ws| ws.id == id && ws.is_active)
    }

    /// Returns the ids of all follow-mode windows which have to be moved to
    /// the focused workspace now.
    ///
    /// A window with the `always` policy has to move whenever it isn't on the
    /// focused workspace, of which there is only one.  A window with the
    /// `if-invisible` policy may stay where it is for as long as it remains
    /// visible, i.e., as long as its workspace is the active one of its
    /// output.
    pub fn follow_mode_wins_to_move(&self, focused_ws_id: u64) -> Vec<u64> {
        self.follow_mode_wins
            .iter()
            .filter(|(id, policy)| {
                let win_ws_id = self
                    .all_windows
                    .iter()
                    .find(|w| w.id == **id)
                    .and_then(|w| w.workspace_id);

                // Nothing to do if it's already where it should go.
                if win_ws_id == Some(focused_ws_id) {
                    return false;
                }

                match policy {
                    FollowPolicy::Always => true,
                    FollowPolicy::IfInvisible => !win_ws_id
                        .is_some_and(|ws_id| self.is_workspace_visible(ws_id)),
                }
            })
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn get_focused_workspace(&self) -> Option<&Workspace> {
        self.all_workspaces.iter().find(|ws| ws.is_focused)
    }

    /// Gets the id of the focused workspace or returns a descriptive error
    /// when there is no focused workspace.
    pub fn focused_workspace_id_or_err(&self) -> Result<u64, String> {
        self.get_focused_workspace()
            .map(|ws| ws.id)
            .ok_or_else(|| "No focused workspace.".to_owned())
    }

    pub fn get_bottom_workspace_id_and_idx_of_output(
        &self,
        output: &str,
    ) -> Option<(u64, u8)> {
        self.all_workspaces
            .iter()
            .filter(|ws| ws.output.as_ref().is_some_and(|o| o == output))
            .max_by(|a, b| a.idx.cmp(&b.idx))
            .map(|ws| (ws.id, ws.idx))
    }

    pub fn is_bottom_workspace_focused(&self) -> bool {
        if let Some(ws) = self.get_focused_workspace() {
            let (_, ws_idx) = self
                .get_bottom_workspace_id_and_idx_of_output(
                    ws.output.as_ref().expect("Workspace without output."),
                )
                .expect("No bottom but a focused workspace.");
            // It's the bottom workspace if the max index of all workspaces on
            // the same output is this workspace's index + 1 because there is
            // always one empty workspace at the bottom.
            ws.idx + 1 == ws_idx
        } else {
            false
        }
    }

    /// Returns whether the focused workspace is a scratchpad workspace, i.e.,
    /// the bottom-most non-empty workspace of its output which holds nothing
    /// but scratchpad windows.
    ///
    /// A workspace where a scratchpad window has merely been shown next to
    /// other windows doesn't qualify, so focusing it won't hide the scratchpad
    /// windows again.
    pub fn is_scratchpad_workspace_focused(&self) -> bool {
        // Scratchpad windows always rest at the very bottom, so any workspace
        // above the bottom-most non-empty one is out of the question.
        if !self.is_bottom_workspace_focused() {
            return false;
        }

        // No focused workspace at all, hence none which is the scratchpad one.
        let Some(ws_id) = self.get_focused_workspace().map(|ws| ws.id) else {
            return false;
        };

        let mut wins_on_ws = self
            .all_windows
            .iter()
            .filter(|w| w.workspace_id == Some(ws_id))
            .peekable();

        // An empty workspace is no scratchpad workspace, and one holding some
        // window which isn't a scratchpad window is a regular workspace the
        // user works on, no matter if scratchpad windows are shown there, too.
        // The peek() is required because all() is vacuously true for an empty
        // iterator; peeking doesn't consume, so all() still sees every window.
        wins_on_ws.peek().is_some()
            && wins_on_ws.all(|w| self.scratchpad_win_ids.contains(&w.id))
    }
}

pub static STATE: LazyLock<RwLock<State>> = LazyLock::new(|| {
    RwLock::new(State {
        all_windows: VecDeque::new(),
        all_workspaces: Vec::new(),
        follow_mode_wins: HashMap::new(),
        scratchpad_win_ids: vec![],
        mark_to_win_ids: HashMap::new(),
    })
});
