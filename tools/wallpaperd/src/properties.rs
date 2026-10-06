//! Project definitions stay authored; only editable values are normalized for engines.
use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::{
    catalog,
    domain::{Error, Selection},
};

pub type Values = BTreeMap<String, Value>;

#[derive(Debug, Serialize)]
pub struct Definition {
    pub key: String,
    #[serde(flatten)]
    pub metadata: Map<String, Value>,
    pub editable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Definition {
    fn normalize(&self, value: &Value) -> Result<Value, String> {
        match self
            .metadata
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
        {
            "bool" if value.is_boolean() => Ok(value.clone()),
            "textinput" if value.is_string() => Ok(value.clone()),
            "slider" => {
                let n = value
                    .as_f64()
                    .filter(|n| n.is_finite() && (*n as f32).is_finite())
                    .ok_or("expected a finite number")?;
                let min = self
                    .metadata
                    .get("min")
                    .and_then(Value::as_f64)
                    .ok_or("slider has no numeric min")?;
                let max = self
                    .metadata
                    .get("max")
                    .and_then(Value::as_f64)
                    .ok_or("slider has no numeric max")?;
                if min > max || n < min || n > max {
                    return Err(format!("expected a number between {min} and {max}"));
                }
                if !self
                    .metadata
                    .get("fraction")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && n.fract() != 0.0
                {
                    return Err("expected an integer".into());
                }
                // step/precision describe the editor; floating-point modulo is not validation.
                Ok(value.clone())
            }
            "color" => {
                let components: Vec<f64> = match value {
                    Value::String(text) => text
                        .split_whitespace()
                        .map(|v| {
                            v.parse::<f64>()
                                .map_err(|_| "invalid color channel".to_owned())
                        })
                        .collect::<Result<_, _>>()?,
                    Value::Array(items) => items
                        .iter()
                        .map(|v| v.as_f64().ok_or_else(|| "invalid color channel".to_owned()))
                        .collect::<Result<_, _>>()?,
                    _ => return Err("expected three RGB channels".into()),
                };
                if components.len() != 3
                    || components
                        .iter()
                        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                {
                    return Err("expected three RGB channels between 0 and 1".into());
                }
                Ok(json!(components))
            }
            "combo" => {
                let options = self
                    .metadata
                    .get("options")
                    .and_then(Value::as_array)
                    .ok_or("combo has no options")?;
                options
                    .iter()
                    .filter_map(|v| v.get("value"))
                    .find(|option| {
                        *option == value
                            && (value.is_string() || value.is_boolean() || value.is_number())
                    })
                    .cloned()
                    .ok_or_else(|| "value is not an authored combo option".into())
            }
            "bool" => Err("expected a boolean".into()),
            "textinput" => Err("expected a string".into()),
            "group" | "text" => Err("display-only property".into()),
            _ => Err("unsupported property type".into()),
        }
    }
}

#[derive(Default)]
pub struct Schema {
    pub definitions: Vec<Definition>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Resolved {
    pub overrides: Values,
    pub values: Values,
    pub warnings: BTreeMap<String, String>,
    pub visible: BTreeMap<String, bool>,
}

impl Schema {
    pub fn load(selection: &Selection) -> Result<Self, Error> {
        if !selection.asset_id.starts_with("we:") {
            return Ok(Self::default());
        }
        let project: Value = catalog::read_project(catalog::path(selection))?;
        Self::parse(&project)
    }

    fn parse(project: &Value) -> Result<Self, Error> {
        let properties = &project["general"]["properties"];
        if properties.is_null() {
            return Ok(Self::default());
        }
        let object = properties.as_object().ok_or_else(|| {
            Error::new("asset_unavailable", "general.properties must be an object")
        })?;
        let mut definitions = Vec::new();
        for (key, raw) in object {
            let mut metadata = raw.as_object().cloned().unwrap_or_default();
            for reserved in ["key", "editable", "default", "group", "error"] {
                metadata.remove(reserved);
            }
            let mut definition = Definition {
                key: key.clone(),
                metadata,
                editable: false,
                default: None,
                group: None,
                error: None,
            };
            let kind = definition
                .metadata
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("");
            if matches!(kind, "file" | "directory") {
                // WE scripts expect these keys even before the user selects a file.
                // Keep them read-only and empty until file selection is supported.
                definition.default = Some(json!(""));
                definition.error = Some("file/directory selection is not supported".into());
            } else if !matches!(kind, "group" | "text") {
                match definition.normalize(&raw["value"]) {
                    Ok(default) => {
                        definition.default = Some(default);
                        definition.editable = true;
                    }
                    Err(error) => definition.error = Some(error),
                }
            }
            definitions.push(definition);
        }
        definitions.sort_by(|a, b| {
            let order = |d: &Definition| {
                (
                    d.metadata
                        .get("order")
                        .and_then(Value::as_i64)
                        .unwrap_or(i64::MAX),
                    d.metadata
                        .get("index")
                        .and_then(Value::as_i64)
                        .unwrap_or(i64::MAX),
                )
            };
            order(a).cmp(&order(b)).then_with(|| a.key.cmp(&b.key))
        });
        let mut group = None;
        for definition in &mut definitions {
            if definition.metadata.get("type").and_then(Value::as_str) == Some("group") {
                group = Some(definition.key.clone());
            } else {
                definition.group = group.clone();
            }
        }
        Ok(Self { definitions })
    }

    pub fn resolve(&self, saved: &Values, patch: &Values) -> Result<Resolved, Error> {
        let mut overrides = saved.clone();
        for (key, value) in patch {
            if value.is_null() && saved.contains_key(key) {
                overrides.remove(key);
                continue;
            }
            let definition = self
                .definitions
                .iter()
                .find(|d| d.key == *key && d.editable)
                .ok_or_else(|| {
                    Error::new(
                        "bad_request",
                        format!("unknown or read-only property: {key}"),
                    )
                })?;
            if value.is_null() {
                overrides.remove(key);
            } else {
                overrides.insert(
                    key.clone(),
                    definition
                        .normalize(value)
                        .map_err(|e| Error::new("bad_request", format!("property {key}: {e}")))?,
                );
            }
        }
        let mut resolved = Resolved {
            overrides,
            ..Resolved::default()
        };
        for definition in &self.definitions {
            if let Some(default) = &definition.default {
                resolved
                    .values
                    .insert(definition.key.clone(), default.clone());
            }
        }
        for (key, value) in &resolved.overrides {
            let valid = self
                .definitions
                .iter()
                .find(|d| d.key == *key && d.editable)
                .ok_or_else(|| "property no longer exists or is read-only".to_owned())
                .and_then(|d| d.normalize(value));
            match valid {
                Ok(value) => {
                    resolved.values.insert(key.clone(), value);
                }
                Err(error) => {
                    resolved.warnings.insert(key.clone(), error);
                }
            }
        }
        for definition in &self.definitions {
            let condition = definition
                .metadata
                .get("condition")
                .and_then(Value::as_str)
                .unwrap_or("");
            let visible = match condition_visible(condition, &resolved.values) {
                Some(visible) => visible,
                None => {
                    resolved
                        .warnings
                        .entry(definition.key.clone())
                        .or_insert_with(|| format!("unsupported display condition: {condition}"));
                    true
                }
            };
            let group_visible = definition
                .group
                .as_ref()
                .and_then(|key| resolved.visible.get(key))
                .copied()
                .unwrap_or(true);
            resolved
                .visible
                .insert(definition.key.clone(), visible && group_visible);
        }
        // Leave room for worker command fields within the existing 64 KiB framing limit.
        if serde_json::to_vec(&resolved.values)
            .expect("serializing property values")
            .len()
            > crate::ipc::MAX_LINE / 2
        {
            return Err(Error::new("bad_request", "property values exceed 32 KiB"));
        }
        Ok(resolved)
    }
}

// ponytail: only scalar display conditions; add an expression parser when authors need more.
// Conditions are data, never JavaScript. Unknown expressions remain visible.
fn condition_visible(expression: &str, values: &Values) -> Option<bool> {
    let expression = expression.trim();
    if expression.is_empty() {
        return Some(true);
    }
    if let Some(inner) = expression.strip_prefix('!') {
        if inner.contains(['=', '!']) {
            return None;
        }
        return condition_visible(inner, values).map(|v| !v);
    }
    for op in ["===", "!==", "==", "!="] {
        if let Some((reference, literal)) = expression.split_once(op) {
            let key = reference.trim().strip_suffix(".value")?;
            let lhs = values.get(key)?;
            let rhs: Value = serde_json::from_str(literal.trim()).ok().or_else(|| {
                literal
                    .trim()
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''))
                    .map(|s| Value::String(s.into()))
            })?;
            let equal = if lhs.is_number() && rhs.is_number() {
                lhs.as_f64() == rhs.as_f64()
            } else if op.len() == 2
                && (lhs.is_number() || rhs.is_number() || lhs.is_boolean() || rhs.is_boolean())
                && !(lhs.is_boolean() && rhs.is_boolean())
                && !rhs.is_null()
            {
                let number = |value: &Value| match value {
                    Value::Number(value) => value.as_f64(),
                    Value::Bool(value) => Some(u8::from(*value) as f64),
                    Value::String(value) if value.trim().is_empty() => Some(0.0),
                    Value::String(value) => value.trim().parse::<f64>().ok(),
                    _ => None,
                };
                number(lhs)? == number(&rhs)?
            } else {
                lhs == &rhs
            };
            return Some(if op.starts_with('!') { !equal } else { equal });
        }
    }
    let key = expression.strip_suffix(".value")?;
    match values.get(key)? {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => Some(value.as_f64()? != 0.0),
        Value::String(value) => Some(!value.is_empty()),
        _ => None,
    }
}

pub fn patch(params: &Value) -> Result<Values, Error> {
    match params.get("properties") {
        None => Ok(Values::new()),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| Error::new("bad_request", "properties must be an object")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn values(value: Value) -> Values {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn authored_project_options_groups_and_hidden_settings() {
        let project: Value =
            serde_json::from_str(include_str!("../tests/fixtures/properties.json")).unwrap();
        let schema = Schema::parse(&project).unwrap();
        assert_eq!(schema.definitions.len(), 14);
        assert_eq!(schema.definitions.iter().filter(|d| d.editable).count(), 8);
        let selected = schema.resolve(&Values::new(), &values(json!({"newproperty4":"0", "b1":"2", "musicbar":false, "newproperty55":"测试文字", "newproperty51":[0.1,0.2,0.3]}))).unwrap();
        assert_eq!(selected.values["newproperty4"], "0");
        assert_eq!(selected.values["b1"], "2");
        assert!(!selected.visible["newproperty4"]);
        let enabled = schema
            .resolve(&selected.overrides, &values(json!({"musicbar":true})))
            .unwrap();
        assert!(enabled.visible["newproperty4"]);
        assert_eq!(enabled.values["newproperty4"], "0");
        assert_eq!(enabled.values["newproperty55"], "测试文字");
        assert!(
            schema
                .resolve(&enabled.overrides, &values(json!({"newproperty4":0})))
                .is_err()
        );
        assert!(
            schema
                .resolve(&enabled.overrides, &values(json!({"b1":"6"})))
                .is_err()
        );
        let reset = schema
            .resolve(&enabled.overrides, &values(json!({"b1":null})))
            .unwrap();
        assert_eq!(reset.values["b1"], "5");
        assert!(!reset.overrides.contains_key("b1"));
        assert_eq!(reset.values["newproperty4"], "0");
        let definition = schema
            .definitions
            .iter()
            .find(|d| d.key == "newproperty4")
            .unwrap();
        assert_eq!(definition.group.as_deref(), Some("newproperty19"));
        assert_eq!(definition.metadata["options"][0]["label"], "样式1");
    }

    #[test]
    fn invalid_saved_values_are_retained_but_never_sent_to_engine() {
        let schema = Schema::parse(&json!({"general":{"properties":{
            "choice":{"type":"combo","value":7,"options":[{"label":"seven","value":7}]},
            "integer":{"type":"slider","min":0,"max":10,"value":3},
            "label":{"type":"text","value":""},
            "future":{"type":"new_type","value":{}},
            "bad":false
        }}}))
        .unwrap();
        let saved = values(json!({"choice":"7","removed":true,"integer":100}));
        let resolved = schema.resolve(&saved, &Values::new()).unwrap();
        assert_eq!(resolved.overrides, saved);
        assert_eq!(resolved.values, values(json!({"choice":7,"integer":3})));
        assert_eq!(resolved.warnings.len(), 3);
        assert!(
            schema
                .resolve(&saved, &values(json!({"integer":1.5})))
                .is_err()
        );
        assert!(
            schema
                .resolve(&saved, &values(json!({"label":"new"})))
                .is_err()
        );
        assert!(schema.resolve(&saved, &values(json!({"choice":7}))).is_ok());
        assert_eq!(
            condition_visible("choice.value == '7'", &resolved.values),
            Some(true)
        );
        assert_eq!(
            condition_visible("choice.value === '7'", &resolved.values),
            Some(false)
        );
        assert_eq!(condition_visible("unknown()", &resolved.values), None);
        let flags = values(json!({"enabled":true,"choice":"0"}));
        assert_eq!(condition_visible("enabled.value == 1", &flags), Some(true));
        assert_eq!(
            condition_visible("enabled.value === 1", &flags),
            Some(false)
        );
        assert_eq!(condition_visible("!enabled.value", &flags), Some(false));
        assert_eq!(condition_visible("!choice.value === '1'", &flags), None);
        let reset = schema
            .resolve(&saved, &values(json!({"removed":null,"choice":null})))
            .unwrap();
        assert!(!reset.overrides.contains_key("removed"));
        assert_eq!(reset.values["choice"], 7);
        assert!(
            schema
                .resolve(&saved, &values(json!({"typo":null})))
                .is_err()
        );
    }
}
