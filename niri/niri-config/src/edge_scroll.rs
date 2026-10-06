use crate::{Binds, FloatOrInt};

/// An output edge region with ordinary scroll bindings.
#[derive(knuffel::Decode, Debug, PartialEq)]
pub struct EdgeScrollRule {
    #[knuffel(argument)]
    pub edge: ScreenEdge,
    #[knuffel(property, default = FloatOrInt(4.))]
    pub width: FloatOrInt<0, 65535>,
    #[knuffel(property)]
    pub output: Option<String>,
    #[knuffel(child)]
    pub binds: Binds,
}

#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenEdge {
    Top,
    Bottom,
    Left,
    Right,
}

#[cfg(test)]
mod tests {
    use crate::{Action, Config, Trigger};

    #[test]
    fn edge_scroll_rules_parse_and_reject_invalid_bindings() {
        let config = Config::parse_mem(
            r#"
            edge-scroll "top" width=0.5 {
                binds { WheelScrollUp cooldown-ms=50 { spawn "noctalia" "msg" "volume-up" "2%"; }; }
            }
            edge-scroll "right" output="eDP-1" {
                binds { Shift+TouchpadScrollDown { focus-workspace-down; }; }
            }
        "#,
        )
        .unwrap();
        assert_eq!(config.edge_scroll.len(), 2);
        assert_eq!(config.edge_scroll[0].width.0, 0.5);
        assert_eq!(config.edge_scroll[1].width.0, 4.);
        assert_eq!(config.edge_scroll[1].output.as_deref(), Some("eDP-1"));
        let bind = &config.edge_scroll[0].binds.0[0];
        assert_eq!(bind.key.trigger, Trigger::WheelScrollUp);
        assert!(matches!(bind.action, Action::Spawn(_)));
        assert_eq!(bind.cooldown.unwrap().as_millis(), 50);
        for text in [
            r#"edge-scroll "diagonal" { binds { WheelScrollUp { close-window; }; }; }"#,
            r#"edge-scroll "top" width=0 { binds { WheelScrollUp { close-window; }; }; }"#,
            r#"edge-scroll "top" width=-1 { binds { WheelScrollUp { close-window; }; }; }"#,
            r#"edge-scroll "top" width=inf { binds { WheelScrollUp { close-window; }; }; }"#,
            r#"edge-scroll "top" output="" { binds { WheelScrollUp { close-window; }; }; }"#,
            r#"edge-scroll "top" { binds {}; }"#,
            r#"edge-scroll "top" { binds { Mod+T { close-window; }; }; }"#,
            r#"edge-scroll "top" { binds { MouseLeft { close-window; }; }; }"#,
            r#"edge-scroll "top" { binds { WheelScrollUp { close-window; }; WheelScrollUp { quit; }; }; }"#,
        ] {
            assert!(
                Config::parse_mem(text).is_err(),
                "accepted invalid config: {text}"
            );
        }
    }
}
