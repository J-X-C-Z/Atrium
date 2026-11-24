// Ported from apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts,
// `buildCodexTurnStartParams` and its tests in CodexAdapterV2.test.ts
// (T3 Code v0.1.11, MIT). Pure functions only, so the cases run without a
// provider process.
//! Building `turn/start` and `thread/settings/update` parameters for Codex.
//!
//! Codex applies model and effort per turn; a thread's service tier can also
//! be changed while it is loaded. T3 sends only `serviceTier` in
//! `thread/settings/update`; Atrium's adapter sends the model, effort and
//! approval policy too, so a UI change reaches a loaded thread immediately.

use codex_client::ThreadSettingsUpdate;
use serde_json::{Value, json};

use crate::contracts::{
    ModelSelection, ProviderInteractionMode, ProviderOptionSelections, RuntimeMode,
};

/// The model-selection option id T3 uses for reasoning effort.
pub const REASONING_EFFORT_OPTION: &str = "reasoningEffort";
/// The model-selection option id T3 uses for a service tier.
pub const SERVICE_TIER_OPTION: &str = "serviceTier";
/// The older id some provider catalogs still use for the fast tier.
pub const FAST_MODE_OPTION: &str = "fastMode";

pub fn option_string(selection: &ModelSelection, id: &str) -> Option<String> {
    let ProviderOptionSelections(options) = selection.options.as_ref()?;
    options
        .iter()
        .find(|option| option.id == id)
        .and_then(|option| option.value.as_str().map(str::to_owned))
}

/// T3's `getCodexServiceTierOptionValue`: `serviceTier`, then `fastMode=true`
/// meaning `priority`.
pub fn service_tier_value(selection: &ModelSelection) -> Option<String> {
    if let Some(tier) = option_string(selection, SERVICE_TIER_OPTION) {
        return Some(tier);
    }
    let ProviderOptionSelections(options) = selection.options.as_ref()?;
    options
        .iter()
        .find(|option| option.id == FAST_MODE_OPTION)
        .and_then(|option| option.value.as_bool())
        .map(|fast| if fast { "priority" } else { "default" }.to_string())
}

/// T3's `codexRuntimeModeTurnDefaults`.
pub fn runtime_mode_turn_defaults(runtime_mode: RuntimeMode) -> (Value, &'static str, Value) {
    match runtime_mode {
        RuntimeMode::ApprovalRequired => (json!("untrusted"), "user", json!({"type": "readOnly"})),
        RuntimeMode::AutoAcceptEdits => (
            json!("on-request"),
            "user",
            json!({"type": "workspaceWrite"}),
        ),
        RuntimeMode::Auto => (
            json!("on-request"),
            "auto_review",
            json!({"type": "workspaceWrite"}),
        ),
        RuntimeMode::FullAccess => (
            // `never` also rejects browser and app access prompts before the
            // client sees them; MCP elicitations stay visible.
            json!({
                "granular": {
                    "mcp_elicitations": true,
                    "request_permissions": false,
                    "rules": false,
                    "sandbox_approval": false,
                    "skill_approval": false
                }
            }),
            "user",
            json!({"type": "dangerFullAccess"}),
        ),
    }
}

/// Everything `buildCodexTurnStartParams` reads, in Rust terms.
pub struct TurnStartInput<'a> {
    pub native_thread_id: &'a str,
    pub text: &'a str,
    pub model_selection: &'a ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub cwd: Option<&'a str>,
    /// An administrator override for `approvalPolicy`, as raw JSON.
    pub approval_policy_override: Option<&'a Value>,
    /// An administrator override for `sandboxPolicy`, as raw JSON.
    pub sandbox_policy_override: Option<&'a Value>,
    /// Passed through when the thread's config sets `model_reasoning_summary`.
    pub configured_reasoning_summary: Option<&'a str>,
    /// ChatGPT token sharing does not accept service tiers.
    pub omit_service_tier: bool,
}

/// T3's `buildCodexTurnStartParams`, without the T3-only orchestration
/// context. `collaborationMode` is always sent, so exiting Plan mode clears
/// it on the provider side.
pub fn build_turn_start_params(input: TurnStartInput<'_>) -> Value {
    let (default_approval, reviewer, default_sandbox) =
        runtime_mode_turn_defaults(input.runtime_mode);
    let approval_policy = input
        .approval_policy_override
        .cloned()
        .unwrap_or(default_approval);
    let sandbox_policy = input
        .sandbox_policy_override
        .cloned()
        .unwrap_or(default_sandbox);
    let effort = option_string(input.model_selection, REASONING_EFFORT_OPTION);
    let service_tier = if input.omit_service_tier {
        None
    } else {
        service_tier_value(input.model_selection)
    };

    let mut params = json!({
        "threadId": input.native_thread_id,
        "input": [{"type": "text", "text": input.text, "text_elements": []}],
        "model": input.model_selection.model,
        // Some Responses providers reject `detailed`; `auto` is the most
        // detailed a model supports.
        "summary": input.configured_reasoning_summary.unwrap_or("auto"),
        "approvalsReviewer": reviewer,
        "approvalPolicy": approval_policy,
        "sandboxPolicy": sandbox_policy,
        "collaborationMode": {
            "mode": match input.interaction_mode {
                ProviderInteractionMode::Default => "default",
                ProviderInteractionMode::Plan => "plan",
            },
            "settings": {
                "model": input.model_selection.model,
            }
        }
    });
    if let Some(cwd) = input.cwd {
        params["cwd"] = json!(cwd);
    }
    if let Some(effort) = &effort {
        params["effort"] = json!(effort);
        params["collaborationMode"]["settings"]["reasoning_effort"] = json!(effort);
    }
    if let Some(tier) = &service_tier {
        params["serviceTier"] = json!(tier);
    }
    params
}

/// Build the mid-thread settings update. Every field is optional: T3 sends
/// only the service tier, while Atrium also sends model, effort and approval
/// policy. The approval policy uses the turn defaults for the runtime mode
/// unless an administrator override is set.
pub fn build_thread_settings_update(
    native_thread_id: &str,
    model_selection: &ModelSelection,
    runtime_mode: RuntimeMode,
    approval_policy_override: Option<&Value>,
) -> ThreadSettingsUpdate {
    // `turn/start` carries full access as a granular policy; the settings
    // update uses the plain Codex policy name.
    let approval_policy = match runtime_mode {
        RuntimeMode::ApprovalRequired => json!("untrusted"),
        RuntimeMode::AutoAcceptEdits | RuntimeMode::Auto => json!("on-request"),
        RuntimeMode::FullAccess => json!("never"),
    };
    ThreadSettingsUpdate {
        thread_id: native_thread_id.to_string(),
        model: Some(model_selection.model.clone()),
        effort: option_string(model_selection, REASONING_EFFORT_OPTION),
        service_tier: service_tier_value(model_selection),
        approval_policy: Some(approval_policy_override.cloned().unwrap_or(approval_policy)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{ProviderOptionSelection, ProviderOptionValue};

    fn selection(model: &str, options: Vec<(&str, Value)>) -> ModelSelection {
        ModelSelection {
            instance_id: "codex".into(),
            model: model.into(),
            options: Some(ProviderOptionSelections(
                options
                    .into_iter()
                    .map(|(id, value)| ProviderOptionSelection {
                        id: id.into(),
                        value: match value {
                            Value::Bool(value) => ProviderOptionValue::Boolean(value),
                            Value::String(value) => ProviderOptionValue::String(value),
                            _ => unreachable!("options are strings or bools"),
                        },
                    })
                    .collect(),
            )),
        }
    }

    // Ported from CodexAdapterV2.test.ts, "selects native collaboration modes
    // without prompt overrides" (T3 Code v0.1.11, MIT).
    #[test]
    fn selects_native_collaboration_modes_without_prompt_overrides() {
        for interaction_mode in [
            ProviderInteractionMode::Plan,
            ProviderInteractionMode::Default,
        ] {
            for effort in [None, Some("high")] {
                let options = effort
                    .map(|effort| vec![(REASONING_EFFORT_OPTION, json!(effort))])
                    .unwrap_or_default();
                let model = selection("gpt-5.4", options);
                let params = build_turn_start_params(TurnStartInput {
                    native_thread_id: "native-mode-switch",
                    text: "test",
                    model_selection: &model,
                    runtime_mode: RuntimeMode::FullAccess,
                    interaction_mode,
                    cwd: None,
                    approval_policy_override: None,
                    sandbox_policy_override: None,
                    configured_reasoning_summary: None,
                    omit_service_tier: false,
                });
                let expected_mode = match interaction_mode {
                    ProviderInteractionMode::Plan => "plan",
                    ProviderInteractionMode::Default => "default",
                };
                assert_eq!(params["collaborationMode"]["mode"], expected_mode);
                assert_eq!(params["collaborationMode"]["settings"]["model"], "gpt-5.4");
                assert_eq!(params["effort"], json!(effort));
                assert_eq!(
                    params["collaborationMode"]["settings"]["reasoning_effort"],
                    json!(effort)
                );
                // Some Responses providers reject `detailed`.
                assert_eq!(params["summary"], "auto");
            }
        }
    }

    // Ported from CodexAdapterV2.test.ts, "derives concrete Codex turn
    // policies from every T3 runtime mode" (T3 Code v0.1.11, MIT).
    #[test]
    fn derives_concrete_codex_turn_policies_from_every_runtime_mode() {
        let build = |runtime_mode| {
            let model = selection("gpt-5.4", Vec::new());
            build_turn_start_params(TurnStartInput {
                native_thread_id: "native",
                text: "test",
                model_selection: &model,
                runtime_mode,
                interaction_mode: ProviderInteractionMode::Default,
                cwd: None,
                approval_policy_override: None,
                sandbox_policy_override: None,
                configured_reasoning_summary: None,
                omit_service_tier: false,
            })
        };
        let approval_required = build(RuntimeMode::ApprovalRequired);
        let auto_accept_edits = build(RuntimeMode::AutoAcceptEdits);
        let auto = build(RuntimeMode::Auto);
        let full_access = build(RuntimeMode::FullAccess);

        assert_eq!(approval_required["approvalPolicy"], "untrusted");
        assert_eq!(approval_required["approvalsReviewer"], "user");
        assert_eq!(approval_required["sandboxPolicy"]["type"], "readOnly");
        assert_eq!(auto_accept_edits["approvalPolicy"], "on-request");
        assert_eq!(auto_accept_edits["approvalsReviewer"], "user");
        assert_eq!(auto_accept_edits["sandboxPolicy"]["type"], "workspaceWrite");
        assert_eq!(auto["approvalPolicy"], "on-request");
        assert_eq!(auto["approvalsReviewer"], "auto_review");
        assert_eq!(auto["sandboxPolicy"]["type"], "workspaceWrite");
        assert_eq!(
            full_access["approvalPolicy"],
            json!({
                "granular": {
                    "mcp_elicitations": true,
                    "request_permissions": false,
                    "rules": false,
                    "sandbox_approval": false,
                    "skill_approval": false
                }
            })
        );
        assert_eq!(full_access["approvalsReviewer"], "user");
        assert_eq!(full_access["sandboxPolicy"]["type"], "dangerFullAccess");
    }

    // Ported from CodexAdapterV2.test.ts, the service-tier signatures sent by
    // a steer and by the model picker (T3 Code v0.1.11, MIT).
    #[test]
    fn settings_updates_carry_the_selected_options() {
        let model = selection(
            "mock-1",
            vec![
                (REASONING_EFFORT_OPTION, json!("high")),
                (SERVICE_TIER_OPTION, json!("fast")),
            ],
        );
        assert_eq!(
            build_thread_settings_update("native-thread", &model, RuntimeMode::Auto, None),
            ThreadSettingsUpdate {
                thread_id: "native-thread".into(),
                model: Some("mock-1".into()),
                effort: Some("high".into()),
                service_tier: Some("fast".into()),
                approval_policy: Some(json!("on-request")),
            }
        );
        let default_tier = selection("mock-1", vec![(SERVICE_TIER_OPTION, json!("default"))]);
        assert_eq!(
            build_thread_settings_update("native-thread", &default_tier, RuntimeMode::Auto, None)
                .service_tier,
            Some("default".into())
        );
        // `fastMode=true` is the older catalog spelling of `priority`.
        let legacy = selection("mock-1", vec![(FAST_MODE_OPTION, json!(true))]);
        assert_eq!(
            build_thread_settings_update("native-thread", &legacy, RuntimeMode::Auto, None)
                .service_tier,
            Some("priority".into())
        );
    }
}
