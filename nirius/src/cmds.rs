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

use crate::{
    ipc,
    state::{STATE, State},
};
use niri_ipc::{
    Action, Request, Response, Window, Workspace, WorkspaceReferenceArg,
};
use regex::Regex;
use serde::{Deserialize, Serialize};

static NO_MATCHING_WINDOW: &str = "No matching window.";

#[derive(clap::Parser, PartialEq, Eq, Debug, Clone, Deserialize, Serialize)]
pub enum NiriusCmd {
    /// Focus the window matching the given options.  If there is more than one
    /// matching window, cycle through them.  If there is none, exit non-zero.
    Focus {
        #[clap(flatten)]
        match_opts: MatchOptions,
    },
    /// Focus the window matching the given options.  If there is more than one
    /// matching window, cycle through them.  If there is none, spawn the given
    /// COMMAND instead.
    FocusOrSpawn {
        #[clap(flatten)]
        match_opts: MatchOptions,
        /// The command to execute if no window matches.
        command: Vec<String>,
    },
    /// Move a window matching the given options to the current workspace.
    /// Only windows of unfocused workspaces are considered unless the
    /// `--include-current-workspace` flag is given.  If there is no such
    /// window, exit non-zero.
    MoveToCurrentWorkspace {
        #[clap(flatten)]
        match_opts: MatchOptions,

        #[clap(
            short = 'f',
            long,
            help = "Focus the window after moving it to the current workspace."
        )]
        focus: bool,

        #[clap(long, help = "Don't exclude windows of the current workspace.")]
        include_current_workspace: bool,
    },
    /// Move a window matching the given options to the current workspace.
    /// Only windows of unfocused workspaces are considered unless the
    /// `--include-current-workspace` flag is given.  If there is no such
    /// window, spawn the given command.
    MoveToCurrentWorkspaceOrSpawn {
        #[clap(flatten)]
        match_opts: MatchOptions,

        #[clap(
            short = 'f',
            long,
            help = "Focus the window after moving it to the current workspace."
        )]
        focus: bool,

        #[clap(long, help = "Don't exclude windows of the current workspace.")]
        include_current_workspace: bool,

        /// The command to execute if no window matches.
        command: Vec<String>,
    },
    /// Enables or disables follow-mode for the currently focused window.  A
    /// window in follow-mode moves automatically to the workspace you are
    /// working on, either whenever another workspace receives focus or only
    /// once it would become invisible, see `--policy`.
    ///
    /// If the window already is in follow-mode with a different policy than
    /// the given one, its policy is switched instead of disabling follow-mode.
    ToggleFollowMode {
        #[clap(
            long,
            value_enum,
            help = "When a follow-mode window is moved (default: always)"
        )]
        policy: Option<FollowPolicy>,
    },
    /// Marks or unmarks the currently focused window with the given or default
    /// mark.  You can switch to the marked window or cycle trough all marked
    /// windows using the `focus-marked` command.
    ToggleMark { mark: Option<String> },
    /// Focuses the window with the given mark or the default mark, if no mark
    /// is given.  If there are multiple marked windows, cycles through all of
    /// them.  To mark a window, use the `toggle-mark` command.
    FocusMarked { mark: Option<String> },
    /// List all windows with the given or default mark, if no mark is given,
    /// on stdout.
    ListMarked {
        mark: Option<String>,
        #[clap(short = 'a', long, help = "List all marks with their windows")]
        all: bool,
    },
    /// Toggles the scratchpad state of the current window or a window matching
    /// the given window matching options, see `--help`.
    ///
    /// If it's no scratchpad window currently, makes it foating (if it's not
    /// already) and moves it to the scratchpad workspace (the bottom-most
    /// workspace) unless the `--no-move` flag is specified.
    ///
    /// If it's already a scratchpad window, removes it from there, i.e., from
    /// then on, it's just a normal window.
    ScratchpadToggle {
        #[clap(flatten)]
        match_opts: MatchOptions,
        #[clap(
            long,
            help = "Toggle scratchpad state without moving the window"
        )]
        no_move: bool,
    },
    /// Shows a window from the scratchpad or moves it back to the scratchpad
    /// if the current window is a scratchpad window.  Repeated invocations
    /// cycle through all scratchpad windows.  The scratch window shown can
    /// optionally be further specified using window matching options, see
    /// `--help`.
    ScratchpadShow {
        #[clap(flatten)]
        match_opts: MatchOptions,

        /// Show the scratchpad window with this exact window id.  Errors if
        /// the window is not a scratchpad window.
        #[clap(long)]
        id: Option<u64>,
    },

    /// Shows all windows in scratchpad or moves back all windows to scratchpad
    /// if current window is a scratchpad window.
    ScratchpadShowAll,

    /// Lists all scratchpad windows.
    ListScratchpad,
}

/// Determines when a window in follow-mode is moved to the workspace the user
/// is working on.
#[derive(
    clap::ValueEnum,
    PartialEq,
    Eq,
    Debug,
    Clone,
    Copy,
    Default,
    Deserialize,
    Serialize,
)]
pub enum FollowPolicy {
    /// Move the window whenever another workspace receives focus.
    #[default]
    Always,
    /// Move the window only when it would become invisible, i.e., when the
    /// workspace it is on is no longer the active one of its output.  With
    /// just one output, this is equivalent to `always`.
    IfInvisible,
}

impl std::fmt::Display for FollowPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            FollowPolicy::Always => "always",
            FollowPolicy::IfInvisible => "if-invisible",
        })
    }
}

#[derive(
    clap::Parser, PartialEq, Eq, Debug, Clone, Default, Deserialize, Serialize,
)]
pub struct MatchOptions {
    #[clap(
        short = 'a',
        long,
        help = "Selects windows whose app-id matches this regex"
    )]
    app_id: Option<String>,

    #[clap(
        short = 't',
        long,
        help = "Selects windows whose title matches this regex"
    )]
    title: Option<String>,

    #[clap(
        short = 'p',
        long,
        help = "Selects windows belonging to the process with this PID"
    )]
    pid: Option<i32>,

    #[clap(long, help = "Selects windows on the currently focused workspace")]
    focused_workspace: bool,

    #[clap(
        long,
        help = "Selects windows on a currently active workspace (one per output)"
    )]
    active_workspace: bool,

    #[clap(long, help = "Selects windows shown on the workspace with this ID")]
    workspace_id: Option<u64>,

    #[clap(
        long,
        help = "Selects windows shown on the workspace with this index"
    )]
    workspace_index: Option<u8>,

    #[clap(
        long,
        help = "Selects windows shown on a workspace whose name matches this regex"
    )]
    workspace_name: Option<String>,

    #[clap(long, help = "Selects only windows marked urgent")]
    urgent: bool,

    #[clap(long, help = "Selects only floating windows (opposite of --tiled)")]
    floating: bool,

    #[clap(long, help = "Selects only tiled windows (opposite of --floating)")]
    tiled: bool,

    // The two options below select on nirius' own state rather than on what
    // niri knows about a window.  They are #[serde(default)] because
    // MatchOptions is the IPC wire format: without it, a new niriusd could not
    // read the requests of an older nirius client anymore.
    #[clap(
        long,
        num_args = 0..=1,
        default_missing_value = DEFAULT_MARK,
        help = "Selects windows carrying this mark (default mark if no value)"
    )]
    #[serde(default)]
    marked: Option<String>,

    #[clap(long, help = "Selects only scratchpad windows")]
    #[serde(default)]
    scratchpad: bool,
}

impl MatchOptions {
    /// Returns whether no matching option at all was given, i.e., whether
    /// commands which match the focused window by default should do so.
    ///
    /// This compares against the default value instead of testing the fields
    /// one by one so that a newly added option is accounted for right away:
    /// clap represents every unset option by exactly that field's default.
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

static DEFAULT_MARK: &str = "__default__";

/// Returns the given mark or, if none is given, the default mark.
fn mark_or_default(mark: &Option<String>) -> String {
    mark.clone().unwrap_or(DEFAULT_MARK.to_owned())
}

pub fn exec_nirius_cmd(cmd: NiriusCmd) -> Result<String, String> {
    match &cmd {
        NiriusCmd::Focus { match_opts } => focus(match_opts),
        NiriusCmd::FocusOrSpawn {
            match_opts,
            command,
        } => focus_or_spawn(match_opts, command),
        NiriusCmd::MoveToCurrentWorkspace {
            match_opts,
            include_current_workspace,
            focus,
        } => move_to_current_workspace(
            match_opts,
            *include_current_workspace,
            *focus,
        ),
        NiriusCmd::MoveToCurrentWorkspaceOrSpawn {
            match_opts,
            include_current_workspace,
            focus,
            command,
        } => move_to_current_workspace_or_spawn(
            match_opts,
            *include_current_workspace,
            *focus,
            command,
        ),
        NiriusCmd::ToggleFollowMode { policy } => toggle_follow_mode(*policy),
        NiriusCmd::ToggleMark { mark } => toggle_mark(mark_or_default(mark)),
        NiriusCmd::FocusMarked { mark } => focus_marked(mark_or_default(mark)),
        NiriusCmd::ListMarked { mark, all } => {
            if *all {
                list_all_marked()
            } else {
                list_marked(mark_or_default(mark))
            }
        }
        NiriusCmd::ScratchpadToggle {
            match_opts,
            no_move,
        } => scratchpad_toggle(match_opts, *no_move),
        NiriusCmd::ScratchpadShow { match_opts, id } => {
            scratchpad_show(match_opts, *id)
        }
        NiriusCmd::ScratchpadShowAll => scratchpad_show_all(),
        NiriusCmd::ListScratchpad => list_scratchpad(),
    }
}

/// Toggles `id`'s membership in `v`: removes it if present (returns `false`),
/// otherwise appends it (returns `true`).  Order is preserved on removal.
fn toggle_vec_membership(id: u64, v: &mut Vec<u64>) -> bool {
    if let Some(index) = v.iter().position(|x| *x == id) {
        // swap_remove() would be more efficient but I think we want to retain
        // the order.
        v.remove(index);
        false
    } else {
        v.push(id);
        true
    }
}

fn toggle_follow_mode(policy: Option<FollowPolicy>) -> Result<String, String> {
    let mut w_state = STATE.write().expect("Could not write() STATE.");
    let Some(win_id) = w_state.get_focused_win_id() else {
        return Err("No focused window".to_owned());
    };

    let wins = &mut w_state.follow_mode_wins;
    match (wins.get(&win_id).copied(), policy) {
        // Requesting a policy other than the current one switches the policy
        // rather than disabling follow mode.  That way, a keybinding without
        // `--policy` always disables it.
        (Some(current), Some(policy)) if policy != current => {
            wins.insert(win_id, policy);
            Ok(format!(
                "Set follow mode policy of window {win_id} to {policy}"
            ))
        }
        (Some(_), _) => {
            wins.remove(&win_id);
            Ok(format!("Disabled follow mode for window {win_id}"))
        }
        (None, policy) => {
            let policy = policy.unwrap_or_default();
            wins.insert(win_id, policy);
            Ok(format!(
                "Enabled follow mode with policy {policy} for window {win_id}"
            ))
        }
    }
}

/// Sends the given action to niri and expects a `Response::Handled` reply,
/// returning `ok_msg` on success and an error describing any other reply.
fn exec_niri_action(
    action: Action,
    ok_msg: impl Into<String>,
) -> Result<String, String> {
    match ipc::query_niri(Request::Action(action))? {
        Response::Handled => Ok(ok_msg.into()),
        x => Err(format!("Received unexpected reply {x:?}")),
    }
}

/// Spawns the given command via niri.
fn spawn(command: &[String]) -> Result<String, String> {
    exec_niri_action(
        Action::Spawn {
            command: command.to_vec(),
        },
        "Spawned successfully",
    )
}

/// Returns `result` unchanged, except when it is an `Err` whose message is
/// exactly `NO_MATCHING_WINDOW`: in that case the given command is spawned
/// instead.  Any other `Err` is passed through untouched.
fn or_spawn(
    result: Result<String, String>,
    command: &[String],
) -> Result<String, String> {
    match result {
        Err(str) if NO_MATCHING_WINDOW == str => spawn(command),
        x => x,
    }
}

fn focus_or_spawn(
    match_opts: &MatchOptions,
    command: &[String],
) -> Result<String, String> {
    or_spawn(focus(match_opts), command)
}

fn focus(match_opts: &MatchOptions) -> Result<String, String> {
    let state = STATE.read().expect("Could not read() STATE.");
    let matcher = WindowMatcher::new(match_opts, &state)?;
    let currently_focused = state.get_focused_win_id();

    let focused_matches = currently_focused.is_some_and(|id| {
        state
            .all_windows
            .iter()
            .find(|w| w.id == id)
            .is_some_and(|w| matcher.matches(w, &state.all_workspaces))
    });

    let window_id = if focused_matches {
        // `all_windows` is ordered by focus history (most recently focused at
        // the back), so the focused window is at the back.  In this case, we
        // cannot use state.get_last_focused_matching() because this would
        // return the very same window again.
        state
            .all_windows
            .iter()
            .find(|w| matcher.matches(w, &state.all_workspaces))
            .map(|w| w.id)
    } else {
        // The focused window (if any) does not match, so jump to the most
        // recently focused matching window.
        state.get_last_focused_matching(|w| {
            matcher.matches(w, &state.all_workspaces)
        })
    };

    match window_id {
        Some(id) => focus_window(id, &state),
        None => Err(NO_MATCHING_WINDOW.to_owned()),
    }
}

/// Focuses the window with the given id.  A normal window is focused in place;
/// a hidden scratchpad window is first pulled onto the focused workspace,
/// because focusing it where it lives would switch to the scratchpad workspace
/// and trigger the daemon's auto-hide logic, i.e., it would be moved one
/// workspace further down with all other scratchpad workspaces.
fn focus_window(id: u64, state: &State) -> Result<String, String> {
    if state.get_focused_win_id() == Some(id) {
        return Ok(format!("Window {id} is already focused."));
    }
    if state.scratchpad_win_ids.contains(&id) {
        let focused_ws_id = state.focused_workspace_id_or_err()?;
        move_window_to_workspace_and_focus(
            id,
            WorkspaceReferenceArg::Id(focused_ws_id),
        )
    } else {
        focus_window_by_id(id)
    }
}

/// Low-level focus: tells niri to focus the window with the given id wherever
/// it currently lives.  Beware that focusing a hidden scratchpad window this
/// way switches to the scratchpad workspace and thus triggers the daemon's
/// auto-hide logic; prefer [`focus_window`], which pulls scratchpad windows
/// onto the focused workspace first.
fn focus_window_by_id(id: u64) -> Result<String, String> {
    exec_niri_action(
        Action::FocusWindow { id },
        format!("Focused window with id {id}"),
    )
}

/// A [`MatchOptions`] with its regexes pre-compiled, so that matching windows
/// doesn't recompile them for every window.
struct WindowMatcher<'a> {
    opts: &'a MatchOptions,
    app_id_rx: Option<Regex>,
    title_rx: Option<Regex>,
    workspace_name_rx: Option<Regex>,
    /// The window ids allowed by the options selecting on nirius' own state
    /// (`--marked`, `--scratchpad`), already intersected.  `None` means
    /// neither option was given, i.e., no restriction.
    state_ids: Option<Vec<u64>>,
}

impl<'a> WindowMatcher<'a> {
    /// Builds a matcher from `opts`, compiling its regexes once and resolving
    /// the options selecting on nirius' own state against `state`.  Errors if
    /// any of the regexes is invalid.
    fn new(opts: &'a MatchOptions, state: &State) -> Result<Self, String> {
        Ok(Self {
            opts,
            app_id_rx: Self::compile_opt_regex(&opts.app_id)?,
            title_rx: Self::compile_opt_regex(&opts.title)?,
            workspace_name_rx: Self::compile_opt_regex(&opts.workspace_name)?,
            state_ids: Self::resolve_state_ids(opts, state),
        })
    }

    /// Resolves `--marked` and `--scratchpad` into the set of window ids they
    /// allow, intersecting both when both are given.  Returns `None` if
    /// neither was given.
    ///
    /// A mark nobody has set yet resolves to the empty set rather than to an
    /// error.  Marks only exist once used, so an unknown mark is
    /// indistinguishable from a not-yet-used one, and the empty set is what
    /// makes `focus-or-spawn --marked foo CMD` spawn on first use instead of
    /// erroring out.
    fn resolve_state_ids(
        opts: &MatchOptions,
        state: &State,
    ) -> Option<Vec<u64>> {
        let marked = opts.marked.as_ref().map(|mark| {
            state.mark_to_win_ids.get(mark).cloned().unwrap_or_default()
        });
        let scratchpad =
            opts.scratchpad.then(|| state.scratchpad_win_ids.clone());

        match (marked, scratchpad) {
            (None, None) => None,
            (Some(ids), None) | (None, Some(ids)) => Some(ids),
            (Some(marked), Some(scratchpad)) => Some(
                marked
                    .into_iter()
                    .filter(|id| scratchpad.contains(id))
                    .collect(),
            ),
        }
    }

    /// Compiles an optional regex pattern, logging and returning an error
    /// message if the pattern is invalid.
    fn compile_opt_regex(
        pattern: &Option<String>,
    ) -> Result<Option<Regex>, String> {
        match pattern {
            None => Ok(None),
            Some(rx) => Regex::new(rx).map(Some).map_err(|e| {
                let msg = format!("Invalid regex {rx:?}: {e}");
                log::error!("{msg}");
                msg
            }),
        }
    }

    /// Returns whether `value` satisfies the optional compiled regex `rx`: an
    /// unset regex matches anything; a set regex requires `value` to be present
    /// and to match.
    fn regex_matches(rx: &Option<Regex>, value: Option<&str>) -> bool {
        match rx {
            None => true,
            Some(r) => value.is_some_and(|v| r.is_match(v)),
        }
    }

    fn matches(&self, w: &Window, workspaces: &[Workspace]) -> bool {
        let opts = self.opts;
        log::debug!("Matching window {w:?}");

        if let Some(ids) = &self.state_ids
            && !ids.contains(&w.id)
        {
            log::debug!(
                "window has not the requested mark or is no scratchpad window."
            );
            return false;
        }

        if opts.urgent && !w.is_urgent {
            log::debug!("window is not urgent.");
            return false;
        }

        if opts.floating && !w.is_floating {
            log::debug!("window is not floating.");
            return false;
        }

        if opts.tiled && w.is_floating {
            log::debug!("window is not tiled.");
            return false;
        }

        if !Self::regex_matches(&self.app_id_rx, w.app_id.as_deref()) {
            log::debug!("app-id does not match.");
            return false;
        }

        if !Self::regex_matches(&self.title_rx, w.title.as_deref()) {
            log::debug!("title does not match.");
            return false;
        }

        if w.pid.is_none() && opts.pid.is_some()
            || opts.pid.is_some_and(|pid| w.pid.unwrap() != pid)
        {
            log::debug!("pid does not match.");
            return false;
        }

        if w.workspace_id.is_none() && opts.workspace_id.is_some()
            || opts
                .workspace_id
                .is_some_and(|wid| w.workspace_id.unwrap() != wid)
        {
            log::debug!("workspace-id does not match.");
            return false;
        }

        if w.workspace_id.is_none()
            && (opts.workspace_index.is_some()
                || opts.workspace_name.is_some()
                || opts.focused_workspace
                || opts.active_workspace)
        {
            log::debug!("workspace does not match (window has none).");
            return false;
        } else if let Some(ws) = workspaces
            .iter()
            .find(|ws| ws.id == w.workspace_id.unwrap())
        {
            if opts.workspace_index.is_some_and(|idx| ws.idx != idx) {
                log::debug!("workspace-index does not match.");
                return false;
            }

            if !Self::regex_matches(&self.workspace_name_rx, ws.name.as_deref())
            {
                log::debug!("workspace-name does not match.");
                return false;
            }

            if opts.focused_workspace && !ws.is_focused {
                log::debug!("workspace is not focused.");
                return false;
            }

            if opts.active_workspace && !ws.is_active {
                log::debug!("workspace is not active.");
                return false;
            }
        } else {
            log::warn!(
                "No workspace with workspace id {} stated in window {}.
                 This looks like a bug.",
                w.workspace_id.unwrap(),
                w.id
            );
            if opts.workspace_index.is_some() || opts.workspace_name.is_some() {
                return false;
            }
        }

        true
    }
}

fn move_to_current_workspace(
    match_opts: &MatchOptions,
    include_current_workspace: bool,
    focus: bool,
) -> Result<String, String> {
    let state = STATE.read().expect("Could not read() STATE");
    let matcher = WindowMatcher::new(match_opts, &state)?;
    let focused_ws_id = state.focused_workspace_id_or_err()?;
    if let Some(win) = state.all_windows.iter().find(|w| {
        w.workspace_id.is_none_or(|ws_id| {
            include_current_workspace || ws_id != focused_ws_id
        }) && matcher.matches(w, &state.all_workspaces)
    }) {
        let move_result = move_window_to_workspace(
            win.id,
            niri_ipc::WorkspaceReferenceArg::Id(focused_ws_id),
            focus,
        );
        if focus {
            focus_window_by_id(win.id)?;
        }
        move_result
    } else {
        Err(NO_MATCHING_WINDOW.to_owned())
    }
}

fn move_to_current_workspace_or_spawn(
    match_opts: &MatchOptions,
    include_current_workspace: bool,
    focus: bool,
    command: &[String],
) -> Result<String, String> {
    or_spawn(
        move_to_current_workspace(match_opts, include_current_workspace, focus),
        command,
    )
}

pub fn move_window_to_workspace(
    window_id: u64,
    workspace_ref: niri_ipc::WorkspaceReferenceArg,
    focus: bool,
) -> Result<String, String> {
    exec_niri_action(
        Action::MoveWindowToWorkspace {
            window_id: Some(window_id),
            reference: workspace_ref,
            focus,
        },
        "Moved successfully",
    )
}

/// Moves the window to the given workspace and then focuses it.
fn move_window_to_workspace_and_focus(
    window_id: u64,
    workspace_ref: niri_ipc::WorkspaceReferenceArg,
) -> Result<String, String> {
    move_window_to_workspace(window_id, workspace_ref, true)?;
    focus_window_by_id(window_id)
}

/// Calls `f` for each item, returning the number of items processed or the
/// first error encountered.
pub(crate) fn try_for_each_and_count<I, T, F>(
    items: I,
    mut f: F,
) -> Result<usize, String>
where
    I: IntoIterator<Item = T>,
    F: FnMut(T) -> Result<String, String>,
{
    let mut count = 0;
    for item in items {
        f(item)?;
        count += 1;
    }
    Ok(count)
}

fn toggle_mark(mark: String) -> Result<String, String> {
    let mut state = STATE.write().expect("Could not write() STATE.");
    if let Some(focused_win_id) = state.get_focused_win_id() {
        let ids = state.mark_to_win_ids.entry(mark).or_default();
        if toggle_vec_membership(focused_win_id, ids) {
            Ok(format!("Set mark for window {focused_win_id:?}"))
        } else {
            Ok(format!("Unset mark for window {focused_win_id:?}"))
        }
    } else {
        Err("No focused window.".to_owned())
    }
}

fn focus_marked(mark: String) -> Result<String, String> {
    focus(&MatchOptions {
        marked: Some(mark),
        ..Default::default()
    })
}

fn list_marked(mark: String) -> Result<String, String> {
    let state = STATE.read().expect("Could not read() STATE.");

    if let Some(marked_windows) = state.mark_to_win_ids.get(&mark) {
        let wins: Vec<&Window> = state
            .all_windows
            .iter()
            .filter(|w| marked_windows.contains(&w.id))
            .collect();
        Ok(list_windows(wins))
    } else {
        Err("No such mark.".to_owned())
    }
}

fn list_windows(wins: Vec<&Window>) -> String {
    let mut str = String::new();
    for win in wins {
        let line = format!(
            "id: {}, app-id: {:?}, title: {:?}, on workspace: {:?}",
            win.id, win.app_id, win.title, win.workspace_id
        );
        str.push_str(line.as_str());
        str.push('\n');
    }
    str
}

fn list_all_marked() -> Result<String, String> {
    let keys: Vec<String> = STATE
        .read()
        .expect("Could not read() STATE.")
        .mark_to_win_ids
        .keys()
        .cloned()
        .collect();

    let mut s = String::new();
    for mark in keys {
        s.push_str(format!("-> {mark}:\n").as_str());
        match list_marked(mark.to_string()) {
            Ok(marks) => s.push_str(marks.as_str()),
            err @ Err(_) => return err,
        }
    }
    Ok(s)
}

fn scratchpad_toggle(
    match_opts: &MatchOptions,
    no_move: bool,
) -> Result<String, String> {
    let mut state = STATE.write().expect("Could not write() STATE.");
    let matcher = WindowMatcher::new(match_opts, &state)?;

    if let Some(window_id) = if match_opts.is_empty() {
        state.get_focused_win_id()
    } else {
        state
            .all_windows
            .iter()
            .find(|w| matcher.matches(w, &state.all_workspaces))
            .map(|w| w.id)
    } {
        if toggle_vec_membership(window_id, &mut state.scratchpad_win_ids) {
            drop(state);

            if no_move {
                Ok(format!("Added window {window_id} to scratchpad (no move)."))
            } else {
                scratchpad_move()
            }
        } else {
            Ok(format!("Removed window {window_id} from scratchpad."))
        }
    } else {
        Err(NO_MATCHING_WINDOW.to_owned())
    }
}

pub(crate) fn scratchpad_move() -> Result<String, String> {
    let state = STATE.read().expect("Could not read() STATE.");
    if state.scratchpad_win_ids.is_empty() {
        return Ok("No scratchpad windows to move.".to_owned());
    }
    let output = state
        .get_focused_workspace()
        .and_then(|ws| ws.output.as_ref())
        .ok_or(String::from("No focused output."))?;
    if let Some((ws_id, _)) =
        state.get_bottom_workspace_id_and_idx_of_output(output)
    {
        let i = try_for_each_and_count(
            state
                .all_windows
                .iter()
                .filter(|w| state.scratchpad_win_ids.contains(&w.id)),
            |w| {
                if !w.is_floating {
                    exec_niri_action(
                        Action::ToggleWindowFloating { id: Some(w.id) },
                        "Toggled floating.",
                    )?;
                }
                move_window_to_workspace(
                    w.id,
                    niri_ipc::WorkspaceReferenceArg::Id(ws_id),
                    false,
                )
            },
        )?;
        Ok(format!(
            "Moved {i} scratchpad windows to workspace with id {ws_id}."
        ))
    } else {
        Err("Can't move scratchpad windows. No focused workspace.".to_owned())
    }
}

fn scratchpad_show(
    match_opts: &MatchOptions,
    id: Option<u64>,
) -> Result<String, String> {
    let state = STATE.read().expect("Could not read STATE.");
    let matcher = WindowMatcher::new(match_opts, &state)?;

    // If --id was given, show that exact window or error if that's no
    // scratchpad window.
    if let Some(window_id) = id {
        if !state.scratchpad_win_ids.contains(&window_id) {
            return Err("Not a scratchpad window.".to_string());
        }
        // It already has focus, so there's nothing to do.
        if state.get_focused_win_id() == Some(window_id) {
            return Ok(format!("Window {window_id} already has focus."));
        }
        let focused_ws_id = state.focused_workspace_id_or_err()?;
        // In case a different scratchpad window is already visible, move it
        // back to the scratchpad.
        scratchpad_move()?;
        return move_window_to_workspace_and_focus(
            window_id,
            WorkspaceReferenceArg::Id(focused_ws_id),
        );
    }

    if state.focused_win_is_scratchpad_window() {
        // The focused window is itself a scratchpad window, so hide the
        // scratchpad again by moving its windows back down.
        scratchpad_move()
    } else {
        // Show the first scratchpad window matching the given options (if
        // there are any) on the focused workspace.
        let focused_ws_id = state.focused_workspace_id_or_err()?;

        if let Some(window_id) = state
            .all_windows
            .iter()
            .find(|w| {
                state.scratchpad_win_ids.contains(&w.id)
                    && matcher.matches(w, &state.all_workspaces)
            })
            .map(|w| w.id)
        {
            move_window_to_workspace_and_focus(
                window_id,
                WorkspaceReferenceArg::Id(focused_ws_id),
            )
        } else {
            Err("No matching scratchpad window.".to_string())
        }
    }
}

fn scratchpad_show_all() -> Result<String, String> {
    let state = STATE.read().expect("Could not read STATE.");
    if state.focused_win_is_scratchpad_window() {
        scratchpad_move()
    } else {
        let focused_ws_id = state.focused_workspace_id_or_err()?;

        let i = try_for_each_and_count(
            state
                .all_windows
                .iter()
                .filter(|w| state.scratchpad_win_ids.contains(&w.id)),
            |w| {
                move_window_to_workspace_and_focus(
                    w.id,
                    WorkspaceReferenceArg::Id(focused_ws_id),
                )
            },
        )?;
        Ok(format!(
            "Moved {i} scratchpad windows to workspace with id {focused_ws_id}."
        ))
    }
}

fn list_scratchpad() -> Result<String, String> {
    let state = STATE.read().expect("Could not read() STATE.");
    let scratch_wins = state
        .all_windows
        .iter()
        .filter(|w| state.scratchpad_win_ids.contains(&w.id))
        .collect();
    let str = list_windows(scratch_wins);
    Ok(str)
}
