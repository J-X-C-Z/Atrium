// Ported from apps/server/src/provider/CodexMcpElicitation.ts and
// apps/server/src/provider/CodexMcpElicitation.test.ts
// (T3 Code v0.1.11, MIT). Each test keeps T3's case and expected value.
//! Codex MCP elicitation requests, mapped onto the approval surface.
//!
//! Codex sends `mcpServer/elicitation/request` when an MCP server wants to ask
//! the person something (typically an app-access prompt). T3 keeps an
//! approval prompt for it unless the thread runs at full access, where it
//! accepts the request itself. This module builds that prompt's options and
//! the JSON-RPC response Codex expects.

use serde_json::{Map, Value, json};

use crate::contracts::{ProviderApprovalDecision, ProviderApprovalOption};

/// What an elicitation prompt shows: the app's name and the decisions a
/// person can make.
#[derive(Clone, Debug, PartialEq)]
pub struct ElicitationDescription {
    pub app_name: String,
    pub options: Vec<ProviderApprovalOption>,
}

fn nullable_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn metadata(payload: &Value) -> Option<&Map<String, Value>> {
    object(&payload["_meta"])
}

fn field_string(meta: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    nullable_string(meta.and_then(|meta| meta.get(key)))
}

fn metadata_app_name(meta: Option<&Map<String, Value>>) -> Option<String> {
    field_string(meta, "app_name")
        .or_else(|| field_string(meta, "appName"))
        .or_else(|| field_string(meta, "app"))
        .or_else(|| {
            meta.and_then(|meta| meta.get("target"))
                .and_then(object)
                .and_then(|target| field_string(Some(target), "app"))
        })
        .or_else(|| {
            meta.and_then(|meta| meta.get("target"))
                .and_then(object)
                .and_then(|target| field_string(Some(target), "name"))
        })
        .or_else(|| {
            meta.and_then(|meta| meta.get("tool_params"))
                .and_then(object)
                .and_then(|params| field_string(Some(params), "app_name"))
        })
        .or_else(|| {
            meta.and_then(|meta| meta.get("tool_params"))
                .and_then(object)
                .and_then(|params| field_string(Some(params), "app"))
        })
        .or_else(|| field_string(meta, "connector_name"))
        .or_else(|| field_string(meta, "connectorName"))
}

/// Ported from `mcpElicitationPersistenceDecision`.
fn persistence_decision(value: &str) -> Option<ProviderApprovalDecision> {
    let normalized = value.to_ascii_lowercase();
    if normalized.contains("session") {
        return Some(ProviderApprovalDecision::AcceptForSession);
    }
    if ["always", "permanent", "forever", "persistent"]
        .iter()
        .any(|word| normalized.contains(word))
    {
        return Some(ProviderApprovalDecision::AcceptAlways);
    }
    None
}

fn is_persistence_field(key: &str, field: &Map<String, Value>) -> bool {
    persistence_decision(key).is_some()
        || key.eq_ignore_ascii_case("persist")
        || field
            .get("title")
            .and_then(Value::as_str)
            .is_some_and(|title| persistence_decision(title).is_some())
        || field
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|description| persistence_decision(description).is_some())
}

/// The form schema when the request is a form (not a URL elicitation).
fn form_fields(payload: &Value) -> Option<&Map<String, Value>> {
    if payload["mode"].as_str() == Some("url") {
        return None;
    }
    let schema = object(&payload["requestedSchema"])?;
    object(schema.get("properties")?)
}

/// One field's `(value, label)` options, from `oneOf` first, then `enum`.
fn field_options(field: &Map<String, Value>) -> Vec<(String, Option<String>)> {
    if let Some(one_of) = field.get("oneOf").and_then(Value::as_array) {
        return one_of
            .iter()
            .filter_map(|option| {
                Some((
                    option.get("const")?.as_str()?.to_string(),
                    nullable_string(option.get("title")),
                ))
            })
            .collect();
    }
    let enum_names = field.get("enumNames").and_then(Value::as_array);
    field
        .get("enum")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .enumerate()
                .filter_map(|(index, value)| {
                    Some((
                        value.as_str()?.to_string(),
                        enum_names
                            .and_then(|names| names.get(index))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Ported from `describeMcpElicitation`.
pub fn describe_mcp_elicitation(payload: &Value) -> ElicitationDescription {
    let meta = metadata(payload);
    let app_name = metadata_app_name(meta)
        .or_else(|| {
            let message = payload["message"].as_str().unwrap_or_default();
            // "Allow ChatGPT to use Google Chrome?"
            message
                .trim()
                .strip_prefix("Allow ChatGPT to use ")
                .and_then(|rest| rest.strip_suffix('?'))
                .map(str::to_owned)
        })
        .or_else(|| nullable_string(payload.get("serverName")))
        .unwrap_or_default();

    let mut persistence: Vec<(ProviderApprovalDecision, String)> = Vec::new();
    let mut remember = |decision: ProviderApprovalDecision, label: String| {
        if let Some((_, current)) = persistence.iter_mut().find(|(seen, _)| *seen == decision) {
            if current.is_empty() && !label.is_empty() {
                *current = label;
            }
        } else {
            persistence.push((decision, label));
        }
    };
    if let Some(meta) = meta {
        match meta.get("persist") {
            Some(Value::String(value)) => {
                if let Some(decision) = persistence_decision(value) {
                    remember(decision, String::new());
                }
            }
            Some(Value::Array(values)) => {
                for value in values.iter().filter_map(Value::as_str) {
                    if let Some(decision) = persistence_decision(value) {
                        remember(decision, String::new());
                    }
                }
            }
            _ => {}
        }
        if meta.get("allowPersistentApproval").and_then(Value::as_bool) == Some(true) {
            remember(ProviderApprovalDecision::AcceptAlways, String::new());
        }
    }

    if let Some(fields) = form_fields(payload) {
        for (key, field) in fields {
            let Some(field) = object(field) else { continue };
            for (value, label) in field_options(field) {
                if let Some(decision) = persistence_decision(&value) {
                    remember(decision, label.unwrap_or_default());
                }
            }
            if field.get("type").and_then(Value::as_str) == Some("boolean")
                && is_persistence_field(key, field)
            {
                remember(
                    ProviderApprovalDecision::AcceptAlways,
                    field
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                );
            }
        }
    }

    let supports = |decision: ProviderApprovalDecision| {
        to_mcp_elicitation_response(payload, decision)["action"].as_str() == Some("accept")
    };
    let label_for = |decision: ProviderApprovalDecision, fallback: &str| {
        persistence
            .iter()
            .find(|(seen, _)| *seen == decision)
            .map(|(_, label)| {
                if label.is_empty() {
                    fallback.to_string()
                } else {
                    label.clone()
                }
            })
            .unwrap_or_else(|| fallback.to_string())
    };

    let mut options = vec![
        ProviderApprovalOption {
            decision: ProviderApprovalDecision::Cancel,
            label: "Cancel".into(),
            warning: None,
        },
        ProviderApprovalOption {
            decision: ProviderApprovalDecision::Decline,
            label: "Decline".into(),
            warning: None,
        },
    ];
    if persistence
        .iter()
        .any(|(decision, _)| *decision == ProviderApprovalDecision::AcceptForSession)
        && supports(ProviderApprovalDecision::AcceptForSession)
    {
        options.push(ProviderApprovalOption {
            decision: ProviderApprovalDecision::AcceptForSession,
            label: label_for(
                ProviderApprovalDecision::AcceptForSession,
                "Always allow this session",
            ),
            warning: None,
        });
    }
    if persistence
        .iter()
        .any(|(decision, _)| *decision == ProviderApprovalDecision::AcceptAlways)
        && supports(ProviderApprovalDecision::AcceptAlways)
    {
        options.push(ProviderApprovalOption {
            decision: ProviderApprovalDecision::AcceptAlways,
            label: label_for(ProviderApprovalDecision::AcceptAlways, "Always allow"),
            warning: None,
        });
    }
    options.push(ProviderApprovalOption {
        decision: ProviderApprovalDecision::Accept,
        label: "Approve".into(),
        warning: None,
    });

    ElicitationDescription { app_name, options }
}

fn is_accept_once_value(value: &str) -> bool {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if persistence_decision(value).is_some() {
        return false;
    }
    let lower = lower.as_str();
    matches!(lower, "once" | "accept" | "approve" | "allow")
        || lower.starts_with("once_")
        || lower.starts_with("once ")
        || lower.starts_with("once-")
        || lower.starts_with("accept ")
        || lower.starts_with("approve ")
        || lower.starts_with("allow ")
        || lower == "this_time"
        || lower == "this time"
        || lower == "this-time"
}

/// Ported from `toMcpElicitationResponse`. Returns the exact JSON-RPC result
/// Codex expects for `mcpServer/elicitation/request`.
pub fn to_mcp_elicitation_response(payload: &Value, decision: ProviderApprovalDecision) -> Value {
    match decision {
        ProviderApprovalDecision::Decline => return json!({"action": "decline"}),
        ProviderApprovalDecision::Cancel => return json!({"action": "cancel"}),
        _ => {}
    }
    // T3 cannot open a URL on the provider's behalf. `decline` would save a
    // thread-wide site denial, so a URL elicitation is cancelled instead.
    if payload["mode"].as_str() == Some("url") {
        return json!({"action": "cancel"});
    }

    let persist = match decision {
        ProviderApprovalDecision::AcceptForSession => Some("session"),
        ProviderApprovalDecision::AcceptAlways => Some("always"),
        _ => None,
    };
    let form = form_fields(payload);
    let mut content = Map::new();
    if let Some(fields) = form {
        for (key, field) in fields {
            let Some(field) = object(field) else { continue };
            let options = field_options(field);
            let chosen = options.iter().find(|(value, _)| match persist {
                Some(_) => persistence_decision(value) == Some(decision),
                None => is_accept_once_value(value),
            });
            if let Some((value, _)) = chosen {
                content.insert(key.clone(), Value::String(value.clone()));
            } else if field.get("type").and_then(Value::as_str) == Some("boolean")
                && is_persistence_field(key, field)
            {
                content.insert(
                    key.clone(),
                    Value::Bool(decision == ProviderApprovalDecision::AcceptAlways),
                );
            } else if let Some(default) = field.get("default")
                && !default.is_null()
            {
                content.insert(key.clone(), default.clone());
            }
        }
    }

    let required = payload["requestedSchema"]["required"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if required
        .iter()
        .filter_map(Value::as_str)
        .any(|key| !content.contains_key(key))
    {
        return json!({"action": "cancel"});
    }

    let mut response = json!({"action": "accept"});
    if let Some(persist) = persist {
        response["_meta"] = json!({"persist": persist});
    }
    if form.is_some() {
        response["content"] = Value::Object(content);
    }
    response
}

/// The request kind shown for an elicitation: `mcp-elicitation`.
pub const MCP_ELICITATION_REQUEST_KIND: &str = "mcp-elicitation";

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from apps/server/src/provider/CodexMcpElicitation.test.ts.
    fn request() -> Value {
        json!({
            "mode": "form",
            "message": "Allow ChatGPT to use Safari?",
            "serverName": "computer-use",
            "threadId": "provider-thread-1",
            "turnId": "turn-1",
            "_meta": {
                "app_name": "Safari",
                "persist": ["session", "always"]
            },
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "approval": {
                        "type": "string",
                        "oneOf": [
                            {"const": "once", "title": "Allow once"},
                            {"const": "session", "title": "Allow for this session"},
                            {"const": "always", "title": "Always allow Safari"}
                        ]
                    }
                },
                "required": ["approval"]
            }
        })
    }

    #[test]
    fn preserves_the_app_name_and_advertised_persistence_choices() {
        let described = describe_mcp_elicitation(&request());
        assert_eq!(described.app_name, "Safari");
        let options: Vec<(ProviderApprovalDecision, &str)> = described
            .options
            .iter()
            .map(|option| (option.decision, option.label.as_str()))
            .collect();
        assert_eq!(
            options,
            vec![
                (ProviderApprovalDecision::Cancel, "Cancel"),
                (ProviderApprovalDecision::Decline, "Decline"),
                (
                    ProviderApprovalDecision::AcceptForSession,
                    "Allow for this session"
                ),
                (
                    ProviderApprovalDecision::AcceptAlways,
                    "Always allow Safari"
                ),
                (ProviderApprovalDecision::Accept, "Approve"),
            ]
        );
    }

    #[test]
    fn returns_the_accepted_form_option_to_codex() {
        assert_eq!(
            to_mcp_elicitation_response(&request(), ProviderApprovalDecision::Accept),
            json!({"action": "accept", "content": {"approval": "once"}})
        );
    }

    #[test]
    fn returns_session_scoped_approval_in_the_mcp_response() {
        assert_eq!(
            to_mcp_elicitation_response(&request(), ProviderApprovalDecision::AcceptForSession),
            json!({
                "action": "accept",
                "_meta": {"persist": "session"},
                "content": {"approval": "session"}
            })
        );
    }

    #[test]
    fn returns_persistent_approval_in_the_mcp_response() {
        assert_eq!(
            to_mcp_elicitation_response(&request(), ProviderApprovalDecision::AcceptAlways),
            json!({
                "action": "accept",
                "_meta": {"persist": "always"},
                "content": {"approval": "always"}
            })
        );
    }

    #[test]
    fn supports_boolean_permanent_approval_fields() {
        let mut boolean_request = request();
        boolean_request["_meta"] = json!({"app_name": "Safari"});
        boolean_request["requestedSchema"] = json!({
            "type": "object",
            "properties": {
                "always": {"type": "boolean", "title": "Always allow Safari"}
            }
        });
        assert!(
            describe_mcp_elicitation(&boolean_request)
                .options
                .iter()
                .any(|option| option.decision == ProviderApprovalDecision::AcceptAlways)
        );
        assert_eq!(
            to_mcp_elicitation_response(&boolean_request, ProviderApprovalDecision::AcceptAlways),
            json!({
                "action": "accept",
                "_meta": {"persist": "always"},
                "content": {"always": true}
            })
        );
    }

    #[test]
    fn preserves_valid_nullable_mcp_form_fields_and_persistence_choices() {
        let mut nullable_request = request();
        nullable_request["_meta"] = json!({
            "app_name": null,
            "appName": "Safari",
            "connector_name": null,
            "persist": null,
            "target": null,
            "tool_params": null
        });
        nullable_request["requestedSchema"] = json!({
            "type": "object",
            "properties": {
                "approval": {
                    "type": "string",
                    "title": null,
                    "description": null,
                    "default": null,
                    "enum": ["once", "always"],
                    "enumNames": null
                }
            },
            "required": ["approval"]
        });
        assert_eq!(
            describe_mcp_elicitation(&nullable_request).app_name,
            "Safari"
        );
        assert!(
            describe_mcp_elicitation(&nullable_request)
                .options
                .iter()
                .any(|option| option.decision == ProviderApprovalDecision::AcceptAlways)
        );
        assert_eq!(
            to_mcp_elicitation_response(&nullable_request, ProviderApprovalDecision::AcceptAlways),
            json!({
                "action": "accept",
                "_meta": {"persist": "always"},
                "content": {"approval": "always"}
            })
        );
    }

    #[test]
    fn declines_required_form_fields_an_approval_prompt_cannot_collect() {
        let mut input_request = request();
        input_request["requestedSchema"] = json!({
            "type": "object",
            "properties": {
                "email": {"type": "string", "format": "email"}
            },
            "required": ["email"]
        });
        assert_eq!(
            to_mcp_elicitation_response(&input_request, ProviderApprovalDecision::Accept),
            json!({"action": "cancel"})
        );
    }

    #[test]
    fn does_not_approve_url_elicitations_without_opening_their_requested_url() {
        let url_request = json!({
            "mode": "url",
            "message": "Finish signing in to continue.",
            "serverName": "computer-use",
            "threadId": "provider-thread-1",
            "turnId": "turn-1",
            "elicitationId": "sign-in-1",
            "url": "https://example.com/authorize"
        });
        assert_eq!(
            to_mcp_elicitation_response(&url_request, ProviderApprovalDecision::Accept),
            json!({"action": "cancel"})
        );
    }

    #[test]
    fn omits_persistence_choices_that_cannot_satisfy_required_form_fields() {
        let mut once_only = request();
        once_only["_meta"] = json!({"app_name": "Safari", "persist": ["session", "always"]});
        once_only["requestedSchema"] = json!({
            "type": "object",
            "properties": {"approval": {"type": "string", "enum": ["once"]}},
            "required": ["approval"]
        });
        let described = describe_mcp_elicitation(&once_only);
        let options: Vec<(ProviderApprovalDecision, &str)> = described
            .options
            .iter()
            .map(|option| (option.decision, option.label.as_str()))
            .collect();
        assert_eq!(
            options,
            vec![
                (ProviderApprovalDecision::Cancel, "Cancel"),
                (ProviderApprovalDecision::Decline, "Decline"),
                (ProviderApprovalDecision::Accept, "Approve"),
            ]
        );
    }
}
