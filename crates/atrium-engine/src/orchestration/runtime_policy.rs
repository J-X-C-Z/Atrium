// Ported from apps/server/src/orchestration-v2/RuntimePolicy.ts and apps/server/src/orchestration-v2/runtimeLayer.ts (T3 Code v0.1.11, MIT).

//! Provider/model selection and runtime-mode policy for starting a turn.
//!
//! `RuntimePolicy.ts` decides the mode a turn actually runs in: the thread's
//! stored mode, narrowed to what the provider offers, under an optional
//! managed-policy ceiling, with per-launch overrides. `runtimeLayer.ts` is a
//! DI composition root; its Rust analogue is the builder here
//! ([`RuntimePolicy::from_project_store`] and friends). No provider process is
//! consulted; lookups are plain data.

use super::types::{InteractionMode, ModelSelection, RequestKind, RuntimeMode, Thread, Timestamp};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedPolicy {
    pub organization: String,
    pub max_runtime_mode: Option<RuntimeMode>,
}

impl ManagedPolicy {
    pub fn new(organization: impl Into<String>, max_runtime_mode: Option<RuntimeMode>) -> Self {
        Self {
            organization: organization.into(),
            max_runtime_mode,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimePolicyOverride {
    pub cwd: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox_policy: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedRuntimePolicy {
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub cwd: Option<String>,
    pub max_runtime_mode: Option<RuntimeMode>,
    pub approval_policy: Option<String>,
    pub sandbox_policy: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimePolicyError {
    Resolve {
        project_id: String,
        provider_instance_id: String,
        cause: String,
    },
    AboveCeiling {
        thread_id: String,
        runtime_mode: RuntimeMode,
        max_runtime_mode: RuntimeMode,
        organization: String,
    },
}

impl RuntimePolicyError {
    pub fn message(&self) -> String {
        match self {
            Self::Resolve {
                project_id,
                provider_instance_id,
                ..
            } => format!(
                "Failed to resolve runtime policy for provider instance {provider_instance_id} in project {project_id}."
            ),
            Self::AboveCeiling {
                runtime_mode: _,
                max_runtime_mode,
                organization,
                ..
            } => format!(
                "{} Switch this thread's mode to continue.",
                managed_runtime_mode_ceiling_message(organization, *max_runtime_mode)
            ),
        }
    }

    pub fn is_above_ceiling(&self) -> bool {
        matches!(self, Self::AboveCeiling { .. })
    }
}

impl std::fmt::Display for RuntimePolicyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for RuntimePolicyError {}

/// "Example Corp limits threads to Auto-accept edits."
pub fn managed_runtime_mode_ceiling_message(organization: &str, ceiling: RuntimeMode) -> String {
    format!("{organization} limits threads to {}.", ceiling.label())
}

/// A mode the provider does not offer falls back to Supervised.
pub fn provider_runtime_mode(
    runtime_mode: RuntimeMode,
    supported_runtime_modes: Option<&[RuntimeMode]>,
) -> RuntimeMode {
    match supported_runtime_modes {
        None => runtime_mode,
        Some([]) => runtime_mode,
        Some(modes) if modes.contains(&runtime_mode) => runtime_mode,
        Some(_) => RuntimeMode::ApprovalRequired,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDisposition {
    Allow,
    Ask,
    Deny,
}

/// The small slice of `acpPermissionDisposition` the runtime-policy tests read:
/// a command is allowed only at full access; edits may be implicit at
/// auto-accept-edits. Consolidation: replace with the provider ACP port.
pub fn acp_permission_disposition(
    policy: &ResolvedRuntimePolicy,
    kind: RequestKind,
) -> PermissionDisposition {
    match policy.runtime_mode {
        RuntimeMode::FullAccess => PermissionDisposition::Allow,
        RuntimeMode::AutoAcceptEdits
            if matches!(kind, RequestKind::FileRead | RequestKind::FileChange) =>
        {
            PermissionDisposition::Allow
        }
        _ => PermissionDisposition::Ask,
    }
}

type ResolveFn = dyn Fn(&Thread, &ModelSelection) -> Result<ResolvedRuntimePolicy, RuntimePolicyError>
    + Send
    + Sync;

/// A composed runtime policy, the Rust analogue of a T3 `Layer`.
pub struct RuntimePolicy {
    resolve: Box<ResolveFn>,
}

impl std::fmt::Debug for RuntimePolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RuntimePolicy")
    }
}

impl RuntimePolicy {
    /// The base layer: the thread's own mode, interaction mode and worktree.
    pub fn base() -> Self {
        Self {
            resolve: Box::new(|thread, _selection| {
                Ok(ResolvedRuntimePolicy {
                    runtime_mode: thread.runtime_mode,
                    interaction_mode: thread.interaction_mode,
                    cwd: thread.worktree_path.clone(),
                    ..ResolvedRuntimePolicy::default()
                })
            }),
        }
    }

    /// The provider-backed layer: narrows the mode to what the instance offers
    /// and fills `cwd` from the project when the thread has no worktree.
    pub fn from_project_store<P, I>(
        projects: P,
        instances: I,
        managed_policy: Option<ManagedPolicy>,
    ) -> Self
    where
        P: Fn(&str) -> Option<String> + Send + Sync + 'static,
        I: Fn(&str) -> Option<Vec<RuntimeMode>> + Send + Sync + 'static,
    {
        let max_runtime_mode = managed_policy
            .as_ref()
            .and_then(|policy| policy.max_runtime_mode);
        Self {
            resolve: Box::new(move |thread, selection| {
                refuse_above_ceiling(managed_policy.as_ref(), thread)?;
                let supported = instances(&selection.instance_id);
                let runtime_mode = provider_runtime_mode(thread.runtime_mode, supported.as_deref());
                let cwd = match &thread.worktree_path {
                    Some(path) => path.clone(),
                    None => {
                        projects(&thread.project_id).ok_or_else(|| RuntimePolicyError::Resolve {
                            project_id: thread.project_id.clone(),
                            provider_instance_id: selection.instance_id.clone(),
                            cause: "Project not found.".into(),
                        })?
                    }
                };
                Ok(ResolvedRuntimePolicy {
                    runtime_mode,
                    interaction_mode: thread.interaction_mode,
                    cwd: Some(cwd),
                    max_runtime_mode,
                    ..ResolvedRuntimePolicy::default()
                })
            }),
        }
    }

    /// Lays a managed policy's ceiling over another policy.
    pub fn enforcing_ceiling(self, managed_policy: ManagedPolicy) -> Self {
        let base = std::sync::Arc::new(self);
        let managed_policy = std::sync::Arc::new(managed_policy);
        Self {
            resolve: Box::new(move |thread, selection| {
                refuse_above_ceiling(Some(managed_policy.as_ref()), thread)?;
                base.resolve(thread, selection)
            }),
        }
    }

    /// Applies per-launch overrides to whatever the base resolved.
    pub fn with_override(self, overrides: RuntimePolicyOverride) -> Self {
        let base = std::sync::Arc::new(self);
        let overrides = std::sync::Arc::new(overrides);
        Self {
            resolve: Box::new(move |thread, selection| {
                let mut resolved = base.resolve(thread, selection)?;
                if let Some(cwd) = &overrides.cwd {
                    resolved.cwd = Some(cwd.clone());
                }
                if let Some(policy) = &overrides.approval_policy {
                    resolved.approval_policy = Some(policy.clone());
                }
                if let Some(policy) = &overrides.sandbox_policy {
                    resolved.sandbox_policy = Some(policy.clone());
                }
                if let Some(effort) = &overrides.reasoning_effort {
                    resolved.reasoning_effort = Some(effort.clone());
                }
                Ok(resolved)
            }),
        }
    }

    pub fn resolve(
        &self,
        thread: &Thread,
        selection: &ModelSelection,
    ) -> Result<ResolvedRuntimePolicy, RuntimePolicyError> {
        (self.resolve)(thread, selection)
    }
}

fn refuse_above_ceiling(
    managed_policy: Option<&ManagedPolicy>,
    thread: &Thread,
) -> Result<(), RuntimePolicyError> {
    let Some(managed_policy) = managed_policy else {
        return Ok(());
    };
    let Some(max_runtime_mode) = managed_policy.max_runtime_mode else {
        return Ok(());
    };
    if thread.runtime_mode.is_above(Some(max_runtime_mode)) {
        return Err(RuntimePolicyError::AboveCeiling {
            thread_id: thread.id.clone(),
            runtime_mode: thread.runtime_mode,
            max_runtime_mode,
            organization: managed_policy.organization.clone(),
        });
    }
    Ok(())
}

/// The runtime policy as the provider layer sees it: just what a turn start
/// needs, in the shape T3's `ProviderAdapterV2RuntimePolicy` carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnRuntimePolicy {
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub cwd: Option<String>,
    pub started_at: Option<Timestamp>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::types::{Actor, CreationSource};

    fn make_thread(runtime_mode: RuntimeMode, worktree_path: Option<&str>) -> Thread {
        Thread {
            id: "thread:runtime-policy".into(),
            project_id: "project:runtime-policy".into(),
            title: "Runtime policy".into(),
            provider_instance_id: "codex".into(),
            model_selection: ModelSelection::new("codex", "gpt-5.5"),
            runtime_mode,
            interaction_mode: InteractionMode::Default,
            branch: None,
            worktree_path: worktree_path.map(str::to_owned),
            created_by: Actor::User,
            creation_source: CreationSource::Web,
            created_at: 1,
            updated_at: 1,
            ..Thread::default()
        }
    }

    fn policy_with_modes(
        modes: Option<Vec<RuntimeMode>>,
        managed_policy: Option<ManagedPolicy>,
    ) -> RuntimePolicy {
        RuntimePolicy::from_project_store(
            |_| Some("/project-root".into()),
            move |_| modes.clone(),
            managed_policy,
        )
    }

    /// T3 `RuntimePolicy.test.ts`: Antigravity command approval.
    #[test]
    fn asks_for_antigravity_command_approval_when_auto_falls_back() {
        let antigravity = Some(vec![
            RuntimeMode::ApprovalRequired,
            RuntimeMode::AutoAcceptEdits,
            RuntimeMode::FullAccess,
        ]);
        let policy = policy_with_modes(antigravity, None);
        let selection = ModelSelection::new("antigravity", "test-model");
        let resolve = |mode| {
            policy
                .resolve(&make_thread(mode, None), &selection)
                .expect("policy resolves")
        };

        let auto = resolve(RuntimeMode::Auto);
        assert_eq!(auto.runtime_mode, RuntimeMode::ApprovalRequired);
        assert_eq!(
            acp_permission_disposition(&auto, RequestKind::Command),
            PermissionDisposition::Ask
        );
        assert_eq!(
            acp_permission_disposition(
                &resolve(RuntimeMode::AutoAcceptEdits),
                RequestKind::Command
            ),
            PermissionDisposition::Ask
        );
        assert_eq!(
            acp_permission_disposition(&resolve(RuntimeMode::FullAccess), RequestKind::Command),
            PermissionDisposition::Allow
        );
    }

    /// T3 `RuntimePolicy.test.ts`: a mode the provider does not offer runs in Supervised.
    #[test]
    fn runs_a_mode_the_provider_does_not_offer_in_supervised() {
        let grok = policy_with_modes(
            Some(vec![
                RuntimeMode::ApprovalRequired,
                RuntimeMode::Auto,
                RuntimeMode::FullAccess,
            ]),
            None,
        );
        let open = policy_with_modes(None, None);
        let mode_for = |policy: &RuntimePolicy, instance_id: &str, mode| {
            policy
                .resolve(
                    &make_thread(mode, None),
                    &ModelSelection::new(instance_id, "test-model"),
                )
                .expect("policy resolves")
                .runtime_mode
        };

        assert_eq!(
            mode_for(&grok, "grok", RuntimeMode::AutoAcceptEdits),
            RuntimeMode::ApprovalRequired
        );
        assert_eq!(
            mode_for(&grok, "grok", RuntimeMode::Auto),
            RuntimeMode::Auto
        );
        assert_eq!(
            mode_for(&grok, "grok", RuntimeMode::FullAccess),
            RuntimeMode::FullAccess
        );
        assert_eq!(
            mode_for(&open, "codex", RuntimeMode::AutoAcceptEdits),
            RuntimeMode::AutoAcceptEdits
        );
    }

    /// T3 `RuntimePolicy.test.ts`: refuse a turn above a managed ceiling.
    #[test]
    fn refuses_to_start_a_turn_above_the_ceiling() {
        let policy = policy_with_modes(
            None,
            Some(ManagedPolicy::new(
                "Example Corp",
                Some(RuntimeMode::AutoAcceptEdits),
            )),
        );
        let selection = ModelSelection::new("codex", "gpt-5.5");

        let refused = policy
            .resolve(
                &make_thread(RuntimeMode::FullAccess, Some("/work")),
                &selection,
            )
            .expect_err("full access is above the ceiling");
        assert_eq!(
            refused.message(),
            "Example Corp limits threads to Auto-accept edits. Switch this thread's mode to continue."
        );
        assert!(refused.is_above_ceiling());

        let allowed = policy
            .resolve(
                &make_thread(RuntimeMode::AutoAcceptEdits, Some("/work")),
                &selection,
            )
            .expect("auto-accept-edits is allowed");
        assert_eq!(allowed.runtime_mode, RuntimeMode::AutoAcceptEdits);
        assert_eq!(allowed.max_runtime_mode, Some(RuntimeMode::AutoAcceptEdits));
    }

    #[test]
    fn override_replaces_cwd_and_effort() {
        let policy = RuntimePolicy::base().with_override(RuntimePolicyOverride {
            cwd: Some("/override".into()),
            reasoning_effort: Some("high".into()),
            ..RuntimePolicyOverride::default()
        });
        let resolved = policy
            .resolve(
                &make_thread(RuntimeMode::FullAccess, Some("/work")),
                &ModelSelection::new("codex", "gpt"),
            )
            .expect("policy resolves");
        assert_eq!(resolved.cwd.as_deref(), Some("/override"));
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn missing_project_is_a_resolve_error() {
        let policy = RuntimePolicy::from_project_store(|_| None, |_| None, None);
        let error = policy
            .resolve(
                &make_thread(RuntimeMode::FullAccess, None),
                &ModelSelection::new("codex", "gpt"),
            )
            .expect_err("no project");
        assert!(matches!(
            error,
            RuntimePolicyError::Resolve { ref cause, .. } if cause == "Project not found."
        ));
    }
}
