use std::cmp::Reverse;

use niri_config::utils::RegexEq;
use niri_config::window_filter::WindowFilter;
use smithay::desktop::Window;

use super::Mapped;
use crate::layout::workspace::WorkspaceId;
use crate::layout::Layout;
use crate::utils::with_toplevel_role;

/// Reuses compiled patterns across candidates; never owns a copy of compositor state.
pub(crate) struct WindowSelector {
    pub(crate) filter: WindowFilter,
    app_id: Option<RegexEq>,
    title: Option<RegexEq>,
}

impl WindowSelector {
    /// Keep a focused match, otherwise use MRU with a stable ID tie-breaker.
    pub(crate) fn select(&self, layout: &Layout<Mapped>) -> Option<(bool, Window)> {
        let current = layout.active_workspace().map(|ws| ws.id());
        let mut best = None;
        layout.with_windows(|window, _, workspace, _| {
            if !self.matches(window, workspace, current) {
                return;
            }
            let preference = (
                window.is_focused(),
                window.get_focus_timestamp(),
                Reverse(window.id().get()),
            );
            if best.as_ref().is_none_or(|(key, _)| preference > *key) {
                best = Some((preference, window.window.clone()));
            }
        });
        best.map(|((focused, _, _), window)| (focused, window))
    }

    pub(crate) fn new(filter: WindowFilter) -> Result<Self, String> {
        let (app_id, title) = filter.compile_regexes()?;
        Ok(Self {
            filter,
            app_id,
            title,
        })
    }

    pub(crate) fn matches(
        &self,
        window: &Mapped,
        workspace: Option<WorkspaceId>,
        current_workspace: Option<WorkspaceId>,
    ) -> bool {
        let f = &self.filter;
        if f.id.is_some_and(|id| id != window.id().get())
            || f.workspace_id
                .is_some_and(|id| Some(id) != workspace.map(|ws| ws.get()))
            || (f.current_workspace == Some(true)
                && (current_workspace.is_none() || workspace != current_workspace))
            || f.floating
                .is_some_and(|value| value != window.is_floating())
            || f.urgent.is_some_and(|value| value != window.is_urgent())
        {
            return false;
        }
        with_toplevel_role(window.toplevel(), |role| {
            regex_matches(&self.app_id, role.app_id.as_deref())
                && regex_matches(&self.title, role.title.as_deref())
        })
    }
}

fn regex_matches(regex: &Option<RegexEq>, value: Option<&str>) -> bool {
    regex
        .as_ref()
        .is_none_or(|regex| value.is_some_and(|value| regex.0.is_match(value)))
}
