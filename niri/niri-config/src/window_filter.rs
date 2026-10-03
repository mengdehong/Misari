use crate::utils::RegexEq;

/// KDL form of the shared window selection conditions.
#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq, Eq)]
pub struct WindowFilter {
    #[knuffel(property)]
    pub id: Option<u64>,
    #[knuffel(property)]
    pub app_id: Option<String>,
    #[knuffel(property)]
    pub title: Option<String>,
    #[knuffel(property)]
    pub workspace_id: Option<u64>,
    #[knuffel(property)]
    pub current_workspace: Option<bool>,
    #[knuffel(property)]
    pub floating: Option<bool>,
    #[knuffel(property)]
    pub urgent: Option<bool>,
}

impl From<niri_ipc::WindowFilter> for WindowFilter {
    fn from(filter: niri_ipc::WindowFilter) -> Self {
        Self {
            id: filter.id,
            app_id: filter.app_id,
            title: filter.title,
            workspace_id: filter.workspace_id,
            current_workspace: filter.current_workspace.then_some(true),
            floating: filter.floating,
            urgent: filter.urgent,
        }
    }
}

impl WindowFilter {
    pub fn compile_regexes(&self) -> Result<(Option<RegexEq>, Option<RegexEq>), String> {
        let compile = |name, pattern: &Option<String>| {
            pattern
                .as_ref()
                .map(|pattern| {
                    pattern
                        .parse::<RegexEq>()
                        .map_err(|err| format!("invalid {name} regular expression: {err}"))
                })
                .transpose()
        };
        Ok((
            compile("app-id", &self.app_id)?,
            compile("title", &self.title)?,
        ))
    }

    pub fn validate_focus(&self) -> Result<(), String> {
        if self.id.is_none()
            && self.app_id.is_none()
            && self.title.is_none()
            && self.workspace_id.is_none()
            && self.current_workspace != Some(true)
            && self.floating.is_none()
            && self.urgent.is_none()
        {
            return Err("focus-window-matching requires at least one condition".into());
        }
        self.compile_regexes().map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use crate::{Action, Config};

    #[test]
    fn window_selection_config_validation() {
        let config = Config::parse_mem(r#"binds { Mod+B { focus-window-matching app-id="^firefox$" title="GitHub" current-workspace=true floating=false; }; }"#).unwrap();
        let Action::FocusWindowMatching(filter) = &config.binds.0[0].action else {
            panic!("wrong action")
        };
        assert_eq!(filter.app_id.as_deref(), Some("^firefox$"));
        assert_eq!(filter.current_workspace, Some(true));
        assert_eq!(filter.floating, Some(false));
        for action in [
            "focus-window-matching",
            "focus-window-matching current-workspace=false",
            "focus-window-matching app-id=\"[\"",
            "focus-window-matching title=\"(\"",
        ] {
            assert!(
                Config::parse_mem(&format!("binds {{ Mod+B {{ {action}; }}; }}")).is_err(),
                "{action}"
            );
        }
    }
}
