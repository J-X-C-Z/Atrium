// Ported from apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts (T3 Code v0.1.11, MIT).
// The message shapes themselves are the Agent SDK 0.3.321 wire shapes T3 uses.

//! Wire types for the control channel and the input stream.
//!
//! Everything is newline-delimited JSON. The CLI writes `type: "control_request"`
//! frames for `can_use_tool`, hooks and elicitation; the host writes
//! `type: "control_response"` frames keyed by the same `request_id`. The host
//! writes user turns as `type: "user"` frames with Anthropic Messages API content.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The envelope kind the CLI and host use for control traffic.
pub const CONTROL_REQUEST: &str = "control_request";
/// The envelope kind the CLI and host use to answer control traffic.
pub const CONTROL_RESPONSE: &str = "control_response";

/// A control request in either direction. `request.fields` keeps every
/// subtype-specific field so a new CLI subtype is still readable and
/// answerable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlRequestEnvelope {
    #[serde(rename = "type")]
    pub kind: String,
    pub request_id: String,
    pub request: ControlRequestBody,
}

impl ControlRequestEnvelope {
    /// Builds a request with a caller-chosen id.
    pub fn new(request_id: impl Into<String>, request: Value) -> Self {
        Self {
            kind: CONTROL_REQUEST.into(),
            request_id: request_id.into(),
            request: ControlRequestBody::from_value(request),
        }
    }

    /// The subtype, such as `can_use_tool` or `interrupt`.
    pub fn subtype(&self) -> &str {
        &self.request.subtype
    }

    /// Reads a typed field from the request.
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.request.fields.get(name)
    }
}

/// A control request body: a subtype plus its fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlRequestBody {
    pub subtype: String,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl ControlRequestBody {
    fn from_value(value: Value) -> Self {
        let Value::Object(mut fields) = value else {
            return Self {
                subtype: String::new(),
                fields: Map::new(),
            };
        };
        let subtype = fields
            .remove("subtype")
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        Self { subtype, fields }
    }
}

/// The single reply to a control request. Shape from
/// `SDKControlResponse` in the Agent SDK's `sdk.d.ts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponseEnvelope {
    #[serde(rename = "type")]
    pub kind: String,
    pub response: ControlResponseBody,
}

impl ControlResponseEnvelope {
    /// A success reply. `payload` may be `Value::Null` for an empty ack.
    pub fn success(request_id: impl Into<String>, payload: Value) -> Self {
        Self {
            kind: CONTROL_RESPONSE.into(),
            response: ControlResponseBody {
                subtype: "success".into(),
                request_id: request_id.into(),
                response: payload,
                error: None,
                pending_permission_requests: Vec::new(),
            },
        }
    }

    /// An error reply.
    pub fn error(request_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            kind: CONTROL_RESPONSE.into(),
            response: ControlResponseBody {
                subtype: "error".into(),
                request_id: request_id.into(),
                response: Value::Null,
                error: Some(error.into()),
                pending_permission_requests: Vec::new(),
            },
        }
    }
}

/// The body of a `control_response`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponseBody {
    /// `success` or `error`.
    pub subtype: String,
    /// Echo of the request's `request_id`.
    pub request_id: String,
    /// Subtype-specific success payload; absent or `{}` for a bare ack.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub response: Value,
    /// Human-readable failure text on an `error` reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `can_use_tool` requests the CLI still has open. Present on an
    /// `initialize` success from Claude Code v2.1.268 and later.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_permission_requests: Vec<ControlRequestEnvelope>,
}

impl ControlResponseBody {
    /// True for a `success` reply.
    pub fn is_success(&self) -> bool {
        self.subtype == "success"
    }
}

/// A `can_use_tool` request lifted out of its envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    /// The tool input exactly as the model emitted it.
    pub input: Value,
    /// Tool call this prompt belongs to; the SDK echoes it into the answer.
    pub tool_use_id: Option<String>,
    /// Permission rules the host may persist if the user chooses "always allow".
    pub permission_suggestions: Vec<Value>,
    /// The path whose access triggered the prompt, when reported.
    pub blocked_path: Option<String>,
    pub decision_reason: Option<String>,
    pub decision_reason_type: Option<String>,
    /// Subagent id when the call came from a subagent.
    pub agent_id: Option<String>,
    /// MCP server provenance for `mcp__*` tools.
    pub mcp_server: Option<Value>,
    pub title: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    /// When true a persistent "don't ask again" rule must not be offered.
    pub suppress_always_allow_rule: Option<bool>,
}

impl PermissionRequest {
    /// Returns the typed request when `envelope` is a `can_use_tool` prompt.
    pub fn from_envelope(envelope: &ControlRequestEnvelope) -> Option<Self> {
        if envelope.subtype() != "can_use_tool" {
            return None;
        }
        let field = |name: &str| envelope.request.fields.get(name);
        let tool_name = field("tool_name")?.as_str()?.to_owned();
        let input = field("input").cloned().unwrap_or(Value::Null);
        Some(Self {
            request_id: envelope.request_id.clone(),
            tool_name,
            input,
            tool_use_id: string_field(field("tool_use_id")),
            permission_suggestions: field("permission_suggestions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            blocked_path: string_field(field("blocked_path")),
            decision_reason: string_field(field("decision_reason")),
            decision_reason_type: string_field(field("decision_reason_type")),
            agent_id: string_field(field("agent_id")),
            mcp_server: field("mcp_server").cloned(),
            title: string_field(field("title")),
            display_name: string_field(field("display_name")),
            description: string_field(field("description")),
            suppress_always_allow_rule: field("suppress_always_allow_rule")
                .and_then(Value::as_bool),
        })
    }
}

fn string_field(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

/// The answer to a [`PermissionRequest`], shaped as the SDK's `PermissionResult`.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDecision {
    /// Run the tool, optionally with rewritten input and saved rules.
    Allow {
        updated_input: Option<Value>,
        updated_permissions: Vec<Value>,
    },
    /// Refuse the tool. `interrupt` also stops the turn.
    Deny { message: String, interrupt: bool },
}

impl PermissionDecision {
    /// Builds the `response` payload the CLI reads.
    pub fn payload(&self, tool_use_id: Option<&str>) -> Value {
        match self {
            Self::Allow {
                updated_input,
                updated_permissions,
            } => {
                let mut value = json!({ "behavior": "allow" });
                if let Some(input) = updated_input {
                    value["updatedInput"] = input.clone();
                }
                if !updated_permissions.is_empty() {
                    value["updatedPermissions"] = Value::Array(updated_permissions.clone());
                }
                if let Some(id) = tool_use_id {
                    value["toolUseID"] = json!(id);
                }
                value
            }
            Self::Deny { message, interrupt } => {
                let mut value = json!({ "behavior": "deny", "message": message });
                if *interrupt {
                    value["interrupt"] = json!(true);
                }
                if let Some(id) = tool_use_id {
                    value["toolUseID"] = json!(id);
                }
                value
            }
        }
    }
}

/// A user turn written to the CLI's stdin.
#[derive(Debug, Clone, Serialize)]
pub struct UserTurnInput {
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Empty until the CLI reports a session id; the SDK writes `""`.
    pub session_id: String,
    pub message: UserMessage,
    pub parent_tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    /// Marks a prompt the host composed itself rather than one typed by the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_composed: Option<bool>,
}

impl UserTurnInput {
    /// A top-level user turn carrying `content`.
    pub fn new(session_id: impl Into<String>, content: Vec<UserContentBlock>) -> Self {
        Self {
            kind: "user",
            session_id: session_id.into(),
            message: UserMessage {
                role: "user".into(),
                content,
            },
            parent_tool_use_id: None,
            uuid: None,
            client_composed: None,
        }
    }
}

/// An Anthropic Messages API user message.
///
/// T3 sends a plain string when there is only one text block, and an array
/// otherwise (images, several blocks). Both are valid Messages API shapes;
/// matching T3 keeps slash-command and skill expansion identical.
#[derive(Debug, Clone)]
pub struct UserMessage {
    pub role: String,
    pub content: Vec<UserContentBlock>,
}

impl Serialize for UserMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut message = serializer.serialize_struct("UserMessage", 2)?;
        message.serialize_field("role", &self.role)?;
        if let [UserContentBlock::Text { text }] = self.content.as_slice() {
            message.serialize_field("content", text)?;
        } else {
            message.serialize_field("content", &self.content)?;
        }
        message.end()
    }
}

/// A content block a host can send.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContentBlock {
    Text { text: String },
    Image { source: ImageSource },
}

impl UserContentBlock {
    /// A text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// A base64 image block, as attachments arrive from the app.
    pub fn image(media_type: impl Into<String>, data_base64: impl Into<String>) -> Self {
        Self::Image {
            source: ImageSource {
                kind: "base64".into(),
                media_type: media_type.into(),
                data: data_base64.into(),
            },
        }
    }
}

/// Base64 image source for a user content block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub kind: String,
    pub media_type: String,
    pub data: String,
}

/// What the CLI reported in the `initialize` control response.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionInfo {
    pub commands: Vec<SlashCommand>,
    pub agents: Vec<AgentInfo>,
    pub models: Vec<ModelInfo>,
    pub output_style: String,
    pub available_output_styles: Vec<String>,
    pub account: Option<AccountInfo>,
    /// Prompts already open when this host attached.
    pub pending_permission_requests: Vec<PermissionRequest>,
}

impl SessionInfo {
    /// The slash command and skill names, without the leading slash.
    pub fn command_names(&self) -> Vec<&str> {
        self.commands
            .iter()
            .map(|command| command.name.as_str())
            .collect()
    }
}

/// A slash command, skill or built-in command the CLI advertises.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlashCommand {
    /// Skill name without the leading slash.
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "argumentHint")]
    pub argument_hint: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// True for Claude Code's own commands.
    #[serde(default)]
    pub builtin: bool,
}

/// A subagent definition the CLI can launch.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub model: Option<String>,
}

/// A model row from the initialize response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub value: String,
    #[serde(default)]
    pub resolved_model: Option<String>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub supports_effort: Option<bool>,
    #[serde(default)]
    pub supported_effort_levels: Vec<String>,
    #[serde(default)]
    pub supports_adaptive_thinking: Option<bool>,
}

/// The logged-in account, when the initialize response includes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub subscription_type: Option<String>,
    #[serde(default)]
    pub token_source: Option<String>,
    #[serde(default)]
    pub api_key_source: Option<String>,
    #[serde(default)]
    pub api_provider: Option<String>,
}

/// Parses the `response` payload of an `initialize` control response.
pub(crate) fn session_info_from_response(body: &ControlResponseBody) -> SessionInfo {
    let payload = &body.response;
    let from_value = |name: &str| -> Vec<Value> {
        payload
            .get(name)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let commands = from_value("commands")
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect();
    let agents = from_value("agents")
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect();
    let models = from_value("models")
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect();
    let account = payload
        .get("account")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let pending_permission_requests = body
        .pending_permission_requests
        .iter()
        .filter_map(PermissionRequest::from_envelope)
        .collect();
    SessionInfo {
        commands,
        agents,
        models,
        output_style: payload
            .get("output_style")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        available_output_styles: payload
            .get("available_output_styles")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        account,
        pending_permission_requests,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permission_request_keeps_the_fields_the_sdk_reads() {
        let envelope: ControlRequestEnvelope = serde_json::from_value(json!({
            "type": "control_request",
            "request_id": "req-1",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Bash",
                "input": {"command": "ls"},
                "permission_suggestions": [{"type": "addRules", "rules": [], "behavior": "allow", "destination": "session"}],
                "blocked_path": "/etc/hosts",
                "decision_reason": "outside the workspace",
                "decision_reason_type": "workingDir",
                "tool_use_id": "toolu_1",
                "agent_id": "agent-2"
            }
        }))
        .unwrap();
        let request = PermissionRequest::from_envelope(&envelope).expect("typed request");
        assert_eq!(request.tool_name, "Bash");
        assert_eq!(request.input["command"], "ls");
        assert_eq!(request.tool_use_id.as_deref(), Some("toolu_1"));
        assert_eq!(request.blocked_path.as_deref(), Some("/etc/hosts"));
        assert_eq!(request.permission_suggestions.len(), 1);
    }

    #[test]
    fn an_allow_answer_echoes_the_tool_use_id() {
        let decision = PermissionDecision::Allow {
            updated_input: Some(json!({"command": "ls -la"})),
            updated_permissions: Vec::new(),
        };
        let payload = decision.payload(Some("toolu_1"));
        assert_eq!(payload["behavior"], "allow");
        assert_eq!(payload["updatedInput"]["command"], "ls -la");
        assert_eq!(payload["toolUseID"], "toolu_1");
    }

    #[test]
    fn a_user_turn_with_an_image_serializes_as_anthropic_content() {
        let turn = UserTurnInput::new(
            "",
            vec![
                UserContentBlock::text("what is this?"),
                UserContentBlock::image("image/png", "AAAA"),
            ],
        );
        let value = serde_json::to_value(turn).unwrap();
        assert_eq!(value["type"], "user");
        assert_eq!(value["session_id"], "", "the SDK writes an empty id");
        assert_eq!(value["message"]["role"], "user");
        assert_eq!(value["message"]["content"][0]["type"], "text");
        assert_eq!(value["message"]["content"][1]["type"], "image");
        assert_eq!(
            value["message"]["content"][1]["source"]["media_type"],
            "image/png"
        );
    }

    #[test]
    fn a_single_text_turn_uses_the_string_content_shape() {
        let turn = UserTurnInput::new("s1", vec![UserContentBlock::text("hello")]);
        let value = serde_json::to_value(turn).unwrap();
        assert_eq!(value["message"]["content"], "hello");
    }

    #[test]
    fn control_responses_round_trip_with_an_empty_ack() {
        let dotted = ControlResponseEnvelope::success("req-3", Value::Null);
        let text = serde_json::to_string(&dotted).unwrap();
        let back: ControlResponseEnvelope = serde_json::from_str(&text).unwrap();
        assert_eq!(back.response.subtype, "success");
        assert_eq!(back.response.request_id, "req-3");
        assert!(back.response.response.is_null());
    }
}
