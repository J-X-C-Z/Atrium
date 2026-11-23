// Ported from packages/contracts/src/modelSelection.ts (T3 Code v0.1.11, MIT).
//! Model selection and its legacy shapes.
//!
//! Also ports `ProviderOptionSelections` from
//! `packages/contracts/src/model.ts`: an array of `{id, value}` on the wire,
//! but older peers sent a plain object. Decoding normalizes both to the array
//! and encoding always emits the canonical array.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::base::{ProviderInstanceId, trim_non_empty};

/// One option's value: a non-blank string or a boolean.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderOptionValue {
    String(String),
    Boolean(bool),
}

impl ProviderOptionValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(*value),
            Self::String(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Boolean(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOptionSelection {
    pub id: String,
    pub value: ProviderOptionValue,
}

fn normalize_selection(
    selection: ProviderOptionSelection,
) -> Result<ProviderOptionSelection, String> {
    let id = trim_non_empty(&selection.id).ok_or("an option id must not be blank")?;
    let value = match selection.value {
        ProviderOptionValue::String(value) => ProviderOptionValue::String(
            trim_non_empty(&value).ok_or("an option string must not be blank")?,
        ),
        ProviderOptionValue::Boolean(value) => ProviderOptionValue::Boolean(value),
    };
    Ok(ProviderOptionSelection { id, value })
}

fn legacy_object_to_array(
    record: serde_json::Map<String, serde_json::Value>,
) -> Vec<ProviderOptionSelection> {
    let mut out = Vec::new();
    for (raw_key, raw_value) in record {
        let Some(id) = trim_non_empty(&raw_key) else {
            continue;
        };
        match raw_value {
            serde_json::Value::String(value) => {
                if let Some(value) = trim_non_empty(&value) {
                    out.push(ProviderOptionSelection {
                        id,
                        value: ProviderOptionValue::String(value),
                    });
                }
            }
            serde_json::Value::Bool(value) => out.push(ProviderOptionSelection {
                id,
                value: ProviderOptionValue::Boolean(value),
            }),
            _ => {}
        }
    }
    out
}

/// T3's `ProviderOptionSelections`, in its canonical array form.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ProviderOptionSelections(pub Vec<ProviderOptionSelection>);

impl ProviderOptionSelections {
    pub fn as_slice(&self) -> &[ProviderOptionSelection] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ProviderOptionSelections {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::Array(items) => {
                let mut selections = Vec::with_capacity(items.len());
                for item in items {
                    let selection: ProviderOptionSelection =
                        serde_json::from_value(item).map_err(D::Error::custom)?;
                    selections.push(normalize_selection(selection).map_err(D::Error::custom)?);
                }
                Ok(Self(selections))
            }
            serde_json::Value::Object(record) => Ok(Self(legacy_object_to_array(record))),
            other => Err(D::Error::custom(format!(
                "model options must be an array or an object, got {other}"
            ))),
        }
    }
}

/// T3's `ModelSelection` plus the `provider`-to-`instanceId` migration.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelSelection {
    pub instance_id: ProviderInstanceId,
    pub model: String,
    pub options: Option<ProviderOptionSelections>,
}

impl ModelSelection {
    pub fn new(instance_id: impl Into<ProviderInstanceId>, model: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            model: model.into(),
            options: None,
        }
    }
}

impl<'de> Deserialize<'de> for ModelSelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut record = match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::Object(record) => record,
            other => {
                return Err(D::Error::custom(format!(
                    "a model selection must be an object, got {other}"
                )));
            }
        };

        // T3 prefers an explicit `instanceId` even when it is null (which
        // then fails the wire schema), and only falls back to `provider`
        // when the key is absent.
        let instance_source = match record.remove("instanceId") {
            Some(value) => Some(value),
            None => record.remove("provider"),
        };
        let instance_id = match instance_source {
            Some(serde_json::Value::String(value)) => trim_non_empty(&value)
                .map(ProviderInstanceId::new)
                .ok_or_else(|| D::Error::custom("an instanceId must not be blank"))?,
            Some(other) => {
                return Err(D::Error::custom(format!(
                    "an instanceId must be a string, got {other}"
                )));
            }
            None => return Err(D::Error::missing_field("instanceId")),
        };

        let model = match record.remove("model") {
            Some(serde_json::Value::String(value)) => trim_non_empty(&value)
                .ok_or_else(|| D::Error::custom("a model must not be blank"))?,
            Some(other) => {
                return Err(D::Error::custom(format!(
                    "a model must be a string, got {other}"
                )));
            }
            None => return Err(D::Error::missing_field("model")),
        };

        let options = match record.remove("options") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(
                serde_json::from_value::<ProviderOptionSelections>(value)
                    .map_err(D::Error::custom)?,
            ),
        };

        Ok(Self {
            instance_id,
            model,
            options,
        })
    }
}

impl Serialize for ModelSelection {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            instance_id: &'a ProviderInstanceId,
            model: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            options: Option<&'a ProviderOptionSelections>,
        }
        Wire {
            instance_id: &self.instance_id,
            model: &self.model,
            options: self.options.as_ref(),
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode(value: serde_json::Value) -> ModelSelection {
        serde_json::from_value(value).unwrap()
    }

    // Ported from packages/contracts/src/modelSelection.test.ts.
    #[test]
    fn migrates_legacy_provider_to_instance_id() {
        let parsed = decode(json!({
            "provider": "codex",
            "model": "gpt-5-codex",
            "options": [{ "id": "reasoningEffort", "value": "high" }],
        }));
        assert_eq!(parsed.instance_id.as_str(), "codex");
        assert_eq!(parsed.model, "gpt-5-codex");
        assert_eq!(
            parsed.options.unwrap().0,
            vec![ProviderOptionSelection {
                id: "reasoningEffort".into(),
                value: ProviderOptionValue::String("high".into()),
            }]
        );
    }

    #[test]
    fn prefers_explicit_instance_id_over_legacy_provider() {
        let parsed = decode(json!({
            "provider": "codex",
            "instanceId": "codex_personal",
            "model": "gpt-5-codex",
        }));
        assert_eq!(parsed.instance_id.as_str(), "codex_personal");
    }

    #[test]
    fn treats_null_options_as_omitted() {
        let parsed = decode(json!({
            "instanceId": "codex",
            "model": "gpt-5",
            "options": null,
        }));
        assert_eq!(parsed, ModelSelection::new("codex", "gpt-5"));
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            json!({ "instanceId": "codex", "model": "gpt-5" })
        );
    }

    #[test]
    fn decodes_unknown_driver_kind_via_legacy_provider() {
        let parsed = decode(json!({
            "provider": "ollama",
            "model": "llama3:70b",
            "options": [{ "id": "temperature", "value": "0.4" }],
        }));
        assert_eq!(parsed.instance_id.as_str(), "ollama");
        assert_eq!(parsed.model, "llama3:70b");
    }

    #[test]
    fn normalizes_legacy_object_options_to_canonical_array() {
        let decoded = decode(json!({
            "provider": "claudeAgent",
            "model": "claude-opus-4-6",
            "options": {
                "effort": "max",
                "fastMode": true,
                "emptyStr": "   ",
                "nullish": null,
                "nested": { "foo": 1 }
            }
        }));
        assert_eq!(
            decoded.options.clone().unwrap().0,
            vec![
                ProviderOptionSelection {
                    id: "effort".into(),
                    value: ProviderOptionValue::String("max".into()),
                },
                ProviderOptionSelection {
                    id: "fastMode".into(),
                    value: ProviderOptionValue::Boolean(true),
                },
            ]
        );
        assert_eq!(
            serde_json::to_value(&decoded).unwrap()["options"],
            json!([
                { "id": "effort", "value": "max" },
                { "id": "fastMode", "value": true }
            ])
        );
    }

    #[test]
    fn rejects_a_missing_instance_id_and_blank_values() {
        assert!(serde_json::from_value::<ModelSelection>(json!({ "model": "x" })).is_err());
        assert!(
            serde_json::from_value::<ModelSelection>(
                json!({ "instanceId": "codex", "model": "  " })
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ProviderOptionSelections>(
                json!([{ "id": "x", "value": "  " }])
            )
            .is_err()
        );
    }
}
